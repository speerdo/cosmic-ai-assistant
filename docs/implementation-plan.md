# cosmo — implementation plan

**Derives from:** `docs/cosmo-blueprint.md` (v4)
**Scope:** build order from empty repo to packaged release. Phases follow blueprint §13; each is expanded into concrete, checkable steps.

## Where we are (2026-09-10)

| | Status |
|---|---|
| **Step 0** | Mostly done — workspace of 15 crate stubs, pinned toolchain, CI green (`fmt`/`clippy -D warnings`/`test`), reference trees checked out, phase-1 system packages installed except `libxkbcommon-dev`. Outstanding: `libxkbcommon-dev` (needed in §1.3), `--locked` in CI. API key storage is now **decided** (§1.4) and implemented during phase 1. |
| **Phase 0** | **COMPLETE — kill criterion PASS.** MCP timings, GNOME-shadowing, layer shell, and evdev hold-to-talk all verified on real hardware (`docs/phase0-findings.md`). Trigger *keycode* choice deliberately left open (findings §7). |
| **Phase 1** | Not started. Unblocked — this is next. |
| Phases 2–8 | Not started. |

No production code exists yet: every crate is a documented stub, and the only real code is the three phase-0 probes under `crates/*/examples/`.

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
9. **`focused_window` from the MCP agent is advisory only.** It is silently broken on COSMIC (findings §3 item 1: returns `null`, and `list_windows` reports `focused: false` / `workspace: null` for every window). Focus and workspace state come from compositor protocols. Never build focus logic on the agent's answer.
10. **Lock-screen checks fail closed.** Screenshots, clicks, typing, and clipboard reads refuse when the session is locked *or* when lock state cannot be determined. This is a gate property from phase 1, not a `doctor` line item in phase 8.

---

## Step 0 — environment prep

Dev box is **Pop!_OS 24.04**, so apt names are primary here; Fedora names follow for the COPR work in phase 8.

