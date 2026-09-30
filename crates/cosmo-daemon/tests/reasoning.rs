//! Phase 5 through the engine, against a fake streaming OpenAI server and a
//! fake tool host: confirmation end to end (§5.5) and token discipline
//! (§5.6).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cosmo_daemon::engine::Engine;
use cosmo_daemon::reflex::{Actuator, Reflex};
use cosmo_gate::UtteranceSource;
use cosmo_ipc::{Event, TurnResult};
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_reflex::{AppIndex, Intent, Matcher};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Request bodies the fake server received.
type Bodies = Arc<Mutex<Vec<Value>>>;

/// Streams one scripted response per request; counts requests.
async fn fake_api(responses: Vec<Vec<String>>) -> (String, Arc<AtomicUsize>) {
    let (base, count, _) = fake_api_recording(responses).await;
    (base, count)
}

/// [`fake_api`], also keeping each request's body.
async fn fake_api_recording(responses: Vec<Vec<String>>) -> (String, Arc<AtomicUsize>, Bodies) {
    let bodies: Bodies = Arc::default();
    let seen = Arc::clone(&bodies);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&count);
    tokio::spawn(async move {
        for payloads in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            let mut one = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if sock.read_exact(&mut one).await.is_err() {
                    return;
                }
                head.push(one[0]);
            }
            let head = String::from_utf8_lossy(&head).to_lowercase();
            let len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            let _ = sock.read_exact(&mut body).await;
            if let Ok(v) = serde_json::from_slice(&body) {
                seen.lock().unwrap().push(v);
            }
            served.fetch_add(1, Ordering::SeqCst);
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      x-ratelimit-remaining-requests: 9\r\nconnection: close\r\n\r\n",
                )
                .await;
            for p in payloads {
                let _ = sock.write_all(format!("data: {p}\n\n").as_bytes()).await;
            }
            let _ = sock.write_all(b"data: [DONE]\n\n").await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), count, bodies)
}

fn chunk(delta: Value, finish: Option<&str>) -> String {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}).to_string()
}

fn usage(p: u64, c: u64) -> String {
    json!({"choices": [], "usage": {"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c}})
        .to_string()
}

#[derive(Default)]
struct Tools {
    ran: Mutex<Vec<(String, Value)>>,
}

impl ToolHost for Tools {
    fn tool_schemas(&self) -> Vec<Value> {
        vec![function_schema(
            "run_in_terminal",
            "Run a command",
            json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        )]
    }
    fn annotations_of(&self, _: &str) -> cosmo_gate::Annotations {
        cosmo_gate::Annotations::default()
    }
    fn execute(
        &self,
        name: &str,
        args: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        self.ran.lock().unwrap().push((name.to_owned(), args));
        Box::pin(async { "done".to_owned() })
    }
}

struct NoopActuator;
impl Actuator for NoopActuator {
    fn act<'a>(&'a self, _: &'a Intent) -> futures::future::BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Ok("done".into()) })
    }
}

/// `COSMO_API_BASE` and `OPENAI_API_KEY` are process-wide: tests take turns.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

async fn engine(base: &str) -> (Engine, Arc<Tools>, tokio::sync::broadcast::Receiver<Event>) {
    // SAFETY: tests serialize on `exclusive()`; nothing else reads these.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("COSMO_API_BASE", base);
        std::env::set_var("OPENAI_API_KEY", "sk-test");
    }
    let (events, rx) = tokio::sync::broadcast::channel::<Event>(64);
    let engine = Engine::new(cosmo_config::Config::default(), events).await;
    let tools = Arc::new(Tools::default());
    engine.attach_tools(Arc::clone(&tools) as Arc<dyn ToolHost>, 1);
    engine.attach_reflex(Reflex::new(
        Matcher::new(AppIndex::default()),
        Arc::new(NoopActuator),
    ));
    (engine, tools, rx)
}

/// §5.5: the model's gated call is held; an open-mic "confirm that" does
/// nothing and asks no model; a key-held one runs the call locally, also
/// without a model request.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_call_completes_only_on_a_key_held_confirm_and_never_via_the_model() {
    let _turn = exclusive().await;
    let (base, requests) = fake_api(vec![vec![
        chunk(json!({"content": "I'll reboot it."}), None),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function",
                "function": {"name": "run_in_terminal", "arguments": "{\"command\":\"systemctl reboot\"}"}}]}),
            None,
        ),
        chunk(json!({}), Some("tool_calls")),
        usage(1200, 20),
    ]])
    .await;
    let (engine, tools, _rx) = engine(&base).await;

    let result = engine
        .utterance("reboot the machine".into(), UtteranceSource::KeyHeld)
        .await;
    assert!(
        matches!(&result, TurnResult::Completed { held, .. } if held.len() == 1),
        "{result:?}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(tools.ran.lock().unwrap().is_empty(), "held, not run");

    let result = engine
        .utterance("Confirm that.".into(), UtteranceSource::OpenMic)
        .await;
    assert!(matches!(result, TurnResult::ConfirmNeedsKey), "{result:?}");
    assert_eq!(engine.gate().pending().len(), 1, "still held");

    let result = engine
        .utterance("Confirm that.".into(), UtteranceSource::KeyHeld)
        .await;
    assert!(
        matches!(result, TurnResult::ConfirmedLocally { .. }),
        "{result:?}"
    );
    assert_eq!(
        *tools.ran.lock().unwrap(),
        [(
            "run_in_terminal".to_owned(),
            json!({"command": "systemctl reboot"})
        )]
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "neither confirm attempt asked the model anything"
    );
}

