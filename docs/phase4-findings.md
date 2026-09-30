# Phase 4 findings

**Started:** 2026-09-29
**Scope so far:** the spoken-confirm rule, the reflex matcher, its three
executors, transcripts as turns, and the release → ack budget measured on
the user's own recordings.

## §1. The reflex path (spec §4.1–§4.4, §4.6; 2026-09-29)

### 1a. Spoken confirmation needs the key (§4.1; the user's decision)

Gate invariant #5: every utterance carries an `UtteranceSource` (`Typed`,
`KeyHeld`, `OpenMic`). A confirm phrase from an open mic (`cosmo listen`
now, the wake word later) resolves nothing: `ConfirmResult::NeedsKey`. It
isn't passed on to reasoning either, and cosmo says "Hold the key while
you confirm." (a new cached phrase). A key-held confirm still obeys
invariant #2 (new turn only). Typed `cosmo say` keeps confirming: it's the
owner-only socket, the same trust as `cosmo confirm`.

The test was written first and seen failing to compile, then passing:
`open_mic_speech_never_confirms` runs every confirm phrase through an open
mic in fresh turns and checks the hold survives; engine-level tests in
`hold_confirm.rs` drive `Engine::utterance`, as that file's own history
insists.

### 1b. The matcher (§4.3)

`cosmo-reflex` is a small grammar over normalized words, not a phrase
list: media (play/pause/stop/next/previous), launch/open/start *app*,
focus/switch to *app*, switch to workspace *n*, move this window to
workspace *n*, maximize/minimize. Filler is dropped ("could you … for me
please"). Anything left over means no match, and the turn escalates.

- **App names** are fuzzy but scored against installed `.desktop`
  entries: exact 1.0; re-split letters 0.95 ("Thunder Bird"); a prefix or a
  subset of the name, by how much was said ("D Beaver" → DBeaver CE 0.86);
  a near miss by edit distance, for names of 6+ letters only ("Libra
  Wolf"). Two apps fitting equally well get cut below the 0.8 threshold.
  A first version took the *first* rule that applied rather than the best,
  and scored "Open DBeaver" at exactly 0.80.
- **Numbers** go through the bench's normalizer, plus the homophones the
  bench caught after "workspace" ("for" → 4, "to" → 2).
- Tested on every line of the user's command list and on what the eight
  model pairings actually wrote. 18 non-reflex lines and the hotword
  hallucinations ("Zoom Zoom.", "Claude LibreWolf.") escalate.
- **Gate test** (`cosmo-reflex/tests/gate.rs`): every `Intent`'s tool call
  gets `Allow` from the real gate, none carries a string for the gate to
  vet, and none is lock-sensitive. The list of intents is an exhaustive
  `match`, so a new verb doesn't compile until it's tested. Reflex verbs
  must declare `destructive: false`: the gate holds unannotated tools.
  They aren't lock-sensitive (they read no screen and inject no input),
  which also keeps them working on COSMIC, where the lock state is always
  `Unknown`.

### 1c. The executors (§4.4)

| Verb | How | Checked |
|---|---|---|
| media | MPRIS over zbus: pause/stop silence all playing players; play resumes a paused one; next/previous go to the one playing | fake player on the real session bus; live: found Spotify (paused), sent it nothing |
| launch | `Exec=` split per the Desktop Entry spec, **no shell**, field codes dropped, detached and reaped; terminal apps refused | 98 of 102 installed apps parse; the 4 refused are terminal apps. Shell syntax stays literal |
| focus, maximize, minimize, move, switch workspace | `WindowService`: one persistent Wayland connection | live: a no-op workspace switch, 0.9 ms |

**Why a persistent connection.** The compositor offers
`ext_foreign_toplevel_list_v1` (app ids), `zcosmic_toplevel_info_v1` v3
(window state), `zcosmic_toplevel_manager_v1` v4 (actions, including
`move_to_ext_workspace`) and `ext_workspace_manager_v1` (workspaces). It
does *not* offer `zcosmic_workspace_manager_v1`, which is why phase 1's
mirror saw no workspace names. A fresh connection needed **60–150 ms**
before it knew which window was focused: cosmic-comp sends window state
on its next refresh, not in answer to a round trip, and never sends
`info.done`. Persistent, a snapshot costs 0.35 ms and a command 0.6–0.9
ms. The blueprint (§5) predicted exactly this.

**App id ≠ `.desktop` id.** Spotify's entry is `com.spotify.Client`, its
window `spotify`. A test had asserted that mismatch as expected; clippy
flagged the line's style, and reading it showed the bug. Windows now
match on the entry's id *or* its `StartupWMClass` (Spotify, Discord and
Edge all set one).

COSMIC ignores virtual-keyboard modifiers for shortcuts (`cosmo-type`), so
none of this could have been a key chord.

### 1d. Release → ack, measured (§4.6)

`scripts/bench-reflex` replays the user's 30 recorded commands through
the **daemon's own ears controller**: a real-time ring, the press where
the user's was, the release where theirs was. It times release → committed
transcript → reflex match. The actuator is a dry run.

| | release → ack, 15 reflex commands | release → commit, all 30 |
|---|---|---|
| first run | p50 104 ms, p95 312 ms, max 346 ms | p50 172 ms, p95 346 ms |
| after the two changes below | **p50 98 ms, p95 207 ms, max 246 ms** | p50 128 ms, p95 246 ms |

The breakdown showed decoding at 60–110 ms for commands. The slow ones
were **all release tail**: 190–250 ms waiting for the VAD to confirm
silence after speech that ran up to the release.

- The VAD's silence debounce went from 100 to **50 ms**. It only
  suppresses single-window flicker; pauses are the segmenter's 400 ms.
- The tail cap went from 300 to **200 ms**. On the user's recordings real
  speech ran at most ~100 ms past the release (phase-3 §6f, less the
  debounce).

Accuracy is unchanged: pairing B re-run on the recordings gives 1.8% WER
on commands and 21/22 exact, with the same two misses.

**12 of 15 reflex commands now ack within the 150 ms budget.** The rest
(201–247 ms) are commands the user was still saying at the release. The
next step, if it matters in use: at the release, decode what's there
**speculatively** while the tail runs, and act on it if it's already a
confident reflex match. The cost is a second decode; the risk is acting
on a command cut short, which the matcher's whole-utterance rule largely
guards against. Recorded, not done. The ack's own audio start (a cached
buffer pushed to playback, one PipeWire quantum) is on top of these
numbers.

