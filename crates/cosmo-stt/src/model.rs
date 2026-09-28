//! Finding a transducer's files in a model directory, and building the two
//! recognizers from them.
//!
//! Every model cosmo fetches (`scripts/fetch-models --asr-bench`) is a NeMo
//! transducer with the same layout: `encoder`, `decoder` and `joiner` ONNX
//! files (int8 preferred) and `tokens.txt`. A directory is the unit of
//! choice, so switching models is naming a different directory.

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineTransducerModelConfig,
};

use crate::SttError;

/// Where `scripts/fetch-models --asr` unpacks models: `$XDG_CACHE_HOME` or
/// `~/.cache`, then `cosmo/models/asr`.
pub fn default_asr_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("cosmo/models/asr"))
}

/// The default streaming (partials) model, by directory name.
pub const DEFAULT_STREAMING: &str =
    "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25";
/// The default offline (commit) model, by directory name.
pub const DEFAULT_OFFLINE: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";

/// A transducer's files.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    pub dir: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

impl ModelFiles {
    pub fn find(dir: &Path) -> Result<Self, SttError> {
        if !dir.is_dir() {
            return Err(SttError::Missing(dir.to_owned()));
        }
        let pick = |stem: &str| {
            [format!("{stem}.int8.onnx"), format!("{stem}.onnx")]
                .into_iter()
                .map(|f| dir.join(f))
                .find(|p| p.is_file())
                .ok_or_else(|| SttError::Missing(dir.join(format!("{stem}.int8.onnx"))))
        };
        let tokens = dir.join("tokens.txt");
        if !tokens.is_file() {
            return Err(SttError::Missing(tokens));
        }
        Ok(Self {
            dir: dir.to_owned(),
            encoder: pick("encoder")?,
            decoder: pick("decoder")?,
            joiner: pick("joiner")?,
            tokens,
        })
    }

    /// The directory's name: what `doctor` and the config call the model.
    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `bpe.vocab` beside the model, derived from `tokens.txt` the first
    /// time it's needed (phase-3 findings §1f). `None` if it can't be
    /// written: the model still works, only unbiased.
    fn bpe_vocab(&self) -> Option<PathBuf> {
        let path = self.dir.join("bpe.vocab");
        if path.is_file() {
            return Some(path);
        }
        let write = || -> std::io::Result<()> {
            let tokens = std::fs::read_to_string(&self.tokens)?;
            let tmp = self.dir.join("bpe.vocab.part");
            std::fs::write(&tmp, crate::bpe::vocab_from_tokens(&tokens))?;
            std::fs::rename(&tmp, &path)
        };
        match write() {
            Ok(()) => Some(path),
            Err(e) => {
                tracing::warn!(dir = %self.dir.display(), "no bpe.vocab, hotwords off: {e}");
                None
            }
        }
    }

    fn s(p: &Path) -> Option<String> {
        Some(p.to_string_lossy().into_owned())
    }

    /// The streaming recognizer: greedy, no hotwords (nobody waits on a
    /// partial, and the commit is where biasing matters).
    pub(crate) fn online(&self, threads: i32) -> Result<OnlineRecognizer, SttError> {
        let config = OnlineRecognizerConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: Self::s(&self.encoder),
                    decoder: Self::s(&self.decoder),
                    joiner: Self::s(&self.joiner),
                },
                tokens: Self::s(&self.tokens),
                num_threads: threads,
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            // Endpointing is the segmenter's and the key's job, not sherpa's.
            enable_endpoint: false,
            ..Default::default()
        };
        OnlineRecognizer::create(&config).ok_or_else(|| SttError::Load(self.dir.clone()))
    }

    /// The offline recognizer: modified beam search, so hotwords apply.
    pub(crate) fn offline(
        &self,
        threads: i32,
        hotwords_score: f32,
    ) -> Result<(OfflineRecognizer, bool), SttError> {
        let vocab = self.bpe_vocab();
        let biased = vocab.is_some();
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Self::s(&self.encoder),
                    decoder: Self::s(&self.decoder),
                    joiner: Self::s(&self.joiner),
                },
                tokens: Self::s(&self.tokens),
                num_threads: threads,
                model_type: Some("nemo_transducer".into()),
                modeling_unit: biased.then(|| "bpe".into()),
                bpe_vocab: vocab.as_deref().and_then(Self::s),
                ..Default::default()
            },
            decoding_method: Some("modified_beam_search".into()),
            hotwords_score,
            ..Default::default()
        };
        let rec =
            OfflineRecognizer::create(&config).ok_or_else(|| SttError::Load(self.dir.clone()))?;
        Ok((rec, biased))
    }
}
