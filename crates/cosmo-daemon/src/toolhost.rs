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
        schemas.push(function_schema(
            "weather",
            "The weather forecast: now, the rest of today and the next three days. Without \
             `place` it's for the user's home (from their profile).",
            serde_json::json!({"type": "object", "properties": {
                "place": {"type": "string", "description": "Only when the user names somewhere other than home"}
            }}),
        ));
        schemas.push(function_schema(
            "web_search",
            "Look something up: facts, dates, people, places, releases, anything you'd \
             otherwise guess or that may have changed since your training. Answer from \
             the results, briefly, and say if they don't settle it.",
            serde_json::json!({"type": "object", "properties": {
                "query": {"type": "string", "description": "What to search for, as a search engine query"},
                "max_results": {"type": "integer"}
            }, "required": ["query"]}),
        ));
        schemas.push(function_schema(
            "read_page",
            "Read a web page (a search result's URL) as text, when the snippet isn't enough.",
            serde_json::json!({"type": "object", "properties": {
                "url": {"type": "string"}
            }, "required": ["url"]}),
        ));
        schemas.push(function_schema(
            "news",
            "The latest headlines from the user's chosen news feeds. Optionally from one \
             `source` (a feed's name), or only those about a `topic`. Read out a few \
             headlines, not all of them.",
            serde_json::json!({"type": "object", "properties": {
                "source": {"type": "string"},
                "topic": {"type": "string"},
                "count": {"type": "integer", "description": "Headlines per feed, default 5"}
            }}),
        ));
        schemas.push(function_schema(
            "update_profile",
            "Save facts about the user that tools use: their name, their home town (for \
             weather), and metric or imperial units. Only when the user states them.",
            serde_json::json!({"type": "object", "properties": {
                "name": {"type": "string"},
                "home": {"type": "string", "description": "A place name, e.g. \"Pittsburgh\""},
                "units": {"type": "string", "enum": ["metric", "imperial"]},
                "add_news": {"type": "string", "description": format!(
                    "A news feed to add: one of {} (or a feed URL)",
                    cosmo_tools::news::SUGGESTED.iter().map(|s| s.0).collect::<Vec<_>>().join(", "))},
                "remove_news": {"type": "string", "description": "A chosen feed's name to remove"}
            }}),
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
            "read_terminal" | "recall" | "system_query" | "weather" | "news" | "web_search"
            | "read_page" => Annotations {
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
            | "minimize_window"
            | "update_profile" => Annotations {
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
                "weather" => {
                    let place = args["place"].as_str().unwrap_or_default().trim().to_owned();
                    match weather(&place).await {
                        Ok(text) | Err(text) => text,
                    }
                }
                "web_search" => {
                    let query = args["query"].as_str().unwrap_or_default().to_owned();
                    let max = args["max_results"].as_u64().unwrap_or(5) as usize;
                    match web_search(&query, max).await {
                        Ok(text) => text,
                        Err(e) => format!("tool error: {e}"),
                    }
                }
                "read_page" => {
                    let url = args["url"].as_str().unwrap_or_default().to_owned();
                    match cosmo_tools::search::read_page(&url).await {
                        Ok(text) => text,
                        Err(e) => format!("tool error: {e}"),
                    }
                }
                "news" => {
                    let feeds = cosmo_config::profile::load().unwrap_or_default().news;
                    let count = args["count"].as_u64().unwrap_or(5) as usize;
                    match cosmo_tools::news::latest(
                        &feeds,
                        args["source"].as_str(),
                        args["topic"].as_str().filter(|t| !t.trim().is_empty()),
                        count,
                    )
                    .await
                    {
                        Ok(text) => text,
                        Err(e) => format!("tool error: {e}"),
                    }
                }
                "update_profile" => match update_profile(&args).await {
                    Ok(text) => text,
                    Err(e) => format!("tool error: {e}"),
                },
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

/// The forecast for `place`, or for home when it's empty. `Err` is a
/// message for the model (what's missing and how to fix it).
async fn weather(place: &str) -> Result<String, String> {
    let profile = cosmo_config::profile::load().unwrap_or_default();
    let target = if place.is_empty() {
        profile.home.clone().ok_or(
            "tool error: no home location is set. Ask the user where they live, then save it \
             with update_profile (or they can run `cosmo setup`).",
        )?
    } else {
        cosmo_tools::geo::geocode(place)
            .await
            .map_err(|e| format!("tool error: {e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| format!("tool error: no place called {place:?} was found"))?
    };
    cosmo_tools::weather::forecast(&target, profile.units)
        .await
        .map_err(|e| format!("tool error: {e}"))
}

/// Search with the configured backend, its key read from the Secret
/// Service each time (so `cosmo search use` applies at once).
async fn web_search(query: &str, max: usize) -> Result<String, String> {
    let backend = cosmo_config::load()
        .map(|c| c.search_provider)
        .unwrap_or_else(|_| "wikipedia".into());
    let key = match cosmo_tools::search::BACKENDS
        .iter()
        .find(|b| b.0 == backend)
        .and_then(|b| b.2)
    {
        Some(_) => tokio::time::timeout(
            std::time::Duration::from_secs(3),
            cosmo_reason::secret::resolve_keyring(&cosmo_tools::search::key_name(&backend)),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .map(|k| k.expose().to_owned()),
        None => None,
    };
    cosmo_tools::search::web_search(
        &backend,
        key.as_deref(),
        query,
        max,
        &cosmo_tools::search::today(),
    )
    .await
}

/// Save what the user said about themselves to their profile.
async fn update_profile(args: &Value) -> Result<String, String> {
    use cosmo_config::profile::Units;
    let mut profile = cosmo_config::profile::load()?;
    let mut said = Vec::new();
    if let Some(name) = args["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        profile.name = Some(name.to_owned());
        said.push(format!("name: {name}"));
    }
    if let Some(home) = args["home"]
        .as_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        let found = cosmo_tools::geo::geocode(home).await?;
        let place = found
            .first()
            .cloned()
            .ok_or_else(|| format!("no place called {home:?} was found"))?;
        said.push(format!("home: {}", place.name));
        if found.len() > 1 {
            let others: Vec<&str> = found[1..].iter().map(|p| p.name.as_str()).collect();
            said.push(format!(
                "(other matches, if that's wrong: {})",
                others.join("; ")
            ));
        }
        profile.home = Some(place);
    }
    if let Some(add) = args["add_news"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let feed = match cosmo_tools::news::suggested(add) {
            Some(f) => f,
            None if add.starts_with("https://") || add.starts_with("http://") => {
                cosmo_config::profile::Feed {
                    name: add.to_owned(),
                    url: add.to_owned(),
                }
            }
            None => {
                return Err(format!(
                    "{add:?} isn't a known feed; known: {} (or give a feed URL)",
                    cosmo_tools::news::SUGGESTED
                        .iter()
                        .map(|s| s.1)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };
        if !profile.news.iter().any(|f| f.url == feed.url) {
            said.push(format!("news: added {}", feed.name));
            profile.news.push(feed);
        }
    }
    if let Some(drop) = args["remove_news"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let before = profile.news.len();
        let d = drop.to_lowercase();
        profile.news.retain(|f| !f.name.to_lowercase().contains(&d));
        if profile.news.len() < before {
            said.push(format!("news: removed {drop}"));
        }
    }
    match args["units"].as_str() {
        Some("metric") => profile.units = Units::Metric,
        Some("imperial") => profile.units = Units::Imperial,
        _ => {}
    }
    if let Some(u) = args["units"].as_str() {
        said.push(format!("units: {u}"));
    }
    if said.is_empty() {
        return Err("nothing to save".into());
    }
    cosmo_config::profile::save(&profile)?;
    Ok(format!("saved to the profile: {}", said.join(", ")))
}

fn run_tool(result: Result<String, cosmo_tools::ToolError>) -> String {
    match result {
        Ok(text) => text,
        Err(e) => format!("tool error: {e}"),
    }
}