### 1e. Along the way

- **A suite hung because of a new phrase.** Adding the sixth cached
  phrase hung the daemon's unit tests: its fake synthesizer granted
  exactly five permits, and three tests counted to five. They now follow
  the phrase list's length. `cosmo-tts`'s cache tests had the same
  assumption.
- **`cargo test` stops at the first failing crate.** A green-looking
  total was partial; runs now use `--no-fail-fast`, with a timeout, so a
  hang can't pass as slowness.
- The phase-1 `Event::ToolStarted` / `ToolFinished` were defined but never
  emitted. Reflex calls are the first to emit them, for the overlay
  (phase 6).

## §2. Hotwords curated; failures answered aloud (spec §4.5, §4.7; 2026-09-29)

### 2a. Curation: kept on principle, not on a measured win

The rule: bias rare names the model spells badly on its own, not short
ordinary-looking ones. So single-word app names of 6 letters or fewer
("Zoom", "Claude", "Tasks", "Help") are no longer biased, and a small list
of rare domain words ("PipeWire") is. The matcher still knows every app.
One deviation from the spec: **the reflex grammar's words aren't added**.
They're ordinary English the model already gets right, and biasing
ordinary words is what caused the phase-3 insertions.

Measured on the user's 30 recordings, all runs with §1d's VAD change:

| Pairing | every app name | curated | phase 3 (every name, old VAD) |
|---|---|---|---|
| A (nemotron + tdt-0.6b-v2) | 10.5% | 10.5% | 14.0% |
| **B (default)** | **1.8%** | **1.8%** | 1.8% |
| G (tdt-110m) | 10.5% | 12.3% | 12.3% |

- **The hallucinations were fixed by §1d, not by curation.** With every
  app name still biased, A's "Mute" → "Zoom Zoom." is gone under the
  shorter VAD debounce, which changes how each segment is trimmed.
- **Curation makes no measurable difference to the default.** G lost one
  word ("Next track" → "The next track."): one clip, within noise.
- **Kept anyway**: a smaller biasing set means fewer candidates to insert
  as more apps get installed, and it costs the default nothing here. This
  is a judgement, recorded as one. `bench-asr` keeps both
  (`COSMO_HOTWORDS=all`), so it can be re-measured on new recordings.

**A method slip, caught.** The first A/B ran both halves on the old
binary: the edit script had failed partway, and the build-then-bench
pipeline didn't check the build's exit status. The two identical tables
and the unchanged report header gave it away. Both measurements were
redone. Builds before measurements are now checked explicitly.

### 2b. Failed spoken turns are answered aloud (§4.5)

A spoken turn that escalates and fails (no agent, no API key) used to fail
silently: no terminal waits on it. Now it's answered with a cached
phrase: "I need an API key for anything beyond simple commands." the
first time a key is missing (once per run), otherwise "I can't do that
right now." The no-key case is flagged by the reasoning path itself, not
matched from the error text, whose wording varies ("no key stored",
"keyring locked", "keyring unavailable"). Typed turns are unchanged: the
CLI prints the reason. Two new phrases, eight in all.
