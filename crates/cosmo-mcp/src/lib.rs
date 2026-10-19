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

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, JsonObject, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::Value;

use cosmo_config::Config;

/// How long an agent tool call may take before the turn moves on. A portal
/// consent dialog nobody answers, or a hung agent, must not hold the turn.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One registered tool: MCP tool metadata + gate verdicts already resolved.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    /// The name the reasoning model sees.
    pub name: String,
    pub description: String,
    /// Gate inputs.
    pub read_only: bool,
    pub destructive: bool,
    /// The tool's JSON-Schema parameters, exactly as the agent advertised
    /// them. Passed through to the model unchanged: dropping it and sending
    /// a bare `{"type": "object"}` tells the model that `click` exists but
    /// nothing about `x`/`y`, so it calls it with invented arguments.
    pub input_schema: Value,
    /// Where it executes.
    pub origin: ToolOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOrigin {
    /// Spawned MCP agent (`computer-use-linux`).
    Agent,
    /// cosmo native tool (cosmo-tools).
    Native,
}

/// An error from the host layer.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("agent process could not be spawned: {0}")]
    Spawn(String),
    #[error("mcp protocol error: {0}")]
    Protocol(String),
    #[error("tool `{0}` is not in the configured allowlist")]
    NotAllowed(String),
    #[error("tool `{0}` did not answer within {1} s")]
    Timeout(String, u64),
}

/// The running MCP host: one or more agent connections + the native tools.
pub struct McpHost {
    /// Kept to respawn the agent, and for future re-discovery (config, not
    /// code, when a second agent is added).
    cfg: Arc<Config>,
    /// The live connection. Calls clone it out, so a slow call never blocks
    /// the swap to a fresh one.
    agent: tokio::sync::Mutex<Arc<AgentConnection>>,
    /// The last call showed the agent's transport is gone: spawn a new
    /// agent before the next call.
    broken: AtomicBool,
    call_timeout: Duration,
    /// Tools allowed from the agent (config allowlist), keyed by name.
    agent_tools: BTreeMap<String, RegisteredTool>,
}

struct AgentConnection {
    service: RunningService<RoleClient, ()>,
    #[allow(dead_code)]
    server_name: String,
}

impl McpHost {
    /// Spawn the configured agent and discover tools.
    pub async fn connect(cfg: Arc<Config>) -> Result<Self, HostError> {
        let agent = AgentConnection::spawn(&cfg).await?;
        let agent_tools = agent.discover(&cfg.allowed_tools).await?;
        Ok(Self {
            cfg,
            agent: tokio::sync::Mutex::new(Arc::new(agent)),
            broken: AtomicBool::new(false),
            call_timeout: CALL_TIMEOUT,
            agent_tools,
        })
    }

    /// Change how long an agent call may take (tests use a short one).
    pub fn set_call_timeout(&mut self, timeout: Duration) {
        self.call_timeout = timeout;
    }

    /// Snapshot of registered tools (agent + native) for `cosmo doctor`.
    pub fn registered_tools(&self) -> Vec<RegisteredTool> {
        self.agent_tools.values().cloned().collect()
    }

    /// Execute an agent tool call. Returns the raw result text.
    pub async fn call_agent_tool(&self, name: &str, args: JsonObject) -> Result<String, HostError> {
        if !self.agent_tools.contains_key(name) {
            return Err(HostError::NotAllowed(name.to_owned()));
        }
        let agent = self.live_agent().await?;
        match tokio::time::timeout(self.call_timeout, agent.call(name, args)).await {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => {
                if is_transport_failure(&e) {
                    tracing::warn!(error = %e, "MCP agent connection lost; will respawn it");
                    self.broken.store(true, Ordering::SeqCst);
                }
                Err(HostError::Protocol(e.to_string()))
            }
            Err(_) => Err(HostError::Timeout(
                name.to_owned(),
                self.call_timeout.as_secs(),
            )),
        }
    }

    /// The agent connection, spawned afresh first if the last one died.
    async fn live_agent(&self) -> Result<Arc<AgentConnection>, HostError> {
        let mut slot = self.agent.lock().await;
        if self.broken.load(Ordering::SeqCst) || slot.service.is_transport_closed() {
            tracing::info!("respawning MCP agent");
            *slot = Arc::new(AgentConnection::spawn(&self.cfg).await?);
            self.broken.store(false, Ordering::SeqCst);
        }
        Ok(Arc::clone(&slot))
    }

    pub fn shutdown(&self) {
        // rmcp's cancel is cooperative; the RunningService drops its
        // transport on the next tick. Full graceful shutdown happens in
        // `shutdown_graceful` once the daemon owns an async context here.
        tracing::debug!("mcp host shutdown requested");
    }
}

