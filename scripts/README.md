# scripts

Benchmarks and test harnesses, populated from phase 0 onward:

- `bench-asr` (phase 3): candidate ASR models against your own command set.
- `bench-reflex` (phase 4): end-to-end reflex timing against the 150ms budget.
- Nested-compositor harness for overlay testing.
- `fetch-native` (phase 3): run once before any heavy-tier build. It
  fetches sherpa-onnx's static bundle into `.native/`, whose ONNX Runtime
  both sherpa-onnx and `ort` (Kokoro) link through `.cargo/config.toml`.
- `fetch-models` (phase 2): Kokoro-82M (ONNX) + its tokenizer and English
  voice packs into `~/.cache/cosmo/models/kokoro-v1.0/`, pinned to one
  revision, SHA-256 checked, idempotent. `--variant fp32|fp16|q8`.
