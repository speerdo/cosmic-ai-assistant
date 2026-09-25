//! Text → IPA through the system's `libespeak-ng`, loaded at run time.
//!
//! Why `dlopen` and not a linked crate (findings §5): the `-dev` package
//! is not needed, nothing is compiled from C, and eSpeak NG (GPL-3.0)
//! stays a separate system component rather than being linked into cosmo's
//! MIT binary. If the library is missing, the Kokoro provider fails to
//! construct with a message naming the package; nothing else is affected.
//!
//! eSpeak keeps global state and is not thread-safe, so the library is
//! loaded once and every call goes through one process-wide mutex.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

use crate::TtsError;

/// `AUDIO_OUTPUT_RETRIEVAL`: we never ask eSpeak to make sound.
const AUDIO_OUTPUT_RETRIEVAL: c_int = 1;
/// `espeakCHARS_UTF8`.
const CHARS_UTF8: c_int = 1;
/// `espeakPHONEMES_IPA | espeakPHONEMES_TIE`, with `^` as the tie in
/// bits 8–23. Ties mark multi-letter phonemes (`a^ɪ`, `d^ʒ`) so the Kokoro
/// mapping can tell a diphthong from two adjacent vowels.
const PHONEME_MODE: c_int = 0x02 | 0x80 | ((b'^' as c_int) << 8);

/// eSpeak's names for the two accents Kokoro's English voices speak. `en`
/// is eSpeak's British English; `en-gb` is *not* a voice name here and is
/// refused with error 2 (findings §5).
pub(crate) const VOICE_US: &str = "en-us";
pub(crate) const VOICE_GB: &str = "en";

const LIBRARY: &str = "libespeak-ng.so.1";

type InitializeFn = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type SetVoiceFn = unsafe extern "C" fn(*const c_char) -> c_int;
type TextToPhonemesFn = unsafe extern "C" fn(*mut *const c_void, c_int, c_int) -> *const c_char;

struct Espeak {
    set_voice: SetVoiceFn,
    text_to_phonemes: TextToPhonemesFn,
    /// The voice last set successfully, to skip redundant switches.
    current: Option<&'static str>,
    /// Keeps the symbols above valid; never unloaded.
    _lib: Library,
}

// Every access is serialized through `ESPEAK`'s mutex.
unsafe impl Send for Espeak {}

static ESPEAK: OnceLock<Result<Mutex<Espeak>, String>> = OnceLock::new();

fn load() -> Result<Mutex<Espeak>, String> {
    // SAFETY: loading a system library runs its initializers; libespeak-ng
    // has none with side effects beyond its own globals.
    let lib = unsafe { Library::new(LIBRARY) }.map_err(|e| {
        format!(
            "cannot load {LIBRARY} ({e}) — install the system package \
             (`libespeak-ng1` and `espeak-ng-data` on Debian/Pop!_OS, \
             `espeak-ng` on Fedora)"
        )
    })?;
    // SAFETY: the signatures match speak_lib.h for espeak-ng ≥ 1.49.
    let (initialize, set_voice, text_to_phonemes) = unsafe {
        let init = *lib
            .get::<InitializeFn>(b"espeak_Initialize\0")
            .map_err(|e| e.to_string())?;
        let set = *lib
            .get::<SetVoiceFn>(b"espeak_SetVoiceByName\0")
            .map_err(|e| e.to_string())?;
        let ttp = *lib
            .get::<TextToPhonemesFn>(b"espeak_TextToPhonemes\0")
            .map_err(|e| e.to_string())?;
        (init, set, ttp)
    };
    // SAFETY: null path = the library's compiled-in data directory.
    let rate = unsafe { initialize(AUDIO_OUTPUT_RETRIEVAL, 0, std::ptr::null(), 0) };
    if rate <= 0 {
        return Err(format!(
            "espeak_Initialize failed ({rate}) — is `espeak-ng-data` installed?"
        ));
    }
    Ok(Mutex::new(Espeak {
        set_voice,
        text_to_phonemes,
        current: None,
        _lib: lib,
    }))
}

/// Whether the library can be loaded — `doctor`'s probe.
pub(crate) fn available() -> Result<(), String> {
    ESPEAK
        .get_or_init(load)
        .as_ref()
        .map(|_| ())
        .map_err(Clone::clone)
}

/// Raw eSpeak IPA (with `^` ties) for `text` in `voice` — one of
/// [`VOICE_US`] / [`VOICE_GB`]. Punctuation is dropped by eSpeak; the
/// caller splits on it first (see `phonemes::phonemize`).
pub(crate) fn ipa(text: &str, voice: &'static str) -> Result<String, TtsError> {
    let espeak = ESPEAK
        .get_or_init(load)
        .as_ref()
        .map_err(|e| TtsError::Synthesis(e.clone()))?;
    let mut espeak = espeak.lock().unwrap_or_else(|p| p.into_inner());

    if espeak.current != Some(voice) {
        let name = CString::new(voice).expect("static voice names have no NUL");
        // SAFETY: valid NUL-terminated string; serialized by the mutex.
        let err = unsafe { (espeak.set_voice)(name.as_ptr()) };
        if err != 0 {
            // A refused voice silently keeps the previous one, which would
            // speak British text with American phonemes. Fail instead.
            return Err(TtsError::Synthesis(format!(
                "eSpeak refused voice {voice:?} (error {err})"
            )));
        }
        espeak.current = Some(voice);
    }

    let text = CString::new(text.replace('\0', " ")).expect("NULs replaced");
    let mut cursor: *const c_void = text.as_ptr().cast();
    let mut out = String::new();
    // One clause per call; eSpeak advances `cursor` and nulls it at the end.
    while !cursor.is_null() {
        // SAFETY: `cursor` points into `text`, which outlives the loop; the
        // returned buffer is eSpeak's, valid until the next call, so it is
        // copied out immediately.
        let clause = unsafe { (espeak.text_to_phonemes)(&mut cursor, CHARS_UTF8, PHONEME_MODE) };
        if clause.is_null() {
            break;
        }
        let clause = unsafe { CStr::from_ptr(clause) }.to_string_lossy();
        let clause = clause.trim();
        if !clause.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(clause);
        }
    }
    Ok(out)
}
