//! The Kokoro-82M provider (spec §2.5): local synthesis on `ort`, phonemes
//! from the system's eSpeak NG. The default voice layer once its model
//! files are fetched (`scripts/fetch-models`).
//!
//! Inference is CPU-bound and `ort::Session::run` takes `&mut self`, so one
//! dedicated worker thread owns the session and serves requests in order;
//! [`VoiceProvider::synthesize`] hands it the text and awaits a oneshot.
//! That keeps the daemon's async runtime free while the model runs, needs
//! no particular runtime, and serializes eSpeak (which is not thread-safe)
//! for free.

// The one FFI surface in this crate: every `unsafe` in it is a call into
// libespeak-ng through symbols resolved at run time (see its module docs).
#[allow(unsafe_code)]
mod espeak;
mod phonemes;
mod voices;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use futures::channel::oneshot;
use futures::future::BoxFuture;
use ort::session::Session;
use ort::value::Tensor;
use tracing::Instrument;

use crate::TtsError;
use crate::pcm::Pcm;
use crate::provider::{LatencyClass, Voice, VoiceProvider};
use crate::registry::ProviderInit;

use phonemes::Vocab;
use voices::Pack;

/// Kokoro's output rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// Model variants `scripts/fetch-models` knows, by the name `voice_model`
/// takes. `fp32` is the default: on CPU it measured faster than both
/// reduced-precision exports (findings §5).
const VARIANTS: &[(&str, &str)] = &[
    ("fp32", "model.onnx"),
    ("fp16", "model_fp16.onnx"),
    ("q8", "model_quantized.onnx"),
];
const DEFAULT_VARIANT: &str = "fp32";

/// Where `scripts/fetch-models` puts the model: `$XDG_CACHE_HOME` or
/// `~/.cache`, then `cosmo/models/kokoro-v1.0`.
pub fn default_model_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("cosmo/models/kokoro-v1.0"))
}

/// Registry constructor under the name `"kokoro"`.
pub fn factory(init: &ProviderInit) -> Result<Box<dyn VoiceProvider>, TtsError> {
    Ok(Box::new(KokoroTts::new(init)?))
}

pub struct KokoroTts {
    voices: Vec<Voice>,
    requests: mpsc::Sender<Request>,
}

struct Request {
    text: String,
    voice: String,
    reply: oneshot::Sender<Result<Pcm, TtsError>>,
}

