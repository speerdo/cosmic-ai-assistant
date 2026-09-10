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