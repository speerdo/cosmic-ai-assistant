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

## §8. Lock state on COSMIC, measured (2026-10-05)

Clicking, typing and screenshots had been refused outright on COSMIC
because nothing reported the lock (phase-1 findings §L). Read again,
then measured with the user locking and unlocking once while
`cosmo-focus`'s `lock_probe` example and `gdbus monitor` recorded:

- **cosmic-greeter's source** (master, 2026-09-30): it locks on logind's
  `Session.Lock` signal or `PrepareForSleep`, and unlocks after PAM
  through the Wayland session-lock protocol alone. No `Unlock` signal, no
  `LockedHint`, no D-Bus name or file. There is no published unlock signal.
- **Measured:** `Session.Lock` fired as the lock screen appeared (10:05:35.7).
  ~0.5 s later **no window was activated**. At unlock (10:07:03.4, PAM),
  the previous window was activated again ~0.4 s later. No `Unlock`
  signal arrived.

The tracker (`cosmo-daemon/src/lock.rs`, 6 tests) is fail-closed at every
step:
- A `Lock` or sleep signal ⇒ **Locked**, until every window has been seen
  inactive and then one active again. The activation left over from
  before the lock doesn't count.
- Otherwise, a window is active ⇒ **Unlocked**.
- No window is active ⇒ **Unknown** (refuse). The lock screen and "nothing
  focused" look the same from here; the cost is that screen tools also
  refuse on an empty workspace until an app is opened or activated.
- The window connection fails, or logind's signal stream ends ⇒ Unknown
  (permanently, for the latter).
- If the signal subscription can't be made, COSMIC stays deny-all as
  before.

The gate is set from the tracker the moment either input changes,
instead of once per turn, so a lock in the middle of a turn counts.
Live after install: "unlocked (a window is active, and no lock since)".
**Observed live, 2026-10-05:**
- The `Lock` signal reached cosmo at 10:39:31.065, and it read **Locked**
  35 ms *before* the greeter drew its lock surface (10:39:31.100).
- The user unlocked at 10:48:47.733 (PAM), and cosmo read **Unlocked**
  0.36 s later.
- Screen tools were refused for the whole 9 minutes.

**Two older bugs found on the way.**
- logind's session object path escapes a leading digit: session `3` is
  `/session/_33`, not `/session/3`. So the GNOME `LockedHint` probe could
  never have worked, even with `XDG_SESSION_ID` set.
- A systemd user service has no `XDG_SESSION_ID` anyway. Both now take
  the path from logind's `User.Display`.

## §9. What reasoning can do on the desktop (2026-10-05)

- **The reflex verbs became reasoning tools:** launch_app, focus_app,
  switch_workspace, move_window_to_workspace, maximize/minimize_window.
  They use the same actuator. Before this, "open a browser on workspace 2
  and search…" had no way to switch workspaces or launch anything.
- **`open_url`** (http/https only, the URL always its own argument): the
  default browser, or with `new_window` its `[Desktop Action new-window]`
  plus `--new-window` (Edge's action line lacks the flag). A web search is
  the results URL, with no clicking or typing.
- **The agent allowlist matched computer-use-linux 0.5.0's tools again**:
  four names had gone, and its AT-SPI tools (`get_app_state`,
  `perform_action`, `set_value`, `list_apps`) were missing. `get_app_state`
  is lock-sensitive, since it reads the screen.

## §10. From the user's use (2026-10-05)

- **"Play" after "pause" escalated to reasoning,** which then said Spotify
  "is already playing" without checking. Three causes:
  - the matcher's phrasings were narrow ("play some music", "turn the music
    back on", "play Spotify" all missed);
  - `playerctld`, a proxy player, was counted as a player of its own;
  - "play" picked the first paused player on the bus.
  Now: more phrasings (20 tested, and "start Spotify" still launches);
  playerctld is ignored; "play" resumes what cosmo paused; and
  `media_control status` lets the model check.
- **The overlay could stay up for good.** A client that falls behind on
  events (the mic level alone is ~20 a second) has some skipped, so a
  missed "idle" or "resolved" stranded the card. Now:
  - the overlay checks the daemon's status every 2 s while it shows;
  - held actions expire after 2 minutes (cancelled, never run);
  - the card has a ✕ that closes it and cancels what it was waiting on.
