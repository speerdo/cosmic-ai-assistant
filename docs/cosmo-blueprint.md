# cosmo: a personal voice assistant for the COSMIC desktop

**Status:** blueprint v4, pre-code. All Rust.
**Goal:** a Jarvis. Fast, good-looking, does real work, sounds the way you want it to.
**Name:** `cosmo`. (Was: placeholder — shortlist had been `hark`, `behest`, `mynah`.)

**Change log**
- v1: planned a compositor adapter from scratch.
- v2: prior art killed that. Rescoped around `computer-use-linux` as MCP hands.
- v3: added the reflex path, overlay, and wake word. Replaced speech-to-speech with a pluggable voice layer, because the Realtime API cannot do accents.
- v4: **Rust throughout.** Absorbed hard-won findings from `techgeek1/cosmic-voice` (MIT), which corrected the hotkey design, the audio design, and the STT model choice, and flagged a landmine that can wedge your keyboard session-wide.

---

## 1. Why Rust

COSMIC is Rust. `cosmic-voice` is Rust and is the single best source of working COSMIC voice code. `computer-use-linux` is Rust. The applet must be Rust because libcosmic is. Writing the daemon in Python meant a language boundary at every interesting seam, plus a packaging problem for a distro.

More concretely: this needs a native PipeWire client on the RT data-loop, an evdev reader with `EVIOCSMASK`, two resident ONNX models, and a Wayland virtual keyboard. All of those are Rust-first on Linux and awkward from Python.

## 2. The four subsystems

| Subsystem | Owns | Why it exists |
|---|---|---|
| **Reflex path** | Local intent match, sub-150ms | The model round trip is the latency floor. Most commands never need it. |
| **Reasoning path** | Realtime model, text out, MCP tools | Anything reflex can't match |
| **Voice layer** | Pluggable TTS, pre-rendered phrase cache | Accent choice, and instant audio on the reflex path |
| **Overlay** | wlr-layer-shell surface | The face. A panel dot is not a UI. |

### Latency budget

| Path | Budget | How |
|---|---|---|
| Reflex, cached phrase | **< 150ms** | Ring-buffered audio, local transducer, phrase match, pre-rendered WAV, MCP call |
| Reflex, uncached speech | **< 400ms** | Same, plus Kokoro synthesis |
| Reasoning turn | **1.5 to 2.5s** | Realtime round trip, tool calls, sentence-streamed TTS |

**Escalation rule:** reflex first, always. Below confidence threshold, hand the transcript to reasoning. If reflex matched but the action failed, escalate rather than report failure.

## 3. Corrections from cosmic-voice

These four findings are load-bearing. All of them contradict earlier versions of this document.

### 3.1 The hotkey cannot be a COSMIC shortcut

cosmic-comp's shortcut system only spawns processes on key **press**, and `xdg-desktop-portal-cosmic` does not implement GlobalShortcuts, so neither can report a **release**. Hold-to-talk is therefore impossible through COSMIC's shortcut system.

The working approach, taken from `hotkey.rs`: read the trigger straight off evdev, **grab** the device with `EVIOCGRAB` so the key never leaks onward to the compositor or focused client, and use `EVIOCSMASK` to restrict this process's file descriptor to the trigger keycode, so no other keystroke — and no `MSC_SCAN` metadata — is ever delivered to the daemon. That combination matters for correctness, for not typing your trigger into your editor, and for being able to say honestly that the daemon is not a keylogger.

Critically: **no root and no `input` group.** logind's `uaccess` ACL is sufficient. Add udev hotplug handling so a keyboard replug doesn't kill the trigger.

Keep a COSMIC `Spawn` shortcut bound to `cosmo toggle` as the toggle-mode fallback. It works fine for press-only semantics.

### 3.2 Continuous capture with a pre-roll ring buffer

Do not start recording when the key goes down. Capture continuously into a ring buffer so the roughly 750ms before the key registers is still there and the first syllable survives.

And use a **native PipeWire client**, not `pw-record`. The callback rides the RT data-loop and keeps being serviced when the machine is saturated. On a box that is compiling while you talk to it, that is a correctness property, not a nicety.

### 3.3 Transducers, not Whisper

Push-to-talk brackets every utterance with silence, and Whisper hallucinates in silence. Transducers emit nothing there. Run two resident int8 models via sherpa-onnx:

- **Streaming** (`nemotron-speech-streaming-en-0.6b` class), decoding at ~560ms chunks, drives live partials in the overlay.
- **Offline** (`parakeet-tdt-0.6b-v2` or sibling English `parakeet-*` class; pick the exact one on `scripts/bench-asr` data), with **hotword biasing**, produces the text that actually commits.

