# cosmo — implementation plan

**Derives from:** `docs/cosmo-blueprint.md` (v4)
**Scope:** build order from empty repo to packaged release. Phases follow blueprint §13; each is expanded into concrete, checkable steps.

---

## Invariants (apply from day one, reference in code comments)

These are load-bearing and easy to violate accidentally:

1. **Never bind `zwp_input_method_v2`.** One slot per seat; binding while IBus holds it wedges keyboard input session-wide on cosmic-comp. Virtual keyboard (`zwp_virtual_keyboard_v1`) only. Put a comment at the Wayland init site.
2. **Never register `run_shell`** from `computer-use-linux`. It stays absent (no `COMPUTER_USE_LINUX_ENABLE_SHELL`).
3. **The daemon owns the engine.** Applet and overlay are thin IPC clients. Do not "simplify" by moving engine state into the applet (blueprint §3.5).
4. **Policy gate properties** (blueprint §7): gated call + confirmation in the same model response → reject; confirmation only after a genuinely new user turn; confirm phrase matched as a whole utterance; local confirm path never asks the model.
5. **Reflex path is allowlist-only.** Nothing on deny/hold is reachable without the model.
6. **Hotkey grabs the keyboard and reads only the trigger keycode** via `EVIOCGRAB` + `EVIOCSMASK`, with `MSC_SCAN` filtered for the daemon's fd. No root, no `input` group (logind `uaccess` ACL).
7. **No live desktop state in the prompt.** Windows/workspaces come from tool calls. Static prompt < 3,000 tokens; log the server-reported rate limit every turn.
8. **Half-duplex by default.** Mic gated while speaking + ~350ms settle. Barge-in behind config with a `doctor` warning.

---

## Step 0 — environment prep

- [ ] Rust stable via rustup; `rust-toolchain.toml` pinned; `cargo clippy` and `rustfmt` configured workspace-wide.
- [ ] System packages (Fedora names; Pop!_OS equivalents):
  - `pkg-config`, `gcc`
  - `pipewire-devel`, `pipewire-alsa` (native PipeWire client)
  - `systemd-devel` (libudev)
  - `tmux` (terminal tools)
  - Wayland scanner / protocol headers as needed by `smithay-client-toolkit`, `libcosmic`
- [ ] Check out reference trees next to the repo (read-only, MIT):
  - `cosmic-voice` — steal from `hotkey.rs`, `audio.rs`, `vad.rs`; study the IBus multiplexer **as a cautionary tale**
  - `agent-sh/computer-use-linux` — tool list, `ToolAnnotations`, `computer-use-linux-cosmic` helper, doctor output
- [ ] API keys available for testing: OpenAI (reasoning + TTS). Optional: ElevenLabs.
- [ ] Empty Cargo workspace committed: root `Cargo.toml` with `workspace.dependencies` inheritance, all 13 crates from blueprint §12 as empty `lib.rs` stubs so the tree compiles from the first commit.
- [ ] CI from the first week: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.

---

## Phase 0 — feasibility spike (one evening) — **kill criterion gate**

Goal: prove the three riskiest assumptions before any real code.

- [ ] Install `computer-use-linux` on the real COSMIC session. Run `computer-use-linux doctor | jq .readiness`; confirm `can_query_windows`.
- [ ] Drive it over MCP (`rmcp` + `TokioChildProcess`, the exact snippet from blueprint §5): `list_windows`, activate, move-to-workspace. **Time all three.** Record numbers.
- [ ] Confirm the two GNOME backends don't shadow the COSMIC helper with timeouts (known slow-probe issue; note whether a cached-probe upstream PR is needed — blueprint §5).
- [ ] Tiny evdev test program: open `/dev/input/by-id/...` as the logged-in user (no root, no `input` group), `EVIOCGRAB` + `EVIOCSMASK` filtered to the trigger keycode, verify press **and release** events arrive, nothing else does, and the key does not leak to the focused app while held.
- [ ] Tiny layer-shell test (libcosmic or `smithay-client-toolkit`): one surface anchored bottom-center renders on cosmic-comp.
- [ ] Record findings in `docs/phase0-findings.md`: measured latencies, evdev device path semantics, layer-shell verdict.

**Kill criterion:** if COSMIC window control can't be made to work, stop the project here. Do not proceed on hope.

---

## Phase 1 — the spine (no audio; works on GNOME too)

Goal: text-in → gated tool calls → text-out. Most of the usefulness; the place where timing instrumentation gets built.

### 1.1 Config + IPC

