# Cosmo

A personal voice assistant for the [COSMIC](https://system76.com/cosmic/) desktop. All Rust.

> **cosmo is a voice front-end for MCP. It gives a hard-gated microphone to any MCP server on your Linux desktop, speaks in a voice you chose, and ships knowing how to drive COSMIC.**

Every improvement to [`computer-use-linux`](https://github.com/agent-sh/computer-use-linux) makes cosmo better, and a second MCP server is config, not code.

**Status: early access (v0.1.0).** It works day to day on Pop!_OS with COSMIC, where it is built and tested. Fedora packaging is written but not yet tested. There are no published releases yet, so you build the packages yourself (below). Expect rough edges, and please report them. What each development phase measured is in [`docs/`](docs/), third-party terms are in [`THIRD_PARTY.md`](THIRD_PARTY.md), and security notes are in [`SECURITY.md`](SECURITY.md).

## Install

**From a package.** There is no download page yet, so build the Pop!_OS / Ubuntu `.deb`s from a checkout (`cargo-about` and `scripts/fetch-native` are needed once, see [`packaging/`](packaging/README.md)), then install them:

```sh
packaging/build-deb         # writes packaging/out/*.deb
sudo apt install ./packaging/out/cosmo_0.1.0-1_amd64.deb ./packaging/out/cosmo-applet_0.1.0-1_amd64.deb
cosmo setup                 # once: about you (name, home town for the weather, units),
                            # the local models (~1.6 GB), and a reasoning provider
npm install -g @agent-sh/computer-use-linux@0.5.0   # the "hands" (desktop control)
cosmo auth-login --provider openrouter   # sign in with your browser (or any provider below, with a key)
cosmo doctor                # what works, what doesn't, and the fix for each
```

Fedora has an `.rpm` spec for COPR in the same folder, which hasn't been built yet.

The daemon is a systemd user unit that the package enables for every user. It starts with your graphical session, or right away with `systemctl --user start cosmo`. The overlay starts at login. Add the applet in Settings → Desktop → Panel → Applets. No root, and no `input` group: the trigger key is read through logind's `uaccess` ACL.

The applet's popup also lets you pause listening, quit cosmo (**Quit cosmo**) and bring it back (**Start cosmo**), and choose whether it starts at login (**Start at login**), so you never need a terminal to stop it.

**From source:** `scripts/fetch-native` (once), then `scripts/install-dev`, which installs into `~/.local`.

The goal is not another chatbot with a mic glued on. The goal is a Jarvis: fast enough that commands feel like reflexes, honest enough that it never runs anything scary without asking, and polished enough that it looks like it belongs on COSMIC. It speaks in a voice *you* picked, including an actual accent rather than the five voices an API decided to offer.

## What you can ask it

Hold Right Ctrl and speak, or say "Cosmo, ..." with the wake word on.

| You say | What happens | How |
|---|---|---|
| "Pause the music", "next track", "volume up", "mute" | Controls the player or the volume | instant, local |
| "What time is it?", "what's the date?" | Answered from your clock | instant, local |
| "Open Firefox", "open the PDF reader", "open Chromium in a new workspace" | Launches any installed app, by name or by what it is (a web browser, a PDF reader) | instant, local |
| "Focus Discord", "switch to workspace 2", "move this window to workspace 3", "maximize this window" | Drives your windows and workspaces | instant, local |
| "How's the weather?", "what's in the news?" | Reads your forecast and chosen feeds | reasoning model |
| "When does Dune 3 come out?" | Searches, reads a page if needed, answers in a sentence or two | reasoning model |
| "Search for COSMIC themes", "open the Arch wiki" | Opens the results or page in your browser | reasoning model |
| "Type thanks, see you at 3", "write a reply saying I'll be late" | Types into the window you're working in. It never presses Enter, so nothing is sent or run until you do | reasoning model |
| "Run htop in a terminal", "what's using my disk?" | Runs commands in cosmo's own tmux session and reads the result back | reasoning model, risky commands wait for you |
| "Click Save", "scroll down" | Drives the screen through the MCP desktop agent | reasoning model, held for your confirmation |

Anything that could do harm waits for you. See [Security](#security).

## What it is

Cosmo is four cooperating subsystems, each with a hard performance budget:

| Subsystem | What it owns | Budget |
|---|---|---|
| **Reflex path** | Local intent match for ~30 command phrases + app names | < 150ms to acknowledgement |
| **Reasoning path** | Streamed LLM tool calls over MCP for anything reflex can't match | 1.5-2.5s per turn |
| **Voice layer** | Pluggable TTS with accent choice, plus a pre-rendered phrase cache | first audio in a few hundred ms |
| **Overlay** | layer-shell surface with six states (listening/thinking/acting/waiting/speaking/idle) | at most one buffer commit per input event |

Everything that can be local is local: push-to-talk is read straight off `evdev` (no root, no `input` group, thanks to logind `uaccess`), audio capture is a native PipeWire client on the RT data-loop so it doesn't stall while your machine compiles something, both STT models (`nemotron-speech-streaming-en-0.6b` class + a Parakeet-class offline pass) are resident int8 ONNX via `sherpa-onnx`, and the default TTS is Kokoro-82M running on `ort`. The cloud is used for what it's actually good at, the reasoning model, and even then it runs **text-out**, so your chosen voice speaks the reply instead of the API's fixed catalogue.

## Your profile

`cosmo setup` asks a few things once, so you don't have to repeat them. It saves them to `~/.config/cosmo/profile.json`, readable only by you:

- **Your name**, so cosmo can address you.
- **Your home town:** "how's the weather today?" needs no place. It's looked up once with OpenStreetMap's Nominatim, and you pick from the matches.
- **Units:** metric or imperial, defaulting from your locale.
- **News sources:** RSS or Atom feeds, for "what's in the news?" or "any news about Linux?". Setup offers ten (BBC, NPR, The Guardian, The New York Times, Al Jazeera, BBC Technology, Ars Technica, The Verge, Hacker News, Phoronix), and you can add any feed URL. There's no account and no key: cosmo reads the feeds from your machine, keeps them for 15 minutes, and the model reads out a few headlines.

You can change them by voice too ("I've moved to Leeds", "use metric", "add The Verge to my news"), with `cosmo setup` again, or by editing the JSON. `cosmo profile` shows them.

The reasoning model is told your name and town, never your coordinates. Only the weather service receives coordinates, rounded to its 4-decimal limit. Forecasts come from MET Norway (free, no key, CC BY 4.0) and are cached for as long as the service says they're valid.

## Looking things up

Ask "when does Dune 3 come out?" or "how long ago was Stonehenge built?" and cosmo searches, reads, and answers in a sentence or two. It knows today's date, so "how long ago" and "is it out yet" come out right. Where it looks is `search_provider` in `config.ron`, or `cosmo search` to see the options:

| Backend | Covers | Needs |
|---|---|---|
| `wikipedia` (default) | encyclopaedic facts: films, history, people, places | nothing |
| `ollama` | the whole web | a free Ollama account's key: `cosmo search use ollama` |
| `tavily` | the whole web | a Tavily key (free monthly searches): `cosmo search use tavily` |

If a keyed backend fails, cosmo falls back to Wikipedia and says so. It can also open a result (`read_page`) when the summary isn't enough. Search engines' own pages are never scraped. Web text reaches the model labelled as data, and the policy gate still decides every action, so a page can't talk cosmo into doing anything.

## Reasoning providers

The reasoning model is the only part that can use the cloud, and it isn't tied to one company, or to the cloud at all: it can run on your own computer.

**Connect as many providers as you like, use one at a time.** Each provider has its own key, stored in the Secret Service (never in a file), so you can stay connected to several and switch whenever you want. Leave `model` empty for a provider's default.

The simplest way is the panel applet's **Reasoning** section:

- Every connected provider is its own card, showing what's in use ("In use · glm-5.3-flash") or how many models it offers.
- Open a card to see its models, and press **Use** on any of them. Switching providers is one click.
- Providers you haven't connected sit under **Add a provider**, each with the one step that connects it: **Sign in** (OpenRouter), **Add key** (the other cloud providers), or **Get Ollama** (a local server).
- Ollama Cloud and OpenCode Go list all of their models, fetched with your key. Where a provider's list would be hundreds of entries or mix in non-chat models (OpenAI, OpenRouter), only its default is offered, and `model` in `config.ron` picks any other.

From a terminal: `cosmo use` lists the providers, `cosmo use <name>` switches, and `cosmo auth-login --provider <name>` connects one (cosmo then offers to switch to it).

**Three ways to connect:**

- **Sign in with your browser: OpenRouter.** `cosmo auth-login --provider openrouter` opens OpenRouter, you approve cosmo, and the key comes back by itself, with nothing to copy. This is the quickest start, and one OpenRouter account reaches most models (Claude, GPT, Gemini, GLM and more).
- **Paste an API key: everyone else.** `auth-login` opens the provider's key page; create a key there and paste it (it isn't echoed).
- **Nothing at all: a local model.** See below.

Why only OpenRouter: browser sign-in has to be a flow the provider publishes for other apps. OpenRouter publishes one. Anthropic forbids third-party apps from using Claude.ai logins, and Z.ai, OpenCode Go and Ollama Cloud offer API keys only. OpenAI's "Sign in with ChatGPT" (October 2026, in preview) is different: it bills your ChatGPT plan, not an API key, and isn't supported yet. Use `--paste` to paste a key even for OpenRouter (over SSH, for example).

| `provider` | Connect | Format | Default model | Notes |
|---|---|---|---|---|
| `openrouter` | **browser sign-in** | chat completions | `anthropic/claude-haiku-4.5` | one account, many models |
| `openai` | API key | chat completions | `gpt-4o-mini` | a ChatGPT subscription isn't an API key |
| `anthropic` | API key | Messages API | `claude-haiku-4-5` | needs a Claude Console API key; a Claude Pro/Max subscription isn't one |
| `opencode-go` | API key | chat completions, or Messages with `api_format: "anthropic"` (Qwen, MiniMax) | `glm-5.3-flash` | full model list in the applet; its terms say it's designed for coding agents, so check them before using it for a voice assistant |
| `ollama` | API key | chat completions (Ollama Cloud) | `gpt-oss:120b` | full model list in the applet |
| `zai` | API key | chat completions (pay-as-you-go) | `glm-5.3-flash` | a GLM *Coding Plan* is meant for coding tools |
| `local` | nothing | chat completions (any OpenAI-compatible server) | `granite4.1:8b` | runs on this computer: nothing leaves it |

### Local models

With `provider: "local"`, reasoning runs on your own machine: no account, no key, no usage bill, and nothing you say leaves the computer. cosmo talks to any server that speaks the OpenAI chat-completions API:

- **[Ollama](https://ollama.com/download)** is the default (`localhost:11434`): install it, run `ollama pull granite4.1:8b`, then `cosmo use local`, or press **Use** in the applet.
- **LM Studio** or **llama.cpp's `llama-server`**: set `api_base` to their address (`http://localhost:1234/v1/chat/completions` or `http://localhost:8080/v1/chat/completions`).

`cosmo doctor` says whether the server is up and has the model, and the applet lists the models it has, each with a **Use** button. Ollama's own cloud models (`:cloud` or `-cloud`) show up in that list too, under a plain warning that they run on ollama.com and aren't private. What to expect:

- **The model has to call tools well.** That's how cosmo acts on the desktop. The default is IBM's Granite 4.1 8B (Apache-2.0): it supports tool calling and answers without a "thinking" pass, which a spoken reply can't afford to wait for.
- **Size is the trade-off.** Small models are quick but simpler; a 30B model handles harder requests and wants a good GPU. Many newer models think before answering, which costs seconds per turn.
- **Speed depends on your hardware.** On a CPU alone, expect several seconds per reply.

`api_base` points any of these at another endpoint (a local server, an unlisted provider), and `api_format` picks the wire. Tool calling has to work for cosmo to be useful, and it varies by model, so try a model with a few commands before relying on it. Voice output stays local (Kokoro) whatever is chosen here.

## Security

cosmo listens to a microphone and can act on your desktop, so it is built to distrust its own inputs. The short version, with the full model and how to report a problem in [`SECURITY.md`](SECURITY.md):

- **Speech recognition and the voice run on your computer.** What you say is transcribed locally. The *text* of a request goes to the reasoning provider you chose (or nowhere, with a local model). Audio never leaves the machine.
- **API keys live in the Secret Service**, not in a file, and are never logged. The daemon's socket is yours alone (`0600`).
- **Nothing risky runs on a voice command alone.** Destructive or irreversible actions are held until you confirm with a click, `cosmo confirm`, or by voice *while holding the key*. Your speakers, a video or another person can't confirm.
- **Web pages are data, not instructions.** `read_page` refuses `localhost` and private network addresses, including after redirects, so a page can't point cosmo at your router or local services.
- **Typing never presses Enter**, and screen, click and typing tools refuse while the session is locked.

## Hard rules (the interesting part)

These are the invariants the codebase is built around. They're the reason Cosmo can have a wake word without becoming a liability:

- **Never bind `zwp_input_method_v2`.** One slot per seat, and grabbing it while IBus holds it wedges keyboard input session-wide on cosmic-comp. Text injection is a synthesised keymap over `zwp_virtual_keyboard_v1` only.
- **Never register `run_shell`** from `computer-use-linux`. The MCP agent's most dangerous tool stays absent.
- **The daemon owns the engine.** The COSMIC panel spawns one applet process per output; the applet and overlay are thin clients over a Unix socket, and the daemon (not any applet) owns the mic, hotkey, and resident models, always.
- **Policy gate:** a gated tool call and a confirmation in the same model response are rejected; confirmation only counts on a genuinely new user turn; the confirm phrase is matched as a whole utterance ("don't confirm that" does not confirm); and the local confirm path (overlay click, `cosmo confirm`) never goes back to the model. A spoken-only confirmation is forgeable by anything that reaches your microphone, including your own speakers.
- **Reflex path is allowlist-only.** Deny-listed and hold-listed verbs are unreachable without the model, so the fast path can't become the unsafe path. This is also why a wake-word false accept costs an annoyed look instead of an action.
- **No live desktop state in the prompt.** Window/workspace info comes from tool calls, keeping the static prompt under ~3,000 tokens and the reflex path at zero tokens.
- **Half-duplex by default.** Mic gated while Cosmo speaks (+350ms settle) so it never transcribes its own voice as a command.

## Honest limitations

- **No overlay on GNOME-on-Wayland.** Layer shell isn't supported there, so Cosmo degrades to notifications. The daemon, hotkey, TTS and reasoning all still work.
- **US and UK English voices only.** Kokoro ships those two accents. An Australian voice (MeloTTS) was considered and isn't being pursued.
- **COSMIC doesn't publish its lock state**, so cosmo infers it from logind's `Lock` signal and whether any window is active (`docs/phase8-findings.md` §8). Screenshots, clicks and typing refuse while locked, and also while no window is active, for example on an empty workspace: open or activate an app first.
- **Fedora packages are new.** The `.deb` is built and tested on Pop!_OS. The Fedora `.rpm` spec is written but hasn't yet been through COPR, so expect rough edges and please report them.
- **The reasoning model sees what you say, and what tools return.** A cloud provider gets the text of your requests, page text and tool results. Use a local model (`cosmo use local`) if that matters to you.
- **It will not act unprompted.** Jarvis anticipates; Cosmo deliberately doesn't. You get very fast reactive execution and a memory of your projects, not an agent rummaging through your shell while you're away.

## Prior art and attribution

Cosmo stands on others' work, and the blueprint says so explicitly: [`omarchy-voice`](https://github.com/wombatoperator/omarchy-voice) (MIT) for the design, [`techgeek1/cosmic-voice`](https://github.com/techgeek1/cosmic-voice) (MIT) for the input and audio layers (its corrected hotkey/audio/STT findings killed our naive first plan), [`agent-sh/computer-use-linux`](https://github.com/agent-sh/computer-use-linux) (MIT) for the hands, and [Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) (Apache-2.0) for the voice. Speech recognition runs NVIDIA's NeMo models through [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) (Apache-2.0). [`THIRD_PARTY.md`](THIRD_PARTY.md) has the full list and terms.

## Docs

- **[`docs/cosmo-blueprint.md`](docs/cosmo-blueprint.md)**: why the architecture is shaped this way, including the cosmic-voice corrections that changed the design.
- **[`docs/implementation-plan.md`](docs/implementation-plan.md)**: the phase-by-phase build order with checkable steps, from the one-evening feasibility spike (kill criterion included) through packaging for Fedora/Pop!_OS.
- **`docs/phase{N}-plan.md` / `-findings.md`**: each phase's spec, and what was measured, decided and found while building it.

## License

MIT. See [LICENSE](LICENSE). One exception, as distributed: the `cosmo-applet` binary links two GPL-3.0 crates from cosmic-panel, as every COSMIC panel applet does, so that binary (and its package) is GPL-3.0. Its source is MIT like the rest.
