//! A held turn must leave `history` a **valid** conversation.
//!
//! The chat-completions API rejects a conversation in which an assistant
//! message announces a `tool_call` that no `role: "tool"` message answers.
//! The tool loop used to `return` the moment the gate parked a call, so the
//! assistant message with its tool_calls stayed in history with no results at
//! all — the daemon then persisted that and replayed it, and *every*
//! subsequent turn failed with a 400. The failure surfaces one turn after the
//! hold, which is the sort of thing a test has to pin because a live session
//! misattributes it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_gate::{Gate, LockState};
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_reason::{ReasonError, Reasoner, ToolOutcome};

static SERVED: AtomicU64 = AtomicU64::new(0);

/// Fake API: one response carrying **two** tool calls, the first of which the
/// gate will park (`systemctl poweroff` ⇒ Hold).
async fn spawn_fake_api() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            loop {
                let mut head = Vec::new();
                let mut one = [0u8; 1];
                loop {
                    if sock.read_exact(&mut one).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(one[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let head_str = String::from_utf8_lossy(&head).to_string();
                let content_length: usize = head_str
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                let mut reqbody = vec![0u8; content_length];
                let _ = sock.read_exact(&mut reqbody).await;

                let n = SERVED.fetch_add(1, Ordering::SeqCst);
                let body = if n == 0 {
                    json!({
                        "choices": [{
                            "index": 0,
                            "finish_reason": "tool_calls",
                            "message": {
                                "role": "assistant",
                                "content": "Shutting down, and I'll check the load first.",
                                "tool_calls": [
                                    {
                                        "id": "call-held",
                                        "type": "function",
                                        "function": {
                                            "name": "run_in_terminal",
                                            "arguments": "{\"command\": \"systemctl poweroff\"}"
                                        }
                                    },
                                    {
                                        "id": "call-after",
                                        "type": "function",
                                        "function": {
                                            "name": "run_in_terminal",
                                            "arguments": "{\"command\": \"htop\"}"
                                        }
                                    }
                                ]
                            }
                        }],
                        "usage": {"total_tokens": 20}
                    })
                    .to_string()
                } else {
                    json!({
                        "choices": [{
                            "index": 0,
                            "finish_reason": "stop",
                            "message": {"role": "assistant", "content": "ok"}
                        }],
                        "usage": {"total_tokens": 1}
                    })
                    .to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        }
    });
    format!("http://{addr}")
}

struct FakeHost;

impl ToolHost for FakeHost {
    fn tool_schemas(&self) -> Vec<Value> {
        vec![function_schema(
            "run_in_terminal",
            "Run a command",
            json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        )]
    }

    fn annotations_of(&self, _tool: &str) -> cosmo_gate::Annotations {
        cosmo_gate::Annotations::default()
    }

    fn execute(
        &self,
        _tool: &str,
        _args: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        panic!("nothing in a held response may execute");
    }
}

struct FixedSource;
impl cosmo_reason::secret::KeySource for FixedSource {
    fn resolve(&self) -> Result<cosmo_reason::secret::SecretKey, ReasonError> {
        Ok(cosmo_reason::secret::SecretKey::from_raw("sk-test".into()))
    }
}

/// Every `tool_call` id announced by an assistant message must be answered by
/// exactly one `role: "tool"` message.
fn assert_history_well_formed(history: &[Value]) {
    for message in history {
        let Some(calls) = message["tool_calls"].as_array() else {
            continue;
        };
        for call in calls {
            let id = call["id"].as_str().expect("tool call id");
            let answers = history
                .iter()
                .filter(|m| m["role"] == "tool" && m["tool_call_id"] == id)
                .count();
            assert_eq!(
                answers, 1,
                "tool_call `{id}` must have exactly one tool result, found {answers}\n\
                 history: {history:#?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn held_turn_leaves_history_replayable() {
    let gate = Gate::new();
    gate.set_lock_state(LockState::Unlocked);
    gate.begin_turn();

    let addr = spawn_fake_api().await;
    // SAFETY: unique var name; tests run their own process.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("COSMO_API_BASE", &addr)
    };

    let cfg = Arc::new(cosmo_config::Config {
        model: "test-model".into(),
        ..cosmo_config::Config::default()
    });
    let mut reasoner = Reasoner::new(cfg, &FixedSource).expect("reasoner");
    let mut history = Vec::new();
    let outcome = reasoner
        .turn("shut the machine down", &gate, &FakeHost, &mut history)
        .await
        .expect("turn");

    match outcome {
        ToolOutcome::Held { tool, .. } => assert_eq!(tool, "run_in_terminal"),
        other => panic!("expected Held, got {other:?}"),
    }
    assert_eq!(gate.pending().len(), 1, "the call must be parked");

    // The point of the test: the conversation is still replayable.
    assert_history_well_formed(&history);

    // The second call in the same response is reported as not run, so the
    // model is not left believing it succeeded.
    let after = history
        .iter()
        .find(|m| m["tool_call_id"] == "call-after")
        .expect("the trailing call needs a result too");
    assert!(
        after["content"]
            .as_str()
            .unwrap_or("")
            .starts_with("NOT RUN"),
        "got {:?}",
        after["content"]
    );
}
