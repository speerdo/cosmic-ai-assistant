//! Link spike (phase-3 spec §3.1), part two: **both ONNX Runtimes in one
//! process.** Kokoro (on `ort`'s static runtime) speaks command-like lines;
//! sherpa-onnx (with its own static runtime) transcribes each, greedy and
//! with hotwords. Calls alternate between the two runtimes.
//!
//! `BPE_VOCAB=… cargo run --release -p cosmo-daemon --example link_asr_tts \
//!     --features speech,ears -- <asr-model-dir>`

use std::time::Instant;

use cosmo_stt::sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
};
use cosmo_tts::{ProviderInit, Registry};

const HOTWORDS: &str = "firefox\nkubernetes\npipewire\nspotube\nobsidian\ncosmo";

fn main() {
    let dir = std::env::args().nth(1).expect("asr model dir");
    let f = |name: &str| Some(format!("{dir}/{name}"));
    let config = OfflineRecognizerConfig {
        model_config: OfflineModelConfig {
            transducer: OfflineTransducerModelConfig {
                encoder: f("encoder.int8.onnx"),
                decoder: f("decoder.int8.onnx"),
                joiner: f("joiner.int8.onnx"),
            },
            tokens: f("tokens.txt"),
            num_threads: 4,
            model_type: Some("nemo_transducer".into()),
            modeling_unit: Some("bpe".into()),
            bpe_vocab: std::env::var("BPE_VOCAB").ok(),
            ..Default::default()
        },
        decoding_method: Some("modified_beam_search".into()),
        hotwords_score: 2.0,
        ..Default::default()
    };
    let t = Instant::now();
    let asr = OfflineRecognizer::create(&config).expect("sherpa recognizer");
    println!(
        "sherpa-onnx recognizer loaded in {:.0}ms",
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
            let run = |hotwords: bool| {
                let stream = if hotwords {
                    asr.create_stream_with_hotwords(HOTWORDS)
                } else {
                    asr.create_stream()
                };
                let t = Instant::now();
                stream.accept_waveform(pcm.sample_rate as i32, &pcm.data);
                asr.decode(&stream);
                let text = stream.get_result().map(|r| r.text).unwrap_or_default();
                (text, t.elapsed().as_secs_f64() * 1e3)
            };
            let (plain, plain_ms) = run(false);
            let (biased, biased_ms) = run(true);
            println!(
                "[{voice}] {line}\n   tts {tts_ms:.0}ms | plain {plain_ms:.0}ms {plain:?}\n   {:>15}| hotwords {biased_ms:.0}ms {biased:?}",
                ""
            );
        }
    }
}
