//! One test per blueprint §7 gate invariant. These are the contract; any
//! change to the gate that breaks one of these is wrong, not the test.

use cosmo_gate::{ConfirmResult, Gate, UtteranceSource, Verdict, is_confirm_utterance};
use serde_json::json;

/// **Invariant #1:** a gated tool call and a confirmation in the **same model
/// response** is rejected outright.
#[test]
fn same_response_confirmation_rejected() {
    let gate = Gate::new();
    // A held call whose own response text approves it → escalated to Deny.
    assert_eq!(
        gate.same_response_verdict("Confirmed! Shutting down now.", Verdict::Hold),
        Verdict::Deny
    );
    assert_eq!(
        gate.same_response_verdict("Going ahead with the shutdown.", Verdict::Hold),
        Verdict::Deny
    );
    // The natural shape — action requested, no self-approval — stays Hold.
    assert_eq!(
        gate.same_response_verdict("Shutting the machine down.", Verdict::Hold),
        Verdict::Hold
    );
    // Allow is unaffected (the detector only escalates gated calls).
    assert_eq!(
        gate.same_response_verdict("Confirmed, listing windows.", Verdict::Allow),
        Verdict::Allow
    );
    // And a Deny is still a Deny.
    assert_eq!(
        gate.same_response_verdict("Confirmed, running it.", Verdict::Deny),
        Verdict::Deny
    );
}

/// **Invariant #2:** confirmation only takes effect after a genuinely **new
/// user turn**. A hold parked in turn N cannot be released by anything that
/// happens in turn N — including a confirm in the very `say` that triggered
/// the hold, and a model claiming approval.
#[test]
fn confirm_requires_new_turn() {
    let gate = Gate::new();

    // Turn 1: the user asks for the gated action.
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".into(),
    );

    // A confirm *within* turn 1 (the daemon checks the utterance before it
    // reaches the model, but the turn counter hasn't advanced) resolves
    // nothing…
    let turn1 = ConfirmResult::NotAConfirm; // "reboot the machine" isn't a confirm at all
    assert!(matches!(turn1, ConfirmResult::NotAConfirm));
    // …and neither does a token confirm presented in the same turn.
    assert!(matches!(gate.confirm_token(&token), ConfirmResult::Unknown));
    assert_eq!(
        gate.pending().len(),
        1,
        "the hold must survive a same-turn confirm"
    );

    // The model text approving it also cannot release it — there is no API
    // by which model output confirms anything (see invariant #4's structure).

    // Turn 2: a genuinely new user turn confirms.
    gate.begin_turn();
    match gate.confirm_token(&token) {
        ConfirmResult::Executed(p) => {
            assert_eq!(p.tool, "run_in_terminal");
            assert_eq!(p.args["command"], "reboot");
        }
        other => panic!("expected Executed, got {other:?}"),
    }
    assert!(gate.pending().is_empty());
}

/// **Invariant #3:** the confirm phrase is matched as a **whole utterance** —
/// "don't confirm that" does not confirm.
#[test]
fn whole_utterance_matching() {
    for text in [
        "confirm",
        "confirm that",
        "Confirm that.",
        "yes",
        "Yes, please!",
        "do it",
        "  okay  ",
        "make it so",
    ] {
        assert!(is_confirm_utterance(text), "`{text}` should confirm");
    }
    for text in [
        "don't confirm that",
        "dont confirm that",
        "confirm that and also delete everything",
        "confirm and run rm -rf /",
        "wait, don't do it",
        "not yet",
        "confirm this is right", // partial overlap, not whole-utterance
        "yes but read the file first",
        "",
    ] {
        assert!(!is_confirm_utterance(text), "`{text}` must NOT confirm");
    }
}

/// **Invariant #4:** the **local** confirm path never asks the model.
///
/// This is structural: `Gate` has no model handle, no HTTP client, no async
/// runtime. The confirm paths return the fully-formed parked call for the
/// daemon to execute directly. This test pins the observable half of that:
/// confirming resolves and hands back the parked call synchronously, and a
/// whole-utterance confirm does the same.
#[test]
fn local_confirm_is_model_free() {
    let gate = Gate::new();
    gate.begin_turn();
    gate.park(
        "run_in_terminal",
        json!({"command": "systemctl suspend"}),
        "suspend".into(),
    );
    gate.begin_turn();
    match gate.confirm_utterance("confirm that", UtteranceSource::Typed) {
        ConfirmResult::Executed(p) => assert_eq!(p.args["command"], "systemctl suspend"),
        other => panic!("expected Executed, got {other:?}"),
    }

    // Unknown token: reported, never guessed.
    assert!(matches!(
        gate.confirm_token("deadbeef"),
        ConfirmResult::Unknown
    ));
    // Non-confirm utterance is passed through (the daemon routes it to the
    // model — the gate itself never runs one).
    assert!(matches!(
        gate.confirm_utterance("what time is it", UtteranceSource::Typed),
        ConfirmResult::NotAConfirm
    ));
    // Reject discards without executing.
    gate.begin_turn();
    let tok = gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".into(),
    );
    gate.begin_turn();
    assert!(gate.reject(&tok));
    assert!(matches!(gate.confirm_token(&tok), ConfirmResult::Unknown));
}

/// **Invariant #5 (phase 4, the user's decision of 2026-09-29):** a spoken
/// confirmation resolves a hold **only if it was said during a physical hold
/// of the trigger key**. Anything that reaches the microphone can say
/// "confirm": a video, a call, someone in the room. An open-mic utterance
/// (`cosmo listen`, later the wake word) never confirms, whatever it says
/// and however many turns pass.
#[test]
fn open_mic_speech_never_confirms() {
    let phrases = [
        "confirm",
        "Confirm.",
        "confirm that",
        "yes",
        "Yes!",
        "yeah",
        "ok",
        "do it",
        "go ahead",
        "proceed",
        "make it so",
        "affirmative",
    ];
    let gate = Gate::new();
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "systemctl reboot"}),
        "reboot".into(),
    );

    for phrase in phrases {
        gate.begin_turn(); // a genuinely new turn, so only the source decides
        match gate.confirm_utterance(phrase, UtteranceSource::OpenMic) {
            ConfirmResult::NeedsKey => {}
            other => panic!("open-mic `{phrase}` must not confirm, got {other:?}"),
        }
        assert_eq!(gate.pending().len(), 1, "`{phrase}` released the hold");
    }

    // Not confirm-shaped: open-mic text flows on like any other.
    assert!(matches!(
        gate.confirm_utterance("what time is it", UtteranceSource::OpenMic),
        ConfirmResult::NotAConfirm
    ));

    // The same words said while the key is held do confirm.
    gate.begin_turn();
    match gate.confirm_utterance("confirm", UtteranceSource::KeyHeld) {
        ConfirmResult::Executed(p) => assert_eq!(p.token, token),
        other => panic!("key-held confirm must execute, got {other:?}"),
    }
    assert!(gate.pending().is_empty());
}

/// A key-held confirm is still bound by invariant #2: it cannot release a
/// hold parked in its own turn.
#[test]
fn key_held_confirm_still_needs_a_new_turn() {
    let gate = Gate::new();
    gate.begin_turn();
    gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".into(),
    );
    assert!(matches!(
        gate.confirm_utterance("yes", UtteranceSource::KeyHeld),
        ConfirmResult::NonePending
    ));
    assert_eq!(gate.pending().len(), 1);
}
