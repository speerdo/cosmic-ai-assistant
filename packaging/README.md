# packaging

How cosmo is packaged (phase 8, `docs/phase8-plan.md`). The packaging
argument: a few "static-ish" Rust binaries plus model files that each user
downloads, so the packages stay small and nobody redistributes a model.

| Path | What |
|---|---|
| `systemd/cosmo.service` | The daemon, as a **user** unit (`WantedBy=graphical-session.target`). |
| `desktop/…CosmoOverlay.desktop` | The overlay, started at login (`/etc/xdg/autostart`). |
| `desktop/…CosmoApplet.desktop` | The panel applet's entry (`X-CosmicApplet`). The user adds it in the panel settings. |
| `build-deb` | Builds `cosmo` and `cosmo-applet` `.deb`s for Pop!_OS / Ubuntu into `out/`. |
| `rpm/cosmo.spec` | The Fedora spec, for COPR (networking enabled for builds). |
| `check-deps` | Fails if a binary gains a shared-library dependency beyond the audited set. |
| `gen-licenses`, `about.toml` | Licence notices from the dependency graph (`cargo about`). GPL is refused for everything except `cosmo-applet`. |

`scripts/install-dev` installs these same files into `~/.local` for
development.

## Two packages, because of the GPL

`cosmo-applet` links two GPL-3.0-only crates from cosmic-panel, as every
COSMIC panel applet does, so that binary is GPL-3.0 as distributed. It
gets its own package, with its own licence field and a pointer to the
complete source. `cosmo` (daemon, CLI, overlay) links nothing copyleft
beyond MPL-2.0, which is file-level (`THIRD_PARTY.md`).

## What the binaries link (audited 2026-10-04)

| Binary | Shared libraries beyond glibc / libgcc |
|---|---|
| `cosmod` | `libpipewire-0.3`, `libstdc++` (ONNX Runtime and sherpa-onnx are static) |
| `cosmo` | none |
| `cosmo-overlay`, `cosmo-applet` | `libxkbcommon` (Wayland is the pure-Rust client) |

eSpeak NG (Kokoro's phonemes, GPL-3.0) is the system's
`libespeak-ng.so.1`, `dlopen`ed at run time. It's a package dependency,
never linked or shipped.

## Building

```sh
scripts/fetch-native                              # once: sherpa-onnx + ONNX Runtime, and their licence files
cargo install --locked cargo-about --features cli # once
packaging/build-deb                               # → packaging/out/*.deb
```

For COPR, point a project at `rpm/cosmo.spec` with networking on. The
build runs `fetch-native`, installs `cargo-about` into the build root,
and needs a Fedora `rust` at least as new as `rust-toolchain.toml`'s.
**The spec hasn't been built yet:** no Fedora machine or container was
available when it was written.

## After install

Each user runs `cosmo models fetch` once (Kokoro, Silero VAD, two NeMo
speech models: about 1.6 GB, each file SHA-256 checked), then
`cosmo doctor`. The MCP agent is an npm package and isn't packaged:
`npm install -g @agent-sh/computer-use-linux`. The daemon finds it under
nvm or a user npm prefix even though the unit's PATH has neither.
