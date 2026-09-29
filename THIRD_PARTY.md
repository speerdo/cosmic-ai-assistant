# Third-party components and their terms

cosmo's own source is MIT (see `LICENSE`). This file records what else goes
into cosmo, under what terms, and what that asks of anyone who builds or
redistributes it. It was audited on 2026-09-29 (phase-3 findings §9).

**The rule it follows:** nothing linked into a cosmo binary is under a
copyleft license that would bind the binary as a whole, and anything cosmo
downloads for the user is used within its license. Re-check this file
whenever a dependency, native library or model is added.

## Linked into the binaries

### Rust crates

Every crate in the dependency graph (with `--features ears`) is under a
permissive license: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unlicense,
Unicode-3.0 or CC0, most offering a choice among several. There are three
worth naming:

| Crate | License | Note |
|---|---|---|
| `libcosmic` (applet only) | MPL-2.0 | File-level copyleft: fine to link; modifications to its own files would have to be shared. cosmo doesn't modify it. |
| `webpki-roots`, `webpki-root-certs` | CDLA-Permissive-2.0 | Permissive data license (Mozilla's CA list). |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1+ | Used under MIT/Apache. |

**Removed on 2026-09-29:** `cosmic-protocols`, which is GPL-3.0-only.
`cosmo-focus` now generates its own bindings (see "Vendored" below).

### Native libraries (heavy tier: features `kokoro`, `sherpa`, `speech`, `ears`)

`scripts/fetch-native` **builds sherpa-onnx from source with TTS turned
off**. sherpa's prebuilt bundle statically links eSpeak NG (GPL-3.0) even
into a speech-recognition-only binary; this build has none of it, and a
symbol check of `cosmod` finds no eSpeak NG code.

| Library | License |
|---|---|
| sherpa-onnx 1.13.8 | Apache-2.0 |
| ONNX Runtime 1.28.2 (static, prebuilt by the sherpa project) | MIT |
| kaldi-native-fbank, kaldi-decoder, kaldifst | Apache-2.0 |
| OpenFst | Apache-2.0 |
| simple-sentencepiece | Apache-2.0 |
| kissfft | BSD-3-Clause |
| hclust-cpp (fastcluster) | BSD-2-Clause |
| Eigen 5.0.1 (headers; sherpa includes only `Eigen/Dense`) | MPL-2.0. Its one file of LGPL-derived code, `IncompleteLUT.h`, was relicensed to MPL-2.0 by its author, and sherpa doesn't include it. |
| nlohmann/json (headers) | MIT |

`libespeak-ng.a`, `libpiper_phonemize.a` and `libucd.a` in `.native/` are
**empty placeholder archives**: the `sherpa-onnx-sys` crate names them on
its link line, and they contain no code.

## Vendored into the repository

| File | Terms |
|---|---|
| `crates/cosmo-focus/protocols/cosmic-toplevel-info-unstable-v1.xml`, `cosmic-workspace-unstable-v1.xml` | The HPND-style permission notice in each file (copyright Ilia Bozhinov, Isaac Freund, Christopher Billington, Victoria Brekenfeld), reproduced unchanged. Only these XML files are used; the bindings are generated from them with `wayland-scanner` (MIT). |

## Loaded at run time, not linked

| Component | License | How |
|---|---|---|
| eSpeak NG (`libespeak-ng.so.1`, the distribution's package) | GPL-3.0 | Kokoro's phonemizer. Opened with `dlopen` when Kokoro starts. cosmo neither links nor ships it; the user's system provides it. cosmo's MIT source is GPL-compatible. |

## Downloaded by the scripts, never in the repository

`scripts/fetch-models` downloads these into `~/.cache/cosmo/models/`,
checks each against a pinned SHA-256, and writes a `NOTICE.txt` beside
each speech model.

| Model | License | Obligations |
|---|---|---|
| Kokoro-82M v1.0 (ONNX export, voices) | Apache-2.0 | Notice if redistributed. |
| Silero VAD | MIT | Notice if redistributed. |
| nemotron-speech-streaming-en-0.6b (default streaming model) | [NVIDIA Open Model License](https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/) (2025-10-24) | Use, including commercial use, is permitted. **Redistribution** needs a notice ("Licensed by NVIDIA Corporation under the NVIDIA Open Model License") and a copy of the agreement. Rights end if safety guardrails are bypassed or if you sue over the model. Also subject to NVIDIA's Trustworthy AI terms. |
| parakeet-unified-en-0.6b (default offline model) | NVIDIA Open Model License | As above. |
| parakeet-tdt-0.6b-v2, parakeet-tdt 110M, fastconformer streaming (bench only) | CC-BY-4.0 | Attribution if redistributed. |

cosmo doesn't redistribute any model today: the user's own machine
downloads them. The files are sherpa-onnx's int8 ONNX exports, taken from
sherpa's GitHub releases.

## Before anything is packaged (phase 8)

- A binary distribution must carry the license texts and notices of what
  it links: the permissive licenses above require that much. Generate
  them from the dependency graph (for example with `cargo about`) rather
  than by hand.
- If a package **bundles** the NVIDIA models, include each model's
  `NOTICE.txt` and a copy of the NVIDIA Open Model License Agreement.
- eSpeak NG stays a separate system package, never bundled into cosmo's
  own files.

## Data that stays local

The user's own recordings (`scripts/bench-asr`, in
`~/.local/share/cosmo/bench/`) and model caches are never committed.
