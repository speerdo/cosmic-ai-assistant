//! The matcher (phase-4 spec §4.3): transcript → [`Intent`] with a
//! confidence, over a small grammar of safe verbs.
//!
//! Not a list of exact strings. The transcript is normalized (case,
//! punctuation, number words, "work space"), polite filler is dropped, and
//! what's left must be *entirely* one of the verb patterns. A word nobody
//! accounted for means no match, and the transcript escalates to reasoning.
//! That is the safe direction to be wrong in.

use crate::apps::{AppIndex, AppMatch};
use crate::intent::{Ask, Intent, MediaCommand, VolumeCommand};
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
            .or_else(|| {
                volume(&w).map(|cmd| Match {
                    intent: Intent::Volume(cmd),
                    confidence: 1.0,
                })
            })
            .or_else(|| {
                ask(&w).map(|a| Match {
                    intent: Intent::Ask(a),
                    confidence: 1.0,
                })
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
        let mut rest = strip_articles(rest);
        // "open up a new terminal": launching is already a new window, so
        // the particle and "new" say nothing more. Only for launching:
        // "focus the new one" isn't a reflex command.
        if launch {
            if rest.first() == Some(&"up") {
                rest.remove(0);
            }
            rest = strip_articles(&rest);
            if rest.first() == Some(&"new") {
                rest.remove(0);
            }
        }
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
    // Players by name: "play Spotify" resumes it. Only after play/resume/
    // pause, never after "start" ("start Spotify" launches it).
    const PLAYER: &[&str] = &["spotify", "rhythmbox", "vlc", "youtube", "podcast"];
    // "play some music again", "turn the music back on": the extra words
    // don't change the command.
    let w: Vec<&str> = w
        .iter()
        .copied()
        .filter(|w| !matches!(*w, "some" | "again" | "back"))
        .collect();
    let w = w.as_slice();
    let thing = |rest: &[&str]| rest.is_empty() || (rest.len() == 1 && THING.contains(&rest[0]));
    let named = |rest: &[&str]| thing(rest) || (rest.len() == 1 && PLAYER.contains(&rest[0]));
    Some(match w {
        ["pause", rest @ ..] if named(rest) => MediaCommand::Pause,
        ["stop", rest @ ..] if rest.len() == 1 && THING.contains(&rest[0]) => MediaCommand::Stop,
        ["play" | "resume" | "unpause", rest @ ..] if named(rest) => MediaCommand::Play,
        ["play" | "resume", "playing" | "playback"] => MediaCommand::Play,
        ["continue" | "keep" | "start", "playing"] => MediaCommand::Play,
        ["continue" | "start", rest @ ..] if !rest.is_empty() && thing(rest) => MediaCommand::Play,
        // "turn the music on", "turn on the music", "turn it back on".
        ["turn", "on", t] | ["turn", t, "on"] if THING.contains(t) => MediaCommand::Play,
        ["turn", "off", t] | ["turn", t, "off"] if THING.contains(t) => MediaCommand::Pause,
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

/// "What time is it", "what's the date", "what day is it": questions the
/// clock answers. Nothing else (a question about *another* time or place,
/// "what time is it in Tokyo", is the model's).
fn ask(w: &[&str]) -> Option<Ask> {
    // "what's" is one word; "what is" is two.
    let w: Vec<&str> = match w {
        ["what's" | "whats", rest @ ..] => std::iter::once("what")
            .chain(std::iter::once("is"))
            .chain(rest.iter().copied())
            .collect(),
        w => w.to_vec(),
    };
    Some(match w.as_slice() {
        ["what", "time", "is", "it"]
        | ["what", "is", "time"]
        | ["what", "is", "current", "time"] => Ask::Time,
        ["tell", "time"] | ["tell", "me", "time"] | ["current", "time"] | ["time"] => Ask::Time,
        ["what", "is", "date"]
        | ["what", "is", "today's", "date"]
        | ["what", "is", "date", "today"]
        | ["what", "day", "is", "it"]
        | ["what", "day", "is", "it", "today"]
        | ["what", "is", "today"]
        | ["what", "is", "the", "day"]
        | ["today's", "date"]
        | ["what", "date", "is", "it"]
        | ["what", "is", "the", "date"] => Ask::Date,
        _ => return None,
    })
}

/// The volume verbs: "volume up", "turn it down", "louder", "turn the
/// volume up by 5 percent", "set the volume to 40", "mute", "unmute".
/// Anything left over (a word nobody accounted for) is no match.
fn volume(w: &[&str]) -> Option<VolumeCommand> {
    // "turn the sound back on": the extra word doesn't change the command.
    let w: Vec<&str> = w.iter().copied().filter(|w| *w != "back").collect();
    let w = w.as_slice();
    // The amount: nothing (a step), "a bit" (a small one), or a number of
    // percent, with or without "by" and "percent".
    let amount = |rest: &[&str]| -> Option<Option<u8>> {
        let rest = match rest {
            ["by", r @ ..] => r,
            r => r,
        };
        match rest {
            [] => Some(None),
            ["bit" | "little" | "notch"] => Some(Some(5)),
            [n] | [n, "percent"] => n.parse().ok().filter(|n| *n <= 100).map(Some),
            _ => None,
        }
    };
    let up = |w: &str| matches!(w, "up" | "louder" | "higher");
    let down = |w: &str| matches!(w, "down" | "quieter" | "softer" | "lower");
    let step = |dir_up: bool, amount: Option<u8>| {
        let n = amount.unwrap_or(10);
        if dir_up {
            VolumeCommand::Up(n)
        } else {
            VolumeCommand::Down(n)
        }
    };
    let sound = |w: &str| matches!(w, "volume" | "sound" | "audio");
    Some(match w {
        ["mute"] | ["mute", "it" | "this" | "everything"] => VolumeCommand::Mute,
        ["mute", s] if sound(s) => VolumeCommand::Mute,
        ["unmute"] | ["unmute", "it" | "this" | "everything"] => VolumeCommand::Unmute,
        ["unmute", s] if sound(s) => VolumeCommand::Unmute,
        ["turn", s, "off"] | ["turn", "off", s] if sound(s) => VolumeCommand::Mute,
        ["turn", s, "on"] | ["turn", "on", s] if sound(s) => VolumeCommand::Unmute,
        // "louder", "make it quieter".
        [d] if matches!(*d, "louder" | "quieter" | "softer") => step(up(d), None),
        ["make", "it", d] if matches!(*d, "louder" | "quieter" | "softer") => step(up(d), None),
        // "volume up (by 5 percent)".
        ["volume", d, rest @ ..] if up(d) || down(d) => step(up(d), amount(rest)?),
        // "turn (the volume | it) up (a bit)", "turn up the volume".
        ["turn" | "bring" | "put", s, d, rest @ ..]
            if (sound(s) || *s == "it") && (up(d) || down(d)) =>
        {
            step(up(d), amount(rest)?)
        }
        ["turn", d, s, rest @ ..] if sound(s) && (up(d) || down(d)) => step(up(d), amount(rest)?),
        // "raise / increase / lower / decrease / reduce the volume".
        ["raise" | "increase", s, rest @ ..] if sound(s) => step(true, amount(rest)?),
        ["lower" | "decrease" | "reduce", s, rest @ ..] if sound(s) => step(false, amount(rest)?),
        // "set the volume to 40 (percent)".
        ["set" | "turn" | "put" | "change", s, "to", n]
        | ["set" | "turn" | "put" | "change", s, "to", n, "percent"]
            if sound(s) =>
        {
            VolumeCommand::Set(n.parse().ok().filter(|n| *n <= 100)?)
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
