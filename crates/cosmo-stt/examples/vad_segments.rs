//! Print where Silero hears speech in a 16 kHz WAV, and where the
//! segmenter would cut it (spec §3.3). For checking the pause rules on
//! your own recordings.
//!
//! `cargo run --release -p cosmo-stt --example vad_segments --features sherpa -- <wav>...`

use cosmo_audio::{SegmentConfig, SegmentEvent, Segmenter};
use cosmo_stt::vad::{Vad, WINDOW, default_model_path};

fn main() {
    let model = default_model_path().expect("no cache dir");
    for path in std::env::args().skip(1) {
        let mut vad = Vad::new(&model).unwrap_or_else(|e| panic!("{e}"));
        let reader = hound::WavReader::open(&path).expect("wav");
        assert_eq!(reader.spec().sample_rate, 16_000, "{path}: need 16 kHz");
        let samples: Vec<f32> = reader
            .into_samples::<i16>()
            .map(|s| f32::from(s.unwrap()) / 32768.0)
            .collect();

        let secs = |p: u64| p as f64 / 16_000.0;
        let mut seg = Segmenter::new(SegmentConfig::default(), 0);
        let mut timeline = String::new();
        let mut events = Vec::new();
        let mut last_speech = None;
        vad.feed(&samples, |speech| {
            if speech {
                last_speech = Some(seg.position() + WINDOW as u64);
            }
            // One character per 4 windows (128 ms) keeps a line readable.
            if (seg.position() / WINDOW as u64).is_multiple_of(4) {
                timeline.push(if speech { '#' } else { '.' });
            }
            if let Some(e) = seg.push(WINDOW, speech) {
                events.push(e);
            }
        });
        println!("{path} ({:.2}s)\n  {timeline}", secs(samples.len() as u64));
        if let Some(end) = last_speech {
            // bench-asr clips end 300 ms after the key release.
            println!(
                "  speech heard until {:.2}s, {:.0} ms before the clip ends",
                secs(end),
                (samples.len() as f64 - end as f64) / 16.0
            );
        }
        for e in events {
            match e {
                SegmentEvent::Cut(p) => println!("  cut at {:.2}s", secs(p)),
                SegmentEvent::Backstop => println!("  backstop"),
            }
        }
    }
}
