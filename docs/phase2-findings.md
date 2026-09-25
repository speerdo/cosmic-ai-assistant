# Phase 2 findings

**Started:** 2026-09-17
**Scope so far:** §2.0 done (packages installed 2026-09-18), §2.1 link spike
done (§1 — `ort` and `pipewire` both pass; `koko` turned out not to exist),
§2.2 core types done and then reviewed (§R — five defects fixed), §2.4
provider + §2.10 splitter done (§4, §10) and reviewed (§R2 — three defects
fixed). §2.3 playback done 2026-09-24 (§3); the daemon speaks replies (§4b). §2.5 Kokoro done 2026-09-24 (§5):
`ort` plus the system eSpeak NG loaded at run time; it's the default voice.
§2.6 phrase cache done 2026-09-25 (§6).

## §0. System packages (spec part 2.0) — DONE 2026-09-18

Audited 2026-09-17, installed 2026-09-18. **DoD met:**
`pkg-config --exists libpipewire-0.3` succeeds.

| Package | Version installed |
|---|---|
| `clang` | 18.1.3 (`1:18.0-59~exp2`) |
| `libclang-dev` | `1:18.0-59~exp2` |
| `cmake` | 3.28.3 (`3.28.3-1build7`) |
| `libpipewire-0.3-dev` | 1.6.8 (`1.6.8-1pop1~…~24.04~fdff050`) |
| `pkg-config` | 1.8.1 (present since step 0) |
| `build-essential`, `libwayland-dev`, `wayland-protocols` | present (phase-1 subset, 2026-09-10) |

`pkg-config --modversion libpipewire-0.3` → **1.6.8**. Worth writing down:
the workspace pins the `pipewire` *crate* at 0.10, whose bindings are
generated against whatever `libpipewire-0.3` the machine has. 1.6.8 is well
ahead of the 0.3.x-era API the crate was written for (PipeWire kept the
`0.3` soname across its 1.x releases), so a binding mismatch would show up
at §2.1 as a compile error in the generated bindings rather than as a
runtime surprise. §2.1 is the test of that.

### What this unblocked

Between the 2026-09-17 audit and this install, **two parts were blocked, not
one**: §2.1 (the `ort`/`koko`/`pipewire` link spike) and §2.3 (the PipeWire
playback path, whose `pipewire` crate binds through `libclang`). Because
§2.4's DoD is "`cosmo say` *speaks*", the phase-2 headline DoD was downstream
of this install too — the OpenAI *provider* needs no native dependency, but
the speaker it plays through does. §2.2, §2.4 and §2.10 were all deliberately
native-free and shipped meanwhile.

The command that ran:

    sudo apt install clang libclang-dev cmake libpipewire-0.3-dev

## §1. Link spike (spec part 2.1, 2026-09-18)

**Verdict: `ort` and `pipewire` both PASS, live-verified. `koko` does not
exist as the plan describes it, and no Kokoro crate is a clean fit — §2.5
has a decision to make that it did not know it had.**

Two feature-gated examples, both run against the real session:

    cargo run -p cosmo-audio --example link_pipewire --features pipewire-backend
    cargo run -p cosmo-tts   --example link_ort      --features kokoro

### 1a. The headline: `koko = "0.2"` was a phantom dependency

The blueprint §4 table says Kokoro lives at "`kokoros` (the `koko` crate)",
and `koko = "0.2"` has sat in `workspace.dependencies` since step 0, never
resolved. It is not the TTS crate:

| Name on crates.io | What it actually is |
|---|---|
| `koko` 0.2.0 | **"A tool to simplify self-hosting services"** — last published 2023-06-26, no repository, no keywords, unrelated |
| `kokoros` | **not published on crates.io at all** (GitHub only — it would have to be a git dependency) |
| `kokoro` 0.0.0 | an unrelated dynamic publish-subscribe framework |

Had §2.5 started by adding the declared dependency, it would have compiled a
self-hosting tool and wondered where `synthesize` went. This is precisely the
failure the plan's "prove the link **before** designing around these crates"
was written to catch, and it is worth stating that the cost of *not* running
the spike first would have been paid in §2.5 debugging, not here.

`koko` has been removed from `workspace.dependencies`. Nothing replaced it
yet — see 1e.

### 1b. onnxruntime acquisition: prebuilt static, no system library

`ort`'s `download-binaries` fetches a prebuilt archive from pyke's CDN into
`~/.cache/ort.pyke.io/dfbin/<target>/<hash>/` and links it **statically**:

    cargo:rustc-link-lib=static=onnxruntime
    cargo:rustc-link-lib=stdc++

| Measure | Value |
|---|---|
| Cached `libonnxruntime.a` | **101 MB** |
| `ldd` on the spike binary | no `libonnxruntime` — nothing to ship alongside |
| System `libonnxruntime` needed | **no** |

So there is no runtime library to package (packaging §8 gets an easier job
than feared), at the cost of a 101 MB one-time download and a fat binary.

### 1c. `ort`'s defaults wanted OpenSSL; we took rustls instead

The first build failed in `openssl-sys`: `ort`'s default feature set includes
`tls-native`, so the *downloader* pulls native-tls and the build needs
`libssl-dev` — a system package **not** on §2.0's list. TLS here exists only
to fetch one archive, so the workspace now pins:

    ort = { version = "2.0.0-rc.13", default-features = false,
            features = ["std", "ndarray", "tracing", "download-binaries", "tls-rustls"] }

That keeps §2.0's package list exactly as recorded. **`ort 2.0.0-rc.13` did
not fight in any other way** — it compiled clean and committed a live
environment first try, which is the opposite of what a release candidate was
budgeted for.

### 1d. Which system packages actually earned their place