impl AgentConnection {
    async fn spawn(cfg: &Config) -> Result<Self, HostError> {
        // Beyond PATH too: a user unit's PATH has neither nvm nor the npm
        // prefix, and the agent's `node` lives beside it (phase-8 §8.1).
        let found = cosmo_config::locate::agent(&cfg.agent_command).ok_or_else(|| {
            HostError::Spawn(format!(
                "`{}` not found on PATH or in the usual npm locations",
                cfg.agent_command
            ))
        })?;
        tracing::info!(agent = %found.program.display(), "spawning MCP agent");
        let transport = TokioChildProcess::new(
            tokio::process::Command::new(&found.program).configure(|cmd| {
                cmd.env("PATH", &found.path_env);
                for arg in &cfg.agent_args {
                    cmd.arg(arg);
                }
            }),
        )
        .map_err(|e| HostError::Spawn(e.to_string()))?;

        let service = ().serve(transport).await.map_err(|e| HostError::Protocol(e.to_string()))?;
        Ok(Self {
            service,
            server_name: "computer-use-linux".into(),
        })
    }

    /// List tools, map annotations, and filter to the allowlist. `run_shell`
    /// is dropped unconditionally (invariant #2), even if configured.
    async fn discover(
        &self,
        allowlist: &[String],
    ) -> Result<BTreeMap<String, RegisteredTool>, HostError> {
        let listed = self
            .service
            .list_all_tools()
            .await
            .map_err(|e| HostError::Protocol(e.to_string()))?;

        let mut tools = BTreeMap::new();
        for tool in listed {
            if tool.name.as_ref() == "run_shell" {
                // Invariant #2: never register the shell tool, whatever the
                // agent advertises.
                tracing::warn!("agent offered run_shell — refused (invariant #2)");
                continue;
            }
            if !allowlist.iter().any(|a| a == tool.name.as_ref()) {
                continue;
            }
            let (read_only, destructive) = annotations_of(&tool);
            tools.insert(
                tool.name.to_string(),
                RegisteredTool {
                    name: tool.name.to_string(),
                    description: tool.description.unwrap_or_default().to_string(),
                    read_only,
                    destructive,
                    input_schema: Value::Object((*tool.input_schema).clone()),
                    origin: ToolOrigin::Agent,
                },
            );
        }
        Ok(tools)
    }

    async fn call(&self, name: &str, args: JsonObject) -> Result<String, ServiceError> {
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(args);
        let result = self.service.call_tool(params).await?;
        Ok(render_result(&result))
    }
}

/// The agent can't be reached any more (as opposed to answering with an
/// error of its own).
fn is_transport_failure(e: &ServiceError) -> bool {
    matches!(
        e,
        ServiceError::TransportClosed | ServiceError::TransportSend(_)
    )
}

/// Extract gate-relevant annotations, defaulting conservatively: missing
/// hints read as `read_only=false, destructive=true` — an unannotated tool is
/// held, not allowed.
///
/// The defaults are the whole point of this function and they must round
/// towards Hold:
///
/// - `destructiveHint` is defined by the MCP specification as defaulting to
///   **`true`** when absent. `unwrap_or(false)` inverts the protocol's own
///   default, and does it silently.
/// - `readOnlyHint` defaults to `false` — absence is not a claim of safety.
///
/// This is one `unwrap_or` away from being the most permissive line in the
/// codebase: every agent tool that ships without annotations would map to
/// [`cosmo_gate::Verdict::Allow`]. `unannotated_tool_defaults_to_destructive`
/// pins it.
fn annotations_of(tool: &Tool) -> (bool, bool) {
    match &tool.annotations {
        Some(a) => (
            a.read_only_hint.unwrap_or(false),
            a.destructive_hint.unwrap_or(true),
        ),
        None => (false, true),
    }
}

/// Flatten a `CallToolResult` to plain text for the reasoning model.
fn render_result(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            rmcp::model::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Capabilities cosmo advertises: none — we are a tool consumer.
#[allow(dead_code)]
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that ships no annotations at all must map to
    /// `destructive = true` ⇒ [`cosmo_gate::Verdict::Hold`]. The MCP
    /// specification defines `destructiveHint` as defaulting to `true`, and
    /// an authorization system has to round an unknown tool towards Hold
    /// regardless. This previously asserted `!destr` — the assertion encoded
    /// the bug, so every unannotated agent tool was silently allowed.
    #[test]
    fn unannotated_tool_defaults_to_destructive() {
        let tool = Tool::new(
            "run_shell",
            "execute a shell command",
            Arc::new(JsonObject::new()),
        );
        let (ro, destr) = annotations_of(&tool);
        assert!(!ro, "absence of a hint is not a claim of read-only");
        assert!(destr, "absence of a hint must read as destructive");

        // And the gate turns that into a Hold, not an Allow.
        let gate = cosmo_gate::Gate::new();
        gate.set_lock_state(cosmo_gate::LockState::Unlocked);
        let annotations = cosmo_gate::Annotations {
            read_only: ro,
            destructive: destr,
        };
        assert_eq!(
            gate.verdict_for_call("some_unannotated_tool", &Value::Null, &annotations),
            cosmo_gate::Verdict::Hold
        );
    }
}
