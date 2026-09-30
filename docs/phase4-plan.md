# cosmo — phase 4 spec: the reflex path

**Derives from:** `docs/implementation-plan.md` §Phase 4, `docs/cosmo-blueprint.md`
§2 (latency budgets, escalation rule), §4 (phrase cache), §6–7 (tools, gate)
**Drafted:** 2026-09-29
**Status:** §4.1 done 2026-09-29 (the spoken-confirm path is closed and
tested). §4.2–§4.4 done 2026-09-29: transcripts are turns, and reflex
verbs run live (window verbs in under 1 ms over a persistent connection).
§4.6 measured 2026-09-29 (findings §1d). §4.5 and §4.7 done 2026-09-29
(findings §2). **Open: the user trying reflex live** (it needs the daemon
restarted on this build), then phase 4 closes.

Goal: **a spoken command runs locally, with a cached spoken ack, in under
150 ms**, and anything reflex can't do safely goes to reasoning. Transcripts
stop being display-only: they become turns.

## Decisions carried in

- **Spoken confirmation needs the key (user, 2026-09-29).** A spoken
  "confirm" resolves a held action **only if it was said during a physical
  hold of the trigger key**. Anything that reaches the mic can say
  "confirm" (a video, a call, someone in the room). A key hold proves a
  person at the keyboard, and half-duplex already keeps cosmo's own voice
  out. Recordings started by `cosmo listen` (scriptable) or, later, the
  wake word can never confirm. Typed `cosmo say` keeps confirming: it
  arrives on the owner-only socket, the same trust as `cosmo confirm`.
- **Nothing legally doubtful goes in (user, 2026-09-29).** New protocol
  bindings are generated from permissively licensed XML; new dependencies
  get a license check (`THIRD_PARTY.md`).
- **The models** are phase 3's choice: nemotron-0.6b + parakeet-unified-0.6b.

## What phase 3 found that shapes this

