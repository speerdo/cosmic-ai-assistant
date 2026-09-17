//! Integration test: the OpenAI TTS provider against a local fake speech
//! server (spec §2.4). Mirrors `cosmo-reason/tests/tool_loop.rs`: a
//! hand-rolled HTTP server, no HTTP-test dependency. Asserts the request
//! shape the real endpoint expects — path, bearer auth, model/input/voice/
//! response_format, `instructions` present only when configured — and the
//! error mapping: 429 → RateLimited, 401 → Synthesis, transport → Network.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cosmo_config::secret::SecretKey;
use cosmo_tts::{LatencyClass, Pcm, ProviderInit, TtsError, VoiceProvider};

/// What the fake server saw in one request.
#[derive(Clone, Default)]
struct CapturedRequest {
    path: String,
    authorization: String,
    body: Value,
}

/// Spawn a fake speech server answering every request with `status` and a
/// body of `ok(wav)` bytes (success) or `err(json)` text (failure). Returns
/// the base URL and a handle to the captured requests.
async fn spawn_fake_speech(
    status: u16,
    ok_body: Option<Vec<u8>>,
    err_body: Option<Value>,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let handle = captured.clone();

    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            loop {
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

                let path = head_str
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_string();
                let authorization = head_str
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("authorization:")
                            .map(|v| v.trim().to_string())
                    })
                    .unwrap_or_default();
                let body: Value = serde_json::from_slice(&reqbody).unwrap_or(Value::Null);
                captured.lock().unwrap().push(CapturedRequest {
                    path,
                    authorization,
                    body,
                });

                let response = match (&ok_body, &err_body) {
                    (Some(bytes), _) if status == 200 => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: audio/wav\r\ncontent-length: {}\r\n\r\n",
                        bytes.len()
                    )
                    .into_bytes()
                    .tap(bytes),
                    _ => {
                        let text = err_body.clone().map(|v| v.to_string()).unwrap_or_default();
                        format!(
                            "HTTP/1.1 {status} ERR\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{text}",
                            text.len()
                        )
                        .into_bytes()
                    }
                };
                let _ = sock.write_all(&response).await;
                let _ = sock.flush().await;
            }
        }
    });

    (format!("http://{addr}"), handle)
}

/// Append raw bytes after the header block.
trait Tap {
    fn tap(self, bytes: &[u8]) -> Vec<u8>;
}
impl Tap for Vec<u8> {
    fn tap(mut self, bytes: &[u8]) -> Vec<u8> {
        self.extend_from_slice(bytes);
        self
    }
}

/// A small but valid WAV fixture (what OpenAI's `wav` response format
/// returns: 24 kHz PCM).
fn wav_fixture() -> Vec<u8> {
    let samples: Vec<f32> = (0..48).map(|i| (i as f32 / 48.0) * 0.5 - 0.25).collect();
    Pcm::new(24_000, samples).to_wav_bytes().unwrap()
}

fn init_with(base: &str) -> ProviderInit {
    ProviderInit {
        api_key: Some(SecretKey::from_raw("sk-test-KEY".into())),
        base_url: Some(base.to_string()),
        model: Some("gpt-4o-mini-tts".into()),
        instructions: Some("warm and patient".into()),
    }
}

#[tokio::test]
async fn happy_path_posts_the_expected_request_and_decodes_wav() {
    let (base, captured) = spawn_fake_speech(200, Some(wav_fixture()), None).await;
    let provider = cosmo_tts::OpenAiTts::new(&init_with(&base)).expect("provider builds");

    let pcm = provider
        .synthesize("open the terminal", "default")
        .await
        .expect("synthesis succeeds");

    let req = &captured.lock().unwrap()[0];
    assert_eq!(req.path, "/v1/audio/speech");
    assert_eq!(req.authorization, "Bearer sk-test-KEY");
    assert_eq!(req.body["model"], "gpt-4o-mini-tts");
    assert_eq!(req.body["input"], "open the terminal");
    // `voice_id: "default"` resolves to the catalogue's default before it
    // reaches the wire.
    assert_eq!(req.body["voice"], "alloy");
    assert_eq!(req.body["response_format"], "wav");
    assert_eq!(req.body["instructions"], "warm and patient");

    // The reply decodes to the canonical mono buffer.
    assert_eq!(pcm.sample_rate, 24_000);
    assert_eq!(pcm.data.len(), 48);
    assert_eq!(provider.latency_class(), LatencyClass::Network);
    assert!(!provider.is_local());
}

#[tokio::test]
async fn no_instructions_field_when_unconfigured() {
    let (base, captured) = spawn_fake_speech(200, Some(wav_fixture()), None).await;
    let mut init = init_with(&base);
    init.instructions = None;
    let provider = cosmo_tts::OpenAiTts::new(&init).expect("provider builds");

    provider.synthesize("hi", "coral").await.expect("succeeds");

    let req = &captured.lock().unwrap()[0];
    assert_eq!(req.body["voice"], "coral");
    assert!(
        req.body.get("instructions").is_none(),
        "instructions must be omitted, not empty: {}",
        req.body
    );
}

#[tokio::test]
async fn rate_limited_maps_to_its_own_state() {
    let (base, _) =
        spawn_fake_speech(429, None, Some(json!({"error": {"message": "slow down"}}))).await;
    let provider = cosmo_tts::OpenAiTts::new(&init_with(&base)).expect("provider builds");

    let err = provider.synthesize("hi", "alloy").await.unwrap_err();
    assert!(matches!(err, TtsError::RateLimited(_)), "{err:?}");
    assert!(err.to_string().contains("retry later"));
}

#[tokio::test]
async fn http_error_maps_to_synthesis_with_status() {
    let (base, _) = spawn_fake_speech(
        401,
        None,
        Some(json!({"error": {"message": "bad key", "type": "invalid_request_error"}})),
    )
    .await;
    let provider = cosmo_tts::OpenAiTts::new(&init_with(&base)).expect("provider builds");

    let err = provider.synthesize("hi", "alloy").await.unwrap_err();
    assert!(matches!(err, TtsError::Synthesis(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("401"), "{msg}");
    assert!(msg.contains("bad key"), "{msg}");
    // The key itself never appears in an error string.
    assert!(!msg.contains("sk-test-KEY"), "{msg}");
}

#[tokio::test]
async fn unreachable_server_maps_to_network() {
    // Port 1 on localhost: nothing listens there.
    let provider = cosmo_tts::OpenAiTts::new(&init_with("http://127.0.0.1:1")).expect("builds");
    let err = provider.synthesize("hi", "alloy").await.unwrap_err();
    assert!(matches!(err, TtsError::Network(_)), "{err:?}");
}
