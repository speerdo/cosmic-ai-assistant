//! The BPE vocab sherpa needs to tokenize hotwords (phase-3 findings §1f).
//!
//! NeMo archives ship only `tokens.txt` (`piece id` per line), and sherpa
//! looks a hotword up whole in it unless it has a SentencePiece vocab to
//! split words with. SentencePiece BPE merges in id order, so a vocab with
//! **score = −id** reproduces the model's own segmentation.

/// `tokens.txt` → `bpe.vocab` (`piece<TAB>score` per line). Special tokens
/// (`<unk>`, `<blk>`) are left out: no hotword should ever tokenize to them.
pub fn vocab_from_tokens(tokens: &str) -> String {
    let mut out = String::with_capacity(tokens.len() + tokens.len() / 2);
    for line in tokens.lines() {
        let Some((piece, id)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        if piece.starts_with('<') && piece.ends_with('>') {
            continue;
        }
        out.push_str(&format!("{piece}\t-{id}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_follow_ids_and_specials_are_dropped() {
        let tokens = "<unk> 0\n▁t 1\n▁th 2\nin 4\n<blk> 1024\n";
        assert_eq!(vocab_from_tokens(tokens), "▁t\t-1\n▁th\t-2\nin\t-4\n");
    }

    #[test]
    fn a_piece_that_is_a_space_survives() {
        // Some vocabs carry a bare "▁" piece; rsplit keeps it intact.
        assert_eq!(vocab_from_tokens("▁ 7\n"), "▁\t-7\n");
    }
}
