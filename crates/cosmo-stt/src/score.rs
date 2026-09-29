//! Scoring transcripts against what was said (phase-3 spec §3.6).
//!
//! The candidates disagree on things that don't matter for a command:
//! casing, punctuation, "three" vs "3", "fifty percent" vs "50%". Both
//! sides are normalized before words are compared, so the word error rate
//! counts only real misrecognitions.

/// Lowercase words, punctuation dropped (apostrophes inside words kept),
/// hyphens as spaces, `%` as "percent", and number words as digits
/// ("twenty five" → "25").
pub fn normalize(text: &str) -> Vec<String> {
    let spaced = text.replace('%', " percent ").replace(['-', '/'], " ");
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    numbers_to_digits(join_spellings(words))
}

/// Spelled-out letters are the word: "p d f" → "pdf". And "per cent" is
/// "percent".
fn join_spellings(words: Vec<String>) -> Vec<String> {
    let letter = |w: &str| w.len() == 1 && w.chars().all(|c| c.is_ascii_alphabetic());
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut run = String::new();
    for (i, w) in words.iter().enumerate() {
        // "a" and "i" are words on their own unless a letter follows them
        // ("p d f i downloaded" is "pdf i downloaded").
        let next_letter = words.get(i + 1).is_some_and(|n| letter(n));
        let pronoun = matches!(w.as_str(), "a" | "i") && !next_letter;
        if letter(w) && !pronoun && (!run.is_empty() || next_letter) {
            run.push_str(w);
            continue;
        }
        if !run.is_empty() {
            out.push(std::mem::take(&mut run));
        }
        if w == "cent" && out.last().is_some_and(|p| p == "per") {
            out.pop();
            out.push("percent".into());
        } else {
            out.push(w.clone());
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

fn unit(w: &str) -> Option<u32> {
    const UNITS: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    UNITS.iter().position(|u| *u == w).map(|n| n as u32)
}

fn tens(w: &str) -> Option<u32> {
    const TENS: [&str; 8] = [
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    TENS.iter()
        .position(|t| *t == w)
        .map(|n| 20 + 10 * n as u32)
}

fn numbers_to_digits(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if let Some(t) = tens(w) {
            match words
                .get(i + 1)
                .and_then(|n| unit(n))
                .filter(|&u| (1..10).contains(&u))
            {
                Some(u) => {
                    out.push((t + u).to_string());
                    i += 2;
                }
                None => {
                    out.push(t.to_string());
                    i += 1;
                }
            }
            continue;
        }
        if w == "hundred"
            && out
                .last()
                .is_some_and(|p: &String| p.parse::<u32>().is_ok())
        {
            let n: u32 = out.pop().unwrap().parse().unwrap();
            out.push((n * 100).to_string());
        } else {
            out.push(unit(w).map_or_else(|| w.to_owned(), |n| n.to_string()));
        }
        i += 1;
    }
    out
}

/// Word-level edit distance between a reference and a hypothesis, both
/// already normalized: substitutions + deletions + insertions.
pub fn word_errors(reference: &[String], hypothesis: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut row = vec![i + 1; hypothesis.len() + 1];
        for (j, h) in hypothesis.iter().enumerate() {
            let substitute = prev[j] + usize::from(r != h);
            row[j + 1] = substitute.min(prev[j + 1] + 1).min(row[j] + 1);
        }
        prev = row;
    }
    prev[hypothesis.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Vec<String> {
        normalize(s)
    }

    #[test]
    fn formatting_differences_vanish() {
        assert_eq!(
            n("Open Firefox and move it to Workspace 3."),
            n("open firefox and move it to workspace three")
        );
        assert_eq!(
            n("Set the volume to 50%."),
            n("set the volume to fifty percent")
        );
        assert_eq!(n("Twenty-five"), ["25"]);
        assert_eq!(n("one hundred"), ["100"]);
        assert_eq!(n("I don't know!"), ["i", "don't", "know"]);
        assert_eq!(n("Pause... please"), ["pause", "please"]);
        assert_eq!(
            n("find the p d f i downloaded"),
            n("Find the PDF I downloaded")
        );
        assert_eq!(n("thirty per cent"), ["30", "percent"]);
        assert_eq!(n("a cat"), ["a", "cat"], "a lone letter stays a word");
    }

    #[test]
    fn numbers_only_merge_where_they_should() {
        assert_eq!(n("twenty"), ["20"]);
        assert_eq!(n("twenty ten"), ["20", "10"]);
        assert_eq!(n("seventy zero"), ["70", "0"]);
    }

    #[test]
    fn edit_distance_counts_each_kind_of_error() {
        let r = n("launch spotube now");
        assert_eq!(word_errors(&r, &r), 0);
        assert_eq!(word_errors(&r, &n("launch spot tuber now")), 2, "sub + ins");
        assert_eq!(word_errors(&r, &n("launch now")), 1, "deletion");
        assert_eq!(word_errors(&r, &[]), 3);
        assert_eq!(word_errors(&[], &n("hello")), 1);
    }
}