| Package | Exercised? | By what |
|---|---|---|
| `clang` / `libclang-dev` | **yes** | bindgen: 7,691 lines of `pipewire-sys` bindings + 10,080 of `libspa-sys`, generated against libpipewire **1.6.8** |
| `libpipewire-0.3-dev` | **yes** | the above, plus linking `libpipewire-0.3.so.0` |
| `cmake` | **no** | nothing in the spike invoked it — no `CMakeCache.txt` or `CMakeFiles` anywhere in the target tree. `aws-lc-sys` (rustls's crypto backend) took its non-cmake path |

`cmake` stays installed as insurance for phase 3's `sherpa-onnx`, which the
phase-3 plan lists it for. It bought nothing in phase 2, and that is worth
knowing before anyone treats the §2.0 list as load-bearing.

Also settled: the crate-vs-library version worry from §0 is a non-issue. The
`pipewire` **crate** at 0.10 binds fine against libpipewire **1.6.8**, because
PipeWire kept the `0.3` soname across its 1.x line.

### 1e. Cold build times and binary size (24-core machine, debug profile)

Measured from a wiped target directory with the `ort` download cache deleted,
so the 101 MB fetch is inside the number. Reproduced twice, within 0.5 s.

| Spike | Cold wall time | Crates compiled | Stripped binary |
|---|---|---|---|
| `link_pipewire` | **8.5 s** | 49 | **416 KB** |
| `link_ort` | **12.6 s** | 143 | **22 MB** |

The 22 MB is the static onnxruntime, and it is the number §8 packaging should
plan around. Neither build is slow enough to justify the two-tier CI on time
grounds — the reason for two tiers is the *system packages*, not the clock.

### 1f. Kokoro crate choice — a recommendation, not yet a decision

With `koko` gone, two candidates are actually published. Both are
**application-shaped, not library-shaped**: each ships its own CLI, its own
model downloader, and its own audio playback stack.

| | `kokoroxide` 0.1.5 | `kokoro-tiny` 0.1.0 |
|---|---|---|
| ONNX runtime | **`ort` 1.16** — a different major from ours | `ort` 2.0.0-rc.10, unifies on our **rc.13** ✓ |
| Voice listing | none | `voices() -> Vec<String>` ✓ |
| Raw f32 PCM | via `GeneratedAudio` | `synthesize() -> Result<Vec<f32>, String>` ✓ — exactly `Pcm`'s shape |
| Style blending (parked) | `VoiceStyle::get_style_vector` ✓ | style vectors held as `HashMap<String, Vec<f32>>`, reachable |
| Own playback stack | `rodio` + symphonia, **not optional** ✗ | `cpal` + `rodio` behind a default feature — **`default-features = false` drops them** ✓ |
| Optional deps at all | **0** | 4 |
| Errors | `Box<dyn Error>` | `String` |

`kokoroxide` is disqualified on the `ort` major alone: two onnxruntime statics
in one binary is not a thing to attempt. **`kokoro-tiny` is the recommendation
on API shape** — `voices()` plus `Vec<f32>` is almost literally
`VoiceProvider::list_voices` and `synthesize`.

Its costs are real and are §2.5's to accept or reject:

1. **It needs `libssl-dev`.** Verified by building: `kokoro-tiny` depends on
   `reqwest` 0.12 with default features (its model downloader), which pulls
   `openssl-sys` and fails exactly as `ort` did — and unlike `ort`, we cannot
   reach in and swap its TLS backend. That is one more `sudo apt install`.
2. **A second `reqwest` major** (0.12 beside our 0.13) in the binary.
3. **`atty` 0.2.14**, unmaintained and carrying a published advisory, pulled
   as a non-optional dependency of a library that only needs it for its own
   binary.
4. Its downloader fetches models from a hard-coded GitHub release URL at run
   time, which is not §2.5's `scripts/fetch-models` with checksums.

**The alternative worth pricing before committing:** drive the Kokoro ONNX
model through `ort` directly — which this spike has now proven works — with
`espeak-rs` for grapheme-to-phoneme. That is essentially what both crates do
in a few hundred lines, and it drops all four costs above. `libespeak-ng1`
and `espeak-ng-data` are already installed on this machine.

No Kokoro crate has been added to `Cargo.toml`. Declaring one before the
decision is made is the mistake `koko = "0.2"` already made once.

**Decided 2026-09-24: the alternative.** §2.5 drives the Kokoro ONNX model
through `ort` directly and uses eSpeak for phonemes. §5a explains why that
became the system `libespeak-ng` loaded at run time rather than `espeak-rs`. No Kokoro crate,
no `libssl-dev`, no second `reqwest`, no `atty`, and model files arrive
through `scripts/fetch-models` with checksums.

### 1g. CI two-tier shape — decided

**Every native dependency sits behind a non-default feature**, which is what
makes the split clean rather than aspirational:

- `cosmo-audio/pipewire-backend` → `dep:pipewire`
- `cosmo-tts/kokoro` → `dep:ort`

**Core tier** — always runs, needs nothing beyond phase 1's packages:

    cargo test --workspace            # 99 tests, zero native crates in the graph
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    cargo doc --no-deps --workspace

Verified after the spike landed: `cargo tree -p cosmo-tts` contains **no**
`ort` and no `pipewire`, so §2.2's "no native build in this crate's
dependency graph" DoD still holds with the feature off.

**Heavy tier** — opt-in, needs `clang libclang-dev libpipewire-0.3-dev` and
network for the 101 MB onnxruntime fetch:

    cargo build -p cosmo-audio --features pipewire-backend --example link_pipewire
    cargo build -p cosmo-tts   --features kokoro           --example link_ort

Per the spec, **CI is not made red by this in phase 2** either way: the heavy
tier is added as a separate job that may be skipped, and the core tier is the
gate. The examples stay in the tree as the tier's smoke test — they are the
cheapest thing that fails loudly if a native dependency stops resolving.

### 1h. Carried forward to §2.3

`pipewire` 0.10 replaced 0.8's plain `MainLoop::new()` with explicit
ownership variants — the working idiom is now:

```rust
let main_loop = pw::main_loop::MainLoopRc::new(None)?;
let context   = pw::context::ContextRc::new(&main_loop, None)?;
let core      = context.connect_rc(None)?;
```

The plan calls cosmic-voice's `audio.rs` "close to liftable"; it is, after
this rename. Worth knowing before §2.3 starts rather than during.

## §2. Core types (spec part 2.2, 2026-09-17)

Decisions and deviations, so the next session doesn't re-derive them:

1. **`synthesize` returns a `BoxFuture`, not a plain `Result`.** The
   blueprint's trait signature is synchronous, but every real provider wants
   the daemon's tokio runtime (reqwest for OpenAI §2.4, CPU-bound ort for
   Kokoro §2.5), and async-fn-in-trait is not object-safe — the registry has
   to hand out `Box<dyn VoiceProvider>`. The method set and `stream`'s
   BoxStream signature match blueprint §4 exactly. The default `stream` maps
   each incoming chunk through `synthesize` in order; phase 5 replaces it
   with real sentence streaming.
2. **`SecretKey` moved to `cosmo-config::secret`**, re-exported from
   `cosmo_reason::secret`, so every existing path still resolves. Why here:
   §2.4's provider takes the key in `ProviderInit`, and `cosmo-tts` must not
   depend on `cosmo-reason` — in phase 5 the reasoner calls the voice layer
   for sentence streaming, so the arrow has to point the other way.
   `cosmo-config` is where the "never a credential" policy already lives;
   the newtype is its enforcement arm. The redaction tests moved with it,
   and `from_raw` became the documented constructor the key sources build
   through.
