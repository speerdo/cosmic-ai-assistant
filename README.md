# Cosmo

A personal voice assistant for the [COSMIC](https://system76.com/cosmic/) desktop. All Rust.

> **Status:** pre-code blueprint and implementation plan. Phases, invariants, and latency budgets below are the *design contract* — code lands in the order described in [`docs/implementation-plan.md`](docs/implementation-plan.md).

The goal is not another chatbot with a mic glued on. The goal is a Jarvis: fast enough that commands feel like reflexes, honest enough that it never runs anything scary without asking, and polished enough that it looks like it belongs on COSMIC — in a voice *you* picked, including an actual accent rather than the five voices an API decided to offer.

## What it is

Cosmo is four cooperating subsystems, each with a hard performance budget:

| Subsystem | What it owns | Budget |
|---|---|---|
| **Reflex path** | Local intent match for ~30 command phrases + app names | < 150ms to acknowledgement |
| **Reasoning path** | Realtime LLM tool calls over MCP for anything reflex can't match | 1.5–2.5s per turn |
| **Voice layer** | Pluggable TTS with accent choice, plus a pre-rendered phrase cache | first audio in a few hundred ms |
| **Overlay** | layer-shell surface with six states (listening/thinking/acting/waiting/speaking/idle) | at most one buffer commit per input event |

Everything that can be local is local: push-to-talk is read straight off `evdev` (no root, no `input` group, thanks to logind `uaccess`), audio capture is a native PipeWire client on the RT data-loop so it doesn't stall while your machine compiles something, both STT models (`nemotron-speech-streaming-en-0.6b` class + a Parakeet-class offline pass) are resident int8 ONNX via `sherpa-onnx`, and the default TTS is Kokoro-82M running on `ort`. The cloud is used for what it's actually good at — the reasoning model — and even then it runs **text-out**, so your chosen voice speaks the reply instead of the API's fixed catalogue.

## Hard rules (the interesting part)

These are the invariants the codebase is built around. They're the reason Cosmo can have a wake word without becoming a liability:

- **Never bind `zwp_input_method_v2`.** One slot per seat, and grabbing it while IBus holds it wedges keyboard input session-wide on cosmic-comp. Text injection is a synthesised keymap over `zwp_virtual_keyboard_v1` only.
- **Never register `run_shell`** from `computer-use-linux`. The MCP agent's most dangerous tool stays absent.
- **The daemon owns the engine.** The COSMIC panel spawns one applet process per output; the applet and overlay are thin clients over a Unix socket, and the daemon (not any applet) owns the mic, hotkey, and resident models, always.
- **Policy gate:** a gated tool call and a confirmation in the same model response are rejected; confirmation only counts on a genuinely new user turn; the confirm phrase is matched as a whole utterance ("don't confirm that" does not confirm); and the local confirm path (overlay click, `cosmo confirm`) never goes back to the model — a spoken-only confirmation is forgeable by anything that reaches your microphone, including your own speakers.
- **Reflex path is allowlist-only.** Deny-listed and hold-listed verbs are unreachable without the model, so the fast path can't become the unsafe path. This is also why a wake-word false accept costs an annoyed look instead of an action.
- **No live desktop state in the prompt.** Window/workspace info comes from tool calls, keeping the static prompt under ~3,000 tokens and the reflex path at zero tokens.
- **Half-duplex by default.** Mic gated while Cosmo speaks (+350ms settle) so it never transcribes its own voice as a command.

## Honest limitations

- **Not on GNOME-on-Wayland for the overlay** — layer shell isn't supported there; Cosmo degrades to notifications. (The daemon, hotkey, TTS, and reasoning all still work, and the spine phase explicitly targets GNOME first to prove this.)
- **Australian accent is the weak one.** Kokoro ships US/UK; MeloTTS is the only free en-AU path and its quality still has to be measured before it's promised.
- **It will not act unprompted.** Jarvis anticipates; Cosmo deliberately doesn't. You get very fast reactive execution and a memory of your projects, not an agent rummaging through your shell while you're away.

## Prior art and attribution

Cosmo stands on three shoulders, and the blueprint says so explicitly: [`omarchy-voice`](https://github.com/omarchy/omarchy-voice) for the original design, [`techgeek1/cosmic-voice`](https://github.com/techgeek1/cosmic-voice) (MIT) for the corrected hotkey/audio/STT findings that killed our naive first plan, [`agent-sh/computer-use-linux`](https://github.com/agent-sh/computer-use-linux) (MIT) for the COSMIC MCP "hands", and [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) for the default voice.

## Docs

- **[`docs/cosmo-blueprint.md`](docs/cosmo-blueprint.md)** — why the architecture is shaped this way, including the cosmic-voice corrections that changed the design.
- **[`docs/implementation-plan.md`](docs/implementation-plan.md)** — the phase-by-phase build order with checkable steps, from the one-evening feasibility spike (kill criterion included) through packaging for Fedora/Pop!_OS.

## License

MIT — see [LICENSE](LICENSE).
