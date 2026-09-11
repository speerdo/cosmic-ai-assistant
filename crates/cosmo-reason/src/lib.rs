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

pub mod secret;
pub mod tools;

use std::sync::Arc;

use serde_json::{Value, json};

use cosmo_gate::{Gate, Verdict};
use secret::{KeySource, SecretKey};

/// One round of the tool loop's result.
#[derive(Debug)]
pub enum ToolOutcome {
    /// The model produced a final text reply; the turn is done.
    Reply(String),
    /// A tool call was dispatched and executed; the loop continues.
    ToolRan { call_id: String, result: String },
    /// A tool call was parked by the gate awaiting local confirmation.
    Held { token: String, tool: String },
}

/// Errors from the reasoning client.
#[derive(Debug, thiserror::Error)]
pub enum ReasonError {
    #[error("api request failed: {0}")]
    Http(String),
    #[error("bad api response: {0}")]
    BadResponse(String),
    #[error("model requested unknown tool `{0}`")]
    UnknownTool(String),
    #[error("api key unavailable: {0}")]
    NoKey(String),
}

/// The chat-completions reasoning client (v1).
pub struct Reasoner {
    http: reqwest::Client,
    cfg: Arc<cosmo_config::Config>,
    key: SecretKey,
    /// Static prompt (system role) — no live desktop state (invariant #7).
    static_prompt: String,
}

impl Reasoner {
    /// Build a client with the key resolved lazily by the caller's
    /// [`KeySource`]. `resolve` may be called on first use only (keyring
    /// locked at boot ⇒ retry later; plan §1.4).
    pub fn new(
        cfg: Arc<cosmo_config::Config>,
        key_source: &dyn KeySource,
    ) -> Result<Self, ReasonError> {
        let key = key_source.resolve()?;
        Ok(Self {
            http: reqwest::Client::new(),
            cfg,
            key,
            static_prompt: static_prompt(),
        })
    }

