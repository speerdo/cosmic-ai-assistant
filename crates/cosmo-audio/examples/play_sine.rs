//! §2.3 DoD: `cargo run -p cosmo-audio --example play_sine --features pipewire-backend`
//!
//! Plays, in order, and prints when each one resolves:
//!
//! 1. a 1.5s sine sweep, 220 → 1760 Hz, at 48 kHz;
//! 2. two short beeps queued back to back at 24 kHz — a **rate switch**
//!    (the stream is rebuilt once the sweep drains) and a **gapless
//!    hand-off** between clips;
//! 3. after a pause long enough for the stream to go idle, one more beep —
//!    proving an idle (deactivated) stream wakes up again;
//! 4. a long tone cut off by `stop()` after 300ms → `Cancelled`.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use cosmo_audio::{Clip, Player, SETTLE};

fn sweep(rate: u32, secs: f32, from_hz: f32, to_hz: f32) -> Clip {
    let n = (rate as f32 * secs) as usize;
    let mut phase = 0.0f32;
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            // Exponential sweep sounds even across octaves.
            let hz = from_hz * (to_hz / from_hz).powf(t);
            phase = (phase + TAU * hz / rate as f32) % TAU;
            0.3 * phase.sin() * envelope(i, n, rate)
        })
        .collect();
    Clip::new(rate, samples).expect("non-zero rate")
}

/// 10ms fade in/out so clip edges don't click.
fn envelope(i: usize, n: usize, rate: u32) -> f32 {
    let ramp = (rate as usize / 100).max(1);
    let edge = i.min(n.saturating_sub(1 + i));
    (edge as f32 / ramp as f32).min(1.0)
}

fn main() {
    let player = Player::start().expect("PipeWire reachable");
    let gate = player.gate();
    let t0 = Instant::now();
    let at = || format!("{:>6.0}ms", t0.elapsed().as_secs_f64() * 1e3);

    println!("{} sweep 220→1760 Hz @ 48 kHz", at());
    let a = player.play(&sweep(48_000, 1.5, 220.0, 1760.0));
    println!("{} beeps @ 24 kHz (rate switch, gapless pair)", at());
    let b = player.play(&sweep(24_000, 0.25, 660.0, 660.0));
    let c = player.play(&sweep(24_000, 0.25, 880.0, 880.0));
    println!("{} speaking = {}", at(), gate.is_speaking());

    // Clips queued back to back resolve together, when the run drains.
    let a = a.wait();
    println!("{} sweep → {a:?}", at());
    let (b, c) = (b.wait(), c.wait());
    println!("{} beeps → {b:?}, {c:?}", at());
    println!(
        "{} speaking = {}, mic open now = {}, after settle = {}",
        at(),
        gate.is_speaking(),
        gate.mic_open(Instant::now(), SETTLE),
        gate.mic_open(Instant::now() + SETTLE, SETTLE),
    );

    std::thread::sleep(Duration::from_millis(800));
    println!("{} beep after idle", at());
    let idle = player.play(&sweep(24_000, 0.3, 440.0, 440.0)).wait();
    println!("{} → {idle:?}", at());

    println!("{} long tone, stopped after 300ms", at());
    let long = player.play(&sweep(24_000, 5.0, 330.0, 330.0));
    std::thread::sleep(Duration::from_millis(300));
    player.stop();
    let long = long.wait();
    println!("{} → {long:?}", at());
}
