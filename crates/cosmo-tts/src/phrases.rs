//! The phrase cache (spec §2.6): the reflex vocabulary rendered to WAV
//! once per voice, so an ack is a buffer push instead of a synthesis.
//!
//! Layout: `<root>/<provider>/<voice>/<key>.<hash>.wav`, root defaulting to
//! `~/.cache/cosmo/voice`. The hash covers everything that changes the
//! audio: provider, voice, model variant, the phrase text, and a format
//! version. So **staleness is a lookup miss**: edit a phrase, switch the
//! model, bump the format, and the old file simply no longer matches. No
//! manifest can disagree with the directory, because there is none.
//!
//! Every file is written as `…wav.tmp` and renamed into place. A render
//! killed midway leaves finished phrases intact and the rest missing (plus
//! perhaps one `.tmp`), and the next [`PhraseCache::render`] fills exactly
//! the gaps. Superseded files are swept once a render completes.
//!
//! The vocabulary is a **list of [`Phrase`]s, not an enum**. Phase 4 owns
//! the final set and passes a longer list; nothing here changes.

use std::path::{Path, PathBuf};

use crate::TtsError;
use crate::pcm::Pcm;
use crate::provider::VoiceProvider;

/// Bumped when the cache's on-disk form changes, invalidating every entry.
const FORMAT: u32 = 1;

/// One cached utterance: a stable key the playback site asks for, and the
/// words it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phrase {
    pub key: String,
    pub text: String,
}

impl Phrase {
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
        }
    }
}

/// The phase-2 reflex vocabulary (plan §Phase 2). Phase 4 replaces this
/// list with its matcher's; the keys are what its dispatch asks for.
pub fn default_phrases() -> Vec<Phrase> {
    [
        ("ack-moving", "Moving it."),
        ("ack-focused", "Done."),
        ("ack-launching", "Opening it."),
        ("err-notfound", "I couldn't find that."),
        ("confirm-hold", "That one needs your confirmation."),
    ]
    .into_iter()
    .map(|(k, t)| Phrase::new(k, t))
    .collect()
}

/// Which voice a cache directory belongs to. `model` is the provider's
/// model variant as configured (empty = provider default).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VoiceKey {
    pub provider: String,
    pub voice: String,
    pub model: String,
}

/// Progress of one render, reported after each phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderReport {
    /// Phrases synthesized by this render.
    pub rendered: usize,
    /// Phrases already cached and valid.
    pub reused: usize,
    /// Stale or partial files removed.
    pub swept: usize,
}

#[derive(Debug, Clone)]
pub struct PhraseCache {
    root: PathBuf,
}

