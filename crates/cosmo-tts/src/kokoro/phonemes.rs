//! Text → Kokoro phoneme string → token ids.
//!
//! Kokoro v1.0 was trained on Misaki's phoneme set: IPA plus single-letter
//! diphthongs (`A` = eɪ, `I` = aɪ, `O` = oʊ, `W` = aʊ, `Y` = ɔɪ, `Q` = British
//! əʊ) and `ʤ`/`ʧ` affricates. eSpeak writes those as tied pairs (`e^ɪ`,
//! `d^ʒ`), so [`to_kokoro`] rewrites eSpeak's output the way Misaki's own
//! eSpeak fallback does. Without it, the tokenizer would still accept the
//! loose IPA letters, but as two tokens the model was never trained to read
//! as one sound.
//!
//! Punctuation carries prosody for Kokoro (a comma is a pause, a `?` lifts
//! the pitch), and eSpeak discards it, so [`phonemize`] splits the text on
//! punctuation, phonemizes the runs between, and puts the marks back.

use std::collections::HashMap;

use crate::TtsError;

use super::espeak;

/// Replacements for both accents, applied in order. Longer keys come
/// before any key they contain.
const COMMON: &[(&str, &str)] = &[
    // Glottal stop + syllabic n ("button" → bʌʔn̩) is a t + reduced n.
    ("ʔˌn\u{0329}", "tᵊn"),
    ("ʔn\u{0329}", "tᵊn"),
    ("ʔn", "tᵊn"),
    ("ʔ", "t"),
    // Triphthongs before the diphthongs they start with ("fire", "hour").
    ("a^ɪ^ɚ", "Iəɹ"),
    ("a^ɪ^ə", "Iə"),
    ("a^ʊ^ɚ", "Wəɹ"),
    ("a^ʊ^ə", "Wə"),
    ("a^ɪ", "I"),
    ("a^ʊ", "W"),
    ("e^ɪ", "A"),
    ("ɔ^ɪ", "Y"),
    ("d^ʒ", "ʤ"),
    ("t^ʃ", "ʧ"),
    ("ə^l", "ᵊl"),
    ("ɚ", "əɹ"),
    ("r", "ɹ"),
    ("x", "k"),
    ("ç", "k"),
    ("ɐ", "ə"),
    ("ɬ", "l"),
    ("ʲ", ""),
];

/// British: applied before [`COMMON`].
const GB: &[(&str, &str)] = &[
    ("e^ə", "ɛː"),
    ("i^ə", "ɪə"),
    ("ʊ^ə", "ʊə"),
    ("ə^ʊ", "Q"),
    ("o^ʊ", "Q"),
    ("iə", "ɪə"),
];

/// American: applied before [`COMMON`]. Misaki's US set has no length
/// marks, and spells the r-coloured NURSE vowel `ɜɹ`.
const US: &[(&str, &str)] = &[
    ("o^ʊ", "O"),
    ("ɜː^ɹ", "ɜɹ"),
    ("ɜːɹ", "ɜɹ"),
    ("ɜː", "ɜɹ"),
    ("ɪə", "iə"),
];

/// Punctuation Kokoro's vocabulary knows, kept in the phoneme string.
fn is_kept_punct(c: char) -> bool {
    matches!(
        c,
        ';' | ':' | ',' | '.' | '!' | '?' | '—' | '…' | '"' | '(' | ')' | '“' | '”'
    )
}

/// Rewrite one run of eSpeak IPA (with `^` ties) into Kokoro's set.
pub(crate) fn to_kokoro(ipa: &str, british: bool) -> String {
    let mut s = ipa.to_owned();
    for (from, to) in if british { GB } else { US } {
        s = s.replace(from, to);
    }
    for (from, to) in COMMON {
        s = s.replace(from, to);
    }
    // Any other syllabic consonant: n̩ → ᵊn.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if chars.peek() == Some(&'\u{0329}') {
            chars.next();
            out.push('ᵊ');
        }
        out.push(c);
    }
    let mut s = out.replace('^', "");
    if !british {
        s = s.replace('ː', "");
    }
    // eSpeak < 1.52 writes a bare `o` where Misaki has `ɔ`.
    s.replace('o', "ɔ")
}

