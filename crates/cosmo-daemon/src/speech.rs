//! Spoken replies (spec §2.4's daemon box) and cached reflex phrases
//! (§2.6). After a turn completes, the
//! reply is synthesized with the configured voice provider and played
//! through `cosmo-audio`. This is what makes the reserved `Speaking` state
//! real.
//!
//! Shape:
//!
//! - **Off the response path.** `say` answers the CLI with the reply text at
//!   once; synthesis and playback run in a spawned task. The CLI does not
//!   wait out the audio.
//! - **State:** `Thinking` holds through synthesis (the user hears nothing
//!   yet), flips to `Speaking` when the clip is admitted to playback, and
//!   to `Idle` when it drains.
//! - **A new turn cuts speech off.** [`Speech::interrupt`] bumps a
//!   generation counter and stops playback; a task from an older generation
//!   never touches state again. The check and the state write share one
//!   lock with the interrupt, so a stale task cannot flip a new turn's
//!   `Thinking` to `Idle`.
//! - **Speech failure never fails the turn.** The reply was already
//!   delivered as text; a synthesis or playback error is logged and kept
//!   for `doctor`.
//!
//! - **Reflex phrases are cached per voice** (spec §2.6): rendered to disk
//!   by `cosmo_tts::PhraseCache` and held in memory, so
//!   [`Speech::play_phrase`] is a buffer push. A voice switch keeps the old
//!   voice (replies and phrases) until the new one's phrases are rendered,
//!   then swaps both at once.
//!
//! The sink is a trait so the core tier (no PipeWire) can test all of the
//! above with a fake; the real sink is `cosmo_audio::Player`, behind the
//! `speech` feature.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use tokio::sync::broadcast;

use cosmo_audio::{AudioError, Clip, Outcome};
use cosmo_config::Config;
use cosmo_config::secret::SecretKey;
use cosmo_ipc::{Event, State};
use cosmo_tts::{
    Phrase, PhraseCache, ProviderInit, Registry, TtsError, VoiceKey, VoiceProvider, default_phrases,
};

/// Where synthesized audio goes.
pub trait SpeechSink: Send + Sync {
    /// Queue a clip; the future resolves when its audio has drained.
    fn play(&self, clip: Clip) -> BoxFuture<'static, Result<Outcome, AudioError>>;
    /// Cut off everything playing or queued.
    fn stop(&self);
}

#[cfg(feature = "speech")]
impl SpeechSink for cosmo_audio::Player {
    fn play(&self, clip: Clip) -> BoxFuture<'static, Result<Outcome, AudioError>> {
        Box::pin(cosmo_audio::Player::play(self, &clip).finished())
    }

    fn stop(&self) {
        cosmo_audio::Player::stop(self);
    }
}

/// Resolves the API key for providers that need one. A trait so tests do
/// not touch the real keyring.
pub trait SpeechKey: Send + Sync {
    fn resolve(&self) -> Option<SecretKey>;
}

/// Production: the same source as the reasoner (env var → Secret Service).
pub struct DefaultSpeechKey;

impl SpeechKey for DefaultSpeechKey {
    fn resolve(&self) -> Option<SecretKey> {
        use cosmo_reason::secret::KeySource;
        cosmo_reason::secret::DefaultKeySource.resolve().ok()
    }
}

/// The daemon's state plus the event stream announcing it. Shared between
/// the engine and speech tasks; every write goes through here.
pub struct StateCell {
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
}

impl StateCell {
    pub fn new(events: broadcast::Sender<Event>) -> Self {
        Self {
            state: Mutex::new(State::Idle),
            events,
        }
    }

    pub fn get(&self) -> State {
        *self.state.lock().unwrap()
    }

    /// Broadcast a non-state event on the same stream.
    pub fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    pub fn set(&self, state: State) {
        let mut guard = self.state.lock().unwrap();
        *guard = state;
        let _ = self.events.send(Event::State { state });
    }

    /// Set `state` only if `generation` still equals `expected`. The check
    /// and the write happen under the state lock, which
    /// [`Speech::interrupt`] also takes.
    fn set_if_current(&self, generation: &AtomicU64, expected: u64, state: State) -> bool {
        let mut guard = self.state.lock().unwrap();
        if generation.load(Ordering::SeqCst) != expected {
            return false;
        }
        *guard = state;
        let _ = self.events.send(Event::State { state });
        true
    }
}

/// The voice in use, and its reflex phrases held in memory so an ack is
/// a buffer push with no disk read and no synthesis.
struct Active {
    key: VoiceKey,
    phrases: HashMap<String, Clip>,
}

