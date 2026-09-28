//! Push WAVs through a recording session as if they were being spoken
//! (spec §3.5): capture-sized chunks at real-time pace, partials as they
//! change, each segment cut, then the commit and release → commit latency.
//!
//! `cargo run --release -p cosmo-stt --example transcribe --features sherpa -- [--fast] [--hotwords "A,B"] <wav>...`
//!
//! `--fast` feeds audio as fast as it's read, instead of in real time.
//! `--apps` adds every installed application's name as a hotword.
//! `COSMO_ASR_STREAMING` / `COSMO_ASR_OFFLINE` name other model
//! directories under the ASR cache; `none` turns that model off.

use std::time::{Duration, Instant};

use cosmo_stt::hotwords::Hotwords;
use cosmo_stt::{Event, Stt, SttConfig};

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let (mut fast, mut hotwords) = (false, Hotwords::new());
    let mut wavs = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--fast" => fast = true,
            "--apps" => {
                let apps = cosmo_stt::hotwords::desktop_apps();
                let before = hotwords.len();
                hotwords.add(apps.iter().map(|a| a.name.as_str()));
                println!(
                    "{} installed apps, {} usable as hotwords",
                    apps.len(),
                    hotwords.len() - before
                );
            }
            "--hotwords" => {
                hotwords.add(args.next().expect("--hotwords A,B").split(','));
            }
            _ => wavs.push(a),
        }
    }

    let mut config = SttConfig::defaults().expect("no cache dir");
    let asr = cosmo_stt::model::default_asr_dir().unwrap();
    for (var, slot) in [
        ("COSMO_ASR_STREAMING", &mut config.streaming),
        ("COSMO_ASR_OFFLINE", &mut config.offline),
    ] {
        if let Ok(name) = std::env::var(var) {
            *slot = (name != "none").then(|| asr.join(name));
        }
    }
    let t = Instant::now();
    let stt = Stt::load(&config).unwrap_or_else(|e| panic!("{e}"));
    println!("resident in {:.2}s", t.elapsed().as_secs_f64());
    for m in [&stt.streaming_model, &stt.offline_model]
        .into_iter()
        .flatten()
    {
        println!(
            "loaded {} in {:.2}s{}",
            m.name,
            m.load.as_secs_f64(),
            if m.hotwords { " (hotwords)" } else { "" }
        );
    }
    let hotwords = hotwords.for_app(None);

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for path in wavs {
        let reader = hound::WavReader::open(&path).expect("wav");
        assert_eq!(reader.spec().sample_rate, 16_000, "{path}: need 16 kHz");
        let samples: Vec<f32> = reader
            .into_samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.0)
            .collect();
        println!("\n{path} ({:.2}s)", samples.len() as f64 / 16e3);

        let mut session = stt.session(&hotwords).unwrap();
        let start = Instant::now();
        let mut fed = 0usize;
        for chunk in samples.chunks(1024) {
            fed += chunk.len();
            if !fast {
                let due = Duration::from_secs_f64(fed as f64 / 16e3);
                std::thread::sleep(due.saturating_sub(start.elapsed()));
            }
            for e in session.push(chunk) {
                let at = start.elapsed().as_secs_f64();
                match e {
                    Event::Partial(t) => println!("  {at:5.2}s partial  {t}"),
                    Event::Segment { audio } => {
                        println!("  {at:5.2}s segment  {:.2}s cut", audio.as_secs_f64())
                    }
                    Event::Backstop => println!("  {at:5.2}s backstop"),
                }
            }
        }
        let t = rt.block_on(session.finish()).expect("finish");
        for (i, d) in t.segments.iter().enumerate() {
            println!(
                "  seg {i}: {:.2}s audio, {:.0}ms decode  {:?}",
                d.audio.as_secs_f64(),
                d.decode.as_secs_f64() * 1e3,
                d.text
            );
        }
        println!("  streaming: {:?}", t.streaming);
        println!("  COMMIT: {:?}", t.text);
        println!("  release → commit: {:.0}ms", t.latency.as_secs_f64() * 1e3);
    }
}