Separate thread pools: `asr_threads` for streaming, `offline_threads` for the offline pass, because nobody waits on a partial and everybody waits on the commit. Past four threads the offline model stops getting faster.

**Hotword biasing is the reflex path's unlock.** Bias the beam search toward your ~30 command phrases plus installed app names, keyed by focused `app_id`. cosmic-voice scaffolded this and never wired it up. For dictation it's a nicety; for command matching it's the difference between working and not.

**Segmented decoding for long utterances.** The offline pass costs more than linearly in audio length: measured at four threads, **RTF** (decode time ÷ audio duration) is ~0.052 on a 30s clip but ~0.093 on a 120s clip — faster than realtime in both cases, but noticeably less efficient as the buffer grows. Cut the recording at pauses and decode each finished segment while the next is still being spoken. Their measurement on 60s of dictation: 3.9s of waiting before, 0.35s after. Cuts land inside silence so no word splits.

### 3.4 Virtual keyboard yes, input method never

Text injection uses a synthesised keymap over `zwp_virtual_keyboard_v1`. It reaches every client, types arbitrary Unicode, and never contends with a real IME. This corrects v3's ydotool recommendation, which needed the `input` group and was worse.

**The landmine.** A seat has one `zwp_input_method_v2` slot. Binding it while IBus holds it **wedges keyboard input session-wide on cosmic-comp** (a smithay bug). cosmic-voice built an entire IBus multiplexer and a documented VT-switch-and-pkill recovery procedure to deal with this. That is a dictation problem. **cosmo is not a dictation tool and must never bind the input-method slot.** Virtual keyboard only, no exceptions, and say so in a comment where someone might be tempted.

### 3.5 Applet process multiplicity

The COSMIC panel spawns **one applet process per output**. cosmic-voice elects a single primary that owns the microphone, the hotkey, and the resident models, and has the rest mirror it.

cosmo avoids this by construction: the daemon is a separate long-lived process, and the applet and overlay are thin clients over the control socket. Worth stating explicitly so nobody later "simplifies" by moving the engine into the applet.

## 4. The voice layer

### Why speech-to-speech is out

The Realtime API exposes a fixed voice catalogue: alloy, ash, ballad, coral, echo, sage, shimmer, verse, marin, cedar, with marin and cedar recommended for quality. None are British or Australian, the voice cannot be changed during a session once the model has produced audio, and marin and cedar have been reported to ignore session instructions, so prompting for an accent is unreliable.

So the Realtime model runs **text-out**. It still does reasoning and tool calls. cosmo does the speaking, and STT is local anyway.

### The provider trait

```rust
pub trait VoiceProvider: Send + Sync {
    fn id(&self) -> &str;
    fn list_voices(&self) -> Vec<Voice>;              // id, label, accent, gender, sample
    fn synthesize(&self, text: &str, voice: &str) -> Result<Pcm>;
    fn stream<'a>(&'a self, text: BoxStream<'a, String>, voice: &str)
        -> BoxStream<'a, Result<Pcm>>;
    fn is_local(&self) -> bool;
    fn latency_class(&self) -> LatencyClass;          // Instant | Fast | Network
}
```

`Voice` carries an `accent` field (`en-US`, `en-GB`, `en-AU`, `en-IE`) so the picker groups by accent, which is how you'll actually choose.

### Providers

| Provider | Rust path | Accents | Notes |
|---|---|---|---|
| **Kokoro** (default) | `kokoros` (the `koko` crate), or `kokoro-tiny`/`kokoro-micro` for a minimal embed, or `kokoroxide`; all sit on `ort` | American (`af_`/`am_`), British (`bf_`/`bm_`) | 82M params, open weights, top-ranked among accessible-weight models on the TTS Arena. Voice packs are **blendable embeddings**. `kokoro-tiny` reports 0.5 to 2s time-to-first-audio, which is exactly why the phrase cache exists. |
| **Piper** | subprocess or ONNX via `ort` | Wide en_GB/en_US set | Fallback on weak hardware |
| **MeloTTS** | subprocess | American, British, Indian, **Australian** | The only free en-AU path. Verify quality before promising it. |
| **OpenAI TTS** | `reqwest` | Limited but steerable | `gpt-4o-mini-tts` takes an `instructions` field for affect, tone, pacing |
| **ElevenLabs** | `reqwest` | Anything | Quality ceiling, opt-in, paid |

