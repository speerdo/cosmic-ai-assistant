//! Kokoro voice packs: one file per voice, discovered from the model
//! directory.
//!
//! A pack is a raw little-endian `f32` array of 510 × 256: one 256-wide
//! style vector per possible utterance length, indexed by the number of
//! phoneme tokens. The accent comes from the name's first letter (`a` =
//! American, `b` = British) and the gender from the second (`f`/`m`) — the
//! naming convention every Kokoro v1.0 voice follows.

use std::path::Path;

use crate::TtsError;
use crate::provider::{Accent, Gender, Voice};

pub(crate) const STYLE_DIM: usize = 256;
const ROWS: usize = 510;
const PACK_BYTES: usize = ROWS * STYLE_DIM * 4;

/// What `"default"` resolves to: `af_heart`, the voice Kokoro's own
/// model card grades highest.
pub(crate) const DEFAULT_VOICE: &str = "af_heart";

/// A loaded voice pack.
#[derive(Debug)]
pub(crate) struct Pack {
    styles: Vec<f32>,
}

impl Pack {
    pub(crate) fn from_bytes(id: &str, bytes: &[u8]) -> Result<Self, TtsError> {
        if bytes.len() != PACK_BYTES {
            return Err(TtsError::Synthesis(format!(
                "voice pack {id}: {} bytes, expected {PACK_BYTES} — re-run scripts/fetch-models",
                bytes.len()
            )));
        }
        let styles = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(Self { styles })
    }

    /// The style vector for an utterance of `tokens` phoneme tokens.
    pub(crate) fn style(&self, tokens: usize) -> &[f32] {
        let row = tokens.min(ROWS - 1);
        &self.styles[row * STYLE_DIM..(row + 1) * STYLE_DIM]
    }
}

/// English voices only: the two prefixes this build can phonemize.
pub(crate) fn describe(id: &str) -> Option<Voice> {
    let mut chars = id.chars();
    let accent = match chars.next()? {
        'a' => Accent::from_code("en-US"),
        'b' => Accent::from_code("en-GB"),
        _ => return None,
    };
    let gender = match chars.next()? {
        'f' => Some(Gender::Female),
        'm' => Some(Gender::Male),
        _ => return None,
    };
    let name = id.split_once('_')?.1;
    if name.is_empty() {
        return None;
    }
    let mut label = name.to_owned();
    label[..1].make_ascii_uppercase();
    Some(Voice {
        id: id.to_owned(),
        label,
        accent,
        gender,
        sample: None,
    })
}

pub(crate) fn is_british(id: &str) -> bool {
    id.starts_with('b')
}

/// Every English voice pack in `dir`, sorted by id.
pub(crate) fn discover(dir: &Path) -> Vec<Voice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut voices: Vec<Voice> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            describe(name.strip_suffix(".bin")?)
        })
        .collect();
    voices.sort_by(|a, b| a.id.cmp(&b.id));
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_accent_and_gender() {
        let v = describe("bm_george").unwrap();
        assert_eq!(v.accent.as_str(), "en-GB");
        assert_eq!(v.gender, Some(Gender::Male));
        assert_eq!(v.label, "George");
        assert_eq!(describe("af_heart").unwrap().accent.as_str(), "en-US");
        // Other languages and the unnamed blend are not offered.
        assert!(describe("ff_siwis").is_none());
        assert!(describe("af").is_none());
        assert!(describe("ax_foo").is_none());
    }

    #[test]
    fn pack_indexes_by_token_count_and_clamps() {
        let mut bytes = Vec::with_capacity(PACK_BYTES);
        for row in 0..ROWS {
            for _ in 0..STYLE_DIM {
                bytes.extend_from_slice(&(row as f32).to_le_bytes());
            }
        }
        let pack = Pack::from_bytes("t", &bytes).unwrap();
        assert_eq!(pack.style(0)[0], 0.0);
        assert_eq!(pack.style(42)[255], 42.0);
        assert_eq!(pack.style(10_000)[0], 509.0);
        assert!(Pack::from_bytes("t", &bytes[..100]).is_err());
    }
}
