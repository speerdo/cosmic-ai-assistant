//! Spec §3.3's DoD: on a speech–silence–speech buffer, Silero plus the
//! segmenter cut only inside silence, once per real pause, and the
//! backstop ends a recording that goes quiet.
//!
//! The speech is the two test clips shipped with the 110M transducer
//! (`scripts/fetch-models --asr`), with room-level noise in the gaps so
//! "silence" isn't digital zero. Needs `scripts/fetch-models --vad` too.

use std::path::PathBuf;

use cosmo_audio::{SegmentConfig, SegmentEvent, Segmenter};
use cosmo_stt::vad::{Vad, WINDOW, default_model_path};

const RATE: usize = 16_000;

fn asr_test_wav(name: &str) -> Vec<f32> {
    let dir = default_model_path()
        .expect("no cache dir")
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("asr/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8/test_wavs");
    let path: PathBuf = dir.join(name);
    let reader = hound::WavReader::open(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run scripts/fetch-models --asr)", path.display()));
    assert_eq!(reader.spec().sample_rate as usize, RATE);
    reader
        .into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

/// Deterministic low-level noise (about -50 dBFS): a quiet room, not zeros.
fn room(seconds: f32, seed: &mut u32) -> Vec<f32> {
    (0..(seconds * RATE as f32) as usize)
        .map(|_| {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (*seed >> 8) as f32 / (1u32 << 24) as f32 * 0.006 - 0.003
        })
        .collect()
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Grow `gap` outwards, 10 ms at a time, while the audio stays at room
/// level.
fn quiet_extent(buffer: &[f32], gap: std::ops::Range<usize>) -> std::ops::Range<usize> {
    const STEP: usize = 160;
    let (mut start, mut end) = (gap.start, gap.end);
    while start >= STEP && rms(&buffer[start - STEP..start]) < 0.01 {
        start -= STEP;
    }
    while end + STEP <= buffer.len() && rms(&buffer[end..end + STEP]) < 0.01 {
        end += STEP;
    }
    start..end
}

#[test]
fn cuts_land_in_silence_and_the_backstop_fires() {
    let model = default_model_path().unwrap();
    let mut vad = Vad::new(&model).unwrap_or_else(|e| panic!("{e}"));

    let short = asr_test_wav("en-english.wav"); // "I love you." ~1 s
    let long = asr_test_wav("0.wav"); // 7.4 s read sentence

    let mut seed = 7;
    let mut buffer = room(0.5, &mut seed);
    let mut gaps = Vec::new();
    let mut push_gap = |buffer: &mut Vec<f32>, secs: f32, seed: &mut u32| {
        let start = buffer.len();
        buffer.extend(room(secs, seed));
        gaps.push(start..buffer.len());
    };
    buffer.extend(&short);
    push_gap(&mut buffer, 1.0, &mut seed);
    buffer.extend(&long);
    push_gap(&mut buffer, 0.8, &mut seed);
    buffer.extend(&short);
    // The tail: long enough for the backstop (6 s) with room to spare.
    let tail = buffer.len();
    buffer.extend(room(7.0, &mut seed));

    // Feed in capture-sized chunks (1024 samples, one PipeWire quantum at
    // 16 kHz), not windows, to exercise the partial-window carry.
    let config = SegmentConfig::default();
    let mut seg = Segmenter::new(config, 0);
    let mut events = Vec::new();
    for chunk in buffer.chunks(1024) {
        vad.feed(chunk, |speech| {
            let at = seg.position();
            if let Some(e) = seg.push(WINDOW, speech) {
                events.push((at, e));
            }
        });
    }
    println!("events: {events:?}");

    let cuts: Vec<u64> = events
        .iter()
        .filter_map(|&(_, e)| match e {
            SegmentEvent::Cut(p) => Some(p),
            SegmentEvent::Backstop => None,
        })
        .collect();

    // Every cut sits in quiet audio: the 80 ms around it is at room level,
    // whether it's an inserted gap or a pause the reader made.
    for &c in &cuts {
        let c = c as usize;
        let around = rms(&buffer[c - 640..c + 640]);
        assert!(around < 0.01, "cut at {c} is in audio at RMS {around:.4}");
    }
    // Each inserted pause got exactly one cut, and so did the tail. A
    // pause is the whole quiet stretch around the inserted gap: the clips
    // carry their own leading and trailing silence.
    for gap in gaps.iter().chain([&(tail..buffer.len())]) {
        let pause = quiet_extent(&buffer, gap.clone());
        let n = cuts
            .iter()
            .filter(|&&c| pause.contains(&(c as usize)))
            .count();
        assert_eq!(n, 1, "pause {pause:?} has {n} cuts; cuts {cuts:?}");
    }

    let backstops: Vec<u64> = events
        .iter()
        .filter(|(_, e)| *e == SegmentEvent::Backstop)
        .map(|&(at, _)| at)
        .collect();
    let [at] = backstops[..] else {
        panic!("expected one backstop, got {backstops:?}");
    };
    // Measured from where the room actually went quiet, to the end of the
    // window that fired it.
    let quiet = quiet_extent(&buffer, tail..buffer.len()).start;
    let after = (at as usize + WINDOW - quiet) as f32 / RATE as f32;
    println!("backstop {after:.2}s after the audio went quiet");
    assert!(
        (6.0..6.5).contains(&after),
        "backstop {after:.2}s into the quiet, want 6 s plus the VAD's hangover"
    );
}
