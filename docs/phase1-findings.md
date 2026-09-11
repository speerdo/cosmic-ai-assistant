# Phase 1 findings

**Date:** 2026-09-10 (session 4–5)
**Scope so far:** §1.1 complete (committed), §1.2 lock-detect investigation (this file, §L).

## §L. Lock-screen detection (plan §1.2, invariant #10)

Question: which mechanism is authoritative for "session locked" on COSMIC,
readable by an unprivileged client process?

### Investigated, in order

1. **logind `LockedHint`** (`loginctl show-session 3 -p LockedHint`).
   - Readable by any process, no privileges. Currently `no`.
   - **Nobody on this stack sets it.** Searched the binaries of
     `cosmic-comp`, `cosmic-greeter`, `cosmic-greeter-daemon`,
     `cosmic-idle`, `cosmic-panel`, `cosmic-osd` for `SetLockedHint` /
     `LockedHint`: zero hits in all of them. On GNOME,
     `gnome-settings-daemon` sets `LockedHint`, so this hook *is* the
     portable answer when cosmo runs on GNOME — and it silently stays
     `no` on COSMIC, which is exactly the fail-closed situation.
   - cosmic-greeter's binary contains `logind lock` / `logind unlock`
     strings (it *listens* to lock/sleep events) but never *writes* the
     hint.
2. **`org.freedesktop.ScreenSaver`** — owned by `cosmic-idle` on the user
   bus, but only `Inhibit`/`UnInhibit` exist. **No `GetActive`.**
3. **`com.system76.CosmicGreeter`** (cosmic-greeter-daemon) — present on
   the *system* bus, introspection and property reads are polkit-denied
   for a plain user. Not a usable source without root.
4. **`com.system76.CosmicComp`** on the user bus — only the libei input
   node (`/com/system76/CosmicComp/Ei`) and FDO base interfaces. No lock
   state.
5. **Wayland `ext_session_lock_v1`** — cosmic-comp implements the
   compositor (server) side, but the protocol exposes no read-only state
   to ordinary clients: clients may only *create* locks.
6. **cosmic-voice** — no lock-screen handling at all; nothing to lift.

### Verdict

**No reliable, unprivileged lock-state source exists on
Pop!_OS/COSMIC today.** Per plan §1.2, that means fail-closed: the gate
denies screenshot / click / type / clipboard unless a verified source
says the session is *unlocked* — and with no source, "unknown" denies.

### Implementation consequence

- `cosmo-gate` gains `LockState` (`Unlocked | Locked | Unknown`) and a
  `LockSource` trait. The gate holds the verdict when the state is not
  provably `Unlocked`.
- Shipped source: **logind `LockedHint`** via `zbus` — reads the
  property on the session of the daemon's uid. On GNOME it flips
  correctly; on COSMIC it always reads `no` ⇒ wait, it must not read
  `no` as truth when the greeter never sets it.
- **COSMIC-specific resolution:** because logind stays `no` on COSMIC
  even when locked (greeter bug, upstreamable), reading logind alone
  would let clicks/typing through *while locked* — the exact hole
  invariant #10 forbids. So the shipped default is stricter than the
  hook:
  - **COSMIC sessions** (`XDG_CURRENT_DESKTOP=COSMIC`): the sensitive
    tools (screenshot/click/type/press_key/scroll/drag/clipboard_get)
    are **denied outright** until a verified source is wired (an
    upstream greeter that sets `LockedHint`, or a future
    cosmic-protocols surface). Logged once at startup.
  - **GNOME sessions**: logind `LockedHint` is authoritative; poll +
    subscribe, deny when locked or unknown.
  - `doctor` reports which mode is active in either case, so this is
    observable, not silent.

### Reopen condition

If upstream cosmic-greeter starts calling
`org.freedesktop.login1.Manager.SetLockedHint` (or exposes a D-Bus
property a user process can read), COSMIC can move to the logind path
and re-enable the tools — delete the `COSMIC` branch in
`cosmo-gate::LockPolicy`, keep the test.

## §F. Focus mirror (plan §1.3 work item)

`zcosmic_toplevel_info_v1` on cosmic-comp (advertised **v3**) works:
bound at **v1** (the deprecated `toplevel` event flow), the mirror lists
all toplevels with correct `activated` state and titles, live on the real
session. `zwlr_foreign_toplevel_management` is **not** advertised by
cosmic-comp; the plan's two options resolve to cosmic-protocols only.

Two protocol notes worth keeping:

1. **Bind v1, not v3.** At v2+ the flow changes: the compositor sends
   nothing until the client binds `ext_foreign_toplevel_list_v1` and
   calls `get_cosmic_toplevel` per foreign handle (per the v1.2 XML).
   v1 still fires the batch `Toplevel` event with all initial state —
   one roundtrip, no pairing. Verified live: v3 bind → 0 toplevels
   silently; v1 bind → all 5 toplevels, focus correct.
