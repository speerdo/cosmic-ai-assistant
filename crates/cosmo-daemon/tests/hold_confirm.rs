//! The hold → confirm path, driven through `Engine::handle` exactly as the
//! control socket drives it (DoD §1.5: `cosmo say "shut the machine down"` →
//! Hold → `cosmo confirm` completes it locally).
//!
//! cosmo-gate's invariant suite covers the same ground at unit level and was
//! green while this path was broken end to end, because it advanced the turn
//! counter by hand where the daemon does not. The lesson is the test, not the
//! fix: gate semantics have to be exercised through the caller that actually
//! uses them.

use cosmo_daemon::engine::Engine;
use cosmo_ipc::{Command, ConfirmOutcome, Event, Response};
use serde_json::json;

async fn engine() -> Engine {
    let (events, _rx) = tokio::sync::broadcast::channel::<Event>(64);
    // No agent is connected here; the gate and the hold queue are under test.
    Engine::new(cosmo_config::Config::default(), events).await
}

/// A hold parked in one turn is released by a `cosmo confirm <token>` — the
/// CLI confirmation *is* the "genuinely new user turn" invariant #2 requires.
/// Before the fix this returned `Unknown` forever: the daemon's confirm path
/// never advanced the turn counter, so every parked call stayed parked and
/// the DoD's shutdown example could not be completed.
#[tokio::test]
async fn cli_confirm_resolves_a_hold_parked_this_turn() {
    let engine = engine().await;
    let gate = engine.gate();

    // Turn 1: the user says something that parks a gated call.
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "systemctl poweroff"}),
        "shut the machine down".to_owned(),
    );
    assert_eq!(gate.pending().len(), 1);

    // `cosmo confirm <token>` over the socket.
    let response = engine
        .handle(Command::Confirm {
            token: token.clone(),
        })
        .await;
    match response {
        Response::Confirm {
            outcome: ConfirmOutcome::Executed { token: t, .. },
        } => assert_eq!(t, token),
        other => panic!("confirm must execute the parked call, got {other:?}"),
    }
    assert!(gate.pending().is_empty(), "the hold must be consumed");
}

/// Confirming twice does not run the action twice, and an unknown token is
/// not treated as a confirmation of whatever happens to be pending.
#[tokio::test]
async fn confirm_is_single_use_and_token_specific() {
    let engine = engine().await;
    let gate = engine.gate();
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".to_owned(),
    );

    let first = engine
        .handle(Command::Confirm {
            token: token.clone(),
        })
        .await;
    assert!(matches!(
        first,
        Response::Confirm {
            outcome: ConfirmOutcome::Executed { .. }
        }
    ));
    let second = engine.handle(Command::Confirm { token }).await;
    assert!(
        matches!(
            second,
            Response::Confirm {
                outcome: ConfirmOutcome::Unknown
            }
        ),
        "a consumed token must not resolve again"
    );

    gate.begin_turn();
    let _parked = gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".to_owned(),
    );
    let wrong = engine
        .handle(Command::Confirm {
            token: "deadbeef".to_owned(),
        })
        .await;
    assert!(matches!(
        wrong,
        Response::Confirm {
            outcome: ConfirmOutcome::Unknown
        }
    ));
    assert_eq!(gate.pending().len(), 1, "the real hold must survive");
}

/// `cosmo cancel <token>` discards a hold without executing it.
#[tokio::test]
async fn cancel_discards_without_executing() {
    let engine = engine().await;
    let gate = engine.gate();
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "reboot"}),
        "reboot".to_owned(),
    );

    let response = engine
        .handle(Command::Cancel {
            token: token.clone(),
        })
        .await;
    assert!(matches!(response, Response::Cancelled { ok: true, .. }));
    assert!(gate.pending().is_empty());

    let after = engine.handle(Command::Confirm { token }).await;
    assert!(matches!(
        after,
        Response::Confirm {
            outcome: ConfirmOutcome::Unknown
        }
    ));
}

/// Cancelling the last pending hold returns the daemon to Idle. It stayed
/// in Waiting before, so the next turn started from a state that was no
/// longer true (found live in spec §2.7's run, findings §7).
#[tokio::test]
async fn cancelling_the_last_hold_leaves_waiting() {
    let engine = engine().await;
    let gate = engine.gate();
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "apt install cowsay"}),
        "install cowsay".to_owned(),
    );
    engine.set_state(cosmo_ipc::State::Waiting);

    let response = engine.handle(Command::Cancel { token }).await;
    assert!(matches!(response, Response::Cancelled { ok: true, .. }));
    match engine.handle(Command::Status).await {
        Response::Status(status) => assert_eq!(status.state, cosmo_ipc::State::Idle),
        other => panic!("{other:?}"),
    }
}

/// Gate invariant #5 through the engine (phase 4, the user's decision): a
/// spoken "confirm" with no trigger key held resolves nothing and is not
/// handed on to the reasoning path; the same words said during a key hold
/// execute the hold locally.
#[tokio::test]
async fn open_mic_confirm_is_refused_and_key_held_confirm_executes() {
    use cosmo_gate::UtteranceSource;
    use cosmo_ipc::TurnResult;

    let engine = engine().await;
    let gate = engine.gate();
    gate.begin_turn();
    let token = gate.park(
        "run_in_terminal",
        json!({"command": "echo held"}),
        "a held command".to_owned(),
    );

    for phrase in ["confirm", "Yes.", "go ahead"] {
        match engine
            .utterance(phrase.into(), UtteranceSource::OpenMic)
            .await
        {
            TurnResult::ConfirmNeedsKey => {}
            // `Failed` here would mean it reached the reasoning path (no
            // agent is connected in this test), which it must not.
            other => panic!("open-mic `{phrase}` must be refused, got {other:?}"),
        }
        assert_eq!(gate.pending().len(), 1, "`{phrase}` released the hold");
    }

    match engine
        .utterance("confirm".into(), UtteranceSource::KeyHeld)
        .await
    {
        TurnResult::ConfirmedLocally { token: t, .. } => assert_eq!(t, token),
        other => panic!("key-held confirm must execute the hold, got {other:?}"),
    }
    assert!(gate.pending().is_empty());
}

/// Typed `cosmo say "confirm"` keeps its phase-1 behaviour: the control
/// socket is owner-only, the same trust as `cosmo confirm`.
#[tokio::test]
async fn typed_confirm_still_resolves_a_hold() {
    let engine = engine().await;
    let gate = engine.gate();
    gate.begin_turn();
    gate.park(
        "run_in_terminal",
        json!({"command": "echo held"}),
        "a held command".to_owned(),
    );
    match engine
        .handle(Command::Say {
            text: "confirm".into(),
        })
        .await
    {
        Response::Said {
            result: cosmo_ipc::TurnResult::ConfirmedLocally { .. },
        } => {}
        other => panic!("typed confirm must execute, got {other:?}"),
    }
    assert!(gate.pending().is_empty());
}
