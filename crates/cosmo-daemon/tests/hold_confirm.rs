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