- [ ] `~/.config/cosmo/config.ron`, written with commented defaults on first run (COSMIC convention). `ron` + `serde`.
- [ ] `cosmo-ipc`: Unix domain control socket at `$XDG_RUNTIME_DIR/cosmo.sock`. Types: `Command`, `Response`, and a broadcast `Event` stream (state changes, transcript partials, tool activity) — the overlay and applet will consume these later.
- [ ] `cosmo` binary skeleton: `cosmo status`, `cosmo doctor`, `cosmo confirm`, `cosmo toggle`, `cosmo say`. Unknown-socket exit codes documented.

### 1.2 Policy gate (build before the tools it guards)

- [ ] `cosmo-gate`: verdicts `Allow` / `Hold` / `Deny`.
  - Deny list: `rm -rf`, `dd`, `mkfs`, `sudo`, `pkexec`, `ssh`, `passwd`, curl-piped-to-shell, `git push`.
  - Hold: shutdown, reboot, suspend, package installs, config resets, close-everything.
  - Map MCP `ToolAnnotations`: `destructiveHint=true` → hold; read-only → allow.
  - Enforce all four gate invariants; each one gets a unit test (they are mechanical and highly testable — property tests where possible).
- [ ] Pending-hold queue with confirm tokens; `cosmo confirm <token>` resolves **locally, no model round trip**.

### 1.3 MCP host

