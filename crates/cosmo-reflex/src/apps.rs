//! Installed applications, as the matcher resolves spoken names against
//! them: fuzzy, but scored, so a doubtful match escalates instead of
//! opening the wrong thing.
//!
//! The misrecognitions it has to absorb are the bench's (phase-3 findings
//! §6e): names split into words ("Thunder Bird", "D Beaver", "VS Codium",
//! "key pass XC") or a letter off ("Libra Wolf").

use crate::normalize::words;

/// An application a verb can name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRef {
    /// The `.desktop` file name without the suffix.
    pub id: String,
    /// Its `Name=`.
    pub name: String,
}

/// A lookup result: the app and how well the spoken name fits it (0–1).
#[derive(Debug, Clone, PartialEq)]
pub struct AppMatch {
    pub app: AppRef,
    pub score: f32,
}

struct Entry {
    app: AppRef,
    /// Spoken forms: the name's words, and the id's last dotted part
    /// (`org.mozilla.firefox` → `firefox`).
    keys: Vec<Vec<String>>,
    /// What the app is ("web browser"), from its generic name and
    /// keywords: a weaker claim than its name (see [`ALIAS_WEIGHT`]).
    aliases: Vec<Vec<String>>,
}

/// An alias match counts for less than a name match, so "open Firefox"
/// never loses to an app that merely lists "firefox" as a keyword.
const ALIAS_WEIGHT: f32 = 0.9;

#[derive(Default)]
pub struct AppIndex {
    entries: Vec<Entry>,
}

impl AppIndex {
    pub fn new(apps: impl IntoIterator<Item = AppRef>) -> Self {
        Self::with_aliases(apps.into_iter().map(|app| (app, Vec::new())))
    }

    /// As [`new`](Self::new), with each app's generic names and keywords.
    pub fn with_aliases(apps: impl IntoIterator<Item = (AppRef, Vec<String>)>) -> Self {
        let entries = apps
            .into_iter()
            .map(|(app, aliases)| {
                let mut keys = vec![words(&app.name)];
                let tail = app.id.rsplit('.').next().unwrap_or(&app.id);
                let tail = words(&tail.replace(['-', '_'], " "));
                if !keys.contains(&tail) {
                    keys.push(tail);
                }
                keys.retain(|k| !k.is_empty());
                let mut alias_keys: Vec<Vec<String>> = Vec::new();
                for alias in aliases {
                    let key = words(&alias);
                    if !key.is_empty() && !keys.contains(&key) && !alias_keys.contains(&key) {
                        alias_keys.push(key);
                    }
                }
                Entry {
                    app,
                    keys,
                    aliases: alias_keys,
                }
            })
            .collect();
        Self { entries }
    }