2. **`zcosmic_workspace_manager_v2` is what cosmic-comp advertises** (v2,
   new interface name); cosmic-protocols 0.2 generates the v1 manager
   only, so the workspace manager never binds on this session and
   workspace names resolve to nothing (the mirror reports them empty).
   Focus is unaffected. When workspace *names* are needed (phase 4
   reflex verbs), either pin the ext-workspace protocol path or bump
   cosmic-protocols — not blocking for phase 1 (workspace moves are a
   virtual-keyboard chord, not a mirror query).

## §C. Workspace moves via virtual-keyboard chord — **VERDICT: NOT POSSIBLE on this build**

The plan's fallback (§1.3, findings §3.2 item 2) was tested live on
cosmic-comp (2026-09-10) through eleven variations and **fails**:

- **Text injection works** — bare keys through a synthesised keymap
  reach the focused client (`z`, `x`, `1`, `2` all typed correctly,
  Unicode keysym `U<hex>` shape).
- **Modifier chords do not.** Whatever ordering or mechanism:
  - modifier keys as real key events with `modifier_map` declared
    (in `xkb_symbols`, where the real keymaps put it — in
    `xkb_compatibility` this xkbcommon rejects the whole keymap with
    *"Compat files may not include other types"*, reproduced locally
    against libxkbcommon 1.6 via `new_from_fd`);
  - the `zwp_virtual_keyboard_v1.modifiers` request (Shift=0x1 |
    Mod4=0x20), before *or* after the modifier key events;
  - virtual modifiers (`virtual_modifiers Meta` + `virtualMods=Meta`,
    the only variant libxkbcommon accepts for vmod declarations);
  - even **bare Super** with the mods request —
  …the focused client always receives the *unmodified* keysym (a held
  Shift produced plain `2`, never `@`), and no COSMIC shortcut ever
  fires (Super alone does not open the launcher).

**Conclusion:** this cosmic-comp build delivers virtual-keyboard `key`
events to clients but its shortcut engine (and modifier propagation to
clients) does not process virtual-keyboard modifiers. Workspace moves
via cosmo-type are **impossible here**. Consequences:

1. **Phase 4 must not advertise "workspace moves" as a reflex verb on
   this compositor build.** (DoD item §1.5 "a window actually moves
   workspace via the virtual-keyboard chord" — not achieved; recorded
   as a limitation, not silently dropped.)
2. The chord code is trivial to re-enable if upstream fixes modifier
   handling (the failure is in the compositor, not our client — the
   same keymap text compiles and the same request sequence is what
   `ydotool`'s Wayland backend sends).
3. Alternative paths if workspace moves become required before an
   upstream fix: `zwlr_foreign_toplevel_management` **set** requests
   (cosmic-comp does not advertise the manager), the RemoteDesktop
   portal keyboard (abandoned in phase 0 for other reasons — may be
   worth re-testing since it injects at a lower level), or an
   upstreamed `move_to_workspace` action in the agent.
## §S. Phase 1 session summary (2026-09-10, sessions 4–6)

**Shipped, all committed:**
- §1.1 `cosmo-config` (commented-default first run), `cosmod` socket lifecycle
  (single-instance, stale-socket cleanup, SIGTERM unlink), `cosmo` CLI
  (status/doctor/say/confirm/cancel/toggle + exit-code contract), bench span
  names (`turn/transcript/gate/tool/reason/ack`).
- §1.2 Gate: Surface A/B, four invariants each tested, lock-screen fail-closed
  (§L), pending-hold queue with tokens. Found and fixed a `confirm_token`
  self-deadlock (guard across `match` scrutinee).
- §1.3 `cosmo-mcp` (agent connects live, 10 tools, `run_shell` refused),
  `cosmo-tools` (tmux terminal live-verified incl. done-signal watch,
  announce ≥8s, remember, no-shell system_query, media vocabulary),
  `cosmo-type` (live-verified Unicode typing), `cosmo-focus` (live-verified
  mirror, invariant #9 demonstrated).
- §1.4 `cosmo-reason` (chat-completions tool loop, gate on every call,
  fake-API integration test), secrets (oo7, redacting newtype, lazy
  resolution), daemon wired end to end, `cosmo auth-*` CLI.

**Live-verified on real hardware/session:** socket lifecycle (SIGKILL stale
socket, SIGTERM unlink, single-instance), tmux terminal tools, virtual-
keyboard text injection, focus mirror, MCP agent discovery, Secret Service
auth-status.

**Not achieved, honestly recorded:**
1. Workspace-move chord — compositor-side limitation (§C).
2. Full `say`→model→executes-tools round trip with a real API key — the
   pipeline reaches OpenAI and fails structured without a key; everything
   short of the key is tested (fake server + unit tests). Needs one real
   `OPENAI_API_KEY` run to observe.

**Tests:** 39 test groups green across the workspace; `fmt` + `clippy -D
warnings` clean.
