//! Spoken replies (spec §2.4's daemon box): after a turn completes, the
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
//! The sink is a trait so the core tier (no PipeWire) can test all of the
//! above with a fake; the real sink is `cosmo_audio::Player`, behind the
//! `speech` feature.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::broadcast;

use cosmo_audio::{AudioError, Clip, Outcome};
use cosmo_config::Config;
use cosmo_config::secret::SecretKey;
use cosmo_ipc::{Event, State};
use cosmo_tts::{ProviderInit, Registry, TtsError, VoiceProvider};

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

pub struct Speech {
    cfg: Config,
    registry: Registry,
    sink: Arc<dyn SpeechSink>,
    key: Arc<dyn SpeechKey>,
    state: Arc<StateCell>,
    /// Built lazily on first use (the keyring may be locked at boot) and
    /// cached only once it was built with a key, so `cosmo auth login`
    /// after startup takes effect without a restart.
    provider: Mutex<Option<Arc<dyn VoiceProvider>>>,
    generation: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Speech {
    pub fn new(
        cfg: Config,
        sink: Arc<dyn SpeechSink>,
        key: Arc<dyn SpeechKey>,
        state: Arc<StateCell>,
    ) -> Self {
        Self::with_registry(cfg, Registry::with_builtins(), sink, key, state)
    }

    pub fn with_registry(
        cfg: Config,
        registry: Registry,
        sink: Arc<dyn SpeechSink>,
        key: Arc<dyn SpeechKey>,
        state: Arc<StateCell>,
    ) -> Self {
        Self {
            cfg,
            registry,
            sink,
            key,
            state,
            provider: Mutex::new(None),
            generation: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
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
        let provider = self.provider().await.map_err(|e| e.to_string())?;
        // Providers own the `speak/synthesize` span (they know their id and
        // resolved voice); wrapping it again here would nest a duplicate.
        let pcm = provider
            .synthesize(text, &self.cfg.voice_id)
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
        match self.sink.play(clip).await {
            Ok(Outcome::Played | Outcome::Cancelled) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The configured provider, built on the blocking pool: a local one
    /// loads its model (Kokoro: ~0.6s), which must not stall the runtime.
    async fn provider(self: &Arc<Self>) -> Result<Arc<dyn VoiceProvider>, TtsError> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.provider_blocking())
            .await
            .unwrap_or_else(|e| Err(TtsError::Synthesis(format!("provider setup panicked: {e}"))))
    }

    fn provider_blocking(&self) -> Result<Arc<dyn VoiceProvider>, TtsError> {
        let mut cached = self.provider.lock().unwrap();
        if let Some(provider) = cached.as_ref() {
            return Ok(Arc::clone(provider));
        }
        let mut init = ProviderInit {
            // The same override the reasoner honours: one fake server can
            // stand in for both halves of the OpenAI API in a live test.
            base_url: std::env::var("COSMO_API_BASE").ok(),
            model: Some(self.cfg.voice_model.clone()).filter(|m| !m.trim().is_empty()),
            instructions: Some(self.cfg.voice_instructions.clone()),
            request_timeout: Some(Duration::from_secs(30)),
            ..ProviderInit::default()
        };
        // A local provider is built without touching the keyring at all;
        // only a cloud one goes on to resolve the key.
        let mut provider: Arc<dyn VoiceProvider> = self
            .registry
            .create(&self.cfg.voice_provider, &init)?
            .into();
        if !provider.is_local() {
            init.api_key = self.key.resolve();
            let has_key = init.api_key.is_some();
            provider = self
                .registry
                .create(&self.cfg.voice_provider, &init)?
                .into();
            // Cached only once keyed, so `cosmo auth login` after startup
            // takes effect without a restart.
            if !has_key {
                return Ok(provider);
            }
        }
        *cached = Some(Arc::clone(&provider));
        Ok(provider)
    }

    /// Build the provider now rather than on the first reply, so a local
    /// model's load time is paid at startup — and a missing model shows in
    /// `doctor` before anyone asks cosmo anything.
    pub fn warm(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            match this.provider().await {
                Ok(p) => tracing::info!(
                    provider = p.id(),
                    local = p.is_local(),
                    "voice provider ready"
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "voice provider unavailable");
                    *this.last_error.lock().unwrap() = Some(e.to_string());
                }
            }
        });
    }

    /// `doctor` line: provider, voice, and the last failure if any.
    pub fn doctor(&self) -> (bool, String) {
        let base = format!(
            "provider {}, voice {}",
            self.cfg.voice_provider, self.cfg.voice_id
        );
        match self.last_error.lock().unwrap().as_ref() {
            None => (true, base),
            Some(err) => (false, format!("{base} — last reply not spoken: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
