//! Every message on the wire must survive `serialize → deserialize`.
//!
//! This suite exists because `Response::Confirm` did not. It was a newtype
//! variant wrapping `ConfirmOutcome`, and both enums are internally tagged on
//! `type`, so serde wrote two `type` keys into one map and the client failed
//! with ``duplicate field `type` ``. The daemon had already executed the
//! confirmed action by then — the reply, not the work, was lost, which is the
//! most confusing shape a protocol bug can take.
//!
//! Serialising alone would not have caught it: the bug is on the way back.
//! Assert the round trip, over every variant, or this recurs the next time a
//! nested enum is added.

use cosmo_ipc::*;

fn round_trip_response(response: Response) {
    let json = serde_json::to_string(&response).expect("serialize");
    let back: Response = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("{response:?} does not round-trip: {e}\n  wire: {json}"));
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        json,
        "round trip changed the message"
    );
}

fn hold() -> PendingHold {
    PendingHold {
        token: "38e1b3fb".into(),
        action: "run_in_terminal".into(),
        parked_at_ms: 1_789_133_093_220,
    }
}

#[test]
fn every_response_variant_round_trips() {
    for response in [
        Response::Status(StatusInfo {
            state: State::Waiting,
            paused: false,
            version: "0.1.0".into(),
            pending_holds: vec![hold()],
        }),
        Response::Doctor(DoctorReport {
            checks: vec![DoctorCheck {
                name: "lock policy".into(),
                ok: false,
                detail: "no lock-state source on COSMIC".into(),
            }],
        }),
        Response::Said {
            result: TurnResult::Completed {
                reply: "done".into(),
                held: vec![hold()],
            },
        },
        Response::Said {
            result: TurnResult::ConfirmedLocally {
                token: "38e1b3fb".into(),
                summary: "ok".into(),
            },
        },
        Response::Said {
            result: TurnResult::ConfirmIgnored,
        },
        Response::Said {
            result: TurnResult::Failed {
                reason: "no key stored".into(),
            },
        },
        // The variant that regressed: a tagged enum inside a tagged enum.
        Response::Confirm {
            outcome: ConfirmOutcome::Executed {
                token: "38e1b3fb".into(),
                summary: "installed".into(),
            },
        },
        Response::Confirm {
            outcome: ConfirmOutcome::Unknown,
        },
        Response::Confirm {
            outcome: ConfirmOutcome::AlreadyGone {
                reason: "rejected".into(),
            },
        },
        Response::Cancelled {
            ok: true,
            reason: None,
        },
        Response::Cancelled {
            ok: false,
            reason: Some("no pending hold with that token".into()),
        },
        Response::Toggled { paused: true },
        Response::Error {
            message: "bad request".into(),
        },
    ] {
        round_trip_response(response);
    }
}

#[test]
fn every_command_round_trips() {
    for cmd in [
        Command::Status,
        Command::Doctor,
        Command::Say {
            text: "run htop".into(),
        },
        Command::Confirm {
            token: "38e1b3fb".into(),
        },
        Command::Cancel {
            token: "38e1b3fb".into(),
        },
        Command::Toggle,
    ] {
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: Command = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{cmd:?} does not round-trip: {e}\n  wire: {json}"));
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }
}

/// The full envelope the socket actually carries, both directions.
#[test]
fn daemon_messages_round_trip() {
    let messages = vec![
        DaemonMessage::Response {
            id: 7,
            response: Response::Confirm {
                outcome: ConfirmOutcome::Executed {
                    token: "38e1b3fb".into(),
                    summary: "installed".into(),
                },
            },
        },
        DaemonMessage::Event {
            event: Event::State {
                state: State::Acting,
            },
        },
        DaemonMessage::Event {
            event: Event::Held {
                token: "38e1b3fb".into(),
                action: "run_in_terminal".into(),
            },
        },
        DaemonMessage::Event {
            event: Event::HoldResolved {
                token: "38e1b3fb".into(),
                executed: true,
                summary: "ok".into(),
            },
        },
    ];
    for msg in messages {
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: DaemonMessage = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{msg:?} does not round-trip: {e}\n  wire: {json}"));
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    let req = Request {
        id: 1,
        cmd: Command::Say {
            text: "hello".into(),
        },
    };
    let json = serde_json::to_string(&req).expect("serialize");
    let _back: Request = serde_json::from_str(&json).expect("request round-trip");
}

/// NDJSON framing: no message may contain a bare newline, or the framing
/// splits one message into two.
#[test]
fn messages_are_single_line() {
    let msg = DaemonMessage::Response {
        id: 1,
        response: Response::Said {
            result: TurnResult::Failed {
                reason: "line one\nline two".into(),
            },
        },
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    assert!(
        !json.contains('\n'),
        "a raw newline would break NDJSON framing: {json}"
    );
}