- [x] Rust stable via rustup; `rust-toolchain.toml` pinned (1.94.0, edition 2024); `cargo clippy` and `rustfmt` configured workspace-wide.
- [x] Empty Cargo workspace committed: root `Cargo.toml` with `workspace.dependencies` inheritance, all **15** crates from blueprint §12 as `lib.rs` stubs carrying their invariants as doc comments, so the tree compiles from the first commit.
- [x] CI from the first week: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`.
- [x] Check out reference trees next to the repo (read-only, MIT): `../cosmic-voice` (steal `hotkey.rs`, `audio.rs`, `vad.rs`; study the IBus multiplexer **as a cautionary tale**) and `../computer-use-linux` (tool list, `ToolAnnotations`, `computer-use-linux-cosmic` helper, doctor output).
- [ ] **System packages — not yet installed** (only `libwayland-dev` and `pkg-config` are present; verified 2026-09-10):

  | Need | Pop!_OS / apt | Fedora | Phase first needed |
  |---|---|---|---|
  | build basics | `pkg-config`, `build-essential` | `pkg-config`, `gcc` | step 0 |
  | tmux terminal tools | `tmux` | `tmux` | **1** (`run_in_terminal`) |
  | Wayland client + protocols | `libwayland-dev`, `wayland-protocols` | `wayland-devel`, `wayland-protocols-devel` | 1 (`cosmo-type`) |
  | keymap synthesis | `libxkbcommon-dev` | `libxkbcommon-devel` | 1 (`cosmo-type`) |
  | bindgen / native builds for `ort` + `sherpa-onnx` | `clang`, `libclang-dev`, `cmake` | `clang`, `clang-devel`, `cmake` | 2 |
  | native PipeWire client | `libpipewire-0.3-dev` | `pipewire-devel`, `pipewire-alsa` | 2 (playback) / 3 (capture) |
  | libudev (hotplug) | `libudev-dev` | `systemd-devel` | 3 |

  One-liner for the phase-1 subset: `sudo apt install build-essential pkg-config tmux libwayland-dev wayland-protocols libxkbcommon-dev`.
- [ ] API keys for testing: OpenAI (reasoning + TTS); optional ElevenLabs. **Not yet in the environment.** Decide the mechanism alongside §1.4 (env var vs. config vs. secret service) rather than exporting ad hoc.
- [ ] CI uses `--locked` so the committed `Cargo.lock` is actually load-bearing.
- [ ] **Dependency pins verified against crates.io** (2026-09-10): every version in `workspace.dependencies` is the current release — `ort 2.0.0-rc.13`, `sherpa-onnx 1.13.7`, `koko 0.2.0`, `pipewire 0.10.1`, `evdev 0.13.2`, `rmcp 3.3.0`, `reqwest 0.13.5`, `tokio 1.53.1`, `cosmic-text 0.19`, `tiny-skia 0.12`, `zbus 5.19`, `clap 4.6`, `ron 0.12`. **But none of them are in `Cargo.lock` yet** — no crate consumes them, so nothing has ever resolved or linked. See the link spikes at the head of phases 2 and 3.

---

## Phase 0 — feasibility spike (one evening) — **kill criterion gate**

Goal: prove the three riskiest assumptions before any real code.

- [x] Install `computer-use-linux` on the real COSMIC session. Run `computer-use-linux doctor | jq .readiness`; confirm `can_query_windows`.
- [x] Drive it over MCP (`rmcp` + `TokioChildProcess`, the exact snippet from blueprint §5): `list_windows`, activate, move-to-workspace. **Time all three.** Record numbers. (Timings in `docs/phase0-findings.md` §2. Caveats: activation only verified on the already-focused window; **no workspace-move tool exists** — fallback decided in findings §3.)
- [x] Confirm the two GNOME backends don't shadow the COSMIC helper with timeouts (known slow-probe issue; note whether a cached-probe upstream PR is needed — blueprint §5). (They don't; no upstream PR needed — findings §2.)
- [x] Tiny evdev test program written: `crates/cosmo-hotkey/examples/phase0_evdev.rs` (`--list` / `--capture` / `--code N --secs N`). Devices open as the logged-in user with **no root and no `input` group** — uaccess ACLs confirmed for open + `EVIOCGRAB` + `EVIOCSMASK`.
- [x] **Trigger verification RUN — PASS** (findings §5 item 1): press+release edges both arrive, `EVIOCGRAB` acquires/releases on the edges, and **0 non-trigger events** were delivered across a 30s window including a 2.66s hold. No root, no `input` group. Two follow-ons recorded: `cosmo-hotkey` must ignore `value==2` autorepeats (38 of them in one hold), and the final trigger keycode is still open (findings §7 — F9 was a discovery artifact and collides with common dev bindings).
- [x] Tiny layer-shell test (libcosmic or `smithay-client-toolkit`): one surface anchored bottom-center renders on cosmic-comp. (PASS — `crates/cosmo-overlay/examples/phase0_layer.rs`, raw SCTK.)
- [x] Record findings in `docs/phase0-findings.md`: measured latencies, evdev device path semantics, layer-shell verdict.

**Kill criterion: PASS** (findings §6) — window listing and activation work on COSMIC at 35–45 ms per op against a 150 ms ack budget, and evdev hold-to-talk is proven on real hardware. **Phase 0 is complete; phase 1 is unblocked.**

Phase 0 also produced three changes to the plan's assumptions, folded into invariants #9–#10 and §1.3 below: `focused_window` is silently broken on COSMIC, there is no workspace-move tool in the agent at all, and the feared cached-probe upstream PR is **not needed** (that open question is closed).

---

## Phase 1 — the spine (no audio; works on GNOME too)

Goal: text-in → gated tool calls → text-out. Most of the usefulness; the place where timing instrumentation gets built.

### 1.1 Config + IPC

- [ ] `~/.config/cosmo/config.ron`, written with commented defaults on first run (COSMIC convention). `ron` + `serde`.
- [ ] `cosmo-ipc`: Unix domain control socket at `$XDG_RUNTIME_DIR/cosmo.sock`. Types: `Command`, `Response`, and a broadcast `Event` stream (state changes, transcript partials, tool activity) — the overlay and applet will consume these later.
- [ ] `cosmo` binary skeleton: `cosmo status`, `cosmo doctor`, `cosmo confirm`, `cosmo toggle`, `cosmo say`. Unknown-socket exit codes documented.
- [ ] **The daemon process — decided, 2026-09-10.** Two binaries, not one:
  - `cosmo-daemon` gets `[[bin]] name = "cosmod"`. `cosmo-cli` keeps `[[bin]] name = "cosmo"`.
  - **Why two:** the daemon links the heavy native tree (onnxruntime, PipeWire, Wayland, sherpa) from phase 2 onward. Folding it into the CLI would make `cosmo status` drag all of that into its link graph and its startup. Two binaries also give the phase-8 systemd unit a stable `ExecStart` and match the crate split already in blueprint §12.
  - **The CLI never auto-spawns the daemon.** A process that owns the microphone starts explicitly. When the socket is absent, `cosmo` exits non-zero with: `daemon not running — start it with 'systemctl --user start cosmo' or run 'cosmod' in a terminal`.
  - Dev loop: `RUST_LOG=debug cargo run --bin cosmod` in one terminal, `cargo run --bin cosmo -- say "..."` in another. The systemd unit is phase 8; do not build it now.
- [ ] Socket lifecycle: single-instance guard, **stale-socket cleanup** after an unclean exit (a leftover `cosmo.sock` must not brick startup), `SIGTERM` shutdown that unlinks it, and a client-side "daemon not running" path distinct from "daemon refused".
- [ ] Observability bootstrap: `tracing-subscriber` with `RUST_LOG`, and the span names that `scripts/bench-*` will parse later agreed **now** (one span per hop: transcript → gate → tool → ack). Retrofitting span names after two benchmarks exist is the expensive version of this.

### 1.2 Policy gate (build before the tools it guards)

The gate has **two distinct surfaces**, and conflating them is the easy way to ship a hole. Make them separate code paths with separate tests:

- [ ] **Surface A — tool identity.** Every call is gated on tool name + `ToolAnnotations`: `destructiveHint=true` → hold, read-only → allow. Annotations are *hints*, so the gate must be able to be stricter than them (findings §3 item 4: `activate_window` is annotated non-destructive, but stealing focus mid-typing is not "safe"). Hold: shutdown, reboot, suspend, package installs, config resets, close-everything.
- [ ] **Surface B — command strings.** The deny list (`rm -rf`, `dd`, `mkfs`, `sudo`, `pkexec`, `ssh`, `passwd`, curl-piped-to-shell, `git push`) only ever sees a string because `run_shell` is absent (invariant #2) — so the **only** string-bearing surface is cosmo's own `run_in_terminal`, plus anything `cosmo-type` injects into a terminal. Attach the matcher explicitly to those, and assert in a test that a new string-bearing tool cannot be registered without passing through it. A deny list wired only to Surface A would match nothing and look like it worked.
- [ ] Enforce all four gate invariants; each gets a unit test (mechanical and highly testable — property tests where possible).
- [ ] **Lock-screen fail-closed** (invariant #10): screenshot / click / type / clipboard refuse when locked or when lock state is unknown. Needs a COSMIC lock-detection mechanism first — investigate `cosmic-idle` / logind `LockedHint` / the session D-Bus property, and record which one is authoritative in `docs/phase1-findings.md`. **If none is reliable, the gate denies those tools outright** rather than guessing; that is the fail-closed behavior, not a bug.
- [ ] Pending-hold queue with confirm tokens; `cosmo confirm <token>` resolves **locally, no model round trip**.

### 1.3 MCP host

- [ ] `cosmo-mcp` on `rmcp`: spawn `computer-use-linux mcp` via `TokioChildProcess`.
- [ ] Tool allowlist filter (~a dozen of its ~20 tools; hardcode the allowlist in config). Verify `run_shell` never registers.
- [ ] Capability discovery + graceful absence (agent not installed → `doctor` explains, nothing crashes).
- [ ] Native tools registered alongside: `run_in_terminal` / `read_terminal` / `watch_terminal` (tmux `capture-pane`, `pane_current_command`; shell-reappearing = done signal), `announce` (queue, ≥8s spacing, degrade to notification), `remember` (flat file), `system_query` (`df`/`ip`/`free`/`systemctl`/sensors — read-only allowlist, **no shell**), `clipboard` (`wl-clipboard-rs`, gated), `media_control` (MPRIS via `zbus`).
- [ ] `cosmo-type`: Wayland virtual keyboard (`zwp_virtual_keyboard_v1` via `wayland-protocols-misc`) with a synthesised keymap, for Unicode-safe text injection into terminals/fields. **Never** bind `zwp_input_method_v2` (invariant #1); add a comment at the protocol-init site so nobody "fixes" that later.
- [ ] Integration test against a fake stdio MCP server (scripted tool list + annotations) so gate mapping is testable without the real agent.

**Two work items phase 0 added here** (findings §6 — neither existed in the blueprint, both are on the phase-1 critical path because phase 3 hotwords and phase 4 reflex both depend on them):

- [ ] **Focus + workspace tracking from the compositor, not the agent.** `zwlr_foreign_toplevel_management` (`activated` state) or `cosmic-protocols` (`zcosmic_toplevel_info_v1`), held on a persistent Wayland connection so the mirror stays live and listing is free. This is what feeds hotword biasing keyed by focused `app_id` (§3.3) and anything that reasons about "the current window". Degrade gracefully on GNOME so the phase-1 portability claim survives.
- [ ] **Workspace moves via `cosmo-type` chord.** The agent has **no** `move_to_workspace` tool at all, and its `move_window` is an x/y position move routed through GNOME Shell or X11/EWMH — neither exists on COSMIC. Send COSMIC's Super+Shift+N chord through our own virtual keyboard (no portal, no ydotool). Verify it actually moves a window before phase 4 advertises "workspace moves" as a reflex verb.

### 1.4 Reasoning (text first)

*Interpretation note:* the blueprint puts the Realtime WS client in `cosmo-reason` for phase 5, but `cosmo say` must execute real commands in phase 1. So: build `cosmo-reason` now against the **plain chat-completions API** (tool-calling, text in/out — trivial with `reqwest`), and upgrade it to the Realtime session in phase 5. Same prompt, same tool schemas.

- [ ] `cosmo-reason` v1: chat completions, tool loop, gate interposed on **every** tool call (MCP and native alike).
- [ ] Prompt skeleton honoring the token budget: static prompt < 3,000 tokens; **no desktop state in prompt**; log server-reported rate-limit/usage headers every turn.
- [ ] Daemon state machine with the six states already modeled (`Idle/Listening/Thinking/Acting/Waiting/Speaking`) and exported over IPC events — audio just fills them in later.

#### Secret handling — **decided, 2026-09-10**

Not a menu; implement exactly this. Rationale is recorded so nobody re-opens it mid-phase.

- [ ] **Store the key in the Secret Service** (`org.freedesktop.secrets` over D-Bus) using **`oo7` 0.6** — pure Rust on `zbus`, which is already a workspace dependency. Do **not** link `libsecret`: a C dependency weakens the phase-8 "static-ish binary plus models" packaging argument. (`secret-service` 5.2 is an equivalent fallback if `oo7` disappoints; `keyring` 4.2 adds cross-platform abstraction a COSMIC-only daemon does not need.) Verified available on the dev box: `gnome-keyring-daemon` running, `org.freedesktop.secrets` registered on the session bus.
  - Attribute set for lookup: `{ "application": "cosmo", "provider": "openai" }`. Label: `cosmo — OpenAI API key`.
- [ ] **Resolution order, exactly:** `OPENAI_API_KEY` env var → Secret Service → structured error pointing at `cosmo auth login`. The env var exists for **dev and CI only** and is documented as such; it must not become the recommended path in the README.
- [ ] **Never store the key in `config.ron`.** That file is written with commented defaults on first run and is the thing users paste into bug reports. The config may hold a *provider name*, never a credential. Add a comment at the config-write site saying so.
- [ ] **`cosmo auth login`**: open `https://platform.openai.com/api-keys` in the browser, then read the key from stdin with **echo disabled**, and store it via `oo7`. Also `cosmo auth status` (is a key resolvable, and from which source) and `cosmo auth logout` (delete the secret). This is the whole browser-tab story — see the note below on why there is no OAuth flow.
- [ ] **Keyring-locked is not a crash.** The daemon runs as a systemd user unit under `graphical-session.target` and can start **before** the keyring is unlocked. Treat a locked keyring as *retry on next use*, not a startup failure — a daemon that dies at boot because the keyring was not ready yet is an unacceptable first-run experience. Resolve the key lazily on the first reasoning turn, not in `main()`.
- [ ] **`doctor` distinguishes three states**, because they have three different fixes: `key present (source: env|keyring)` / `keyring locked — unlock and retry` / `no key stored — run cosmo auth login`. "Auth broken" as a single state is not actionable.
- [ ] **Redaction, tested.** Wrap the key in a newtype whose `Debug`/`Display` print `[redacted]`, so it cannot reach a `tracing` span, an error chain, or a transcript log by accident. Unit-test that `format!("{:?}", …)` of the config/client does not contain the key. Note the specific trap: the item below logs rate-limit headers every turn — that request-logging path must not dump `Authorization`.