- **Hotwords hallucinate short, common-word app names** ("Mute" → "Zoom
  Zoom.", "Launch LibreWolf" → "Claude LibreWolf"; phase-3 findings §6e).
  The hotword list is curated before the reflex phrases join it.
- **Virtual-keyboard chords can't trigger compositor shortcuts on COSMIC**
  (`cosmo-type`: the shortcut engine ignores virtual-keyboard modifiers).
  So window and workspace verbs use COSMIC's toplevel-management
  protocol, not a chord.
- The user's 30 recorded commands (`~/.local/share/cosmo/bench/commands/`)
  and every model's transcripts of them are test data for the matcher:
  real phrasings plus real misrecognitions ("Work Space Three").

## Parts

### 4.1 The spoken-confirm path, closed first

- [x] `cosmo-gate`: an `UtteranceSource` (`Typed`, `KeyHeld`, `OpenMic`)
      that `confirm_utterance` requires. `OpenMic` never resolves a hold;
      it reports a distinct refusal so cosmo can say how to confirm.
      *(Gate invariant #5; `ConfirmResult::NeedsKey`.)*
- [x] **Test first**, in the gate's invariant suite: a hold is parked; an
      `OpenMic` "confirm" (every confirm phrase) leaves it parked; a
      `KeyHeld` "confirm" in a later turn executes it; `Typed` behaves as
      today. *(Written and seen failing before the implementation:
      `open_mic_speech_never_confirms`, plus
      `key_held_confirm_still_needs_a_new_turn`.)*
- [x] Engine: a confirm-shaped open-mic utterance isn't forwarded to the
      model either. It gets a spoken hint: "Hold the key to confirm."
      *(`Engine::utterance(text, source)` is the one entry point; `cosmo
      say` is `Typed`. New cached phrase `confirm-needs-key`, IPC
      `TurnResult::ConfirmNeedsKey`. Tested through the engine in
      `hold_confirm.rs`.)*

### 4.2 Transcripts become turns

- [x] Ears' final transcript carries its source (key hold vs `cosmo
      listen`) into one engine entry point, `utterance(text, source)`:
      confirm check → reflex match → escalate. `cosmo say` goes through the
      same entry point as `Typed` (wake-independent: testable without audio).
      *(`Host::turn`; empty transcripts are not turns. Tested in `ears.rs`
      (source per trigger) and `reflex.rs` (engine, recording actuator).)*
- [x] Holds, the busy state, pause: the same rules as typed turns. *(One
      entry point, so one set of rules.)*

### 4.3 `cosmo-reflex`: the matcher (core tier, pure)

- [x] Intents over a small grammar, not a list of exact strings: media
      (play/pause/next/previous/stop), launch/open/start *app*, focus/switch
      to *app*, switch to workspace *n*, move this window to workspace *n*,
      maximize/minimize this window.
- [x] Normalization shared with the bench scorer ("Work Space Three" →
      workspace 3; "per cent"), filler words ("please", "can you").
- [x] App names resolved against installed `.desktop` entries (name, id,
      generic name), fuzzy but scored. *(Name and id; exact 1.0, re-split
      0.95, prefix or subset by coverage, near-miss by edit distance for
      names of 6+ letters; two apps fitting equally well are cut below the
      threshold.)*
- [x] A confidence score per match; below the threshold, escalate.
      *(Threshold 0.8.)*
- [x] Tests from the user's recorded commands and every model's
      transcripts of them, including the misrecognitions. *(Every reflex
      line of the user's list matches; "Thunder Bird", "D Beaver", "Libra
      Wolf", "key pass x c" and "Work Space Three" too; 18 non-reflex lines
      and hallucinations escalate.)*

### 4.4 Safe verbs (the only things reflex can do)

- [x] Media: MPRIS over `zbus` (fills in `cosmo-tools::media`). *(Pause
      and stop silence all playing players; play resumes a paused one.
      Tested on a fake bus player; live against Spotify read-only.)*
- [x] Launch: the `.desktop` entry's `Exec`, split per the Desktop Entry
      spec and spawned **without a shell**, field codes removed. Only
      installed, visible applications. *(98 of 102 installed apps parse;
      the 4 refused are terminal apps. Detached, reaped, no shell.)*
- [x] Focus, maximize/minimize, move to workspace, switch workspace:
      COSMIC toplevel-management plus workspace protocols (vendored XML,
      generated bindings, as `cosmo-focus` does). *(`WindowService`: one
      persistent connection; windows from `ext_foreign_toplevel_list`,
      state from `zcosmic_toplevel_info` v3, actions through
      `zcosmic_toplevel_manager` v4, workspaces through `ext_workspace`.
      Commands in 0.6–0.9 ms. Windows are matched by `.desktop` id or
      `StartupWMClass`.)*
- [ ] **Not reflex:** closing windows, volume, anything touching files,
      terminals or settings. Those escalate to reasoning, where the gate
      applies.
- [x] A gate integration test: every reflex verb is `Allow`, and no deny or
      hold verb is reachable from reflex. *(`cosmo-reflex/tests/gate.rs`:
      an exhaustive list of `Intent` variants that fails to compile when a
      new one is added. Reflex verbs declare `destructive: false`
      explicitly, since the gate holds unannotated tools. They're not
      lock-sensitive: they read no screen and inject no input, and so they
      work with COSMIC's permanently `Unknown` lock state.)*

### 4.5 Escalation

- [x] Below the confidence threshold → the reasoning path with the
      transcript. A reflex match whose action **fails** → also escalate,
      rather than report the failure. *(Tested through the engine.)*
- [x] Without an API key, the escalation says so once, spoken, and logs.
      *(Any failed **spoken** turn is now answered aloud: "I need an API
      key for anything beyond simple commands." the first time a key is
      missing, else "I can't do that right now." Typed turns are printed
      by the CLI, as before.)*

### 4.6 Acks and latency

- [x] Cached phrase acks: the phrase cache's buffer pushed straight to
      playback. Targets: **< 150 ms** release → ack audio for cached acks;
      **< 400 ms** with an uncached Kokoro reply. *(Median 98 ms, 12 of 15
      of the user's reflex commands within 150 ms; p95 207 ms, max 246 ms,
      all speech still going at the release. Findings §1d, which also
      records the next step, a speculative decode.)*
- [x] `scripts/bench-reflex`: the user's recorded commands → transcript →
      match → action (dry-run executor) → ack started, timed per span.

### 4.7 Hotwords, curated

- [x] ~~The reflex vocabulary joins the list.~~ App names that are ordinary
      English words or very short ("Zoom", "Claude", "Tasks", "Help") are
      dropped from biasing; the matcher still knows them. *(Deviation, on
      evidence: the reflex grammar's words are ordinary English the model
      already gets right, and biasing ordinary words is what caused the
      insertions, so they are not added. Biased: single-word app names of
      7+ letters, multi-word names, and rare domain words ("PipeWire").
      Findings §2.)*
- [x] Re-run `bench-asr` on the user's recordings to confirm the
      hallucinations are gone and nothing regressed. *(Gone, but because
      of §4.6's VAD change; curation itself measured neutral for the
      default (1.8% either way). Kept on principle; findings §2a.)*

## Not in phase 4

- Streaming reasoning replies (phase 5), the overlay (phase 6), the wake
  word (phase 7). The `OpenMic` source exists now so phase 7 inherits the
  confirm rule instead of re-deciding it.