pub struct Speech {
    cfg: Config,
    registry: Registry,
    sink: Arc<dyn SpeechSink>,
    key: Arc<dyn SpeechKey>,
    state: Arc<StateCell>,
    /// `None` when there is no cache directory (HOME unset): acks then
    /// fall back to live synthesis.
    cache: Option<PhraseCache>,
    vocabulary: Vec<Phrase>,
    active: Mutex<Arc<Active>>,
    /// Providers by (name, model). Built lazily (the keyring may be locked
    /// at boot); a cloud one is cached only once it was built with a key,
    /// so `cosmo auth login` after startup takes effect without a restart.
    providers: Mutex<HashMap<(String, String), Arc<dyn VoiceProvider>>>,
    /// Turn generation: see [`Speech::interrupt`].
    generation: AtomicU64,
    /// Voice-switch generation: a render superseded by a newer switch
    /// never swaps its voice in.
    switch_generation: AtomicU64,
    /// `Some((done, total))` while a phrase render is running.
    rendering: Mutex<Option<(usize, usize)>>,
    last_error: Mutex<Option<String>>,
    /// The last reply's time to first audio (turn handed over → clip
    /// admitted to playback) and its length, for `doctor`.
    last_ttfa: Mutex<Option<(Duration, Duration)>>,
}

/// What `cosmo voice preview` says. Fixed, so previews compare voices and
/// not sentences; cached per voice like any phrase.
const PREVIEW: &str = "Hello, I'm Cosmo. This is how I'll sound when I answer you.";

/// Previews live in their own tree beside the vocabulary: a vocabulary
/// render sweeps its voice directory, and must not take the preview along.
const PREVIEW_DIR: &str = ".previews";

impl Speech {
    pub fn new(
        cfg: Config,
        sink: Arc<dyn SpeechSink>,
        key: Arc<dyn SpeechKey>,
        state: Arc<StateCell>,
    ) -> Self {
        Self::with_registry(cfg, Registry::with_builtins(), sink, key, state)
            .with_cache(PhraseCache::default_root().map(PhraseCache::new))
    }

    pub fn with_registry(
        cfg: Config,
        registry: Registry,
        sink: Arc<dyn SpeechSink>,
        key: Arc<dyn SpeechKey>,
        state: Arc<StateCell>,
    ) -> Self {
        let voice = VoiceKey {
            provider: cfg.voice_provider.clone(),
            voice: cfg.voice_id.clone(),
            model: cfg.voice_model.clone(),
        };
        Self {
            cfg,
            registry,
            sink,
            key,
            state,
            cache: None,
            vocabulary: default_phrases(),
            active: Mutex::new(Arc::new(Active {
                key: voice,
                phrases: HashMap::new(),
            })),
            providers: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            switch_generation: AtomicU64::new(0),
            rendering: Mutex::new(None),
            last_error: Mutex::new(None),
            last_ttfa: Mutex::new(None),
        }
    }

    /// Where phrase WAVs live; `None` disables the on-disk cache.
    pub fn with_cache(mut self, cache: Option<PhraseCache>) -> Self {
        self.cache = cache;
        self
    }

    /// Replace the reflex vocabulary (phase 4 passes its own list).
    pub fn with_vocabulary(mut self, phrases: Vec<Phrase>) -> Self {
        self.vocabulary = phrases;
        self
    }

    fn active(&self) -> Arc<Active> {
        Arc::clone(&self.active.lock().unwrap())
    }

    /// The voice currently speaking.
    pub fn voice(&self) -> VoiceKey {
        self.active().key.clone()
    }

