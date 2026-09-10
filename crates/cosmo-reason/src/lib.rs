//! The reasoning path.
//!
//! - **v1 (phase 1):** chat-completions client with a tool loop — enough for
//!   `cosmo say` to execute real commands before audio exists.
//! - **v2 (phase 5):** Realtime API over `tokio-tungstenite`, **text-out**
//!   only. The Realtime voice catalogue has no en-GB/en-AU voices and cannot
//!   be changed mid-session, so cosmo does the speaking.
//!
//! ## Token discipline
//!
//! Every turn re-sends the prompt and it counts against tokens per minute
//! whether or not it was cached.
//!
//! - No live desktop state in the prompt: windows and workspaces come from a
//!   tool call.
//! - Filter the MCP tool list (see `cosmo-mcp`).
//! - Static prompt under 3,000 tokens.
//! - Log the server-reported rate limit every turn.
