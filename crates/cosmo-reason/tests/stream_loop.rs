//! The tool loop over a **streamed** response (phase-5 spec §5.1), against a
//! local fake that speaks server-sent events and paces them like a model.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_gate::Gate;
use cosmo_reason::secret::{KeySource, SecretKey};
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_reason::{ReasonError, Reasoner, ToolOutcome};

/// One scripted response: SSE payloads, sent `gap` apart. `cut` closes the
/// connection early, after that many payloads.
#[derive(Clone)]
struct Script {
    payloads: Vec<String>,
    gap: Duration,
    cut: Option<usize>,
}

fn text_chunk(t: &str) -> String {
    json!({"choices": [{"index": 0, "delta": {"content": t}, "finish_reason": null}]}).to_string()
}
fn finish(reason: &str) -> String {
    json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}).to_string()
}
fn usage(p: u64, c: u64) -> String {
    json!({"choices": [], "usage": {"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c}}).to_string()
}
fn call_chunk(index: u64, id: Option<&str>, name: Option<&str>, args: &str) -> String {
    let mut call = json!({"index": index, "function": {"arguments": args}});
    if let Some(id) = id {
        call["id"] = json!(id);
        call["type"] = json!("function");
    }
    if let Some(name) = name {
        call["function"]["name"] = json!(name);
    }
    json!({"choices": [{"index": 0, "delta": {"tool_calls": [call]}, "finish_reason": null}]})
        .to_string()
}

/// Serves each script to one request, in order. Records the request bodies.
async fn fake_api(scripts: Vec<Script>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    tokio::spawn(async move {
        for script in scripts {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            // Headers, then content-length bytes of body.
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
            seen.lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap());

            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      x-ratelimit-remaining-requests: 499\r\n\
                      x-ratelimit-remaining-tokens: 199000\r\nconnection: close\r\n\r\n",
                )
                .await;
            for (i, p) in script.payloads.iter().enumerate() {
                if script.cut == Some(i) {
                    break;
                }
                tokio::time::sleep(script.gap).await;
                let _ = sock.write_all(format!("data: {p}\n\n").as_bytes()).await;
                let _ = sock.flush().await;
            }
            if script.cut.is_none() {
                let _ = sock.write_all(b"data: [DONE]\n\n").await;
            }
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), bodies)
}

struct Key;
impl KeySource for Key {
    fn resolve(&self) -> Result<SecretKey, ReasonError> {
        Ok(SecretKey::from_raw("sk-test".into()))
    }
}

/// Records what ran.
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

fn reasoner(base: &str) -> Reasoner {
    // SAFETY: tests serialize on `exclusive()`; nothing else reads it.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("COSMO_API_BASE", base)
    };
    Reasoner::new(Arc::new(cosmo_config::Config::default()), &Key).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn text_streams_out_before_the_turn_ends_and_tool_calls_reassemble() {
    let _turn = exclusive().await;
    let gap = Duration::from_millis(40);
    let (base, bodies) = fake_api(vec![
        // Round 1: a read-only tool call, its name and arguments in pieces.
        Script {
            payloads: vec![
                call_chunk(0, Some("call_1"), Some("list_"), ""),
                call_chunk(0, None, Some("windows"), "{"),
                call_chunk(0, None, None, "}"),
                finish("tool_calls"),
                usage(900, 12),
            ],
            gap,
            cut: None,
        },
        // Round 2: the reply, word by word.
        Script {
            payloads: vec![
                text_chunk("You have "),
                text_chunk("Firefox "),
                text_chunk("and VSCodium open."),
                finish("stop"),
                usage(950, 9),
            ],
            gap,
            cut: None,
        },
    ])
    .await;
    let mut r = reasoner(&base);
    let host = Host::default();
    let started = Instant::now();
    let pieces: Arc<Mutex<Vec<(Duration, String)>>> = Arc::default();
    let sink = {
        let pieces = Arc::clone(&pieces);
        move |t: &str| {
            pieces
                .lock()
                .unwrap()
                .push((started.elapsed(), t.to_owned()))
        }
    };
    let outcome = r
        .turn_streaming("what's open?", &Gate::new(), &host, &mut Vec::new(), &sink)
        .await
        .unwrap();
    let total = started.elapsed();

    let ToolOutcome::Reply(reply) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(reply, "You have Firefox and VSCodium open.");
    let pieces = pieces.lock().unwrap();
    assert_eq!(pieces.len(), 3, "one call per delta: {pieces:?}");
    assert!(
        pieces[0].0 + Duration::from_millis(60) < total,
        "the first words arrived well before the turn ended ({:?} vs {total:?})",
        pieces[0].0
    );
    assert_eq!(host.ran.lock().unwrap()[0].0, "list_windows");

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies[0]["stream"], true);
    assert_eq!(bodies[0]["stream_options"]["include_usage"], true);
    // The reassembled call went back to the model with its result.
    assert_eq!(
        bodies[1]["messages"][2]["tool_calls"][0]["function"]["name"],
        "list_windows"
    );

    let u = r.last_usage();
    assert_eq!(
        (u.prompt_tokens, u.completion_tokens, u.requests),
        (1850, 21, 2)
    );
    assert_eq!(
        (u.remaining_requests, u.remaining_tokens),
        (Some(499), Some(199_000))
    );
}

/// Invariant #1 on a stream: the confirmation language arrives in a
/// different chunk from the gated call, and still escalates it to Deny.
#[tokio::test(flavor = "multi_thread")]
async fn confirmation_language_anywhere_in_the_stream_still_denies() {
    let _turn = exclusive().await;
    let (base, _) = fake_api(vec![
        Script {
            payloads: vec![
                text_chunk("Confirmed, "),
                text_chunk("running it now."),
                call_chunk(
                    0,
                    Some("c"),
                    Some("run_in_terminal"),
                    "{\"command\":\"systemctl reboot\"}",
                ),
                finish("tool_calls"),
            ],
            gap: Duration::from_millis(5),
            cut: None,
        },
        Script {
            payloads: vec![text_chunk("It was not permitted."), finish("stop")],
            gap: Duration::from_millis(5),
            cut: None,
        },
    ])
    .await;
    let mut r = reasoner(&base);
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
    let denied = history
        .iter()
        .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap_or("").starts_with("DENIED"));
    assert!(denied, "{history:#?}");
}

/// A stream that dies mid-call is an error, and the half-built call never
/// reaches the gate or a tool.
#[tokio::test(flavor = "multi_thread")]
async fn a_cut_off_stream_runs_nothing() {
    let _turn = exclusive().await;
    let (base, _) = fake_api(vec![Script {
        payloads: vec![
            call_chunk(0, Some("c"), Some("run_in_terminal"), "{\"command\":\"rm"),
            call_chunk(0, None, None, " -rf ~/scratch\"}"),
            finish("tool_calls"),
        ],
        gap: Duration::from_millis(5),
        cut: Some(1),
    }])
    .await;
    let mut r = reasoner(&base);
    let host = Host::default();
    let err = r
        .turn_streaming("clean up", &Gate::new(), &host, &mut Vec::new(), &|_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cut off"), "{err}");
    assert!(host.ran.lock().unwrap().is_empty());
}
