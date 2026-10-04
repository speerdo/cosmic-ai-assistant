//! The local models cosmo runs on, and whether they're on disk (phase-8
//! §8.2). Fetched by `scripts/fetch-models` (installed as
//! `/usr/libexec/cosmo/fetch-models`), never packaged. `cosmo models` and
//! `cosmo doctor` read this; the engines resolve their own files from the
//! same directories.

use std::path::{Path, PathBuf};

/// The default streaming (partials) speech-recognition model, by directory.
pub const ASR_STREAMING: &str =
    "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25";
/// The default offline (commit) speech-recognition model, by directory.
pub const ASR_OFFLINE: &str = "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming";

/// `$XDG_CACHE_HOME` or `~/.cache`, then `cosmo/models`.
pub fn root() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("cosmo/models"))
}

/// One model the default install needs.
#[derive(Debug, Clone)]
pub struct Model {
    /// Short name, as `cosmo models` prints it.
    pub name: &'static str,
    /// What it's for.
    pub role: &'static str,
    /// The file or directory whose presence means it's fetched.
    pub path: PathBuf,
    pub present: bool,
}

/// The default set under `root`: what `cosmo models fetch` gets.
pub fn default_set(root: &Path) -> Vec<Model> {
    let asr = root.join("asr");
    // An ASR model is in place once its verified checksum is written beside
    // it (fetch-models writes it last).
    [
        (
            "kokoro-82m",
            "speech (the voice)",
            root.join("kokoro-v1.0/model.onnx"),
        ),
        (
            "silero-vad",
            "speech detection",
            root.join("vad/silero_vad.onnx"),
        ),
        (
            "nemotron-streaming-0.6b",
            "live transcript",
            asr.join(ASR_STREAMING).join(".sha256"),
        ),
        (
            "parakeet-unified-0.6b",
            "final transcript, wake word",
            asr.join(ASR_OFFLINE).join(".sha256"),
        ),
    ]
    .into_iter()
    .map(|(name, role, path)| Model {
        name,
        role,
        present: path.is_file(),
        path,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_follows_the_marker_files() {
        let root = std::env::temp_dir().join(format!("cosmo-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(default_set(&root).iter().all(|m| !m.present));
        std::fs::create_dir_all(root.join("vad")).unwrap();
        std::fs::write(root.join("vad/silero_vad.onnx"), b"x").unwrap();
        // A half-unpacked ASR model (no checksum marker) isn't present.
        std::fs::create_dir_all(root.join("asr").join(ASR_OFFLINE)).unwrap();
        let set = default_set(&root);
        let present: Vec<_> = set.iter().filter(|m| m.present).map(|m| m.name).collect();
        assert_eq!(present, ["silero-vad"]);
    }
}
