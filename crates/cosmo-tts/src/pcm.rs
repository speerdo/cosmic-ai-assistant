//! The canonical audio buffer and its WAV encoding (spec §2.2).
//!
//! `Pcm` is the one shape every provider produces and every consumer
//! (playback, phrase cache) reads: mono `f32` in `[-1.0, 1.0]` plus a sample
//! rate. WAV files are the phrase cache's on-disk form, so encode/decode
//! lives beside the buffer. Decoding accepts the encodings real providers
//! hand over (16-bit PCM from OpenAI and Piper, float from Kokoro, stereo
//! from either) and collapses to canonical mono.

use std::io::Cursor;
use std::time::Duration;

use hound::{SampleFormat, WavReader, WavWriter};

use crate::TtsError;

/// Mono `f32` samples at a known rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub sample_rate: u32,
    /// Interleaved-free: one channel, `[-1.0, 1.0]`.
    pub data: Vec<f32>,
}

impl Pcm {
    pub fn new(sample_rate: u32, data: Vec<f32>) -> Self {
        Self { sample_rate, data }
    }

    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.data.len() as f64 / f64::from(self.sample_rate))
    }

    /// Encode as 16-bit mono PCM WAV — the phrase cache's file format.
    ///
    /// A zero sample rate is a structured error, not a panic: `hound`
    /// divides by the rate when it writes the `fmt` chunk, and the phrase
    /// cache encodes whatever a provider hands back.
    pub fn to_wav_bytes(&self) -> Result<Vec<u8>, TtsError> {
        if self.sample_rate == 0 {
            return Err(TtsError::Wav("encode: sample rate is zero".into()));
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut buf, spec)
                .map_err(|e| TtsError::Wav(format!("encode: {e}")))?;
            for s in &self.data {
                writer
                    .write_sample(f32_to_i16(*s))
                    .map_err(|e| TtsError::Wav(format!("encode: {e}")))?;
            }
            writer
                .finalize()
                .map_err(|e| TtsError::Wav(format!("encode: {e}")))?;
        }
        Ok(buf.into_inner())
    }

    /// Decode a WAV into canonical mono. Handles 8/16/24/32-bit integer PCM
    /// and 32-bit float, at any channel count (stereo averages down).
    pub fn from_wav_bytes(bytes: &[u8]) -> Result<Self, TtsError> {
        let mut reader = WavReader::new(Cursor::new(bytes))
            .map_err(|e| TtsError::Wav(format!("decode: {e}")))?;
        let spec = reader.spec();
        if spec.sample_rate == 0 {
            return Err(TtsError::Wav("decode: sample rate is zero".into()));
        }
        let channels = usize::from(spec.channels.max(1));
        // Integer samples arrive at their own depth: `hound` sign-extends a
        // 24-bit sample into an `i32` in ±2^23 rather than widening it to
        // the `i32` range, so the scale comes from `bits_per_sample` and
        // never from the carrier type.
        let bits = spec.bits_per_sample;
        let flat: Vec<f32> = match (spec.sample_format, bits) {
            (SampleFormat::Int, 8) => reader
                .samples::<i8>()
                .map(|s| s.map(|v| int_to_f32(i32::from(v), 8)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(wav_decode)?,
            (SampleFormat::Int, 16) => reader
                .samples::<i16>()
                .map(|s| s.map(|v| int_to_f32(i32::from(v), 16)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(wav_decode)?,
            (SampleFormat::Int, 24 | 32) => reader
                .samples::<i32>()
                .map(|s| s.map(|v| int_to_f32(v, bits)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(wav_decode)?,
            (SampleFormat::Float, 32) => reader
                .samples::<f32>()
                .map(|s| s.map(|v| v.clamp(-1.0, 1.0)).map_err(wav_decode))
                .collect::<Result<Vec<_>, _>>()?,
            (format, bits) => {
                return Err(TtsError::Wav(format!(
                    "decode: unsupported WAV format {format:?} at {bits} bits"
                )));
            }
        };
        let data = if channels <= 1 {
            flat
        } else {
            flat.chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
                .collect()
        };
        Ok(Self {
            sample_rate: spec.sample_rate,
            data,
        })
    }
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

/// Scale an integer sample of `bits` depth into canonical `[-1.0, 1.0]`.
///
/// The divisor is the positive full scale (`2^(bits-1) - 1`), which mirrors
/// [`f32_to_i16`] so a WAV round trip stays symmetric. The clamp covers the
/// one asymmetric input — the negative full scale is a step past `-1.0` —
/// so [`Pcm`]'s documented range holds for every file we accept.
fn int_to_f32(v: i32, bits: u16) -> f32 {
    debug_assert!((2..=32).contains(&bits), "bit depth {bits} out of range");
    let full_scale = ((1i64 << (bits - 1)) - 1) as f32;
    (v as f32 / full_scale).clamp(-1.0, 1.0)
}

fn wav_decode(e: hound::Error) -> TtsError {
    TtsError::Wav(format!("decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 24_000;

    fn sine(rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin())
            .collect()
    }

    #[test]
    fn wav_round_trip_preserves_samples_and_rate() {
        let pcm = Pcm::new(RATE, sine(RATE, 480));
        let bytes = pcm.to_wav_bytes().unwrap();
        let back = Pcm::from_wav_bytes(&bytes).unwrap();
        assert_eq!(back.sample_rate, RATE);
        assert_eq!(back.data.len(), pcm.data.len());
        // 16-bit quantization: within one step everywhere.
        for (a, b) in pcm.data.iter().zip(&back.data) {
            assert!((a - b).abs() < 1.0 / 32767.0, "{a} vs {b}");
        }
        assert_eq!(back.duration(), pcm.duration());
    }

    #[test]
    fn stereo_wav_downmixes_to_mono() {
        // A 2-channel 16-bit file by hand: left = 1.0, right = -1.0 → mean 0.
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = WavWriter::new(&mut buf, spec).unwrap();
            w.write_sample(i16::MAX).unwrap();
            w.write_sample(i16::MIN).unwrap();
            w.finalize().unwrap();
        }
        let pcm = Pcm::from_wav_bytes(buf.into_inner().as_slice()).unwrap();
        assert_eq!(pcm.data.len(), 1);
        // i16::MIN scales to a step past -1.0 and is clamped back to it, so
        // the two channels cancel exactly.
        assert_eq!(pcm.data[0], 0.0, "downmix should cancel");
    }

    #[test]
    fn float_wav_decodes() {
        // What Kokoro hands over: 32-bit float, 24 kHz.
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = WavWriter::new(&mut buf, spec).unwrap();
            for s in [0.25f32, -0.5, 1.0] {
                w.write_sample(s).unwrap();
            }
            w.finalize().unwrap();
        }
        let pcm = Pcm::from_wav_bytes(buf.into_inner().as_slice()).unwrap();
        assert_eq!(pcm.sample_rate, RATE);
        assert_eq!(pcm.data, vec![0.25, -0.5, 1.0]);
    }

    #[test]
    fn garbage_is_a_structured_error() {
        let err = Pcm::from_wav_bytes(b"not a wav at all").unwrap_err();
        assert!(matches!(err, TtsError::Wav(_)));
        assert!(err.to_string().starts_with("wav:"));
    }

    /// Regression: `hound` sign-extends a 24-bit sample into an `i32` in
    /// ±2^23 without widening it, so scaling by `i32::MAX` decoded 24-bit
    /// files 256x too quiet — near-silence, not an obvious failure.
    #[test]
    fn int_24_bit_decodes_at_full_scale() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 24,
            sample_format: SampleFormat::Int,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = WavWriter::new(&mut buf, spec).unwrap();
            for s in [8_388_607i32, -4_194_304, 0] {
                w.write_sample(s).unwrap();
            }
            w.finalize().unwrap();
        }
        let pcm = Pcm::from_wav_bytes(buf.into_inner().as_slice()).unwrap();
        assert_eq!(pcm.data[0], 1.0, "24-bit positive full scale");
        assert!((pcm.data[1] + 0.5).abs() < 1e-6, "{}", pcm.data[1]);
        assert_eq!(pcm.data[2], 0.0);
    }

    /// `Pcm::data` documents `[-1.0, 1.0]`; the negative full scale of every
    /// integer depth sits one step outside it, so decoding clamps.
    #[test]
    fn decoded_samples_stay_in_range() {
        for (bits, min) in [(8u16, i32::from(i8::MIN)), (16, i32::from(i16::MIN))] {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: RATE,
                bits_per_sample: bits,
                sample_format: SampleFormat::Int,
            };
            let mut buf = Cursor::new(Vec::new());
            {
                let mut w = WavWriter::new(&mut buf, spec).unwrap();
                w.write_sample(min).unwrap();
                w.finalize().unwrap();
            }
            let pcm = Pcm::from_wav_bytes(buf.into_inner().as_slice()).unwrap();
            assert_eq!(pcm.data[0], -1.0, "{bits}-bit negative full scale");
        }
    }

    /// hound divides by the sample rate when it writes the `fmt` chunk, so
    /// an unchecked zero rate panicked inside the writer. The voice layer
    /// owes its callers a structured error — the phrase cache encodes
    /// whatever a provider returns.
    #[test]
    fn zero_rate_encode_is_a_structured_error() {
        let err = Pcm::new(0, vec![0.1, -0.1]).to_wav_bytes().unwrap_err();
        assert!(matches!(err, TtsError::Wav(_)));
        assert!(err.to_string().contains("sample rate is zero"), "{err}");
    }

    #[test]
    fn duration_of_empty_is_zero() {
        assert_eq!(Pcm::new(RATE, vec![]).duration(), Duration::ZERO);
        assert_eq!(Pcm::new(0, vec![0.0]).duration(), Duration::ZERO);
    }
}
