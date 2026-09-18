# cosmo — phase 2 spec: the voice layer in ten parts

**Derives from:** `docs/implementation-plan.md` §Phase 2, `docs/cosmo-blueprint.md` §4 (voice layer), §8 (audio discipline)
**Drafted:** 2026-09-17
**Status:** §2.0 and §2.1 done (2026-09-18); §2.2, §2.4 (provider half) and
§2.10 done and reviewed (2026-09-17; eight defects fixed across two review
passes, findings §R and §R2). §2.3 playback is next. §2.5 is unblocked on
`ort` but needs a Kokoro-crate decision first (findings §1f). Everything
else not started.

Phase 2 looks like one phase but is five different kinds of work: unproven
native-stack risk (`ort`, `koko`, `pipewire` — declared in
`workspace.dependencies` since step 0 and **never resolved or compiled**),
a pure-Rust trait surface, an audio output path, several providers of very
different shapes, and CLI/daemon glue. Each part below is sized to land in
one sitting with its own DoD, ordered so the native risk cannot block the
vertical slice.

## Ordering deviation from the plan (deliberate — flag for review)

The plan's checklist implies Kokoro first among providers. This spec builds
**OpenAI TTS first** (2.4): it is the only provider with zero new native
dependencies, and phase 1's key infrastructure already resolves its
credential. So trait → provider → playback → "`cosmo say` speaks" closes as
a complete vertical slice while the `ort` link verdict (2.1) is still
pending. Kokoro remains the **default** provider once it lands (2.5); only
build order changes, not precedence.

One honest limit on that claim: the *provider* needs no native dependency,
but the playback it speaks through (2.3) links `pipewire`, so the slice is
free of **2.1**, not of **2.0**. Everything in 2.4 except its final
checkbox can be built and tested before the apt install; the DoD itself
cannot close without it.

## Parts

### 2.0 System packages (30 min)

The step-0 leftovers that phase 2 actually needs. Nothing else in this spec
builds without them.

*(2026-09-17 audited, 2026-09-18 installed. `pkg-config --exists
libpipewire-0.3` succeeds — DoD met. §2.1 and §2.3 are unblocked, and with
them the path to §2.4's "say speaks". Versions in
`docs/phase2-findings.md` §0.)*

- [x] `sudo apt install clang libclang-dev cmake libpipewire-0.3-dev`
      (build-essential, pkg-config, libwayland-dev already present, verified
      2026-09-10). *(clang 18.1.3, cmake 3.28.3, libpipewire-0.3 1.6.8)*
- [x] Record installed versions in `docs/phase2-findings.md`.

**DoD:** `pkg-config --exists libpipewire-0.3 && echo ok` succeeds; findings
file exists with a §0.

### 2.1 Link spike — half a day, gate for 2.5

The plan is explicit: prove the link **before** designing around these
crates. `ort 2.0.0-rc.13` is a release candidate; if it fights, that is a
day-one fact.

- [x] A scratch example (feature-gated target or `examples/` in
      `cosmo-tts`/`cosmo-audio`) that consumes `ort`, `koko`, and `pipewire`
      and does nothing but link. Time the cold build. *(two examples rather
      than one — `link_pipewire` in cosmo-audio, `link_ort` in cosmo-tts —
      so each native dep sits in the crate that will really own it. Both run
      live. `koko` is absent because it does not exist: findings §1a.)*
- [x] Record in findings: onnxruntime acquisition model (bundled download
      vs. system `libonnxruntime`), whether `clang`/`cmake` were actually
      exercised, cold build time, added binary size. *(prebuilt static, 101 MB
      cached, no system lib; clang yes, **cmake no**; 8.5 s / 12.6 s cold;
      +22 MB stripped. Findings §1b–§1e.)*
- [~] Choose the Kokoro crate — `koko` vs. `kokoroxide` vs. `kokoro-tiny` —
      on API shape against `VoiceProvider`: does it expose voice listing,
      raw f32 PCM, and (later, parked) style blending? *(`koko` is an
      unrelated crate and `kokoros` is unpublished; `kokoroxide` is
      disqualified on an incompatible `ort` major. `kokoro-tiny` is the
      recommendation but costs `libssl-dev`, a second reqwest and an
      advisory-carrying `atty` — **open decision**, findings §1f.)*
- [x] Decide the CI two-tier shape now (cross-cutting table assigns this to
      the link spike): a core tier that always builds, and a heavy tier
      (feature-gated or container-installed) for native-dep crates. Write
      the decision down; CI is not made red by this in phase 2 either way.
      *(every native dep behind a non-default feature; core tier verified to
      contain no native crate. Findings §1g.)*

