# Phase 5 findings

**Started:** 2026-09-30
**Scope so far:** everything except the live run, which needs the user's
API key (§5.8, and phase 2's E1).

## §1. The API: streaming chat, not Realtime (the user's decision)

The plan said "Realtime API, text-out". Pricing checked on 2026-09-30, per
million tokens:

| Model | Input | Output |
|---|---|---|
| gpt-4o-mini (chat; cosmo's default) | $0.15 | $0.60 |
| gpt-realtime-2.1-mini | $0.60 | $2.40 |
| gpt-realtime-2.1 | $4.00 | $24.00 |

Local STT and cosmo's own voice leave Realtime running text-in, text-out:
its one mode with no advantage, at 4–40× the price. Streaming and
sentence-by-sentence speech work the same on the chat API. The user chose
the streaming chat API. The current Realtime shapes (`output_modalities:
["text"]`, `response.output_text.delta`, function calls in
`response.done`) are noted here in case it's revisited.

## §2. Streaming (§5.1)

`cosmo-reason` sends `stream: true` with `stream_options.include_usage`.
Two pure pieces (`stream.rs`) turn the response into the same assistant
message a non-streamed one held, so the tool loop and the gate after it
are unchanged:

- `SseDecoder`: bytes → event payloads. A test splits a stream at every
  byte offset, including inside a multi-byte character, and gets the same
  events every time.
- `Accumulator`: payloads → text deltas as they arrive, plus tool calls
  rebuilt from fragments by index. A stream that ends without a finish
  reason is an **error**: a cut-off `rm -rf ~/scratch` never becomes a
  half-built call. An `error` object mid-stream is reported.
- A server answering with plain JSON is still accepted, so the older
  fake-server tests now cover the non-streamed path.

Tested against a fake that speaks server-sent events at a model's pace
(`tests/stream_loop.rs`): the first words arrive well before the turn
ends, tool calls reassemble and run, usage adds up across round trips,
the rate-limit headers are read, and **invariant #1 holds on a stream**:
"Confirmed," in one chunk and the gated call in a later one still makes
it Deny.

## §3. A warm connection (§5.2)

The engine built a fresh `Reasoner`, and with it a fresh HTTP client, for
**every** turn, so each turn paid a TLS handshake to OpenAI. Now one
`Reasoner` is created on the first reasoning turn (the key is still
resolved lazily; a missing key is retried next turn) and kept. Its
connection stays warm. Not measurable without a key; the saving is a
handshake, typically 100–300 ms.

## §4. Sentence-streamed speech (§5.3)

`Speech::speak_stream` takes text as it streams. Each complete sentence
(the phase-2 splitter) is synthesized and queued while the model writes
the next. The last piece is held until more text arrives, because it may
still grow ("…Mr." + " Smith"). In a live stream that costs one token's
wait. A key press interrupts it mid-reply like any speech.

Measured with real Kokoro and a fake model streaming a 24-word,
three-sentence reply (300 ms to the first token, then 25 ms a word).
Model's first token → first audio queued, three runs:

| | first audio | reply fully streamed at |
|---|---|---|
| **streamed** | **543–558 ms** | ~630 ms |
| whole reply (before) | 2,135–2,182 ms | ~630 ms |

Speech starts about **4× sooner**, before the model has finished writing.
The 550 ms is the first six-word sentence arriving (~175 ms) plus Kokoro
synthesizing it (~375 ms). That's within the blueprint's "a few hundred
ms", at its upper end. If it matters live, a shorter first chunk (split
the first sentence at a comma) would trim it. `examples/bench_stream.rs`
re-measures.

## §5. Half-duplex and barge-in (§5.4)

Half-duplex holds on the streamed path by construction: capture is gated
while cosmo is audible, and the only way to be recording is a key press,
which interrupts speech. New config key **`barge_in`** (default false)
keeps the mic open while cosmo speaks, for headsets. `doctor`'s `ears`
line warns, and fails, while it's on.

## §6. Confirmation, token discipline, memory (§5.5–§5.7)

Through the engine, against the fake streaming server
(`tests/reasoning.rs`):

- **§5.5:** the model streams a gated `systemctl reboot`; it's held. An
  open-mic "Confirm that." returns `ConfirmNeedsKey`; a key-held one runs
  it locally. **Neither confirm attempt makes a model request.**
- **§5.6:** four reflex commands make **zero** API requests; a reasoning
  turn emits `Event::Usage` with its tokens and the rate-limit headers.
  That event was defined in phase 1 and never sent until now.
- **§5.7:** the `remember` file is read fresh before each reasoning turn
  and appended to the system prompt as notes (framed as facts, not
  instructions), newest first within the 3,000-token budget. Oldest
  entries are dropped, and logged, when it's full. A test puts two notes
  in a temporary state dir and finds them in the prompt the model
  received.

The engine now takes its tool host as `Arc<dyn ToolHost>` (with the
agent's tool count kept for `doctor`), so tests can reach the reasoning
path without an MCP agent.

## §7. Along the way

- **An ears test failed deterministically**: a 690 ms hold measured as
  220 ms and discarded as a tap. The harness started the fake mic's clock
  *before* loading the speech models (~2 s for whichever test goes first),
  so that test pressed its key late. Phase 5 changed the order the tests
  took their lock, not the controller. The rig now loads models first.
  The tap discard now logs the hold it measured, which is what found
  this.
- Env-var writes in tests are `unsafe` in Rust 2024: each is guarded by a
  test lock and a SAFETY note, and allowed explicitly, block by block.

## Open

- **§5.8, the live run**: `cosmo auth login`, then a spoken question end to
  end against the 1.5–2.5 s budget. It costs money, so it waits for the
  user's key.
