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

    /// Whether any phrases depend on the focused window: if not, looking
    /// the focus up can be skipped.
    pub fn has_app_sets(&self) -> bool {
        !self.per_app.is_empty()
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

/// Rare words cosmo is asked about that aren't app names: biased like the
/// rarer app names. The bench heard "PipeWire" as "pipe wire".
pub const DOMAIN_WORDS: &[&str] = &["PipeWire"];

/// Whether an app name is worth biasing toward (phase-4 spec §4.7).
///
/// Biasing helps with names the model spells badly on its own ("Spotube",
/// "KeePassXC"). It hurts with short ordinary-looking ones: on the user's
/// recordings, app-name hotwords turned "Mute" into "Zoom Zoom." and
/// "Launch LibreWolf" into "Claude LibreWolf." (phase-3 findings §6e).
/// So single-word names of six letters or fewer aren't biased; the reflex
/// matcher still knows them.
pub fn worth_biasing(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    !(words.len() == 1 && words[0].chars().count() <= 6)
}

/// The biasing set cosmo uses: [`DOMAIN_WORDS`] plus the installed app
/// names [`worth_biasing`] keeps.
pub fn curated(apps: &[DesktopApp]) -> Hotwords {
    let mut h = Hotwords::new();
    h.add(DOMAIN_WORDS.iter().copied());
    h.add(
        apps.iter()
            .map(|a| a.name.as_str())
            .filter(|n| worth_biasing(n)),
    );
    h
}

/// An installed application, from its `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// The file name without `.desktop`, which is what Wayland `app_id`s
    /// normally match (`org.mozilla.firefox`, `firefox`).
    pub id: String,
    /// The untranslated `Name=`.
    pub name: String,
    /// `Exec=`, unparsed (the launcher parses it; phase-4 spec §4.4).
    pub exec: Option<String>,
    /// `Path=`: the working directory to start it in.
    pub path: Option<String>,
    /// `Terminal=true`: needs a terminal to run in.
    pub terminal: bool,
    /// `StartupWMClass=`: what its windows call themselves when that isn't
    /// the file's id (`com.spotify.Client` → `spotify`).
    pub wm_class: Option<String>,
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
            if let Some(app) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_entry(id, &text))
            {
                apps.push(app);
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

/// One installed application by `.desktop` id, if visible (the user's own
/// entry winning, as in [`desktop_apps`]).
pub fn desktop_app(id: &str) -> Option<DesktopApp> {
    application_dirs().into_iter().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join(format!("{id}.desktop"))).ok()?;
        parse_entry(id, &text)
    })
}

/// The `Exec=` of an application's `[Desktop Action <action>]` group
/// (`new-window` on browsers), unparsed.
pub fn desktop_action_exec(id: &str, action: &str) -> Option<String> {
    let header = format!("[Desktop Action {action}]");
    application_dirs().into_iter().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join(format!("{id}.desktop"))).ok()?;
        action_exec(&text, &header)
    })
}

fn action_exec(text: &str, header: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if inside && let Some(exec) = line.strip_prefix("Exec=") {
            return Some(exec.to_owned());
        }
    }
    None
}

#[cfg(test)]
fn visible_name(text: &str) -> Option<String> {
    parse_entry("test", text).map(|a| a.name)
}

/// A `[Desktop Entry]` that is a shown application, or `None` for hidden
/// entries, links and anything malformed.
fn parse_entry(id: &str, text: &str) -> Option<DesktopApp> {
    let mut in_entry = false;
    let (mut name, mut app, mut hidden) = (None, false, false);
    let (mut exec, mut path, mut terminal, mut wm_class) = (None, None, false, None);
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
            ("Exec", v) => exec = Some(v.to_owned()),
            ("Path", v) if !v.is_empty() => path = Some(v.to_owned()),
            ("Terminal", v) => terminal = v == "true",
            ("StartupWMClass", v) if !v.is_empty() => wm_class = Some(v.to_owned()),
            _ => {}
        }
    }
    name.filter(|_| app && !hidden).map(|name| DesktopApp {
        id: id.to_owned(),
        name,
        exec,
        path,
        terminal,
        wm_class,
    })
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
    fn short_single_word_names_are_not_biased() {
        for hallucinated in ["Zoom", "Claude", "Tasks", "Help"] {
            assert!(!worth_biasing(hallucinated), "{hallucinated}");
        }
        for rare in [
            "Spotube",
            "KeePassXC",
            "Thunderbird",
            "LibreWolf",
            "COSMIC Files",
        ] {
            assert!(worth_biasing(rare), "{rare}");
        }
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

    #[test]
    fn a_desktop_action_s_exec_is_read_from_its_own_group() {
        let text = "[Desktop Entry]\nName=Firefox\nExec=firefox %u\n\
                    [Desktop Action new-window]\nName=New Window\nExec=firefox --new-window %u\n\
                    [Desktop Action private]\nExec=firefox --private-window %u\n";
        assert_eq!(
            action_exec(text, "[Desktop Action new-window]").as_deref(),
            Some("firefox --new-window %u")
        );
        assert_eq!(action_exec(text, "[Desktop Action missing]"), None);
    }
}
