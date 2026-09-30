# Phase 7 findings

**Started:** 2026-09-30
**Scope:** the wake word, built and tested. Open: turning it on and
measuring false accepts over real days of the user's audio.

## §1. What wakes it, and why not the plan's choice

The plan named openWakeWord. Checked 2026-09-30:

| Option | Why not |
|---|---|
| openWakeWord | Code Apache-2.0, but **all pretrained models CC BY-NC-SA 4.0** (non-commercial), "due to the inclusion of datasets with unknown or restrictive licensing". None is "cosmo"; a custom one means a training pipeline. Fails the user's licensing rule. |
| sherpa-onnx keyword spotting | Open-vocabulary, 17.6 MB, already linkable. But the English model is trained on **GigaSpeech**, whose audio its authors don't own and grant only "for non-commercial research and/or educational purposes": the same problem. |
| Always-on streaming ASR (the resident nemotron) | Heard "Cosmo" in 9/9 clips, but costs **2.3 CPU cores while it hears speech**: a TV or a call would keep two cores busy. |

**Chosen: a VAD-gated check of each stretch of speech's first words, with
the offline model already resident.** Silero (MIT) watches the ring while
idle, which costs nothing in silence. Each new stretch of speech has its
first ~1.5 s decoded **once**, unbiased (a "Cosmo" hotword would help the
model hear "Cosmo" in noise: a false accept), by the offline model
already loaded (NVIDIA OML). So there's no new model, no new memory, and
nothing new to license. If the transcript starts with the phrase ("cosmo",
optionally after "hey"/"hi"/"ok"), that recording, from the speech's
onset, becomes the command. Anything else is dropped: not kept, not
logged, not counted beyond "checks".

Measured: 9/9 Kokoro clips in three voices detected from their first
1.5 s alone; **~90 ms of decode per check** (p50 91 ms, max 103). With
speech in the room all the time, that's roughly a tenth of one core,
against 2.3 cores for continuous streaming.

**A phase-3 cost this exposed:** the streaming model behind the live
partials also costs ~2.3 cores while a recording is running. That's fine
for a few seconds of hold-to-talk, but it's noted for anyone considering
longer dictation.

## §2. Built

- **`cosmo_stt::wake`** (pure, 5 tests): `wake_prefix` / `strip_wake` (only
  the transcript's start counts: "I told Cosmo…" and "Cosmos is…" don't
  wake), and `WakeTracker`, which asks for **one** check per stretch of
  speech (at 1.5 s, or at its end if shorter, with a 300 ms margin before
  the VAD's onset), and not again until 400 ms of silence.
- **The ears controller** (`ears::run` gains `wake`): while idle and
  unpaused, a `Watcher` runs the VAD and tracker over the ring and decodes
  each check with `Stt::decode_once` (no hotwords).
  - A wake starts a recording **from the speech's onset**, with source
    **`OpenMic`**: it can run reflex verbs and ask reasoning, and can never
    confirm a held action (gate invariant #5, set up in phase 4 for
    exactly this).
  - **End-pointing**: 800 ms of silence once a command has started after
    the checked window; up to 4 s for a command to follow a bare "Hey
    Cosmo"; 15 s at most. The committed text must still start with the
    phrase; the command is what follows ("pause the music."). A wake with
    nothing after it is dropped.
  - **The key wins.** A press mid-check preempts it. A press *during* a
    wake recording drops that recording and starts a key recording:
    marking open-mic audio "key held" would let it confirm.
  - Half-duplex already zeroes the ring while cosmo speaks, so cosmo
    can't wake itself.
- **Config**: `wake_word` (default **off**: it's the user's choice to have
  a listening room) and `wake_phrase` (default "cosmo", validated as plain
  words). **`doctor`**'s `ears` line: on/off, the phrase, checks, wakes,
  and the last wake's transcript, so a false accept can be seen.

## §3. Tested

Heavy tier (`cosmo-daemon/tests/ears.rs`: the daemon's controller on a
real-time ring, real models, Kokoro speaking in the test):

| Test | Result |
|---|---|
| "Hey Cosmo, pause the music." | one turn, **"pause the music."**, `OpenMic`; states Listening → Idle |
| a 7.4 s read sentence, then "I told my brother about the cosmos last night." | **no** turn, no transcript, never Listening; checked, 0 wakes, nothing kept |
| "Hey Cosmo." then 5.5 s of room | a wake, **no** turn |
| a Right Ctrl hold with the wake word on | the key recording, `KeyHeld`, as before |

Reflex-first (§7.3) holds by composition: a wake turn is the open-mic turn
phase 4 already tests going to reflex with **zero API requests**.

`scripts/bench-wake` (the detector over recordings, exactly as the daemon
runs it): **0 false accepts on the user's 30 recorded commands**, 9/9 on
the Kokoro wake clips. Thirty short commands are a weak test of false
accepts, though; the plan's bar is "real days of audio" (§4).

## §4. Open (needs the user)

- **Turn it on**: `wake_word: true` in `config.ron`, restart the daemon.
- **Positives in the user's voice**: `scripts/bench-asr record --list
  scripts/bench-wake-phrases.txt --dir ~/.local/share/cosmo/bench/wake`,
  then `scripts/bench-wake --positives ~/.local/share/cosmo/bench/wake`.
- **False accepts over real days**: `doctor` counts wakes and shows the
  last one. Any recorded folder of 16 kHz WAVs (a meeting, an evening of
  TV) can go through `scripts/bench-wake --negatives DIR`.
