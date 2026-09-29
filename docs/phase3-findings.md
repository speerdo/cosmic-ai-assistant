# Phase 3 findings

**Started:** 2026-09-26
**Scope so far:** §1, the link spike (spec §3.1): done. Its verdict changed
how the heavy tier builds.

## §1. Link spike (spec part 3.1, 2026-09-26)

**Verdict: sherpa-onnx links and runs well, but not beside Kokoro's `ort`
as shipped. Two static ONNX Runtimes in one binary fail to link. Fixed by
pointing both crates at *one* runtime (sherpa's), fetched once by a
checksummed script, and confirmed working in one process. One licensing
question is open for the user (§1e).**

### 1a. sherpa-onnx alone: fast, static, works first time

`sherpa-onnx` 1.13.8 with its default `static` feature downloads a prebuilt
bundle (23 MB archive) and links it statically: no `.so` to ship, and no
system package exercised (not `cmake`, not `clang`). The `sherpa` feature on
`cosmo-stt` gates it (the two-tier rule).

The 110M-parameter int8 NeMo transducer (`parakeet_tdt_transducer_110m`,
108 MB, SHA-256 checked) on 4 threads:

| Clip | Audio | Decode | RTF | Transcript |
|---|---|---|---|---|
| `test_wavs/0.wav` | 7.43 s | 103 ms | **0.014** | "Well, I don't wish to see it any more, observed Phoebe, turning away her eyes. It is certainly very like the old portrait." |
| `en-english.wav` | 0.99 s | 20 ms | 0.020 | "I love you." |

The model loads in about 0.5–0.6 s. The cold build takes 14 s, and the
example binary is 36 MB. For short commands, this small model is already
far under any latency budget. Whether its *accuracy* on commands is enough
is §3.6's question, and it's now a serious candidate, as blueprint §16
suspected.

### 1b. The two runtimes do not coexist, as shipped

A binary with both `ort` (Kokoro) and `sherpa-onnx` failed at link time:

    rust-lld: error: duplicate symbol: re2::ToStringWalker::PreVisit(…)
      >>> defined in libort_sys-….rlib
      >>> defined in libsherpa_onnx_sys-….rlib
    rust-lld: error: duplicate symbol: google::protobuf::internal::DestroyString(…)
    …

Both crates embed a full static ONNX Runtime: pyke's **1.28.0** build for
`ort`, and sherpa's own **1.28.2** build.

### 1c. The fix: one runtime, sherpa's, for both

`ort-sys` checks `ORT_LIB_PATH` before it downloads anything, and
`sherpa-onnx-sys` honours `SHERPA_ONNX_LIB_DIR`. So:

- **`scripts/fetch-native`** downloads sherpa's static bundle once (pinned
  version, SHA-256 checked) into `.native/` (gitignored).
- **`.cargo/config.toml`** points both variables there. `ort` links
  sherpa's `libonnxruntime.a`, which is 1.28.2 against `ort`'s expected
  1.28, so the C API version satisfies both.
- **Neither build script downloads anything any more.** A clean rebuild of
  both native crates after deleting every earlier download found only
  `.native/` on the link path, and took 9 s. Builds are now offline and
  deterministic once the script has run.

**Verified in one process** (`cosmo-daemon/examples/link_asr_tts`). Kokoro
spoke five command-like lines in two voices, and sherpa transcribed each
one, the two runtimes alternating calls. Both loaded side by side: sherpa
in 604 ms, Kokoro in 649 ms. The binary is 44.5 MB with no ONNX, sherpa or
eSpeak shared-library dependencies.

**Heavy-tier builds now need `scripts/fetch-native` first**, including
Kokoro-only builds, since `ort` links the same bundle. Without it, the
build fails naming the missing `.native/…` path. The core tier is
unaffected, because it never builds `ort` or sherpa. One gotcha: an
*empty* `ORT_LIB_PATH` doesn't fall back to the download; it panics in
`ort-sys`'s build script.

### 1d. Cost of sharing: Kokoro on sherpa's runtime, and the fix

Kokoro measured **~35% slower** on sherpa's runtime build (RTF ≈ 0.21
against 0.153). A thread sweep found the cause. Sherpa's build defaults to
an intra-op thread count that spills onto this hybrid CPU's E-cores (8
P-cores + 16 E-cores, one thread each). Five runs per setting:

| Runtime | Threads | RTF (5 runs) |
|---|---|---|
| sherpa 1.28.2 | default | 0.235 0.200 0.202 0.227 0.214 |
| sherpa 1.28.2 | 6 | 0.184 0.183 0.185 0.181 0.189 |
| sherpa 1.28.2 | **8** | **0.171 0.170 0.168 0.166 0.173** |
| sherpa 1.28.2 | 10 | 0.178 0.187 0.184 0.182 0.183 |
| pyke 1.28.0 | default | 0.159 0.165 0.163 0.157 0.159 |

Kokoro now sets `min(available_parallelism, 8)` explicitly, and re-measured
at **0.165–0.174**, within about 6% of what it was on its own runtime.
Phase-2 findings §5c's TTFA table therefore shifts by roughly that much;
a 1.9 s ack is still around 330 ms.

### 1e. Licensing: sherpa's bundle puts GPL eSpeak in the binary (**user decision**)

The prebuilt static bundle contains `libespeak-ng.a` and
`libpiper_phonemize.a`. Sherpa's C API is one translation unit covering ASR
and TTS alike, so an ASR-only binary still links them. **69 eSpeak NG
symbols** end up in the executable, statically, and eSpeak NG is
**GPL-3.0**. That undoes the separation phase-2 §5a chose deliberately
(Kokoro loads the *system* libespeak-ng with `dlopen`, so no GPL code is
linked).

- No runtime conflict: none of those symbols are exported dynamically
  (`nm -D` shows zero eSpeak or Ort symbols), so the `dlopen`ed system
  library still resolves to itself.