**Honest caveat:** Australian is the weak one. Kokoro is US and UK only. MeloTTS is the free route and needs testing. ElevenLabs is the reliable answer if it matters enough to pay for.

### The pre-rendered phrase cache

The trick that makes reflex feel instant. On voice selection, synthesize every canned reflex response to WAV:

```
~/.cache/cosmo/voice/<provider>/<voice-id>/
  ack-moving.wav      "Moving that over."
  ack-focused.wav     "Got it."
  ack-launching.wav   "Opening that now."
  err-notfound.wav    "I can't find that one."
  confirm-hold.wav    "That needs a confirmation."
```

Playback is an existing buffer pushed to the PipeWire output stream. Zero synthesis latency, already in the chosen voice. Switching voices re-renders in the background with progress in the overlay.

### Streaming on the reasoning path

Synthesize sentence by sentence as text streams in, so first audio lands in a few hundred milliseconds rather than after the full response. This is most of what makes speech-to-speech feel fast, and you get it without speech-to-speech.

### Voice blending

Kokoro voice packs are embeddings that can be blended and saved. `kokoros` and `kokoro-tiny` both expose style mixing. A "design your own voice" slider in the overlay is a genuinely novel feature nobody in this space has. Park it until the picker works, then try it.

## 5. Desktop control, delegated

`agent-sh/computer-use-linux` (MIT, Rust) reads accessibility trees, takes screenshots, and drives clicks, scrolls, and keystrokes across GNOME, KDE/KWin, Hyprland, i3, and COSMIC, Wayland-first. It ships a `computer-use-linux-cosmic` helper built on the `cosmic-protocols` crate, and its window registry tries each backend in order and reports which one won or why each failed.

cosmo is an MCP **host**, using `rmcp`, the official Rust MCP SDK on tokio. Its `TokioChildProcess` transport spawns a stdio server directly, which is exactly the shape needed:

```rust
use rmcp::{ServiceExt, transport::{TokioChildProcess, ConfigureCommandExt}};

let client = ().serve(TokioChildProcess::new(
    Command::new("computer-use-linux").configure(|c| { c.arg("mcp"); })
)?).await?;
```

Adding another MCP server later is config, not code.

**Tool filtering matters.** It exposes twenty-odd tools; sending all of them every turn is expensive. Allowlist about a dozen. Never register `run_shell` (absent unless `COMPUTER_USE_LINUX_ENABLE_SHELL=1`, and it stays absent).

**If it's slow**, in order: check whether the two GNOME backends time out before the COSMIC helper is reached (a cached-probe fix, possibly a one-line upstream PR), cache `list_windows` for a second within a turn, prefetch on mic-open, acknowledge before the tool returns, and only then write a native fast path. If you do, hold a **persistent** Wayland connection so `zcosmic_toplevel_info_v1` events keep a live mirror and listing costs nothing. Five operations, not twenty.

Being in Rust now makes that native path a much smaller step than it was in v3.

## 6. Native tools

| Tool | Implementation |
|---|---|
| `run_in_terminal` / `read_terminal` / `watch_terminal` | tmux `capture-pane` and `pane_current_command`. The shell reappearing is an unambiguous done signal. The single most useful behavior in the project. |
| `announce` | The only unprompted speech. Waits for any in-flight reply, never lands closer than 8s apart, degrades to a notification when listening is off. |
| `remember` | Flat file, the only memory spanning two sittings |
| `system_query` | `df`, `ip`, `free`, `systemctl`, sensors. Read-only allowlist, no shell. |
| `clipboard` | `wl-clipboard-rs`. Reading leaves the machine, so gate it. |
| `media_control` | MPRIS over `zbus`. Reflex-path candidate. |

## 7. The policy gate

The differentiator against everything else in this space. Four properties, none negotiable:

- A gated tool call and a confirmation in the **same model response** is rejected outright.
- Confirmation only takes effect after a genuinely **new user turn**.
- The confirm phrase is matched as a whole utterance, so "don't confirm that" does not confirm.
- The **local** confirm path (overlay click, `cosmo confirm`) never asks the model.

That last one matters more with a wake word. A spoken-only confirmation is forgeable by anything that reaches the microphone, including your own speakers.

Deny: `rm -rf`, `dd`, `mkfs`, `sudo`, `pkexec`, `ssh`, `passwd`, curl piped to a shell, `git push`. Hold: shutdown, reboot, suspend, package installs, config resets, closing everything.

