use std::io::{Cursor, Write as _};

use anyhow::{Result, anyhow};
use ironworks::file::scd::{Codec, SoundEntry};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use zip::{ZipWriter, write::SimpleFileOptions};

/// Decoded interleaved f32 PCM plus its loop region.
pub struct Decoded {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
    /// Per-channel frame indices; `None` if the track does not loop.
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
}

/// Decode a BGM sound entry to interleaved PCM, downmixed to stereo for playback.
pub fn decode(entry: &SoundEntry) -> Result<Decoded> {
    decode_entry(entry, true)
}

/// The same, keeping every channel the source holds; playback always downmixes instead, but an
/// export billed as lossless can't drop the surrounds.
pub fn decode_full(entry: &SoundEntry) -> Result<Decoded> {
    decode_entry(entry, false)
}

fn decode_entry(entry: &SoundEntry, downmix: bool) -> Result<Decoded> {
    let mut decoded = decode_stream_data(entry.format(), entry.data(), downmix)?;
    // MS ADPCM carries no loop metadata of its own (unlike the Ogg/HCA containers, which do and
    // are read from inside `decode_stream_data`); the SCD-level fields are the only source, and
    // they are byte offsets into the compressed body rather than sample indices.
    if entry.format() == Codec::MsAdpcm
        && entry.loop_end() > 0
        && let (Some(block_align), Some(samples_per_block)) =
            (entry.adpcm_block_align(), entry.adpcm_samples_per_block())
        && block_align > 0
    {
        let to_frame = |byte_offset: u32| {
            (byte_offset / u32::from(block_align)) * u32::from(samples_per_block)
        };
        decoded.loop_start = Some(to_frame(entry.loop_start()));
        decoded.loop_end = Some(to_frame(entry.loop_end()));
    }
    Ok(decoded)
}

/// Decode raw codec bytes directly, for re-decoding a stream already held in memory.
pub fn decode_data(codec: Codec, data: &[u8]) -> Result<Decoded> {
    decode_stream_data(codec, data, true)
}

fn decode_stream_data(codec: Codec, data: &[u8], downmix: bool) -> Result<Decoded> {
    let mut decoded = match codec {
        Codec::OggVorbis => decode_stream(data, "ogg"),
        Codec::Hca => decode_hca(data),
        Codec::MsAdpcm => decode_stream(data, "wav"),
        other => Err(anyhow!("unsupported audio codec {other:?}")),
    }?;
    if downmix {
        downmix_to_stereo(&mut decoded);
    }
    Ok(decoded)
}

/// The extension a codec's own bytes are already a complete file in, or `None` where `data()` is
/// either empty ([`Codec::Empty`]) or a bare payload with no container of its own.
fn native_extension(codec: Codec) -> Option<&'static str> {
    match codec {
        Codec::OggVorbis => Some("ogg"),
        Codec::Hca => Some("hca"),
        // Already a standalone WAVE_FORMAT_ADPCM `.wav`; qualified so it never collides with the
        // decoded WAV of the same entry.
        Codec::MsAdpcm => Some("adpcm.wav"),
        Codec::Mp3 => Some("mp3"),
        Codec::Atrac9 => Some("at9"),
        Codec::Pcm => Some("pcm"),
        Codec::Unknown(_) => Some("bin"),
        Codec::Empty => None,
    }
}

/// Every entry's own bytes, native container and all, as `(extension, bytes)` for [`package`].
pub fn export_native(entries: &[SoundEntry]) -> Vec<(&'static str, Vec<u8>)> {
    entries
        .iter()
        .filter_map(|entry| Some((native_extension(entry.format())?, entry.data().clone())))
        .collect()
}

/// Every entry this crate can actually decode, as a 16-bit WAV, for [`package`].
pub fn export_wav(entries: &[SoundEntry]) -> Result<Vec<(&'static str, Vec<u8>)>> {
    entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.format(),
                Codec::OggVorbis | Codec::Hca | Codec::MsAdpcm
            )
        })
        .map(|entry| Ok(("wav", encode_wav(&decode_full(entry)?)?)))
        .collect()
}

/// A lone stream saves directly under `stem`; more than one zips together under it instead, since
/// a save dialog only ever picks one file.
pub fn package(files: Vec<(&str, Vec<u8>)>, stem: &str) -> Result<(String, Vec<u8>)> {
    match files.len() {
        0 => anyhow::bail!("no streams to export"),
        1 => {
            let (ext, bytes) = files.into_iter().next().unwrap();
            Ok((format!("{stem}.{ext}"), bytes))
        }
        _ => {
            let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
            for (index, (ext, bytes)) in files.iter().enumerate() {
                archive.start_file(format!("{stem}_{index:02}.{ext}"), SimpleFileOptions::default())?;
                archive.write_all(bytes)?;
            }
            Ok((format!("{stem}.zip"), archive.finish()?.into_inner()))
        }
    }
}

