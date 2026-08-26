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
    let mut decoded = decode_data(entry.format(), entry.data())?;
    // MS ADPCM carries no loop metadata of its own (unlike the Ogg/HCA containers, which do and
    // are read from inside `decode_data`); the SCD-level fields are the only source, and they are
    // byte offsets into the compressed body rather than sample indices.
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
    let mut decoded = match codec {
        Codec::OggVorbis => decode_ogg(data),
        Codec::Hca => decode_hca(data),
        Codec::MsAdpcm => decode_ms_adpcm(data),
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

/// Microsoft ADPCM, from the standalone `.wav` ironworks wraps the codec's stream in: a `fmt `
/// chunk (WAVEFORMATEX plus the ADPCM coefficient table) and a `data` chunk of encoded blocks.
fn decode_ms_adpcm(data: &[u8]) -> Result<Decoded> {
    if data.get(0..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WAVE") {
        return Err(anyhow!("msadpcm: not a RIFF/WAVE stream"));
    }

    let (mut fmt, mut pcm) = (None, None);
    let mut pos: usize = 12;
    while let Some(header) = data.get(pos..pos.saturating_add(8)) {
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let body_start = pos
            .checked_add(8)
            .ok_or_else(|| anyhow!("msadpcm: chunk header overflows"))?;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| anyhow!("msadpcm: chunk size overflows"))?;
        let body = data
            .get(body_start..body_end)
            .ok_or_else(|| anyhow!("msadpcm: chunk extends past end of stream"))?;
        match &header[0..4] {
            b"fmt " => fmt = Some(body),
            b"data" => pcm = Some(body),
            _ => {}
        }
        let padded = size
            .checked_add(size % 2)
            .ok_or_else(|| anyhow!("msadpcm: chunk padding overflows"))?;
        pos = body_end
            .checked_add(padded - size)
            .ok_or_else(|| anyhow!("msadpcm: chunk padding overflows"))?;
        if fmt.is_some() && pcm.is_some() {
            break;
        }
    }
    let fmt = fmt.ok_or_else(|| anyhow!("msadpcm: missing fmt chunk"))?;
    let pcm = pcm.ok_or_else(|| anyhow!("msadpcm: missing data chunk"))?;

    if fmt.len() < 22 || u16::from_le_bytes(fmt[0..2].try_into().unwrap()) != 2 {
        return Err(anyhow!("msadpcm: fmt chunk is not WAVE_FORMAT_ADPCM"));
    }
    let channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
    let block_align = usize::from(u16::from_le_bytes(fmt[12..14].try_into().unwrap()));
    let bits_per_sample = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
    let num_coef = usize::from(u16::from_le_bytes(fmt[20..22].try_into().unwrap()));

    if bits_per_sample != 4 {
        return Err(anyhow!("msadpcm: unsupported bit depth {bits_per_sample}"));
    }
    if !matches!(channels, 1 | 2) {
        return Err(anyhow!("msadpcm: unsupported channel count {channels}"));
    }
    let header_size = 7usize
        .checked_mul(channels.into())
        .ok_or_else(|| anyhow!("msadpcm: channel count overflows"))?;
    if block_align <= header_size {
        return Err(anyhow!("msadpcm: block align too small for its own header"));
    }

    let coefficients: Vec<(i32, i32)> = (0..num_coef)
        .map_while(|index| {
            let at = 22usize.checked_add(index.checked_mul(4)?)?;
            let pair = fmt.get(at..at.checked_add(4)?)?;
            Some((
                i16::from_le_bytes(pair[0..2].try_into().unwrap()).into(),
                i16::from_le_bytes(pair[2..4].try_into().unwrap()).into(),
            ))
        })
        .collect();
    if coefficients.is_empty() {
        return Err(anyhow!("msadpcm: empty coefficient table"));
    }

    let mut samples = Vec::new();
    for block in pcm.chunks(block_align) {
        decode_adpcm_block(block, channels.into(), header_size, &coefficients, &mut samples)?;
    }

    Ok(Decoded {
        samples,
        channels,
        sample_rate,
        loop_start: None,
        loop_end: None,
    })
}

const MS_ADAPTATION_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

struct AdpcmChannel {
    coef1: i32,
    coef2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl AdpcmChannel {
    fn expand_nibble(&mut self, nibble: u8) -> i32 {
        let signed = if nibble & 0x08 != 0 {
            i32::from(nibble) - 16
        } else {
            i32::from(nibble)
        };
        let predicted = (self.sample1 * self.coef1 + self.sample2 * self.coef2) / 256
            + signed * self.delta;
        let predicted = predicted.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.sample2 = self.sample1;
        self.sample1 = predicted;
        self.delta = (MS_ADAPTATION_TABLE[usize::from(nibble)] * self.delta) / 256;
        self.delta = self.delta.max(16);
        predicted
    }
}

/// One block's preamble (predictor index, delta, and the two seed samples per channel) plus its
/// nibble stream, decoded and appended to `samples` as interleaved f32. Blocks carry no state
/// between them: each declares its own predictor and seed samples. A trailing block shorter than
/// a full `block_align` decodes however many whole frames its own byte count actually holds,
/// which is how a stream not evenly divisible by the block size ends without over-reading.
fn decode_adpcm_block(
    block: &[u8],
    channels: usize,
    header_size: usize,
    coefficients: &[(i32, i32)],
    samples: &mut Vec<f32>,
) -> Result<()> {
    if block.len() <= header_size {
        return Ok(());
    }
    let mut pos = 0;
    let mut states = Vec::with_capacity(channels);
    for _ in 0..channels {
        let predictor = usize::from(block[pos]);
        let &(coef1, coef2) = coefficients
            .get(predictor)
            .ok_or_else(|| anyhow!("msadpcm: predictor {predictor} exceeds coefficient table"))?;
        states.push(AdpcmChannel {
            coef1,
            coef2,
            delta: 0,
            sample1: 0,
            sample2: 0,
        });
        pos += 1;
    }
    for state in &mut states {
        state.delta = i16::from_le_bytes(block[pos..pos + 2].try_into().unwrap()).into();
        pos += 2;
    }
    for state in &mut states {
        state.sample1 = i16::from_le_bytes(block[pos..pos + 2].try_into().unwrap()).into();
        pos += 2;
    }
    for state in &mut states {
        state.sample2 = i16::from_le_bytes(block[pos..pos + 2].try_into().unwrap()).into();
        pos += 2;
    }

    for state in &states {
        samples.push(state.sample2 as f32 / 32768.0);
    }
    for state in &states {
        samples.push(state.sample1 as f32 / 32768.0);
    }

    match channels {
        1 => {
            let state = &mut states[0];
            for &byte in &block[pos..] {
                samples.push(state.expand_nibble(byte >> 4) as f32 / 32768.0);
                samples.push(state.expand_nibble(byte & 0x0F) as f32 / 32768.0);
            }
        }
        _ => {
            let (left, right) = states.split_at_mut(1);
            for &byte in &block[pos..] {
                samples.push(left[0].expand_nibble(byte >> 4) as f32 / 32768.0);
                samples.push(right[0].expand_nibble(byte & 0x0F) as f32 / 32768.0);
            }
        }
    }
    Ok(())
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
}
