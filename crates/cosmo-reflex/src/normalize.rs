//! Transcript → words, shared with the ASR bench's scorer so a phrasing the
//! bench counts as correct is one the matcher understands.

/// Lowercase words without punctuation, number words as digits (via
/// `cosmo_stt::score::normalize`), and "work space" as one word: the
/// streaming models split it (phase-3 findings §6e).
pub fn words(text: &str) -> Vec<String> {
    let w = cosmo_stt::score::normalize(text);
    let mut out: Vec<String> = Vec::with_capacity(w.len());
    for word in w {
        if word == "space" && out.last().is_some_and(|p| p == "work") {
            out.pop();
            out.push("workspace".into());
        } else {
            out.push(word);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcripts_normalize_to_the_same_words() {
        assert_eq!(
            words("Move this window to Work Space Three."),
            ["move", "this", "window", "to", "workspace", "3"]
        );
        assert_eq!(
            words("Switch to workspace two"),
            ["switch", "to", "workspace", "2"]
        );
        assert_eq!(words("Open KeePassXC!"), ["open", "keepassxc"]);
    }
}
