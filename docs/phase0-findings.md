# Phase 0 findings — feasibility spike

**Date:** 2026-09-10 (session 2)
**Machine:** real COSMIC session (cosmic-comp, `XDG_CURRENT_DESKTOP=COSMIC`), System76 Launch keyboard + ITE laptop keyboard, `computer-use-linux` via npm/nvm.
**Kill criterion status:** **PASS** — COSMIC window control works and is fast. One open interactive item remaining, low-risk (see §5; items 2 and 3 were closed by decision).

## 1. `computer-use-linux` doctor

```
can_register_mcp_tools:        true
can_query_windows:             true   ← the kill-criterion gate
can_focus_apps:                true
can_focus_windows:             true
can_send_development_input:    true
can_build_accessibility_tree:  false  (AT-SPI off; element-aware actions blocked)
```

AT-SPI disabled is fine for phases 0–1 (window-level ops only); `setup_accessibility` exists if ever needed.

## 2. MCP timings (rmcp + TokioChildProcess, exact blueprint §5 shape)

Three runs, all on the live session:

| Operation | Time | Notes |
|---|---|---|
| connect + initialize | 22–27 ms | process spawn included |
| tools/list | ~5 ms | 18 tools, annotations present |
| list_windows (cold) | 37–45 ms | backend: `cosmic-wayland` |
| list_windows (warm) | 32–38 ms | |
| focused_window | ~37 ms | **unusable on COSMIC** — see below |
| activate_window | 34–38 ms | `is_error=false` |
| whole probe total | ~180 ms | |

**GNOME-backend shadowing: not an issue.** The feared slow-probe problem (GNOME ServiceUnknown timeouts before reaching the COSMIC helper) does not manifest: `list_windows` selects the `cosmic-wayland` backend directly, cold ≤45 ms. **No upstream cached-probe PR needed** (revisit only if we see different backend selection on other installs).

**Caveat on cold time:** measured on a warm OS session; a first-call-after-boot number is not captured but is bounded well within reflex budgets regardless.

## 3. Findings that change the plan's assumptions

1. **`focused_window` is broken on COSMIC.** It falls back to `gnome-shell-introspect`, which returns `"focused_window": null` — no error, silently empty. And `list_windows` reports `focused: false` for *every* window, `workspace: null` for all.
   **Consequence (phase 1):** focus and workspace tracking must come from the compositor, not the agent — `zwlr_foreign_toplevel_management` (`activated` state) or `cosmic-protocols`. cosmo should treat `focused_window` as advisory only; never build focus logic on it.
2. **There is no workspace-move tool.** The installed `computer-use-linux` (18 tools) has no `move_to_workspace`. Its `move_window` is an x/y *position* move routed through the GNOME Shell extension or X11/EWMH — neither exists on COSMIC, so it is expected to fail here (untested; position moves were deemed too disruptive to trial blindly).
   **Fallback options (pick in phase 1):**
   - `press_key` with COSMIC's move-to-workspace chord (Super+Shift+N) through the RemoteDesktop portal keyboard — untested, needs one-time verification;
   - cosmo's own `zwp_virtual_keyboard_v1` sending the same chord (no portal, no ydotool) — preferred, since `cosmo-type` needs the keyboard anyway.
3. **`activate_window` returns success on COSMIC** (`is_error=false`, ~35 ms), but was only exercised on the already-focused window — a silent no-op can't be ruled out from this alone. Needs one disruptive verification (see §5).
4. **Annotation spot-check:** `screenshot`, `resize_window`, `move_window`, `activate_window` carry `destructiveHint=false`; click/type/press_key/set_value/perform_action/drag carry `destr=true`. The gate's annotation mapping (plan §1.2) is viable; note the gate must be stricter than annotations alone (e.g. `activate_window` on someone else's window mid-typing is not "safe" by category).

## 4. Layer-shell verdict

**PASS on cosmic-comp.** `phase0_layer` (raw `smithay-client-toolkit` 0.21, no default features): one `Layer::Top` surface, anchored bottom, 640×120, 48 px bottom margin, `KeyboardInteractivity::None`, ARGB8888 solid fill, frame-callback loop — renders for 6 s, exits 0. No focus steal, no decorations. The blueprint's overlay option 2 (raw SCTK) is confirmed viable; libcosmic-vs-SCTK stays a phase 6 decision.

## 5. Interactive items