**DoD:** numbers + crate choice + CI decision in `docs/phase2-findings.md`
§1. If `ort` fights badly, 2.5 re-plans here — nothing downstream of 2.4 is
blocked meanwhile. *(done 2026-09-18. `ort` did **not** fight — one feature
swap for TLS and it committed a live environment. The re-plan §2.5 needs is
for the Kokoro crate, not for `ort`.)*

### 2.2 cosmo-tts core types (no native deps)

The trait surface everything else implements. Buildable and testable with
zero native dependencies, so it can land before or alongside 2.1.

- [x] `VoiceProvider` exactly per blueprint §4: `id`, `list_voices`,
      `synthesize`, `stream`, `is_local`, `latency_class`. *(one recorded
      deviation: `synthesize` returns a boxed future — providers want the
      daemon's runtime and the registry needs `dyn`; findings §2 item 1)*
- [x] `Voice { id, label, accent, gender, sample }` — `accent` is the field
      the picker groups by (`en-US` / `en-GB` / `en-AU` / `en-IE`). *(`Accent`
      is a normalized newtype, not an enum — MeloTTS ships `en-IN`, Piper
      more; findings §2 item 5)*
- [x] Canonical `Pcm` (decide f32 mono + sample rate now; WAV encode/decode
      helpers live beside it — the phrase cache stores WAV). *(`hound`,
      pure Rust; decode accepts int 8–32-bit and float, collapses to mono;
      findings §2 item 3)*
- [x] `stream` gets a default implementation that buffers text and calls
      `synthesize`; phase 5 replaces it with real sentence streaming. Trait
      shape settles now, behavior later. *(maps each chunk in order; tested)*
- [x] Provider registry (name → constructor); unknown name is a structured
      error, not a panic. *(`ProviderInit` carries the §1.4 key type — moved
      to `cosmo-config::secret`, re-exported by `cosmo-reason` so tts never
      depends on reason; findings §2 item 2)*
- [x] Config: `voice.provider` / `voice.voice_id` in `config.ron`, commented
      defaults updated on first run, **no credentials in config** (existing
      §1.4 invariant — the config may name a provider, never a key).
      *(shipped as `voice_provider`/`voice_id`, defaults `openai`/`default`
      — kokoro becomes the default when §2.5 lands; findings §2 item 4)*
- [x] Unit tests: WAV round-trip, registry errors, defaults.

**DoD:** `cargo test -p cosmo-tts` green; the crate's dependency graph
contains no native build. *(done 2026-09-17 — 71 green workspace-wide after
the review pass; fmt, clippy, and `cargo doc --no-deps --workspace` clean.
The review found five defects on these ticked boxes, three of them in the
WAV codec: findings §R.)*

### 2.3 Playback path (cosmo-audio, output only)

~~Blocked by 2.0~~ — unblocked 2026-09-18. The
crate's doc-comment invariants describe capture — that stays phase 3. This
part delivers the speaker half only.

- [ ] Native PipeWire playback stream (not `pw-play`): open at the buffer's
      own rate and let PipeWire resample; push a `Pcm`/WAV buffer; block or
      track completion.
- [ ] Example binary: generated sine sweep, then a WAV file, audible on the
      real session.
- [ ] Half-duplex: nothing to enforce yet (no mic until phase 3) — leave the
      gating hook and a comment at the stream site per invariant #8, so
      phase 3 doesn't have to find the seam.

**DoD:** `cargo run -p cosmo-audio --example play_sine` is audible; a
daemon-usable "enqueue speech buffer" API exists (daemon wiring itself is
2.7).

### 2.4 First provider: OpenAI TTS

The vertical slice. Network, `reqwest`, no new native deps.

- [x] `gpt-4o-mini-tts` with the `instructions` field sourced from config
      (affect/tone/pacing); response format chosen for decode simplicity
      (`wav` with a parseable header, or raw `pcm` at a known rate — decide
      against the canonical `Pcm` from 2.2). *(`wav` through the existing
      decoder; `voice_model`/`voice_instructions` config keys, empty
      instructions omitted from the request; findings §4 item 1)*
- [x] Key resolution reuses the phase-1 source exactly: env → Secret Service
      → structured error. Same credential as chat; no new auth surface.
      *(the provider takes the resolved key in `ProviderInit.api_key` — the
      daemon-side resolution that feeds it is §2.7)*
- [x] Errors follow the §1.4 three-state discipline (no key / network /
      rate limit) so `doctor` and the CLI render actionable fixes — not
      "TTS broken". *(`NoKey` checked before any network, `Network` on
      transport, `RateLimited` for 429, `Synthesis` otherwise; findings
      §4 item 3)*
- [x] `latency_class() = Network`, `is_local() = false`.
- [x] Tests: request/response shape against a fake HTTP server, same harness
      style as phase 1's fake chat-completions server.
- [ ] Daemon: after a completed turn, speak the reply through 2.3's
      playback. `Speaking` state (reserved since phase 1) becomes real.
      *(provider done; this box waits on §2.3, which is blocked on §2.0's
      apt install)*

**DoD:** with provider=openai and a key, `cosmo say "..."` **speaks its
reply** — the phase-2 headline DoD, achieved with zero new native
dependencies. Cross-ref carry-over E1: the same real-key run that closes
E1 exercises this end to end. *(provider half done 2026-09-17 — exercised
end to end against a fake speech server, then reviewed: a stalled server
hung the speak path forever, now a 30s timeout surfacing as `Network`
(findings §R2). The "speaks" half is §2.3 + the daemon box above, both
downstream of §2.0. findings §4.)*

### 2.5 Kokoro provider — the default

Blocked by 2.1's verdict — **which came back needing a decision** (findings
§1f): no published Kokoro crate is a clean fit. Pick one of (a) `kokoro-tiny`
with `default-features = false`, accepting `libssl-dev`, a second `reqwest`
major and an advisory-carrying `atty`; or (b) drive the ONNX model through
`ort` directly with `espeak-rs` for phonemes, which §2.1 has already proven
links. Otherwise self-contained.

