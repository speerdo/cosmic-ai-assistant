//! Sentence splitting for streamed synthesis (spec §2.10).
//!
//! Phase 5 feeds the TTS one sentence at a time so first audio lands in a
//! few hundred milliseconds instead of after the whole reply. This is that
//! splitter, built early so the streaming API settles on real boundaries.
//!
//! It is deliberately a **heuristic over ordinary prose** — assistant
//! replies, not legal documents. The cost of an over-split is a slightly
//! early pause; the cost of an under-split is latency. The rules:
//!
//! - A terminator followed by whitespace splits when something sentence-like
//!   follows — except a single `.` after a known abbreviation (`Mr.`, `e.g.`,
//!   the data list below) or an initial (`J. R. R.`), and except a lowercase
//!   continuation (`one. two.` stays together).
//! - Runs (`...`, `!!`) and `…` are strong: they split regardless of case.
//! - `!`/`?` that sit directly against a closing quote or bracket were
//!   interior to a quotation or parenthetical (`He said "Go now!"` vs
//!   `(really!)`), so they split only before a sentence-like start.
//! - Decimals and glued text (`3.14`, `wait...what`) never split; a blank
//!   line is a boundary even without a terminator.
//! - A number that is the first thing on its line is an **ordered-list
//!   marker**, not a sentence: `1. First step. 2. Second step.` yields two
//!   chunks, not four. Scoped to line-initial so `Shipped in 2024. Then…`
//!   still splits.

/// Abbreviations after which a single `.` does not end a sentence. Matched
/// case-insensitively against the whitespace-delimited token before the dot,
/// quotes/brackets stripped. **Data, not code** — extend here, not in the
/// scanner. Deliberately conservative: only tokens that are almost never the
/// last word of a sentence (`may` as a month is *not* here for exactly that
/// reason; neither is `no`, which sentences end on constantly, nor `etc`,
/// which ends list sentences constantly — "milk, etc. Then come home").
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "mx", "dr", "prof", "sr", "jr", "st", "mt", "rev", "hon", "vs", "e.g",
    "i.e", "cf", "al", "fig", "vol", "pp", "ed", "approx", "dept", "est", "inc", "ltd", "corp",
    "univ", "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
    "u.s", "u.k", "ph.d",
];

/// Split `text` into sentences, terminators (and any closing quotes or
/// brackets) attached, whitespace trimmed, empty chunks dropped. Input
/// without a boundary comes back as one chunk.
pub fn split(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        let (pos, c) = chars[i];

        // Blank line = paragraph boundary, terminator or not.
        if c == '\n' {
            let mut j = i;
            let mut newlines = 0;
            while j < chars.len() && chars[j].1.is_whitespace() {
                if chars[j].1 == '\n' {
                    newlines += 1;
                }
                j += 1;
            }
            if newlines >= 2 {
                push_chunk(&mut out, &text[start..pos]);
                start = chars.get(j).map_or(text.len(), |(p, _)| *p);
                i = j;
                continue;
            }
        }

        if !is_terminator(c) {
            i += 1;
            continue;
        }

        // One terminator run ("...", "…", "!!") acts as a single boundary,
        // then any closing quotes/brackets stay attached to the chunk.
        let mut j = i;
        while j + 1 < chars.len() && chars[j + 1].1 == c {
            j += 1;
        }
        let run_len = j - i + 1;
        let mut end = j;
        let mut closing = false;
        while end + 1 < chars.len() && is_closing(chars[end + 1].1) {
            end += 1;
            closing = true;
        }
        let boundary_end = chars[end].0 + chars[end].1.len_utf8();

        let boundary = match chars.get(end + 1) {
            // End of input: the run closes the final chunk.
            None => true,
            // Glued text ("3.14", "wait...what") never splits.
            Some((_, next)) if !next.is_whitespace() => false,
            Some(_) => {
                let mut k = end + 1;
                while k < chars.len() && chars[k].1.is_whitespace() {
                    k += 1;
                }
                match chars.get(k) {
                    // Nothing but whitespace follows: chunk ends here.
                    None => true,
                    Some((_, first)) => {
                        allows_boundary(text, start, pos, c, *first, run_len, closing)
                    }
                }
            }
        };

        if boundary {
            push_chunk(&mut out, &text[start..boundary_end]);
            start = boundary_end;
        }
        i = end + 1;
    }

    if start < text.len() {
        push_chunk(&mut out, &text[start..]);
    }
    out
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