/// Encode decoded PCM as a 16-bit PCM WAV file.
pub fn encode_wav(audio: &Decoded) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: audio.channels,
        sample_rate: audio.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buffer = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut buffer, spec)?;
    for &sample in &audio.samples {
        // A full-scale negative sample maps to the true i16::MIN rather than clipping a step
        // short of it, since the positive and negative ranges of i16 are not symmetric.
        let pcm = if sample <= -1.0 {
            i16::MIN
        } else {
            (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
        };
        writer.write_sample(pcm)?;
    }
    writer.finalize()?;
    Ok(buffer.into_inner())
}

const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Fold anything wider than stereo down using the ITU-R BS.775 weights: front channels at unity,
/// center and surrounds at -3 dB, LFE dropped. Channel order is WAVE (FL,FR[,FC,LFE],RL,RR), so
/// quad and 5.1 are handled by name; other counts keep the front pair. Compacted in place: the
/// write cursor (stride 2) never catches up to the read cursor (stride N >= 4).
fn downmix_to_stereo(decoded: &mut Decoded) {
    let channels = decoded.channels as usize;
    if channels <= 2 {
        return;
    }
    let frames = decoded.samples.len() / channels;
    let samples = &mut decoded.samples;
    for i in 0..frames {
        let base = i * channels;
        let (l, r) = match channels {
            4 => (
                samples[base] + SQRT_HALF * samples[base + 2],
                samples[base + 1] + SQRT_HALF * samples[base + 3],
            ),
            6 => (
                samples[base] + SQRT_HALF * (samples[base + 2] + samples[base + 4]),
                samples[base + 1] + SQRT_HALF * (samples[base + 2] + samples[base + 5]),
            ),
            _ => (samples[base], samples[base + 1]),
        };
        samples[2 * i] = l.clamp(-1.0, 1.0);
        samples[2 * i + 1] = r.clamp(-1.0, 1.0);
    }
    samples.truncate(frames * 2);
    decoded.channels = 2;
}

/// Anything symphonia probes, named by the extension its container would carry. Loop points
/// come from the `LoopStart`/`LoopEnd` Vorbis comments where the container has them, as the
/// game uses; the SCD byte offsets are ignored.
fn decode_stream(data: &[u8], extension: &str) -> Result<Decoded> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut format = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let (mut loop_start, mut loop_end) = (None, None);
    if let Some(revision) = format.metadata().current() {
        for tag in &revision.media.tags {
            match tag.raw.key.to_ascii_uppercase().as_str() {
                "LOOPSTART" => loop_start = tag.raw.value.to_string().parse().ok(),
                "LOOPEND" => loop_end = tag.raw.value.to_string().parse().ok(),
                _ => {}
            }
        }
    }

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("{extension} has no default track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| anyhow!("{extension} track has no audio codec parameters"))?;
    let channels = params
        .channels
        .as_ref()
        .ok_or_else(|| anyhow!("{extension} track has no channel layout"))?
        .count() as u16;
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| anyhow!("{extension} track has no sample rate"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())?;

    let mut samples = Vec::new();
    let mut block = Vec::new();
    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                audio.copy_to_vec_interleaved(&mut block);
                samples.extend_from_slice(&block);
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(Decoded {
        samples,
        channels,
        sample_rate,
        loop_start,
        loop_end,
    })
}