/// A mark only counts as punctuation at a boundary: `12.5`, `1,000` and
/// `10:30` stay whole for eSpeak to read as numbers.
fn splits_here(c: char, next: Option<char>) -> bool {
    if !is_kept_punct(c) {
        return false;
    }
    match c {
        '.' | ',' | ':' => next.is_none_or(char::is_whitespace),
        _ => true,
    }
}

/// Text → Kokoro phoneme string, punctuation preserved.
pub(crate) fn phonemize(text: &str, british: bool) -> Result<String, TtsError> {
    let voice = if british {
        espeak::VOICE_GB
    } else {
        espeak::VOICE_US
    };
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| -> Result<(), TtsError> {
        if !run.trim().is_empty() {
            let ipa = espeak::ipa(run, voice)?;
            if !ipa.is_empty() {
                if !out.is_empty() && !out.ends_with([' ', '(', '"', '“']) {
                    out.push(' ');
                }
                out.push_str(&to_kokoro(&ipa, british));
            }
        }
        run.clear();
        Ok(())
    };
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if splits_here(c, chars.get(i + 1).copied()) {
            flush(&mut run, &mut out)?;
            if matches!(c, '(' | '"' | '“') && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(c);
        } else {
            run.push(c);
        }
    }
    flush(&mut run, &mut out)?;
    Ok(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Phoneme → token id, from the model's `tokenizer.json` (data, not code:
/// a re-export of the model brings its own table).
#[derive(Debug, Clone)]
pub(crate) struct Vocab {
    ids: HashMap<char, i64>,
}

/// Kokoro's context: 512 positions, two of them the `$` pads.
pub(crate) const MAX_TOKENS: usize = 510;

impl Vocab {
    pub(crate) fn from_tokenizer_json(json: &str) -> Result<Self, TtsError> {
        let parsed: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| TtsError::Synthesis(format!("tokenizer.json: {e}")))?;
        let map = parsed["model"]["vocab"]
            .as_object()
            .ok_or_else(|| TtsError::Synthesis("tokenizer.json: no model.vocab".into()))?;
        let mut ids = HashMap::with_capacity(map.len());
        for (k, v) in map {
            let mut chars = k.chars();
            if let (Some(c), None, Some(id)) = (chars.next(), chars.next(), v.as_i64()) {
                ids.insert(c, id);
            }
        }
        if !ids.contains_key(&'$') {
            return Err(TtsError::Synthesis("tokenizer.json: no `$` pad".into()));
        }
        Ok(Self { ids })
    }

    /// Token ids for `phonemes`, without the pads. Characters outside the
    /// vocabulary are dropped — the tokenizer's own normalizer does the
    /// same — and counted so a mapping gap shows up in the logs.
    pub(crate) fn encode(&self, phonemes: &str) -> Vec<i64> {
        let mut dropped = 0usize;
        let ids = phonemes
            .chars()
            .filter_map(|c| {
                let id = self.ids.get(&c).copied();
                dropped += usize::from(id.is_none());
                id
            })
            .collect();
        if dropped > 0 {
            tracing::debug!(dropped, "phonemes outside the Kokoro vocabulary");
        }
        ids
    }

    pub(crate) fn pad(&self) -> i64 {
        self.ids[&'$']
    }
}

