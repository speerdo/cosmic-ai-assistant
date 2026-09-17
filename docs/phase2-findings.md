# Phase 2 findings

**Started:** 2026-09-17
**Scope so far:** §2.0 audited (install pending), §2.2 core types done and
then reviewed (§R — five defects fixed), §2.4 provider + §2.10 splitter
done (§4, §10). §2.1 link spike and §2.3 playback are still blocked on
§0's install.

## §0. System packages (spec part 2.0) — audit done, install pending

Checked 2026-09-17:

| Package | State |
|---|---|
| `pkg-config` | present (1.8.1) |
| `build-essential`, `libwayland-dev`, `wayland-protocols` | present (phase-1 subset, 2026-09-10) |
| `clang`, `libclang-dev` | **missing** |
| `cmake` | **missing** |
| `libpipewire-0.3-dev` | **missing** |

The working session has no passwordless sudo, so the install could not be
run there. Until it does, **two parts are blocked, not one**: §2.1 (the
`ort`/`koko`/`pipewire` link spike) and §2.3 (the PipeWire playback path,
whose `pipewire` crate binds through `libclang`). Because §2.4's DoD is
"`cosmo say` *speaks*", the phase-2 headline DoD is downstream of this
install too — the OpenAI *provider* needs no native dependency, but the
speaker it plays through does. §2.2 was deliberately native-free and
shipped anyway:

    sudo apt install clang libclang-dev cmake libpipewire-0.3-dev

Installed versions get recorded here once it has run.

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