The gate wraps **MCP tool calls**, not just shell strings. `computer-use-linux` annotates tools with MCP `ToolAnnotations` splitting read-only from UI-state mutators from destructive desktop-action mutators, and states plainly that annotations are hints and not an authorization system, and that hosts should still ask before anything that commits state. cosmo is that host. Map `destructiveHint=true` to hold by default.

**Reflex exception:** the reflex path fires only allowlisted safe verbs. Nothing on deny or hold is reachable without the model, so the fast path can't become the unsafe path.

## 8. Audio discipline

Half-duplex by default: hold the mic shut while speaking, plus ~350ms for the room to settle. The failure this prevents is documented in `omarchy-voice`: on speakers, the model's own voice returns through the mic, turn detection reads it as the user talking, cancels the in-flight reply, and transcribes the assistant's own words as a command. A fragment that transcribes as an instruction is not noise.

With local STT this is cheaper to enforce than it was over a websocket: gate at the ring buffer.

Barge-in behind config. `doctor` warns when it's on with mic and speakers on one machine.

## 9. The overlay

Layer shell is supported on Smithay-based compositors including COSMIC, plus wlroots compositors and KDE Plasma. Not on GNOME-on-Wayland, which degrades to notifications.

Two viable paths, in preference order:

1. **libcosmic layer surface.** Same toolkit as the applet, inherits COSMIC theming for free.
2. **Raw `wl_surface` via `smithay-client-toolkit`**, painted into `wl_shm` with `tiny-skia`, shaped with `cosmic-text`, colours read from the COSMIC theme. This is exactly what cosmic-voice does for its candidate window, so it is proven on cosmic-comp and there is code to read.

Redraw discipline from the same source: coalesce twice, once across the burst of events a single change produces and again on `wl_surface.frame`, so one input is at most one buffer.

States, in build order: listening (waveform, live partial from the streaming model), thinking, acting (tool name in plain words), waiting (pending action plus confirm affordance), speaking, idle. Anchor bottom-center, no decorations, no focus steal.

## 10. Wake word

Push-to-talk is not a Jarvis, and an always-open cloud mic is a real privacy cost. Local wake word resolves it: `openWakeWord` via `ort`, on-device, nothing leaves the machine until the phrase fires. The evdev trigger stays as the deterministic fallback.

This composes with reflex: after the wake word, local STT feeds the matcher first, so "Cosmo, pause the music" completes with zero network calls.

## 11. Token budget

Every turn re-sends the prompt and it counts against tokens per minute whether or not it was cached. `omarchy-voice` burns around 10,200 tokens a turn, which on a 40,000 TPM tier is roughly four turns a minute, and its docs concede prompt size *is* the throughput limit.

1. **No live desktop state in the prompt.** Windows and workspaces come from a tool call.
2. **Filter the MCP tool list.** See section 5.

Target: static prompt under 3,000 tokens. Log the server-reported limit every turn.

Local STT plus the reflex path cut spend hard, since the highest-frequency commands never reach the model and audio never gets billed.

## 12. Workspace layout

```
cosmo/
  Cargo.toml                    # workspace
  crates/
    cosmo-ipc/                  # control-socket protocol types, shared
    cosmo-hotkey/               # evdev, EVIOCSMASK, udev hotplug
    cosmo-audio/                # native PipeWire client, pre-roll ring, VAD
    cosmo-stt/                  # sherpa-onnx: streaming + offline, hotwords
    cosmo-tts/                  # VoiceProvider trait, kokoro/piper/melo/openai/eleven, phrase cache
    cosmo-mcp/                  # rmcp host, discovery, tool filtering
    cosmo-type/                 # zwp_virtual_keyboard_v1, synthesised keymap (§3.4)
    cosmo-reflex/               # intent matcher, confidence, escalation
    cosmo-reason/               # Realtime WS client (tokio-tungstenite), text-out
    cosmo-gate/                 # deny / hold / allow, annotation mapping
    cosmo-tools/                # tmux, announce, memory, system, clipboard, mpris
    cosmo-daemon/               # the state machine, owns everything above
    cosmo-overlay/              # layer-shell surface
    cosmo-applet/               # libcosmic panel applet
    cosmo-cli/                  # `cosmo` binary
  packaging/                    # systemd user unit, COPR spec, deb
  scripts/                      # nested-compositor harness
```

### Key dependencies

