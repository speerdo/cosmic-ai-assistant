//! Synthesize with Kokoro and write a WAV, printing the §2.5 numbers.
//!
//! `cargo run --release -p cosmo-tts --example kokoro_say --features kokoro -- \
//!     [--variant fp32|fp16|q8] [--voice af_heart] [--out out.wav] "text"`
//!
//! `-- --list` prints the voices grouped by accent instead (the data
//! `cosmo voice list`, spec §2.7, will render).
//!
//! Play the result with `cosmo-audio`'s `play_wav` example.

use std::time::Instant;

use cosmo_tts::{ProviderInit, Registry};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut variant, mut voice, mut out, mut text) = (None, "default".to_owned(), None, None);
    let mut list = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--variant" => variant = args.next(),
            "--voice" => voice = args.next().expect("--voice needs a value"),
            "--out" => out = args.next(),
            "--list" => list = true,
            _ => text = Some(a),
        }
    }
    let text = text.unwrap_or_else(|| "Hello. I'm Cosmo, speaking locally with Kokoro.".into());

    let t = Instant::now();
    let provider = Registry::with_builtins()
        .create(
            "kokoro",
            &ProviderInit {
                model: variant.clone(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let load = t.elapsed();

    if list {
        let mut voices = provider.list_voices();
        voices.sort_by(|a, b| (a.accent.as_str(), &a.id).cmp(&(b.accent.as_str(), &b.id)));
        let mut accent = "";
        for v in &voices {
            if v.accent.as_str() != accent {
                accent = v.accent.as_str();
                println!("{accent}");
            }
            println!("  {:<12} {:<9} {:?}", v.id, v.label, v.gender);
        }
        return;
    }

    let rt = futures::executor::block_on(async {
        let mut runs = Vec::new();
        let mut pcm = None;
        // First call is cold (graph warm-up); report it and two warm ones.
        for _ in 0..3 {
            let t = Instant::now();
            let p = provider
                .synthesize(&text, &voice)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            runs.push(t.elapsed());
            pcm = Some(p);
        }
        (runs, pcm.unwrap())
    });
    let (runs, pcm) = rt;
    let audio = pcm.duration();
    println!(
        "variant={} voice={voice} load={:.0}ms audio={:.2}s cold={:.0}ms warm={:.0}ms/{:.0}ms rtf={:.3}",
        variant.as_deref().unwrap_or("fp32"),
        load.as_secs_f64() * 1e3,
        audio.as_secs_f64(),
        runs[0].as_secs_f64() * 1e3,
        runs[1].as_secs_f64() * 1e3,
        runs[2].as_secs_f64() * 1e3,
        runs[2].as_secs_f64() / audio.as_secs_f64(),
    );
    if let Some(out) = out {
        std::fs::write(&out, pcm.to_wav_bytes().expect("encodable")).expect("writable");
        println!("wrote {out}");
    }
}
