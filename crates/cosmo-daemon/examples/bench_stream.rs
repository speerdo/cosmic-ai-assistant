//! Sentence-streamed speech, measured (phase-5 spec §5.3): the model's
//! first token → first audio queued, with real Kokoro, against a fake model
//! that streams a three-sentence reply at a realistic pace. Compared with
//! the phase-1 way: synthesize the whole reply once it is complete.
//!
//! Nothing is played: a timing sink records when audio would start.
//!
//! `cargo run --release -p cosmo-daemon --example bench_stream --features speech`

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmo_audio::{AudioError, Clip, Outcome};
use cosmo_daemon::speech::{DefaultSpeechKey, Speech, SpeechSink, StateCell};
use cosmo_reason::secret::{KeySource, SecretKey};
use cosmo_reason::tools::ToolHost;
use cosmo_reason::{ReasonError, Reasoner};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const REPLY: &str = "Your disk is eighty percent full. Most of it is the Flatpak cache and old kernels. \
Clearing the cache would free about twelve gigabytes.";
/// A model-like pace: time to first token, then one word at a time.
const FIRST_TOKEN: Duration = Duration::from_millis(300);
const PER_WORD: Duration = Duration::from_millis(25);

struct Timing {
    first_play: Mutex<Option<Instant>>,
}

impl SpeechSink for Timing {
    fn play(&self, _: Clip) -> BoxFuture<'static, Result<Outcome, AudioError>> {
        self.first_play
            .lock()
            .unwrap()
            .get_or_insert_with(Instant::now);
        Box::pin(async { Ok(Outcome::Played) })
    }
    fn stop(&self) {}
}

async fn fake_model(runs: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..runs {
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
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                .await;
            tokio::time::sleep(FIRST_TOKEN).await;
            for (i, word) in REPLY.split(' ').enumerate() {
                let piece = if i == 0 {
                    word.to_owned()
                } else {
                    format!(" {word}")
                };
                let c = json!({"choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": null}]});
                let _ = sock.write_all(format!("data: {c}\n\n").as_bytes()).await;
                tokio::time::sleep(PER_WORD).await;
            }
            let end = json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
            let _ = sock
                .write_all(format!("data: {end}\n\ndata: [DONE]\n\n").as_bytes())
                .await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

struct Key;
impl KeySource for Key {
    fn resolve(&self) -> Result<SecretKey, ReasonError> {
        Ok(SecretKey::from_raw("sk-test".into()))
    }
}

struct NoTools;
impl ToolHost for NoTools {
    fn tool_schemas(&self) -> Vec<Value> {
        Vec::new()
    }
    fn annotations_of(&self, _: &str) -> cosmo_gate::Annotations {
        cosmo_gate::Annotations::default()
    }
    fn execute(
        &self,
        _: &str,
        _: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        Box::pin(async { String::new() })
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    const RUNS: usize = 3;
    let base = fake_model(RUNS * 2).await;
    // SAFETY: single-threaded setup, before anything reads the variable.
    #[allow(unsafe_code)] // env writes are unsafe in Rust 2024; see SAFETY
    unsafe {
        std::env::set_var("COSMO_API_BASE", &base)
    };
    let cfg = cosmo_config::Config::default();
    let (events, _rx) = tokio::sync::broadcast::channel(64);
    let sink = Arc::new(Timing {
        first_play: Mutex::new(None),
    });
    let speech = Arc::new(Speech::new(
        cfg.clone(),
        Arc::clone(&sink) as Arc<dyn SpeechSink>,
        Arc::new(DefaultSpeechKey),
        Arc::new(StateCell::new(events)),
    ));
    // Load Kokoro first: its one-off load isn't what's measured.
    speech.speak("Warm up.".into());
    tokio::time::sleep(Duration::from_secs(3)).await;

    let mut reasoner = Reasoner::new(Arc::new(cfg), &Key).expect("reasoner");
    let gate = cosmo_gate::Gate::new();
    println!(
        "reply: {} words, first token after {FIRST_TOKEN:?}, then {PER_WORD:?}/word\n",
        REPLY.split(' ').count()
    );
    for run in 1..=RUNS {
        for streamed in [true, false] {
            *sink.first_play.lock().unwrap() = None;
            let first_token = Arc::new(Mutex::new(None::<Instant>));
            let stream = streamed.then(|| speech.speak_stream());
            let on_text = {
                let first_token = Arc::clone(&first_token);
                let stream = &stream;
                move |t: &str| {
                    first_token.lock().unwrap().get_or_insert_with(Instant::now);
                    if let Some(s) = stream {
                        s.push(t);
                    }
                }
            };
            let outcome = reasoner
                .turn_streaming(
                    "how full is my disk?",
                    &gate,
                    &NoTools,
                    &mut Vec::new(),
                    &on_text,
                )
                .await
                .unwrap();
            let reply_done = Instant::now();
            if let Some(s) = stream {
                s.finish();
            } else if let cosmo_reason::ToolOutcome::Reply(r) = outcome {
                speech.speak(r);
            }
            while sink.first_play.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let t0 = first_token.lock().unwrap().unwrap();
            let audio = sink.first_play.lock().unwrap().unwrap().duration_since(t0);
            println!(
                "run {run} {:<10} first token → first audio {:>5.0} ms   (reply finished streaming at {:>5.0} ms)",
                if streamed { "streamed" } else { "whole" },
                audio.as_secs_f64() * 1e3,
                reply_done.duration_since(t0).as_secs_f64() * 1e3
            );
            tokio::time::sleep(Duration::from_secs(2)).await; // let synthesis finish
        }
    }
}