| Need | Crate |
|---|---|
| MCP host | `rmcp` (official, tokio) |
| Realtime websocket | `tokio-tungstenite` |
| ONNX runtime | `ort` |
| ASR | `sherpa-onnx` Rust bindings |
| TTS | `kokoros` / `kokoro-tiny`, on `ort` |
| Wayland client | `wayland-client`, `smithay-client-toolkit` |
| Virtual keyboard | `wayland-protocols-misc` (`zwp_virtual_keyboard_v1`) |
| COSMIC toplevels (if native path needed) | `cosmic-protocols` |
| UI | `libcosmic` |
| Text shaping | `cosmic-text`, `tiny-skia` |
| evdev | `evdev`, `udev` |
| Audio | `pipewire` (native bindings) |
| D-Bus | `zbus` |
| Clipboard | `wl-clipboard-rs` |
| Config | `ron`, `serde` |
| Async | `tokio` |

Config at `~/.config/cosmo/config.ron`, written with commented defaults on first run, matching the COSMIC convention.

## 13. Phases

**Phase 0, one evening.** `computer-use-linux doctor | jq .readiness` on your COSMIC session. Confirm `can_query_windows`. List, activate, move to workspace, **time all three**. Confirm evdev read works without root or the `input` group. Confirm layer shell renders. **Kill criterion:** if COSMIC window control can't be made to work, stop here.

**Phase 1, the spine.** `cosmo-ipc`, `cosmo-mcp`, `cosmo-gate`, `cosmo-tools`, `cosmo-daemon`, `cosmo-cli`. `cosmo say "..."` reads a command from the terminal and executes it. No audio at all. Testable, works on GNOME, and this is where most of the *usefulness* lives. Time everything now, while mistakes cost an afternoon.

**Phase 2, the voice layer.** `cosmo-tts`, provider trait, Kokoro, phrase cache, `cosmo voice list/preview/set`. Still no microphone: `cosmo say` speaks its replies. Pick your accent before the thing can hear you, because every later phase means listening to it repeatedly while you debug.

**Phase 3, ears.** `cosmo-hotkey`, `cosmo-audio`, `cosmo-stt`. Hold the key, see a transcript. This is the phase to steal from cosmic-voice most directly: `hotkey.rs`, `audio.rs`, and `vad.rs` are close to liftable.

**Phase 4, the reflex path.** `cosmo-reflex`. Hotword-biased matching, cached-audio playback, escalation stub. **This is where it starts feeling like Jarvis.**

**Phase 5, reasoning voice.** `cosmo-reason`, sentence-streamed TTS, half-duplex gate, confirmation flow end to end.

**Phase 6, the overlay.** Layer-shell surface, six states, voice picker with previews.

**Phase 7, wake word.** openWakeWord via `ort`.

**Phase 8, packaging.** COPR for Fedora, deb for Pop!_OS. A single static-ish Rust binary plus model files is dramatically easier to package than the Python plan was, which is a real argument for this rewrite on its own. The distro angle: a voice layer that ships configured and working, with an accent picker, is a differentiator nobody bundles.

## 14. Positioning

> **cosmo is a voice front-end for MCP. It gives a hard-gated microphone to any MCP server on your Linux desktop, speaks in a voice you chose, and ships knowing how to drive COSMIC.**

Every improvement to `computer-use-linux` makes cosmo better. A second MCP server is config, not code.

Attribution: `omarchy-voice` (MIT) for the design, `cosmic-voice` (MIT) for the input and audio layers, `computer-use-linux` (MIT) for the hands, `Kokoro-82M` for the voice.

## 15. What this will not be

Jarvis anticipates and acts unprompted. The policy gate exists specifically to prevent that, and it should. What you get is very fast reactive execution with a chosen voice, a real face, and memory of your projects. Unprompted agency is not a missing feature, it's a thing you don't want holding your shell.

## 16. Open questions

- **Workspace moves and latency on COSMIC** via the MCP helper. Phase 0.
- **Which ASR models.** cosmic-voice's pair is a strong starting point but is tuned for dictation. Command recognition with heavy hotword biasing may do better with a smaller streaming model and no offline pass at all. Bench on your own command set.
- **Australian accent quality.** MeloTTS is the free path; verify before promising it.
- **Kokoro voice blending** as a UI feature. Novel, unproven, park it.
- **Lock-screen detection on COSMIC.** Screenshots and clicks must refuse when locked. **Fail closed when lock state can't be determined.**
- **Wake-word false accepts.** With an open reasoning path a false accept costs tokens; with reflex it can cost an action. Reflex stays allowlist-only for exactly this reason.
- **Whether to upstream the evdev hotkey** as a small shared crate, since cosmic-voice needs it too and cosmic-comp is unlikely to grow key-release shortcuts soon.
