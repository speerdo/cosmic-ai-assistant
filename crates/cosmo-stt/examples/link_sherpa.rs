//! Link spike (phase-3 spec §3.1), part one: sherpa-onnx alone.
//! Loads an int8 transducer and transcribes WAVs, greedy and with hotwords.
//!
//! `cargo run --release -p cosmo-stt --example link_sherpa --features sherpa -- <model-dir> <wav>...`

use std::time::Instant;

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    Wave,
};

fn recognizer(dir: &str, method: &str) -> OfflineRecognizer {
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
            // Hotwords are words; sherpa tokenizes them with this vocab.
            // NeMo archives ship none, so it is derived from tokens.txt
            // (SentencePiece BPE: merge priority = id order).
            modeling_unit: Some("bpe".into()),
            bpe_vocab: std::env::var("BPE_VOCAB").ok(),
            ..Default::default()
        },
        decoding_method: Some(method.into()),
        hotwords_score: 1.5,
        ..Default::default()
    };
    OfflineRecognizer::create(&config).expect("recognizer")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("model dir");
    let wavs: Vec<String> = args.collect();

    for method in ["greedy_search", "modified_beam_search"] {
        let t = Instant::now();
        let rec = recognizer(&dir, method);
        println!("[{method}] load {:.0}ms", t.elapsed().as_secs_f64() * 1e3);
        for wav in &wavs {
            let w = Wave::read(wav).expect("wav");
            let secs = w.samples().len() as f64 / f64::from(w.sample_rate());
            let stream = if method == "greedy_search" {
                rec.create_stream()
            } else {
                rec.create_stream_with_hotwords("firefox\npipewire\nkubernetes")
            };
            let t = Instant::now();
            stream.accept_waveform(w.sample_rate(), w.samples());
            rec.decode(&stream);
            let text = stream.get_result().map(|r| r.text).unwrap_or_default();
            let dt = t.elapsed().as_secs_f64();
            println!(
                "  {wav}: {secs:.2}s audio, {:.0}ms, RTF {:.3}\n    {text:?}",
                dt * 1e3,
                dt / secs
            );
        }
    }
}