3. **WAV via `hound`** (pure Rust; workspace `hound = "3.5"`): the phrase
   cache's file format. Encoding is always 16-bit mono PCM; decoding accepts
   8/16/24/32-bit integer and 32-bit float at any channel count and
   collapses to mono — the provider-boundary contract (playback and cache
   never see channel math).
4. **Config: `voice_provider` defaults to `"openai"`, `voice_id` to
   `"default"`.** Deliberate: the phase-2 end state is Kokoro as default,
   but that lands in §2.5 — a `kokoro` default today would name a provider
   that does not exist. `"default"` defers the choice to the provider; the
   commented-first-run file notes kokoro becomes the default once local
   models are fetched.
5. **`Accent` is a normalized newtype, not an enum.** The plan names four
   codes, but MeloTTS also ships `en-IN` and Piper carries more; grouping
   only needs the code to sort. `Accent::from_code` maps `en_US` → `en-US`
   so provider spellings group together.
6. **Bench span names** `speak/synthesize`, `speak/first_audio`,
   `speak/push` (spec, cross-cutting) are agreed but not yet emitted — they
   land with the playback path (§2.3) and the first provider (§2.4); §2.2
   has no caller to instrument.

**Tests:** 67 green across the workspace. 56 at the phase-1 wrap-up; the two
redaction tests *moved* with `SecretKey` rather than disappearing (they now
run in `cosmo-config`, joined by a third covering `expose`/`clone`), plus ten
new voice-layer ones — 56 − 2 + 3 + 10 = 67. `fmt` and `clippy -D warnings`
clean. No native dependency entered the build — spec §2.2's DoD holds.
(Superseded by §R: 71 after the review.)

## §R. Review of §2.2 (2026-09-17)

The same pass phase 1 got at its wrap-up (§R there), run against the one
part that has landed. Five defects, all on ticked boxes; three are in the
WAV codec, which is exactly the surface §2.6's phrase cache and §2.3's
playback are about to build on. Each fix ships with a test that fails
against the previous code.

The phase-1 lesson repeated in a new shape. There it was *tests over the
wrong caller*; here it is **tests over the encoder's own output**. The WAV
round-trip test writes with `to_wav_bytes` and reads with `from_wav_bytes`,
so it only ever exercises 16-bit mono — the one path that happened to be
right. The two hand-built fixtures (stereo, float) were written the same
way. No test ever handed the decoder a file this crate did not write, and
that is where all three codec defects lived.

### R1. 24-bit WAV decoded 256x too quiet — effectively silence

```rust
// 24-bit samples are delivered as i32 by hound, already shifted.
(SampleFormat::Int, 24 | 32) => … v as f32 / i32::MAX as f32
```

The comment is false. `hound`'s `read_le_i24` sign-extends a 24-bit sample
into an `i32` in ±2^23 and does **not** widen it to the `i32` range
(`hound-3.5.1/src/lib.rs`, `impl Sample for i32::read`). Positive full scale
decoded as `0.0039` instead of `1.0`. A 24-bit file — what `ffmpeg` and
`pw-record` produce by default — would have played as near-silence, and the
failure mode is a quiet output, not an error: the kind of thing that gets
blamed on the speaker, the voice, or PipeWire.

**Fixed:** the scale comes from `spec.bits_per_sample`, never from the
carrier type — `int_to_f32(v, bits)` divides by `2^(bits-1) - 1`, which is
also what the 8- and 16-bit arms were already doing by hand.
**Test:** `int_24_bit_decodes_at_full_scale`.

### R2. Decoded samples escaped `Pcm`'s documented range