/// §5.6: reflex commands cost no tokens: not one API request. A reasoning
/// turn reports what it cost.
#[tokio::test(flavor = "multi_thread")]
async fn reflex_turns_make_no_requests_and_reasoning_turns_report_usage() {
    let _turn = exclusive().await;
    let (base, requests) = fake_api(vec![vec![
        chunk(json!({"content": "It's quiet today."}), None),
        chunk(json!({}), Some("stop")),
        usage(1100, 6),
    ]])
    .await;
    let (engine, _tools, mut rx) = engine(&base).await;

    for text in [
        "pause",
        "next track",
        "switch to workspace two",
        "maximize this window",
    ] {
        let result = engine
            .utterance(text.into(), UtteranceSource::KeyHeld)
            .await;
        assert!(
            matches!(result, TurnResult::Reflexed { .. }),
            "`{text}`: {result:?}"
        );
    }
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "reflex made API requests"
    );

    let result = engine
        .utterance("how's my day looking".into(), UtteranceSource::Typed)
        .await;
    assert!(
        matches!(&result, TurnResult::Completed { reply, .. } if reply == "It's quiet today."),
        "{result:?}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let usage = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|e| match e {
        Event::Usage {
            prompt_tokens,
            completion_tokens,
            remaining_requests,
            ..
        } => Some((prompt_tokens, completion_tokens, remaining_requests)),
        _ => None,
    });
    assert_eq!(usage, Some((1100, 6, Some(9))));
}

/// §5.7: what `remember` holds reaches the model's system prompt, read
/// fresh for the turn.
#[tokio::test(flavor = "multi_thread")]
async fn remembered_notes_reach_the_system_prompt() {
    let _turn = exclusive().await;
    let state = std::env::temp_dir().join(format!("cosmo-remember-{}", std::process::id()));
    std::fs::create_dir_all(state.join("cosmo")).unwrap();
    std::fs::write(
        state.join("cosmo/remember.txt"),
        "prefers British spelling\nthe build box is called forge\n",
    )
    .unwrap();
    // SAFETY: tests serialize on `exclusive()`.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("XDG_STATE_HOME", &state);
    }
    let (base, _, bodies) = fake_api_recording(vec![vec![
        chunk(json!({"content": "Noted."}), None),
        chunk(json!({}), Some("stop")),
    ]])
    .await;
    let (engine, _tools, _rx) = engine(&base).await;
    let _ = engine
        .utterance("what's my build box called?".into(), UtteranceSource::Typed)
        .await;
    let system = bodies.lock().unwrap()[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(system.contains("- prefers British spelling"), "{system}");
    assert!(
        system.contains("- the build box is called forge"),
        "{system}"
    );
    let _ = std::fs::remove_dir_all(&state);
}