impl PhraseCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `$XDG_CACHE_HOME/cosmo/voice`, else `~/.cache/cosmo/voice`.
    pub fn default_root() -> Option<PathBuf> {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        Some(cache.join("cosmo/voice"))
    }

    /// The directory for one voice. Components are sanitized so a voice id
    /// can never climb out of the cache root.
    pub fn dir(&self, voice: &VoiceKey) -> PathBuf {
        self.root
            .join(sanitize(&voice.provider))
            .join(sanitize(&voice.voice))
    }

    /// Where `phrase` lives for `voice` — whether or not it exists yet.
    pub fn path(&self, voice: &VoiceKey, phrase: &Phrase) -> PathBuf {
        self.dir(voice).join(format!(
            "{}.{:016x}.wav",
            sanitize(&phrase.key),
            content_hash(voice, phrase)
        ))
    }

    /// The cached audio, if present and current.
    pub fn load(&self, voice: &VoiceKey, phrase: &Phrase) -> Option<Pcm> {
        let bytes = std::fs::read(self.path(voice, phrase)).ok()?;
        Pcm::from_wav_bytes(&bytes).ok()
    }

    /// Phrases with no current file.
    pub fn missing<'p>(&self, voice: &VoiceKey, phrases: &'p [Phrase]) -> Vec<&'p Phrase> {
        phrases
            .iter()
            .filter(|p| !self.path(voice, p).is_file())
            .collect()
    }

    /// Synthesize whichever of `phrases` are missing, and touch nothing
    /// else in the directory. For one-off entries (a voice preview) whose
    /// render must not sweep the voice's full vocabulary.
    pub async fn ensure(
        &self,
        provider: &dyn VoiceProvider,
        voice: &VoiceKey,
        phrases: &[Phrase],
    ) -> Result<(), TtsError> {
        let dir = self.dir(voice);
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        for phrase in self.missing(voice, phrases) {
            let pcm = provider.synthesize(&phrase.text, &voice.voice).await?;
            write_atomically(&self.path(voice, phrase), &pcm.to_wav_bytes()?)?;
        }
        Ok(())
    }

    /// Bring `voice`'s directory up to date with `phrases`: synthesize what
    /// is missing, keep what is current, then sweep everything else
    /// (superseded hashes, removed phrases, interrupted `.tmp` files).
    ///
    /// `progress` fires after every phrase, reused ones included, so a
    /// consumer sees `total` steps whatever the cache state was.
    /// Synthesis stops at the first error; files written before it are kept
    /// (they are valid), and nothing is swept.
    pub async fn render(
        &self,
        provider: &dyn VoiceProvider,
        voice: &VoiceKey,
        phrases: &[Phrase],
        mut progress: impl FnMut(Progress),
    ) -> Result<RenderReport, TtsError> {
        let dir = self.dir(voice);
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let total = phrases.len();
        let (mut rendered, mut reused) = (0, 0);
        for (i, phrase) in phrases.iter().enumerate() {
            let path = self.path(voice, phrase);
            if path.is_file() {
                reused += 1;
            } else {
                let pcm = provider.synthesize(&phrase.text, &voice.voice).await?;
                write_atomically(&path, &pcm.to_wav_bytes()?)?;
                rendered += 1;
            }
            progress(Progress { done: i + 1, total });
        }
        let swept = self.sweep(voice, phrases);
        Ok(RenderReport {
            rendered,
            reused,
            swept,
        })
    }

    /// Remove every file in `voice`'s directory that is not a current
    /// phrase. Returns how many went.
    fn sweep(&self, voice: &VoiceKey, phrases: &[Phrase]) -> usize {
        let keep: Vec<PathBuf> = phrases.iter().map(|p| self.path(voice, p)).collect();
        let Ok(entries) = std::fs::read_dir(self.dir(voice)) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && !keep.contains(p))
            .filter(|p| std::fs::remove_file(p).is_ok())
            .count()
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), TtsError> {
    let tmp = path.with_extension("wav.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| io_err(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_err(path, e))
}

fn io_err(path: &Path, e: std::io::Error) -> TtsError {
    TtsError::Synthesis(format!("phrase cache {}: {e}", path.display()))
}

/// Keep `[A-Za-z0-9._-]`, map anything else to `_`, and never yield a
/// component that means "this" or "parent" directory.
fn sanitize(component: &str) -> String {
    let s: String = component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match s.as_str() {
        "" | "." | ".." => format!("_{s}"),
        _ => s,
    }
}

/// FNV-1a over the fields that shape the audio. Stable across Rust
/// releases, unlike `DefaultHasher`, so a toolchain bump does not
/// invalidate every cache. Fields are length-prefixed so `("ab","c")` and
/// `("a","bc")` differ.
fn content_hash(voice: &VoiceKey, phrase: &Phrase) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&FORMAT.to_le_bytes());
    eat(voice.provider.as_bytes());
    eat(voice.voice.as_bytes());
    eat(voice.model.as_bytes());
    eat(phrase.text.as_bytes());
    h
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::executor::block_on;
    use futures::future::BoxFuture;

    use super::*;
    use crate::provider::{LatencyClass, Voice};

    /// One sample per character; fails on the text "boom".
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        seen: Mutex<Vec<String>>,
    }

    impl VoiceProvider for Counting {
        fn id(&self) -> &str {
            "fake"
        }
        fn list_voices(&self) -> Vec<Voice> {
            Vec::new()
        }
        fn synthesize(&self, text: &str, _voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(text.to_owned());
            let out = if text == "boom" {
                Err(TtsError::Synthesis("boom".into()))
            } else {
                Ok(Pcm::new(16_000, vec![0.25; text.len()]))
            };
            Box::pin(async move { out })
        }
        fn is_local(&self) -> bool {
            true
        }
        fn latency_class(&self) -> LatencyClass {
            LatencyClass::Fast
        }
    }

    fn tmp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cosmo-phrases-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn voice(v: &str) -> VoiceKey {
        VoiceKey {
            provider: "fake".into(),
            voice: v.into(),
            model: String::new(),
        }
    }

    #[test]
    fn renders_everything_then_reuses_it() {
        let cache = PhraseCache::new(tmp_root("reuse"));
        let p = Counting::default();
        let phrases = default_phrases();
        let mut steps = Vec::new();
        let report = block_on(cache.render(&p, &voice("a"), &phrases, |s| steps.push(s))).unwrap();
        assert_eq!(report.rendered, phrases.len());
        assert_eq!(steps.last(), Some(&Progress { done: 5, total: 5 }));
        assert!(cache.missing(&voice("a"), &phrases).is_empty());
        let loaded = cache.load(&voice("a"), &phrases[0]).unwrap();
        assert_eq!(loaded.data.len(), phrases[0].text.len());

        let again = block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).unwrap();
        assert_eq!((again.rendered, again.reused, again.swept), (0, 5, 0));
        assert_eq!(p.calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_changed_phrase_is_stale_and_its_old_file_is_swept() {
        let cache = PhraseCache::new(tmp_root("stale"));
        let p = Counting::default();
        let mut phrases = default_phrases();
        block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).unwrap();

        phrases[1].text = "Focused.".into();
        assert_eq!(cache.missing(&voice("a"), &phrases).len(), 1);
        let report = block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).unwrap();
        assert_eq!((report.rendered, report.reused, report.swept), (1, 4, 1));
        assert_eq!(p.seen.lock().unwrap().last().unwrap(), "Focused.");
        let files = std::fs::read_dir(cache.dir(&voice("a"))).unwrap().count();
        assert_eq!(files, phrases.len());
    }

    #[test]
    fn voice_and_model_are_part_of_identity() {
        let cache = PhraseCache::new(tmp_root("identity"));
        let phrase = Phrase::new("k", "text");
        let a = voice("a");
        let mut a_q8 = voice("a");
        a_q8.model = "q8".into();
        assert_ne!(cache.path(&a, &phrase), cache.path(&voice("b"), &phrase));
        assert_ne!(cache.path(&a, &phrase), cache.path(&a_q8, &phrase));
    }

    /// A render killed midway: some files done, a `.tmp` left behind. The
    /// next render fills only the gaps and clears the debris.
    #[test]
    fn an_interrupted_render_is_repaired() {
        let cache = PhraseCache::new(tmp_root("repair"));
        let p = Counting::default();
        let phrases = default_phrases();
        block_on(cache.render(&p, &voice("a"), &phrases[..2], |_| {})).unwrap();
        // The phrase being written when the process died…
        let debris = cache
            .path(&voice("a"), &phrases[2])
            .with_extension("wav.tmp");
        std::fs::write(&debris, b"half a wav").unwrap();
        // …and an orphan from a render of an older phrase list.
        let orphan = cache
            .dir(&voice("a"))
            .join("ack-old.0123456789abcdef.wav.tmp");
        std::fs::write(&orphan, b"older debris").unwrap();

        let report = block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).unwrap();
        // The half-written file is rewritten through the same tmp name and
        // renamed into place; the orphan is swept.
        assert_eq!((report.rendered, report.reused, report.swept), (3, 2, 1));
        assert!(!debris.exists() && !orphan.exists());
        assert!(cache.load(&voice("a"), &phrases[2]).is_some());
        assert!(cache.missing(&voice("a"), &phrases).is_empty());
    }

    #[test]
    fn a_failed_render_keeps_what_it_finished_and_sweeps_nothing() {
        let cache = PhraseCache::new(tmp_root("fail"));
        let p = Counting::default();
        let phrases = vec![
            Phrase::new("one", "fine"),
            Phrase::new("two", "boom"),
            Phrase::new("three", "never"),
        ];
        assert!(block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).is_err());
        assert_eq!(cache.missing(&voice("a"), &phrases).len(), 2);
        assert!(cache.load(&voice("a"), &phrases[0]).is_some());
    }

    #[test]
    fn ensure_adds_without_sweeping() {
        let cache = PhraseCache::new(tmp_root("ensure"));
        let p = Counting::default();
        let phrases = default_phrases();
        block_on(cache.render(&p, &voice("a"), &phrases, |_| {})).unwrap();
        let preview = Phrase::new("preview", "This is how I sound.");
        block_on(cache.ensure(&p, &voice("a"), std::slice::from_ref(&preview))).unwrap();
        block_on(cache.ensure(&p, &voice("a"), std::slice::from_ref(&preview))).unwrap();
        assert_eq!(p.calls.load(Ordering::SeqCst), phrases.len() + 1);
        assert!(
            cache.missing(&voice("a"), &phrases).is_empty(),
            "vocabulary kept"
        );
        assert!(cache.load(&voice("a"), &preview).is_some());
    }

    #[test]
    fn hostile_ids_stay_inside_the_root() {
        let root = tmp_root("hostile");
        let cache = PhraseCache::new(&root);
        let evil = VoiceKey {
            provider: "..".into(),
            voice: "../../etc".into(),
            model: String::new(),
        };
        let path = cache.path(&evil, &Phrase::new("../x", "t"));
        assert!(path.starts_with(&root), "{}", path.display());
        assert!(!path.components().any(|c| c.as_os_str() == ".."));
    }
}
