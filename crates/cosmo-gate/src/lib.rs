//! The policy gate: `Allow` / `Hold` / `Deny` verdicts for every tool call,
//! MCP and native alike.
//!
//! Deny outright: `rm -rf`, `dd`, `mkfs`, `sudo`, `pkexec`, `ssh`, `passwd`,
//! curl piped to a shell, `git push`. Hold: shutdown, reboot, suspend,
//! package installs, config resets, closing everything. Map MCP
//! `ToolAnnotations` `destructiveHint=true` to hold — annotations are hints,
//! and cosmo is the host that actually asks.
//!
//! ## Invariants
//!
//! 1. A gated tool call and a confirmation in the **same model response** are
//!    rejected outright.
//! 2. Confirmation only takes effect after a genuinely **new user turn**.
//! 3. The confirm phrase is matched as a **whole utterance**, so "don't
//!    confirm that" does not confirm.
//! 4. The **local** confirm path (overlay click, `cosmo confirm`) never asks
//!    the model. A spoken-only confirmation is forgeable by anything that
//!    reaches the microphone, including your own speakers.
