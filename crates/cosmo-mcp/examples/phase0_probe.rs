//! Phase 0 probe: drive `computer-use-linux` over MCP exactly the way cosmo
//! will (`rmcp` + `TokioChildProcess`), timing the operations the reflex and
//! reasoning paths depend on:
//!
//! - connect + initialize handshake
//! - `tools/list` (with annotations — the gate's `destructiveHint` mapping)
//! - `list_windows` cold and warm (the cached-probe question, blueprint §5)
//! - `focused_window`
//! - `activate_window` targeting the already-focused window (least
//!   disruptive way to exercise activation)
//! - `move_window` (workspace move) — only with `--move <window_id> --workspace <n>`;
//!   disruptive, so opt-in, and it moves back when `--move-back` is given.
//!
//! Also verifies `run_shell` is absent (invariant #2).
//!
//! `--schema <tool>` prints a tool's input schema and exits.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rmcp::{
    Peer, RoleClient, ServiceExt,
    model::{CallToolRequestParams, ContentBlock},
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use tokio::process::Command;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

async fn tool_text(
    peer: &Peer<RoleClient>,
    name: &'static str,
    args: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<(String, Duration, bool)> {
    let t = Instant::now();
    let mut params = CallToolRequestParams::new(name);
    if let Some(a) = args {
        params = params.with_arguments(a);
    }
    let res = tokio::time::timeout(CALL_TIMEOUT, peer.call_tool(params))
        .await
        .map_err(|_| anyhow::anyhow!("timeout after {CALL_TIMEOUT:?}"))?
        .with_context(|| format!("call_tool({name}) failed"))?;
    let is_error = res.is_error.unwrap_or(false);
    let text = res
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, t.elapsed(), is_error))
}

