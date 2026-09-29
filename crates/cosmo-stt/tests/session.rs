//! Spec §3.5 end to end, with the default model pair: segments are cut and
//! queued while audio is still arriving, the commit joins them, hotwords
//! reach the offline pass, partials stream, and silence trips the backstop.
//!
//! Needs `scripts/fetch-models --vad --asr-bench` (the speech is the 110M
//! transducer's two test clips).

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use cosmo_stt::{Event, Stt, SttConfig};

const RATE: usize = 16_000;

/// One recording streams at a time (a new session supersedes the last),
/// so these tests take turns on the shared models.
fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn stt() -> &'static Stt {
    static STT: OnceLock<Stt> = OnceLock::new();
    STT.get_or_init(|| {
        let config = SttConfig::defaults().expect("no cache dir");
        Stt::load(&config).unwrap_or_else(|e| panic!("{e}"))
    })
}

fn clip(name: &str) -> Vec<f32> {
    let path = cosmo_stt::model::default_asr_dir()
        .unwrap()
        .join("sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8/test_wavs")
        .join(name);
    hound::WavReader::open(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e} (run scripts/fetch-models --asr-bench)",
                path.display()
            )
        })
        .into_samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32768.0)
        .collect()
}

/// About −50 dBFS of deterministic noise: a quiet room.
fn room(seconds: f32) -> Vec<f32> {
    let mut seed = 11u32;
    (0..(seconds * RATE as f32) as usize)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 0.006 - 0.003
        })
        .collect()
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

/// "I love you." — a pause — the 7.4 s sentence.
fn speech_pause_speech() -> Vec<f32> {
    let mut audio = room(0.3);
    audio.extend(clip("en-english.wav"));
    audio.extend(room(1.0));
    audio.extend(clip("0.wav"));
    audio
}

#[test]
fn segments_decode_during_the_recording_and_join_into_the_commit() {
    let _turn = exclusive();
    let audio = speech_pause_speech();
    let mut session = stt().session("Phebe").unwrap();
    let mut cut_while_recording = 0;
    for chunk in audio.chunks(1024) {
        for e in session.push(chunk) {
            if let Event::Segment { .. } = e {
                cut_while_recording += 1;
            }
        }
    }
    let t = block_on(session.finish()).unwrap();
    println!("{t:#?}");

    assert!(
        cut_while_recording >= 1,
        "the pause after \"I love you\" must be cut before the release"
    );
    assert_eq!(
        t.segments.len(),
        cut_while_recording + 1,
        "every cut segment plus the tail is decoded"
    );
    let text = t.text.to_lowercase();
    assert!(text.starts_with("i love you"), "{:?}", t.text);
    assert!(text.contains("old portrait"), "{:?}", t.text);
    // Unbiased, the default model writes "Phoebe", correctly. Biasing it
    // toward a *misspelling* proves the hotwords reach the offline pass
    // whatever the model gets right on its own (findings §6e).
    assert!(
        t.text.contains("Phebe"),
        "hotword not applied: {:?}",
        t.text
    );
}

#[test]
fn partials_stream_while_audio_arrives() {
    let _turn = exclusive();
    let mut session = stt().session("").unwrap();
    let mut partials = Vec::new();
    for chunk in clip("0.wav").chunks(1024) {
        partials.extend(session.push(chunk).into_iter().filter_map(|e| match e {
            Event::Partial(p) => Some(p),
            _ => None,
        }));
    }
    // The streaming thread may still be catching up: poll, don't finish.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !partials.last().is_some_and(|p| p.contains("portrait")) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        partials.extend(session.push(&[]).into_iter().filter_map(|e| match e {
            Event::Partial(p) => Some(p),
            _ => None,
        }));
    }
    assert!(
        partials.len() >= 3,
        "partials should grow as words arrive: {partials:?}"
    );
    assert!(
        partials.windows(2).all(|w| w[0] != w[1]),
        "a partial is sent only when the text changes"
    );
    let _ = block_on(session.finish()).unwrap();
}

#[test]
fn silence_trips_the_backstop_once_and_commits_nothing() {
    let _turn = exclusive();
    let mut session = stt().session("").unwrap();
    let mut backstops = 0;
    for chunk in room(8.0).chunks(1024) {
        backstops += session
            .push(chunk)
            .iter()
            .filter(|e| **e == Event::Backstop)
            .count();
    }
    let t = block_on(session.finish()).unwrap();
    assert_eq!(backstops, 1);
    assert!(
        t.segments.is_empty(),
        "silence is never sent to the offline model"
    );
    assert_eq!(t.text, "");
}
