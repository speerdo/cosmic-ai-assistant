//! `scripts/bench-asr` (phase-3 spec §3.6): the ASR model choice, on the
//! user's own voice.
//!
//! - `record [--list FILE] [--dir DIR] [--redo N,M]` prompts each line of
//!   the command list. Hold the trigger key (Right Ctrl), say it, release.
//!   The clip is cut the way the daemon will cut an utterance: from 750 ms
//!   before the press to 300 ms after the release. Existing clips are kept,
//!   so a session can be resumed; `--redo` re-records chosen ones.
//! - `run [--dir DIR] [--only A,C] [--fast] [--score S]` replays the clips through each
//!   candidate model pairing, one child process per pairing (so memory is
//!   that pairing's alone), and writes a results table.
//!
//! Replay runs twice per pairing: once plain and fast (accuracy only),
//! once with installed app names as hotwords **at real-time pace**, which
//! is where release → commit latency is measured. `--fast` skips the pacing
//! for a quick look; its latencies are then not meaningful. `--score` sets
//! how hard hotwords pull (default: `SttConfig`'s).
//!
//! Recordings stay on this machine, under `~/.local/share/cosmo/bench/`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cosmo_audio::{CAPTURE_RATE, Capture, SpeechGate};
use cosmo_hotkey::{Edge, Watcher};
use cosmo_stt::score::{normalize, word_errors};
use cosmo_stt::{Stt, SttConfig};
use serde::{Deserialize, Serialize};

/// What the daemon will use (§3.7): audio from before the press survives
/// key latency; a little after the release catches the last syllable of
/// someone who lets go as they finish the word.
const PREROLL_MS: u32 = 750;
const TAIL: Duration = Duration::from_millis(300);
/// Shorter holds are taps or shortcuts (findings §3a).
const MIN_HOLD: Duration = Duration::from_millis(300);
/// A command, for the per-kind split: at most this many words.
const COMMAND_WORDS: usize = 6;

const NEMOTRON: &str = "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25";
const FASTCONF: &str = "sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8";
const TDT06: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";
const UNIFIED: &str = "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming";
const M110: &str = "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8";

/// (label, streaming, offline, description)
const PAIRINGS: &[(&str, Option<&str>, Option<&str>, &str)] = &[
    (
        "A",
        Some(NEMOTRON),
        Some(TDT06),
        "nemotron-0.6b + tdt-0.6b-v2",
    ),
    (
        "B",
        Some(NEMOTRON),
        Some(UNIFIED),
        "nemotron-0.6b + unified-0.6b (default)",
    ),
    ("C", Some(NEMOTRON), Some(M110), "nemotron-0.6b + tdt-110m"),
    ("D", Some(NEMOTRON), None, "nemotron-0.6b alone"),
    (
        "E",
        Some(FASTCONF),
        Some(M110),
        "fastconformer-480ms + tdt-110m",
    ),
    ("F", Some(FASTCONF), None, "fastconformer-480ms alone"),
    ("G", None, Some(M110), "tdt-110m alone (no partials)"),
    ("H", None, Some(TDT06), "tdt-0.6b-v2 alone (no partials)"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let dir = flag("--dir").map(PathBuf::from).unwrap_or_else(default_dir);
    match args.first().map(String::as_str) {
        Some("record") => {
            let list = flag("--list").unwrap_or_else(|| "scripts/bench-commands.txt".into());
            let redo: Vec<usize> = flag("--redo")
                .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
                .unwrap_or_default();
            record(Path::new(&list), &dir, &redo);
        }
        Some("run") => {
            let only = flag("--only").map(|s| s.to_uppercase());
            let score = flag("--score");
            run(
                &dir,
                only.as_deref(),
                args.iter().any(|a| a == "--fast"),
                score.as_deref(),
            );
        }
        Some("one") => {
            let score = flag("--score").map(|s| s.parse().expect("--score is a number"));
            one(&args[1], &dir, args.iter().any(|a| a == "--fast"), score);
        }
        _ => {
            eprintln!("usage: bench-asr record [--list FILE] [--dir DIR] [--redo N,M]");
            eprintln!("       bench-asr run [--dir DIR] [--only A,C] [--fast] [--score S]");
            std::process::exit(2);
        }
    }
}

fn all_hotwords() -> bool {
    std::env::var("COSMO_HOTWORDS").as_deref() == Ok("all")
}

fn default_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share")
        });
    data.join("cosmo/bench/commands")
}