impl KokoroTts {
    /// Load the model and start the worker. Fails — with the fix in the
    /// message — when the model files or eSpeak are missing, so `doctor`
    /// and the daemon's startup can say exactly what to do.
    pub fn new(init: &ProviderInit) -> Result<Self, TtsError> {
        let dir = init
            .model_dir
            .clone()
            .or_else(default_model_dir)
            .ok_or_else(|| TtsError::Synthesis("no model directory (HOME unset)".into()))?;
        let variant = match init.model.as_deref() {
            None | Some("") => DEFAULT_VARIANT,
            Some(v) => v,
        };
        let file = VARIANTS
            .iter()
            .find(|(name, _)| *name == variant)
            .map(|(_, file)| *file)
            .ok_or_else(|| {
                TtsError::Synthesis(format!(
                    "unknown Kokoro model variant {variant:?} (fp32, fp16, q8)"
                ))
            })?;
        let model_path = dir.join(file);
        if !model_path.exists() {
            return Err(missing(&model_path));
        }
        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer =
            std::fs::read_to_string(&tokenizer_path).map_err(|_| missing(&tokenizer_path))?;
        let vocab = Vocab::from_tokenizer_json(&tokenizer)?;
        let voice_dir = dir.join("voices");
        let voices = voices::discover(&voice_dir);
        if voices.is_empty() {
            return Err(missing(&voice_dir));
        }
        espeak::available().map_err(TtsError::Synthesis)?;

        let (ready_tx, ready_rx) = mpsc::channel();
        let (requests, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("cosmo-kokoro".into())
            .spawn(move || {
                let started = Instant::now();
                match Worker::load(&model_path, vocab, voice_dir) {
                    Ok(mut worker) => {
                        tracing::info!(
                            model = %model_path.display(),
                            load_ms = started.elapsed().as_millis() as u64,
                            "kokoro model loaded"
                        );
                        let _ = ready_tx.send(Ok(()));
                        worker.serve(rx);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| TtsError::Synthesis(format!("spawning the Kokoro worker: {e}")))?;
        ready_rx
            .recv()
            .map_err(|_| TtsError::Synthesis("Kokoro worker died while loading".into()))??;

        Ok(Self { voices, requests })
    }
}

fn missing(path: &Path) -> TtsError {
    TtsError::Synthesis(format!(
        "Kokoro model files missing ({}) — run scripts/fetch-models",
        path.display()
    ))
}

impl VoiceProvider for KokoroTts {
    fn id(&self) -> &str {
        "kokoro"
    }

    fn list_voices(&self) -> Vec<Voice> {
        self.voices.clone()
    }

    fn synthesize(&self, text: &str, voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
        let span = tracing::info_span!(
            "speak/synthesize",
            provider = "kokoro",
            voice = %voice,
            text_bytes = text.len(),
        );
        let voice = if voice == "default" {
            voices::DEFAULT_VOICE
        } else {
            voice
        };
        if !self.voices.iter().any(|v| v.id == voice) {
            let err = TtsError::UnknownVoice {
                provider: "kokoro".into(),
                voice: voice.to_owned(),
            };
            return Box::pin(async move { Err(err) });
        }
        let (reply, rx) = oneshot::channel();
        let sent = self.requests.send(Request {
            text: text.to_owned(),
            voice: voice.to_owned(),
            reply,
        });
        Box::pin(
            async move {
                sent.map_err(|_| TtsError::Synthesis("Kokoro worker is gone".into()))?;
                rx.await
                    .map_err(|_| TtsError::Synthesis("Kokoro worker is gone".into()))?
            }
            .instrument(span),
        )
    }

    fn is_local(&self) -> bool {
        true
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Fast
    }
}

/// Owns the session; lives on the worker thread.
struct Worker {
    session: Session,
    vocab: Vocab,
    voice_dir: PathBuf,
    packs: HashMap<String, Arc<Pack>>,
    /// The token input's name: `input_ids` in the onnx-community export,
    /// `tokens` in others. Read from the model rather than assumed.
    tokens_input: String,
}

impl Worker {
    fn load(model: &Path, vocab: Vocab, voice_dir: PathBuf) -> Result<Self, TtsError> {
        let ort_err = |e: ort::Error| TtsError::Synthesis(format!("onnxruntime: {e}"));
        let session = Session::builder()
            .map_err(ort_err)?
            .commit_from_file(model)
            .map_err(ort_err)?;
        let names: Vec<&str> = session.inputs().iter().map(|i| i.name()).collect();
        let tokens_input = ["input_ids", "tokens"]
            .into_iter()
            .find(|n| names.contains(n))
            .ok_or_else(|| {
                TtsError::Synthesis(format!("unexpected Kokoro model inputs: {names:?}"))
            })?
            .to_owned();
        for required in ["style", "speed"] {
            if !names.contains(&required) {
                return Err(TtsError::Synthesis(format!(
                    "Kokoro model has no `{required}` input (inputs: {names:?})"
                )));
            }
        }
        Ok(Self {
            session,
            vocab,
            voice_dir,
            packs: HashMap::new(),
            tokens_input,
        })
    }

    fn serve(&mut self, requests: mpsc::Receiver<Request>) {
        for req in requests {
            let result = self.synthesize(&req.text, &req.voice);
            let _ = req.reply.send(result);
        }
    }

    fn pack(&mut self, voice: &str) -> Result<Arc<Pack>, TtsError> {
        if let Some(pack) = self.packs.get(voice) {
            return Ok(Arc::clone(pack));
        }
        let path = self.voice_dir.join(format!("{voice}.bin"));
        let bytes = std::fs::read(&path).map_err(|_| missing(&path))?;
        let pack = Arc::new(Pack::from_bytes(voice, &bytes)?);
        self.packs.insert(voice.to_owned(), Arc::clone(&pack));
        Ok(pack)
    }

    fn synthesize(&mut self, text: &str, voice: &str) -> Result<Pcm, TtsError> {
        let pack = self.pack(voice)?;
        let started = Instant::now();
        let phonemes = phonemes::phonemize(text, voices::is_british(voice))?;
        let g2p_ms = started.elapsed().as_secs_f64() * 1e3;
        let tokens = self.vocab.encode(&phonemes);
        let mut audio = Vec::new();
        for chunk in phonemes::chunk_tokens(&tokens, self.vocab.space()) {
            audio.extend(self.infer(chunk, pack.style(chunk.len()))?);
        }
        tracing::debug!(
            g2p_ms,
            total_ms = started.elapsed().as_secs_f64() * 1e3,
            tokens = tokens.len(),
            audio_s = audio.len() as f64 / f64::from(SAMPLE_RATE),
            "kokoro synthesized"
        );
        Ok(Pcm::new(SAMPLE_RATE, audio))
    }

    fn infer(&mut self, tokens: &[i64], style: &[f32]) -> Result<Vec<f32>, TtsError> {
        let ort_err = |e: ort::Error| TtsError::Synthesis(format!("onnxruntime: {e}"));
        let pad = self.vocab.pad();
        let mut ids = Vec::with_capacity(tokens.len() + 2);
        ids.push(pad);
        ids.extend_from_slice(tokens);
        ids.push(pad);
        let ids = Tensor::from_array(([1usize, ids.len()], ids)).map_err(ort_err)?;
        let style =
            Tensor::from_array(([1usize, voices::STYLE_DIM], style.to_vec())).map_err(ort_err)?;
        let speed = Tensor::from_array(([1usize], vec![1.0f32])).map_err(ort_err)?;
        let outputs = self
            .session
            .run(ort::inputs![
                self.tokens_input.as_str() => ids,
                "style" => style,
                "speed" => speed,
            ])
            .map_err(ort_err)?;
        let (_, samples) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        Ok(samples.to_vec())
    }
}
