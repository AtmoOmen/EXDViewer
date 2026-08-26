use std::io::Cursor;

use anyhow::{Result, anyhow};
use ironworks::file::scd::{Codec, SoundEntry};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded interleaved f32 PCM plus its loop region.
pub struct Decoded {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
    /// Per-channel frame indices; `None` if the track does not loop.
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
}

/// Decode a BGM sound entry to interleaved PCM.
pub fn decode(entry: &SoundEntry) -> Result<Decoded> {
    decode_data(entry.format(), entry.data())
}

/// Decode raw codec bytes directly, for re-decoding a stream already held in memory.
pub fn decode_data(codec: Codec, data: &[u8]) -> Result<Decoded> {
    let mut decoded = match codec {
        Codec::OggVorbis => decode_ogg(data),
        Codec::Hca => decode_hca(data),
        other => Err(anyhow!("unsupported audio codec {other:?}")),
    }?;
    downmix_to_stereo(&mut decoded);
    Ok(decoded)
}

/// Encode decoded PCM as a 16-bit PCM WAV file.
pub fn encode_wav(audio: &Decoded) -> Result<Vec<u8>> {
    let channels = u32::from(audio.channels);
    let data_size = audio
        .samples
        .len()
        .checked_mul(2)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| anyhow!("PCM data too large for WAV"))?;
    let riff_size = data_size
        .checked_add(36)
        .ok_or_else(|| anyhow!("PCM data too large for WAV"))?;
    let byte_rate = audio
        .sample_rate
        .checked_mul(channels)
        .and_then(|rate| rate.checked_mul(2))
        .ok_or_else(|| anyhow!("WAV byte rate overflows"))?;
    let block_align = audio
        .channels
        .checked_mul(2)
        .ok_or_else(|| anyhow!("WAV block alignment overflows"))?;

    let mut wav = Vec::with_capacity(44 + data_size as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&audio.channels.to_le_bytes());
    wav.extend_from_slice(&audio.sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    for &sample in &audio.samples {
        // A full-scale negative sample maps to the true i16::MIN rather than clipping a step
        // short of it, since the positive and negative ranges of i16 are not symmetric.
        let pcm = if sample <= -1.0 {
            i16::MIN
        } else {
            (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
        };
        wav.extend_from_slice(&pcm.to_le_bytes());
    }
    Ok(wav)
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

/// OggVorbis via symphonia. Loop points come from the `LoopStart`/`LoopEnd` Vorbis comments,
/// as the game uses; the SCD byte offsets are ignored.
fn decode_ogg(data: &[u8]) -> Result<Decoded> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
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
        .ok_or_else(|| anyhow!("ogg has no default track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| anyhow!("ogg track has no audio codec parameters"))?;
    let channels = params
        .channels
        .as_ref()
        .ok_or_else(|| anyhow!("ogg track has no channel layout"))?
        .count() as u16;
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| anyhow!("ogg track has no sample rate"))?;
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