// ---- manifest ------------------------------------------------------------

/// `NN.wav<TAB>what was said`, one line per clip, in list order.
fn read_manifest(dir: &Path) -> BTreeMap<usize, String> {
    std::fs::read_to_string(dir.join("manifest.tsv"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (file, text) = l.split_once('\t')?;
            Some((file.strip_suffix(".wav")?.parse().ok()?, text.to_owned()))
        })
        .collect()
}

fn write_manifest(dir: &Path, m: &BTreeMap<usize, String>) {
    let text: String = m
        .iter()
        .map(|(n, t)| format!("{n:02}.wav\t{t}\n"))
        .collect();
    std::fs::write(dir.join("manifest.tsv"), text).expect("write manifest");
}

fn read_wav(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

// ---- record --------------------------------------------------------------

fn record(list: &Path, dir: &Path, redo: &[usize]) {
    let lines: Vec<String> = std::fs::read_to_string(list)
        .unwrap_or_else(|e| panic!("{}: {e}", list.display()))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    std::fs::create_dir_all(dir).expect("create bench dir");
    let mut manifest = read_manifest(dir);

    let key = cosmo_config::load().map(|c| c.trigger_key).unwrap_or(97);
    // Nothing plays during a recording session, so the gate never closes.
    let capture = Capture::start(SpeechGate::new(), 30).expect("capture");
    let (tx, edges) = mpsc::channel();
    let watcher = Watcher::start(key, move |ev| {
        let _ = tx.send(ev);
    })
    .expect("hotkey watcher");
    std::thread::sleep(Duration::from_millis(500));
    if watcher.devices().is_empty() {
        eprintln!("no keyboard with key {key} is readable: check `cosmo doctor`");
        std::process::exit(1);
    }
    println!(
        "Recording into {}\nFor each line: hold Right Ctrl, say it, release. \
         Ctrl+C stops; run again to resume.\n",
        dir.display()
    );

    for (i, text) in lines.iter().enumerate() {
        let n = i + 1;
        let file = dir.join(format!("{n:02}.wav"));
        let same = manifest.get(&n) == Some(text);
        if file.exists() && same && !redo.contains(&n) {
            continue;
        }
        print!("[{n:2}/{}] say: \x1b[1m{text}\x1b[0m  ", lines.len());
        std::io::stdout().flush().ok();
        let samples = loop {
            // Drop stale edges (keys pressed while the previous clip saved).
            while edges.try_recv().is_ok() {}
            let press = wait_for(&edges, Edge::Pressed);
            let start = capture.ring().mark_preroll(PREROLL_MS);
            let release = wait_for(&edges, Edge::Released);
            std::thread::sleep(TAIL);
            let end = capture.ring().now();
            let held = release.duration_since(press);
            if held < MIN_HOLD {
                print!("(tap ignored, hold while speaking) ");
                std::io::stdout().flush().ok();
                continue;
            }
            break capture.ring().read(start, end).1;
        };
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        write_wav(&file, &samples);
        manifest.insert(n, text.clone());
        write_manifest(dir, &manifest);
        let quiet = if peak < 0.03 {
            "  \x1b[33mvery quiet: is the mic right?\x1b[0m"
        } else {
            ""
        };
        println!(
            "{:.1}s, peak {:.2}{quiet}",
            samples.len() as f64 / f64::from(CAPTURE_RATE),
            peak
        );
    }
    println!(
        "\n{} clips in {}. Next: scripts/bench-asr run",
        manifest.len(),
        dir.display()
    );
}

fn wait_for(edges: &mpsc::Receiver<cosmo_hotkey::TriggerEvent>, want: Edge) -> Instant {
    loop {
        let ev = edges.recv().expect("hotkey watcher stopped");
        if ev.edge == want {
            return ev.at;
        }
    }
}

fn write_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: CAPTURE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).expect("create wav");
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}