/// Cut a token sequence longer than [`MAX_TOKENS`] at the last space (id
/// of `' '`) before the limit, so no word is split across two inferences.
pub(crate) fn chunk_tokens(tokens: &[i64], space: Option<i64>) -> Vec<&[i64]> {
    let mut chunks = Vec::new();
    let mut rest = tokens;
    while rest.len() > MAX_TOKENS {
        let cut = space
            .and_then(|sp| rest[..MAX_TOKENS].iter().rposition(|&t| t == sp))
            .filter(|&i| i > 0)
            .unwrap_or(MAX_TOKENS);
        let (head, tail) = rest.split_at(cut);
        chunks.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

impl Vocab {
    pub(crate) fn space(&self) -> Option<i64> {
        self.ids.get(&' ').copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real eSpeak 1.51 output (en-us, `^` ties) captured on the dev box.
    #[test]
    fn us_diphthongs_affricates_and_rhotics() {
        assert_eq!(
            to_kokoro("həlˈo^ʊ ˈædəm ðɪs ɹᵻplˈa^ɪ kˈe^ɪm", false),
            "həlˈO ˈædəm ðɪs ɹᵻplˈI kˈAm"
        );
        assert_eq!(
            to_kokoro("t^ʃˈɜːt^ʃ d^ʒˈʌd^ʒ d^ʒˈɔ^ɪ bˈɜːd", false),
            "ʧˈɜɹʧ ʤˈʌʤ ʤˈY bˈɜɹd"
        );
        // "Firefox": the triphthong must not be eaten by `a^ɪ` first.
        assert_eq!(to_kokoro("fˈa^ɪ^ɚfɑːks", false), "fˈIəɹfɑks");
        // "button", "bottle": glottal + syllabic n, dark l.
        assert_eq!(to_kokoro("bˈʌʔn\u{0329} bˈɑːɾə^l", false), "bˈʌtᵊn bˈɑɾᵊl");
        // Length marks are not in Misaki's US set.
        assert_eq!(to_kokoro("θɹuː dˈiːmən", false), "θɹu dˈimən");
    }

    #[test]
    fn gb_keeps_length_and_uses_q() {
        assert_eq!(
            to_kokoro("ɡˌə^ʊ tə ðə bˈɜːd nˌi^ə ðə kˈɑː ʃˈʊ^ə", true),
            "ɡˌQ tə ðə bˈɜːd nˌɪə ðə kˈɑː ʃˈʊə"
        );
        assert_eq!(to_kokoro("skˈe^ə", true), "skˈɛː");
    }

    #[test]
    fn numbers_and_times_are_not_split_on_their_punctuation() {
        assert!(!splits_here('.', Some('5')));
        assert!(!splits_here(',', Some('0')));
        assert!(!splits_here(':', Some('3')));
        assert!(splits_here('.', None));
        assert!(splits_here(',', Some(' ')));
        assert!(splits_here('?', Some('x')));
        assert!(!splits_here('-', Some(' ')));
    }

    fn vocab() -> Vocab {
        Vocab::from_tokenizer_json(r#"{"model":{"vocab":{"$":0," ":16,"a":43,"b":44,"ˈ":156}}}"#)
            .unwrap()
    }

    #[test]
    fn encode_drops_unknowns() {
        let v = vocab();
        assert_eq!(v.encode("ˈab ?"), vec![156, 43, 44, 16]);
        assert_eq!(v.pad(), 0);
    }

    #[test]
    fn tokenizer_without_pad_is_refused() {
        assert!(Vocab::from_tokenizer_json(r#"{"model":{"vocab":{"a":1}}}"#).is_err());
        assert!(Vocab::from_tokenizer_json("{}").is_err());
    }

    #[test]
    fn long_sequences_cut_at_spaces() {
        let mut tokens = vec![1i64; 400];
        tokens.push(16);
        tokens.extend(vec![1i64; 300]);
        let chunks = chunk_tokens(&tokens, Some(16));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 400);
        assert_eq!(chunks[1].len(), 301);
        assert!(chunks.iter().all(|c| c.len() <= MAX_TOKENS));

        // No space at all: hard cut, still bounded.
        let solid = vec![1i64; 1200];
        let chunks = chunk_tokens(&solid, Some(16));
        assert_eq!(
            chunks.iter().map(|c| c.len()).collect::<Vec<_>>(),
            [510, 510, 180]
        );
        assert!(chunk_tokens(&[], Some(16)).is_empty());
    }
}
