//! Spec §3.7 end to end, minus the hardware: the ears controller the
//! daemon runs, fed a ring written in real time (room noise, then real
//! speech) and synthetic trigger edges, with the real models.
//!
//! Needs `scripts/fetch-models --vad --asr-bench` (default pair plus the
//! 110M model's test clips).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cosmo_audio::{CAPTURE_RATE, Ring};
use cosmo_daemon::ears::{self, Host, Models, Trigger};
use cosmo_ipc::{Event, State};
use cosmo_stt::{Stt, SttConfig};
use tokio::sync::mpsc;

const RATE: usize = CAPTURE_RATE as usize;

#[derive(Default)]
struct TestHost {
    log: Mutex<Vec<String>>,
    finals: Mutex<Vec<(String, u64)>>,
    partials: Mutex<Vec<String>>,
    states: Mutex<Vec<State>>,
    interrupted: AtomicBool,
    paused: AtomicBool,
    turns: Mutex<Vec<(String, cosmo_gate::UtteranceSource)>>,
    levels: std::sync::atomic::AtomicUsize,
}

impl Host for TestHost {
    fn set_state(&self, state: State) {
        self.states.lock().unwrap().push(state);
    }
    fn emit(&self, event: Event) {
        match event {
            Event::Transcript {
                text,
                r#final: true,
                latency_ms,
            } => self
                .finals
                .lock()
                .unwrap()
                .push((text, latency_ms.unwrap())),
            Event::Transcript { text, .. } => self.partials.lock().unwrap().push(text),
            Event::Log { line } => self.log.lock().unwrap().push(line),
            Event::Level { .. } => {
                self.levels.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }
    fn interrupt_speech(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }
    fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    fn hotwords(&self) -> String {
        String::new()
    }
    fn turn(self: Arc<Self>, text: String, source: cosmo_gate::UtteranceSource) {
        self.turns.lock().unwrap().push((text, source));
    }
}

/// One recording streams at a time on the shared models: tests take
/// turns. Async, since each holds its turn across awaits.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

fn models() -> Models {
    static STT: OnceLock<Stt> = OnceLock::new();
    let stt = STT.get_or_init(|| {
        Stt::load(&SttConfig::defaults().unwrap()).unwrap_or_else(|e| panic!("{e}"))
    });
    Models::ready(stt.clone())
}

fn clip(name: &str) -> Vec<f32> {
    let path = cosmo_stt::model::default_asr_dir()
        .unwrap()
        .join("sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8/test_wavs")
        .join(name);
    hound::WavReader::open(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (scripts/fetch-models --asr-bench)", path.display()))
        .into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

fn room(seconds: f32, seed: &mut u32) -> Vec<f32> {
    (0..(seconds * RATE as f32) as usize)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 8) as f32 / (1u32 << 24) as f32 * 0.006 - 0.003
        })
        .collect()
}

/// A microphone: writes `script` into the ring at real-time pace, then
/// room noise until dropped. Returns when the script's sample `n` is due.
struct Mic {
    ring: Arc<Ring>,
    t0: Instant,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mic {
    fn start(script: Vec<f32>) -> Self {
        let ring = Arc::new(Ring::with_seconds(30));
        let stop = Arc::new(AtomicBool::new(false));
        let t0 = Instant::now();
        let (r, s) = (Arc::clone(&ring), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let mut seed = 99;
            let mut written = 0usize;
            let mut script = script.into_iter();
            while !s.load(Ordering::SeqCst) {
                let due = t0 + Duration::from_secs_f64(written as f64 / RATE as f64);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                let mut chunk: Vec<f32> = script.by_ref().take(1024).collect();
                if chunk.len() < 1024 {
                    chunk.extend(room((1024 - chunk.len()) as f32 / RATE as f32, &mut seed));
                }
                r.write(&chunk);
                written += chunk.len();
            }
        });
        Self {
            ring,
            t0,
            stop,
            thread: Some(thread),
        }
    }