- What it affects is **distribution**. Cosmo's source stays MIT (which is
  GPL-compatible), but a binary *distributed* with the bundle linked in is
  a combined work under GPL-3.0 terms. Building and running from source is
  unaffected.

The options, for phase 8 at the latest:

1. **Accept it**, and say in the packaging docs that the binary is GPL-3.0
   as a whole.
2. **Build sherpa-onnx from source with TTS disabled**
   (`-DSHERPA_ONNX_ENABLE_TTS=OFF`) in `scripts/fetch-native`. This keeps one
   static runtime and drops eSpeak and piper from the binary, at the cost
   of a cmake build (several minutes, once).
3. **Link sherpa as a shared library** (`shared` feature, with `ort`
   `load-dynamic` against the same `.so`). eSpeak then lives in a shipped
   `.so` rather than the executable.

**Recommendation: (2) before anything is packaged.** It's the only option
that keeps the §5a property (no GPL code in cosmo's binary) without a
second runtime. Development continues on the prebuilt bundle meanwhile,
which is fine for building and running from source. This is flagged, not
decided.

### 1f. Hotwords on NeMo models: they need a BPE vocab, and they work

The first hotword attempt logged `Cannot find ID for token FIREFOX`:
sherpa looked each hotword up whole in `tokens.txt`. These NeMo models use
1,024 SentencePiece BPE pieces, and the archive ships no `bpe.vocab` for
sherpa to tokenize with. The likely reason blueprint §3.3 says
cosmic-voice "scaffolded [hotwords] and never wired it up".

The fix is to derive `bpe.vocab` from `tokens.txt` with **score = −id**
(SentencePiece BPE's merge priority follows id order), and set
`modeling_unit = "bpe"` plus `bpe_vocab`. The encode errors stop, and
biasing measurably works. Kokoro speech → sherpa, plain vs with hotwords:

| Spoken (voice) | Plain | Hotwords |
|---|---|---|
| "Launch Spotube." (bm_george) | "Launch **Spot Tuber**." | "Launch **spotube**." |
| "Restart PipeWire, please." (af_heart) | "Restart **pipe wire**, please." | "Restart **pipewire**, please." |
| "Show me the Kubernetes dashboard." | correct | correct |

What §3.5 and §3.6 need to settle:

- **Hotword casing is copied into the output.** Lowercase hotwords turned a
  correct "Open Firefox" into "Open firefox". Hotwords should be given in
  their display casing.
- "PipeWire" with `bm_george` stayed "pipe wire" even with the hotword.
  Scores and phrasing need tuning on real recordings.
- Beam search with hotwords costs roughly 1.3–2× the greedy decode. For
  commands that's tens of milliseconds.
- Feeding Kokoro's 24 kHz audio made sherpa log a resampler notice per
  stream. Capture will deliver 16 kHz, so this won't happen live.

### 1g. What changed in the tree

- `cosmo-stt`: `sherpa` feature. There's a temporary `pub use sherpa_onnx`
  for the spike, replaced by the crate's own API in §3.5. There's also the
  `link_sherpa` example.
- `cosmo-daemon`: an `ears` feature (`cosmo-stt/sherpa`) and the
  `link_asr_tts` example (needs `speech,ears`).
- `scripts/fetch-native`, `.cargo/config.toml`, and `/.native` in
  `.gitignore`.
- Kokoro sets its intra-op thread count explicitly (§1d).
- The ASR model for the spike lives under `~/.cache/cosmo/models/asr/`.
  §3.5 folds ASR models into `scripts/fetch-models` with pinned checksums.

## §2. Capture (spec part 3.2, 2026-09-26)

**DoD met.** The microphone streams into the pre-roll ring. The ring never
holds cosmo's own voice, and capture survives a machine loaded to twice its
core count.

### 2a. What was built

- **`Ring`** (`cosmo-audio/src/ring.rs`, core tier, 5 tests). The design is
  lifted from cosmic-voice: `AtomicU32` sample bits, absolute `u64`
  positions, a single RT writer and lock-free readers. A lapped reader
  skips the overwritten span rather than returning garbage.
  `mark_preroll(ms)` reaches back to where an utterance should start.
  `write_silence(n)` keeps positions continuous while the gate is closed.
- **`Capture`** (`capture.rs`, heavy tier). It's a native PipeWire capture
  stream with `RT_PROCESS`, mono F32LE at **16 kHz** (PipeWire resamples),
  and it's always on. Node `cosmo-mic`, role `Communication`, and
  `COSMO_SOURCE` pins a source. A stream that errors is rebuilt every 2 s.
  The RT callback converts through a stack window: no locks, no allocation.
- **Half-duplex at the ring** (invariant #8). While
  `SpeechGate::mic_open(now, SETTLE)` is false, the callback writes zeros
  instead of samples. It's the same gate playback drives (§2.3); there is
  no second notion of "speaking".
- **`CaptureStats`** counts callbacks, samples, gated samples, the **longest
  gap between callbacks**, reconnects, and whether the stream is streaming.
  These back `doctor` and the saturated-machine test.
- The `record` example has three modes: plain (writes a 16-bit WAV),
  `--ungated` (acoustic-loop tests), and `--gate-test`.

### 2b. Live verification

**Half-duplex gate.** A 1 s tone was played through `Player` at t = 1 s
while capturing:

    tone played 1.00s → drained 2.02s; expect silence until 2.37s
      silent 0.98s – 2.37s                  (room noise on either side)
    gated: 22186 samples (1.39 s)

**The acoustic loop.** A Kokoro sentence went out through the speaker,
through the air into the laptop mic (`--ungated`), into the ring and a WAV,
then to sherpa:

    greedy:    "Open Firebox and move it to workspace three."   70 ms
    hotwords:  "Open firefox and move it to work space three."   82 ms

The capture path carries intelligible speech end to end. Hotwords fixed a
real misrecognition on real acoustic audio, not just on synthesized input.
With the speaker right beside the mic, 266 of 96,256 samples clipped.
PipeWire delivers floats slightly above 1.0 (peak 1.018); the ring stores
them unclamped, and only the WAV writer clamps.

**The saturated-machine test** (spec §3.8, run early because capture is
where it's decided):

| Condition | Duration | Samples, expected → got | Max callback gap |
|---|---|---|---|
| idle | 10 s | ~167k → 166,885 | 21.7 ms |
| clean `--release` workspace build (load ~11) | 25 s | ~408k → 408,890 | 28.1 ms |
| **48 CPU hogs on 24 cores** (load → 23+, still rising) | 20 s | ~328k → 327,653 | 27.7 ms |

No samples were lost in any condition. The worst gap sits at one PipeWire
quantum (~21.3 ms at 1024/48k) plus about 6 ms. That's the RT data loop
doing what blueprint §3.2 said it would, and the reason capture is a native
client. The §3.8 box stays open until it has been repeated end to end with
a real utterance and a transcript.

### 2c. Open

- `doctor`'s capture line (stream up, max gap) lands with the daemon wiring
  (§3.7).

## §3. Hotkey (spec part 3.4, 2026-09-26)

**The trigger is Right Ctrl** (evdev 97), chosen by the user on
2026-09-26. The Launch has no physical F13. **And the blueprint's grab is
gone:** it was unsound.

### 3a. Why `EVIOCGRAB` came out (a blueprint correction)

Blueprint §3.1 and phase 0 grabbed the keyboard *while the trigger was
held*, "so the key never leaks to the focused application". Working
through it for a modifier trigger showed three problems:

1. **The press always leaks.** A grab can only be taken *after* this
   process reads the press, and by then libinput has read it too. Grabbing
   cannot un-send it.
2. **The release is swallowed.** While grabbed, the release goes only to
   the grabber, so the compositor believes the key is **still down**.
   Phase 0's F9 test didn't show this, because a stuck F9 does nothing
   visible. A stuck **Right Ctrl** would turn every later keystroke into a
   Ctrl-shortcut.
3. **A permanent grab isn't an option**: it takes the *whole* device, the
   whole keyboard.

cosmic-voice (the design's source) never grabbed at all. It relied on F13
being a key nothing uses.

**New rule:** no grab. `EVIOCSMASK` still restricts each descriptor to the
trigger code (and masks `EV_MSC` scancodes off), so the "not a keylogger"
property is unchanged and still kernel-enforced. The trigger must be a key
that is **inert on its own**. Its press and release flow to the desktop
normally, so nothing is ever stuck.

**Cost, and how it's contained.** Right Ctrl shortcuts (RCtrl+C and so on)
also press the trigger. Cosmo can't see the other key, and by design it
shouldn't. The mitigation is at the utterance layer (§3.7): **a hold
under 300 ms is discarded** as a tap or shortcut. A longer shortcut hold
opens the mic briefly, and silence transcribes to nothing, so the worst
case is a transcript that's empty. There is no action path from
transcripts in phase 3 at all.

### 3b. What was built (`cosmo-hotkey`)

- A `Watcher` thread that attaches to **every** `/dev/input/event*` whose
  key bitmap has the trigger, sets the two masks, and polls. **Hotplug is
  inotify** on `/dev/input` (`IN_CREATE`, `IN_ATTRIB`, `IN_DELETE` →
  rescan). `IN_ATTRIB` matters because logind's uaccess ACL lands just
  *after* a node appears. There's no libudev, so no `-dev` package.
- `TriggerState` (pure, 4 tests) combines edges across keyboards: held
  while any keyboard holds the key. Autorepeat (`value == 2`) is ignored,
  a double press or stray release is harmless, and **a device vanishing
  mid-hold counts as a release**, so an unplug can't leave cosmo listening.
- Every `unsafe` in the crate is an ioctl in `evdev.rs`.
- The config key is `trigger_key` (default 97), validated as a keyboard
  code (1–255; `BTN_*` codes refused).
- The `trigger` example prints edges with hold durations and flags holds
  under 300 ms.

### 3c. Live

With no root, no `input` group and no grab, it attached to five keyboard
nodes that advertise Right Ctrl:

    event3   AT Translated Set 2 keyboard
    event6   ITE Tech. Inc. ITE Device(8258) Keyboard     (laptop)
    event11  Logitech K400 Plus
    event14  System76 Launch Configurable Keyboard (launch_1)
    event17  System76 Launch Configurable Keyboard (launch_1) Keyboard

**Not yet verified live:** actual press and release edges from Right Ctrl.
That needs a human at the keyboard (`cargo run -p cosmo-hotkey --example
trigger`), and so does replug handling. Phase 0 already proved edge
delivery through the same masks on this hardware (F9, 1 press / 1 release,
0 non-trigger events).

### 3d. Deferred to §3.7

- The 300 ms minimum hold, `cosmo listen` (the press-only fallback, bound
  as a COSMIC `Spawn` shortcut), and `doctor`'s "trigger attached on N
  keyboards" line.
- `cosmo trigger capture` for rebinding without editing the config. Not
  needed now that the default is settled.

## §4. VAD and segmentation (spec part 3.3, 2026-09-28)

**Done.** Silero finds speech, a small rule engine decides where to cut,
and on a real speech–silence–speech buffer every cut lands in silence.

### 4a. The split: Silero decides per window, the segmenter owns the rules

- **`cosmo_stt::vad::Vad`** (feature `sherpa`) wraps sherpa's Silero
  detector and returns one speech/silence decision per 512-sample (32 ms)
  window. It takes whatever chunk sizes capture delivers and carries the
  partial window over. Only sherpa's frame-level `detected()` state is used;
  its queue of silence-stripped segments is cleared as it fills. The
  utterance is cut from the ring, where positions are absolute and the
  pre-roll lives.
- sherpa's own debounce is set short (100 ms either way) and its
  `max_speech_duration` far out of reach, since that setting forces an end
  *inside speech* by raising the threshold. What counts as a pause is
  decided in one place:
- **`cosmo_audio::Segmenter`** (core tier, 7 tests) turns the decisions
  into two events:
  - **`Cut(pos)`** once a silence after speech reaches **400 ms**, at a
    point 200 ms into it. It fires *during* the pause, so the finished
    segment can decode while the next is spoken (§3.5). One cut per pause.
    None for silence that follows no speech, so quiet isn't split into
    empty segments. **No forced cuts**: a long stretch without a pause
    decodes as one segment.
  - **`Backstop`** after **6 s** of continuous silence, once per
    recording, whether or not anything was said. It's generous on
    purpose: someone holding the key while they think mustn't be cut off.
    It exists for lost releases, not end-pointing.

Both durations are `SegmentConfig` defaults for now. Whether they become
config keys is a §3.7 question, once they've been tried with real speech.

### 4b. Verified (`cosmo-stt/tests/vad_cuts.rs`)

The buffer: 0.5 s room noise, "I love you." (1 s), 1.0 s noise, a 7.4 s
read sentence, 0.8 s noise, "I love you." again, then 7 s noise. The
noise sits at about −50 dBFS, so silence isn't digital zero. It's fed
through in 1024-sample chunks, the size PipeWire delivers.

| Event | Where | |
|---|---|---|
| cut | 1.64 s | inserted 1.0 s pause |
| cut | 5.80 s | the reader's own pause, mid-sentence |
| cut | 7.24 s | the reader's own pause |
| cut | 9.90 s | inserted 0.8 s pause |
| cut | 11.85 s | the tail |
| backstop | 6.13 s after the audio went quiet | 6 s + VAD hangover |

The test asserts the property rather than these positions: each cut has
room-level audio (RMS < 0.01) for 40 ms either side, and each inserted
pause gets exactly one. That was also a lesson: the clips carry their own
leading and trailing silence, so the pause the VAD hears starts *before*
the inserted gap. The test measures a pause as the whole quiet stretch
around it, which is also how a real recording behaves.

Cost: 18.5 s of audio through Silero in about 70 ms, one thread (RTF
~0.004). Negligible beside the recognizer.

### 4c. Model and fetch

`scripts/fetch-models --vad` fetches `silero_vad.onnx` (644 KB, MIT) into
`~/.cache/cosmo/models/vad/`, from sherpa-onnx's `asr-models` release,
pinned by SHA-256 because a release asset has no git revision.

The heavy test also reads the two clips shipped with the 110M transducer,
fetched by `scripts/fetch-models --asr-bench` since §3.5.

### 4d. What changed in the tree

- `cosmo-audio`: `segment.rs` (`Segmenter`, `SegmentConfig`,
  `SegmentEvent`).
- `cosmo-stt`: `vad.rs` (behind `sherpa`), the `vad_cuts` test, and the
  `vad_segments` example, which prints a speech timeline and the cuts for
  any 16 kHz WAV.
- `scripts/fetch-models --vad`.

## §5. Speech recognition (spec part 3.5, 2026-09-28)

**Done.** Hold-to-talk's recognition half works end to end on recorded
speech: live partials while audio arrives, and a committed transcript
**57–116 ms after release**. What's left is the daemon wiring (§3.7) and
choosing models on the user's own voice (§3.6).

### 5a. Models and fetching

`scripts/fetch-models` now takes components (`--kokoro`, `--vad`, `--asr`,
`--asr-bench`, combinable). The ASR models are sherpa-onnx's int8 NeMo
exports, as release archives. They're pinned by the SHA-256 digests GitHub
publishes for release assets, verified before unpacking, with the verified
hash kept beside the files so a re-run skips them.

| Set | Model | Archive | Role |
|---|---|---|---|
| `--asr` | `nemotron-speech-streaming-en-0.6b` (560 ms chunks) | 464 MB | streaming (default) |
| `--asr` | `parakeet-tdt-0.6b-v2` | 482 MB | offline (default) |
| `--asr-bench` | `parakeet_tdt_transducer_110m` | 108 MB | §3.6 candidate |
| `--asr-bench` | `parakeet-unified-en-0.6b` (non-streaming) | 501 MB | §3.6 candidate |

All four are transducers with the same layout, so one loader covers them,
and a model is chosen by naming its directory. The defaults are the
blueprint's pair, pending §3.6. The config keys are
`asr_streaming_model` / `asr_offline_model` (directory names; empty is the
default, `"none"` turns one off), `asr_threads` (default 2) and
`offline_threads` (default 4, validated 1–4).

### 5b. Shape (`cosmo-stt`)

- **`Stt`** loads both models, each on its **own thread**, in parallel:
  **1.77 s** until both are resident (1.76 s and 1.48 s individually).
  Peak RSS with both loaded is about **1.8 GB**, which §3.6 should weigh.
  `Stt` is cheap to clone. Dropping the last clone closes the channels and
  **joins** the threads (see §5e for why).
- **`Session`** is one recording. `push` never blocks on a model. Each
  chunk goes three ways: to the streaming thread (partials), through Silero
  and the segmenter (cuts, §4), and into the current segment's buffer. At a
  cut, the finished segment is queued on the offline thread immediately.
  `finish` queues the tail (only if speech was heard since the last cut)
  and awaits the segments in order. The pure pieces (`Hotwords`, the BPE
  vocab, `join`) are core tier; everything that loads a model is behind
  `sherpa`.
- **The commit never waits on the streaming model.** The first version of
  `finish` asked the streaming model to flush its final text, so a
  streaming backlog delayed the commit. Feeding the clip faster than real
  time showed it: **1,217 ms** from release to commit. Now, with an offline
  model present, the streaming recording is ended without a flush, and an
  **epoch counter** makes its queued audio stale, so it's skipped rather
  than decoded. `Transcript::streaming` is then the last partial seen.
  Only without an offline model does the commit wait for the streaming
  final, because then that *is* the commit.

Real-time pace (the `transcribe` example feeds 1024-sample chunks on the
clock, as capture would):

| Clip | Segments | Partials | Release → commit |
|---|---|---|---|
| 7.4 s read sentence | 3 (cut at 3.3 s and 4.7 s, decoded while "speaking") | every ~0.5 s | **116 ms** (was 194 ms before the fix above) |
| "I love you." (1 s) | 1 | — | **57 ms** |
| the same, fed as fast as possible | 3 / 1 | — | 330 ms / 72 ms (everything decodes after "release") |

Segment decodes on the 0.6B offline model take 60–180 ms each.

### 5c. Hotwords

- **Mechanism.** `Hotwords` holds a base set plus per-`app_id` sets;
  `for_app(focused)` gives sherpa's one-phrase-per-line form. Phrases are
  kept in display casing (§1f). Unusable phrases are dropped before they
  reach the tokenizer: symbols, non-Latin scripts, number-only phrases,
  more than 4 words.
- **App names.** `desktop_apps()` reads `Name=` from `.desktop` files in
  the XDG data dirs (the user's entries win; `NoDisplay`/`Hidden` and
  non-applications skipped). On this machine it finds **102 apps, and 98
  make usable hotwords**. With all 98 active, the test clips transcribe
  the same, at the same latency, with no tokenizer errors.
- **`bpe.vocab`** is derived from `tokens.txt` beside the model on first
  load (§1f). If it can't be written, the model runs unbiased and says so
  (`Loaded::hotwords`).
- **It works through a session.** Unbiased, the 0.6B model writes the
  name in the test sentence as "Phebe"; with "Phoebe" as a hotword it's
  right. The §3.1 Kokoro check (`link_asr_tts`, ported to this API and the
  default pair):

| Spoken (voice) | Plain | Hotwords |
|---|---|---|
| "Restart PipeWire, please." (af_heart) | "Restart **pipe wire** please." | "Restart **PipeWire** please." |
| "Restart PipeWire, please." (bm_george) | "Restart **pipe wire**, please." | "Restart **PipeWire**, please." |
| "Launch Spotube." (af_heart) | "Launch **Spotoob**." | "Launch **Spotube**." |
| "Open my notes in Obsidian." (bm_george) | "…in **obsidian**." | "…in **Obsidian**." |

  "PipeWire" with `bm_george`, which the 110M model got wrong even with
  the hotword (§1f), is right on the 0.6B model. Everything else was
  already correct unbiased. Hotwords cost 0–60 ms per utterance here.
- **Still open for phase 4:** mapping a Wayland `app_id` to a `.desktop`
  id isn't always an exact match (`firefox` vs `org.mozilla.firefox`).
  `DesktopApp` keeps the id for that. The reflex phrase list itself is
  phase 4's.

### 5d. A quirk of segmenting: punctuation at every cut

The offline model sees each segment alone, so it ends each with a full
stop:

    streaming: "…any more, observed Phoebe, turning away her eyes. It is…"
    commit:    "…any more, observed Phebe. turning away her eyes. It is…"

For commands, which are usually one segment, this rarely shows. For a
long utterance, the commit's punctuation is less trustworthy than its
words. Options for later: strip a segment's trailing punctuation when the
next segment starts lowercase, or cut less eagerly (a longer
`min_pause`). §3.6 should look at it on real recordings before anything is
tuned.

### 5e. Exit race: ONNX Runtime torn down under a decode

The first run of the ported `link_asr_tts` ended with:

    [E:onnxruntime … ExecuteKernel] Non-zero status code returned while running
    Reshape node. Name:'/layers.4/self_attn/Reshape_3' … GetElementType is not implemented

The node is from the speech models (Kokoro has no such layer). `main`
returned while the streaming thread was still decoding, and the process's
teardown pulled the runtime out from under it. Harmless at exit, but not
something to leave. Dropping the last `Stt` now bumps the epoch (so any
backlog is skipped), closes both channels and joins both threads. Three
further runs were clean. The daemon gets this for free when it drops its
`Stt` on shutdown.

### 5f. What changed in the tree

- `cosmo-stt`: `engine.rs` (`Stt`, `SttConfig`, workers), `session.rs`
  (`Session`, `Event`, `Transcript`), `model.rs` (finding and building
  transducers), `hotwords.rs`, `bpe.rs`, `join`. The spike's
  `pub use sherpa_onnx` is gone. There's a `session` test (heavy, 3
  tests) and a `transcribe` example (`--fast`, `--hotwords`, `--apps`;
  `COSMO_ASR_STREAMING`/`COSMO_ASR_OFFLINE` pick other models).
- `cosmo-audio`: `Segmenter::speech_pending`.
- `cosmo-config`: the four ASR keys, validated, in the commented default.
- `cosmo-daemon`: `link_asr_tts` now runs through `cosmo_stt`'s API with
  the default pair (no `BPE_VOCAB` needed).
- `scripts/fetch-models`: components, `--asr`, `--asr-bench`.

## §6. bench-asr (spec part 3.6, tool built 2026-09-29; decision open)

**The tool is ready; the decision needs the user's voice.** Built and run
end to end on synthetic speech, where it found and fixed one real bug
(§6a). No model choice is made from synthetic data.

### 6a. Found on the first run: hotwords hallucinating into silence

The first check used six Kokoro-voiced commands, padded the way real
clips are (750 ms of pre-roll, 300 ms of tail, at room noise level), with
all 98 installed app names as hotwords:

    said "Open Firefox" → "Zoom Zoom Zoom Firefox Firefox Firefox Zoom Zoom Zoom Zoom- Open Firefox."

Unbiased, the same clip was right. With many hotwords, the 0.6B offline
model's beam search fills near-silent audio with them. Every real
utterance starts with 750 ms of exactly that: the pre-roll. **Fix, in
`Session`:** the offline model now gets each segment trimmed to its
speech plus 300 ms either side (the VAD's speech windows mark the span).
The pre-roll still protects a first syllable the VAD flags late, which
is what it's for. The hallucination is gone, and a trim unit test covers
the margins.

### 6b. The tool

`scripts/bench-asr` wraps the `bench_asr` example (`cosmo-daemon`,
features `speech,ears`).

- **`record`** prompts each line of `scripts/bench-commands.txt` (30
  lines: 20 reflex-style commands with real installed app names, two
  one-word confirmations, 8 longer requests with pauses). Hold Right Ctrl,
  say it, release. Clips are cut **as the daemon will cut them**: 750 ms
  pre-roll before the press, 300 ms tail after the release, holds under
  300 ms ignored. It warns if a clip peaks under 0.03. Sessions resume;
  `--redo 3,7` re-records. Clips and `manifest.tsv` stay in
  `~/.local/share/cosmo/bench/commands/`. This is also the first live use
  of the hotkey (§3c's open check).
- **`run`** replays every clip through eight pairings, **each in its own
  child process**, so its memory figures (`VmRSS` after load, `VmHWM` at
  the end) are that pairing's alone:

  | | Streaming (partials) | Offline (commit) |
  |---|---|---|
  | A | nemotron-0.6b | tdt-0.6b-v2 (current default) |
  | B | nemotron-0.6b | unified-0.6b |
  | C | nemotron-0.6b | tdt-110m |
  | D | nemotron-0.6b | — |
  | E | fastconformer-480ms (106 MB) | tdt-110m |
  | F | fastconformer-480ms | — |
  | G | — | tdt-110m |
  | H | — | tdt-0.6b-v2 |

  The fastconformer streaming model is new to `--asr-bench`: it's the
  "smaller streaming model" of blueprint §16 (0.66 s load, ~230 MB).
  Each clip runs twice: plain and fast (accuracy), then with app-name
  hotwords **at real-time pace**, where release → commit is timed. The
  report gives WER on commands (≤ 6 words) and on long requests
  separately, exact matches on commands, latency p50/p95/max, load time
  and memory, then every misrecognition. `--only`, `--fast` and
  `--score` (hotword strength) narrow a run.
- **Scoring** (`cosmo_stt::score`, core tier, tested) normalizes both
  sides before counting word errors: case, punctuation, number words to
  digits, `%`, "per cent", spelled letters ("p d f" is "pdf", but a
  trailing "i" stays the pronoun). Otherwise the lowercase, unpunctuated
  streaming models would score as wrong on formatting alone.

### 6c. What the synthetic check says (and doesn't)

Six Kokoro clips (bm_george), `--fast`, app hotwords at score 1.5, after
the §6a fix:

| | Pairing | WER commands | WER long | RSS loaded / peak |
|---|---|---|---|---|
| A | nemotron + tdt-0.6b-v2 | 6.7% | 10.0% | 1,734 / 1,845 MB |
| F | fastconformer alone | 13.3% | 10.0% | 228 / 244 MB |
| G | tdt-110m alone | **0.0%** | **0.0%** | 208 / 298 MB |

- A's command error is **"Yes" → "Spotify."** A score sweep showed it
  isn't hotword strength: at 0.5 it's "Yes, sir.", at 1.0 "I". The 0.6B
  model struggles with a bare synthetic "Yes".
- The 110M model was perfect up to score 1.5 and slipped at 2.0 ("Work
  Space Three"), so **1.5 stays the default**.
- "PipeWire" isn't an app name, so the app list doesn't bias toward it,
  and both big models wrote "pipe wire". Phase 4's phrase list is where
  that gets fixed.
- **The memory gap is large**: ~1.8 GB for the default pair, against
  ~0.2–0.3 GB for either small model.

Six synthetic clips from one voice decide nothing: TTS audio is cleaner
and more regular than a person at a desk mic. They prove the pipeline,
and they suggest the 110M-alone pairing deserves a hard look. That is
exactly blueprint §16's hunch.

### 6d. (Superseded by §6e.)

The recordings were made on 2026-09-29, and §6e is the decision.

### 6e. The decision, on the user's voice (2026-09-29)

30 clips recorded with Right Ctrl (all 30 lines), then all eight
pairings, with app-name hotwords at score 1.5, real-time pace:

| | Pairing | WER commands | exact commands | WER long | WER plain (all) | release → commit p50 / p95 / max | RSS loaded / peak |
|---|---|---|---|---|---|---|---|
| A | nemotron + tdt-0.6b-v2 (old default) | 14.0% | 17/22 | 1.3% | 10.6% | 84 / 155 / 179 ms | 1,723 / 1,849 MB |
| **B** | **nemotron + unified-0.6b** | **1.8%** | **21/22** | **1.3%** | 6.8% | 107 / 189 / 207 ms | 1,735 / 1,879 MB |
| C | nemotron + tdt-110m | 12.3% | 17/22 | 8.0% | 16.7% | 39 / 63 / 74 ms | 1,074 / 1,168 MB |
| D | nemotron alone | 21.1% | 15/22 | 6.7% | 12.9% | 75 / 146 / 150 ms | 889 / 921 MB |
| E | fastconformer + tdt-110m | 12.3% | 17/22 | 8.0% | 16.7% | 37 / 58 / 67 ms | 419 / 496 MB |
| F | fastconformer alone | 24.6% | 15/22 | 10.7% | 16.7% | 23 / 48 / 48 ms | 228 / 244 MB |
| G | tdt-110m alone | 12.3% | 17/22 | 8.0% | 16.7% | 39 / 61 / 69 ms | 208 / 273 MB |
| H | tdt-0.6b-v2 alone | 14.0% | 17/22 | 1.3% | 10.6% | 82 / 144 / 165 ms | 812 / 909 MB |

(The 22 "commands" are the clips of six words or fewer.)

**Decision: B, nemotron-0.6b streaming + parakeet-unified-0.6b offline.**
It is the default from now on (`scripts/fetch-models --asr` fetches it;
tdt-0.6b-v2 moved to `--asr-bench`). It's cosmic-voice's commit model,
which the blueprint started from.

- **B's two misses aren't really misses.** "Turn it up" → "Turned it up"
  and "…GitHub *in* Thunderbird" → "…GitHub *and* Thunderbird" came out
  the same from **all eight pairings**, so that's how the recordings
  sound. On what's actually in the audio, B made no errors.
- **The old default hallucinates hotwords on short commands.** Both
  tdt-0.6b-v2 pairings turned "Mute" into **"Zoom Zoom."**, even with
  the §6a trim, and "Yes" into "Mm.". Its plain pass (10.6%) beat its
  hotwords pass on commands (14.0%): hotwords made it *worse*. B went the
  other way: 6.8% plain → 1.8% with hotwords.
- **The small models are cheaper but not good enough.** The 110M model
  also inserts app names ("Launch LibreWolf" → "**Claude** LibreWolf")
  and mishears ("four" → "for", "build" → "bill"). The streaming models
  alone split names ("key pass XC", "Libra Wolf", "D Beaver"). E
  (420 MB) is the fallback if memory ever matters more than accuracy.
  It's a config change (`asr_offline_model`, `asr_streaming_model`),
  not a code change.
- **Cost of B:** about 1.7 GB resident, and release → commit p50
  **107 ms** (max 207 ms), which is well inside budget.
- The streaming models' dropped first word (§5) didn't recur: the
  pre-roll covers it, as expected.

**For phase 4: the hotword list needs curating.** Every hallucination
above is an installed app whose name is a common word or a short one
("Zoom", "Claude"), inserted into short or quiet audio. Before the reflex
phrases join the list, app names that are ordinary English words should
be dropped or down-weighted.

The `session` test's hotword check was flipped to match. B writes the
test sentence's "Phoebe" correctly unbiased, so the test now biases
toward the misspelling "Phebe" and asserts it appears. That proves the
hotwords reach the offline pass, whatever the model gets right on its
own.

### 6f. The release tail, measured on real releases (closes §7d)

Each bench clip ends exactly 300 ms after the key release, so the VAD's
last speech window says whether speech was still going at the release:

    speech after the release (VAD, incl. its ~100 ms debounce), n = 30:
      min −354, p25 −23, median 51, p75 179, max 201 ms
      18 of 30 clips still had speech after the release

**The tail stays as designed.** Cutting at the release would have lost
the end of more than half the commands. Because it stops as soon as the
VAD hears silence, it costs about 50 ms typically, and on these 30 real
releases it never reached its 300 ms cap. The capped tails in §7c and §8a
were artefacts of stopping 200 ms after `pw-play` returned. (Readout:
the `vad_segments` example now prints where speech ends in each clip.)


## §7. Daemon wiring (spec part 3.7, 2026-09-29)

**Done, and live through the real daemon** by acoustic loop: `cosmo
listen`, a spoken command played from the speaker into the laptop mic,
`cosmo listen` again, and the transcript printed. What only a person can
check (a real hold of Right Ctrl, the saturated-machine run with a real
utterance) is §3.8.

### 7a. Shape (`cosmo-daemon/src/ears.rs`, feature `ears`)

- **`ears::start`** wires the devices: the capture stream (gated by
  playback's `SpeechGate`, so cosmo never hears itself), the hotkey
  watcher on `trigger_key`, and the models, loading on their own thread
  (~1.8 s). None of it is fatal. If something fails, `doctor` says what,
  and `cosmo listen` answers "not ready".
- **`ears::run`** is the controller. It reads a `Ring` and a channel of
  `Trigger`s (press, release, toggle) and talks to the engine through a
  small `Host` trait (state, events, interrupt, paused, hotwords). It never
  touches a device, which is what makes §7c possible. One recording at a
  time:
  - **Press** (or `cosmo listen`): interrupt any reply being spoken, mark
    the ring 750 ms before the *press edge* (not before "now"), open a
    `Session` with the hotwords, go `Listening`. Every 30 ms, the new ring
    audio goes into the session, and partials go out as
    `Event::Transcript { final: false }`.
  - **Release**: holds under **300 ms are discarded** (`Session::cancel`,
    no decode), because Right Ctrl is also a shortcut modifier (§3a).
    Otherwise, wait a **tail of at most 300 ms, cut short as soon as the
    VAD hears silence**, finish, and emit `Event::Transcript { final:
    true, latency_ms }`, then `Idle`.
  - **Silence backstop (6 s) or a 60 s cap** ends a recording whose
    release never arrives.
  - Paused (`cosmo toggle`): presses are ignored.
- **The engine** implements `Host`, holds `Ears` (a `OnceLock`, set after
  the engine is shared, since the controller holds the engine), and gains
  `Command::Listen` and an **`ears` doctor line**: mic streaming and its
  worst callback gap, the trigger key and how many keyboards carry it,
  and both models with load times and whether hotwords are on.
- **Hotwords** in the daemon are the installed app names. The focused
  `app_id` is looked up (one Wayland round trip through `cosmo-focus`)
  only when a per-app set exists; none does until phase 4.
- The `ears` feature now implies `speech`: capture is the same PipeWire
  backend, and half-duplex needs the player's gate.

### 7b. Protocol and CLI

- `Command::Listen` → `Response::Listening { active }`.
- `Event::Transcript { text, final, latency_ms }`. On the wire the field
  is `final`, as the spec writes it; in Rust it's `r#final`. A test pins
  the wire name.
- **`cosmo listen`** toggles. The invocation that *started* the recording
  stays connected, shows the partial on one updating line, and prints
  the final with its latency. A second `cosmo listen` stops it and exits.
  Bound to a COSMIC shortcut, press once to start and once to stop.
- **`cosmo transcripts`** prints every transcript until Ctrl+C. When
  piped it prints finals only, one per line.

### 7c. Verified

**Controller, end to end without hardware** (`cosmo-daemon/tests/ears.rs`,
heavy, 5 tests). A thread writes a ring at real-time pace (room noise,
then a real speech clip), trigger edges are sent on the clock, and the
real models run:

| Test | Result |
|---|---|
| hold across "I love you." | "I love you.", **63 ms** release → commit (tail skipped: speech had ended), states `Listening → Idle`, reply interrupted |
| press 400 ms *into* the speech | still "I love you.": the pre-roll covers it |
| 150 ms tap | discarded, no final |
| `cosmo listen` toggle around the 7.4 s sentence | 11 partials; commit **0 ms** after the stop (every segment was cut and decoded during the pauses) |
| paused | nothing happens |

**The real daemon** (`cosmod` built with `--features ears`):

    ✓ ears   mic streaming (longest callback gap 21 ms, 0 reconnects); trigger
             key 97 on 5 keyboards; streaming …nemotron…560ms… (1.7s); offline
             …parakeet-tdt-0.6b-v2… (1.4s, hotwords)

Acoustic loop through `cosmo listen`, three Kokoro clips played with
`pw-play` (cosmo's own voice would be gated out, correctly):

| Played | `cosmo listen` printed | Release → commit | of which tail / decode |
|---|---|---|---|
| "Restart PipeWire, then play something on Spotify" | "Restart Pipe Wire, then play something on Spotify." | 410 ms | — / 157 ms |
| "Open Firefox" | "Open Firefox." | 378 ms | 283 / 94 ms |
| "Move this window to workspace three" | "Move this window to Workspace 3." | 439 ms | 300 / 137 ms |

### 7d. Open: the release tail is most of the latency here

In these runs the tail ran almost to its 300 ms limit: the VAD still
heard speech at the stop. The stop came 200 ms after `pw-play` returned,
but playback latency and room echo put the audio's real end at the mic
later than that. A person releasing a key after finishing a word is a
different case. The 63 ms test result is what the design does when
speech has already stopped.

**Re-measure with real presses (§3.8) before tuning.** If real releases
also pay the full tail, options are a shorter cap (150 ms), or relying
on the pre-roll-style margin already inside the session trim.

### 7e. What changed in the tree

- `cosmo-daemon`: `ears.rs`; engine `Host` impl, `Command::Listen`, the
  `ears` doctor check; `lib.rs` wires ears after speech (sharing the
  gate). New dependencies are `cosmo-hotkey` and `cosmo-focus` (both core
  tier); `ears` implies `speech`; the `ears` test.
- `cosmo-stt`: `Session::cancel`, `Session::hearing_speech`,
  `Hotwords::has_app_sets`.
- `cosmo-ipc`: `Command::Listen`, `Response::Listening`,
  `Event::Transcript`, with round-trip tests.
- `cosmo-cli`: `cosmo listen`, `cosmo transcripts`.

## §8. DoD (spec part 3.8, 2026-09-29): two of three boxes

### 8a. The saturated-machine test, end to end

§2b ran it on capture alone. This run is the whole path. The daemon was
running (`--features ears`, default models). A **clean `cargo build
--release --workspace`** went into a scratch target directory: 276
crates, 69 s, load 11 → 15 on 24 threads, with some unrelated load
already running (4 before the build). Meanwhile six commands went through
`cosmo listen` by acoustic loop (`pw-play` → speaker → laptop mic):

| Load | Played | `cosmo listen` printed | Release → commit | tail / decode |
|---|---|---|---|---|
| 13.2 | "Open Firefox" | "Open Firefox." | 810 ms | 300 / 508 ms |
| 13.6 | "Set the volume to thirty percent" | "Set the volume to thirty per cent." | 303 ms | 125 / 177 ms |
| 13.8 | "Move this window to workspace three" | "Move this window to Workspace 3." | 430 ms | 301 / 128 ms |
| 14.2 | "Yes" | "Yes." | 184 ms | 63 / 120 ms |
| 14.8 | "Restart PipeWire, then play something on Spotify" | "Restart Pipe Wire, then play something on Spotify." | 418 ms | 188 / 229 ms |
| 14.7 | "Find the PDF I downloaded yesterday and open it in the document viewer" | "Find the PDF I downloaded yesterday and open it in the Document Viewer." | 523 ms | 300 / 222 ms |

- **No audio lost.** Capture's worst callback gap was **28 ms** (21 ms
  idle, one PipeWire quantum), with **0 reconnects**, and no "recording
  fell behind the ring" warning. That's the RT data loop holding, as in
  §2b.
- **Every command was heard right.** "per cent" and "Workspace 3" are
  formatting that the bench normalizes. "Pipe Wire" is a missing hotword:
  it isn't an app name (phase 4).
- **Decoding slows under load**, as it must: 120–230 ms against 94–157 ms
  idle, and one **508 ms** outlier on the first clip, which was also the
  first decode after the build started. Capture is protected by the RT
  loop; recognition shares the CPU with the compiler. If that matters,
  the fix is scheduling (nice levels, pinning the offline threads to
  P-cores), not something to do before a real person has used it.
- The **tail** is again a large share (§7d).

### 8b. `doctor`

One `ears` line covers the list: mic streaming (worst callback gap,
reconnects), the trigger key and how many keyboards carry it, and both
models with names, load times and hotword state. With **no readable
keyboard**, it names the likely cause (the logind uaccess ACL on
`/dev/input/event*`, which only a local seat session gets) and points to
`cosmo listen`.

### 8c. The hold test: done by the user (2026-09-29)

The user held Right Ctrl and spoke to the live daemon, and reported it
working. The daemon's output was on their terminal, not in a file this
session could read. Independent evidence of the hotkey path: all 30
bench clips were recorded with real Right Ctrl holds, through the same
watcher, edges and 300 ms minimum hold the daemon uses. The instructions
as given:

**Hold Right Ctrl, speak, release → partials, then a transcript.** Every
piece is verified: the hotkey attaches to five keyboards (§3c), the
controller is tested with synthetic edges (§7c), and the live daemon has
transcribed through the mic (§7c, §8a). A real finger on a real key is
what's left. It's also the first real measurement of the release tail
(§7d):

    cargo build --release -p cosmo-daemon -p cosmo-cli --features cosmo-daemon/ears
    ./target/release/cosmod &          # or RUST_LOG=cosmo_daemon=debug for tail timings
    ./target/release/cosmo transcripts # then hold Right Ctrl and talk
