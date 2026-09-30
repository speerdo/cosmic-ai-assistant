//! `scripts/bench-reflex` (phase-4 spec §4.6): release → ack, measured on
//! the user's own recorded commands, not estimated.
//!
//! Each `scripts/bench-asr record` clip is replayed through the **same ears
//! controller the daemon runs**: a ring written at real-time pace, a press
//! where the user's press was (750 ms into the clip, after the pre-roll)
//! and a release where theirs was (300 ms before the end). The committed
//! transcript goes through the real reflex matcher; the actuator is a dry
//! run (nothing on the desktop moves), and "ack" is the moment the cached
//! phrase would be pushed to playback.
//!
//! `scripts/bench-reflex [--dir DIR]`

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmo_audio::{CAPTURE_RATE, Ring};
use cosmo_daemon::ears::{self, Host, Models, Trigger};
use cosmo_gate::UtteranceSource;
use cosmo_ipc::{Event, State};
use cosmo_reflex::{AppIndex, Matcher, THRESHOLD};
use cosmo_stt::{Stt, SttConfig};
use tokio::sync::mpsc;

const RATE: usize = CAPTURE_RATE as usize;
const PREROLL: Duration = Duration::from_millis(750);
const TAIL: Duration = Duration::from_millis(300);

/// A committed turn: when, what was heard, and the reflex match (intent in
/// words, time to match), if any.
type Turn = (Instant, String, Option<(String, Duration)>);

/// Records when each hop happened.
struct Clock {
    matcher: Matcher,
    turn: Mutex<Option<Turn>>,
}

impl Host for Clock {
    fn set_state(&self, _: State) {}
    fn emit(&self, _: Event) {}
    fn interrupt_speech(&self) {}
    fn paused(&self) -> bool {
        false
    }
    fn hotwords(&self) -> String {
        String::new()
    }
    fn turn(self: Arc<Self>, text: String, _: UtteranceSource) {
        let at = Instant::now();
        // What the engine does next: match, then (dry) act and ack.
        let t = Instant::now();
        let matched = self
            .matcher
            .match_text(&text)
            .filter(|m| m.confidence >= THRESHOLD)
            .map(|m| (m.intent.describe(), t.elapsed()));
        *self.turn.lock().unwrap() = Some((at, text, matched));
    }
}

fn read_wav(path: &std::path::Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

/// Write `clip` into `ring` at real-time pace (then silence), from `t0`.
fn play_into(ring: Arc<Ring>, clip: Vec<f32>, t0: Instant, stop: Arc<AtomicBool>) {
    let mut written = 0usize;
    let mut rest = clip.into_iter();
    while !stop.load(Ordering::SeqCst) {
        let due = t0 + Duration::from_secs_f64(written as f64 / RATE as f64);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        let mut chunk: Vec<f32> = rest.by_ref().take(1024).collect();
        chunk.resize(1024, 0.0);
        ring.write(&chunk);
        written += chunk.len();
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // `RUST_LOG=cosmo_daemon::ears=debug` shows each release tail and decode.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .map(|i| std::path::PathBuf::from(&args[i + 1]))
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME");
            std::path::PathBuf::from(home).join(".local/share/cosmo/bench/commands")
        });
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).unwrap_or_else(|_| {
        panic!(
            "no recordings in {}: scripts/bench-asr record",
            dir.display()
        )
    });

    let t = Instant::now();
    let stt = tokio::task::spawn_blocking(|| Stt::load(&SttConfig::defaults().unwrap()))
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("{e}"));
    println!("models ready in {:.1}s\n", t.elapsed().as_secs_f64());

    let mut reflex_ms = Vec::new();
    let mut transcript_ms = Vec::new();
    println!("{:<62} {:>9} {:>7}  result", "said", "→commit", "→ack");
    for line in manifest.lines() {
        let Some((file, said)) = line.split_once('\t') else {
            continue;
        };
        let clip = read_wav(&dir.join(file));
        let secs = clip.len() as f64 / RATE as f64;

        let ring = Arc::new(Ring::with_seconds(30));
        let host = Arc::new(Clock {
            matcher: Matcher::new(AppIndex::installed()),
            turn: Mutex::new(None),
        });
        let (triggers, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(ears::run(
            Arc::clone(&ring),
            Models::ready(stt.clone()),
            rx,
            Arc::clone(&host) as Arc<dyn Host>,
            Arc::new(AtomicBool::new(false)),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let t0 = Instant::now();
        let mic = {
            let (ring, stop) = (Arc::clone(&ring), Arc::clone(&stop));
            std::thread::spawn(move || play_into(ring, clip, t0, stop))
        };
        tokio::time::sleep_until((t0 + PREROLL).into()).await;
        triggers.send(Trigger::Press(Instant::now())).unwrap();
        let release_at = t0 + Duration::from_secs_f64(secs) - TAIL;
        tokio::time::sleep_until(release_at.into()).await;
        let released = Instant::now();
        triggers.send(Trigger::Release(released)).unwrap();
        drop(triggers);
        task.await.unwrap();
        stop.store(true, Ordering::SeqCst);
        mic.join().unwrap();

        let turn = host.turn.lock().unwrap().take();
        let Some((at, text, matched)) = turn else {
            println!("{said:<62} {:>9} {:>7}  (no transcript)", "-", "-");
            continue;
        };
        let commit = at.duration_since(released).as_secs_f64() * 1e3;
        transcript_ms.push(commit);
        match matched {
            Some((intent, match_time)) => {
                let ack = commit + match_time.as_secs_f64() * 1e3;
                reflex_ms.push(ack);
                println!("{said:<62} {commit:>7.0}ms {ack:>5.0}ms  {intent}");
            }
            None => println!("{said:<62} {commit:>7.0}ms {:>7}  escalate: {text:?}", "-"),
        }
    }

    reflex_ms.sort_by(f64::total_cmp);
    transcript_ms.sort_by(f64::total_cmp);
    println!();
    if !transcript_ms.is_empty() {
        println!(
            "release → commit, all {}: p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms",
            transcript_ms.len(),
            pct(&transcript_ms, 0.5),
            pct(&transcript_ms, 0.95),
            transcript_ms.last().unwrap()
        );
    }
    if !reflex_ms.is_empty() {
        println!(
            "release → ack, {} reflex commands: p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms (budget 150 ms, plus the phrase's playback start)",
            reflex_ms.len(),
            pct(&reflex_ms, 0.5),
            pct(&reflex_ms, 0.95),
            reflex_ms.last().unwrap()
        );
    }
}
