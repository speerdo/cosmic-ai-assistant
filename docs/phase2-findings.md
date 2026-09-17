# Phase 2 findings

**Started:** 2026-09-17
**Scope so far:** §2.0 audited (install pending), §2.2 core types done. §2.1
link spike is next and is blocked on §0's install.

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
run there. Until it does, §2.1 (`ort`/`koko`/`pipewire` link spike) cannot
build — §2.2 was deliberately native-free and shipped anyway:

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

**Tests:** 67 green across the workspace (56 at the phase-1 wrap-up, minus
the two redaction tests that moved with `SecretKey`, plus ten new voice-layer
ones); `fmt` and `clippy -D warnings` clean. No native dependency entered
the build — spec §2.2's DoD holds.
