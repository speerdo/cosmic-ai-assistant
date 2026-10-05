//! The user's profile: facts cosmo's tools use without being told each time
//! ("how's the weather?" needs a place). `~/.config/cosmo/profile.json`,
//! written by `cosmo setup` or by voice ("my location is…"), readable only
//! by the user (0600): a home location is personal.
//!
//! What reaches the reasoning model is [`Profile::prompt_line`]: the name
//! and the *place name*, never the coordinates. The coordinates go only to
//! the weather service, rounded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// What cosmo calls the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Home, for weather (and anything else "here" means).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<Place>,
    pub units: Units,
    /// News feeds (RSS or Atom) the `news` tool reads, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub news: Vec<Feed>,
}

/// One news feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub name: String,
    pub url: String,
}

/// A place, geocoded once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// As people say it: "Pittsburgh, Pennsylvania, United States".
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Units {
    /// °C, km/h, mm.
    #[default]
    Metric,
    /// °F, mph, inches.
    Imperial,
}

impl Units {
    /// The usual units where the locale is: the US, Liberia and Myanmar
    /// measure in imperial; everywhere else metric.
    pub fn from_locale() -> Self {
        let lang = ["LC_MEASUREMENT", "LC_ALL", "LANG"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();
        let region = lang
            .split(['.', '@'])
            .next()
            .and_then(|l| l.split('_').nth(1))
            .unwrap_or_default();
        if matches!(region, "US" | "LR" | "MM") {
            Self::Imperial
        } else {
            Self::Metric
        }
    }
}

impl Profile {
    /// One line for the reasoning prompt, or "" when there's nothing.
    /// Place *names* only: coordinates never go to the model.
    pub fn prompt_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = &self.name {
            parts.push(format!("their name is {name}"));
        }
        if let Some(home) = &self.home {
            parts.push(format!(
                "they live in {} (the weather tool uses this when no place is named)",
                home.name
            ));
        }
        if !self.news.is_empty() {
            let names: Vec<&str> = self.news.iter().map(|f| f.name.as_str()).collect();
            parts.push(format!("their news sources are {}", names.join(", ")));
        }
        if self.home.is_some() || self.name.is_some() {
            parts.push(format!(
                "they use {} units",
                match self.units {
                    Units::Metric => "metric",
                    Units::Imperial => "imperial",
                }
            ));
        }
        if parts.is_empty() {
            return String::new();
        }
        format!("About the user: {}.", parts.join("; "))
    }
}

/// `~/.config/cosmo/profile.json`.
pub fn profile_path() -> PathBuf {
    crate::config_path().with_file_name("profile.json")
}

/// The profile, or an empty one when there's no file yet.
pub fn load() -> Result<Profile, String> {
    load_from(&profile_path())
}

pub fn load_from(path: &Path) -> Result<Profile, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Profile::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write it, owner-only, by tmp-then-rename.
pub fn save(profile: &Profile) -> Result<PathBuf, String> {
    save_to(&profile_path(), profile)
}

pub fn save_to(path: &Path, profile: &Profile) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())? + "\n";
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_owner_only_and_missing_is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cosmo-profile-{}", std::process::id()));
        let path = dir.join("profile.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_from(&path).unwrap(), Profile::default());
        let p = Profile {
            name: Some("Adam".into()),
            home: Some(Place {
                name: "Pittsburgh, Pennsylvania, United States".into(),
                latitude: 40.4406,
                longitude: -79.9959,
            }),
            units: Units::Imperial,
            news: vec![Feed {
                name: "BBC News".into(),
                url: "https://feeds.bbci.co.uk/news/rss.xml".into(),
            }],
        };
        save_to(&path, &p).unwrap();
        assert_eq!(load_from(&path).unwrap(), p);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn the_prompt_gets_the_place_name_never_the_coordinates() {
        let p = Profile {
            name: None,
            home: Some(Place {
                name: "Leeds, England".into(),
                latitude: 53.7997,
                longitude: -1.5492,
            }),
            units: Units::Metric,
            news: Vec::new(),
        };
        let line = p.prompt_line();
        assert!(
            line.contains("Leeds, England") && line.contains("metric"),
            "{line}"
        );
        assert!(!line.contains("53.") && !line.contains("1.549"), "{line}");
        assert_eq!(Profile::default().prompt_line(), "");
    }
}
