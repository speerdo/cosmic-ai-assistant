//! The `local` provider: no key needed, and none sent, against a fake
//! OpenAI-compatible server (as Ollama, LM Studio and llama.cpp serve).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_gate::Gate;
use cosmo_reason::secret::{KeySource, SecretKey};
use cosmo_reason::tools::ToolHost;
use cosmo_reason::{ReasonError, Reasoner, ToolOutcome};

/// One chat-completions request: its lower-cased head is kept; the reply
/// streams "Hi there.".
async fn fake_server() -> (String, Arc<Mutex<Option<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(None));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut one = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            sock.read_exact(&mut one).await.unwrap();
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
        *log.lock().unwrap() = Some(head);
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hi there."}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let mut out = String::from(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        );
        for c in chunks {
            out.push_str(&format!("data: {c}\n\n"));
        }
        out.push_str("data: [DONE]\n\n");
        let _ = sock.write_all(out.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    (format!("http://{addr}"), seen)
}

/// No key anywhere: what a fresh machine with no keyring entry looks like.
struct NoKey;
impl KeySource for NoKey {
    fn resolve(&self) -> Result<SecretKey, ReasonError> {
        Err(ReasonError::NoKey("no key stored".into()))
    }
}

struct NoTools;
impl ToolHost for NoTools {
    fn tool_schemas(&self) -> Vec<Value> {
        Vec::new()
    }
    fn annotations_of(&self, _: &str) -> cosmo_gate::Annotations {
        cosmo_gate::Annotations {
            read_only: true,
            destructive: false,
        }
    }
    fn execute(
        &self,
        _: &str,
        _: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        Box::pin(async { String::new() })
    }
}

fn cfg(provider: &str) -> Arc<cosmo_config::Config> {
    Arc::new(cosmo_config::Config {
        provider: provider.into(),
        ..cosmo_config::Config::default()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn local_needs_no_key_and_sends_none() {
    let (base, seen) = fake_server().await;
    // SAFETY: the only test in this binary that sets it.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("COSMO_API_BASE", &base)
    };
    // A cloud provider without a key still refuses, as before.
    assert!(matches!(
        Reasoner::new(cfg("openrouter"), &NoKey),
        Err(ReasonError::NoKey(_))
    ));
    let mut r = Reasoner::new(cfg("local"), &NoKey).expect("local needs no key");
    let outcome = r
        .turn_streaming(
            "hello",
            &Gate::new(),
            &NoTools,
            &mut Vec::new(),
            &|_: &str| {},
        )
        .await
        .unwrap();
    let ToolOutcome::Reply(reply) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(reply, "Hi there.");
    let head = seen.lock().unwrap().clone().unwrap();
    assert!(head.starts_with("post /v1/chat/completions"), "{head}");
    assert!(
        !head.contains("authorization:"),
        "no key, no Authorization header: {head}"
    );
}
