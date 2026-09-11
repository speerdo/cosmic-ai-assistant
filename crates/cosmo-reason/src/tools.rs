//! The tool surface the model sees: schemas for the model, annotations for
//! the gate, and execution for the daemon.
//!
//! The daemon wires one [`ToolHost`] that combines the MCP host
//! (`cosmo-mcp`, agent tools) and the native tools (`cosmo-tools`); the
//! reasoning loop sees one flat namespace. This module is deliberately
//! runtime-agnostic so tests can drive the loop with a scripted host.

use serde_json::Value;

use cosmo_gate::Annotations;

/// What the reasoning loop needs from a tool backend.
pub trait ToolHost: Send + Sync {
    /// OpenAI tool schemas for every registered tool (agent + native).
    /// Filtered per config; `run_shell` never appears (invariant #2).
    fn tool_schemas(&self) -> Vec<Value>;

    /// Gate annotations for a tool (unknown tool ⇒ conservative: hold).
    fn annotations_of(&self, tool: &str) -> Annotations;

    /// Execute a gate-approved call. Errors are returned as text to the
    /// model (the loop continues; the model can react).
    fn execute(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + '_>>;
}

/// Standard OpenAI function schema for one tool.
pub fn function_schema(name: &str, description: &str, parameters: Value) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_shape_matches_openai() {
        let s = function_schema(
            "run_in_terminal",
            "Run a command in the cosmo tmux session",
            serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
        );
        assert_eq!(s["type"], "function");
        assert_eq!(s["function"]["name"], "run_in_terminal");
        assert_eq!(s["function"]["parameters"]["type"], "object");
    }
}