// ---- run (parent) --------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Outcome {
    label: String,
    load_s: f64,
    rss_loaded_mb: f64,
    peak_mb: f64,
    clips: Vec<Clip>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Clip {
    n: usize,
    reference: String,
    plain: String,
    hotwords: String,
    latency_ms: f64,
}

fn run(dir: &Path, only: Option<&str>, fast: bool, score: Option<&str>) {
    let manifest = read_manifest(dir);
    if manifest.is_empty() {
        eprintln!(
            "no recordings in {}: run `scripts/bench-asr record` first",
            dir.display()
        );
        std::process::exit(1);
    }
    let exe = std::env::current_exe().expect("own path");
    let mut outcomes = Vec::new();
    for &(label, _, _, desc) in PAIRINGS {
        if only.is_some_and(|o| !o.split(',').any(|l| l.trim() == label)) {
            continue;
        }
        println!("{label}: {desc} …");
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["one", label, "--dir"]).arg(dir);
        if fast {
            cmd.arg("--fast");
        }
        if let Some(s) = score {
            cmd.args(["--score", s]);
        }
        let out = cmd
            .stderr(std::process::Stdio::inherit())
            .output()
            .expect("run child");
        let last = String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .unwrap_or_default()
            .to_owned();
        match serde_json::from_str::<Outcome>(&last) {
            Ok(o) => outcomes.push(o),
            Err(e) => eprintln!("  {label} failed ({}): {e}", out.status),
        }
    }
    let mut report = report(&outcomes, fast);
    let score = score.map_or_else(
        || {
            SttConfig::defaults()
                .map_or(0.0, |c| c.hotwords_score)
                .to_string()
        },
        str::to_owned,
    );
    let set = if all_hotwords() {
        "every installed app name"
    } else {
        "curated (phase 4: rare app names + domain words)"
    };
    report.insert_str(0, &format!("Hotwords: {set}, score {score}.\n\n"));
    println!("\n{report}");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let path = dir.join(format!("results-{stamp}.md"));
    std::fs::write(&path, &report).expect("write results");
    println!("written to {}", path.display());
}

#[derive(Default)]
struct Tally {
    errors: usize,
    words: usize,
    exact: usize,
    clips: usize,
}

impl Tally {
    fn add(&mut self, reference: &str, hypothesis: &str) {
        let (r, h) = (normalize(reference), normalize(hypothesis));
        let e = word_errors(&r, &h);
        self.errors += e;
        self.words += r.len();
        self.exact += usize::from(e == 0);
        self.clips += 1;
    }

    fn wer(&self) -> String {
        if self.words == 0 {
            return "–".into();
        }
        format!("{:.1}%", 100.0 * self.errors as f64 / self.words as f64)
    }

