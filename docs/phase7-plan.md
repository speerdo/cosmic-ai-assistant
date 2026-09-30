# cosmo — phase 7 spec: the wake word

**Derives from:** `docs/implementation-plan.md` §Phase 7, `docs/cosmo-blueprint.md`
§10 (wake word), §16 (false accepts)
**Drafted:** 2026-09-30
**Status:** built and tested 2026-09-30 (findings). Open: the user turning it on, recording positives, and measuring false accepts over real days.

Goal: **"Cosmo, pause the music" works hands-free**, with nothing leaving
the machine until the phrase fires (and, for reflex commands, not even
then). The Right Ctrl hold stays the deterministic fallback, always.

## Decisions (on evidence, 2026-09-30)

- **Not openWakeWord.** Its pretrained models are CC BY-NC-SA 4.0
  (non-commercial) "due to the inclusion of datasets with unknown or
  restrictive licensing", and none is "cosmo". The user's rule: nothing
  legally doubtful.
- **Not sherpa's keyword spotter either.** Its English model is trained on
  GigaSpeech, whose audio its authors don't own and grant "for non-commercial
  research and/or educational purposes": the same problem.
- **Not always-on streaming ASR.** The resident streaming model heard
  "Cosmo" in 9/9 test clips, but it costs **2.3 CPU cores while it hears
  speech**: a TV or a call would keep two cores busy.
- **Chosen: VAD-gated, first-words check with the resident offline
  model.** Silero (MIT) watches the ring while idle. When speech starts,
  its first ~1.5 s is decoded **once** by the offline model already loaded
  (NVIDIA OML; no new model, no new memory), **unbiased** (a "Cosmo"
  hotword would help it hear "Cosmo" in noise). If the transcript starts
  with the wake phrase, that recording, from the speech's onset, becomes
  the command. Measured on 9 Kokoro clips in three voices: 9/9 detected,
  ~90 ms per window. Anything else is dropped: not kept, not logged.
- **A wake turn is an open mic** (`UtteranceSource::OpenMic`): it can run
  reflex verbs and ask reasoning, and can never confirm a held action
  (gate invariant #5, decided in phase 4 with exactly this in mind).
- **Off by default** (`wake_word: false`): it's the user's choice to have a
  listening room; `doctor` says whether it's on and what it's done.

## Parts

### 7.1 Detection (pure)

- [x] The wake check: does a transcript *start* with the wake phrase
      ("cosmo", or "hey"/"ok" + it; configurable `wake_phrase`)? Only the
      start counts, which is the first guard against false accepts.
- [x] Onset tracking: at most one check per stretch of speech, decided on
      its first 1.5 s (or all of it, if shorter).

### 7.2 Listening while idle (ears)

- [x] While no recording runs and `wake_word` is on: VAD over the ring;
      each new stretch of speech gets one check. A wake starts a recording
      from the speech's onset, source `OpenMic`.
- [x] **End-pointing** for wake recordings (no key release to end them):
      finish on ~800 ms of silence after speech, or 15 s at most; a wake
      phrase with no command after it (4 s) is dropped.
- [x] The key still works, and takes precedence: a press during a wake
      check or a wake recording is handled like any press.
- [x] Half-duplex already zeroes the ring while cosmo speaks, so cosmo
      can't wake itself.

### 7.3 Reflex first, reasoning on escalation

- [x] "Cosmo, pause the music" → the matcher (the wake words are filler
      already) → reflex, zero network calls. Tested end to end.

### 7.4 False-accept hygiene

- [x] `scripts/bench-wake`: the detector over recordings. The user's 30
      bench commands (none say "cosmo") are negatives; recordings of wake
      phrases are positives. Reports detections and false accepts.
- [x] `doctor`: wake on/off, checks run, wakes, and the last wake's
      transcript start (so a false accept can be seen).
- [x] Reflex stays allowlist-only, and wake turns can't confirm: a false
      accept can at worst pause music or open an app.

- [ ] Threshold-free, but tuned on real days of the user's audio: needs
      the user (findings §4).

### 7.5 The fallback

- [x] With `wake_word` on, Right Ctrl and `cosmo listen` behave exactly as
      before (tested).
