//! Link spike (phase-3 spec §3.1), part two: **both ONNX Runtimes in one
//! process.** Kokoro (on `ort`) speaks command-like lines; `cosmo-stt`
//! (on sherpa-onnx) transcribes each through a full recording session,
//! plain and with hotwords. Calls alternate between the two runtimes.
//!
//! Since §3.5 this goes through `cosmo_stt`'s own API with the default
//! model pair, not raw sherpa: the proof now covers the real path.
//!
//! `cargo run --release -p cosmo-daemon --example link_asr_tts --features speech,ears`

use std::time::Instant;

use cosmo_stt::hotwords::Hotwords;
use cosmo_stt::{Stt, SttConfig};
use cosmo_tts::{ProviderInit, Registry};

/// Kokoro's 24 kHz → capture's 16 kHz. A 3-tap low-pass, then linear
/// interpolation: crude, but a demo's audio, not capture's (PipeWire
/// resamples real input).
fn to_16k(data: &[f32], rate: u32) -> Vec<f32> {
    let smooth: Vec<f32> = (0..data.len())
        .map(|i| {
            let at = |j: isize| data[(i as isize + j).clamp(0, data.len() as isize - 1) as usize];
            0.25 * at(-1) + 0.5 * at(0) + 0.25 * at(1)
        })
        .collect();
    let step = f64::from(rate) / 16_000.0;
    let n = (data.len() as f64 / step) as usize;
    (0..n)
        .map(|k| {
            let x = k as f64 * step;
            let (i, frac) = (x as usize, (x.fract()) as f32);
            let next = smooth.get(i + 1).copied().unwrap_or(smooth[i]);
            smooth[i] * (1.0 - frac) + next * frac
        })
        .collect()
}

fn main() {
    let t = Instant::now();
    let stt =
        Stt::load(&SttConfig::defaults().expect("no cache dir")).unwrap_or_else(|e| panic!("{e}"));
    println!(
        "cosmo-stt (sherpa) loaded in {:.0}ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let tts = Registry::with_builtins()
        .create("kokoro", &ProviderInit::default())
        .expect("kokoro");
    println!(
        "kokoro (ort) loaded in {:.0}ms — both runtimes resident",
        t.elapsed().as_secs_f64() * 1e3
    );

    let mut hotwords = Hotwords::new();
    hotwords.add([
        "Firefox",
        "Kubernetes",
        "PipeWire",
        "Spotube",
        "Obsidian",
        "cosmo",
    ]);
    let hotwords = hotwords.for_app(None);

    let lines = [
        "Open Firefox and move it to workspace three.",
        "Show me the Kubernetes dashboard.",
        "Restart PipeWire, please.",
        "Launch Spotube.",
        "Open my notes in Obsidian.",
    ];
    for voice in ["af_heart", "bm_george"] {
        for line in lines {
            let t = Instant::now();
            let pcm = futures::executor::block_on(tts.synthesize(line, voice)).expect("synth");
            let tts_ms = t.elapsed().as_secs_f64() * 1e3;
            let audio = to_16k(&pcm.data, pcm.sample_rate);
            let run = |hotwords: &str| {
                let mut session = stt.session(hotwords).expect("session");
                session.push(&audio);
                let t = futures::executor::block_on(session.finish()).expect("finish");
                (t.text, t.latency.as_secs_f64() * 1e3)
            };
            let (plain, plain_ms) = run("");
            let (biased, biased_ms) = run(&hotwords);
            println!(
                "[{voice}] {line}\n   tts {tts_ms:.0}ms | plain {plain_ms:.0}ms {plain:?}\n   {:>15}| hotwords {biased_ms:.0}ms {biased:?}",
                ""
            );
        }
    }
}