    /// The installed applications (see `cosmo_stt::hotwords::desktop_apps`).
    pub fn installed() -> Self {
        Self::with_aliases(cosmo_stt::hotwords::desktop_apps().into_iter().map(|a| {
            (
                AppRef {
                    id: a.id,
                    name: a.name,
                },
                a.aliases,
            )
        }))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The best app for a spoken `name`, if any fits at all. When two
    /// different apps fit almost equally well, the score is cut: guessing
    /// between them is not reflex's job.
    pub fn find(&self, name: &str) -> Option<AppMatch> {
        let query = words(name);
        if query.is_empty() {
            return None;
        }
        let mut scored: Vec<(f32, &Entry)> = self
            .entries
            .iter()
            .map(|e| {
                let by_name = e
                    .keys
                    .iter()
                    .map(|k| similarity(&query, k))
                    .fold(0.0, f32::max);
                let by_alias = e
                    .aliases
                    .iter()
                    .map(|k| similarity(&query, k) * ALIAS_WEIGHT)
                    .fold(0.0, f32::max);
                let s = by_name.max(by_alias);
                (s, e)
            })
            .filter(|(s, _)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (best, entry) = *scored.first()?;
        let rival = scored
            .iter()
            .skip(1)
            .find(|(_, e)| e.app.name != entry.app.name)
            .map_or(0.0, |(s, _)| *s);
        let score = if best - rival < 0.05 {
            best * 0.85
        } else {
            best
        };
        Some(AppMatch {
            app: entry.app.clone(),
            score,
        })
    }
}

/// How well spoken words fit an app's key, 0–1: the best of the rules
/// that apply.
fn similarity(query: &[String], key: &[String]) -> f32 {
    if query == key {
        return 1.0;
    }
    let (q, k) = (query.concat(), key.concat());
    if q == k {
        // Same letters, split differently: "thunder bird".
        return 0.95;
    }
    let (qn, kn) = (q.chars().count(), k.chars().count());
    let mut best: f32 = 0.0;
    // Every spoken word is in the name ("terminal" → "COSMIC Terminal"):
    // good, but less so the more of the name went unsaid.
    if query.iter().all(|w| key.contains(w)) {
        best = best.max(0.7 + 0.2 * query.len() as f32 / key.len() as f32);
    }
    // The name's start, however it was split ("D Beaver" → "DBeaver CE"):
    // scored by how much of it was said.
    if qn >= 4 && k.starts_with(&q) {
        best = best.max(0.7 + 0.2 * qn as f32 / kn as f32);
    }
    // A letter or two off ("libra wolf"): only for names long enough that
    // one letter isn't most of the word.
    let longest = qn.max(kn);
    if longest >= 6 {
        let ratio = 1.0 - edit_distance(&q, &k) as f32 / longest as f32;
        if ratio >= 0.8 {
            best = best.max(ratio);
        }
    }
    best
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            row[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(row[j] + 1);
        }
        prev = row;
    }
    prev[b.len()]
}

#[cfg(test)]
pub(crate) fn test_index() -> AppIndex {
    AppIndex::new(
        [
            ("firefox", "Firefox"),
            ("spotify", "Spotify"),
            ("org.mozilla.Thunderbird", "Thunderbird"),
            ("codium", "VSCodium"),
            ("org.keepassxc.KeePassXC", "KeePassXC"),
            ("io.gitlab.librewolf-community", "LibreWolf"),
            ("io.dbeaver.DBeaverCommunity", "DBeaver CE"),
            ("com.system76.CosmicTerm", "COSMIC Terminal"),
            ("com.system76.CosmicFiles", "COSMIC Files"),
            ("discord", "Discord"),
            ("org.blender.Blender", "Blender"),
            ("org.gnome.Evince", "Document Viewer"),
            ("us.zoom.Zoom", "Zoom"),
            ("com.anthropic.Claude", "Claude"),
        ]
        .map(|(id, name)| AppRef {
            id: id.into(),
            name: name.into(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(q: &str) -> Option<(String, f32)> {
        test_index().find(q).map(|m| (m.app.name, m.score))
    }

    #[test]
    fn exact_and_split_names_resolve() {
        assert_eq!(found("firefox"), Some(("Firefox".into(), 1.0)));
        assert_eq!(found("Thunder Bird").unwrap().0, "Thunderbird");
        assert_eq!(found("D Beaver").unwrap().0, "DBeaver CE");
        assert_eq!(found("VS Codium").unwrap().0, "VSCodium");
        assert_eq!(
            found("COSMIC terminal").unwrap(),
            ("COSMIC Terminal".into(), 1.0)
        );
    }

    #[test]
    fn near_misses_resolve_with_less_confidence() {
        let (name, score) = found("Libra Wolf").unwrap();
        assert_eq!(name, "LibreWolf");
        assert!((0.8..0.95).contains(&score), "{score}");
        let (name, score) = found("key pass x c").unwrap();
        assert_eq!(name, "KeePassXC");
        assert!(score >= 0.8, "{score}");
    }

    fn aliased() -> AppIndex {
        let app = |id: &str, name: &str| AppRef {
            id: id.into(),
            name: name.into(),
        };
        AppIndex::with_aliases([
            (
                app("firefox", "Firefox"),
                vec!["Web Browser".into(), "Internet".into()],
            ),
            (app("chromium", "Chromium"), vec!["Web Browser".into()]),
            (
                app("org.gnome.Evince", "Document Viewer"),
                vec!["PDF Reader".into()],
            ),
        ])
    }

    #[test]
    fn generic_names_resolve_when_unique_and_are_doubtful_when_shared() {
        let idx = aliased();
        let m = idx.find("pdf reader").unwrap();
        assert_eq!(
            (m.app.name.as_str(), m.score >= crate::THRESHOLD),
            ("Document Viewer", true)
        );
        // Two browsers: the reflex must not pick one.
        let m = idx.find("web browser").unwrap();
        assert!(m.score < crate::THRESHOLD, "{}", m.score);
        // A name still beats an alias.
        assert_eq!(idx.find("chromium").unwrap().app.name, "Chromium");
    }

    #[test]
    fn a_word_shared_by_two_apps_is_doubtful() {
        // "cosmic" is in two names: neither may win outright.
        let (_, score) = found("cosmic").unwrap();
        assert!(score < crate::THRESHOLD, "{score}");
    }

    #[test]
    fn unknown_names_and_short_near_misses_find_nothing() {
        assert_eq!(found("photoshop"), None);
        assert_eq!(
            found("zoo"),
            None,
            "one letter off a 4-letter name is another word"
        );
    }
}