- [ ] Implement `VoiceProvider` on the crate chosen in 2.1; enumerate
      voices from the model's voice pack with real `accent` values
      (`af_`/`am_` → en-US, `bf_`/`bm_` → en-GB).
- [ ] `scripts/fetch-models`: download model + voice files into
      `~/.cache/cosmo/models/` with checksums. Script only — the first-run
      fetch UX is phase 8; this is its groundwork.
- [ ] **Measure time-to-first-audio** on target hardware and record it.
      Expected 0.5–2s; this number is the justification for the phrase
      cache (2.6), so it gets measured, not assumed.
- [ ] Make it the commented-default provider in config.

**DoD:** `voice list` shows Kokoro voices grouped by accent; `say` speaks
locally with no key in the environment; TTFA number in findings.

### 2.6 Phrase cache

Blocked by 2.5 (a local provider is what makes the cache load-bearing).

- [ ] On voice selection, synthesize the reflex vocabulary to WAV under
      `~/.cache/cosmo/voice/<provider>/<voice-id>/` (`ack-moving`,
      `ack-focused`, `ack-launching`, `err-notfound`, `confirm-hold`).
- [ ] The vocabulary is a **list, not an enum hardcoded at the playback
      site** — phase 4 owns the final phrase set; the cache mechanism must
      not have to change when it grows.
- [ ] Voice switch re-renders in the background; progress flows over the
      existing IPC event stream (phase 1 already emits events; the overlay
      will consume this in phase 6).
- [ ] Staleness handling: re-render when phrase text, provider, or voice
      changes (content hash or mtime); a cache dir killed mid-render is
      repaired on next start.

**DoD:** switch voice → instant acks from the old cache while re-render
progress streams; kill the daemon mid-render → next start completes it.

### 2.7 Voice CLI + daemon wiring

Blocked by 2.3 + (2.4 or 2.5).

