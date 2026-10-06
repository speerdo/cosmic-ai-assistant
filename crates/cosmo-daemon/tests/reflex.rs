//! The reflex path through `Engine::utterance` (phase-4 spec §4.2, §4.5),
//! with a recording actuator in place of the real desktop.
//!
//! No agent is connected in these tests, so a turn that reaches the
//! reasoning path comes back `Failed("agent not connected …")`: that result
//! is how a test sees an escalation.

use std::sync::{Arc, Mutex};

use cosmo_daemon::engine::Engine;
use cosmo_daemon::reflex::{Actuator, Reflex};
use cosmo_gate::UtteranceSource;
use cosmo_ipc::{Event, TurnResult};
use cosmo_reflex::{AppIndex, AppRef, Intent, Matcher, MediaCommand};
use futures::future::BoxFuture;

#[derive(Default)]
struct Recorder {
    acted: Mutex<Vec<Intent>>,
    fail: bool,
}

impl Actuator for Recorder {
    fn act<'a>(&'a self, intent: &'a Intent) -> BoxFuture<'a, Result<String, String>> {
        self.acted.lock().unwrap().push(intent.clone());
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err("no such window".into())
            } else {
                Ok("done".into())
            }
        })
    }
}

async fn engine(
    fail: bool,
) -> (
    Engine,
    Arc<Recorder>,
    tokio::sync::broadcast::Receiver<Event>,
) {
    let (events, rx) = tokio::sync::broadcast::channel::<Event>(64);
    let engine = Engine::new(cosmo_config::Config::default(), events).await;
    let recorder = Arc::new(Recorder {
        fail,
        ..Recorder::default()
    });
    let apps = AppIndex::new([AppRef {
        id: "firefox".into(),
        name: "Firefox".into(),
    }]);
    engine.attach_reflex(Reflex::new(
        Matcher::new(apps),
        Arc::clone(&recorder) as Arc<dyn Actuator>,
    ));
    (engine, recorder, rx)
}

fn escalated(r: &TurnResult) -> bool {
    matches!(r, TurnResult::Failed { reason } if reason.contains("agent not connected"))
}

#[tokio::test]
async fn a_reflex_command_is_done_locally_with_tool_events() {
    let (engine, rec, mut rx) = engine(false).await;
    let result = engine
        .utterance("Open Firefox.".into(), UtteranceSource::Typed)
        .await;
    match result {
        TurnResult::Reflexed { action, summary } => {
            assert_eq!(action, "launch Firefox");
            assert_eq!(summary, "done");
        }
        other => panic!("expected Reflexed, got {other:?}"),
    }
    let acted = rec.acted.lock().unwrap().clone();
    assert_eq!(acted.len(), 1);
    assert!(matches!(&acted[0], Intent::Launch(a) if a.id == "firefox"));

    let mut tools = Vec::new();
    while let Ok(e) = rx.try_recv() {
        match e {
            Event::ToolStarted { tool, .. } => tools.push(format!("start {tool}")),
            Event::ToolFinished { tool, ok, .. } => tools.push(format!("finish {tool} {ok}")),
            _ => {}
        }
    }
    assert_eq!(tools, ["start launch_app", "finish launch_app true"]);
}

/// Blueprint §2: if reflex matched but the action failed, escalate rather
/// than report the failure.
#[tokio::test]
async fn a_failed_reflex_action_escalates_to_reasoning() {
    let (engine, rec, _rx) = engine(true).await;
    let result = engine
        .utterance("focus firefox".into(), UtteranceSource::Typed)
        .await;
    assert_eq!(rec.acted.lock().unwrap().len(), 1, "reflex tried first");
    assert!(escalated(&result), "then reasoning: {result:?}");
}

#[tokio::test]
async fn anything_else_goes_straight_to_reasoning() {
    let (engine, rec, _rx) = engine(false).await;
    for text in [
        "What time is it in Tokyo",
        "Close this window",
        "Open Photoshop",
        "Set a timer for ten minutes",
    ] {
        let result = engine.utterance(text.into(), UtteranceSource::Typed).await;
        assert!(escalated(&result), "`{text}`: {result:?}");
    }
    assert!(rec.acted.lock().unwrap().is_empty(), "reflex never acted");
}

/// Volume and the clock are answered here, never by the model.
#[tokio::test]
async fn volume_and_clock_questions_are_reflex() {
    let (engine, rec, _rx) = engine(false).await;
    for text in ["Mute", "What time is it", "Turn the volume up 5%"] {
        let result = engine.utterance(text.into(), UtteranceSource::Typed).await;
        assert!(
            matches!(result, TurnResult::Reflexed { .. }),
            "`{text}`: {result:?}"
        );
    }
    assert_eq!(
        *rec.acted.lock().unwrap(),
        [
            Intent::Volume(cosmo_reflex::VolumeCommand::Mute),
            Intent::Ask(cosmo_reflex::Ask::Time),
            Intent::Volume(cosmo_reflex::VolumeCommand::Up(5)),
        ]
    );
}

/// Safe verbs don't need the key: "pause" from `cosmo listen` just pauses.
/// Only confirmation does (gate invariant #5).
#[tokio::test]
async fn open_mic_reflex_commands_run_but_open_mic_confirms_never_reach_reflex() {
    let (engine, rec, _rx) = engine(false).await;
    let result = engine
        .utterance("pause the music".into(), UtteranceSource::OpenMic)
        .await;
    assert!(matches!(result, TurnResult::Reflexed { .. }), "{result:?}");
    assert_eq!(
        *rec.acted.lock().unwrap(),
        [Intent::Media(MediaCommand::Pause)]
    );

    let result = engine
        .utterance("yes".into(), UtteranceSource::OpenMic)
        .await;
    assert!(matches!(result, TurnResult::ConfirmNeedsKey), "{result:?}");
    assert_eq!(rec.acted.lock().unwrap().len(), 1, "nothing more acted");
}

/// `cosmo say` is the same path (wake-independent: phase-4 spec §4.2).
#[tokio::test]
async fn typed_say_takes_the_reflex_path_too() {
    let (engine, rec, _rx) = engine(false).await;
    let response = engine
        .handle(cosmo_ipc::Command::Say {
            text: "switch to workspace two".into(),
        })
        .await;
    assert!(
        matches!(
            response,
            cosmo_ipc::Response::Said {
                result: TurnResult::Reflexed { .. }
            }
        ),
        "{response:?}"
    );
    assert_eq!(*rec.acted.lock().unwrap(), [Intent::SwitchWorkspace(2)]);
}
