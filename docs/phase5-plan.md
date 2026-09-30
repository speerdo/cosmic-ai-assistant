# cosmo — phase 5 spec: the reasoning voice

**Derives from:** `docs/implementation-plan.md` §Phase 5, `docs/cosmo-blueprint.md`
§2 (budgets: reasoning turn 1.5–2.5 s), §4 ("Streaming on the reasoning
path"), §7 (gate)
**Drafted:** 2026-09-30
**Status:** §5.1–§5.7 done 2026-09-30 (findings). Open: §5.8, the live run, which needs the user's API key.

Goal: **anything reflex can't do is answered by the model, spoken as it
streams**: first audio a few hundred milliseconds after the model starts
answering, the whole turn inside 1.5–2.5 s, with every tool call gated.

## Decisions carried in

- **The streaming chat API, not the Realtime API (user, 2026-09-30).** The
  plan's "Realtime API, text-out" dates from the speech-to-speech design.
  With local STT and cosmo's own voice, Realtime would run text-in,
  text-out: its one mode with no advantage, at 4–40× the per-token price
  (checked 2026-09-30: gpt-realtime-2.1-mini $0.60/$2.40, gpt-realtime-2.1
  $4/$24, gpt-4o-mini $0.15/$0.60 per million tokens). Streaming and
  sentence-by-sentence speech work the same on the chat API. The model
  stays `model` in `config.ron` (default gpt-4o-mini).
- **Spoken confirmation needs the key held** (phase-4 decision; gate
  invariant #5). Phase 5 proves it through the reasoning path end to end.
- **No key is stored on this machine yet** (`cosmo auth status`). Everything
  is built and tested against a local fake OpenAI server; the live run
  (phase-2 carry-over E1) waits for the user's key, because it costs money.

## Parts

### 5.1 Streaming (`cosmo-reason`)

- [x] `stream: true` with `stream_options.include_usage`: server-sent
      events parsed incrementally. Text deltas go out as they arrive; tool
      calls are assembled from their deltas (by index: id, name, argument
      fragments).
- [x] Gate invariant #1 unchanged: confirmation language anywhere in the
      response's text still escalates that response's gated call to Deny,
      judged on the full text once the response is complete.
- [x] A malformed or cut-off stream is an error with a message, never a
      half-applied tool call.

### 5.2 A warm connection

- [x] One `Reasoner` for the daemon, created on the first reasoning turn
      (the key is still resolved lazily) and reused: the TLS connection
      stays warm, instead of a handshake per turn as today.

### 5.3 Sentence-streamed speech

- [x] Streamed text feeds the phase-2 sentence splitter; each finished
      sentence is synthesized and queued while the model is still writing
      the next. The ack for a tool call ("Opening it.") still comes from
      the cache.
- [x] Measured with a fake server that streams at a realistic pace and
      real Kokoro: model's first token → first audio. Blueprint target:
      a few hundred ms.

### 5.4 Half-duplex on the live path; barge-in behind config

- [x] Half-duplex already gates the ring (phase 3). Verify it holds while
      a streamed reply is spoken sentence by sentence (gaps between
      sentences must not open the mic).
- [x] `barge_in` config key (default off): the mic stays open while cosmo
      speaks, for headset users. `doctor` warns when it's on and the output
      isn't a headset.

### 5.5 Confirmation end to end

- [x] Through the reasoning path, with a fake model: a gated call is held;
      an open-mic "confirm that" completes nothing; a key-held confirm, or
      `cosmo confirm`, executes it locally without a model call.

### 5.6 Token discipline

- [x] Every reasoning turn logs its usage (from the stream's final chunk)
      and the server's rate-limit headers, and emits `Event::Usage`.
- [x] A test proves reflex turns make **zero** API requests.

### 5.7 `remember` in the prompt

- [x] The `remember` file is loaded into each turn's static prompt, capped
      so the prompt stays under its 3,000-token budget (oldest entries
      dropped first, and said so in the log).

### 5.8 Live (needs the user's key)

- [ ] `cosmo auth login`, then a spoken question end to end: time to first
      audio and the whole turn, against the 1.5–2.5 s budget; usage and
      rate limits in the log. This is also phase 2's E1.

## Not in phase 5

- The overlay (phase 6) renders the events this phase emits; the wake word
  (phase 7) inherits invariant #5 through `UtteranceSource::OpenMic`.