    /// Stop whatever is being said and invalidate every in-flight speech
    /// task. Called at the start of each new turn.
    pub fn interrupt(&self) {
        {
            let _guard = self.state.state.lock().unwrap();
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        self.sink.stop();
    }

    /// Speak `text` in the background. The caller has set `Thinking`; this
    /// task moves it to `Speaking` and then `Idle`.
    pub fn speak(self: &Arc<Self>, text: String) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.speak_inner(&text, generation).await;
            if let Err(err) = &result {
                tracing::warn!(error = %err, "reply not spoken");
            }
            *this.last_error.lock().unwrap() = result.err();
            this.state
                .set_if_current(&this.generation, generation, State::Idle);
        });
    }

    async fn speak_inner(self: &Arc<Self>, text: &str, generation: u64) -> Result<(), String> {
        let started = Instant::now();
        let voice = self.voice();
        let provider = self.provider(&voice).await.map_err(|e| e.to_string())?;
        // Providers own the `speak/synthesize` span (they know their id and
        // resolved voice); wrapping it again here would nest a duplicate.
        let pcm = provider
            .synthesize(text, &voice.voice)
            .await
            .map_err(|e| e.to_string())?;
        let clip = Clip::new(pcm.sample_rate, pcm.data).map_err(|e| e.to_string())?;
        // Admit only if no newer turn began while we were synthesizing.
        if !self
            .state
            .set_if_current(&self.generation, generation, State::Speaking)
        {
            return Ok(());
        }
        *self.last_ttfa.lock().unwrap() = Some((started.elapsed(), clip.duration()));
        match self.sink.play(clip).await {
            Ok(Outcome::Played | Outcome::Cancelled) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Play a cached reflex phrase by key — a buffer push, no synthesis.
    /// Does not touch the daemon state: an ack is short and the caller's
    /// state (Acting, Waiting, …) is the one that matters. Returns `false`
    /// when the phrase is not cached (unknown key, or its render has not
    /// finished yet); the caller decides whether silence is acceptable.
    pub fn play_phrase(&self, key: &str) -> bool {
        let active = self.active();
        let Some(clip) = active.phrases.get(key).cloned() else {
            tracing::debug!(key, "phrase not cached; not played");
            return false;
        };
        let _span = tracing::debug_span!("ack", phrase = key, voice = %active.key.voice).entered();
        tracing::debug!("phrase played from cache");
        let playing = self.sink.play(clip);
        tokio::spawn(async move {
            if let Err(e) = playing.await {
                tracing::warn!(error = %e, "phrase playback failed");
            }
        });
        true
    }

    /// The provider for `voice`, built on the blocking pool: a local one
    /// loads its model (Kokoro: ~0.6s), which must not stall the runtime.
    async fn provider(
        self: &Arc<Self>,
        voice: &VoiceKey,
    ) -> Result<Arc<dyn VoiceProvider>, TtsError> {
        let this = Arc::clone(self);
        let voice = voice.clone();
        tokio::task::spawn_blocking(move || this.provider_blocking(&voice))
            .await
            .unwrap_or_else(|e| Err(TtsError::Synthesis(format!("provider setup panicked: {e}"))))
    }

    fn provider_blocking(&self, voice: &VoiceKey) -> Result<Arc<dyn VoiceProvider>, TtsError> {
        let slot = (voice.provider.clone(), voice.model.clone());
        let mut cached = self.providers.lock().unwrap();
        if let Some(provider) = cached.get(&slot) {
            return Ok(Arc::clone(provider));
        }
        let mut init = ProviderInit {
            // The same override the reasoner honours: one fake server can
            // stand in for both halves of the OpenAI API in a live test.
            base_url: std::env::var("COSMO_API_BASE").ok(),
            model: Some(voice.model.clone()).filter(|m| !m.trim().is_empty()),
            instructions: Some(self.cfg.voice_instructions.clone()),
            request_timeout: Some(Duration::from_secs(30)),
            ..ProviderInit::default()
        };
        // A local provider is built without touching the keyring at all;
        // only a cloud one goes on to resolve the key.
        let mut provider: Arc<dyn VoiceProvider> =
            self.registry.create(&voice.provider, &init)?.into();
        if !provider.is_local() {
            init.api_key = self.key.resolve();
            let has_key = init.api_key.is_some();
            provider = self.registry.create(&voice.provider, &init)?.into();
            if !has_key {
                return Ok(provider);
            }
        }
        cached.insert(slot, Arc::clone(&provider));
        Ok(provider)
    }

    /// Startup: build the configured provider (a local model's load is paid
    /// now, not on the first reply) and bring its phrase cache up to date.
    /// Rendering only what is missing is also what repairs a cache whose
    /// render was killed midway.
    pub fn warm(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let voice = self.voice();
        tokio::spawn(async move {
            match this.provider(&voice).await {
                Ok(p) => tracing::info!(
                    provider = p.id(),
                    local = p.is_local(),
                    "voice provider ready"
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "voice provider unavailable");
                    *this.last_error.lock().unwrap() = Some(e.to_string());
                    return;
                }
            }
            let _ = this.switch_to(voice).await;
        });
    }

    /// Switch to another voice (spec §2.6; §2.7's `cosmo voice set` drives
    /// it). The old voice keeps speaking — replies and cached acks alike —
    /// while the new voice's phrases render, with progress on the event
    /// stream; then both switch together. If the render fails, nothing
    /// switches and the error is kept for `doctor`.
    pub async fn switch_to(self: &Arc<Self>, voice: VoiceKey) -> Result<(), String> {
        let ticket = self.switch_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let result = self.render_and_load(&voice, ticket).await;
        let (ok, detail) = match &result {
            Ok(Some(n)) => (true, format!("{n} phrases ready")),
            Ok(None) => (true, "superseded by a newer voice switch".to_owned()),
            Err(e) => (false, e.clone()),
        };
        *self.rendering.lock().unwrap() = None;
        self.state.emit(Event::VoiceCacheDone {
            provider: voice.provider.clone(),
            voice: voice.voice.clone(),
            ok,
            detail: detail.clone(),
        });
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, voice = %voice.voice, "voice switch failed; keeping the current voice");
                *self.last_error.lock().unwrap() = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Render `voice`'s phrases, load them, and swap them in — unless a
    /// newer switch took over meanwhile (`Ok(None)`).
    async fn render_and_load(
        self: &Arc<Self>,
        voice: &VoiceKey,
        ticket: u64,
    ) -> Result<Option<usize>, String> {
        let provider = self.provider(voice).await.map_err(|e| e.to_string())?;
        let mut phrases = HashMap::new();
        if let Some(cache) = &self.cache {
            let total = self.vocabulary.len();
            *self.rendering.lock().unwrap() = Some((0, total));
            let report = cache
                .render(provider.as_ref(), voice, &self.vocabulary, |p| {
                    *self.rendering.lock().unwrap() = Some((p.done, p.total));
                    self.state.emit(Event::VoiceCacheProgress {
                        provider: voice.provider.clone(),
                        voice: voice.voice.clone(),
                        done: p.done as u32,
                        total: p.total as u32,
                    });
                })
                .await
                .map_err(|e| format!("phrase render failed: {e}"))?;
            tracing::info!(
                voice = %voice.voice,
                rendered = report.rendered,
                reused = report.reused,
                swept = report.swept,
                "phrase cache up to date"
            );
            for phrase in &self.vocabulary {
                let pcm = cache
                    .load(voice, phrase)
                    .ok_or_else(|| format!("phrase {} vanished after render", phrase.key))?;
                let clip = Clip::new(pcm.sample_rate, pcm.data).map_err(|e| e.to_string())?;
                phrases.insert(phrase.key.clone(), clip);
            }
        }
        if self.switch_generation.load(Ordering::SeqCst) != ticket {
            return Ok(None);
        }
        let n = phrases.len();
        *self.active.lock().unwrap() = Arc::new(Active {
            key: voice.clone(),
            phrases,
        });
        Ok(Some(n))
    }

    /// `voice` with `provider` swapped in, or the active provider when
    /// `None`. Another provider starts at its own default model.
    fn target(&self, provider: Option<&str>, voice: &str) -> VoiceKey {
        let active = self.voice();
        match provider {
            Some(p) if p != active.provider => VoiceKey {
                provider: p.to_owned(),
                voice: voice.to_owned(),
                model: String::new(),
            },
            _ => VoiceKey {
                voice: voice.to_owned(),
                ..active
            },
        }
    }

    /// `cosmo voice list`: every voice `provider` (default: the active
    /// one) can speak, plus the active voice resolved when it is that
    /// provider's.
    pub async fn list_voices(
        self: &Arc<Self>,
        provider: Option<&str>,
    ) -> Result<(String, Option<String>, Vec<cosmo_tts::Voice>), String> {
        let active = self.voice();
        let key = self.target(provider, "default");
        let built = self.provider(&key).await.map_err(|e| e.to_string())?;
        let current = (key.provider == active.provider)
            .then(|| built.resolve_voice(&active.voice).to_owned());
        Ok((key.provider, current, built.list_voices()))
    }

    /// `cosmo voice preview`: say a fixed sample line in `voice`, interrupting
    /// whatever is playing. Synthesized once per voice, then cached.
    /// Resolves when the audio has finished.
    pub async fn preview(
        self: &Arc<Self>,
        provider: Option<&str>,
        voice: &str,
    ) -> Result<VoiceKey, String> {
        let key = self.target(provider, voice);
        let built = self.provider(&key).await.map_err(|e| e.to_string())?;
        if !built.has_voice(&key.voice) {
            return Err(unknown_voice(&key));
        }
        let phrase = Phrase::new("preview", PREVIEW);
        let pcm = match &self.cache {
            Some(cache) => {
                let previews = PhraseCache::new(cache.root().join(PREVIEW_DIR));
                previews
                    .ensure(built.as_ref(), &key, std::slice::from_ref(&phrase))
                    .await
                    .map_err(|e| e.to_string())?;
                previews
                    .load(&key, &phrase)
                    .ok_or("preview vanished after render")?
            }
            None => built
                .synthesize(PREVIEW, &key.voice)
                .await
                .map_err(|e| e.to_string())?,
        };
        let clip = Clip::new(pcm.sample_rate, pcm.data).map_err(|e| e.to_string())?;
        self.interrupt();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.state
            .set_if_current(&self.generation, generation, State::Speaking);
        let played = self.sink.play(clip).await;
        self.state
            .set_if_current(&self.generation, generation, State::Idle);
        played.map_err(|e| e.to_string())?;
        Ok(key)
    }

    /// `cosmo voice set`: validate, switch (rendering the phrase cache, old
    /// voice serving meanwhile), and only then persist — a voice that
    /// failed to render is never written to the config.
    pub async fn set_voice(
        self: &Arc<Self>,
        provider: Option<&str>,
        voice: &str,
        config: &std::path::Path,
    ) -> Result<VoiceKey, String> {
        let key = self.target(provider, voice);
        let built = self.provider(&key).await.map_err(|e| e.to_string())?;
        if !built.has_voice(&key.voice) {
            return Err(unknown_voice(&key));
        }
        self.switch_to(key.clone()).await?;
        cosmo_config::set_string_fields(
            config,
            &[("voice_provider", &key.provider), ("voice_id", &key.voice)],
        )
        .map_err(|e| format!("switched to {}, but saving it failed: {e}", key.voice))?;
        Ok(key)
    }

    /// `announce` delivery (spec §2.7): speak `text` in the active voice,
    /// queued behind anything already playing. No state change and no
    /// interrupt — an announcement is not a turn.
    pub async fn announce(self: &Arc<Self>, text: &str) -> Result<(), String> {
        let voice = self.voice();
        let provider = self.provider(&voice).await.map_err(|e| e.to_string())?;
        let pcm = provider
            .synthesize(text, &voice.voice)
            .await
            .map_err(|e| e.to_string())?;
        let clip = Clip::new(pcm.sample_rate, pcm.data).map_err(|e| e.to_string())?;
        self.sink
            .play(clip)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// `doctor` line: provider, voice, phrase-cache state, last failure.
    pub fn doctor(&self) -> (bool, String) {
        let active = self.active();
        let cache = match *self.rendering.lock().unwrap() {
            Some((done, total)) => format!("phrases rendering {done}/{total}"),
            None => format!(
                "{}/{} phrases cached",
                active.phrases.len(),
                self.vocabulary.len()
            ),
        };
        // The resolved voice, when its provider is already built — doctor
        // must answer instantly, so it never builds one.
        let voice = self
            .providers
            .lock()
            .unwrap()
            .get(&(active.key.provider.clone(), active.key.model.clone()))
            .map(|p| p.resolve_voice(&active.key.voice).to_owned())
            .filter(|resolved| *resolved != active.key.voice)
            .map_or_else(
                || active.key.voice.clone(),
                |resolved| format!("{} → {resolved}", active.key.voice),
            );
        let ttfa = match *self.last_ttfa.lock().unwrap() {
            Some((ttfa, audio)) => format!(
                ", last reply: first audio {:.2}s ({:.1}s spoken)",
                ttfa.as_secs_f64(),
                audio.as_secs_f64()
            ),
            None => String::new(),
        };
        let base = format!(
            "provider {}, voice {voice}, {cache}{ttfa}",
            active.key.provider
        );
        match self.last_error.lock().unwrap().as_ref() {
            None => (true, base),
            Some(err) => (false, format!("{base} — {err}")),
        }
    }
}

fn unknown_voice(key: &VoiceKey) -> String {
    format!(
        "{} has no voice \"{}\" — see `cosmo voice list{}`",
        key.provider,
        key.voice,
        if key.provider.is_empty() {
            String::new()
        } else {
            format!(" --provider {}", key.provider)
        }
    )
}

#[cfg(test)]
mod tests {
    /// How many phrases the cache renders: the tests follow the phrase list
    /// rather than hardcoding its length, so adding a phrase doesn't leave
    /// the fake synthesizer waiting on permits nobody grants.
    fn n_phrases() -> usize {
        default_phrases().len()
    }

    use super::*;
    use cosmo_tts::{Accent, LatencyClass, Pcm, Voice};
    use futures::future::ready;
    use tokio::sync::Notify;

    /// Plays nothing; each clip "drains" when the test says so. `notify_one`
    /// stores a permit, so a release sent before the play future is first
    /// polled is not lost.
    #[derive(Default)]
    struct FakeSink {
        release: Arc<Notify>,
        played: Mutex<Vec<usize>>,
        stops: AtomicU64,
    }

    impl SpeechSink for FakeSink {
        fn play(&self, clip: Clip) -> BoxFuture<'static, Result<Outcome, AudioError>> {
            self.played.lock().unwrap().push(clip.samples().len());
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                release.notified().await;
                Ok(Outcome::Played)
            })
        }

        fn stop(&self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.release.notify_one();
        }
    }

    struct NoKey;
    impl SpeechKey for NoKey {
        fn resolve(&self) -> Option<SecretKey> {
            None
        }
    }

    /// Local provider: one sample per character, or an error on "fail".
    struct FakeVoice;
    impl VoiceProvider for FakeVoice {
        fn id(&self) -> &str {
            "fake"
        }
        fn list_voices(&self) -> Vec<Voice> {
            vec![Voice {
                id: "default".into(),
                label: "Fake".into(),
                accent: Accent::from_code("en-US"),
                gender: None,
                sample: None,
            }]
        }
        fn synthesize(&self, text: &str, _voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
            let out = if text == "fail" {
                Err(TtsError::Synthesis("boom".into()))
            } else {
                Ok(Pcm::new(24_000, vec![0.0; text.len()]))
            };
            Box::pin(ready(out))
        }
        fn is_local(&self) -> bool {
            true
        }
        fn latency_class(&self) -> LatencyClass {
            LatencyClass::Fast
        }
    }

    fn setup() -> (
        Arc<Speech>,
        Arc<FakeSink>,
        Arc<StateCell>,
        broadcast::Receiver<Event>,
    ) {
        let (events, rx) = broadcast::channel(64);
        let state = Arc::new(StateCell::new(events));
        let sink = Arc::new(FakeSink::default());
        let mut registry = Registry::new();
        registry.register("fake", |_| Ok(Box::new(FakeVoice)));
        let cfg = Config {
            voice_provider: "fake".into(),
            ..Config::default()
        };
        let speech = Arc::new(Speech::with_registry(
            cfg,
            registry,
            sink.clone(),
            Arc::new(NoKey),
            state.clone(),
        ));
        (speech, sink, state, rx)
    }

    async fn next_state(rx: &mut broadcast::Receiver<Event>) -> State {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                Ok(Ok(Event::State { state })) => return state,
                Ok(Ok(_)) => continue,
                other => panic!("no state event: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn reply_goes_speaking_then_idle_when_audio_drains() {
        let (speech, sink, state, mut rx) = setup();
        state.set(State::Thinking);
        assert_eq!(next_state(&mut rx).await, State::Thinking);

        speech.speak("hello".into());
        assert_eq!(next_state(&mut rx).await, State::Speaking);
        assert_eq!(state.get(), State::Speaking);
        // `Speaking` is set just before the clip reaches the sink.
        while sink.played.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(*sink.played.lock().unwrap(), [5]);

        sink.release.notify_one();
        assert_eq!(next_state(&mut rx).await, State::Idle);
        assert!(speech.doctor().0);
    }

    #[tokio::test]
    async fn a_new_turn_cuts_speech_and_keeps_its_own_state() {
        let (speech, sink, state, mut rx) = setup();
        speech.speak("first reply".into());
        assert_eq!(next_state(&mut rx).await, State::Speaking);

        // Next turn: interrupt, then the engine sets Thinking.
        speech.interrupt();
        state.set(State::Thinking);
        assert_eq!(sink.stops.load(Ordering::SeqCst), 1);
        assert_eq!(next_state(&mut rx).await, State::Thinking);

        // The cut-off task finishes; it must not flip Thinking to Idle.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(state.get(), State::Thinking);
        assert!(rx.try_recv().is_err(), "stale task emitted a state change");
    }

    #[tokio::test]
    async fn synthesis_failure_returns_to_idle_and_reaches_doctor() {
        let (speech, sink, state, mut rx) = setup();
        state.set(State::Thinking);
        assert_eq!(next_state(&mut rx).await, State::Thinking);

        speech.speak("fail".into());
        assert_eq!(next_state(&mut rx).await, State::Idle);
        assert!(sink.played.lock().unwrap().is_empty());
        let (ok, detail) = speech.doctor();
        assert!(!ok);
        assert!(detail.contains("boom"), "{detail}");
    }

    #[tokio::test]
    async fn unknown_provider_is_reported_not_panicked() {
        let (events, mut rx) = broadcast::channel(8);
        let state = Arc::new(StateCell::new(events));
        let cfg = Config {
            voice_provider: "nope".into(),
            ..Config::default()
        };
        let speech = Arc::new(Speech::with_registry(
            cfg,
            Registry::new(),
            Arc::new(FakeSink::default()),
            Arc::new(NoKey),
            state,
        ));
        speech.speak("hi".into());
        assert_eq!(next_state(&mut rx).await, State::Idle);
        assert!(speech.doctor().1.contains("unknown voice provider"));
    }

    // ---- phrase cache + voice switching (spec §2.6) -------------------

    /// Synthesis waits for a permit per call, so a test can hold a render
    /// mid-flight. Audio length encodes the voice: `len(text) × n`, where
    /// `n` is the voice id's trailing digit — so a played clip's length
    /// says which voice it came from.
    struct GatedVoice {
        permits: Arc<tokio::sync::Semaphore>,
    }

    impl VoiceProvider for GatedVoice {
        fn id(&self) -> &str {
            "fake"
        }
        fn list_voices(&self) -> Vec<Voice> {
            ["v1", "v2", "v3", "broken"]
                .into_iter()
                .map(|id| Voice {
                    id: id.into(),
                    label: id.into(),
                    accent: Accent::from_code("en-GB"),
                    gender: None,
                    sample: None,
                })
                .collect()
        }
        fn synthesize(&self, text: &str, voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
            let permits = Arc::clone(&self.permits);
            let n = voice
                .chars()
                .last()
                .and_then(|c| c.to_digit(10))
                .unwrap_or(1) as usize;
            let len = text.len() * n;
            let fail = voice == "broken";
            Box::pin(async move {
                permits.acquire().await.expect("open").forget();
                if fail {
                    Err(TtsError::Synthesis("model exploded".into()))
                } else {
                    Ok(Pcm::new(24_000, vec![0.0; len]))
                }
            })
        }
        fn is_local(&self) -> bool {
            true
        }
        fn latency_class(&self) -> LatencyClass {
            LatencyClass::Fast
        }
    }

    struct Rig {
        speech: Arc<Speech>,
        sink: Arc<FakeSink>,
        permits: Arc<tokio::sync::Semaphore>,
        rx: broadcast::Receiver<Event>,
        root: std::path::PathBuf,
    }

    fn rig(name: &str, voice: &str) -> Rig {
        let root = std::env::temp_dir().join(format!("cosmo-speech-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (events, rx) = broadcast::channel(256);
        let state = Arc::new(StateCell::new(events));
        let sink = Arc::new(FakeSink::default());
        let cfg = Config {
            voice_provider: "fake".into(),
            voice_id: voice.into(),
            ..Config::default()
        };
        let speech =
            Speech::with_registry(cfg, Registry::new(), sink.clone(), Arc::new(NoKey), state)
                .with_cache(Some(PhraseCache::new(&root)));
        let permits = Arc::new(tokio::sync::Semaphore::new(0));
        speech.providers.lock().unwrap().insert(
            ("fake".into(), String::new()),
            Arc::new(GatedVoice {
                permits: Arc::clone(&permits),
            }),
        );
        Rig {
            speech: Arc::new(speech),
            sink,
            permits,
            rx,
            root,
        }
    }

    fn key(voice: &str) -> VoiceKey {
        VoiceKey {
            provider: "fake".into(),
            voice: voice.into(),
            model: String::new(),
        }
    }

    /// "ack-moving" is "Moving it." — 10 characters.
    const ACK: &str = "ack-moving";
    const ACK_LEN: usize = 10;

    async fn next_voice_event(rx: &mut broadcast::Receiver<Event>) -> Event {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                Ok(Ok(e @ (Event::VoiceCacheProgress { .. } | Event::VoiceCacheDone { .. }))) => {
                    return e;
                }
                Ok(Ok(_)) => continue,
                other => panic!("no voice event: {other:?}"),
            }
        }
    }

    async fn played_last(sink: &FakeSink) -> usize {
        for _ in 0..200 {
            if let Some(&n) = sink.played.lock().unwrap().last() {
                return n;
            }
            tokio::task::yield_now().await;
        }
        panic!("nothing played");
    }

    #[tokio::test]
    async fn startup_renders_the_cache_and_acks_become_instant() {
        let mut r = rig("startup", "v1");
        assert!(!r.speech.play_phrase(ACK), "nothing cached before warm");
        r.permits.add_permits(n_phrases());
        r.speech.warm();
        let n = n_phrases() as u32;
        for step in 1..=n {
            match next_voice_event(&mut r.rx).await {
                Event::VoiceCacheProgress { done, total, .. } => {
                    assert_eq!((done, total), (step, n))
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            next_voice_event(&mut r.rx).await,
            Event::VoiceCacheDone { ok: true, .. }
        ));
        assert!(r.speech.play_phrase(ACK));
        assert_eq!(played_last(&r.sink).await, ACK_LEN);
        let n = n_phrases();
        assert!(
            r.speech
                .doctor()
                .1
                .contains(&format!("{n}/{n} phrases cached"))
        );
        let _ = std::fs::remove_dir_all(&r.root);
    }

    /// The §2.6 DoD: switch voice → acks keep coming, instantly, from the
    /// old cache while the new render's progress streams; then everything
    /// switches at once.
    #[tokio::test]
    async fn a_switch_serves_the_old_voice_until_the_new_one_is_ready() {
        let mut r = rig("switch", "v1");
        r.permits.add_permits(n_phrases());
        r.speech.switch_to(key("v1")).await.unwrap();
        while r.rx.try_recv().is_ok() {} // the first render's own events

        let speech = Arc::clone(&r.speech);
        let switching = tokio::spawn(async move { speech.switch_to(key("v2")).await });
        r.permits.add_permits(2);
        for _ in 0..2 {
            assert!(matches!(
                next_voice_event(&mut r.rx).await,
                Event::VoiceCacheProgress { .. }
            ));
        }
        // Mid-render: the old voice answers, and the doctor says so.
        assert_eq!(r.speech.voice().voice, "v1");
        assert!(r.speech.play_phrase(ACK));
        assert_eq!(played_last(&r.sink).await, ACK_LEN);
        assert!(
            r.speech
                .doctor()
                .1
                .contains(&format!("rendering 2/{}", n_phrases())),
            "{}",
            r.speech.doctor().1
        );

        r.permits.add_permits(n_phrases() - 2);
        switching.await.unwrap().unwrap();
        assert_eq!(r.speech.voice().voice, "v2");
        assert!(r.speech.play_phrase(ACK));
        assert_eq!(*r.sink.played.lock().unwrap().last().unwrap(), ACK_LEN * 2);
        let _ = std::fs::remove_dir_all(&r.root);
    }

    #[tokio::test]
    async fn a_failed_render_keeps_the_old_voice() {
        let mut r = rig("failed", "v1");
        r.permits.add_permits(n_phrases());
        r.speech.switch_to(key("v1")).await.unwrap();

        r.permits.add_permits(1);
        let err = r.speech.switch_to(key("broken")).await.unwrap_err();
        assert!(err.contains("model exploded"), "{err}");
        loop {
            if let Event::VoiceCacheDone { ok, voice, .. } = next_voice_event(&mut r.rx).await
                && voice == "broken"
            {
                assert!(!ok);
                break;
            }
        }
        assert_eq!(r.speech.voice().voice, "v1");
        assert!(r.speech.play_phrase(ACK));
        let (ok, detail) = r.speech.doctor();
        assert!(!ok && detail.contains("model exploded"), "{detail}");
        let _ = std::fs::remove_dir_all(&r.root);
    }

    /// Two switches in a row: the first finishes last but must not win.
    #[tokio::test]
    async fn a_superseded_switch_never_lands() {
        let r = rig("superseded", "v1");
        let (a, b) = (Arc::clone(&r.speech), Arc::clone(&r.speech));
        let first = tokio::spawn(async move { a.switch_to(key("v2")).await });
        tokio::task::yield_now().await;
        let second = tokio::spawn(async move { b.switch_to(key("v3")).await });
        // Enough for both renders in full, however they interleave.
        r.permits.add_permits(2 * n_phrases());
        second.await.unwrap().unwrap();
        first.await.unwrap().unwrap();
        assert_eq!(r.speech.voice().voice, "v3");
        let _ = std::fs::remove_dir_all(&r.root);
    }

    /// A render killed midway (here: two phrases on disk, the rest absent)
    /// is completed by the next start — only the gaps are synthesized.
    #[tokio::test]
    async fn the_next_start_completes_an_interrupted_render() {
        let r = rig("repair", "v1");
        let cache = PhraseCache::new(&r.root);
        let vocab = default_phrases();
        r.permits.add_permits(2);
        let provider = r
            .speech
            .providers
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned()
            .unwrap();
        cache
            .render(provider.as_ref(), &key("v1"), &vocab[..2], |_| {})
            .await
            .unwrap();
        let rest = vocab.len() - 2;
        assert_eq!(cache.missing(&key("v1"), &vocab).len(), rest);

        // Exactly the missing phrases' permits: a full re-render would hang.
        r.permits.add_permits(rest);
        tokio::time::timeout(Duration::from_secs(2), r.speech.switch_to(key("v1")))
            .await
            .expect("only the missing phrases are rendered")
            .unwrap();
        assert!(cache.missing(&key("v1"), &vocab).is_empty());
        assert!(r.speech.play_phrase("confirm-hold"));
        let _ = std::fs::remove_dir_all(&r.root);
    }

    // ---- voice CLI back end (spec §2.7) --------------------------------

    fn config_in(root: &std::path::Path) -> std::path::PathBuf {
        let path = root.join("config.ron");
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(&path, cosmo_config::commented_default()).unwrap();
        path
    }

    #[tokio::test]
    async fn set_switches_first_and_persists_only_on_success() {
        let r = rig("set", "v1");
        let config = config_in(&r.root);
        r.permits.add_permits(n_phrases());
        let key = r.speech.set_voice(None, "v2", &config).await.unwrap();
        assert_eq!(key.voice, "v2");
        assert_eq!(r.speech.voice().voice, "v2");
        let saved = cosmo_config::load_from(&config).unwrap();
        assert_eq!(
            (saved.voice_provider.as_str(), saved.voice_id.as_str()),
            ("fake", "v2")
        );

        // A render that fails switches nothing and writes nothing.
        r.permits.add_permits(1);
        let before = std::fs::read_to_string(&config).unwrap();
        assert!(r.speech.set_voice(None, "broken", &config).await.is_err());
        assert_eq!(r.speech.voice().voice, "v2");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
        let _ = std::fs::remove_dir_all(&r.root);
    }

    #[tokio::test]
    async fn set_and_preview_refuse_unknown_voices_before_doing_anything() {
        let r = rig("typo", "v1");
        let config = config_in(&r.root);
        let before = std::fs::read_to_string(&config).unwrap();
        let err = r.speech.set_voice(None, "v9", &config).await.unwrap_err();
        assert!(
            err.contains("no voice \"v9\"") && err.contains("cosmo voice list"),
            "{err}"
        );
        assert!(r.speech.preview(None, "v9").await.is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
        assert!(r.sink.played.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r.root);
    }

    /// A preview is synthesized once per voice and kept apart from the
    /// vocabulary, so the sweep of a later vocabulary render keeps it.
    #[tokio::test]
    async fn preview_is_cached_speaks_and_survives_a_vocabulary_render() {
        let mut r = rig("preview", "v1");
        r.permits.add_permits(1);
        let speech = Arc::clone(&r.speech);
        let previewing = tokio::spawn(async move { speech.preview(None, "v3").await });
        assert_eq!(next_state(&mut r.rx).await, State::Speaking);
        assert_eq!(played_last(&r.sink).await, PREVIEW.len() * 3);
        r.sink.release.notify_one();
        previewing.await.unwrap().unwrap();
        assert_eq!(next_state(&mut r.rx).await, State::Idle);

        // Render v3's vocabulary (sweeps v3's directory), then preview
        // again with no permit left for a re-synthesis: it must come from
        // the cache.
        r.permits.add_permits(n_phrases());
        r.speech.switch_to(key("v3")).await.unwrap();
        r.sink.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), r.speech.preview(None, "v3"))
            .await
            .expect("second preview needs no synthesis")
            .unwrap();
        let _ = std::fs::remove_dir_all(&r.root);
    }

    #[tokio::test]
    async fn list_marks_the_resolved_active_voice() {
        let r = rig("list", "v2");
        let (provider, active, voices) = r.speech.list_voices(None).await.unwrap();
        assert_eq!(provider, "fake");
        assert_eq!(active.as_deref(), Some("v2"));
        assert_eq!(voices.len(), 4);
        let _ = std::fs::remove_dir_all(&r.root);
    }

    /// An announcement plays in the active voice, queued, without taking
    /// the daemon's state away from whatever turn is running.
    #[tokio::test]
    async fn announce_speaks_without_touching_state() {
        let r = rig("announce", "v2");
        r.permits.add_permits(1);
        r.sink.release.notify_one();
        r.speech.announce("build done").await.unwrap();
        assert_eq!(*r.sink.played.lock().unwrap(), ["build done".len() * 2]);
        assert_eq!(r.speech.state.get(), State::Idle);
        let _ = std::fs::remove_dir_all(&r.root);
    }

    #[tokio::test]
    async fn doctor_reports_the_last_time_to_first_audio() {
        let (speech, sink, _state, mut rx) = setup();
        speech.speak("hello there".into());
        assert_eq!(next_state(&mut rx).await, State::Speaking);
        sink.release.notify_one();
        assert_eq!(next_state(&mut rx).await, State::Idle);
        let (_, detail) = speech.doctor();
        assert!(detail.contains("last reply: first audio"), "{detail}");
    }
}