    fn exact(&self) -> String {
        format!("{}/{}", self.exact, self.clips)
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn report(outcomes: &[Outcome], fast: bool) -> String {
    let mut s = String::new();
    s.push_str("| | Pairing | WER commands (hotwords) | exact commands | WER long | WER plain (all) | release → commit p50 / p95 / max | load | RSS loaded / peak |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for o in outcomes {
        let desc = PAIRINGS.iter().find(|p| p.0 == o.label).map_or("", |p| p.3);
        let (mut cmd, mut long, mut plain) = (Tally::default(), Tally::default(), Tally::default());
        for c in &o.clips {
            let t = if normalize(&c.reference).len() <= COMMAND_WORDS {
                &mut cmd
            } else {
                &mut long
            };
            t.add(&c.reference, &c.hotwords);
            plain.add(&c.reference, &c.plain);
        }
        let mut lat: Vec<f64> = o.clips.iter().map(|c| c.latency_ms).collect();
        lat.sort_by(f64::total_cmp);
        s.push_str(&format!(
            "| {} | {desc} | {} | {} | {} | {} | {:.0} / {:.0} / {:.0} ms | {:.1}s | {:.0} / {:.0} MB |\n",
            o.label,
            cmd.wer(),
            cmd.exact(),
            long.wer(),
            plain.wer(),
            percentile(&lat, 0.5),
            percentile(&lat, 0.95),
            lat.last().copied().unwrap_or(0.0),
            o.load_s,
            o.rss_loaded_mb,
            o.peak_mb,
        ));
    }
    if fast {
        s.push_str("\n`--fast`: latencies are not real-time and not meaningful.\n");
    }
    s.push_str("\n### Misrecognitions (hotwords pass)\n\n");
    for o in outcomes {
        for c in &o.clips {
            if word_errors(&normalize(&c.reference), &normalize(&c.hotwords)) > 0 {
                s.push_str(&format!(
                    "- {} #{:02}: said \"{}\" → \"{}\"\n",
                    o.label, c.n, c.reference, c.hotwords
                ));
            }
        }
    }
    s
}

// ---- one pairing (child) -------------------------------------------------

/// `VmRSS` and `VmHWM` (peak) from /proc, in MB.
fn memory_mb() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

fn one(label: &str, dir: &Path, fast: bool, score: Option<f32>) {
    let &(_, streaming, offline, _) = PAIRINGS
        .iter()
        .find(|p| p.0 == label)
        .expect("known pairing");
    let asr = cosmo_stt::model::default_asr_dir().expect("cache dir");
    let mut config = SttConfig::defaults().expect("cache dir");
    config.streaming = streaming.map(|m| asr.join(m));
    config.offline = offline.map(|m| asr.join(m));
    if let Some(s) = score {
        config.hotwords_score = s;
    }

    let t = Instant::now();
    let stt = Stt::load(&config).unwrap_or_else(|e| panic!("{e}"));
    let load_s = t.elapsed().as_secs_f64();
    let (rss_loaded_mb, _) = memory_mb();

    // `COSMO_HOTWORDS=all` biases every app name, as before phase 4's
    // curation, for comparison.
    let installed = cosmo_stt::hotwords::desktop_apps();
    let apps = if all_hotwords() {
        let mut all = cosmo_stt::hotwords::Hotwords::new();
        all.add(installed.iter().map(|a| a.name.as_str()));
        all
    } else {
        cosmo_stt::hotwords::curated(&installed)
    };
    let apps = apps.for_app(None);

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let transcribe = |samples: &[f32], hotwords: &str, paced: bool| {
        let mut session = stt.session(hotwords).expect("session");
        let start = Instant::now();
        let mut fed = 0usize;
        for chunk in samples.chunks(1024) {
            fed += chunk.len();
            if paced {
                let due = Duration::from_secs_f64(fed as f64 / f64::from(CAPTURE_RATE));
                std::thread::sleep(due.saturating_sub(start.elapsed()));
            }
            session.push(chunk);
        }
        rt.block_on(session.finish()).expect("finish")
    };

    let clips = read_manifest(dir)
        .into_iter()
        .map(|(n, reference)| {
            let samples = read_wav(&dir.join(format!("{n:02}.wav")));
            let plain = transcribe(&samples, "", false).text;
            let hot = transcribe(&samples, &apps, !fast);
            eprintln!(
                "  #{n:02} {:>4.0}ms  {}",
                hot.latency.as_secs_f64() * 1e3,
                hot.text
            );
            Clip {
                n,
                reference,
                plain,
                hotwords: hot.text,
                latency_ms: hot.latency.as_secs_f64() * 1e3,
            }
        })
        .collect();
    let (_, peak_mb) = memory_mb();
    let outcome = Outcome {
        label: label.to_owned(),
        load_s,
        rss_loaded_mb,
        peak_mb,
        clips,
    };
    println!("{}", serde_json::to_string(&outcome).unwrap());
}
