//! `cosmod` — the cosmo daemon binary.
//!
//! Dev loop (plan §1.1):
//!
//! ```text
//! RUST_LOG=debug cargo run --bin cosmod
//! cargo run --bin cosmo -- say "..."
//! ```

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rmcp logs the agent's whole peer_info (several KB) at INFO on each start.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    cosmo_daemon::run().await
}
