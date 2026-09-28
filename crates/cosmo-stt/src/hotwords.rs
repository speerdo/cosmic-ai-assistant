//! Hotword biasing sets (phase-3 spec §3.5): phrases the offline beam
//! search is nudged toward, chosen by the focused window's `app_id`.
//!
//! The mechanism lands in phase 3; phase 4 (the reflex path) owns the
//! final phrase list. Today's sources are whatever the caller supplies plus
//! the names of installed applications, read from their `.desktop` files.
//!
//! Phrases are kept in **display casing**: sherpa copies a hotword's casing
//! into the transcript (phase-3 findings §1f), so "firefox" would turn a
//! correct "Firefox" lowercase.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Longest phrase kept, in words. App names past this ("GNU Image
/// Manipulation Program") are never said in full, and a long hotword
/// biases toward a sequence nobody speaks.
const MAX_WORDS: usize = 4;

/// The biasing vocabulary: a base set for every window, plus extra phrases
/// for particular `app_id`s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hotwords {
    base: BTreeSet<String>,
    per_app: HashMap<String, BTreeSet<String>>,
}

impl Hotwords {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add phrases biased for in every window. Unusable ones are dropped
    /// (see [`clean`]).
    pub fn add<I, S>(&mut self, phrases: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.base
            .extend(phrases.into_iter().filter_map(|p| clean(p.as_ref())));
        self
    }

    /// Add phrases biased for only while `app_id` is focused.
    pub fn add_for_app<I, S>(&mut self, app_id: &str, phrases: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.per_app
            .entry(app_id.to_owned())
            .or_default()
            .extend(phrases.into_iter().filter_map(|p| clean(p.as_ref())));
        self
    }

    /// The set for a recording made while `app_id` is focused, in the form
    /// sherpa takes: one phrase per line. Empty when there is nothing to
    /// bias toward.
    pub fn for_app(&self, app_id: Option<&str>) -> String {
        let extra = app_id.and_then(|id| self.per_app.get(id));
        let all: BTreeSet<&str> = self
            .base
            .iter()
            .chain(extra.into_iter().flatten())
            .map(String::as_str)
            .collect();
        all.into_iter().collect::<Vec<_>>().join("\n")
    }

    pub fn len(&self) -> usize {
        self.base.len() + self.per_app.values().map(BTreeSet::len).sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A phrase as sherpa can use it, or `None`. Whitespace is collapsed;
/// phrases with characters the English models don't spell (symbols,
/// non-Latin scripts), or longer than [`MAX_WORDS`], are dropped rather
/// than handed to the tokenizer to fail on.
pub fn clean(phrase: &str) -> Option<String> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.is_empty() || words.len() > MAX_WORDS {
        return None;
    }
    let ok = words
        .iter()
        .flat_map(|w| w.chars())
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '\'' | '-' | '.'));
    // At least one letter: "2048" is a game, but nobody hotwords a number.
    let lettered = words
        .iter()
        .any(|w| w.chars().any(|c| c.is_ascii_alphabetic()));
    (ok && lettered).then(|| words.join(" "))
}

/// An installed application, from its `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// The file name without `.desktop`, which is what Wayland `app_id`s
    /// normally match (`org.mozilla.firefox`, `firefox`).
    pub id: String,
    /// The untranslated `Name=`.
    pub name: String,
}

/// Every visible application in the XDG data dirs, deduplicated by id with
/// the user's own entries winning (the freedesktop precedence).
pub fn desktop_apps() -> Vec<DesktopApp> {
    let mut seen = BTreeSet::new();
    let mut apps = Vec::new();
    for dir in application_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            if !seen.insert(id.to_owned()) {
                continue;
            }
            if let Some(name) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| visible_name(&text))
            {
                apps.push(DesktopApp {
                    id: id.to_owned(),
                    name,
                });
            }
        }
    }
    apps
}

/// `$XDG_DATA_HOME/applications`, then each `$XDG_DATA_DIRS` entry's.
fn application_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let home = env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| Path::new(&h).join(".local/share")));
    let dirs = env("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&dirs))
        .map(|d| d.join("applications"))
        .collect()
}

/// The `Name=` of a `[Desktop Entry]` that is a shown application, or
/// `None` for hidden entries, links and anything malformed.
fn visible_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    let (mut name, mut app, mut hidden) = (None, false, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            // Only the main group counts; actions come after it.
            if in_entry {
                break;
            }
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Name", v) => name = Some(v.to_owned()),
            ("Type", v) => app = v == "Application",
            ("NoDisplay" | "Hidden", "true") => hidden = true,
            _ => {}
        }
    }
    name.filter(|_| app && !hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_are_cleaned_or_dropped() {
        assert_eq!(
            clean("  Visual   Studio Code "),
            Some("Visual Studio Code".into())
        );
        assert_eq!(clean("PipeWire"), Some("PipeWire".into()));
        assert_eq!(clean("Can't-Stop 2"), Some("Can't-Stop 2".into()));
        assert_eq!(clean("GNU Image Manipulation Program X"), None, "too long");
        assert_eq!(clean("Hardinfo²"), None);
        assert_eq!(clean("計算機"), None);
        assert_eq!(clean("C++ IDE"), None);
        assert_eq!(clean("2048"), None);
        assert_eq!(clean("   "), None);
    }

    #[test]
    fn a_window_gets_the_base_set_plus_its_own() {
        let mut h = Hotwords::new();
        h.add(["Firefox", "Spotube", "Firefox"])
            .add_for_app("org.gnome.Nautilus", ["Downloads", "Trash"])
            .add_for_app("firefox", ["new tab"]);
        assert_eq!(h.for_app(None), "Firefox\nSpotube");
        assert_eq!(h.for_app(Some("unknown")), "Firefox\nSpotube");
        assert_eq!(
            h.for_app(Some("org.gnome.Nautilus")),
            "Downloads\nFirefox\nSpotube\nTrash"
        );
        assert_eq!(h.len(), 5);
    }

    #[test]
    fn an_empty_set_is_an_empty_string() {
        assert_eq!(Hotwords::new().for_app(Some("x")), "");
        assert!(Hotwords::new().is_empty());
    }

    #[test]
    fn desktop_entries_hidden_or_not_applications_are_skipped() {
        let app = "[Desktop Entry]\nType=Application\nName=Firefox\nName[de]=Feuerfuchs\n\
                   [Desktop Action new-window]\nName=New Window\n";
        assert_eq!(visible_name(app), Some("Firefox".into()));
        let hidden = "[Desktop Entry]\nType=Application\nName=Helper\nNoDisplay=true\n";
        assert_eq!(visible_name(hidden), None);
        let link = "[Desktop Entry]\nType=Link\nName=Docs\nURL=https://x\n";
        assert_eq!(visible_name(link), None);
        assert_eq!(visible_name("Name=Loose\nType=Application\n"), None);
    }
}