`Pcm::data` documents `[-1.0, 1.0]`, and the decoder broke it on the first
sample of any file that reaches negative full scale: `i16::MIN / 32767` is
`-1.00003`, `i8::MIN / 127` is `-1.0079`. The stereo test had noticed and
**written the violation into its own assertion** ("cancellation is exact
only up to 16-bit quantization") rather than treating it as a bug. An
invariant a test has been taught to tolerate is not an invariant.

**Fixed:** `int_to_f32` clamps, and the float arm clamps too (a float WAV
can carry anything). The encoder's `*32767` and the decoder's `/32767` stay
symmetric, so round-trip error is unchanged.
**Test:** `decoded_samples_stay_in_range`; the stereo test now asserts exact
cancellation instead of a tolerance.

### R3. `to_wav_bytes()` panicked on a zero sample rate

`hound` computes `bytes_per_sec / spec.sample_rate` when it writes the `fmt`
chunk, so a zero-rate buffer divides by zero **inside the writer**. `Pcm::new`
accepts any rate, `duration()` has an explicit zero-rate branch, and
`duration_of_empty_is_zero` constructs `Pcm::new(0, …)` — so the crate both
produces and tests the value that crashes its encoder. §2.6's phrase cache
encodes whatever a provider returns; a provider mis-reporting its rate would
have taken the daemon down rather than failing one phrase.

**Fixed:** an explicit guard returning `TtsError::Wav`, matching the
decoder's existing zero-rate check and the crate's "structured error, not a
panic" rule. **Test:** `zero_rate_encode_is_a_structured_error`.

### R4. An empty registry rendered `(available: )`

`UnknownProvider` joins the registered names, and the daemon builds its
registry from config — before §2.4 lands there is nothing in it, so the
error a user would actually hit reads *unknown voice provider "openai"
(available: )*, which looks like a truncated message rather than a state.
**Fixed:** "none registered". **Test:**
`empty_registry_says_so_instead_of_trailing_off`.

### R5. Documentation links that do not resolve

`cargo doc` was never run on the new crate. `cosmo-tts`'s crate docs linked
three private modules (three `rustdoc` warnings), and
`cosmo-config::secret`'s `from_raw` carried a link labelled `KeySource`
pointing at its own module — that trait lives in `cosmo-reason`, which
`cosmo-config` cannot depend on. Two pre-existing warnings in `cosmo-gate`
(`HoldQueue`, a type that has never existed — the queue is `Gate`'s private
`HoldQueueInner`) went with them.

**Fixed:** prose and code spans where a link cannot resolve; the crate-root
list now links the re-exported public types instead. `cargo doc --no-deps
--workspace` is warning-free, and is worth keeping in the check set
alongside `fmt` and `clippy`.

**Tests:** 67 → 71 across the workspace; `fmt`, `clippy -D warnings`, and
now `cargo doc --no-deps --workspace` all clean.

### Not changed, on purpose

- **`Accent::from_code` upper-cases everything after the first `-`**, so a
  subtag form like `en-US-x-foo` normalizes oddly. No provider in the plan
  emits one; revisit if Piper's voice list proves otherwise (§2.8).
- **`stream`'s default continues after a failed chunk** rather than ending
  the stream. Phase 5 owns the real streaming policy; the current behavior
  is tested (`stream_carries_synthesis_errors`) so the replacement has a
  baseline to change deliberately.

## §3. Playback path (spec part 2.3, 2026-09-24)

**DoD met.** `cargo run -p cosmo-audio --example play_sine --features
pipewire-backend` plays on the real session. `Player::play(&Clip) ->
Playback` is the "enqueue speech buffer" API the daemon will use.

### What was built

| Piece | Where | Tier |
|---|---|---|
| `Clip`: mono f32 at its own rate, `Arc`-shared samples | `clip.rs` | core |
| `SpeechGate`, the half-duplex hook, with `SETTLE` = 350ms | `gate.rs` | core |
| Clip queue + `Playback` completion handle | `queue.rs` | core (unit-tested) |
| `Player`: the PipeWire thread and stream | `player.rs` | `pipewire-backend` |
| `play_sine`, `play_wav` examples | `examples/` | `pipewire-backend` |

`cosmo-audio` does **not** depend on `cosmo-tts`. `Pcm { sample_rate, data }`
maps to `Clip::new(sample_rate, data)`, so the daemon does the conversion
and the audio crate stays free of provider concerns.

### Design decisions

1. **The stream opens at the clip's rate, and PipeWire resamples.** Mono
   F32LE, position `MONO`, so PipeWire's adapter upmixes to the sink's
   channels. When a clip arrives at a different rate, it waits in a backlog
   until the current run of speech drains; then the stream is rebuilt. Kokoro
   and OpenAI both produce 24 kHz, so in practice one stream serves both.
2. **`RT_PROCESS`, with a callback that never blocks.** The fill callback
   runs on PipeWire's data loop and takes the queue with `try_lock`. When it
   loses the race it writes one quantum of silence. It only ever advances
   cursors, so a clip's last `Arc` is always dropped on the main loop and
   never freed on the RT thread.
3. **Completion is based on the drain.** When the queue runs dry, the
   callback calls `flush(true)` (the same as `pw-cat` at EOF). PipeWire's
   `drained` event means the audio has actually played out, and that event
   resolves the handles. The trade-off, which is documented: clips queued
   back to back resolve **together** when the run ends. Phase 5's sentence
   streaming awaits the last sentence anyway.
4. **The stream is inactive while idle.** After a drain the stream calls
   `set_active(false)`, so an idle daemon doesn't keep the sink awake with
   silence. The next clip reactivates it; no reconnect or renegotiation is
   needed.
5. **After a drain, `flush(false)` runs before reactivating.** This clears
   PipeWire's drained state so that `process` is called again. Nothing is
   buffered at that point, so nothing is discarded.
6. **The node is identified as `cosmo-speech` / "Cosmo speech", with
   `media.role = Accessibility`.** That is the closest of PipeWire's standard
   roles to synthesized speech. `Communication` was avoided because
   WirePlumber may route it through echo-cancel. The `COSMO_SINK` env var
   pins a target sink, for tests.
7. **`Buffer::requested()`** needs the crate's `v0_3_49` feature. Each call
   writes what the graph asked for, not a whole buffer, which would add
   latency.

### Half-duplex seam (invariant #8)

`SpeechGate` goes to *speaking* when a clip is admitted. It goes quiet on
`drained`, on stop, or on failure, and it records a timestamp. The method
`mic_open(now, SETTLE)` is lock-free, so phase 3's RT capture callback can
call it. A comment at the stream-construction site in `player.rs` tells
phase 3 to consult this gate rather than invent a second one.

### Live verification (COSMIC, Bluetooth sink SRS-XB43, Spotify playing)

`play_sine`, with times since start:

    1523ms sweep → Ok(Played)             1.5s sweep @ 48 kHz
    2045ms beeps → Ok(Played), Ok(Played) two 0.25s clips @ 24 kHz: rate switch + gapless
    2045ms speaking = false, mic open now = false, after settle = true
    3155ms → Ok(Played)                   0.3s beep after 800ms idle (stream reactivated)
    3463ms → Ok(Cancelled)                5s tone, stop() at 300ms

Every resolution lands within about 25ms of the audio's real duration. That
is only possible if the stream really drained through the device clock.

**Objective check that audio reached the sink:** a 1.0s 3 kHz tone in a
**22.05 kHz** WAV was played via `play_wav` while `pw-record` captured the
sink's monitor. A Goertzel filter at 3 kHz over 200ms windows measured:

    0 1 1 1 1 1 3658 4095 4095 4097 4095 437 2 2 2 6

That is exactly five windows of tone, silence either side, over music
playing on the same sink, and the resampling from 22.05 to 48 kHz was
PipeWire's.

**Stressing a clip enqueued while the stream drains:** 120 iterations of a
100ms clip, each followed by a second clip enqueued 80–140ms later, which
sweeps across the drain window. Every handle resolved, with no hang. The
worst case for the second clip was 136ms, about its own 100ms plus the
drain. This used a scratch example that was not kept in the tree.

### Not done here (and where it lands)

- **Daemon wiring** (`Speaking` state, and speaking replies after a turn) is
  §2.4's last box and §2.7.
- **Stream-failure recovery** (`state_changed → Error`) fails the in-flight
  handles with `AudioError::Stream` and rebuilds the stream on the next clip.
  It was **not** exercised live, because forcing a stream error on demand
  isn't practical. It gets revisited when the daemon runs it for real.
- **Capture** is phase 3.

## §4. OpenAI TTS provider (spec part 2.4, 2026-09-17)

The first builtin (`Registry::with_builtins()` registers `"openai"`), built
ahead of its speaker: §2.3's playback is blocked on the same apt install as
§2.1, but the provider itself is plain HTTPS over the phase-1 key source.
Decisions worth keeping:

1. **`gpt-4o-mini-tts`, `response_format: "wav"`.** WAV goes through the
   same `Pcm::from_wav_bytes` decoder as the phrase cache — no audio-codec
   dependency, and 24 kHz mono comes back ready for playback. The
   `instructions` field (affect/tone/pacing) is sourced from the new
   `voice_instructions` config key and **omitted entirely when empty**,
   so an unset style never sends `""` to the API.
2. **Voice catalogue = blueprint §4's list minus `marin`/`cedar`** (those
   exist only on the Realtime model cosmo does not use for speech):
   alloy, ash, ballad, coral, echo, sage, shimmer, verse — all `en-US`,
   `gender: None` (OpenAI documents none). `voice_id: "default"` resolves
   to `alloy` before the request; any other voice must be in the catalogue
   or the provider returns `UnknownVoice` locally — a structured error
   beats a 400 round trip.
