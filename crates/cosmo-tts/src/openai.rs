//! The OpenAI TTS provider (spec §2.4) — the first builtin, chosen for the
//! zero-native-dependency vertical slice: plain HTTPS, the phase-1 key
//! source, no model downloads.
//!
//! Endpoint: `POST {base}/v1/audio/speech` with `gpt-4o-mini-tts`, which
//! takes an `instructions` field for affect/tone/pacing (sourced from
//! `voice_instructions` in config). Response format is `wav` — it decodes
//! through [`Pcm::from_wav_bytes`] like everything else, and 24 kHz mono
//! is what comes back.
//!
//! Errors follow the §1.4 discipline — three states, three fixes: missing
//! key ([`TtsError::NoKey`]), transport ([`TtsError::Network`] — which a
//! request past [`DEFAULT_REQUEST_TIMEOUT`] becomes, so a stalled server
//! cannot hang the speak path), rate limit
//! ([`TtsError::RateLimited`]); everything else lands in
//! [`TtsError::Synthesis`] with the status and a body snippet. The key
//! itself never appears in an error or a span (it is a redacting
//! [`SecretKey`], and only the status and header *names* are logged).

use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tracing::Instrument as _;

use cosmo_config::secret::SecretKey;

use crate::TtsError;
use crate::pcm::Pcm;
use crate::provider::{Accent, LatencyClass, Voice, VoiceProvider};
use crate::registry::ProviderInit;

/// The catalogue OpenAI's TTS endpoints speak (blueprint §4's list, minus
/// `marin`/`cedar`, which exist only on the Realtime model cosmo does not
/// use for speech). Gender is deliberately `None`: OpenAI does not document
/// one per voice.
pub const VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse",
];

/// The voice `"default"` resolves to (config's `voice_id` default).
pub const DEFAULT_VOICE: &str = "alloy";

/// The model used when [`ProviderInit::model`] is `None`.
pub const DEFAULT_MODEL: &str = "gpt-4o-mini-tts";

const PROD_BASE: &str = "https://api.openai.com";

/// Ceiling on one synthesis when [`ProviderInit::request_timeout`] is unset.
/// Generous for a sentence of speech and far short of forever: without it a
/// stalled connection hangs `cosmo say` with no error at all, which is not
/// one of the three states §1.4 promises.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct OpenAiTts {
    http: reqwest::Client,
    base_url: String,
    key: SecretKey,
    model: String,
    instructions: Option<String>,
}

impl OpenAiTts {
    /// Constructor the registry's `openai` factory calls. `init.base_url`
    /// overrides the production endpoint (tests, self-hosted gateways).
    pub fn new(init: &ProviderInit) -> Result<Self, TtsError> {
        let key = init.api_key.clone().ok_or(TtsError::NoKey)?;
        let http = reqwest::Client::builder()
            .timeout(init.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT))
            .build()
            .map_err(|e| TtsError::Network(format!("http client: {e}")))?;
        Ok(Self {
            http,
            base_url: init
                .base_url
                .clone()
                .unwrap_or_else(|| PROD_BASE.to_owned()),
            key,
            model: init
                .model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            instructions: init.instructions.clone().filter(|s| !s.trim().is_empty()),
        })
    }
}

/// Registry constructor under the name `"openai"`.
pub fn factory(init: &ProviderInit) -> Result<Box<dyn VoiceProvider>, TtsError> {
    Ok(Box::new(OpenAiTts::new(init)?))
}

impl VoiceProvider for OpenAiTts {
    fn id(&self) -> &str {
        "openai"
    }

    fn list_voices(&self) -> Vec<Voice> {
        VOICES
            .iter()
            .map(|id| Voice {
                id: (*id).to_owned(),
                label: (*id).to_owned(),
                accent: Accent::from_code("en-US"),
                gender: None,
                sample: None,
            })
            .collect()
    }

