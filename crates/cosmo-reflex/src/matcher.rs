//! The matcher (phase-4 spec §4.3): transcript → [`Intent`] with a
//! confidence, over a small grammar of safe verbs.
//!
//! Not a list of exact strings. The transcript is normalized (case,
//! punctuation, number words, "work space"), polite filler is dropped, and
//! what's left must be *entirely* one of the verb patterns. A word nobody
//! accounted for means no match, and the transcript escalates to reasoning.
//! That is the safe direction to be wrong in.

use crate::apps::{AppIndex, AppMatch};
use crate::intent::{Intent, MediaCommand};
use crate::normalize::words;

/// Below this, reflex doesn't act: the transcript goes to reasoning.
pub const THRESHOLD: f32 = 0.8;

/// A match and how sure the matcher is of it (0–1).
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub intent: Intent,
    pub confidence: f32,
}

/// Words that carry no command: dropped wherever they appear.
const FILLER: &[&str] = &[
    "please", "cosmo", "hey", "ok", "okay", "now", "just", "for", "me", "can", "could", "would",
    "you", "will",
];

pub struct Matcher {
    apps: AppIndex,
}

impl Matcher {
    pub fn new(apps: AppIndex) -> Self {
        Self { apps }
    }

    pub fn apps(&self) -> &AppIndex {
        &self.apps
    }

    /// The best reading of `transcript`, or `None` if it isn't a reflex
    /// command at all. Callers act only at or above [`THRESHOLD`].
    pub fn match_text(&self, transcript: &str) -> Option<Match> {
        let raw = words(transcript);
        // "for" is filler ("open Firefox for me") except where it's a
        // misheard four ("workspace for"), which `workspace_number` reads
        // before filler is dropped.
        let workspace = workspace_number(&raw);
        let w: Vec<&str> = raw
            .iter()
            .enumerate()
            .filter(|&(i, w)| {
                let after_workspace =
                    i > 0 && matches!(raw[i - 1].as_str(), "workspace" | "desktop");
                after_workspace || !FILLER.contains(&w.as_str())
            })
            .map(|(_, w)| w.as_str())
            .collect();
        let w = strip_articles(&w);

        media(&w)
            .map(|cmd| Match {
                intent: Intent::Media(cmd),
                confidence: 1.0,
            })
            .or_else(|| workspaces(&w, workspace))
            .or_else(|| window(&w))
            .or_else(|| self.app_verb(&w))
    }

    /// launch/open/start/run *app*, focus/switch to/go to/show *app*.
    fn app_verb(&self, w: &[&str]) -> Option<Match> {
        let (launch, rest) = match w {
            ["open" | "launch" | "start" | "run", rest @ ..] => (true, rest),
            ["focus" | "show", rest @ ..] => (false, rest),
            ["switch" | "go", "to", rest @ ..] => (false, rest),
            _ => return None,
        };
        let rest = strip_articles(rest);
        if rest.is_empty() {
            return None;
        }
        let AppMatch { app, score } = self.apps.find(&rest.join(" "))?;
        let intent = if launch {
            Intent::Launch(app)
        } else {
            Intent::Focus(app)
        };
        Some(Match {
            intent,
            confidence: score,
        })
    }
}

fn strip_articles<'a>(w: &[&'a str]) -> Vec<&'a str> {
    w.iter()
        .copied()
        .filter(|w| !matches!(*w, "the" | "a" | "an" | "my"))
        .collect()
}

fn media(w: &[&str]) -> Option<MediaCommand> {
    const THING: &[&str] = &["music", "song", "track", "playback", "video", "it", "this"];
    let thing = |rest: &[&str]| rest.is_empty() || (rest.len() == 1 && THING.contains(&rest[0]));
    Some(match w {
        ["pause", rest @ ..] if thing(rest) => MediaCommand::Pause,
        ["stop", rest @ ..] if rest.len() == 1 && THING.contains(&rest[0]) => MediaCommand::Stop,
        ["play" | "resume" | "unpause", rest @ ..] if thing(rest) => MediaCommand::Play,
        ["next" | "skip", rest @ ..] if thing(rest) => MediaCommand::Next,
        ["skip", "this", rest @ ..] if thing(rest) => MediaCommand::Next,
        ["previous" | "last", rest @ ..] if !rest.is_empty() && thing(rest) => {
            MediaCommand::Previous
        }
        ["previous"] => MediaCommand::Previous,
        ["go" | "skip", "back", rest @ ..] if rest.is_empty() || thing(rest) => {
            MediaCommand::Previous
        }
        _ => return None,
    })
}

/// The number after "workspace" (or "desktop"), reading the misheard
/// homophones the bench caught ("work space for" is four).
fn workspace_number(raw: &[String]) -> Option<u32> {
    let at = raw
        .iter()
        .position(|w| w == "workspace" || w == "desktop")?;
    let n = raw.get(at + 1)?;
    match n.as_str() {
        "won" => Some(1),
        "to" | "too" => Some(2),
        "for" | "fore" => Some(4),
        "ate" => Some(8),
        n => n.parse().ok().filter(|n| (1..=99).contains(n)),
    }
}

/// switch/go to workspace N; move this window (or it) to workspace N.
fn workspaces(w: &[&str], n: Option<u32>) -> Option<Match> {
    let n = n?;
    let ws = |w: &str| w == "workspace" || w == "desktop";
    let intent = match w {
        ["switch" | "go", "to", k, _] | [k, _] if ws(k) => Intent::SwitchWorkspace(n),
        ["move" | "send" | "put", rest @ ..] => {
            // move [this window | this | it | window] to workspace N
            let target = rest.iter().position(|w| *w == "to")?;
            let what = &rest[..target];
            let tail = &rest[target + 1..];
            let window = matches!(
                what,
                [] | ["this"] | ["it"] | ["window"] | ["this", "window"]
            );
            if !window || tail.len() != 2 || !ws(tail[0]) {
                return None;
            }
            Intent::MoveToWorkspace(n)
        }
        _ => return None,
    };
    Some(Match {
        intent,
        confidence: 1.0,
    })
}

/// maximize / minimize [this window | this | it | the window].
fn window(w: &[&str]) -> Option<Match> {
    let this = |rest: &[&str]| {
        matches!(
            rest,
            [] | ["this"] | ["it"] | ["window"] | ["this", "window"]
        )
    };
    let intent = match w {
        ["maximize" | "maximise", rest @ ..] if this(rest) => Intent::Maximize,
        ["minimize" | "minimise", rest @ ..] if this(rest) => Intent::Minimize,
        _ => return None,
    };
    Some(Match {
        intent,
        confidence: 1.0,
    })
}