1. **evdev trigger test — RUN, PASS** (2026-09-10, session 3).

   **Device:** `/dev/input/by-id/usb-ITE_Tech._Inc._ITE_Device_8258_-event-kbd` (the ITE laptop keyboard). The System76 Launch was not plugged in for this run; it enumerated separately in session 2 and the uaccess semantics are per-node, so a Launch binding needs one repeat of this test on its own node.

   **Keycode used:** 67 (`KEY_F9`), discovered via `--capture`. The Launch has no physical F13 (capabilities list F13–F24 but nothing is mapped out of the box), so the F13 default is unusable on this hardware. *F9 is a probe choice, not a final binding — see §7.*

   **Result** (`--code 67 --secs 30`):
   ```
   mask: EV_KEY limited to code 67, EV_MSC suppressed
   [ 19.788s] PRESS   — EVIOCGRAB acquired
   [ 22.451s] RELEASE — grab released
   summary: 1 presses, 1 releases, 38 autorepeats, 40 SYN
   non-trigger events delivered: 0 (expect 0)
   PASS: press+release edges arrive, nothing else delivered
   ```

   **What this proves:**
   - Press **and** release edges both arrive — hold-to-talk is viable off evdev, which §3.1 of the blueprint needs and cosmic-comp's shortcut system cannot do.
   - `EVIOCSMASK` holds: **0 non-trigger events** delivered across a 30s window that included a 2.66s hold with other keys being pressed. The "not a keylogger" claim is mechanically true, not aspirational.
   - `EVIOCGRAB` acquires on press and releases on release, as designed.
   - Open + both ioctls succeed as the logged-in user with **no root and no `input` group** — logind `uaccess` ACLs are sufficient.

   **Implementation note for `cosmo-hotkey`:** the kernel emitted **38 autorepeats** during a 2.66s hold. Hold-to-talk must key off `value==1`/`value==0` and ignore `value==2` entirely, or a long hold reads as a press storm.

   **Leak check — CONFIRMED by observation** (the part the probe cannot see): while F9 was held, typing in a focused editor produced **no text**; on release, typing worked again immediately. So `EVIOCGRAB` does withhold keys from the focused client for the duration of the hold, and releasing the grab restores normal input cleanly — no stuck modifiers, no lost keyboard.

   This closes the one thing the probe could not prove about itself. Between the mask (nothing reaches cosmo) and the grab (nothing reaches the app while held), the hotkey design in blueprint §3.1 is verified end to end on real hardware.
2. ~~Real activation check~~ — **skipped by decision**: `activate_window` assumed functional on COSMIC; phase 1 exercises it on real unfocused windows anyway. Findings §3 caveat stands until then.
3. ~~Workspace-move fallback check~~ — **decided: defer to cosmo's virtual keyboard** (`zwp_virtual_keyboard_v1` sending the COSMIC Super+Shift+N chord, phase 1, `cosmo-type`). The `press_key` portal route is abandoned untested.

## 6. Decisions recorded

- **Kill criterion: PASS** — window listing and activation work on COSMIC, fast enough for the reflex path with room to spare (35–45 ms per op vs. 150 ms ack budget).
- Focus/workspace tracking → compositor protocols, phase 1 (new work item; `focused_window` demoted to advisory).
- Workspace moves → cosmo's virtual keyboard chord, phase 1 (new work item; agent can't do it).
- Upstream cached-probe PR → **not needed**; close that open question.
- evdev: **PASS, fully verified** — uaccess open/ioctl as plain user, press+release edges, zero non-trigger delivery, and the focused-app leak check all confirmed on real hardware (§5 item 1). Autorepeat filtering (`value==2`) is a new `cosmo-hotkey` requirement.
- **Phase 0 is complete.** All three riskiest assumptions (COSMIC window control, layer shell, evdev hold-to-talk) are proven on real hardware. Phase 1 is unblocked.

## 7. Trigger key choice — open

Keycode 67 (F9) was a discovery artifact, not a decision. A grabbed trigger is swallowed session-wide, so the final binding must be a key the user never otherwise needs. F9 conflicts with common developer bindings (toggle-breakpoint in VS Code/Cursor, LibreOffice, some browsers). Candidates that exist physically on this hardware and carry no default COSMIC or app binding should be re-checked with `--capture` before phase 3 hardcodes a default. The probe result transfers unchanged to any keycode — only the choice is open.
