//! The tool loop over Anthropic's Messages API, against a local fake that
//! streams Messages events: the same loop and gate as chat completions,
//! with the request headers each provider expects.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_gate::Gate;
use cosmo_reason::secret::{KeySource, SecretKey};
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_reason::{ReasonError, Reasoner, ToolOutcome};

/// A request the fake saw: its lower-cased headers and its body.
#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: Value,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.head
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}:")))
            .map(str::trim)
    }
}

/// Serves each list of events to one request, in order.
async fn fake_api(scripts: Vec<Vec<Value>>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        for events in scripts {
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
            sock.read_exact(&mut body).await.unwrap();
            log.lock().unwrap().push(Seen {
                head,
                body: serde_json::from_slice(&body).unwrap(),
            });
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      anthropic-ratelimit-requests-remaining: 49\r\n\
                      anthropic-ratelimit-tokens-remaining: 39000\r\nconnection: close\r\n\r\n",
                )
                .await;
            for e in events {
                let kind = e["type"].as_str().unwrap().to_owned();
                let _ = sock
                    .write_all(format!("event: {kind}\ndata: {e}\n\n").as_bytes())
                    .await;
            }
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn start(input: u64) -> Value {
    json!({"type": "message_start", "message": {"type": "message", "role": "assistant",
        "content": [], "usage": {"input_tokens": input, "output_tokens": 1}}})
}
fn text(index: u64, t: &str) -> Vec<Value> {
    vec![
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": t}}),
        json!({"type": "content_block_stop", "index": index}),
    ]
}
fn tool_use(index: u64, id: &str, name: &str, input: &str) -> Vec<Value> {
    vec![
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": input}}),
        json!({"type": "content_block_stop", "index": index}),
    ]
}
fn stop(reason: &str, output: u64) -> Vec<Value> {
    vec![
        json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": output}}),
        json!({"type": "message_stop"}),
    ]
}

struct Key;
impl KeySource for Key {
    fn resolve(&self) -> Result<SecretKey, ReasonError> {
        Ok(SecretKey::from_raw("sk-ant-test".into()))
    }
}

#[derive(Default)]
struct Host {
    ran: Mutex<Vec<(String, Value)>>,
}

impl ToolHost for Host {
    fn tool_schemas(&self) -> Vec<Value> {
        vec![
            function_schema(
                "list_windows",
                "List windows",
                json!({"type": "object", "properties": {}}),
            ),
            function_schema(
                "run_in_terminal",
                "Run a command",
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            ),
        ]
    }
    fn annotations_of(&self, name: &str) -> cosmo_gate::Annotations {
        cosmo_gate::Annotations {
            read_only: name == "list_windows",
            destructive: name != "list_windows",
        }
    }
    fn execute(
        &self,
        name: &str,
        args: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        self.ran.lock().unwrap().push((name.to_owned(), args));
        Box::pin(async { "firefox, codium".to_owned() })
    }
}

/// `COSMO_API_BASE` is process-wide: tests take turns.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

fn reasoner(base: &str, cfg: cosmo_config::Config) -> Reasoner {
    // SAFETY: tests serialize on `exclusive()`; nothing else reads it.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("COSMO_API_BASE", base)
    };
    Reasoner::new(Arc::new(cfg), &Key).unwrap()
}

fn anthropic() -> cosmo_config::Config {
    cosmo_config::Config {
        provider: "anthropic".into(),
        ..cosmo_config::Config::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_and_reply_over_the_messages_api() {
    let _turn = exclusive().await;
    let mut round1 = vec![start(800)];
    round1.push(json!({"type": "content_block_start", "index": 0,
        "content_block": {"type": "thinking", "thinking": ""}}));
    round1.push(json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "signature_delta", "signature": "SIG"}}));
    round1.push(json!({"type": "content_block_stop", "index": 0}));
    round1.extend(tool_use(1, "toolu_1", "list_windows", "{}"));
    round1.extend(stop("tool_use", 20));
    let mut round2 = vec![start(850)];
    round2.extend(text(0, "Firefox and VSCodium."));
    round2.extend(stop("end_turn", 6));
    let (base, seen) = fake_api(vec![round1, round2]).await;

    let mut r = reasoner(&base, anthropic());
    let host = Host::default();
    let spoken = Arc::new(Mutex::new(String::new()));
    let sink = {
        let spoken = Arc::clone(&spoken);
        move |t: &str| spoken.lock().unwrap().push_str(t)
    };
    let outcome = r
        .turn_streaming("what's open?", &Gate::new(), &host, &mut Vec::new(), &sink)
        .await
        .unwrap();
    let ToolOutcome::Reply(reply) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(reply, "Firefox and VSCodium.");
    assert_eq!(*spoken.lock().unwrap(), reply);
    assert_eq!(host.ran.lock().unwrap()[0].0, "list_windows");

    let seen = seen.lock().unwrap();
    let first = &seen[0];
    assert_eq!(first.header("x-api-key"), Some("sk-ant-test"));
    assert_eq!(
        first.header("authorization"),
        None,
        "Anthropic gets x-api-key only"
    );
    assert_eq!(first.header("anthropic-version"), Some("2023-06-01"));
    assert!(first.header("user-agent").unwrap().starts_with("cosmo/"));
    assert!(
        first.head.starts_with("post /v1/messages "),
        "{}",
        first.head
    );
    assert_eq!(
        first.body["model"], "claude-haiku-4-5",
        "the provider's default"
    );
    assert_eq!(first.body["stream"], true);
    assert_eq!(first.body["tools"][0]["name"], "list_windows");
    assert!(
        first.body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("You are cosmo")
    );

    // The second request: the blocks went back verbatim (signature and
    // all), then the tool result in a user turn.
    let msgs = seen[1].body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "{msgs:#?}");
    assert_eq!(
        msgs[1]["content"][0],
        json!({"type": "thinking", "thinking": "", "signature": "SIG"})
    );
    assert_eq!(msgs[1]["content"][1]["id"], "toolu_1");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
    assert_eq!(msgs[2]["content"][0]["content"], "firefox, codium");

    let u = r.last_usage();
    assert_eq!(
        (u.prompt_tokens, u.completion_tokens, u.requests),
        (1650, 26, 2)
    );
    assert_eq!(
        (u.remaining_requests, u.remaining_tokens),
        (Some(49), Some(39_000))
    );
}

