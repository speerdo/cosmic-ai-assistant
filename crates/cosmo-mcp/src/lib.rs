//! MCP host on `rmcp`: spawns `computer-use-linux` over stdio, filters its
//! tool list down to an allowlist, and registers cosmo's native tools
//! alongside.
//!
//! ## Invariants
//!
//! - `run_shell` is never registered. It is absent unless
//!   `COMPUTER_USE_LINUX_ENABLE_SHELL=1`, and it stays absent.
//! - Tool filtering matters: about a dozen of its twenty-odd tools, never all
//!   of them every turn.
//! - Adding another MCP server later is config, not code.
