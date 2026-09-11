//! Integration test: the tool loop against a local fake OpenAI server.
//! Exercises the invariant-#1 path: the model's response contains
//! confirmation language alongside a gated tool call ⇒ escalated to Deny,
//! the tool result fed back to the model says so, and nothing executes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_gate::Gate;
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_reason::{ReasonError, Reasoner, ToolOutcome};

static SERVED: AtomicU64 = AtomicU64::new(0);

/// Fake chat-completions server: request 1 scripts a tool call whose
/// response text contains confirmation language (invariant #1); requests
/// 2+ return a plain stop reply so the loop terminates.
async fn spawn_fake_api() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            loop {
                // per-socket: serve keep-alive requests until EOF
                // Read headers, then exactly content-length body bytes.
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
                    // Scripted: confirmation language + a tool call in the same
                    // response (invariant #1).
                    json!({
                        "choices": [{
                            "index": 0,
                            "finish_reason": "tool_calls",
                            "message": {
                                "role": "assistant",
                                "content": "I will remove the filesystem right away, confirmed!",
                                "tool_calls": [{
                                    "id": "call-1",
                                    "type": "function",
                                    "function": {
                                        "name": "run_in_terminal",
                                        "arguments": "{\"command\": \"sudo rm -rf /\"}"
                                    }
                                }]
                            }
                        }],
                        "usage": {"total_tokens": 15}
                    })
                    .to_string()
                } else {
                    json!({
                        "choices": [{
                            "index": 0,
                            "finish_reason": "stop",
                            "message": {"role": "assistant", "content": "Cannot do that."}
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
                // Keep-alive: loop handles the next request on this socket.
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
        panic!("execute must never run for a DENIED call");
    }
}

/// Test-only key source.
struct FixedSource;
impl cosmo_reason::secret::KeySource for FixedSource {
    fn resolve(&self) -> Result<cosmo_reason::secret::SecretKey, ReasonError> {
        Ok(cosmo_reason::secret::SecretKey::from_raw("sk-test".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn deny_in_same_response_as_confirmation() {
    // Invariant #1: the model's response contains confirmation language
    // ("confirmed!") alongside the tool call ⇒ escalate to Deny even though
    // the command alone would only ever hold.
    let gate = Gate::new();
    gate.set_lock_state(cosmo_gate::LockState::Unlocked);
    let host = FakeHost;

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
    let outcome = match reasoner
        .turn("remove everything", &gate, &host, &mut history)
        .await
    {
        Ok(o) => o,
        Err(e) => panic!("turn failed: {e}"),
    };

    // The gate denied the tool call; the DENIED text was fed back to the
    // model, which then produced a stop reply.
    let denied = history
        .iter()
        .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap_or("").starts_with("DENIED"));
    assert!(denied, "denied tool call must be reported to the model");
    match outcome {
        ToolOutcome::Reply(reply) => assert_eq!(reply, "Cannot do that."),
        other => panic!("unexpected outcome: {other:?}"),
    }
}
