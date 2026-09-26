# cosmo — phase 3 spec: ears, in eight parts

**Derives from:** `docs/implementation-plan.md` §Phase 3, `docs/cosmo-blueprint.md`
§3.1–3.3 (hotkey, capture, transducers), §8 (audio discipline)
**Drafted:** 2026-09-26
**Status:** §3.1 link spike done 2026-09-26 (findings §1). Two static
ONNX Runtimes don't link, so both crates now share sherpa's via
`scripts/fetch-native`. One licensing decision is open (findings §1e).
§3.2 capture done 2026-09-26 (findings §2), with the saturated-machine
test already passing on capture. §3.4 hotkey done 2026-09-26 (findings §3): the trigger is Right Ctrl,
and the grab was removed as unsound. Next: §3.3 VAD, then §3.5.

Goal: **hold the key, speak, release → a transcript**, with live partials
while you talk. Transcripts are *shown*, not acted on. Wiring them into
turns is deliberately out of scope (see "Not in phase 3").

## Carried in from phase 2

- **Piper (§2.8) is deferred, deliberately.** It exists for weak hardware.
  This machine synthesizes Kokoro at RTF 0.155, OpenAI TTS is already a
  second provider, and `doctor` names the fix when models are missing.
  Nothing downstream needs it. It'll be revisited if cosmo is packaged for
  hardware where Kokoro is too slow (phase 8).
- **The §2.9 en-AU MeloTTS spike** stays open. It needs the user's ears,
  not code.
- **E1 (a real-key run)** stays open. It needs the user's key.

## The risk that shapes this phase

`sherpa-onnx` (the default `static` feature) downloads a prebuilt archive
containing its **own static `libonnxruntime.a`**. The daemon already links
`ort`'s static ONNX Runtime for Kokoro (§2.5). Two static copies of the
same C library in one binary will either fail to link with duplicate
symbols, or link with **one** copy silently serving both. That second case
is only safe if its C API version satisfies both callers. Phase-2 findings
§1f already flagged "two onnxruntimes in one binary" as not to be attempted
blind. **§3.1 answers it before anything is designed around it**, and the
fallback options are named there.

## Parts

### 3.0 Packages: none expected

