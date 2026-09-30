//! `scripts/bench-wake` (phase-7 spec §7.4): the wake detector over
//! recordings, with the exact pieces the daemon runs (VAD, `WakeTracker`,
//! one unbiased offline decode per stretch of speech).
//!
//! `bench_wake --phrase cosmo [--positives DIR] [--negatives DIR] [WAV…]`
//!
//! Every `*.wav` (16 kHz) in `--negatives` should *not* wake (the user's
//! bench commands by default: none says "cosmo"); every one in
//! `--positives` should. Prints each wake with what was heard, then false
//! accepts, misses and the per-check decode time.

use std::path::{Path, PathBuf};

use cosmo_stt::vad::WINDOW;
use cosmo_stt::wake::{WakeCheck, WakeConfig, WakeTracker, wake_prefix};
use cosmo_stt::{Stt, SttConfig};

fn wavs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "wav"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn read(path: &Path) -> Vec<f32> {
    let r = hound::WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        r.spec().sample_rate,
        16_000,
        "{}: need 16 kHz",
        path.display()
    );
    r.into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

/// Every check the detector would make on `audio`: (heard, decode ms).
fn checks(stt: &Stt, audio: &[f32]) -> Vec<(String, u128)> {
    let mut vad = stt.vad().expect("vad");
    let mut tracker = WakeTracker::new(WakeConfig::DEFAULT);
    // A second of room before, so the first word has an onset.
    let mut padded = vec![0.0; 16_000];
    padded.extend_from_slice(audio);
    padded.extend(vec![0.0; 16_000]);
    let mut spans = Vec::new();
    let mut at = 0u64;
    vad.feed(&padded, |speech| {
        if let Some(WakeCheck::Check { from, to }) = tracker.push(at, WINDOW, speech) {
            spans.push((from as usize, (to as usize).min(padded.len())));
        }
        at += WINDOW as u64;
    });
    spans
        .into_iter()
        .map(|(from, to)| {
            let rx = stt
                .decode_once(padded[from..to].to_vec())
                .expect("offline model");
            let d = rx.blocking_recv().expect("decoded");
            (d.text, d.decode.as_millis())
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .map(|i| args[i + 1].clone())
    };
    let phrase = flag("--phrase").unwrap_or_else(|| "cosmo".into());
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let negatives = flag("--negatives")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share/cosmo/bench/commands"));
    let positives = flag("--positives").map(PathBuf::from);

    let mut config = SttConfig::defaults().expect("cache dir");
    config.streaming = None; // the wake check uses the offline model only
    let stt = Stt::load(&config).unwrap_or_else(|e| panic!("{e}"));

    let mut decode_ms = Vec::new();
    let mut run = |files: Vec<PathBuf>, should_wake: bool| -> (usize, usize) {
        let (mut total, mut wrong) = (0, 0);
        for f in files {
            total += 1;
            let found = checks(&stt, &read(&f));
            decode_ms.extend(found.iter().map(|c| c.1));
            let woke = found.iter().find(|c| wake_prefix(&c.0, &phrase).is_some());
            let name = f.file_name().unwrap().to_string_lossy();
            match (woke, should_wake) {
                (Some((heard, _)), true) => println!("  wake  {name}: {heard:?}"),
                (None, false) => {}
                (Some((heard, _)), false) => {
                    wrong += 1;
                    println!("  FALSE ACCEPT  {name}: {heard:?}");
                }
                (None, true) => {
                    wrong += 1;
                    let heard: Vec<&str> = found.iter().map(|c| c.0.as_str()).collect();
                    println!("  MISSED  {name}: heard {heard:?}");
                }
            }
        }
        (total, wrong)
    };
    let (n_neg, false_accepts) = run(wavs(&negatives), false);
    println!(
        "negatives ({}): {n_neg} clips, {false_accepts} false accepts",
        negatives.display()
    );
    if let Some(p) = positives {
        let (n_pos, misses) = run(wavs(&p), true);
        println!(
            "positives ({}): {n_pos} clips, {} detected, {misses} missed",
            p.display(),
            n_pos - misses
        );
    }
    decode_ms.sort_unstable();
    if !decode_ms.is_empty() {
        println!(
            "{} checks, decode p50 {} ms, max {} ms",
            decode_ms.len(),
            decode_ms[decode_ms.len() / 2],
            decode_ms.last().unwrap()
        );
    }
}
