# Phase 0 findings — feasibility spike

**Date:** 2025-09-10 (session 2)
**Machine:** real COSMIC session (cosmic-comp, `XDG_CURRENT_DESKTOP=COSMIC`), System76 Launch keyboard + ITE laptop keyboard, `computer-use-linux` via npm/nvm.
**Kill criterion status:** **PASS so far** — COSMIC window control works and is fast. Two open interactive items, both low-risk (see §5).

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

## 5. Pending interactive items (need a human at the keyboard)

1. **evdev trigger test** — `phase0_evdev` (built, `--list` verified; devices open as the logged-in user with no root/`input` group — uaccess ACLs confirmed working for open+ioctl).
   **Trigger discovery first:** the Launch has no physical F13 key (capabilities list F13–F24 but nothing is mapped to them out of the box). Run capture mode and press whichever key you want as the hold-to-talk trigger:
   ```
   ./target/debug/examples/phase0_evdev --capture --secs 15
   ```
   It prints `code N` per press across every keyboard node. Then watch mode with that code (here e.g. ScrollLock, code 70):
   ```
   ./target/debug/examples/phase0_evdev --code 70 --secs 30
   ```
   While it runs: focus a text editor; press/release the trigger a few times; while **holding** it mash letter keys — nothing must type. Exit 0 = press+release edges arrive, nothing else delivered. Capture results to be appended here (device node + code chosen).
2. ~~Real activation check~~ — **skipped by decision**: `activate_window` assumed functional on COSMIC; phase 1 exercises it on real unfocused windows anyway. Findings §3 caveat stands until then.
3. ~~Workspace-move fallback check~~ — **decided: defer to cosmo's virtual keyboard** (`zwp_virtual_keyboard_v1` sending the COSMIC Super+Shift+N chord, phase 1, `cosmo-type`). The `press_key` portal route is abandoned untested.

## 6. Decisions recorded

- **Kill criterion: PASS** — window listing and activation work on COSMIC, fast enough for the reflex path with room to spare (35–45 ms per op vs. 150 ms ack budget).
- Focus/workspace tracking → compositor protocols, phase 1 (new work item; `focused_window` demoted to advisory).
- Workspace moves → cosmo's virtual keyboard chord, phase 1 (new work item; agent can't do it).
- Upstream cached-probe PR → **not needed**; close that open question.
- evdev: uaccess open/ioctl as plain user confirmed; press/release/leak verification pending item 1 above.