The one package the obvious design needs (`libudev-dev`, for the `udev`
crate's hotplug monitor) is designed out: hotplug uses **inotify on
`/dev/input`** through `rustix` (already a dependency). `sherpa-onnx`
downloads prebuilt libraries. If §3.1 shows a package is needed after all,
it goes on a list for the user, with the reason.

### 3.1 Link spike: gate for 3.5

- [x] `sherpa-onnx` 1.13.8 in `cosmo-stt` behind a non-default `sherpa`
      feature (the two-tier rule, phase-2 findings §1g).
- [x] **One binary with both ONNX Runtimes:** *(fails as shipped, on
      duplicate re2/protobuf symbols; fixed by sharing sherpa's 1.28.2
      runtime through `ORT_LIB_PATH`; verified in one process. Findings
      §1b–d.)* an example that creates a
      Kokoro `ort` session *and* a sherpa-onnx recognizer, runs both, and
      checks both outputs. Record which runtime version(s) actually end up
      in the binary.
- [x] Load an **int8** transducer (the 108 MB `parakeet_tdt_transducer_110m`
      is cheapest) and transcribe a known WAV, with hotwords (beam search).
      *(RTF 0.014; hotwords need a BPE vocab derived from tokens.txt, and
      then they work: "Spot Tuber" → "spotube". Findings §1a, §1f.)*
- [x] Record: acquisition (download or system), packages exercised, cold
      build time, binary size.
- [x] **If coexistence fails**, pick between: (a) sherpa `shared` + `ort`
      `load-dynamic` against the same `libonnxruntime.so`; (b) run Kokoro
      through sherpa-onnx's own TTS API; (c) STT in a separate process. The
      decision goes in the findings before §3.5 starts. *(None of the
      three: a fourth option, one shared static runtime, works. The
      licensing side effect is findings §1e.)*

**DoD:** findings §1 with the verdict, the numbers, and the coexistence
answer.

### 3.2 Capture (`cosmo-audio`, input half)

- [x] Native PipeWire capture stream (RT data loop), mono f32 at 16 kHz,
      where PipeWire resamples, not us. Continuous, into a lock-free
      pre-roll ring (≥750 ms), with the design lifted from cosmic-voice's
      `audio.rs`: atomic samples and absolute positions.
- [x] **Half-duplex at the ring** (invariant #8): samples captured while
      `SpeechGate::mic_open(now, SETTLE)` is false are zeroed, so the ring
      never holds cosmo's own voice. This consults the gate §2.3 built; it
      doesn't create a second one.
- [x] Reconnect when the source goes away. The mic target is overridable by
      env var for tests, like `COSMO_SINK`.

**DoD:** an example records 3 s through the ring, and a `play_wav` of the
result is your voice. With a clip playing, the ring stays silent for the
clip plus 350 ms. *(done 2026-09-26. "Your voice" was proven with an
acoustic loop: Kokoro out of the speaker, into the mic, then sherpa, which
transcribed it correctly. The gate was silent from 0.98 s to 2.37 s against
an expected 1.00 s to 2.37 s. Findings §2.)*

### 3.3 VAD and segmentation

- [ ] Silero VAD through `sherpa_onnx` (cosmic-voice shipped an energy
      placeholder). It serves two jobs: **segment cuts** inside long
      utterances (the offline pass costs more than linearly in length), and a
      **silence backstop** that ends a recording if a key release is lost.
- [ ] Cuts land only inside silence. Tested on a synthetic
      speech-silence-speech buffer.

### 3.4 Hotkey (`cosmo-hotkey`)

- [x] evdev directly (the fd is ours; `EVIOCSMASK` belongs visibly at the
      open site): `EVIOCGRAB` + a mask containing only the trigger. Filter
      autorepeat (`value == 2`, phase-0 finding). No root, no `input` group.
      *(**No `EVIOCGRAB`**: it leaks the press anyway and swallows the
      release, which leaves a modifier stuck. Findings §3a.)*
- [x] **Hotplug via inotify** on `/dev/input` (`IN_CREATE`, plus `IN_ATTRIB`
      because logind's ACL lands just after the node appears). A replugged
      keyboard re-attaches.
- [x] **The trigger key decision** *(Right Ctrl, the user's choice; holds
      under 300 ms are discarded as taps or shortcuts, §3.7)* (phase-0 findings §7, open): a config key
      `trigger_key`, plus a `cosmo trigger capture` that listens for the next
      key press (a user-initiated window, mask off for at most 10 s, as
      cosmic-voice does). The default must be a key with no common binding
      on this hardware. Chosen with the user, not hardcoded silently.
- [ ] **`cosmo toggle` naming conflict.** Phase 1 made `toggle` mean
      pause/resume. The press-only listening fallback becomes `cosmo listen`
      (start/stop), bindable as a COSMIC `Spawn` shortcut.

### 3.5 STT (`cosmo-stt`)

- [ ] Two resident models on their own worker threads, following
      cosmic-voice's `asr.rs`: **streaming** (greedy, around 560 ms chunks)
      producing partials, and **offline** (modified beam search + hotwords)
      producing the commit. Thread counts are configurable (`asr_threads`,
      `offline_threads` ≤ 4).
- [ ] **Segmented decoding**: each finished segment (§3.3) is decoded while
      the next is still being spoken.
- [ ] Hotwords: the reflex vocabulary plus installed app names (from
      `.desktop` files), keyed by the focused `app_id` from `cosmo-focus`.
      The mechanism lands now; phase 4 owns the final list.
- [ ] `scripts/fetch-models` grows `--asr`: pinned SHA-256s, the same
      discipline as Kokoro.

### 3.6 `scripts/bench-asr`: the model choice, on data

- [ ] Record your own command set, then run each candidate on it: WER on
      commands, latency from release to commit, and resident memory.
      Candidates: `parakeet-unified-en-0.6b` (cosmic-voice's commit model),
      `parakeet-tdt-0.6b-v2`, and the **110M** transducer (blueprint §16:
      commands may want a smaller model and no offline pass).
- [ ] The decision and the numbers go in the findings. `doctor` names the
      models in use.

### 3.7 Daemon wiring

- [ ] `Listening` becomes real: key down → `Listening` (partials stream as
      `Event::Transcript { text, final: false }`); release →
      `Thinking`-free `Idle` with a final `Event::Transcript { final: true }`.
      The CLI's `cosmo listen` / `cosmo transcripts` prints them.
- [ ] The trigger during `Speaking` interrupts playback (it's a new
      utterance). The mic opens after the settle window.
- [ ] Latency spans: `transcript` (release → commit), the phase-1 agreed
      name.

### 3.8 DoD and `doctor`

- [ ] Hold the key, speak, release → a final transcript, with partials
      visible meanwhile.
- [ ] **The saturated-machine test:** `cargo build --release` of the
      workspace from clean while talking, with no dropped or clipped audio.
      Measured by capture-loop underrun counters, not by ear alone.
- [ ] `doctor`: evdev node + uaccess ACL, trigger key attached, capture
      stream up, both models resident (plus their names and load times).

## Not in phase 3 (so nobody scope-creeps it)

- **Transcripts becoming turns.** A mic transcript fed to `Engine::say`
  would let a *spoken* "confirm" resolve a hold via the gate's
  whole-utterance confirm. The README invariant says spoken-only
  confirmation is forgeable (anything that reaches the mic, including
  cosmo's own speakers). Phase 4/5 wires transcripts → reflex → reasoning
  **with the spoken-confirm path closed** and a test proving it.
- The reflex matcher (phase 4); wake word (phase 7); overlay rendering of
  partials (phase 6; the events land now).

## Dependency order

```
3.1 link spike ──→ 3.5 STT ──┐
3.2 capture ──→ 3.3 VAD ─────┼─→ 3.7 wiring ──→ 3.8 DoD
3.4 hotkey ──────────────────┘        ↑
3.6 bench-asr (needs 3.2 + 3.5) ──────┘ (picks the models 3.7 loads)
```

3.2 and 3.4 are independent of the spike and can proceed in parallel with
it.