3. **Errors follow the §1.4 three-state discipline, extended:** `NoKey`
   (checked before any network call), `Network` (transport),
   `RateLimited` (HTTP 429 — its own state, "retry later" is its fix),
   and `Synthesis` for the rest (status + a 200-char body snippet; 401
   reads "http 401 Unauthorized: bad key", never the key itself).
4. **`speak/synthesize` span is now emitted** (fields: provider, voice,
   text *bytes* — not the text, not the key). `speak/first_audio` and
   `speak/push` still wait for the playback path.
5. **`ProviderInit` grew the fields §2.2 forecast:** `base_url` (tests,
   self-hosted gateways), `model`, `instructions`. The provider clones the
   text/voice into the returned future so the future borrows only `&self` —
   the trait's elided output lifetime is `&self`'s, and unifying all three
   would have broken the default `stream` impl, which borrows chunk strings
   local to its own closure.
6. **What is deliberately not done:** the daemon does not speak yet. Wiring
   the provider to a completed turn needs §2.3's playback stream, and the
   daemon-side key resolution feeding `ProviderInit` is §2.7. The provider
   is exercised end to end against a fake speech server (phase-1 harness
   style: raw `TcpListener`, no HTTP-test dependency) — request shape,
   `instructions` presence, and every error mapping covered.

**Config:** `voice_model` (`gpt-4o-mini-tts`) and `voice_instructions`
(empty) joined `voice_provider`/`voice_id`, commented defaults and all.

## §4b. Spoken replies in the daemon (spec part 2.4, last box, 2026-09-24)

**The phase-2 headline works end to end against a fake server: `cosmo say`
speaks its reply.** It has not yet been run against the real OpenAI API,
because that needs a stored key (carry-over E1).

### Build

Speech sits behind a **non-default** daemon feature, consistent with §1g:

    cargo build -p cosmo-daemon --features speech

Without the feature the daemon answers in text only, and `doctor` says so:
`✗ speech — replies are text only — built without the speech feature`. If
PipeWire can't be reached at startup, the daemon doesn't exit. It stays
text-only and `doctor` shows the connection error.

### How it's wired (`crates/cosmo-daemon/src/speech.rs`)

- **Off the response path.** The CLI gets the reply text immediately, and
  synthesis and playback run in a spawned task. Nobody waits out the audio
  to see the reply printed.
- **State:** `Thinking` holds through synthesis (nothing is audible yet),
  then `Speaking` when the clip is admitted, then `Idle` on drain.
  `Speaking` has been reserved since phase 1 and is now real.
- **A new turn, or `cosmo confirm`, cuts speech off.** The interrupt bumps a
  generation counter under the state lock and stops the player. A task from
  an older generation never writes state again, so a stale drain can't flip
  the new turn's `Thinking` to `Idle`. This is unit-tested
  (`a_new_turn_cuts_speech_and_keeps_its_own_state`).
- **A speech failure never fails the turn.** The text was already delivered.
  The error is logged and shown by `doctor` as "last reply not spoken: …".
- **The provider is built lazily and cached only once it has a key.** That
  way `cosmo auth login` takes effect after startup without a restart,
  matching §1.4's rule that a locked keyring at boot is not a crash.
- **TTS honours `COSMO_API_BASE`**, the reasoner's existing test override.
  One fake server can then stand in for both halves of the OpenAI API.
