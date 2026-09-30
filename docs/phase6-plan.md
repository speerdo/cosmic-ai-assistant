# cosmo — phase 6 spec: the overlay and the applet

**Derives from:** `docs/implementation-plan.md` §Phase 6, `docs/cosmo-blueprint.md`
§9 (the overlay), §3.2 (daemon/applet split), §4 (voice picker)
**Drafted:** 2026-09-30
**Status:** built 2026-09-30 (findings). Open: the user seeing it live and adding the applet to the panel. One deviation: two commits per change while listening (findings §3).

Goal: **cosmo has a face.** A small surface at the bottom of the screen
shows what it's doing (listening with a live transcript, thinking, acting,
waiting for a confirmation, speaking), and a panel applet shows whether
it's there at all. Both are thin clients of the daemon's socket: they own
nothing.

## Decisions carried in

- Every state the overlay draws is already an IPC event (phases 1–5),
  except one: the listening waveform needs audio levels, a new event.
- The overlay's confirm button is a **local** confirm path, like `cosmo
  confirm`: a click on the user's own screen is physical presence, and
  never asks the model (gate invariant #4).
- Licensing: libcosmic is MPL-2.0 (fine unmodified); its dependency tree is
  checked when it lands (`THIRD_PARTY.md`).

## Parts

### 6.1 Event plumbing (pure, tested first)

- [x] A client in `cosmo-ipc` that connects, subscribes to events and
      reconnects when the daemon restarts (the overlay outlives daemon
      restarts).
- [x] The overlay's **view model**: events in, "what to show" out:
      state, partial or final transcript, tool in plain words, pending
      hold, reply text, voice-render progress. Pure and unit-tested, and
      it reports whether anything visible changed, which is the first half
      of the redraw discipline.
- [x] `Event::Level` from ears while listening (~20 per second), for the
      waveform.

### 6.2 The path decision

- [x] libcosmic layer surface (the plan's preference) or raw
      `smithay-client-toolkit` + `tiny-skia` + `cosmic-text` (proven in
      phase 0). Decided on evidence from building the applet, which needs
      libcosmic anyway. Recorded with the reasons.

### 6.3 The overlay surface

- [x] Layer surface, anchored bottom-center, no decorations, **no focus
      steal** (no keyboard interactivity). Hidden when idle.
- [x] The six states in build order: listening (waveform + live partial) →
      thinking → acting (tool name in plain words) → waiting (pending
      action + confirm/cancel buttons) → speaking → idle.
- [x] COSMIC theme colours; readable in light and dark.
- [~] Redraw discipline: coalesce across each burst of events, and again
      on `wl_surface.frame`. One input change = at most one buffer commit.
      Counted in a test. *(Bursts coalesce and commits are frame-paced and
      stop at rest, but iced commits each change twice: ~40/s while
      listening, 2.7% of a core. Accepted, findings §3.)*
- [x] Confirm/cancel clicks send `Command::Confirm`/`Cancel` for that token.
- [x] GNOME (no layer shell): notifications instead.

### 6.4 Voice picker

- [x] Voices grouped by accent, a preview button per voice, "use this
      voice" with the background re-render's progress.

### 6.5 `cosmo-applet`

- [x] A thin libcosmic panel applet over the socket: an icon showing the
      daemon's state (absent, idle, listening, …), a popup with pause/
      resume, the voice picker, and `doctor`'s summary. The crate comment
      states the invariant: **the panel spawns one applet process per
      output; the daemon owns the mic, hotkey and models, always.**
- [x] Installed as a panel applet (`.desktop` with `X-CosmicApplet`), via
      a script, not by hand.

### Parked

- Kokoro voice blending (blueprint §4): after the picker ships.