/// OpenCode Go's Qwen and MiniMax: the Messages format with a Bearer key,
/// and the same session id on every request of the conversation.
#[tokio::test(flavor = "multi_thread")]
async fn opencode_go_in_messages_format_sends_bearer_and_a_stable_session() {
    let _turn = exclusive().await;
    let mut round1 = vec![start(10)];
    round1.extend(tool_use(0, "t", "list_windows", ""));
    round1.extend(stop("tool_use", 2));
    let mut round2 = vec![start(10)];
    round2.extend(text(0, "Done."));
    round2.extend(stop("end_turn", 2));
    let (base, seen) = fake_api(vec![round1, round2]).await;
    let cfg = cosmo_config::Config {
        provider: "opencode-go".into(),
        api_format: "anthropic".into(),
        model: "qwen3.8-plus".into(),
        ..cosmo_config::Config::default()
    };
    let mut r = reasoner(&base, cfg);
    r.turn("hi", &Gate::new(), &Host::default(), &mut Vec::new())
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].header("authorization"), Some("bearer sk-ant-test"));
    assert_eq!(seen[0].header("x-api-key"), None);
    assert!(
        seen[0].body["system"][0].get("cache_control").is_none(),
        "a proxy isn't sent it"
    );
    let session = seen[0]
        .header("x-opencode-session")
        .expect("session header");
    assert_eq!(seen[1].header("x-opencode-session"), Some(session));
    assert_eq!(seen[0].body["model"], "qwen3.8-plus");
}

/// Invariant #1 holds on this wire too: confirmation language in the reply
/// escalates the gated call in it to Deny.
#[tokio::test(flavor = "multi_thread")]
async fn confirmation_language_still_denies_over_the_messages_api() {
    let _turn = exclusive().await;
    let mut round1 = vec![start(10)];
    round1.extend(text(0, "Confirmed, running it now."));
    round1.extend(tool_use(
        1,
        "c",
        "run_in_terminal",
        "{\"command\":\"systemctl reboot\"}",
    ));
    round1.extend(stop("tool_use", 5));
    let mut round2 = vec![start(10)];
    round2.extend(text(0, "It was not permitted."));
    round2.extend(stop("end_turn", 5));
    let (base, _) = fake_api(vec![round1, round2]).await;
    let mut r = reasoner(&base, anthropic());
    let host = Host::default();
    let gate = Gate::new();
    let mut history = Vec::new();
    let outcome = r
        .turn_streaming("reboot", &gate, &host, &mut history, &|_| {})
        .await
        .unwrap();
    assert!(matches!(outcome, ToolOutcome::Reply(_)), "{outcome:?}");
    assert!(host.ran.lock().unwrap().is_empty(), "nothing ran");
    assert!(gate.pending().is_empty(), "denied, not even held");
}

/// A Messages stream cut off before `message_stop` runs nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_cut_off_messages_stream_runs_nothing() {
    let _turn = exclusive().await;
    let mut round = vec![start(10)];
    round.extend(tool_use(0, "c", "list_windows", "{}"));
    let (base, _) = fake_api(vec![round]).await;
    let mut r = reasoner(&base, anthropic());
    let host = Host::default();
    let err = r
        .turn("what's open?", &Gate::new(), &host, &mut Vec::new())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ended early"), "{err}");
    assert!(host.ran.lock().unwrap().is_empty());
}