**Why there is no "sign in with your browser" flow** (checked 2026-09-10, so nobody spends a weekend on it): OpenAI exposes no `/oauth/authorize` that mints API access for a third-party application. "Sign in with ChatGPT" is identity-only and as of April 2026 ships solely inside Codex tooling. Implementing one anyway would require cosmo to operate its own OAuth broker — a hosted service, a client secret that cannot remain secret inside an open-source desktop binary, and cosmo becoming the middleman for the user's own API traffic. Rejected: wrong trade for a single-user local daemon. The browser tab in `cosmo auth login` opens the *key-creation page*, which captures most of the convenience at no infrastructure cost.

### 1.5 Definition of done

- [ ] `cosmo say "open the terminal and run htop"` → tmux tool → transcript of what happened, printed to the terminal.
- [ ] `cosmo say "shut the machine down"` → Hold → `cosmo confirm` completes it locally.
- [ ] A deny-listed request never executes, ever, under any confirmation phrasing (test suite).
- [ ] Every tool call logged with latency; `cosmo doctor` renders a readiness table.
- [ ] Works on GNOME (no COSMIC-specific code touched yet) — proves the portability claim early.
- [ ] Daemon survives an unclean kill: `SIGKILL` then restart with a stale socket present must come back up without manual cleanup.
- [ ] Focus tracking agrees with reality on COSMIC where `focused_window` does not — i.e. the compositor path returns the actually-focused window while the agent still reports `focused: false` for everything (invariant #9 demonstrated, not assumed).
- [ ] A window actually moves workspace via the virtual-keyboard chord.

---

## Phase 2 — the voice layer (still no microphone)

Goal: pick your accent **before** the thing can hear you, because every later phase means listening to it.

- [ ] **Link spike first (half a day, do it before planning around these crates).** `ort 2.0.0-rc.13`, `koko 0.2`, and `pipewire 0.10` have never been resolved or compiled in this workspace — nothing consumes them, so `Cargo.lock` does not contain them and CI has never proven they link. Add them to one crate, build, and record: onnxruntime acquisition (bundled/download vs. system `libonnxruntime`), whether `clang`/`cmake` are needed, and total cold build time. `ort` is a release candidate — if it fights, that is a phase-2 fact worth knowing on day one, not during the phrase-cache work.
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

- [ ] **Link spike first:** `sherpa-onnx 1.13.7` bindings — confirm how the native library is obtained (vendored build vs. system lib), that `clang`/`libclang-dev`/`cmake` cover it, and that int8 models load. Same reasoning as phase 2: prove the link before designing around two resident models.

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
| Security | Invariants section above; API key in the Secret Service with a redacting newtype (**decided — §1.4**); clipboard gated; lock-screen detection **fails closed** when lock state is undetermined (**phase-1 gate item — §1.2**) |
| Upstream | ~~Cached-probe PR to `computer-use-linux`~~ — **closed, not needed** (findings §2: the COSMIC backend is selected directly, cold ≤45 ms). Two *new* upstream candidates phase 0 surfaced: `focused_window` silently returning `null` on COSMIC, and the absence of any workspace-move tool. Consider extracting `cosmo-hotkey` as a shared crate (open question §16). |
| CI | `--locked` so the lockfile is load-bearing. Native-dep crates (`ort`, `sherpa-onnx`, `pipewire`, `libcosmic`) will not build on a bare `ubuntu-latest` runner once live — plan a two-tier job (always-build core crates; feature-gated or separately-installed heavy tier) at the phase-2 link spike rather than when CI first goes red. |
| Open questions | Track §16 items as issues at repo creation; close each with a measurement, not an opinion. **Closed by phase 0:** workspace moves on COSMIC (no tool exists → virtual-keyboard chord), cached-probe PR (not needed). **Still open:** ASR model choice (phase 3 `bench-asr`), en-AU quality (phase 2 MeloTTS spike), lock-screen detection (**promoted to a phase-1 gate item**, §1.2), wake-word false accepts (phase 7), voice blending (parked), hotkey crate extraction. |

## Dependency chain (what blocks what)

```
Step 0 ─→ Phase 0 ─┬─→ Phase 1 (spine) ─┬─→ Phase 2 (TTS) ─→ Phase 4 (reflex) ─→ Phase 5 (voice reasoning) ─→ Phase 6 (overlay)
                   │                     └─→ Phase 3 (ears) ──┘                                        │
                   └─ kill gate                                                                        └─→ Phase 7 (wake) ─→ Phase 8 (packaging)
```

Phase 2 and phase 3 are independent of each other and can interleave once phase 1 lands; the overlay (6) only needs the IPC event stream to exist and can be started alongside 4–5 for earlier visual feedback if desired.
