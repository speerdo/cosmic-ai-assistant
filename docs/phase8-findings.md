# Phase 8 findings

**Started:** 2026-10-04
**Scope:** packaging and release. The `.deb`s are built; the RPM spec is
written but not yet built; the clean-machine smoke test waits for the user.

## §1. The binaries really are static-ish

`readelf -d` on the release build:

| Binary | Size | NEEDED beyond libc / libm / libgcc_s |
|---|---|---|
| `cosmod` (`--features ears`) | 61 MB | `libpipewire-0.3.so.0`, `libstdc++.so.6`, `libmvec.so.1` |
| `cosmo` | 11 MB | — |
| `cosmo-overlay` | 28 MB | `libxkbcommon.so.0` |
| `cosmo-applet` | 25 MB | `libxkbcommon.so.0` |

ONNX Runtime and sherpa-onnx are linked statically; Wayland uses the
pure-Rust client backend (no `libwayland-client`); eSpeak NG is
`dlopen`ed. `packaging/check-deps` encodes this as a gate.
`dpkg-shlibdeps` turns it into `libc6 (>= 2.39), libgcc-s1, libpipewire-0.3-0t64,
libstdc++6 (>= 12), libxkbcommon0`. The glibc floor is the build machine's
(Pop!_OS 24.04), so a deb built here won't install on older releases.

## §2. Licences, generated

`cargo about` over each binary's dependency graph, merged and deduplicated
into one notices file per package (`packaging/gen-licenses`). What it
turned up:

- **Seven git crates from libcosmic, window_clipboard and
  dbus-settings-bindings declare no licence** in their own directories.
  Each is clarified in `packaging/about.toml` from its repository's root
  licence file, **pinned by SHA-256**, so an upstream relicence fails the
  build instead of passing silently.
- **libcosmic embeds two fonts** (Open Sans and Noto Sans Mono,
  `include_bytes!`), under OFL-1.1, which allows embedding provided the
  licence goes along. It's now in the accepted list, and the notices carry
  it. libcosmic's tree also has CC-BY-SA-4.0 icons, but its `build.rs`
  embeds those only on non-unix targets. Not in our binaries.
- **The GPL gate works:** the same policy run on `cosmo-applet` fails on
  `cosmic-panel-config` and `xdg-shell-wrapper-config`. The applet's run
  accepts GPL-3.0-only for that binary alone.
- **ONNX Runtime's prebuilt static archive has no licence files.**
  `fetch-native` now fetches upstream's `LICENSE` (MIT) and
  `ThirdPartyNotices.txt` (protobuf, abseil, re2, onnx, …) at the same
  tag, pinned by checksum, alongside the licence files of every dependency
  sherpa's cmake fetched.

The notices are about 900 KB for `cosmo` and 420 KB for the applet.

## §3. The agent and the unit's PATH

The phase-6 dev installer baked the PATH of `computer-use-linux` (an nvm
path) into the unit, and that breaks whenever node is upgraded. A
packaged unit can't do that at all. The daemon now finds the agent itself:
PATH, then `$NPM_CONFIG_PREFIX/bin`, `~/.local/bin`, `~/.npm-global/bin`,
the newest `~/.nvm/versions/node/*/bin`, and `/usr/local/bin`. It runs the
agent with that directory first on PATH, so the `#!/usr/bin/env node`
shebang finds nvm's node (`cosmo_config::locate`, 3 tests).

## §4. `doctor`, final form

- **Works with the daemon down.** It checks this machine first (unit
  installed / enabled / running, the four default models, `libespeak-ng`
  loadable, agent found, layer shell via `cosmo-overlay
  --check-layer-shell`), then asks the daemon.
- **A warning state** (`!`, IPC field `warn`, default false for older
  peers). On COSMIC, `lock policy` was a permanent ✗ even though
  fail-closed is the intended behaviour there, so `doctor` could never be
  green. It's now a warning, and so is `barge_in: true`. Neither fails
  `doctor`.
- **Token use**: the last reasoning turn's tokens and the server's
  remaining quota.
- Live on the dev machine, 2026-10-04: all local checks ✓.

## §5. A packaging bug caught before it shipped

The first deb's `control` file was written by an unquoted heredoc, and
its description mentioned `` `cosmo models fetch` `` and `` `cosmo doctor` ``
in backticks. The shell **ran both** while building the package. The
installed `cosmo` was too old to know `models`, so nothing was fetched,
but `cosmo doctor`'s output ended up in the package description. Fixed
(plain quotes); the generated descriptions were checked.

## §6. Also fixed

Error messages told users to run `cosmo auth login`; the command is
`cosmo auth-login` (clap's kebab-case). That was 22 places, in five crates.

## §7. Not done here

- **The RPM spec hasn't been built.** There's no Fedora machine or
  container on the dev box. It needs a COPR project with networking, and
  a Fedora `rust` ≥ 1.94.
- **The debs haven't been installed.** The dev machine runs the
  `install-dev` copy, and installing both would put two `cosmo.service`
  units in play. The clean-account smoke test (plan §8.6) covers this.
- **No lintian run** (not installed).
