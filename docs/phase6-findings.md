# Phase 6 findings

**Started:** 2026-09-30
**Scope so far:** the event plumbing, the overlay (all six states), the
applet with the voice picker, the GNOME fallback and a dev install script.
What's left needs the user: seeing it live, and adding the applet to the
panel.

## §1. The path: libcosmic, on evidence; one licence exception

- **The applet needs libcosmic anyway** (COSMIC panel applets are libcosmic
  apps). Its first build took about a minute, and the API matched what the
  crate expected apart from the library's import name (`libcosmic`).
- **libcosmic does layer surfaces**, and COSMIC's own on-screen display,
  `cosmic-osd`, is one: the overlay's exact job. With the plan's preference
  and free theming, **the overlay is libcosmic**. The raw
  SCTK/tiny-skia/cosmic-text path (proven in phase 0) stayed the fallback
  and wasn't needed.
- **Licences, checked across all 640 crates the applet pulls in** (git
  dependencies checked against their repositories' LICENSE files, since
  nine declared none):
  - libcosmic and its own crates are MPL-2.0; iced/winit and friends are
    MIT/Apache.
  - **`cosmic-protocols` at libcosmic's revision is MIT**. It was
    relicensed after the crates.io 0.2.0 that phase 4 removed.
  - **Only libcosmic's `applet` feature brings GPL-3.0**: `cosmic-panel-config`
    and `xdg-shell-wrapper-config`, from `cosmic-panel`. Every COSMIC applet
    links them. The user chose a real panel applet with that one binary
    under GPL-3.0 terms over a tray icon or no panel presence. The overlay
    uses libcosmic *without* `applet` and `cargo tree` confirms it's
    GPL-free, as are the daemon and CLI.

## §2. Event plumbing (§6.1)

- **`cosmo_ipc::client`** (feature `client`): `subscribe()` follows the
  daemon's events and **survives restarts**: it reports connected and
  disconnected and keeps retrying, so a face started before the daemon, or
  outliving it, shows nothing until it's back (tested with a fake daemon
  that disconnects and returns). `request()` sends one command on its own
  connection.
- **The view model** (`cosmo-overlay/src/view.rs`, 9 tests): events in,
  "what to draw" out, reporting whether anything *visible* changed. It
  turns tool calls into plain words ("Opening Firefox", "Running a
  command", "Going to workspace 2"). Holds keep the overlay up while
  idle. A daemon restart clears everything.
- **Bursts are coalesced before iced sees them**: the subscription drains
  every event already waiting, applies the lot, and emits at most one
  message, only if something visible changed.
- **`Event::Level`**: the mic's RMS every 50 ms of audio while listening,
  for the waveform (the ears test counts them). The applet's subscription
  filters levels and transcripts out, so the panel doesn't redraw at 20 Hz.

## §3. The overlay (§6.3)

Verified live against a scripted fake daemon on a private socket (the
user's daemon was left alone), through the Wayland protocol trace:

- **The layer surface**: anchored bottom (`set_anchor(2)`), **keyboard
  interactivity none** (`0`: no focus steal), 48 px margin, and
  `exclusive_zone(0)`, so it stays clear of the dock and panel. A first
  version used `-1` and was drawn over the dock.
- **Sizing took three tries**, each a lesson about this API:
  1. `size: Some((560, None))` means "the compositor picks the height": it
     picked **1 px**.
  2. `app_layer_shell` with no view closure draws the surface from the
     app's *main* `view`, which here was an empty placeholder: **1 × 1**. A
     diagnostic print showed `view_window` never being called.
  3. What works (and what `cosmic-osd` does; read for API usage only, it's
     GPL): `simple_layer_shell`, `size: None`, and the card drawn in
     `view_window` inside libcosmic's `autosize` widget. The surface then
     grows as content arrives: **560×62** listening, **560×91** with the
     transcript, **560×102** with a held action.
- **Seen on screen** (a screenshot cropped to exactly the surface's
  rectangle, computed from the trace, with the full capture deleted
  unseen): a themed card, "Needs your confirmation / Running a command",
  with an accent **Confirm** and a **Cancel** button. Two earlier,
  wider crops caught unrelated parts of the user's screen; they were
  deleted and no more screenshots were taken.
- **Confirm/Cancel** send `Command::Confirm`/`Cancel` for that token: the
  local path, never the model.

### Redraw discipline: measured, with one deviation

Across the scripted run (4 s listening at 20 level events a second plus
partials, then static):

| | commits per second | CPU (release build) |
|---|---|---|
| listening | ~40 | **2.7% of one core** |
| static / waiting | 0–2 | **0.3%** |

RSS is 23 MB. Commits are frame-paced and stop when nothing changes. **But
each change is committed twice**: once on the change, and once more on the
next frame callback. Removing the autosize wrapper didn't change the rate
(40/s), so it's iced's own behaviour, not the overlay's. The plan's "one
input change = at most one buffer commit" is therefore **not met**, by a
factor of two, while listening only. Rewriting on the raw toolkit to save
a couple of percent of one core while the user is talking isn't worth
losing COSMIC's theming. Recorded, not fixed.

(A process slip here: a restore step in one experiment never ran, because
`pkill -f pattern` matched the shell command itself and killed it. For a
while the overlay was measured without its autosize wrapper, and that
experiment was misdescribed as "fixed size". Caught by clippy's
`autosize_id is never used`. Everything was re-measured on the right
build: the numbers above.)

### GNOME fallback

No `zwlr_layer_shell_v1` (probed at start) → no overlay: held actions
("… confirm with: cosmo confirm 38e1b3fb") and replies become desktop
notifications. The mapping is pure and tested. On COSMIC the probe finds
layer shell and the overlay runs.

## §4. The applet and the voice picker (§6.4, §6.5)

- A panel icon that follows the daemon: absent, paused, listening, waiting
  for a confirm, ready. The tooltip says it in words ("cosmo is ready:
  hold Right Ctrl to talk").
- The popup has a listening on/off switch (`toggle`), **the voice picker**
  (voices grouped by accent, a Preview button each, Use for the others,
  the active one ticked, and the re-render's progress), and `doctor`
  summarised ("All 8 checks pass" / "Needs attention: …"). Everything it
  shows is fetched when it connects, so opening it is instant. The picker
  lives here rather than in the overlay, which takes no focus and hides
  when idle.
- The crate comment states the invariant: one applet process per output,
  and the daemon owns the mic, hotkey and models.

## §5. Installing (`scripts/install-dev`)

It builds the four binaries and installs them in `~/.local/bin`. It adds a
systemd user unit for the daemon, an autostart entry for the overlay, and
the applet's `.desktop` with `X-CosmicApplet=true`, using absolute `Exec`
paths (the panel may not have `~/.local/bin` on its PATH). It starts and
enables nothing, printing the commands instead. `--uninstall` removes
everything. It was tested against a throwaway `HOME`: 7 files installed,
7 removed. It wasn't run on the user's real home.

## Open (needs the user)

- **See it live**: `scripts/install-dev`, start the daemon and overlay,
  add the applet (Settings → Desktop → Panel → Applets), then hold Right
  Ctrl. Worth checking: the card's position above the dock on each
  monitor, the Confirm button, the voice picker's previews, the card
  disappearing at idle (tested in the view model, not yet watched), and a
  light theme (only the dark one has been seen).
- Parked by the plan: Kokoro voice blending, after the picker has been
  used.
