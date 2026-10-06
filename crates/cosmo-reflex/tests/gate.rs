//! The reflex exception (blueprint §7), as a test: every verb reflex can
//! express is one the gate allows, so nothing on the deny or hold lists is
//! reachable without the model.

use cosmo_gate::{Annotations, Gate, LockState, Verdict, is_lock_sensitive, is_string_bearing};
use cosmo_reflex::{AppRef, Ask, Intent, MediaCommand, VolumeCommand};

/// One of every intent. The `match` is exhaustive on purpose: adding a
/// variant to `Intent` fails to compile here until it's listed, so no new
/// reflex verb escapes this test.
fn every_intent() -> Vec<Intent> {
    let app = AppRef {
        id: "firefox".into(),
        name: "Firefox".into(),
    };
    let all = vec![
        Intent::Media(MediaCommand::Play),
        Intent::Media(MediaCommand::Pause),
        Intent::Media(MediaCommand::Stop),
        Intent::Media(MediaCommand::Next),
        Intent::Media(MediaCommand::Previous),
        Intent::Ask(Ask::Time),
        Intent::Ask(Ask::Date),
        Intent::Volume(VolumeCommand::Up(10)),
        Intent::Volume(VolumeCommand::Down(5)),
        Intent::Volume(VolumeCommand::Set(40)),
        Intent::Volume(VolumeCommand::Mute),
        Intent::Volume(VolumeCommand::Unmute),
        Intent::Launch(app.clone()),
        Intent::Focus(app),
        Intent::SwitchWorkspace(2),
        Intent::MoveToWorkspace(3),
        Intent::Maximize,
        Intent::Minimize,
    ];
    for i in &all {
        match i {
            Intent::Media(_)
            | Intent::Volume(_)
            | Intent::Ask(_)
            | Intent::Launch(_)
            | Intent::Focus(_)
            | Intent::SwitchWorkspace(_)
            | Intent::MoveToWorkspace(_)
            | Intent::Maximize
            | Intent::Minimize => {}
        }
    }
    all
}

fn annotations() -> Annotations {
    Annotations {
        read_only: Intent::READ_ONLY,
        destructive: Intent::DESTRUCTIVE,
    }
}

#[test]
fn every_reflex_verb_is_allowed_by_the_gate() {
    let gate = Gate::new();
    for intent in every_intent() {
        let (tool, args) = intent.tool_call();
        assert_eq!(
            gate.verdict_for_call(tool, &args, &annotations()),
            Verdict::Allow,
            "reflex verb {intent:?} ({tool}) is not Allow"
        );
    }
}

/// None carries a free-form string the gate would have to vet (a command,
/// typed text): those belong to the reasoning path.
#[test]
fn no_reflex_verb_carries_a_string_for_the_gate_to_vet() {
    for intent in every_intent() {
        let (tool, _) = intent.tool_call();
        assert!(!is_string_bearing(tool), "{tool} is string-bearing");
    }
}

/// Reflex verbs arrange windows and start apps; none reads the screen or
/// injects input, the surface invariant #10 protects. So they're not
/// lock-sensitive, and they keep working on COSMIC, where the lock state is
/// always `Unknown` (phase-1 findings §L).
#[test]
fn reflex_verbs_are_not_lock_sensitive_and_work_with_lock_state_unknown() {
    let gate = Gate::new();
    gate.set_lock_state(LockState::Unknown);
    for intent in every_intent() {
        let (tool, args) = intent.tool_call();
        assert!(!is_lock_sensitive(tool), "{tool} is lock-sensitive");
        assert_eq!(
            gate.verdict_for_call(tool, &args, &annotations()),
            Verdict::Allow
        );
    }
}

/// The other direction: what the gate holds or denies has no reflex
/// intent. Every tool name reflex produces is from a fixed set, and none
/// of the gate's held or denied tools is in it.
#[test]
fn held_and_denied_tools_are_not_reflex_tools() {
    let reflex: Vec<&str> = every_intent().iter().map(|i| i.tool_call().0).collect();
    for gated in [
        "run_in_terminal",
        "cosmo_type_into_terminal",
        "clipboard_get",
        "clipboard_set",
        "type_text",
        "press_key",
        "click",
        "screenshot",
    ] {
        assert!(!reflex.contains(&gated), "{gated} is reachable from reflex");
    }
}
