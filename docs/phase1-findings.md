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
    tools (screenshot/click/double_click/right_click/type_text/press_key/
    scroll/drag/clipboard_get/clipboard_set/set_value/perform_action)
    are **denied outright** until a verified source is wired (an
    upstream greeter that sets `LockedHint`, or a future
    cosmic-protocols surface). Logged once at startup.
  - **GNOME sessions**: logind `LockedHint` is authoritative; poll +
    subscribe, deny when locked or unknown.
  - `doctor` reports which mode is active in either case, so this is
    observable, not silent.

### Scope correction (2026-09-11, review)

The first implementation also put **`run_in_terminal`** in the
lock-sensitive set. Because `CosmicDenyAll` pins lock state to `Unknown`
forever, that made `run_in_terminal` **permanently denied on COSMIC** —
i.e. phase 1's headline DoD verb (`cosmo say "open the terminal and run
htop"`) could not work on the target desktop, for a reason unrelated to
the missing API key. It was reported as done because it was only ever
exercised at unit level with the lock state set to `Unlocked`.

`run_in_terminal` has been removed from `is_lock_sensitive`. The
membership rule is now exactly invariant #10's: **a tool is lock-sensitive
if it observes or drives the user's own UI surface** — screen, pointer,
focused-window keyboard, shared clipboard, accessibility tree. Those are
what a lock screen exists to hide. `run_in_terminal` writes into cosmo's
*own* tmux server (`tmux -S cosmo`), reads no display, and injects into no
focused client; its real control is Surface B, which is stricter and
applies whether or not the screen is locked.

Verified live on COSMIC after the change: `say` → `run_in_terminal` →
tmux → transcript, and a laundered `bash -c "rm -rf …"` denied with the
target file intact.

**Residual risk, recorded not hidden:** from phase 3 on, a microphone on a
locked machine could reach `run_in_terminal` through the model. That is a
*microphone* gate (half-duplex, wake word — phases 3 and 7), not a tool
gate, and must be solved there. Noted at the `is_lock_sensitive` site.

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

## §R. Review remediation (2026-09-11)

A review of the committed phase-1 work checked the ticked boxes against the
code rather than against the summary. Seven defects, four of them on items
recorded as done. The common shape is worth stating plainly, because it is
the thing to watch for in phase 2:

> **Every one of these was unit-tested, and the unit test passed.** The tests
> exercised a helper directly, with setup the real caller does not perform.
> The gate suite advanced the turn counter by hand; the fake agent always
> supplied annotations; the matcher was tested on bare commands. Nothing
> drove the daemon the way the socket drives it, and `cosmo-daemon` had no
> tests at all. A green suite over the wrong caller is not evidence.

Each fix ships with a test that fails against the previous code (verified by
reverting each fix in turn and watching the new test go red).

### R1. `cosmo confirm <token>` could never succeed — DoD §1.5 item 2

`Engine::confirm` never advanced the turn counter, and `Gate::confirm_token`
refuses to resolve a hold parked in the *current* turn (invariant #2). A hold
parked in turn N was confirmed against turn N, so **every** CLI confirmation
returned `Unknown`, forever. `gate.begin_turn()` appeared in exactly one place
in the workspace — `say` — and the gate's own invariant test passed only
because it called `begin_turn()` by hand.

A `cosmo confirm <token>` *is* the "genuinely new user turn" invariant #2
requires: a separate deliberate act, out of band from the model, with the
token in hand. What the invariant forbids — a model approving its own gated
call inside one response — is `same_response_verdict`, which escalates to
Deny before anything is parked, and is unaffected.

**Fixed:** `confirm` begins a turn. **Test:** `cosmo-daemon`'s new
`tests/hold_confirm.rs`, driving `Engine::handle` exactly as the socket does.

### R2. The MCP annotation default was inverted — fail-open

```rust
/// ... missing hints read as `read_only=false, destructive=true`
    None => (false, false),                      // ⇒ Verdict::Allow
    a.destructive_hint.unwrap_or(false),         // ⇒ Verdict::Allow
```

The doc comment described the correct behaviour; the code did the opposite.
The MCP specification defines `destructiveHint` as defaulting to **`true`**
when absent, so this silently inverted the protocol's own default: any agent
tool shipping without annotations mapped to Allow. `cosmo_gate::Annotations`
had the same inversion via `derive(Default)`, which is what
`toolhost::annotations_of` returns for an *unknown* tool — under a comment
reading `// unknown ⇒ conservative`. Every fixture in `fake_agent.py` supplied
both hints, so the case was never exercised.

**Fixed:** both defaults are `destructive = true`; `Annotations` has a hand-
written `Default` carrying the reasoning. **Tests:**
`unannotated_tool_defaults_to_destructive`, `unannotated_tool_holds`, and a
deliberately unannotated `press_key` in the fake agent.

### R3. Surface B was laundered by any wrapper program

`segment_verdict` classified only each segment's leading program (`sudo` and
`pkexec` excepted). Everything below reached `Verdict::Allow`:

```
bash -c "rm -rf ~"        sh -c 'rm -rf /home/…'    env rm -rf ~
nohup rm -rf ~            xargs rm -rf              eval rm -rf ~
timeout 5 rm -rf ~        nice -n 10 rm -rf ~       setsid rm -rf ~
find . -exec rm -rf {} +  echo $(rm -rf ~)          `rm -rf ~`
r''m -rf ~                python3 -c "os.system('rm -rf ~')"
```

This matters more than a list of missed strings: `run_in_terminal` carrying
`bash -c "…"` **is** `run_shell`, the tool invariant #2 exists to withhold.
The plan's own warning — "a deny list wired only to Surface A would match
nothing and look like it worked" — applied one level down.

**Fixed:** three nesting forms are unwrapped before matching — command
substitution (`$(…)`, backticks), shell interpreters with `-c`, and wrapper
programs (`env`, `nohup`, `xargs`, `timeout`, `nice`, `setsid`, `eval`,
`find -exec`, …) — with a depth cap that denies runaway nesting. Quotes are
now stripped from anywhere in a token, not trimmed from the ends, so `r''m`
reduces to `rm`. `doas` joins `sudo`/`pkexec`.

General-purpose interpreters given inline code (`python -c`, `perl -e`,
`node -e`) **hold** rather than allow: their payload is not shell and cannot
be tokenised, and Hold is the gate's designed answer to "this might be
anything". Running a *script* (`python3 script.py`) stays Allow.

This is a mitigation, not a parser, and the code says so. A token matcher over
an untyped string cannot be made complete. What makes it sufficient is that
the strings come from cosmo's own model behind a prompt, not from an adversary
at a keyboard: the job is to make the blueprint's deny corpus unreachable
through ordinary rephrasing and to round the unrecognised towards Hold.

**Tests:** `nesting_cannot_launder_the_deny_list`, `nesting_preserves_hold`,
`opaque_interpreter_payloads_hold`, `runaway_nesting_fails_closed`, and
`nesting_does_not_over_match` — the last one because an over-eager matcher
that holds `cargo build` is its own failure.

### R4. `run_in_terminal` was permanently denied on COSMIC

See §L, *Scope correction*. Phase 1's headline DoD verb could not run on the
target desktop.

### R5. A held turn poisoned the conversation history

The tool loop `return`ed the moment the gate parked a call, leaving the
assistant message with its `tool_calls` in `history` and **no** `role: "tool"`
result for any of them. The daemon persists history unconditionally and
replays it, so the *next* turn failed at the API with a 400 — one turn after
the hold, which is where it would have been misattributed. History truncation
(`drain(0..len-20)`) could orphan a `role: "tool"` the same way.

