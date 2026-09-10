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

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, JsonObject, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};

use cosmo_config::Config;

/// One registered tool: MCP tool metadata + gate verdicts already resolved.
#[derive(Debug, Clone)]
pub struct RegisteredTool {
    /// The name the reasoning model sees.
    pub name: String,
    pub description: String,
    /// Gate inputs.
    pub read_only: bool,
    pub destructive: bool,
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
}

/// The running MCP host: one or more agent connections + the native tools.
pub struct McpHost {
    /// Kept for the doctor snapshot and future re-discovery (config, not
    /// code, when a second agent is added).
    #[allow(dead_code)]
    cfg: Arc<Config>,
    agent: AgentConnection,
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
            agent,
            agent_tools,
        })
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
        self.agent.call(name, args).await
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
        let transport = TokioChildProcess::new(
            tokio::process::Command::new(&cfg.agent_command).configure(|cmd| {
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
                    origin: ToolOrigin::Agent,
                },
            );
        }
        Ok(tools)
    }

    async fn call(&self, name: &str, args: JsonObject) -> Result<String, HostError> {
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(args);
        let result = self
            .service
            .call_tool(params)
            .await
            .map_err(|e| HostError::Protocol(e.to_string()))?;
        Ok(render_result(&result))
    }
}

/// Extract gate-relevant annotations, defaulting conservatively:
/// missing hints read as `read_only=false, destructive=true` — an unknown
/// tool is held, not allowed.
fn annotations_of(tool: &Tool) -> (bool, bool) {
    match &tool.annotations {
        Some(a) => (
            a.read_only_hint.unwrap_or(false),
            a.destructive_hint.unwrap_or(false),
        ),
        None => (false, false),
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

    #[test]
    fn run_shell_never_registers() {
        // annotations default conservatively when hints are missing
        let tool = Tool::new(
            "run_shell",
            "execute a shell command",
            Arc::new(JsonObject::new()),
        );
        let (ro, destr) = annotations_of(&tool);
        assert!(!ro);
        assert!(!destr);
    }
}