- [ ] `cosmo-mcp` on `rmcp`: spawn `computer-use-linux mcp` via `TokioChildProcess`.
- [ ] Tool allowlist filter (~a dozen of its ~20 tools; hardcode the allowlist in config). Verify `run_shell` never registers.
- [ ] Capability discovery + graceful absence (agent not installed → `doctor` explains, nothing crashes).
- [ ] Native tools registered alongside: `run_in_terminal` / `read_terminal` / `watch_terminal` (tmux `capture-pane`, `pane_current_command`; shell-reappearing = done signal), `announce` (queue, ≥8s spacing, degrade to notification), `remember` (flat file), `system_query` (`df`/`ip`/`free`/`systemctl`/sensors — read-only allowlist, **no shell**), `clipboard` (`wl-clipboard-rs`, gated), `media_control` (MPRIS via `zbus`).
- [ ] `cosmo-type`: Wayland virtual keyboard (`zwp_virtual_keyboard_v1` via `wayland-protocols-misc`) with a synthesised keymap, for Unicode-safe text injection into terminals/fields. **Never** bind `zwp_input_method_v2` (invariant #1); add a comment at the protocol-init site so nobody "fixes" that later.
- [ ] Integration test against a fake stdio MCP server (scripted tool list + annotations) so gate mapping is testable without the real agent.

### 1.4 Reasoning (text first)

*Interpretation note:* the blueprint puts the Realtime WS client in `cosmo-reason` for phase 5, but `cosmo say` must execute real commands in phase 1. So: build `cosmo-reason` now against the **plain chat-completions API** (tool-calling, text in/out — trivial with `reqwest`), and upgrade it to the Realtime session in phase 5. Same prompt, same tool schemas.

- [ ] `cosmo-reason` v1: chat completions, tool loop, gate interposed on **every** tool call (MCP and native alike).
- [ ] Prompt skeleton honoring the token budget: static prompt < 3,000 tokens; **no desktop state in prompt**; log server-reported rate-limit/usage headers every turn.
- [ ] Daemon state machine with the six states already modeled (`Idle/Listening/Thinking/Acting/Waiting/Speaking`) and exported over IPC events — audio just fills them in later.

### 1.5 Definition of done

- [ ] `cosmo say "open the terminal and run htop"` → tmux tool → transcript of what happened, printed to the terminal.
- [ ] `cosmo say "shut the machine down"` → Hold → `cosmo confirm` completes it locally.
- [ ] A deny-listed request never executes, ever, under any confirmation phrasing (test suite).
- [ ] Every tool call logged with latency; `cosmo doctor` renders a readiness table.
- [ ] Works on GNOME (no COSMIC-specific code touched yet) — proves the portability claim early.

---

## Phase 2 — the voice layer (still no microphone)

Goal: pick your accent **before** the thing can hear you, because every later phase means listening to it.

- [ ] `cosmo-tts`: `VoiceProvider` trait exactly per blueprint §4 (`id`, `list_voices`, `synthesize`, `stream`, `is_local`, `latency_class`); `Voice` carries `accent` for grouped display.
- [ ] **Kokoro default provider** via `kokoros`/`kokoro-tiny` on `ort`. Measure time-to-first-audio on target hardware (expect 0.5–2s — this number justifies the phrase cache; record it).
- [ ] **Phrase cache**: on voice selection, render the canned reflex vocabulary to WAV under `~/.cache/cosmo/voice/<provider>/<voice-id>/` (`ack-moving`, `ack-focused`, `ack-launching`, `err-notfound`, `confirm-hold`). Switching voices re-renders in the background.
- [ ] Audio **output** path: PipeWire playback stream that can push either a cached WAV or fresh PCM. (Capture waits for phase 3; playback is needed now.)
- [ ] Piper provider as the weak-hardware fallback (subprocess is fine).
- [ ] OpenAI TTS provider (`gpt-4o-mini-tts`, `instructions` field) via `reqwest`.
- [ ] MeloTTS en-AU spike: one evening, generate samples, judge quality honestly before promising Australian (open question §16).
- [ ] `cosmo voice list / preview / set`. `cosmo say` now speaks its replies.
- [ ] Sentence-splitting helper for streaming (used in phase 5) with unit tests on punctuation/ellipsis cases.

---

## Phase 3 — ears

Goal: hold the key, see a transcript. Steal from cosmic-voice directly (`hotkey.rs`, `audio.rs`, `vad.rs` are close to liftable).

### 3.1 Audio capture

- [ ] `cosmo-audio`: **native** PipeWire client (not `pw-record`); callback on the RT data-loop.
- [ ] Continuous capture into a pre-roll ring buffer (~750ms+) so the first syllable survives key-press latency.
- [ ] VAD at the ring buffer: half-duplex gating (mic shut while speaking + 350ms settle), and pause-based **segment cuts** for long utterances (offline cost is super-linear in duration: RTF ~0.052 on a 30s clip vs ~0.093 on 120s — both faster than realtime, but efficiency drops as the buffer grows).

### 3.2 Hotkey

- [ ] `cosmo-hotkey`: evdev + `EVIOCGRAB` grab of the keyboard and `EVIOCSMASK` restricting this process to the trigger keycode (filtering `MSC_SCAN` for the daemon's fd). udev hotplug re-attach on replug. No root, no `input` group.
- [ ] Hold-to-talk on key press/release; `cosmo toggle` (COSMIC `Spawn` shortcut) as the press-only fallback.

### 3.3 STT

- [ ] `cosmo-stt` on `sherpa-onnx`, two resident int8 models:
  - **Streaming** (`nemotron-speech-streaming-en-0.6b` class) on `asr_threads`, ~560ms chunks → live partials (via IPC events).
  - **Offline** (`parakeet-tdt-0.6b-v2` or sibling English `parakeet-*` class, picked on `bench-asr` data) on `offline_threads` (cap at 4) → the committing transcript, with **hotword biasing** (this is the reflex unlock).
- [ ] **Segmented decoding**: decode each finished pause-delimited segment while the next is still being spoken.
- [ ] Hotword set = ~30 reflex phrases + installed app names, keyed by focused `app_id`.
- [ ] `scripts/bench-asr`: run your own command set against candidate models (open question §16 — command recognition may want a smaller streaming model and no offline pass; decide on data).
- [ ] Model fetch script into `~/.cache/cosmo/models/`.

### 3.4 Definition of done

- [ ] Hold key, speak, release → final transcript; partials visible meanwhile (notification or stdout until the overlay exists).
- [ ] Latency measured and logged; **the saturated-machine test**: compile something big while talking — no dropped or clipped audio.
- [ ] `doctor` verifies: uaccess ACL on the evdev node, PipeWire stream up, both models resident.

---

## Phase 4 — the reflex path — *where it starts feeling like Jarvis*

- [ ] `cosmo-reflex`: matcher over the ~30-phrase command vocabulary + app names; confidence scoring; normalization ("pause the music" / "stop the track").
- [ ] Escalation rule wired: below confidence threshold → hand transcript to reasoning (stubbed log if phase 5 incomplete); reflex matched but action **failed** → escalate rather than report failure.
- [ ] Reflex executes only **allowlisted safe verbs** (media control, focus/launch, workspace moves) — gate integration test asserts deny/hold verbs are unreachable from reflex.
- [ ] Cached-phrase acks on the reflex path: playback is pushing an existing buffer — zero synthesis. **Target < 150ms**; uncached Kokoro replies **< 400ms**.
- [ ] `scripts/bench-reflex`: end-to-end timing harness (utterance audio fixture → action dispatched → ack started) so the 150ms budget is measured, not vibes.
- [ ] Wake-independent correctness: works identically from `cosmo say` text input (testable without audio).

---

## Phase 5 — reasoning voice

- [ ] `cosmo-reason` v2: **Realtime API** over `tokio-tungstenite`, **text-out** (no model audio — voice catalogue has no en-GB/en-AU, and marin/cedar ignore session instructions anyway). Reasoning + tool calls only; cosmo speaks.
- [ ] **Sentence-streamed TTS**: synthesize each sentence as it streams in; first audio in a few hundred ms.
- [ ] Half-duplex enforcement on the live path; barge-in behind config with `doctor` warning when mic+speakers coexist.
- [ ] Confirmation flow end to end: Hold → overlay/CLI confirm (local, no model) → execute. Forgeable-spoken-confirmation **rejected** (a spoken "confirm that" alone never completes a hold).
- [ ] Token discipline verified live: log per-turn token usage + server rate limits; confirm reflex-path commands consume zero tokens.
- [ ] `remember` memory loaded into each session's static prompt within budget.

---

## Phase 6 — the overlay and applet

- [ ] Path decision (blueprint §9 order): **libcosmic layer surface** first — same toolkit as the applet, COSMIC theming free. Fall back to raw `smithay-client-toolkit` + `wl_shm` + `tiny-skia` + `cosmic-text` if libcosmic fights the layer-shell use case (cosmic-voice's candidate window proves the raw path).
- [ ] States, in build order: **listening** (waveform + live streaming partial) → **thinking** → **acting** (tool name in plain words) → **waiting** (pending action + confirm affordance) → **speaking** → **idle**. All driven by IPC events the daemon already emits.
- [ ] Anchor bottom-center; no decorations; no focus steal. GNOME degradation → notifications.
- [ ] Redraw discipline: coalesce twice — across each event burst and on `wl_surface.frame`. One input change = at most one buffer commit.
- [ ] Voice picker with previews and background re-render progress.
- [ ] `cosmo-applet`: thin libcosmic panel applet over the control socket. State the multiplicity invariant in a crate-level comment: **the panel spawns one applet process per output; the daemon owns the mic/hotkey/models, always.**
- [ ] Parked, not forgotten: Kokoro voice-blending slider (blueprint §4) — only after the picker ships.

---

## Phase 7 — wake word

- [ ] `openWakeWord` via `ort` on the existing ring buffer (nothing leaves the machine until the phrase fires).
- [ ] Wake → **reflex first** ("Cosmo, pause the music" = zero network calls), reasoning only on escalation.
- [ ] False-accept hygiene (open question §16): threshold tuning on real days of audio; reflex stays allowlist-only so a false accept can't reach a gated action; `doctor` reports wake stats.
- [ ] evdev trigger remains the deterministic fallback, always.

---

## Phase 8 — packaging and release

- [ ] `packaging/`: systemd **user** unit for the daemon (WantedBy `graphical-session.target`); autostart consideration for overlay/applet.
- [ ] Model files: first-run fetch into `~/.cache/cosmo/models/` with checksums + progress (don't bloat packages; do ship config).
- [ ] COPR spec (Fedora) and deb (Pop!_OS). Rust static-ish binary + models is the packaging argument; keep it true (audit dynamic deps: onnxruntime, pipewire, wayland).
- [ ] `cosmo doctor` final form: uaccess ACL, PipeWire, models present, layer-shell support, lock-screen fail-closed check, barge-in warning, token/rate-limit display.
- [ ] README with the positioning statement (§14) and attribution: `omarchy-voice`, `cosmic-voice`, `computer-use-linux`, Kokoro-82M.
- [ ] Release smoke test on a clean COSMIC VM/user account: install → `doctor` green → `say` → voice → hotkey, with no manual group membership or root.

---

## Cross-cutting (start in step 0, maintain throughout)

| Concern | Practice |
|---|---|
| Latency | `tracing` spans around every hop; reflex/reasoning budgets asserted in `scripts/bench-*`; numbers recorded per phase in `docs/phase{n}-findings.md` |
| Testing | Gate properties unit/property-tested; fake MCP server integration; recorded audio fixtures for STT/VAD; nested-compositor harness in `scripts/` for overlay |
| Security | Invariants section above; no secrets in logs; clipboard gated; lock-screen detection **fails closed** when lock state is undetermined |
| Upstream | Cached-probe PR to `computer-use-linux` if phase 0 flags it; consider extracting `cosmo-hotkey` as a shared crate (open question §16) |
| Open questions | Track §16 items as issues at repo creation; close each with a measurement, not an opinion |

## Dependency chain (what blocks what)

```
Step 0 ─→ Phase 0 ─┬─→ Phase 1 (spine) ─┬─→ Phase 2 (TTS) ─→ Phase 4 (reflex) ─→ Phase 5 (voice reasoning) ─→ Phase 6 (overlay)
                   │                     └─→ Phase 3 (ears) ──┘                                        │
                   └─ kill gate                                                                        └─→ Phase 7 (wake) ─→ Phase 8 (packaging)
```

Phase 2 and phase 3 are independent of each other and can interleave once phase 1 lands; the overlay (6) only needs the IPC event stream to exist and can be started alongside 4–5 for earlier visual feedback if desired.