**Fixed:** every `tool_call` id gets exactly one result — the held call
("HELD: parked …"), any call after it in the same response ("NOT RUN: …"), and
unparseable arguments (fed back instead of aborting the turn). Truncation
walks the cut point forward past leading tool results. **Tests:**
`held_turn_leaves_history_replayable` asserts the invariant over the whole
history, plus `tail_never_starts_on_an_orphan_tool_result`.

### R6. Agent tools were advertised to the model with no parameters

`RegisteredTool` dropped the agent's `inputSchema` and the tool host sent a
bare `{"type": "object"}` for every agent tool — the model was told `click`
exists but nothing about `x`/`y`, so it would call tools with invented
arguments. The likeliest cause of a disappointing first real-key run, and it
would have read as a model failure.

**Fixed:** `input_schema` is carried on `RegisteredTool` and passed through
verbatim. **Test:** the fake agent's `click` now declares real `x`/`y`
properties and the integration test asserts they survive.

### R7. Two tools were gated but did not exist

`clipboard_get` / `clipboard_set` had gate arms, a registry entry, a
`wl-clipboard-rs` workspace dependency and a plan checkbox — and no
implementation anywhere. **Fixed:** implemented in
`cosmo-tools/src/clipboard.rs` and registered in the tool host. They remain
lock-sensitive, so on COSMIC they refuse today (§L) and work on GNOME.

`cosmo_type_into_terminal` is in `is_string_bearing` ahead of the tool
existing. That one is deliberate and now documented at the site: registering
the name first means phase 4 cannot add the tool without a matcher arm, since
`string_tools_all_route_through_matcher` fails until it does.

### Found while verifying the fixes, live

Two more that only a real run surfaces, both on the `cosmo confirm` path that
R1 had been hiding:

- **The keyring error was unreadable.** `cosmo say` on a keyless machine
  printed *"DBus error The collection 'no key stored — run `cosmo auth login`'
  doesn't exists"*. `keyring_lookup` fabricated `oo7::Error` values to carry
  human-readable messages, and `DefaultKeySource` rendered them through oo7's
  `Display` — the message smuggled through a D-Bus error's collection-name
  field and back out. `auth_status` looked fine because it went through
  `ReasonKind`. Plan §1.4's three actionable states existed in one path and
  not the other. **Fixed:** `keyring_lookup` returns `ReasonKind` directly; no
  fabricated transport errors. **Test:** `key_failures_render_their_fix`.
- **`Response::Confirm` did not round-trip.** `Response` and `ConfirmOutcome`
  are both internally tagged on `type`, and `Confirm(ConfirmOutcome)` was a
  newtype variant, so serde wrote two `type` keys into one map and the client
  failed with ``duplicate field `type` `` (exit 4) — *after* the daemon had
  executed the confirmed action. The work happened; the reply was lost.
  **Fixed:** `Confirm { outcome }`, a named field, matching `Said { result }`.
  Every enum-valued payload now sits under a named field, which is structural
  rather than "remember to pick distinct tag names". **Tests:**
  `cosmo-ipc/tests/round_trip.rs` — the crate had none — covering every
  `Response`, `Command`, and `DaemonMessage` variant plus NDJSON framing.

### Live verification after the fixes (COSMIC, real session)

Driven against a local fake chat-completions server so the tool loop runs
without a real key:

- `say` → `run_in_terminal` → tmux → transcript returned to the model →
  reply printed. **This was impossible before R4.**
- `say` → package install → `Hold` + token → `cosmo confirm <token>` →
  executed locally, readable summary, exit 0. **DoD §1.5 item 2, first time
  end to end.**
- `say` → `bash -c "rm -rf <path>"` → `DENIED` fed back to the model, target
  file intact. **Allowed and executed before R3.**
- `say` with no key → `no key stored — run \`cosmo auth login\``.

**Tests:** 37 → 56 across the workspace; `fmt` and `clippy -D warnings` clean.

### Still open

- Workspace-move chord: unchanged, still impossible on this build (§C).
- The full round trip against the **real** OpenAI API still needs one run with
  a real key. Everything up to the network boundary is now exercised against a
  fake server, including the paths that were broken.
