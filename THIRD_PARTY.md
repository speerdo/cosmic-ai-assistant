# Third-party components and their terms

cosmo's own source is MIT (see `LICENSE`). This file records what else goes
into cosmo, under what terms, and what that asks of anyone who builds or
redistributes it. It was audited on 2026-09-29 (phase-3 findings §9), and again for
packaging on 2026-10-04 (phase-8 findings).

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
| `libcosmic` (overlay, applet) and its `cosmic-config`, `cosmic-theme` | MPL-2.0 | File-level copyleft: fine to link; modifications to its own files would have to be shared. cosmo doesn't modify it. Its iced fork, winit fork, `cryoglyph`, `softbuffer`, `smithay-clipboard`, `window_clipboard` and `freedesktop-icons` are MIT/Apache-2.0; `dbus-settings-bindings` is MPL-2.0; its `cosmic-protocols` revision (c0cff4d) is **MIT**. |
| `cosmic-panel-config`, `xdg-shell-wrapper-config` (**`cosmo-applet` only**) | **GPL-3.0-only** | Pulled in by libcosmic's `applet` feature; every COSMIC panel applet links them. **The user chose a real panel applet on 2026-09-30**, so the `cosmo-applet` binary, as distributed, carries GPL-3.0 terms (its source stays MIT, which is GPL-compatible). `cosmod`, `cosmo` and `cosmo-overlay` don't link them: `cargo tree` checked. |
| `webpki-roots`, `webpki-root-certs` | CDLA-Permissive-2.0 | Permissive data license (Mozilla's CA list). |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1+ | Used under MIT/Apache. |

**Removed on 2026-09-29:** `cosmic-protocols` **0.2.0 from crates.io**, which is GPL-3.0-only (the git revision libcosmic uses since is MIT).
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
| `crates/cosmo-focus/protocols/cosmic-toplevel-info-unstable-v1.xml`, `cosmic-workspace-unstable-v1.xml`, `cosmic-toplevel-management-unstable-v1.xml` | The HPND-style permission notice in each file (copyright Ilia Bozhinov, Isaac Freund, Christopher Billington, wb9688, Victoria Brekenfeld), reproduced unchanged. Only these XML files are used; the bindings are generated from them with `wayland-scanner` (MIT). |

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

## In the packages (phase 8, 2026-10-04)

`packaging/` builds two packages per distribution (`packaging/README.md`):

- **`cosmo`** (cosmod, cosmo, cosmo-overlay): MIT, plus the licences of
  what it links. `/usr/share/doc/cosmo/third-party-licenses.txt` (Fedora:
  the `%license` directory) holds every licence text with the crates and
  native libraries that use it, generated from `Cargo.lock` by
  `packaging/gen-licenses` (`cargo about`), with the native libraries'
  own licence files collected by `scripts/fetch-native` (ONNX Runtime's
  `LICENSE` and `ThirdPartyNotices.txt`, which its prebuilt archive
  lacks, are fetched from upstream at the same version, pinned by
  checksum). The generation **refuses GPL** for these three binaries.
- **`cosmo-applet`**: declared GPL-3.0-only as distributed, with the
  source pointed to at the exact commit, and the GPL text included.
- **libcosmic embeds two fonts** (Open Sans, Noto Sans Mono) under the
  SIL Open Font License 1.1, which allows embedding as long as the licence
  goes with them. It does, in the notices above. libcosmic's CC-BY-SA-4.0
  icons are embedded only on non-unix targets, so they aren't in these
  binaries.
- **No model is packaged.** Each user runs `cosmo models fetch`, so the
  NVIDIA notice duty (on redistribution) still never arises. If a package
  ever bundles them, include each model's `NOTICE.txt` and a copy of the
  NVIDIA Open Model License Agreement.
- eSpeak NG is a package dependency (`libespeak-ng1` / `espeak-ng`),
  never bundled.

## Data that stays local

The user's own recordings (`scripts/bench-asr`, in
`~/.local/share/cosmo/bench/`) and model caches are never committed.
