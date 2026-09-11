//! Integration test against a fake stdio MCP server (plan §1.3): the gate
//! mapping must be testable without the real agent.

use std::sync::Arc;

use cosmo_config::Config;
use cosmo_gate::{Annotations, LockState, Verdict};
use cosmo_mcp::McpHost;

fn test_config() -> Config {
    Config {
        agent_command: "python3".into(),
        agent_args: vec![
            "-u".into(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_agent.py").into(),
        ],
        allowed_tools: vec![
            "list_windows".into(),
            "click".into(),
            "screenshot".into(),
            "press_key".into(),
            // run_shell deliberately absent — invariant #2.
        ],
        ..Config::default()
    }
}

#[tokio::test]
async fn fake_agent_discovery_and_gate_mapping() {
    let cfg = Arc::new(test_config());
    let host = McpHost::connect(cfg)
        .await
        .expect("fake agent must spawn and initialize");

    let tools = host.registered_tools();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();

    // run_shell refused even though the agent advertises it (invariant #2).
    assert!(
        !names.contains(&"run_shell"),
        "run_shell must never register"
    );
    // Not on the allowlist ⇒ not registered.
    assert!(!names.contains(&"not_in_allowlist"));

    // Allowlisted tools registered, with annotations mapped to the gate.
    let by_name = |n: &str| tools.iter().find(|t| t.name == n).expect(n);
    let list_windows = by_name("list_windows");
    assert!(list_windows.read_only && !list_windows.destructive);

    let click = by_name("click");
    assert!(!click.read_only && click.destructive);

    let screenshot = by_name("screenshot");
    assert!(!screenshot.read_only && !screenshot.destructive);

    // A tool advertised with no `annotations` at all defaults to destructive
    // (MCP's own default) so the gate holds it rather than allowing it.
    let press_key = by_name("press_key");
    assert!(
        !press_key.read_only && press_key.destructive,
        "an unannotated agent tool must default to destructive"
    );

    // The agent's `inputSchema` is passed through verbatim: dropping it tells
    // the model that `click` exists but nothing about x/y, so it invents
    // arguments.
    assert_eq!(click.input_schema["properties"]["x"]["type"], "integer");
    assert_eq!(click.input_schema["properties"]["y"]["type"], "integer");
    assert_eq!(click.input_schema["required"][0], "x");

    // Gate mapping from the discovered annotations, unlocked session.
    let gate = cosmo_gate::Gate::new();
    gate.set_lock_state(LockState::Unlocked);
    let ro = Annotations {
        read_only: list_windows.read_only,
        destructive: list_windows.destructive,
    };
    assert_eq!(
        gate.verdict_for_call("list_windows", &serde_json::json!({}), &ro),
        Verdict::Allow
    );
    let cd = Annotations {
        read_only: click.read_only,
        destructive: click.destructive,
    };
    assert_eq!(
        gate.verdict_for_call("click", &serde_json::json!({}), &cd),
        Verdict::Hold
    );

    // Calling a registered tool reaches the fake server and comes back.
    let result = host
        .call_agent_tool(
            "list_windows",
            serde_json::json!({}).as_object().unwrap().clone(),
        )
        .await
        .expect("registered tool must call");
    assert!(result.contains("fake-executed list_windows"));

    // Calling an unregistered tool is refused at the host, before any gate.
    let err = host
        .call_agent_tool(
            "not_in_allowlist",
            serde_json::json!({}).as_object().unwrap().clone(),
        )
        .await
        .expect_err("unregistered tool must be refused");
    assert!(err.to_string().contains("not in the configured allowlist"));

    host.shutdown();
}
