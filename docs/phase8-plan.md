# cosmo — phase 8 spec: packaging and release

**Derives from:** `docs/implementation-plan.md` §Phase 8, `docs/cosmo-blueprint.md`
§13 (phase 8), §14 (positioning), `THIRD_PARTY.md` "Before anything is packaged"
**Drafted:** 2026-10-04
**Status:** built 2026-10-04 (findings), except: the RPM spec is unbuilt (no Fedora box), and the clean-machine smoke test needs the user.

Goal: **install a package → `cosmo models fetch` → `cosmo doctor` green →
hold Right Ctrl and talk**, on a user account with no manual group
membership and no root beyond the package install itself.

## Decisions

- **The binaries stay "static-ish", and that's checked, not assumed.**
  `readelf -d` on the release build (2026-10-04): `cosmod` needs only
  libc/libm/libmvec/libgcc_s/libstdc++ and **libpipewire-0.3**; `cosmo`
  only libc/libm/libgcc_s; `cosmo-overlay` and `cosmo-applet` add only
  **libxkbcommon**. ONNX Runtime and sherpa-onnx are static; Wayland is the
  pure-Rust backend; eSpeak NG is `dlopen`ed. `packaging/check-deps` fails
  the package build if a new `NEEDED` entry appears.
- **Models are fetched, never packaged.** The packages carry
  `fetch-models` (checksummed, pinned, already written in phase 2–3) as
  `/usr/libexec/cosmo/fetch-models`; `cosmo models fetch` runs it for the
  default set (Kokoro fp32 + English voices, Silero VAD, the two ASR
  models), then restarts a running daemon. Nothing bundles the NVIDIA
  models, so the OML redistribution notice stays the user's download, as
  today.
- **Two packages, because of the GPL.** `cosmo` (daemon, CLI, overlay,
  unit, fetcher) is MIT plus the permissive licences of what it links.
  `cosmo-applet` carries GPL-3.0 as distributed (THIRD_PARTY.md), so it's
  its own package with its own licence field and a pointer to the source.
- **Licence texts come from the dependency graph** (`cargo about`), one
  bundle per package, and the bundle for `cosmo` is generated with GPL
  *not* accepted: a GPL crate reaching the daemon, CLI or overlay fails
  the generation. Native libraries' licence files are collected by
  `scripts/fetch-native` from the sources it builds.
- **The daemon is a systemd user unit, enabled for every user on
  install** (`systemctl --global enable`; a user preset on Fedora). It
  only listens on the key unless the user turns the wake word on, and it
  starts without models (text replies, `doctor` says why). The overlay
  autostarts via `/etc/xdg/autostart`; the applet is added by the user in
  the panel settings, as every COSMIC applet is.
- **The MCP agent isn't packaged** (`computer-use-linux` is an npm
  package). The daemon now finds it where npm puts it (nvm, `~/.npm-global`,
  `~/.local/bin`) when the unit's PATH doesn't have it, and gives the
  child that directory on PATH so its `node` shebang resolves. No more
  PATH baked into the unit.

## Parts

### 8.1 The unit and desktop files

- [x] `packaging/systemd/cosmo.service`, `packaging/desktop/*.desktop`:
      one copy, used by the packages (`/usr/bin`) and by
      `scripts/install-dev` (rewritten to `~/.local/bin`).
- [x] Daemon: agent lookup beyond PATH; the child's PATH includes the
      agent's own directory.

### 8.2 First-run models

- [x] `cosmo models` (status: what's present, what's missing, sizes) and
      `cosmo models fetch` (runs the fetcher, restarts a running daemon).
- [x] Fetcher found at `$COSMO_FETCH_MODELS`, beside the binary
      (`../libexec/cosmo/`), `/usr/libexec/cosmo/`, or the checkout's
      `scripts/`.
- [x] Every "model missing" message names `cosmo models fetch`.

### 8.3 Packages

- [x] `packaging/build-deb`: release build → staged tree → `cosmo_*.deb`
      and `cosmo-applet_*.deb` with `dpkg-deb`; dependencies from the
      audit above; `copyright` files, licence bundles, THIRD_PARTY.md.
- [ ] `packaging/rpm/cosmo.spec` for COPR (network-enabled build:
      `fetch-native` and cargo fetch), with a `cosmo-applet` subpackage.
      *(Written, not yet built: no Fedora machine or container here.)*
- [x] `packaging/check-deps`: the dynamic-dependency audit as a gate.
- [x] Licence bundles: `packaging/about.toml`, `packaging/gen-licenses`.

### 8.4 `cosmo doctor`, final form

Already present: uaccess (trigger key readable), PipeWire (mic
streaming), ASR models, lock-screen fail-closed, barge-in warning,
reasoning provider and key.

- [x] Works with the daemon down: local checks first (unit installed /
      enabled / running, models present, eSpeak NG loadable, agent found,
      layer shell via `cosmo-overlay --check`), then the daemon's.
- [x] Token use and rate limit of the last reasoning turn.
- [x] Layer-shell support.

### 8.5 README

- [x] Positioning statement (blueprint §14), install instructions,
      current status, attribution (`omarchy-voice`, `cosmic-voice`,
      `computer-use-linux`, Kokoro-82M).

### 8.6 Release smoke test (needs the user)

- [ ] On a clean COSMIC VM or fresh user account: install the packages →
      `cosmo models fetch` → `cosmo doctor` green → `cosmo say` → spoken
      reply → Right Ctrl hold-to-talk. No `input` group, no root beyond
      the install.