/// HCA via cridecoder. `decode_all` output shares the header's loop-block timeline (delay and
/// padding already trimmed); the loop maths mirror vgmstream.
fn decode_hca(data: &[u8]) -> Result<Decoded> {
    let mut decoder = cridecoder::HcaDecoder::from_reader(Cursor::new(data.to_vec()))
        .map_err(|error| anyhow!("hca: {error:?}"))?;
    let info = decoder.info().clone();
    let samples = decoder
        .decode_all()
        .map_err(|error| anyhow!("hca decode: {error:?}"))?;

    let (loop_start, loop_end) = if info.loop_enabled {
        let per_block = info.samples_per_block as u32;
        let delay = info.encoder_delay;
        (
            Some(info.loop_start_block * per_block - delay + info.loop_start_delay),
            Some((info.loop_end_block + 1) * per_block - delay - info.loop_end_padding),
        )
    } else {
        (None, None)
    };

    Ok(Decoded {
        samples,
        channels: info.channel_count as u16,
        sample_rate: info.sampling_rate,
        loop_start,
        loop_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_matches_the_pcm_it_wraps() {
        let audio = Decoded {
            samples: vec![0.0, 1.0, -1.0, 0.5],
            channels: 2,
            sample_rate: 44100,
            loop_start: None,
            loop_end: None,
        };
        let wav = encode_wav(&audio).unwrap();

        let u32_at =
            |offset: usize| u32::from_le_bytes(wav[offset..offset + 4].try_into().unwrap());
        let u16_at =
            |offset: usize| u16::from_le_bytes(wav[offset..offset + 2].try_into().unwrap());

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(4), 44);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(16), 16);
        assert_eq!(u16_at(20), 1);
        assert_eq!(u16_at(22), 2);
        assert_eq!(u32_at(24), 44100);
        assert_eq!(u32_at(28), 44100 * 2 * 2);
        assert_eq!(u16_at(32), 4);
        assert_eq!(u16_at(34), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(40), 8);
        assert_eq!(wav.len(), 44 + 8);

        let pcm: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        assert_eq!(
            pcm,
            vec![
                0,
                i16::MAX,
                i16::MIN,
                (0.5 * f32::from(i16::MAX)).round() as i16
            ]
        );
    }

    #[test]
    fn a_lone_stream_saves_directly() {
        let (name, bytes) = package(vec![("ogg", vec![1, 2, 3])], "bgm_title").unwrap();
        assert_eq!(name, "bgm_title.ogg");
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn several_streams_zip_under_the_shared_stem() {
        let (name, bytes) = package(
            vec![("ogg", vec![1, 2, 3]), ("wav", vec![4, 5, 6, 7])],
            "se_battle",
        )
        .unwrap();
        assert_eq!(name, "se_battle.zip");

        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 2);
        let mut read = |name: &str| -> Vec<u8> {
            let mut file = archive.by_name(name).unwrap();
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut out).unwrap();
            out
        };
        assert_eq!(read("se_battle_00.ogg"), vec![1, 2, 3]);
        assert_eq!(read("se_battle_01.wav"), vec![4, 5, 6, 7]);
    }

    #[test]
    fn no_streams_is_an_error() {
        assert!(package(vec![], "empty").is_err());
    }

    /// The ITU-R BS.775 fold itself, previously untested: front channels at unity, center and
    /// surrounds at -3 dB (1/sqrt(2)), LFE dropped. `decode_stream_data`'s `if downmix {}` is the
    /// only thing standing between this (playback) and skipping it (export); that gate is a
    /// one-line conditional around this same call, not exercised separately here.
    #[test]
    fn downmix_applies_the_itu_r_bs775_weights_and_drops_lfe() {
        // One 5.1 frame: FL, FR, FC, LFE, RL, RR. FC is zero so each output isolates its own
        // front/rear pair; LFE is a large, otherwise-unused value that would push the result past
        // the values asserted below (and past the +-1 clamp) if it leaked in.
        let mut decoded = Decoded {
            samples: vec![0.2, 0.3, 0.0, 0.9, 0.4, 0.5],
            channels: 6,
            sample_rate: 44100,
            loop_start: None,
            loop_end: None,
        };
        downmix_to_stereo(&mut decoded);
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.samples.len(), 2);

        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!((decoded.samples[0] - (0.2 + half * 0.4)).abs() < 1e-6, "FL + sqrt(1/2)*RL");
        assert!((decoded.samples[1] - (0.3 + half * 0.5)).abs() < 1e-6, "FR + sqrt(1/2)*RR");
    }
}

#[cfg(test)]
mod real_scd {
    use super::*;
    use ironworks::file::File as _;
    use ironworks::file::scd::SoundContainer;
    use std::io::Cursor as ReadCursor;

    const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";

    fn read_local(path: &str) -> Vec<u8> {
        use ironworks::sqpack::{Install, SqPack};
        use std::io::Read;
        let pack = SqPack::new(Install::at_sqpack(SQPACK));
        let mut stream = pack.file(path).unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    }

    /// A real multi-entry bank (18 sound entries, two `Empty`, sixteen `MsAdpcm`): `Empty` drops
    /// out of both exports, the rest zip under both, and the decoded WAV keeps each entry's own
    /// channel count rather than the player's stereo downmix.
    #[test]
    #[ignore = "reads the real local FFXIV install"]
    fn a_real_multi_entry_bank_exports_every_non_empty_stream() {
        let bytes = read_local("sound/system/SE_10thMG.scd");
        let container = SoundContainer::read(ReadCursor::new(bytes)).unwrap();
        let entries = container.entries();
        assert_eq!(entries.len(), 18);
        let non_empty = entries.iter().filter(|e| e.format() != Codec::Empty).count();
        assert_eq!(non_empty, 16);

        let native = export_native(entries);
        assert_eq!(native.len(), 16);
        assert!(native.iter().all(|(ext, _)| *ext == "adpcm.wav"));

        let wav = export_wav(entries).unwrap();
        assert_eq!(wav.len(), 16);

        for (entry, (_, wav_bytes)) in entries.iter().filter(|e| e.format() != Codec::Empty).zip(&wav) {
            let channels_at = |offset: usize| u16::from_le_bytes([wav_bytes[offset], wav_bytes[offset + 1]]);
            assert_eq!(
                channels_at(22),
                entry.channel_count() as u16,
                "the decoded wav should keep the source's own channel count, not downmix it"
            );
        }

        let (name, zipped) = package(native, "SE_10thMG").unwrap();
        assert_eq!(name, "SE_10thMG.zip");
        let mut archive = zip::ZipArchive::new(ReadCursor::new(zipped)).unwrap();
        assert_eq!(archive.len(), 16);
        assert!(archive.by_name("SE_10thMG_00.adpcm.wav").is_ok());
    }
}
