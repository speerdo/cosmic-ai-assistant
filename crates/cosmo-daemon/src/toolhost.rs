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
    desktop: Option<DesktopTools>,
}

/// The reflex path's desktop actions (launch, focus, workspaces, window
/// state), offered to reasoning as tools. They run through the very same
/// actuator, so a reasoning call and a reflex command behave identically.
pub struct DesktopTools {
    pub matcher: Arc<cosmo_reflex::Matcher>,
    pub actuator: Arc<dyn crate::reflex::Actuator>,
}

/// How sure an app-name match must be to act on it (a reasoning model
/// passes names like "firefox" or "the files app").
const APP_MATCH: f32 = 0.6;

impl DesktopTools {
    fn app(&self, name: &str) -> Result<cosmo_reflex::AppRef, String> {
        match self.matcher.apps().find(name) {
            Some(m) if m.score >= APP_MATCH => Ok(m.app),
            Some(m) => Err(format!(
                "no installed application clearly matches {name:?} (closest: {})",
                m.app.name
            )),
            None => Err(format!("no installed application matches {name:?}")),
        }
    }

    /// The tool call as a reflex intent, if it is one.
    fn intent(&self, tool: &str, args: &Value) -> Option<Result<cosmo_reflex::Intent, String>> {
        use cosmo_reflex::Intent;
        let app = || self.app(args["app"].as_str().unwrap_or_default());
        let workspace = || {
            args["workspace"]
                .as_u64()
                .filter(|n| (1..=99).contains(n))
                .map(|n| n as u32)
                .ok_or_else(|| "workspace must be a number from 1".to_owned())
        };
        Some(match tool {
            "launch_app" => app().map(Intent::Launch),
            "focus_app" => app().map(Intent::Focus),
            "switch_workspace" => workspace().map(Intent::SwitchWorkspace),
            "move_window_to_workspace" => workspace().map(Intent::MoveToWorkspace),
            "maximize_window" => Ok(Intent::Maximize),
            "minimize_window" => Ok(Intent::Minimize),
            _ => return None,
        })
    }
}

/// The desktop tools' schemas.
fn desktop_schemas() -> Vec<Value> {
    use serde_json::json;
    let app = json!({"type": "object", "properties": {"app": {"type": "string",
        "description": "The application's name, as the user said it (\"firefox\", \"files\")"}},
        "required": ["app"]});
    let workspace = json!({"type": "object", "properties": {"workspace": {"type": "integer",
        "description": "Workspace number, from 1"}}, "required": ["workspace"]});
    vec![
        function_schema(
            "launch_app",
            "Start an installed application. Its window opens on the current workspace.",
            app.clone(),
        ),
        function_schema(
            "focus_app",
            "Bring a running application's window to the front (switching to its workspace).",
            app,
        ),
        function_schema(
            "switch_workspace",
            "Show workspace N. Windows opened afterwards appear there.",
            workspace.clone(),
        ),
        function_schema(
            "move_window_to_workspace",
            "Move the focused window to workspace N.",
            workspace,
        ),
        function_schema(
            "maximize_window",
            "Maximize the focused window.",
            json!({"type": "object", "properties": {}}),
        ),
        function_schema(
            "minimize_window",
            "Minimize the focused window.",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

impl DaemonToolHost {
    /// Agent-tool count for `cosmo doctor`.
    pub fn agent_tool_count(&self) -> usize {
        self.agent.registered_tools().len()
    }

    pub fn new(
        agent: Arc<McpHost>,
        tmux_session: &str,
        announcer: cosmo_tools::announce::Announcer,
        desktop: Option<DesktopTools>,
    ) -> Self {
        Self {
            agent,
            terminal: Terminal::new(tmux_session.to_string()),
            announcer,
            desktop,
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
            "Media players (MPRIS): play, pause, play_pause, next, previous, stop, or \
             status (what each player is doing; check it before saying what's playing). \
             play resumes what was paused.",
            serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        ));
        schemas.push(function_schema(
            "clipboard_get",
            "Read the user's clipboard as text. Held for confirmation: the \
             contents leave the machine.",
            serde_json::json!({"type": "object", "properties": {}}),
        ));
        schemas.push(function_schema(
            "open_url",
            "Open a web address in the user's default browser. To search the web, open the \
             search engine's results URL directly (e.g. \
             https://www.google.com/search?q=cosmic+desktop) rather than clicking and typing. \
             new_window: true opens a fresh window on the current workspace.",
            serde_json::json!({"type": "object", "properties": {
                "url": {"type": "string", "description": "A full http(s) URL, query encoded"},
                "new_window": {"type": "boolean"}
            }, "required": ["url"]}),
        ));
        if self.desktop.is_some() {
            schemas.extend(desktop_schemas());
        }
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
            // The desktop tools are the reflex verbs: Allow, as there.
            "run_in_terminal"
            | "watch_terminal"
            | "announce"
            | "remember"
            | "media_control"
            | "clipboard_get"
            | "clipboard_set"
            | "open_url"
            | "launch_app"
            | "focus_app"
            | "switch_workspace"
            | "move_window_to_workspace"
            | "maximize_window"
            | "minimize_window" => Annotations {
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
                "open_url" => {
                    let url = args["url"].as_str().unwrap_or_default().to_string();
                    let new_window = args["new_window"].as_bool().unwrap_or(false);
                    run_tool(cosmo_tools::browse::open_url(&url, new_window))
                }
                desktop
                    if self
                        .desktop
                        .as_ref()
                        .is_some_and(|d| d.intent(desktop, &args).is_some()) =>
                {
                    let tools = self.desktop.as_ref().expect("checked");
                    match tools.intent(desktop, &args).expect("checked") {
                        Ok(intent) => match tools.actuator.act(&intent).await {
                            Ok(done) => done,
                            Err(e) => format!("tool error: {e}"),
                        },
                        Err(e) => format!("tool error: {e}"),
                    }
                }
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