- Only `Reply` outcomes are spoken. Hold prompts ("confirm with: cosmo
  confirm <token>") stay text-only. Speaking a token aloud is useless, and
  phase 4 owns the spoken `confirm-hold` phrase.
- The sink is a trait (`SpeechSink`), so the core tier tests all of this
  with a fake. `cosmo_audio::Player` implements it under the feature.

### Live run (COSMIC, the real daemon built with `--features speech`)

The fake server answered `/v1/chat/completions` with a fixed reply. On
`/v1/audio/speech` it produced **real words** through `libespeak-ng` (called
via ctypes; the `espeak-ng` binary isn't installed, but the library is),
returned as a 22.05 kHz WAV. The daemon used `OPENAI_API_KEY=sk-fake`, the
env source meant for dev and CI.

    doctor:  ✓ speech   provider openai, voice default
    say:     reply printed at once
    status:  Speaking  0.28s → 4.9s, then Idle     (clip: 213,150 B = 4.83s)
    fake:    speech request voice=alloy input='Hello Adam. This reply came…'
    log:     speak/synthesize … status=200 wav_bytes=213150
             playback stream connected rate=22050
             speak/first_audio latency_ms=16.96   (enqueue → first sample,
                                                   stream creation included)
             stream Streaming → Paused after drain

`voice: default` resolved to `alloy` in the request, as §4 intends. The
22.05 kHz clip went through a stream opened at its own rate, with no
resampling on our side.

### Still open

- **The real-key run (E1).** With a key stored (`cosmo auth-login`), one
  `cosmo say` against the real API covers the reasoning round trip and a
  genuinely OpenAI-voiced reply. Record TTFA from `speak/synthesize` there.
- `cosmo voice list / preview / set` is §2.7.

## §5. Kokoro provider (spec part 2.5, 2026-09-24)

**DoD met.** The daemon speaks through Kokoro with no speech network
traffic and no key involved in TTS. Voices list grouped by accent. The
real-time factor (RTF, synthesis time ÷ audio length) and time to first
audio (TTFA) are measured below. `voice_provider` now defaults to
`"kokoro"`.

    scripts/fetch-models                                      # ~340 MB, once
    cargo build --release -p cosmo-daemon --features speech   # PipeWire + Kokoro

### 5a. Phonemes: eSpeak NG via `dlopen`, not `espeak-rs` and not `misaki-rs`

§1f's decision said "`ort` + `espeak-rs`". The `ort` half stands. The
phonemizer half changed after looking at the options:

| Option | Verdict |
|---|---|
| `misaki-rs` 0.6 (MIT, Misaki's G2P in Rust) | **Rejected on measurement.** Without its `espeak` feature, out-of-vocabulary words are spelled letter by letter, and "Firefox", "Spotify", "Kubernetes", "PipeWire" and "htop" are all OOV. For an assistant that launches apps, that's disqualifying. Its lexicon was also rebuilt from eSpeak output (it emits eSpeak's `o‍ʊ` zero-width-joiner (ZWJ) digraphs, not Misaki's `O`), and it adds **38 MB** of embedded data. |
| `espeak-rs` 0.2 (via `espeak-rs-sys`) | Vendors eSpeak NG's C source and builds it with **cmake**, links it **statically**, and copies a data folder into `target/`. That would put GPL-3.0 code inside cosmo's MIT binary and add a data directory to ship. |
| `espeak-ng` 0.2 (pure-Rust port) | GPL-3.0-or-later, and its maturity is unproven. |
| **System `libespeak-ng.so.1` loaded with `libloading`** | **Chosen.** No `-dev` package, no headers, no C build. The GPL library stays a separate system component, installed by the distro, and is not linked into our binary. If it's missing, only the Kokoro provider fails to construct, with the package name in the message. |

The library and `espeak-ng-data` were already installed here, pulled in by
`speech-dispatcher`. **Packaging note for phase 8:** declare `libespeak-ng1`
+ `espeak-ng-data` (Debian/Pop!_OS) or `espeak-ng` (Fedora) as runtime
dependencies.

One eSpeak detail worth recording: **`en-gb` isn't a voice name in eSpeak
1.51.** `SetVoiceByName("en-gb")` returns error 2 and *silently keeps the
previous voice*, so British text got American phonemes. British is plain
`en`. The wrapper now fails on any non-zero return rather than speaking in
the wrong accent.

### 5b. eSpeak IPA → Kokoro's phoneme set

Kokoro v1.0 was trained on Misaki phonemes. eSpeak is called with IPA
output and `^` as the tie character, and the result is rewritten the way
Misaki's own eSpeak fallback does:

- the diphthongs become single letters: `e^ɪ`→`A`, `a^ɪ`→`I`, `o^ʊ`→`O`
  (US) / `ə^ʊ`→`Q` (GB), `a^ʊ`→`W`, `ɔ^ɪ`→`Y`
- the affricates collapse: `d^ʒ`→`ʤ`, `t^ʃ`→`ʧ`
- a few other sounds are adjusted: dark l `ə^l`→`ᵊl`, the glottal + syllabic
  n in "button" `ʔn̩`→`tᵊn`, `ɚ`→`əɹ`
- the US set drops length marks, and `ɜː`→`ɜɹ`

Triphthongs such as "fire" (`a^ɪ^ɚ`) are matched before the diphthongs
they contain. Without the rewrite, the tokenizer would still accept the
loose letters, but as two tokens the model never learned to read as one
sound.

eSpeak drops punctuation, and Kokoro uses it for prosody. So the text is
split on punctuation at word boundaries, and the marks are put back. `12.5`,
`1,000` and `10:30` are **not** split, so eSpeak still reads them as
numbers.

The token ids come from the model's own `tokenizer.json` (data, not code),
padded with `$` on both sides. Sequences over 510 tokens are cut at the last
space before the limit, so no word is split across two inferences. The
style vector is row `len(tokens)` of the voice pack (510 × 256 f32).

Checked on real text: every character of the mapped output is in Kokoro's
115-symbol vocabulary. eSpeak reads "htop" as "aitch-top", reasonable for a
word no lexicon has. G2P costs **~3 ms** per reply.

### 5c. Model variant and TTFA (Core Ultra 9 275HX, 24 threads, release build)

Same sentence, 3.3 s of audio, voice `af_heart`:

| Variant | File | Load | Synthesis (warm) | RTF |
|---|---|---|---|---|
| **fp32** (default) | `model.onnx`, 326 MB | 620 ms | **~520 ms** | 0.155 |
| fp16 | `model_fp16.onnx`, 163 MB | 674 ms | ~510 ms | 0.158 |
| q8 | `model_quantized.onnx`, 92 MB | 376 ms | 1,960 ms | 0.598 |

**The int8 export is 4× slower on this CPU**: dynamic quantization pays for
itself on memory, not on speed here. fp16 is no faster than fp32 either.
fp32 is the default, and the fetch script fetches only it unless asked.

**TTFA (whole-utterance synthesis, then playback) scales with reply length
at RTF ≈ 0.155:**

| Reply | Audio | Synthesis |
|---|---|---|
| "Moving Firefox." | 1.9 s | ~320 ms |
| one sentence | 3.3 s | ~520 ms |
| three sentences | 7.3–7.9 s | 1.16–1.23 s |

The plan expected 0.5–2 s, and that holds. **Even the shortest ack costs
~320 ms here, twice the reflex path's 150 ms budget.** That is the number
that justifies §2.6's phrase cache. The long-reply numbers are what phase 5's
sentence streaming targets: at RTF 0.155, the first sentence of any reply
is ready in a few hundred ms.

The load is paid **at daemon startup**, not on the first reply: the daemon
builds the provider in the background and `doctor` reports a failure (such
as missing model files) before anyone speaks. Thread count is ort's default
and was **not tuned**. That's worth one experiment before phase 5.

### 5d. Live DoD (the real daemon, default config, release build)

A fake server stood in for the reasoning model only.

    doctor:  ✓ speech   provider kokoro, voice default
    log:     kokoro model loaded … load_ms=634           (at startup)
             voice provider ready provider="kokoro" local=true
             kokoro synthesized g2p_ms=2.8 total_ms=1188 tokens=111 audio_s=7.3
    status:  Thinking 0.28s → Speaking 1.44s → Idle 8.77s
    fake:    POST /v1/chat/completions   ← the only request

Nothing requested `/v1/audio/speech`. The speech was synthesized on this
machine, and the keyring was never consulted for it: a local provider is
constructed without resolving a key at all.

Listening: US (`af_heart`) and GB (`bm_george`) samples were played on the
real session. The objective checks passed: 0 clipped samples, peaks 0.58 and
0.64, and pauses at the sentence boundaries. **Whether they sound right is
the user's call, and it's still open.**

### 5e. What else changed

- `ProviderInit` gained `model_dir` (Kokoro's directory, default
  `~/.cache/cosmo/models/kokoro-v1.0`), as §2.2 anticipated.
- `voice_model` now defaults to empty, meaning the provider's own default.
  An OpenAI model name must not reach Kokoro, and OpenAI treats an empty
  value as `gpt-4o-mini-tts`.
- The daemon's `speech` feature now covers PipeWire **and** Kokoro, so one
  flag builds the whole voice layer.
- The provider is built on tokio's blocking pool, so a 0.6 s model load
  never stalls the runtime. Providers own the `speak/synthesize` span; the
  daemon no longer wraps a duplicate around it.
- Kokoro's worker thread owns the ONNX session (`Session::run` needs
  `&mut`) and serializes eSpeak, which isn't thread-safe.

### 5f. Open

- **Listening verdict** (the user's): do `af_heart` and `bm_george` sound
  natural, and do app names come out right?
- **ort thread count** is untuned.
- **Heteronyms** ("read", "live") get eSpeak's context-free guess. Misaki's
  POS tagger would do better; revisit only if it's audible in practice.

## §6. Phrase cache (spec part 2.6, 2026-09-25)

**DoD met**, with one part verified by test rather than live. The live
trigger for a voice switch is §2.7's `cosmo voice set`, which doesn't exist
yet. Everything else ran on the real daemon with the real Kokoro model.

### 6a. Design

**Where it lives.** `cosmo_tts::PhraseCache` is runtime-agnostic and
generic over any `VoiceProvider`. The daemon's `Speech` holds the active
voice's phrases **in memory** as ready-to-play clips, so an ack involves no
disk read and no synthesis.

**File layout.** Each phrase is stored at
`~/.cache/cosmo/voice/<provider>/<voice>/<key>.<hash>.wav`.

**Staleness is a lookup miss.** The hash in the filename covers the
provider, voice, model variant, phrase text and a format version. Changing
any of them changes the expected name, so the old file simply stops
matching. There is no manifest that could disagree with the directory. The
hash is FNV-1a, not `DefaultHasher`, so a Rust toolchain bump doesn't
invalidate every cache.

**Crash safety.** Each file is written as `…wav.tmp` and then renamed into
place. A render killed midway leaves finished phrases intact and the rest
missing. The next render fills exactly the gaps, and superseded files and
orphan `.tmp` files are swept once a render completes. A failed render
sweeps nothing.

**The vocabulary is a list, not an enum.** It's a `Vec<Phrase { key, text
}>`, currently `default_phrases()`: `ack-moving`, `ack-focused`,
`ack-launching`, `err-notfound` and `confirm-hold`. Phase 4 passes its own
list through `Speech::with_vocabulary`, and nothing in the cache changes.

**Voice switching is atomic.** `Speech::switch_to` renders the new voice
while the old one keeps serving both replies and cached acks. Progress
streams as `VoiceCacheProgress { done, total }` events over IPC. Then the
provider, the voice and the phrases all swap together.

- **If the render fails**, nothing switches. `VoiceCacheDone { ok: false }`
  is emitted, and `doctor` shows the error.
- **A switch superseded by a newer one never lands.** A generation ticket
  checks this at swap time.
- The one trade-off: for a second or two after a switch, *replies* are
  still in the old voice. That's deliberate. Acks and replies never
  disagree, and a failed switch costs nothing.

**Startup is the repair path.** `warm()` renders only what's missing for
the configured voice, so the same code handles a first run, an edited
phrase, and a render killed by `kill -9`.

**First live consumer: `confirm-hold`.** A held action now plays the cached
"That one needs your confirmation." The token itself is never spoken, and
confirmation stays local, as invariant #4 requires.

**Observability.** `doctor` reads `5/5 phrases cached` or `phrases
rendering 2/5`. The CLI prints `[voice] …` lines for the new events during
`say`. Phrase playback emits the `ack` span agreed in phase 1, and
`speak/first_audio` measures the push.

### 6b. Live verification (release daemon, Kokoro fp32, `af_heart`)

**A: first start with an empty cache.** An IPC client on the control socket
received:

     918ms voice_cache_progress done=1 total=5
    1132ms …done=2   1371ms …done=3   1637ms …done=4   2025ms …done=5
    2025ms voice_cache_done ok=true "5 phrases ready"
    log:     phrase cache up to date rendered=5 reused=0 swept=0
    doctor:  ✓ speech  provider kokoro, voice default, 5/5 phrases cached

The model loaded in 649ms, then five phrases rendered in about 1.1s.

**B: `kill -9` mid-render.** The test deleted the cache, started the
daemon, and SIGKILLed it as soon as two WAVs existed:

    after kill -9:   ack-focused.….wav  ack-moving.….wav          (2 of 5)
    restart log:     removing stale socket
                     phrase cache up to date rendered=3 reused=2 swept=0
    after restart:   all five

The restart repaired the phrase cache and the phase-1 socket in one go.

**C: a held action speaks its cue from the cache.** `cosmo say "install
cowsay for me"` went to the fake model, which returned `run_in_terminal`
with `apt install cowsay`. The gate held it (Waiting), and then:

    speak/first_audio latency_ms=37.8   (queued → first sample, including
                                          creating a cold PipeWire stream)
    clip done outcome=Ok(Played)         2.42s later — the whole phrase

That's **38ms against a 150ms budget**, where live synthesis of the same
line would have cost about 400ms (§5c). The hold was never confirmed. It
lived in the gate's memory and ended with the daemon.

### 6c. Verified by test (core tier, a gated fake provider)

Synthesis in these tests waits for one permit per phrase, so each test can
hold a render mid-flight. The audio length encodes which voice made it.

- `a_switch_serves_the_old_voice_until_the_new_one_is_ready` is the DoD
  sentence. With the switch 2 of 5 phrases in, an ack plays the old voice's
  clip and `doctor` says `rendering 2/5`. When the render finishes, voice
  and phrases swap together.
- `a_failed_render_keeps_the_old_voice` checks that the failure appears in
  `doctor` and the old acks still play.
- `a_superseded_switch_never_lands`.
- `the_next_start_completes_an_interrupted_render` gives exactly three
  permits for three missing phrases, so re-rendering all five would hang
  the test.
- In `cosmo-tts`: staleness when a phrase's text changes, voice and model
  as part of the identity, repair of a half-written `.tmp`, a failed render
  keeping its finished files, and hostile ids (`../../etc`) staying inside
  the cache root.

The daemon's speech tests passed 20 of 20 consecutive runs (no flakes).
Totals: 122 core-tier and 78 heavy-tier tests.

### 6d. Open

- **Live voice switch.** It becomes exercisable with §2.7's `cosmo voice
  set`, which calls `Speech::switch_to` and persists the choice.
- **Whether cloud voices get a phrase cache.** Rendering phrases for an
  `openai` voice costs five API calls once per voice. That's accepted for
  now; phase 4 might prefer Kokoro acks even when replies are cloud-voiced.

## §10. Sentence splitter (spec part 2.10, 2026-09-17)

`cosmo_tts::split` — the boundary-finder phase 5 will feed streamed
sentences through, built now so the streaming API settles. A heuristic over
ordinary assistant prose, with the trade made explicit: an over-split costs
a slightly early pause, an under-split costs latency.

- **Strong boundaries:** terminator *runs* (`...`, `!!`) and `…` split
  regardless of what case follows; so do bare `!`/`?` followed by
  whitespace. But `!`/`?` sitting directly against a closing quote or
  bracket were *interior* to the quotation — `He said "Go now!" Then…`
  splits, `(really!) about it` does not.
- **Weak boundary:** a single `.` splits only when something sentence-like
  follows (uppercase, digit, opening quote/bracket), minus the data-driven
  abbreviation list (`Mr.`, `e.g.`, months, `U.S`) and single-letter
  initials (`J. R. R.`). Lowercase continuations (`one. two.`) hold — the
  cost of missing a rare lowercase sentence start is smaller than the cost
  of shredding an unknown abbreviation.
- **Non-boundaries:** decimals (`3.14`) and any terminator glued to the
  next character. **Extra boundary:** a blank line, terminator or not
  (lists, paragraphs).
- The abbreviation list is `const` data with a comment saying to extend it
  there; `may` is deliberately absent (the full word ends sentences; only
  truncated month forms are listed), and so is `no` (sentences end on it
  constantly).

**Tests:** 71 → 94 across the workspace (four provider unit tests, five
fake-server integration tests, fourteen splitter cases); `fmt`,
`clippy -D warnings`, and `cargo doc --no-deps --workspace` all clean.
(Superseded by §R2: 99 after the review.)

## §R2. Review of §2.4 and §2.10 (2026-09-17)

Three defects, all on ticked boxes. None is in the request/response shape
the fake-server tests cover — they are in what happens *around* it: what the
splitter does to a numbered list, and what the provider does when a server
accepts the connection and then says nothing.

The pattern this time is **corpus blindness**. §2.10's fourteen cases are a
punctuation corpus — abbreviations, initials, decimals, quotes, ellipses —
assembled from the grammar of the problem rather than from what cosmo
actually says. Assistant replies are full of ordered lists, and not one case
had a list in it. Likewise §2.4's five integration tests cover every
*answer* a server can give and none of the ways it can fail to answer.

### R6. Ordered-list markers were split into their own sentences

```
split("1. First step. 2. Second step.")
  → ["1.", "First step.", "2.", "Second step."]
```

`is_abbreviation` suppresses the boundary after a single *letter*
(`J. R. R.`) but not after a digit, so every list marker became a sentence.
Phase 5 feeds these to `synthesize` one at a time, so the voice would have
said **"One."** — full stop, pause — before each item, in a reply shape
assistants produce constantly. The splitter's own doc names the trade as
"an over-split costs a slightly early pause"; this one costs a spoken
number.

**Fixed:** a number that is the first thing on its line is a list marker,
not a sentence. Line-initial is what keeps the rule narrow — in
`Shipped in 2024. Then it stuck.` the number is mid-line, so that still
splits. **Tests:** `ordered_list_markers_are_not_sentences` (both the
bare and the `Steps:\n1.` markdown shape) and, for the other direction,
`a_number_mid_line_still_ends_its_sentence`.

### R7. `etc` was on the abbreviation list, against the list's own rule

The list documents its own admission criterion — "only tokens that are
almost never the last word of a sentence" — and names `may` and `no` as
deliberate exclusions for exactly that reason. `etc.` ends list sentences
constantly (*"bread, milk, etc. Then come home."*), and a split there
already requires a sentence-like start, so the lowercase continuation case
(*"bread, etc. and then go"*) is held by the general rule regardless.

**Fixed:** removed, and the criterion's comment now names it alongside `may`
and `no`. **Test:** `etc_ends_a_sentence_when_a_new_one_follows`, covering
both directions.

### R8. A server that accepted and never answered hung the speak path forever

`reqwest::Client::new()` has no timeout. A black-holed connection — accepted,
then silent — is not a transport error, so `synthesize` never returned and
never will. Verified by reverting the fix: the new test ran until a 60-second
external cap killed it. This is the one failure mode the §1.4 discipline
cannot render, because there is no error to render: the user is waiting to
*hear* something and nothing ever arrives, not even a complaint.

**Fixed:** `DEFAULT_REQUEST_TIMEOUT` of 30s, overridable per provider through
the new `ProviderInit::request_timeout` — which is also what makes the
behavior testable in under a second. A timed-out request surfaces as
`TtsError::Network`, the state whose fix ("the service is unreachable")
is the true one. **Test:**
`a_server_that_never_answers_times_out_as_network`.

**`cosmo-reason` has the same gap** — `crates/cosmo-reason/src/lib.rs:73`
also builds a bare `reqwest::Client`, so a black-holed chat completion hangs
`cosmo say` the same way. Left alone deliberately: a completion's right
ceiling is a different number from a sentence of speech (streamed replies
can legitimately run long), and phase 1's DoD is closed. It belongs in §2.7
when the daemon owns both clients, and is recorded here so it is not
rediscovered as a phase-5 mystery.

### Also added

- `builtins_are_registered_under_their_config_names` — `Registry::with_builtins()`
  had no test, so a builtin present in the tree but never registered would
  have surfaced as "unknown provider" for a name `config.ron` legitimately
  allows. It is the daemon's only construction path (§2.7), so it is worth a
  guard before the daemon depends on it.
- A throwaway property check confirmed `split` never loses or reorders
  content across 200k generated inputs over the scanner's special characters
  (not kept: it is slow relative to what it protects, and the invariant it
  checks has no history of breaking).

**Tests:** 94 → 99; `fmt`, `clippy -D warnings`, and `cargo doc --no-deps
--workspace` all clean.

### Looked at, deliberately unchanged

- **`OpenAiTts::new` fails with `NoKey` before a voice can be listed.** The
  catalogue is a `const` that needs no credential, but the provider cannot be
  constructed without one, so `cosmo voice list` (§2.7) will report "no key"
  rather than showing OpenAI's voices. Not wrong today — nothing calls it
  yet — but §2.7 has to decide whether listing is a keyless operation. Flagged
  rather than redesigned, because the answer belongs with the CLI.
- **Markdown emphasis suppresses boundaries** (`**Done.** Next.` stays one
  chunk — `*` is not a closing character). Speaking raw markdown is its own
  problem and not one §2.10 claims to solve.