- [ ] `cosmo voice list / preview / set`, over the control socket — the
      daemon owns playback; the CLI never links audio (invariant #3).
- [ ] `preview` speaks a fixed sample line in the chosen voice (synthesized
      once, cached) — not a bundled WAV, so the preview is always honest.
- [ ] `set` persists to `config.ron` and triggers 2.6's re-render.
- [ ] `announce` (phase 1) upgrades from notification-only to speech when
      voice is live; the ≥8s spacing and queue already exist.
- [ ] `doctor` additions: provider, resolved voice, cache state, last
      measured TTFA.

**DoD:** pick a voice, hear the preview, `say` answers in that voice;
`cosmo status` shows `Speaking` during playback.

### 2.8 Piper fallback provider

Independent; can slip without hurting anything.

- [ ] Subprocess provider (`piper` binary), voices discovered from its
      voices directory; `latency_class() = Fast`.
- [ ] Absence is graceful: `doctor` explains, nothing crashes — same
      discipline as the MCP agent's graceful absence in 1.3.

**DoD:** with `piper` installed, list/preview/set work; with it absent, the
rest of phase 2 is unaffected.

### 2.9 MeloTTS en-AU spike — one evening, decision-gated

Independent; the deliverable is a verdict, not a feature.

- [ ] Generate en-AU samples with MeloTTS; listen honestly; record the
      verdict in findings and close (or keep open, with reason) the §16
      open question.
- [ ] If quality fails: en-AU waits for ElevenLabs (opt-in, paid) and the
      docs say so. **Do not ship "Australian" on an unverified voice.**

### 2.10 Sentence-splitting helper

Independent, pure, cheap. Built now so phase 5's streaming API settles
early.

- [x] Text → sentence boundaries, handling `.`/`!`/`?`, ellipsis,
      abbreviations (Mr., e.g., vs.), decimals, quotes, brackets.
      *(strong boundaries: terminator runs, `…`, bare `!`/`?` in whitespace;
      weak `.`: sentence-start required, minus the abbreviation list and
      initials; closing-quote interiors stay attached; blank lines split;
      findings §10)*
- [x] Unit tests on the awkward corpus; the list of known-abbreviations is
      data, not code. *(fourteen cases; `may` and `no` deliberately absent
      from the list — both are real sentence-final words)*

**DoD:** tests green; nothing depends on it yet — that is fine. *(done
2026-09-17, then reviewed: ordered-list markers were being split into their
own spoken sentences and `etc` was wrongly on the abbreviation list —
findings §R2. Phase 5 consumes it.)*

## Not in phase 2 (so nobody scope-creeps it)

- **Microphone, VAD, ring buffer, half-duplex enforcement** — phase 3;
  2.3 leaves the gating hook.
- **Sentence-streamed synthesis itself** — phase 5; only the splitter
  (2.10) and the trait shape (2.2) are built here.
- **Voice blending slider** — parked (blueprint §4).
- **Overlay voice picker** — phase 6; the CLI picker here is its backend.
- **ElevenLabs provider** — opt-in, paid, not in the plan's phase-2
  checklist; stays out until asked for.
- **systemd unit, first-run fetch UX** — phase 8; 2.5's fetch *script* is
  the groundwork.

## Cross-cutting, decided now

- **Findings:** `docs/phase2-findings.md`, §-numbered per part, house
  style. Numbers (TTFA, cold build time) recorded there, not in commit
  messages.
- **Bench spans:** phase 1 agreed span names at the hops that
  `scripts/bench-*` will parse. Phase 2 adds the speak hops now:
  `speak/synthesize`, `speak/first_audio`, `speak/push`. A `bench-tts`
  script is not built this phase — phase 4's `bench-reflex` times the
  cached-phrase path that matters; TTFA here is measured once, by span.
- **Carry-over E1:** the first real-key run should happen against a tree
  that includes 2.4, so one unlocked keyring observes both the reasoning
  round trip (E1) and spoken replies.

## Rollup to the plan's phase-2 checkboxes

| Plan item (implementation-plan.md) | Parts |
|---|---|
| Link spike first | 2.0, 2.1 |
| `VoiceProvider` trait + `Voice.accent` | 2.2 |
| Kokoro default provider, measure TTFA | 2.5 |
| Phrase cache + background re-render | 2.6 |
| Audio output path (PipeWire playback) | 2.3 |
| Piper fallback | 2.8 |
| OpenAI TTS provider | 2.4 |
| MeloTTS en-AU spike | 2.9 |
| `cosmo voice list/preview/set`; `say` speaks | 2.4, 2.7 |
| Sentence-splitting helper | 2.10 |

## Dependency order

```
2.0 ✓ ─────────┬─→ 2.1 ✓ ───────────→ 2.5 Kokoro ──→ 2.6 phrase cache ──┐
               └─→ 2.3 playback ──┐                                    ├─→ 2.7 CLI + wiring
2.2 trait/types ──────────────────┴─→ 2.4 OpenAI TTS ───────────────────┘
anytime, independent: 2.8 Piper · 2.9 MeloTTS spike · 2.10 splitter
```

2.2 and 2.10 can start immediately with no system packages. The first
sitting of real work should be 2.0 + 2.2 together; 2.1 runs next as its own
sitting so its verdict gates 2.5 without gating anything else.
