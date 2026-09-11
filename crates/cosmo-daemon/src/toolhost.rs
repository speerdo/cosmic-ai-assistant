//! The daemon's tool host: one flat namespace combining the MCP agent
//! tools (`cosmo-mcp`) and the native tools (`cosmo-tools`), with
//! schemas/annotations for the reasoning loop and gated execution.

use std::sync::Arc;

use serde_json::Value;

use cosmo_gate::Annotations;
use cosmo_mcp::McpHost;
use cosmo_reason::tools::{ToolHost, function_schema};
use cosmo_tools::Terminal;

/// The combined host. Agent tools execute over the MCP host; native tools
/// run in-process.
pub struct DaemonToolHost {
    agent: Arc<McpHost>,
    terminal: Terminal,
    announcer: cosmo_tools::announce::Announcer,
}

impl DaemonToolHost {
    /// Agent-tool count for `cosmo doctor`.
    pub fn agent_tool_count(&self) -> usize {
        self.agent.registered_tools().len()
    }

    pub fn new(agent: Arc<McpHost>, tmux_session: &str) -> Self {
        Self {
            agent,
            terminal: Terminal::new(tmux_session.to_string()),
            announcer: cosmo_tools::announce::Announcer::new(),
        }
    }
}

/// The terminal tools' schema parameters.
fn terminal_params() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "The shell command to run"},
            "max_secs": {"type": "integer", "description": "watch_terminal: max seconds to wait"}
        },
        "required": []
    })
}

impl ToolHost for DaemonToolHost {
    fn tool_schemas(&self) -> Vec<Value> {
        let mut schemas = Vec::new();
        // Agent tools first (already allowlisted; run_shell never here).
        //
        // The agent's own `inputSchema` goes through verbatim. Substituting a
        // bare `{"type": "object"}` here tells the model that `click` exists
        // but nothing about `x`/`y` — it then calls tools with invented
        // arguments, which looks like a model failure and is ours.
        for tool in self.agent.registered_tools() {
            schemas.push(function_schema(
                &tool.name,
                &tool.description,
                tool.input_schema.clone(),
            ));
        }
        // Native tools.
        schemas.push(function_schema(
            "run_in_terminal",
            "Run a shell command in cosmo's own tmux session and return the transcript so far.",
            terminal_params(),
        ));
        schemas.push(function_schema(
            "read_terminal",
            "Read the current tmux transcript.",
            serde_json::json!({"type": "object", "properties": {}}),
        ));
        schemas.push(function_schema(
            "watch_terminal",
            "Wait until the running command returns to the shell, then return the transcript.",
            terminal_params(),
        ));
        schemas.push(function_schema(
            "announce",
            "Deliver an unprompted message to the user (min 8s spacing enforced).",
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        ));
        schemas.push(function_schema(
            "remember",
            "Remember one fact across sessions (one line).",
            serde_json::json!({"type": "object", "properties": {"line": {"type": "string"}}, "required": ["line"]}),
        ));
        schemas.push(function_schema(
            "recall",
            "Read everything remembered.",
            serde_json::json!({"type": "object", "properties": {}}),
        ));
        schemas.push(function_schema(
            "system_query",
            "Read-only system facts: one of disk, memory, network, failed_services, failed_user_services, sensors, uptime.",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
        ));
        schemas.push(function_schema(
            "media_control",
            "MPRIS media control: play, pause, play_pause, next, previous, stop.",
            serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        ));
        schemas.push(function_schema(
            "clipboard_get",
            "Read the user's clipboard as text. Held for confirmation: the \
             contents leave the machine.",
            serde_json::json!({"type": "object", "properties": {}}),
        ));
        schemas.push(function_schema(
            "clipboard_set",
            "Replace the user's clipboard contents with the given text.",
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        ));
        schemas
    }

    fn annotations_of(&self, tool: &str) -> Annotations {
        if let Some(registered) = self
            .agent
            .registered_tools()
            .into_iter()
            .find(|t| t.name == tool)
        {
            return Annotations {
                read_only: registered.read_only,
                destructive: registered.destructive,
            };
        }
        // Native defaults (gate may be stricter — its lists decide).
        match tool {
            "read_terminal" | "recall" | "system_query" => Annotations {
                read_only: true,
                destructive: false,
            },
            "run_in_terminal" | "watch_terminal" | "announce" | "remember" | "media_control"
            | "clipboard_get" | "clipboard_set" => Annotations {
                read_only: false,
                destructive: false,
            },
            // Unknown ⇒ conservative, and `Annotations::default()` is now
            // genuinely that: `destructive = true` ⇒ Hold. It used to be a
            // derived `false`/`false`, i.e. Allow, under this same comment.
            _ => Annotations::default(),
        }
    }

    fn execute(
        &self,
        tool: &str,
        args: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>> {
        let agent = Arc::clone(&self.agent);
        let tool = tool.to_string();
        Box::pin(async move {
            let terminal = &self.terminal;
            let announcer = &self.announcer;
            match tool.as_str() {
                // Native tools.
                "run_in_terminal" => {
                    let command = args["command"].as_str().unwrap_or_default().to_string();
                    run_tool(terminal.run(&command).await)
                }
                "read_terminal" => run_tool(terminal.read().await),
                "watch_terminal" => {
                    let secs = args["max_secs"].as_u64().unwrap_or(30);
                    run_tool(terminal.watch(secs).await)
                }
                "announce" => {
                    let text = args["text"].as_str().unwrap_or_default().to_string();
                    announcer.announce(&text).await;
                    "announced".into()
                }
                "remember" => {
                    let line = args["line"].as_str().unwrap_or_default().to_string();
                    run_tool(cosmo_tools::memory::add(&line).await)
                }
                "recall" => run_tool(cosmo_tools::memory::read().await),
                "system_query" => {
                    let query = args["query"].as_str().unwrap_or_default().to_string();
                    run_tool(cosmo_tools::system::query(&query).await)
                }
                "media_control" => {
                    let cmd = args["command"].as_str().unwrap_or_default().to_string();
                    run_tool(cosmo_tools::media::control(&cmd).await)
                }
                "clipboard_get" => run_tool(cosmo_tools::clipboard::get().await),
                "clipboard_set" => {
                    let text = args["text"].as_str().unwrap_or_default().to_string();
                    run_tool(cosmo_tools::clipboard::set(&text).await)
                }
                // Agent tools.
                other => match args.as_object() {
                    Some(obj) => match agent.call_agent_tool(other, obj.clone()).await {
                        Ok(text) => text,
                        Err(e) => format!("tool error: {e}"),
                    },
                    None => "tool error: arguments must be an object".into(),
                },
            }
        })
    }
}

fn run_tool(result: Result<String, cosmo_tools::ToolError>) -> String {
    match result {
        Ok(text) => text,
        Err(e) => format!("tool error: {e}"),
    }
}