    /// The `speak/synthesize` span (agreed in phase 1's span-name set) wraps
    /// the whole round trip. Only the voice and the text *length* are
    /// recorded — never the text and never the key.
    fn synthesize(&self, text: &str, voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
        let span = tracing::info_span!(
            "speak/synthesize",
            provider = "openai",
            voice = %voice,
            text_bytes = text.len(),
        );
        // Own the inputs so the future borrows only `&self` (the trait's
        // elided output lifetime); a clone is nothing next to the HTTPS
        // round trip.
        let text = text.to_owned();
        let voice = voice.to_owned();
        Box::pin(async move { self.synthesize_inner(&text, &voice).instrument(span).await })
    }

    fn is_local(&self) -> bool {
        false
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Network
    }
}

impl OpenAiTts {
    async fn synthesize_inner(&self, text: &str, voice: &str) -> Result<Pcm, TtsError> {
        let voice = resolve_voice(voice)?;
        let mut body = json!({
            "model": self.model,
            "input": text,
            "voice": voice,
            // WAV, not MP3/Opus: it decodes through the same Pcm path as the
            // phrase cache, and there is no audio-codec dependency here.
            "response_format": "wav",
        });
        if let Some(instructions) = &self.instructions {
            body["instructions"] = Value::String(instructions.clone());
        }

        let url = format!("{}/v1/audio/speech", self.base_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            // 429 is its own state (retry later), not a generic failure.
            let snippet = body_snippet(response).await;
            if status.as_u16() == 429 {
                return Err(TtsError::RateLimited(snippet));
            }
            return Err(TtsError::Synthesis(format!("http {status}: {snippet}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TtsError::Network(e.to_string()))?;
        tracing::info!(status = %status, wav_bytes = bytes.len(), "speech received");
        Pcm::from_wav_bytes(&bytes)
    }
}

/// `"default"` defers to the catalogue's first voice; anything else must be
/// a catalogue id — a structured error beats a 400 round trip.
fn resolve_voice(voice: &str) -> Result<&'static str, TtsError> {
    if voice == "default" {
        return Ok(DEFAULT_VOICE);
    }
    VOICES
        .iter()
        .find(|v| **v == voice)
        .copied()
        .ok_or_else(|| TtsError::UnknownVoice {
            provider: "openai".to_owned(),
            voice: voice.to_owned(),
        })
}

/// Error bodies can be long HTML; keep enough to identify, not enough to
/// flood a log.
async fn body_snippet(response: reqwest::Response) -> String {
    let text = response.text().await.unwrap_or_default();
    let text = text.trim();
    if text.len() <= 200 {
        text.to_owned()
    } else {
        let mut cut = 200;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &text[..cut])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(key: &str) -> ProviderInit {
        ProviderInit {
            api_key: Some(SecretKey::from_raw(key.to_owned())),
            ..Default::default()
        }
    }

    #[test]
    fn no_key_is_a_structured_error_before_any_network() {
        let err = OpenAiTts::new(&ProviderInit::default()).unwrap_err();
        assert!(matches!(err, TtsError::NoKey));
        assert!(err.to_string().contains("cosmo auth login"));
    }

    #[test]
    fn default_voice_resolves_and_unknown_voice_is_rejected_locally() {
        assert_eq!(resolve_voice("default").unwrap(), "alloy");
        assert_eq!(resolve_voice("coral").unwrap(), "coral");
        let err = resolve_voice("no-such-voice").unwrap_err();
        assert!(matches!(err, TtsError::UnknownVoice { .. }));
    }

    #[test]
    fn catalogue_carries_american_accents_and_network_latency() {
        let provider = OpenAiTts::new(&init("sk-test")).unwrap();
        let voices = provider.list_voices();
        assert_eq!(voices.len(), VOICES.len());
        assert!(voices.iter().all(|v| v.accent.as_str() == "en-US"));
        assert!(!provider.is_local());
        assert_eq!(provider.latency_class(), LatencyClass::Network);
        assert_eq!(provider.id(), "openai");
    }

    #[test]
    fn instructions_empty_string_is_omitted() {
        let provider = OpenAiTts::new(&ProviderInit {
            api_key: Some(SecretKey::from_raw("sk-test".into())),
            instructions: Some("   ".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(provider.instructions.is_none());
    }
}