fn first_window_id(windows_json: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(windows_json).ok()?;
    // Result shape: {"backend": "...", "windows": [ ... ]}. Fallback: bare array.
    let arr = v
        .get("windows")
        .and_then(|w| w.as_array())
        .or_else(|| v.as_array())?;
    // Prefer an explicitly focused window; the COSMIC backend does not report
    // focus, so in practice fall back to the window the user is working in
    // (this probe runs from an editor terminal).
    let pick = arr
        .iter()
        .find(|w| w.get("focused").and_then(|f| f.as_bool()).unwrap_or(false))
        .or_else(|| {
            arr.iter()
                .find(|w| w.get("app_id").and_then(|a| a.as_str()) == Some("codium"))
        })
        .or_else(|| arr.first())?;
    pick.get("window_id").cloned()
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse flags up front: --schema <tool> short-circuits everything.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| {
        argv.iter()
            .position(|a| a == name)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };
    let move_target = get("--move");
    let move_workspace = get("--workspace");
    let move_back = argv.iter().any(|a| a == "--move-back");

    let total = Instant::now();

    let t = Instant::now();
    let client = ()
        .serve(TokioChildProcess::new(
            Command::new("computer-use-linux").configure(|c| {
                c.arg("mcp");
            }),
        )?)
        .await
        .context("spawn computer-use-linux mcp + initialize")?;
    println!("connect+init: {:?}", t.elapsed());
    let peer = client.peer().clone();

    // tools/list, with annotations for the policy-gate mapping.
    let t = Instant::now();
    let tools = peer.list_tools(None).await?;
    println!(
        "tools/list: {:?} ({} tools)",
        t.elapsed(),
        tools.tools.len()
    );
    let mut run_shell_present = false;
    for tool in &tools.tools {
        let flags = match &tool.annotations {
            Some(a) => format!(
                "ro={} destr={}",
                a.read_only_hint.unwrap_or(false),
                a.destructive_hint.unwrap_or(false)
            ),
            None => "no-annotations".to_string(),
        };
        if tool.name == "run_shell" {
            run_shell_present = true;
        }
        println!("  - {:<18} {flags}", tool.name);
    }
    if run_shell_present {
        bail!("run_shell registered — invariant violation (check COMPUTER_USE_LINUX_ENABLE_SHELL)");
    }
    println!("run_shell absent: OK");

    // --schema <tool>: dump description + input schema, then exit.
    if let Some(name) = get("--schema") {
        let tool = tools
            .tools
            .iter()
            .find(|t| t.name == name)
            .with_context(|| format!("tool {name} not found"))?;
        println!("== {name} ==");
        println!(
            "description: {}",
            tool.description.as_deref().unwrap_or("(none)")
        );
        println!(
            "schema: {}",
            serde_json::to_string_pretty(&tool.input_schema)?
        );
        return Ok(());
    }

    // list_windows, cold then warm — cold includes the backend probe
    // (GNOME ServiceUnknown x2, then the COSMIC helper).
    let (windows_text, cold, err) = tool_text(&peer, "list_windows", None).await?;
    println!("list_windows (cold): {:?}", cold);
    if err {
        println!("  ERROR: {}", &windows_text[..windows_text.len().min(400)]);
    }
    // Focus reporting summary: does the COSMIC backend flag the focused window?
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&windows_text) {
        let wins = v
            .get("windows")
            .and_then(|w| w.as_array())
            .cloned()
            .or_else(|| v.as_array().cloned())
            .unwrap_or_default();
        println!("  {} windows; focused flags:", wins.len());
        for w in &wins {
            println!(
                "    {:?} focused={} hidden={} ws={:?}",
                w.get("app_id").and_then(|a| a.as_str()).unwrap_or("?"),
                w.get("focused").and_then(|f| f.as_bool()).unwrap_or(false),
                w.get("hidden").and_then(|h| h.as_bool()).unwrap_or(false),
                w.get("workspace"),
            );
        }
    } else {
        println!("  raw: {}", &windows_text[..windows_text.len().min(900)]);
    }
    let (_, warm, _) = tool_text(&peer, "list_windows", None).await?;
    println!("list_windows (warm): {:?}", warm);

    // focused_window.
    let (focused_text, tf, err) = tool_text(&peer, "focused_window", None).await?;
    println!("focused_window: {:?}", tf);
    if !err && !focused_text.trim().is_empty() {
        println!("  {}", &focused_text[..focused_text.len().min(200)]);
    }

    // activate_window on the focused window (or first): least disruptive.
    let id = first_window_id(&windows_text)
        .or_else(|| first_window_id(&focused_text))
        .context("no window_id found in list output")?;
    let (ta_text, ta, err) = tool_text(
        &peer,
        "activate_window",
        serde_json::json!({ "window_id": id }).as_object().cloned(),
    )
    .await?;
    println!("activate_window({id}): {:?} is_error={err}", ta);
    if err {
        println!("  ERROR: {}", &ta_text[..ta_text.len().min(300)]);
    }

    // Opt-in workspace move timing: caller supplies a window_id (from the
    // list_windows output above) and a workspace number.
    if let Some(win) = move_target {
        let ws = move_workspace.context("--move requires --workspace <n>")?;
        let mut args = serde_json::Map::new();
        args.insert("window_id".into(), serde_json::from_str(&win)?);
        args.insert("workspace".into(), serde_json::Value::String(ws.clone()));
        let (txt, tm, err) = tool_text(&peer, "move_window", Some(args.clone())).await?;
        println!(
            "move_window(win {win} -> workspace {ws}): {:?} is_error={err}",
            tm
        );
        if err || txt.trim().is_empty() {
            println!("  raw: {}", &txt[..txt.len().min(300)]);
        }
        // Move it back so the desktop ends up where it started.
        if move_back && let Some(orig) = get("--from-workspace") {
            let mut back = serde_json::Map::new();
            back.insert("window_id".into(), serde_json::from_str(&win)?);
            back.insert("workspace".into(), serde_json::Value::String(orig));
            let (_, tb, err) = tool_text(&peer, "move_window", Some(back)).await?;
            println!("move_window back: {:?} is_error={err}", tb);
        }
    }

    println!("total: {:?}", total.elapsed());
    let _ = client.cancel().await;
    Ok(())
}
