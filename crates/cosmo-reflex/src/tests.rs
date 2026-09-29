//! The matcher against the user's own commands (phase-3 bench,
//! `scripts/bench-commands.txt`) and what the models actually heard.

use crate::apps::test_index;
use crate::{Intent, Matcher, MediaCommand, THRESHOLD};

fn matcher() -> Matcher {
    Matcher::new(test_index())
}

/// The intent, if matched at or above the threshold.
fn acted(text: &str) -> Option<Intent> {
    matcher()
        .match_text(text)
        .filter(|m| m.confidence >= THRESHOLD)
        .map(|m| m.intent)
}

fn app(text: &str) -> Option<(bool, String)> {
    match acted(text)? {
        Intent::Launch(a) => Some((true, a.name)),
        Intent::Focus(a) => Some((false, a.name)),
        other => panic!("`{text}` → {other:?}, expected an app verb"),
    }
}

#[test]
fn the_reflex_lines_of_the_users_command_list() {
    assert_eq!(app("Open Firefox"), Some((true, "Firefox".into())));
    assert_eq!(app("Launch Spotify"), Some((true, "Spotify".into())));
    assert_eq!(app("Open Thunderbird"), Some((true, "Thunderbird".into())));
    assert_eq!(app("Start VSCodium"), Some((true, "VSCodium".into())));
    assert_eq!(app("Open KeePassXC"), Some((true, "KeePassXC".into())));
    assert_eq!(app("Launch LibreWolf"), Some((true, "LibreWolf".into())));
    assert_eq!(app("Open DBeaver"), Some((true, "DBeaver CE".into())));
    assert_eq!(
        app("Open the COSMIC terminal"),
        Some((true, "COSMIC Terminal".into()))
    );
    assert_eq!(app("Focus Discord"), Some((false, "Discord".into())));

    assert_eq!(acted("Pause"), Some(Intent::Media(MediaCommand::Pause)));
    assert_eq!(acted("Next track"), Some(Intent::Media(MediaCommand::Next)));
    assert_eq!(
        acted("Previous song"),
        Some(Intent::Media(MediaCommand::Previous))
    );
    assert_eq!(acted("Maximize this window"), Some(Intent::Maximize));
    assert_eq!(
        acted("Switch to workspace two"),
        Some(Intent::SwitchWorkspace(2))
    );
    assert_eq!(
        acted("Move this window to workspace three"),
        Some(Intent::MoveToWorkspace(3))
    );
}

/// What the eight model pairings actually wrote for those lines (findings
/// §6e): formatting, split words, homophones.
#[test]
fn what_the_models_heard_still_matches() {
    assert_eq!(app("Open Thunder Bird"), Some((true, "Thunderbird".into())));
    assert_eq!(app("Start VS Codium"), Some((true, "VSCodium".into())));
    assert_eq!(app("start vs codium"), Some((true, "VSCodium".into())));
    assert_eq!(app("Open D Beaver"), Some((true, "DBeaver CE".into())));
    assert_eq!(app("Launch Libra Wolf"), Some((true, "LibreWolf".into())));
    assert_eq!(app("open key pass x c"), Some((true, "KeePassXC".into())));
    assert_eq!(
        acted("Switch to Work Space Two."),
        Some(Intent::SwitchWorkspace(2))
    );
    assert_eq!(
        acted("Move this window to Work Space Three."),
        Some(Intent::MoveToWorkspace(3))
    );
    assert_eq!(
        acted("move this window to work space three"),
        Some(Intent::MoveToWorkspace(3))
    );
}

/// Where reflex must *not* act: the line goes to reasoning instead.
#[test]
fn everything_else_escalates() {
    for text in [
        // The user's non-reflex lines: not safe verbs, or not verbs at all.
        "Close this window",
        "Mute",
        "Set the volume to thirty percent",
        "Turn it up",
        "What time is it",
        "Open Blender and move it to workspace four",
        "Restart PipeWire, then play something on Spotify",
        "Find the PDF I downloaded yesterday and open it in the document viewer",
        "Remind me in twenty minutes to check the build",
        "Take a screenshot of this window and save it to my desktop",
        // Confirmations are the gate's, never reflex's.
        "Yes",
        "Cancel",
        // Hallucinated hotwords (findings §6e): not a clean app verb.
        "Zoom Zoom.",
        "Claude LibreWolf.",
        // Open *what*? Nothing named.
        "Open",
        "Open the",
        // Unknown app.
        "Open Photoshop",
        // "stop" alone is too many things.
        "Stop",
    ] {
        assert_eq!(acted(text), None, "`{text}` must escalate");
    }
}

#[test]
fn filler_and_politeness_are_ignored() {
    assert_eq!(
        app("Could you open Firefox for me please"),
        Some((true, "Firefox".into()))
    );
    assert_eq!(
        app("Hey cosmo, launch Spotify."),
        Some((true, "Spotify".into()))
    );
    assert_eq!(
        acted("pause the music"),
        Some(Intent::Media(MediaCommand::Pause))
    );
    assert_eq!(
        acted("Skip this song"),
        Some(Intent::Media(MediaCommand::Next))
    );
}

#[test]
fn a_misheard_workspace_number_is_read_as_the_number() {
    // "Open Blender and move it to work space for" is not reflex (two
    // verbs), but the number reading itself is right.
    assert_eq!(
        acted("go to workspace for"),
        Some(Intent::SwitchWorkspace(4))
    );
    assert_eq!(
        acted("move it to workspace to"),
        Some(Intent::MoveToWorkspace(2))
    );
    assert_eq!(acted("switch to workspace"), None, "no number, no move");
}