    /// Run one turn of the tool loop: send messages, dispatch at most one
    /// tool call per model response, until a final reply or hold.
    pub async fn turn(
        &mut self,
        user_text: &str,
        gate: &Gate,
        host: &dyn tools::ToolHost,
        history: &mut Vec<Value>,
    ) -> Result<ToolOutcome, ReasonError> {
        let turn_span = tracing::info_span!("reason", hop = "model_round_trip");
        let _enter = turn_span.enter();

        history.push(json!({"role": "user", "content": user_text}));

        let tools = host.tool_schemas();
        loop {
            let body = {
                let mut messages = vec![json!({"role": "system", "content": self.static_prompt})];
                messages.extend(history.iter().cloned());
                json!({
                    "model": self.cfg.model,
                    "messages": messages,
                    "tools": tools,
                })
            };
            let response = self.chat(body).await?;
            let choice = &response["choices"][0];
            let message = choice["message"].clone();
            let finish = choice["finish_reason"].as_str().unwrap_or("");
            history.push(message.clone());

            match finish {
                "tool_calls" => {
                    let calls = message["tool_calls"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();

                    // A hold ends the turn, but not before every tool_call id
                    // in this response has a result. See `tool_result`.
                    let mut held: Option<ToolOutcome> = None;

                    for call in calls {
                        let call_id = call["id"].as_str().unwrap_or("").to_string();
                        let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                        let args_raw = call["function"]["arguments"].as_str().unwrap_or("{}");

                        // Once something is parked, nothing later in the same
                        // response runs — but it still needs a result.
                        if held.is_some() {
                            history.push(tool_result(
                                &call_id,
                                "NOT RUN: an earlier call in this response is awaiting local \
                                 confirmation.",
                            ));
                            continue;
                        }

                        let args: Value = match serde_json::from_str(args_raw) {
                            Ok(v) => v,
                            Err(e) => {
                                // Feed the parse failure back as this call's
                                // result rather than aborting the turn: an
                                // early return here leaves the assistant's
                                // tool_calls message in `history` with no
                                // matching result, and the *next* request is
                                // then malformed.
                                history.push(tool_result(
                                    &call_id,
                                    &format!("ERROR: could not parse tool arguments: {e}"),
                                ));
                                continue;
                            }
                        };

                        // Gate on EVERY tool call (plan §1.4: gate interposed
                        // on every tool call, MCP and native alike).
                        let annotations = host.annotations_of(&name);
                        let verdict = {
                            let gate_span = tracing::info_span!("gate", tool = %name);
                            let _g = gate_span.enter();
                            let v = gate.verdict_for_call(&name, &args, &annotations);
                            // Invariant #1: confirmation language in the same
                            // response escalates to Deny.
                            let text = message["content"].as_str().unwrap_or("");
                            gate.same_response_verdict(text, v)
                        };
                        match verdict {
                            Verdict::Deny => {
                                history.push(tool_result(
                                    &call_id,
                                    "DENIED: this action is not permitted under any confirmation.",
                                ));
                            }
                            Verdict::Hold => {
                                let tool_span =
                                    tracing::info_span!("tool", tool = %name, held = true);
                                let _t = tool_span.enter();
                                let token = gate.park(&name, args.clone(), name.to_string());
                                history.push(tool_result(
                                    &call_id,
                                    &format!(
                                        "HELD: parked for local confirmation (token {token}). \
                                         It has not run."
                                    ),
                                ));
                                held = Some(ToolOutcome::Held {
                                    token,
                                    tool: name.clone(),
                                });
                            }
                            Verdict::Allow => {
                                let result = {
                                    let tool_span = tracing::info_span!("tool", tool = %name);
                                    let _t = tool_span.enter();
                                    host.execute(&name, args).await
                                };
                                history.push(tool_result(&call_id, &result));
                                // Loop back to the model with the result.
                            }
                        }
                    }

                    if let Some(outcome) = held {
                        return Ok(outcome);
                    }
                }
                "stop" => {
                    let reply = message["content"].as_str().unwrap_or("").to_string();
                    return Ok(ToolOutcome::Reply(reply));
                }
                other => {
                    return Err(ReasonError::BadResponse(format!(
                        "finish_reason: {other:?}"
                    )));
                }
            }
        }
    }

    async fn chat(&self, body: Value) -> Result<Value, ReasonError> {
        // Base URL overridable for tests; production default is OpenAI.
        let base = std::env::var("COSMO_API_BASE")
            .unwrap_or_else(|_| "https://api.openai.com".to_string());
        let url = format!("{}/v1/chat/completions", base.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .bearer_auth(self.key.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let src = std::error::Error::source(&e)
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                ReasonError::Http(format!("{e} (source: {src})"))
            })?;

        // Rate-limit headers, every turn (invariant #7). The Authorization
        // header never reaches a log — we log only these response headers.
        let remaining_requests = parse_header(response.headers(), "x-ratelimit-remaining-requests");
        let remaining_tokens = parse_header(response.headers(), "x-ratelimit-remaining-tokens");

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ReasonError::Http(e.to_string()))?;
        tracing::info!(
            status = %status,
            remaining_requests = ?remaining_requests,
            remaining_tokens = ?remaining_tokens,
            "model usage"
        );
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| ReasonError::BadResponse(format!("{e}: {text}")))?;
        if !status.is_success() {
            return Err(ReasonError::Http(format!("{status}: {text}")));
        }
        if let Some(usage) = value.get("usage") {
            tracing::info!(usage = %usage, "tokens");
        }
        Ok(value)
    }
}

/// A `role: "tool"` history entry.
///
/// **Every** `tool_call` id in an assistant message needs exactly one of
/// these before the next request, whatever the gate decided and whether or
/// not the call ran. The chat-completions API rejects a conversation in which
/// an assistant message announces a tool call that no tool message answers,
/// so skipping one — by returning early on a hold, say — poisons the history
/// for every subsequent turn, not just this one.
fn tool_result(call_id: &str, content: &str) -> Value {
    json!({"role": "tool", "tool_call_id": call_id, "content": content})
}

fn parse_header(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// The static system prompt. Under 3,000 tokens; **no desktop state**
/// (invariant #7) — windows/workspaces arrive via tools.
fn static_prompt() -> String {
    "You are cosmo, a voice assistant driving a Linux desktop. \
You act through tools: list windows, focus/type/click, and run commands \
in a cosmo-owned tmux session (run_in_terminal). Prefer read-only \
inspection first; type into terminals only when the user asks for an \
action. Destructive or irreversible actions will be held for an explicit \
local confirmation by cosmo's gate — that is expected behaviour, do not \
attempt to talk the user out of it or re-request the action. Never claim \
you ran something unless a tool result says so. Keep replies short: one \
or two sentences. If a command is long-running, run it and report the \
transcript so far."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_prompt_is_short_and_stateless() {
        let p = static_prompt();
        // Rough bound: ~700 bytes is ~175 tokens; hard cap 12k bytes for
        // safety against drift.
        assert!(p.len() < 12_000, "static prompt grew beyond budget");
        assert!(!p.contains("focused"), "no live desktop state in prompt");
    }
}
