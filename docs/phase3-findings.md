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
