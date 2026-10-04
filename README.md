# Cosmo

A personal voice assistant for the [COSMIC](https://system76.com/cosmic/) desktop. All Rust.

> **cosmo is a voice front-end for MCP. It gives a hard-gated microphone to any MCP server on your Linux desktop, speaks in a voice you chose, and ships knowing how to drive COSMIC.**

Every improvement to [`computer-use-linux`](https://github.com/agent-sh/computer-use-linux) makes cosmo better, and a second MCP server is config, not code.

**Status:** phases 0–7 are built, and phase 8 (packages) is in progress. Hold Right Ctrl and speak, or say "Cosmo, …" with the wake word on. About thirty common commands ("pause the music", "open Firefox") run locally in well under a second, with no tokens spent. Anything else goes to the reasoning model, which drives the desktop through MCP tools and answers aloud, sentence by sentence, in a local Kokoro voice. Anything risky waits for your confirmation: a click on the overlay, `cosmo confirm`, or saying so *while holding the key*. An open mic (the wake word, or your own speakers) can never confirm. The overlay and the panel applet show what it's doing. What each phase measured is in [`docs/`](docs/), and third-party terms are in [`THIRD_PARTY.md`](THIRD_PARTY.md).

## Install

**From a package** (Pop!_OS / Ubuntu `.deb`, Fedora COPR `.rpm`; see [`packaging/`](packaging/README.md)):

```sh
sudo apt install ./cosmo_0.1.0-1_amd64.deb ./cosmo-applet_0.1.0-1_amd64.deb
cosmo models fetch          # once per user: ~1.6 GB of local models, checksummed
npm install -g @agent-sh/computer-use-linux   # the "hands" (desktop control)
cosmo auth-login            # your reasoning provider's API key → the Secret Service
cosmo doctor                # what works, what doesn't, and the fix for each
```

The daemon is a systemd user unit that the package enables for every user. It starts with your graphical session, or right away with `systemctl --user start cosmo`. The overlay starts at login. Add the applet in Settings → Desktop → Panel → Applets. No root, and no `input` group: the trigger key is read through logind's `uaccess` ACL.

**From source:** `scripts/fetch-native` (once), then `scripts/install-dev`, which installs into `~/.local`.

The goal is not another chatbot with a mic glued on. The goal is a Jarvis: fast enough that commands feel like reflexes, honest enough that it never runs anything scary without asking, and polished enough that it looks like it belongs on COSMIC — in a voice *you* picked, including an actual accent rather than the five voices an API decided to offer.

## What it is

Cosmo is four cooperating subsystems, each with a hard performance budget:

| Subsystem | What it owns | Budget |
|---|---|---|
| **Reflex path** | Local intent match for ~30 command phrases + app names | < 150ms to acknowledgement |
| **Reasoning path** | Streamed LLM tool calls over MCP for anything reflex can't match | 1.5–2.5s per turn |
| **Voice layer** | Pluggable TTS with accent choice, plus a pre-rendered phrase cache | first audio in a few hundred ms |
| **Overlay** | layer-shell surface with six states (listening/thinking/acting/waiting/speaking/idle) | at most one buffer commit per input event |

Everything that can be local is local: push-to-talk is read straight off `evdev` (no root, no `input` group, thanks to logind `uaccess`), audio capture is a native PipeWire client on the RT data-loop so it doesn't stall while your machine compiles something, both STT models (`nemotron-speech-streaming-en-0.6b` class + a Parakeet-class offline pass) are resident int8 ONNX via `sherpa-onnx`, and the default TTS is Kokoro-82M running on `ort`. The cloud is used for what it's actually good at — the reasoning model — and even then it runs **text-out**, so your chosen voice speaks the reply instead of the API's fixed catalogue.

## Reasoning providers

The reasoning model is the only part that needs the cloud, and it isn't tied to one company. Set `provider` in `~/.config/cosmo/config.ron`, then run `cosmo auth-login`: it opens that provider's key page and stores the key in the Secret Service, one key per provider. Leave `model` empty for the provider's default.

| `provider` | Format | Default model | Notes |
|---|---|---|---|
| `openai` | chat completions | `gpt-4o-mini` | |
| `anthropic` | Messages API | `claude-haiku-4-5` | needs a Claude Console API key; a Claude Pro/Max subscription isn't one |
| `openrouter` | chat completions | `anthropic/claude-haiku-4.5` | one key, many models |
| `opencode-go` | chat completions, or Messages with `api_format: "anthropic"` (Qwen, MiniMax) | `glm-5.3-flash` | its terms say it's designed for coding agents: check them before using it for a voice assistant |
| `ollama` | chat completions (Ollama Cloud) | `gpt-oss:120b` | |
| `zai` | chat completions (pay-as-you-go) | `glm-5.3-flash` | a GLM *Coding Plan* is meant for coding tools |

`api_base` points any of these at another endpoint (a local server, an unlisted provider), and `api_format` picks the wire. Tool calling has to work for cosmo to be useful, and it varies by model, so try a model with a few commands before relying on it. Voice output stays local (Kokoro) whatever is chosen here.

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
- **US and UK English voices only.** Kokoro ships those two accents. An Australian voice (MeloTTS) was considered and isn't being pursued.
- **COSMIC exposes no lock state yet**, so screenshots, clicks, typing and clipboard reads are refused outright on COSMIC (fail-closed) until the greeter sets logind's `LockedHint`. `cosmo doctor` shows this as a warning.
- **It will not act unprompted.** Jarvis anticipates; Cosmo deliberately doesn't. You get very fast reactive execution and a memory of your projects, not an agent rummaging through your shell while you're away.

## Prior art and attribution

Cosmo stands on others' work, and the blueprint says so explicitly: [`omarchy-voice`](https://github.com/wombatoperator/omarchy-voice) (MIT) for the design, [`techgeek1/cosmic-voice`](https://github.com/techgeek1/cosmic-voice) (MIT) for the input and audio layers (its corrected hotkey/audio/STT findings killed our naive first plan), [`agent-sh/computer-use-linux`](https://github.com/agent-sh/computer-use-linux) (MIT) for the hands, and [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache-2.0) for the voice. Speech recognition runs NVIDIA's NeMo models through [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) (Apache-2.0). [`THIRD_PARTY.md`](THIRD_PARTY.md) has the full list and terms.

## Docs

- **[`docs/cosmo-blueprint.md`](docs/cosmo-blueprint.md)** — why the architecture is shaped this way, including the cosmic-voice corrections that changed the design.
- **[`docs/implementation-plan.md`](docs/implementation-plan.md)** — the phase-by-phase build order with checkable steps, from the one-evening feasibility spike (kill criterion included) through packaging for Fedora/Pop!_OS.
- **`docs/phase{N}-plan.md` / `-findings.md`** — each phase's spec, and what was measured, decided and found while building it.

## License

MIT — see [LICENSE](LICENSE). One exception, as distributed: the `cosmo-applet` binary links two GPL-3.0 crates from cosmic-panel, as every COSMIC panel applet does, so that binary (and its package) is GPL-3.0. Its source is MIT like the rest.
