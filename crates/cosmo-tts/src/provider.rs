//! The provider trait and its value types (spec §2.2, blueprint §4).
//!
//! The trait shape is the blueprint's, with one recorded deviation:
//! [`VoiceProvider::synthesize`] returns a [`BoxFuture`] rather than a plain
//! `Result`. Network providers (OpenAI, §2.4) synthesize over `reqwest` and
//! local ones (Kokoro on `ort`, §2.5) are CPU-bound — both want to run on
//! the daemon's tokio runtime — and a plain-async trait method would not be
//! object-safe, so the registry could not hand out `Box<dyn VoiceProvider>`.

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::TtsError;
use crate::pcm::Pcm;

/// The accent a voice speaks, grouped on by the picker (blueprint §4).
///
/// A newtype over the BCP-47-ish code rather than an enum: the plan names
/// `en-US`/`en-GB`/`en-AU`/`en-IE`, but MeloTTS also ships `en-IN` and
/// Piper voices carry more variants still; the picker only needs the code
/// to sort and group by. [`Accent::from_code`] normalizes underscore forms
/// (`en_US`) and case so provider-specific spellings group together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Accent(String);

impl Accent {
    /// Normalize a provider's accent code: `en_US` → `en-US`, region
    /// upper-cased, language lower-cased.
    pub fn from_code(code: &str) -> Self {
        let lowered = code.trim().replace('_', "-").to_ascii_lowercase();
        match lowered.split_once('-') {
            Some((lang, region)) => Accent(format!("{lang}-{}", region.to_ascii_uppercase())),
            None => Accent(lowered),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Accent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Voice gender, where a provider declares it. `Option` on [`Voice`]
/// because several providers (OpenAI TTS) do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gender {
    Female,
    Male,
    Neutral,
}

/// One selectable voice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voice {
    /// Provider-local id, as passed back to
    /// [`VoiceProvider::synthesize`](VoiceProvider::synthesize) (e.g.
    /// `af_heart`, `alloy`).
    pub id: String,
    /// Human-readable label for the picker.
    pub label: String,
    /// What the picker groups by.
    pub accent: Accent,
    pub gender: Option<Gender>,
    /// Sample utterance for previews; `None` falls back to the CLI's fixed
    /// preview line (spec §2.7).
    pub sample: Option<String>,
}

/// How soon after a request the first audio can exist. Drives which path a
/// caller is willing to use a provider on: the reflex acks want
/// [`LatencyClass::Instant`] via the phrase cache, never a network round
/// trip (blueprint §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Cached buffer push — no synthesis at all.
    Instant,
    /// Local synthesis (Kokoro, Piper).
    Fast,
    /// Network round trip (OpenAI, ElevenLabs).
    Network,
}

/// A text-to-speech provider (blueprint §4).
///
/// All synthesis collapses to mono [`Pcm`] at the provider boundary, so
/// playback and the phrase cache never see channel math.
pub trait VoiceProvider: Send + Sync {
    /// Registry name (`"kokoro"`, `"openai"`, …).
    fn id(&self) -> &str;

    /// Every voice this provider can speak, for `cosmo voice list`.
    fn list_voices(&self) -> Vec<Voice>;

    /// Synthesize a whole utterance.
    fn synthesize(&self, text: &str, voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>>;

    /// Stream synthesis over incoming text chunks. The default maps each
    /// chunk through [`VoiceProvider::synthesize`] in order — good enough
    /// until phase 5 replaces it with real sentence streaming.
    fn stream<'a>(
        &'a self,
        text: BoxStream<'a, String>,
        voice: &str,
    ) -> BoxStream<'a, Result<Pcm, TtsError>> {
        let voice = voice.to_owned();
        text.then(move |chunk| self.synthesize(&chunk, &voice))
            .boxed()
    }

    /// Runs on this machine (no network) — the phrase cache only renders
    /// from local providers.
    fn is_local(&self) -> bool;

    fn latency_class(&self) -> LatencyClass;
}

/// Compile-time guard: providers are shared across the daemon's await
/// points, so the trait objects must stay `Send + Sync`.
#[allow(dead_code)]
fn assert_provider_object_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Box<dyn VoiceProvider>>();
}
