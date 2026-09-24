//! Play a WAV file through the playback stream at the file's own rate.
//!
//! `cargo run -p cosmo-audio --example play_wav --features pipewire-backend -- file.wav`
//!
//! Decoding here is deliberately minimal (hound, collapse to mono); the
//! real decoder is `cosmo_tts::Pcm::from_wav_bytes`, which the daemon uses.

use std::time::Instant;

use cosmo_audio::{Clip, Player};

fn main() {
    let path = std::env::args().nth(1).expect("usage: play_wav <file.wav>");
    let mut reader = hound::WavReader::open(&path).expect("readable WAV");
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
    };
    let channels = usize::from(spec.channels.max(1));
    let mono: Vec<f32> = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    let clip = Clip::new(spec.sample_rate, mono).expect("non-zero rate");
    println!(
        "{path}: {} Hz, {} ch, {:.2}s",
        spec.sample_rate,
        spec.channels,
        clip.duration().as_secs_f64()
    );

    let player = Player::start().expect("PipeWire reachable");
    let t0 = Instant::now();
    let outcome = player.play(&clip).wait();
    println!("{outcome:?} after {:.2}s", t0.elapsed().as_secs_f64());
}
