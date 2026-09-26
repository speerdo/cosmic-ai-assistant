//! Capture through the pre-roll ring (phase-3 spec §3.2 DoD).
//!
//! `cargo run -p cosmo-audio --example record --features pipewire-backend -- out.wav [secs]`
//!     records `secs` (default 3) from the ring and writes a 16 kHz WAV.
//! `… -- --ungated out.wav [secs]` records with a fresh gate that playback
//!     never closes, so audio from the speakers is captured (acoustic
//!     loop tests).
//! `… --example record … -- --gate-test`
//!     records 4 s while playing a 1 s tone at t = 1 s, then reports where
//!     the ring went silent: expected from the tone's start to its end
//!     + 350 ms settle — cosmo's own audio never reaches the buffer.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use cosmo_audio::{CAPTURE_RATE, Capture, Clip, Player};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let ungated = args.first().map(String::as_str) == Some("--ungated");
    if ungated {
        args.remove(0);
    }
    let player = Player::start().expect("PipeWire reachable");
    let gate = if ungated {
        cosmo_audio::SpeechGate::new()
    } else {
        player.gate()
    };
    let capture = Capture::start(gate, 30).expect("capture");
    std::thread::sleep(Duration::from_millis(500)); // let the stream settle
    let start = capture.ring().now();
    let t0 = Instant::now();

    if args.first().map(String::as_str) == Some("--gate-test") {
        std::thread::sleep(Duration::from_secs(1));
        let rate = 24_000;
        let tone: Vec<f32> = (0..rate)
            .map(|i| 0.2 * (TAU * 440.0 * i as f32 / rate as f32).sin())
            .collect();
        let played_at = t0.elapsed();
        let done = player.play(&Clip::new(rate, tone).unwrap());
        let _ = done.wait();
        let drained_at = t0.elapsed();
        std::thread::sleep(Duration::from_secs(4).saturating_sub(t0.elapsed()));
        let (_, samples) = capture.ring().read(start, capture.ring().now());
        println!(
            "tone played {:.2}s → drained {:.2}s; expect silence until {:.2}s",
            played_at.as_secs_f64(),
            drained_at.as_secs_f64(),
            drained_at.as_secs_f64() + 0.35
        );
        // Runs of exact zeros ≥ 50 ms: what the gate wrote.
        let min = CAPTURE_RATE as usize / 20;
        let mut i = 0;
        while i < samples.len() {
            if samples[i] == 0.0 {
                let j = samples[i..]
                    .iter()
                    .position(|&s| s != 0.0)
                    .map_or(samples.len(), |k| i + k);
                if j - i >= min {
                    println!("  silent {:.2}s – {:.2}s", i as f64 / 16e3, j as f64 / 16e3);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    } else {
        let out = args.first().cloned().unwrap_or_else(|| "record.wav".into());
        let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
        println!("recording {secs}s… speak now");
        std::thread::sleep(Duration::from_secs(secs));
        let (_, samples) = capture.ring().read(start, capture.ring().now());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: CAPTURE_RATE,
            // 16-bit PCM: what every reader (sherpa's `Wave` included) takes.
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&out, spec).expect("writable");
        for s in &samples {
            w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .unwrap();
        }
        w.finalize().unwrap();
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!(
            "wrote {out}: {:.2}s, peak {peak:.3}",
            samples.len() as f64 / 16e3
        );
    }
    println!("{:?}", capture.stats());
}