    /// The instant sample `n` of the script is written.
    fn at(&self, seconds: f32) -> Instant {
        self.t0 + Duration::from_secs_f32(seconds)
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.take().unwrap().join();
    }
}

async fn sleep_until(t: Instant) {
    tokio::time::sleep_until(t.into()).await;
}

struct Rig {
    mic: Mic,
    host: Arc<TestHost>,
    triggers: mpsc::UnboundedSender<Trigger>,
    task: tokio::task::JoinHandle<()>,
}

/// `script` starts at t = 0; the controller runs against it.
fn rig(script: Vec<f32>) -> Rig {
    // Models first: loading them takes ~2 s for whichever test runs first,
    // and the mic's clock must not be running meanwhile, or that test's
    // key presses land late (a 690 ms hold measured as 220 ms and discarded
    // as a tap).
    let models = models();
    let mic = Mic::start(script);
    let host = Arc::new(TestHost::default());
    let (triggers, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(ears::run(
        Arc::clone(&mic.ring),
        models,
        rx,
        Arc::clone(&host) as Arc<dyn Host>,
        Arc::new(AtomicBool::new(false)),
        None,
    ));
    Rig {
        mic,
        host,
        triggers,
        task,
    }
}

impl Rig {
    /// Close the trigger channel and wait for the controller to finish
    /// what it's doing.
    async fn finish(self) -> Arc<TestHost> {
        drop(self.triggers);
        self.task.await.unwrap();
        drop(self.mic);
        self.host
    }
}

/// 1 s of room, "I love you." (~1 s), 2 s of room.
fn love() -> (Vec<f32>, f32, f32) {
    let mut seed = 1;
    let mut s = room(1.0, &mut seed);
    let speech = clip("en-english.wav");
    let end = 1.0 + speech.len() as f32 / RATE as f32;
    s.extend(speech);
    s.extend(room(2.0, &mut seed));
    (s, 1.0, end)
}

#[tokio::test(flavor = "multi_thread")]
async fn hold_speak_release_commits_a_transcript() {
    let _turn = exclusive().await;
    let (script, from, to) = love();
    let rig = rig(script);
    sleep_until(rig.mic.at(from - 0.2)).await;
    rig.triggers.send(Trigger::Press(Instant::now())).unwrap();
    sleep_until(rig.mic.at(to + 0.1)).await;
    rig.triggers.send(Trigger::Release(Instant::now())).unwrap();
    let host = rig.finish().await;

    let finals = host.finals.lock().unwrap().clone();
    println!(
        "finals {finals:?}, states {:?}",
        host.states.lock().unwrap()
    );
    let [(text, latency)] = &finals[..] else {
        panic!("one final expected: {finals:?}");
    };
    assert!(text.to_lowercase().contains("i love you"), "{text:?}");
    assert!(*latency < 1_000, "release → commit {latency} ms");
    // The waveform's levels streamed while listening: ~20 a second over a
    // hold of about a second, plus the pre-roll.
    let levels = host.levels.load(Ordering::SeqCst);
    assert!((15..=60).contains(&levels), "{levels} level events");
    // The transcript became a turn, marked as spoken during a key hold.
    assert_eq!(
        *host.turns.lock().unwrap(),
        [(text.clone(), cosmo_gate::UtteranceSource::KeyHeld)]
    );
    assert_eq!(
        *host.states.lock().unwrap(),
        [State::Listening, State::Idle]
    );
    assert!(
        host.interrupted.load(Ordering::SeqCst),
        "a press interrupts speech"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn preroll_keeps_words_spoken_before_the_press() {
    let _turn = exclusive().await;
    let (script, from, to) = love();
    let rig = rig(script);
    // The key lands 400 ms into "I love you": the 750 ms pre-roll covers it.
    sleep_until(rig.mic.at(from + 0.4)).await;
    rig.triggers.send(Trigger::Press(Instant::now())).unwrap();
    sleep_until(rig.mic.at(to + 0.1)).await;
    rig.triggers.send(Trigger::Release(Instant::now())).unwrap();
    let host = rig.finish().await;
    let finals = host.finals.lock().unwrap().clone();
    assert!(
        finals
            .first()
            .is_some_and(|f| f.0.to_lowercase().contains("i love you")),
        "first words lost: finals {finals:?}, states {:?}, log {:?}",
        host.states.lock().unwrap(),
        host.log.lock().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tap_is_discarded() {
    let _turn = exclusive().await;
    let (script, from, _) = love();
    let rig = rig(script);
    sleep_until(rig.mic.at(from)).await;
    rig.triggers.send(Trigger::Press(Instant::now())).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    rig.triggers.send(Trigger::Release(Instant::now())).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let host = rig.finish().await;
    assert!(
        host.finals.lock().unwrap().is_empty(),
        "a tap never commits"
    );
    assert!(
        host.turns.lock().unwrap().is_empty(),
        "a tap is never a turn"
    );
    assert_eq!(
        *host.states.lock().unwrap(),
        [State::Listening, State::Idle]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn listen_toggles_a_recording_with_partials() {
    let _turn = exclusive().await;
    let mut seed = 5;
    let mut script = room(0.5, &mut seed);
    script.extend(clip("0.wav")); // 7.4 s
    script.extend(room(1.0, &mut seed));
    let rig = rig(script);
    sleep_until(rig.mic.at(0.3)).await;
    rig.triggers.send(Trigger::Toggle).unwrap();
    sleep_until(rig.mic.at(8.2)).await;
    rig.triggers.send(Trigger::Toggle).unwrap();
    let host = rig.finish().await;

    let partials = host.partials.lock().unwrap().clone();
    let finals = host.finals.lock().unwrap().clone();
    println!("{} partials; finals {finals:?}", partials.len());
    assert!(partials.len() >= 3, "partials stream while listening");
    assert!(
        finals[0].0.to_lowercase().contains("old portrait"),
        "{finals:?}"
    );
    // `cosmo listen` is an open mic: its turns can never confirm.
    let turns = host.turns.lock().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].1, cosmo_gate::UtteranceSource::OpenMic);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_listens_while_paused() {
    let _turn = exclusive().await;
    let (script, from, to) = love();
    let rig = rig(script);
    rig.host.paused.store(true, Ordering::SeqCst);
    sleep_until(rig.mic.at(from)).await;
    rig.triggers.send(Trigger::Press(Instant::now())).unwrap();
    sleep_until(rig.mic.at(to)).await;
    rig.triggers.send(Trigger::Release(Instant::now())).unwrap();
    let host = rig.finish().await;
    assert!(host.states.lock().unwrap().is_empty());
    assert!(host.finals.lock().unwrap().is_empty());
}

// ---- the wake word (phase 7) ----------------------------------------------

/// Kokoro saying `text`, at capture's 16 kHz.
async fn say(text: &str) -> Vec<f32> {
    static TTS: tokio::sync::OnceCell<Box<dyn cosmo_tts::VoiceProvider>> =
        tokio::sync::OnceCell::const_new();
    let tts = TTS
        .get_or_init(|| async {
            cosmo_tts::Registry::with_builtins()
                .create("kokoro", &cosmo_tts::ProviderInit::default())
                .expect("kokoro (scripts/fetch-models)")
        })
        .await;
    let pcm = tts.synthesize(text, "af_heart").await.unwrap();
    // 24 kHz → 16 kHz: a 3-tap low-pass, then linear interpolation.
    let d = &pcm.data;
    let smooth: Vec<f32> = (0..d.len())
        .map(|i| 0.25 * d[i.saturating_sub(1)] + 0.5 * d[i] + 0.25 * d[(i + 1).min(d.len() - 1)])
        .collect();
    let step = f64::from(pcm.sample_rate) / RATE as f64;
    (0..(d.len() as f64 / step) as usize)
        .map(|k| {
            let x = k as f64 * step;
            let (i, f) = (x as usize, x.fract() as f32);
            smooth[i] * (1.0 - f) + smooth.get(i + 1).copied().unwrap_or(smooth[i]) * f
        })
        .collect()
}

/// A rig whose controller listens for "cosmo".
fn wake_rig(script: Vec<f32>) -> (Rig, Arc<ears::Wake>) {
    let models = models();
    let mic = Mic::start(script);
    let host = Arc::new(TestHost::default());
    let (triggers, rx) = mpsc::unbounded_channel();
    let wake = Arc::new(ears::Wake {
        phrase: "cosmo".into(),
        stats: ears::WakeStats::default(),
    });
    let task = tokio::spawn(ears::run(
        Arc::clone(&mic.ring),
        models,
        rx,
        Arc::clone(&host) as Arc<dyn Host>,
        Arc::new(AtomicBool::new(false)),
        Some(Arc::clone(&wake)),
    ));
    (
        Rig {
            mic,
            host,
            triggers,
            task,
        },
        wake,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn hey_cosmo_turns_the_rest_into_an_open_mic_command() {
    let _turn = exclusive().await;
    let mut seed = 21;
    let mut script = room(1.0, &mut seed);
    script.extend(say("Hey Cosmo, pause the music.").await);
    script.extend(room(2.5, &mut seed));
    let end = script.len() as f32 / RATE as f32;
    let (rig, wake) = wake_rig(script);
    sleep_until(rig.mic.at(end)).await;
    let host = rig.finish().await;

    let turns = host.turns.lock().unwrap().clone();
    println!(
        "turns {turns:?}, last heard {:?}",
        wake.stats.last.lock().unwrap()
    );
    let [(command, source)] = &turns[..] else {
        panic!("one turn expected: {turns:?}");
    };
    assert_eq!(
        command.to_lowercase().trim_end_matches('.'),
        "pause the music"
    );
    assert_eq!(
        *source,
        cosmo_gate::UtteranceSource::OpenMic,
        "a wake can never confirm"
    );
    assert_eq!(wake.stats.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(
        *host.states.lock().unwrap(),
        [State::Listening, State::Idle]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_speech_never_wakes_and_leaves_nothing() {
    let _turn = exclusive().await;
    let mut seed = 22;
    let mut script = room(0.5, &mut seed);
    script.extend(clip("0.wav")); // a 7.4 s read sentence
    script.extend(room(1.0, &mut seed));
    script.extend(say("I told my brother about the cosmos last night.").await);
    script.extend(room(1.5, &mut seed));
    let end = script.len() as f32 / RATE as f32;
    let (rig, wake) = wake_rig(script);
    sleep_until(rig.mic.at(end)).await;
    let host = rig.finish().await;

    assert!(host.turns.lock().unwrap().is_empty());
    assert!(
        host.finals.lock().unwrap().is_empty(),
        "no transcript was even shown"
    );
    assert!(
        host.states.lock().unwrap().is_empty(),
        "never went to Listening"
    );
    assert!(
        wake.stats.checks.load(Ordering::SeqCst) >= 1,
        "it did check"
    );
    assert_eq!(wake.stats.wakes.load(Ordering::SeqCst), 0);
    assert!(wake.stats.last.lock().unwrap().is_none(), "nothing kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_wake_with_nothing_after_it_is_dropped() {
    let _turn = exclusive().await;
    let mut seed = 23;
    let mut script = room(1.0, &mut seed);
    script.extend(say("Hey Cosmo.").await);
    script.extend(room(5.5, &mut seed)); // past the 4 s wait for a command
    let end = script.len() as f32 / RATE as f32;
    let (rig, wake) = wake_rig(script);
    sleep_until(rig.mic.at(end)).await;
    let host = rig.finish().await;
    assert_eq!(wake.stats.wakes.load(Ordering::SeqCst), 1);
    assert!(host.turns.lock().unwrap().is_empty(), "no command, no turn");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_key_still_works_with_the_wake_word_on() {
    let _turn = exclusive().await;
    let (script, from, to) = love();
    let (rig, _wake) = wake_rig(script);
    sleep_until(rig.mic.at(from - 0.2)).await;
    rig.triggers.send(Trigger::Press(Instant::now())).unwrap();
    sleep_until(rig.mic.at(to + 0.1)).await;
    rig.triggers.send(Trigger::Release(Instant::now())).unwrap();
    let host = rig.finish().await;
    let turns = host.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert!(turns[0].0.to_lowercase().contains("i love you"));
    assert_eq!(turns[0].1, cosmo_gate::UtteranceSource::KeyHeld);
}