/// A quote/bracket that closes something opened earlier in the sentence —
/// the terminator sat inside a quotation or parenthetical.
fn is_closing(c: char) -> bool {
    matches!(c, '"' | '\'' | '”' | '’' | ')' | ']' | '}' | '»')
}

fn allows_boundary(
    text: &str,
    chunk_start: usize,
    term_pos: usize,
    term: char,
    next: char,
    run_len: usize,
    after_closing: bool,
) -> bool {
    match term {
        // "..." / "…" are strong regardless of what follows.
        '.' if run_len >= 2 => true,
        // A single '.' must start something sentence-like; the exceptions
        // are the abbreviation cases.
        '.' => is_sentence_start(next) && !is_abbreviation(text, chunk_start, term_pos),
        // '!'/'?' straight into whitespace are strong; after a closing
        // quote/bracket they were interior, so require a sentence-like start.
        _ => !after_closing || is_sentence_start(next),
    }
}

fn is_sentence_start(c: char) -> bool {
    c.is_uppercase()
        || c.is_numeric()
        || matches!(c, '"' | '\'' | '“' | '‘' | '(' | '[' | '{' | '«')
}

/// The whitespace-delimited token ending just before the terminator, with
/// surrounding quotes/brackets stripped and lower-cased.
fn is_abbreviation(text: &str, chunk_start: usize, term_pos: usize) -> bool {
    let before = &text[chunk_start..term_pos];
    let token = before
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or_default()
        .trim_matches(|c| {
            matches!(
                c,
                '"' | '\'' | '“' | '”' | '‘' | '’' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
        .to_ascii_lowercase();
    ABBREVIATIONS.contains(&token.as_str())
        // A single alphabetic character: "J. R. R. Tolkien".
        || token.chars().count() == 1 && token.chars().all(char::is_alphabetic)
        || is_list_marker(before, &token)
}

/// An ordered-list marker: a short number that is the first thing on its
/// line. Assistant replies are full of `1. Do this. 2. Do that.`, and
/// treating the marker as a sentence makes the voice say "One." on its own,
/// with a pause, before the item.
///
/// Line-initial is what keeps this from swallowing real boundaries — in
/// `Shipped in 2024. Then it stuck.` the number is not the start of its
/// line, so that still splits.
fn is_list_marker(before: &str, token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 3
        && token.bytes().all(|b| b.is_ascii_digit())
        && before.rsplit('\n').next().unwrap_or(before).trim_start() == token
}

fn push_chunk(out: &mut Vec<String>, chunk: &str) {
    let trimmed = chunk.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_sentences_split_and_keep_terminators() {
        assert_eq!(
            split("One here. Two there. Three done."),
            vec!["One here.", "Two there.", "Three done."]
        );
    }

    #[test]
    fn exclamations_and_questions_split_regardless_of_case() {
        assert_eq!(
            split("Wow!! really? yes!"),
            vec!["Wow!!", "really?", "yes!"]
        );
    }

    #[test]
    fn abbreviations_do_not_split() {
        assert_eq!(
            split("Mr. Smith met Dr. Jones at St. James's Park."),
            vec!["Mr. Smith met Dr. Jones at St. James's Park."]
        );
        assert_eq!(
            split("Use e.g. tools, i.e. equipment. Bring them."),
            vec!["Use e.g. tools, i.e. equipment.", "Bring them."]
        );
    }

    #[test]
    fn initials_do_not_split() {
        assert_eq!(
            split("J. R. R. Tolkien wrote it. So did U. Le Guin."),
            vec!["J. R. R. Tolkien wrote it.", "So did U. Le Guin."]
        );
    }

    #[test]
    fn decimals_never_split() {
        // ("approx." would deliberately *not* split here — it is in the
        // abbreviation list — so the corpus uses a plain word.)
        assert_eq!(
            split("Pi is 3.14 exactly. Tau is 6.28."),
            vec!["Pi is 3.14 exactly.", "Tau is 6.28."]
        );
        // Glued terminator with no space: not a boundary at all.
        assert_eq!(split("version 3.14works"), vec!["version 3.14works"]);
    }

    #[test]
    fn quotes_attach_to_their_sentence_but_parentheticals_hold() {
        // The '!' closes a quotation and a sentence follows: split.
        assert_eq!(
            split("He said \"Go now!\" Then he left."),
            vec!["He said \"Go now!\"", "Then he left."]
        );
        // The '!' closes a mid-sentence parenthetical, lowercase follows: hold.
        assert_eq!(
            split("She wrote (really!) about it. Later."),
            vec!["She wrote (really!) about it.", "Later."]
        );
    }

    #[test]
    fn ellipsis_is_one_strong_boundary() {
        assert_eq!(
            split("Wait… what happened next? Nobody knows."),
            vec!["Wait…", "what happened next?", "Nobody knows."]
        );
        assert_eq!(split("hmm... okay"), vec!["hmm...", "okay"]);
    }

    #[test]
    fn blank_line_is_a_boundary_without_a_terminator() {
        assert_eq!(
            split("Shopping list:\n\n- milk\n- eggs"),
            vec!["Shopping list:", "- milk\n- eggs"]
        );
    }

    #[test]
    fn single_newline_is_not_a_boundary() {
        assert_eq!(split("line one\nline two."), vec!["line one\nline two."]);
    }

    #[test]
    fn unterminated_text_comes_back_whole() {
        assert_eq!(split("just one sentence"), vec!["just one sentence"]);
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split("   "), Vec::<String>::new());
    }

    #[test]
    fn lowercase_after_a_period_does_not_split() {
        // Protects the abbreviations the list does not know at the cost of
        // missing rare lowercase sentence starts.
        assert_eq!(split("one. two."), vec!["one. two."]);
    }

    #[test]
    fn unicode_survives_offsets() {
        assert_eq!(
            split("Café naïve — résumé done. Über NEXT ✓."),
            vec!["Café naïve — résumé done.", "Über NEXT ✓."]
        );
    }

    #[test]
    fn month_abbreviations_hold_but_full_may_does_not() {
        assert_eq!(split("In May. Then June."), vec!["In May.", "Then June."]);
        assert_eq!(
            split("It was Sept. 3 when we met."),
            vec!["It was Sept. 3 when we met."]
        );
    }

    /// Ordered lists are everywhere in assistant replies. Treating "1." as a
    /// sentence makes phase 5 synthesize it as its own utterance — the voice
    /// says "One." and pauses before the item.
    #[test]
    fn ordered_list_markers_are_not_sentences() {
        assert_eq!(
            split("1. First step. 2. Second step. 3. Done."),
            vec!["1. First step.", "2. Second step.", "3. Done."]
        );
        // The common markdown shape: marker after a newline, not at the
        // start of the chunk.
        assert_eq!(
            split("Steps:\n1. Open it. 2. Run it."),
            vec!["Steps:\n1. Open it.", "2. Run it."]
        );
    }

    /// The counterpart: a number that is *not* line-initial is an ordinary
    /// sentence-final word and must still split.
    #[test]
    fn a_number_mid_line_still_ends_its_sentence() {
        assert_eq!(
            split("Shipped in 2024. Then it stuck."),
            vec!["Shipped in 2024.", "Then it stuck."]
        );
        assert_eq!(
            split("It failed with exit code 1. I'll retry."),
            vec!["It failed with exit code 1.", "I'll retry."]
        );
    }

    /// `etc.` fails the abbreviation list's own criterion — it ends list
    /// sentences constantly — so it is not on it.
    #[test]
    fn etc_ends_a_sentence_when_a_new_one_follows() {
        assert_eq!(
            split("Bring bread, milk, etc. Then come home."),
            vec!["Bring bread, milk, etc.", "Then come home."]
        );
        // Lowercase continuation still holds it together, as for any word.
        assert_eq!(
            split("Bring bread, etc. and then go."),
            vec!["Bring bread, etc. and then go."]
        );
    }

    #[test]
    fn chunks_are_nonempty_and_trimmed() {
        assert_eq!(split("One.  \n  Two.   "), vec!["One.", "Two."]);
    }
}
