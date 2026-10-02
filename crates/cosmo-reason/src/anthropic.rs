//! Anthropic's Messages API, as a translation layer.
//!
//! The tool loop and the gate (`lib.rs`) work on one history shape, OpenAI's
//! chat messages. This module turns that history into a Messages request on
//! the way out, and the streamed reply back into the same
//! [`Completed`](crate::stream::Completed) a chat-completions reply makes, so
//! nothing after it knows which wire it came over.
//!
//! The one thing that doesn't survive translation is the reply's own blocks
//! (thinking blocks carry a signature that must come back unchanged in a tool
//! loop). They ride along on the assistant message under [`RAW`] and are
//! sent back verbatim. The history only ever goes back to the provider that
//! wrote it, since the reasoner is built for one provider.

use serde_json::{Map, Value, json};

use crate::stream::Completed;

/// The key the assistant message keeps its original blocks under.
pub const RAW: &str = "anthropic_content";

/// The Messages API version header's value.
pub const VERSION: &str = "2023-06-01";

/// Room for the reply. Replies are a sentence or two (the system prompt
/// says so), but a tool call with a long argument mustn't be cut off.
const MAX_TOKENS: u32 = 4096;

/// A Messages request body for `history` (OpenAI-shaped, without the
/// system message) and `tools` (OpenAI function tools). `cache` marks the
/// system prompt as a cache breakpoint (tools and system are the stable
/// prefix); only Anthropic's own API is sent that field.
pub fn request(
    model: &str,
    system: &str,
    history: &[Value],
    tools: &[Value],
    cache: bool,
) -> Value {
    let mut system_block = json!({"type": "text", "text": system});
    if cache {
        system_block["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": [system_block],
        "messages": messages(history),
        "stream": true,
    });
    let tools: Vec<Value> = tools.iter().filter_map(tool).collect();
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    body
}

/// An OpenAI function tool as a Messages tool.
fn tool(t: &Value) -> Option<Value> {
    let f = &t["function"];
    let name = f["name"].as_str()?;
    let mut out = json!({
        "name": name,
        "input_schema": f.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    });
    if let Some(d) = f["description"].as_str() {
        out["description"] = json!(d);
    }
    Some(out)
}

/// OpenAI-shaped history as Messages turns: tool results become
/// `tool_result` blocks in a user turn, and neighbouring turns of the same
/// role are joined (all of one response's results go back in one message).
fn messages(history: &[Value]) -> Vec<Value> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for m in history {
        let (role, blocks) = match m["role"].as_str() {
            Some("user") => ("user", text_blocks(&m["content"])),
            Some("tool") => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m["tool_call_id"],
                    "content": m["content"].as_str().unwrap_or(""),
                })],
            ),
            Some("assistant") => ("assistant", assistant_blocks(m)),
            // A system message is never in the history (it's sent apart).
            _ => continue,
        };
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((r, b)) if r == role => b.extend(blocks),
            _ => out.push((role.to_owned(), blocks)),
        }
    }
    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

fn text_blocks(content: &Value) -> Vec<Value> {
    match content.as_str() {
        Some(t) if !t.is_empty() => vec![json!({"type": "text", "text": t})],
        _ => Vec::new(),
    }
}

fn assistant_blocks(m: &Value) -> Vec<Value> {
    if let Some(raw) = m[RAW].as_array() {
        return raw.clone();
    }
    // Written by another wire (not expected; translate what's there).
    let mut blocks = text_blocks(&m["content"]);
    for call in m["tool_calls"].as_array().into_iter().flatten() {
        let args = call["function"]["arguments"].as_str().unwrap_or("{}");
        blocks.push(json!({
            "type": "tool_use",
            "id": call["id"],
            "name": call["function"]["name"],
            "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
        }));
    }
    blocks
}

/// A whole (non-streamed) Messages response, as a chat-completions one.
pub fn from_message(v: &Value) -> Result<Completed, String> {
    if v["type"] == "error" {
        return Err(v["error"].to_string());
    }
    let blocks = v["content"].as_array().cloned().unwrap_or_default();
    let mut usage = Usage::default();
    usage.start(&v["usage"]);
    usage.delta(&v["usage"]);
    Ok(completed(
        blocks,
        v["stop_reason"].as_str(),
        Some(usage.value()),
    ))
}

/// The stop reason in chat-completions words, which the tool loop reads.
fn finish(stop: Option<&str>) -> String {
    match stop {
        Some("end_turn" | "stop_sequence") => "stop",
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some(other) => other,
        None => "",
    }
    .to_owned()
}

fn completed(blocks: Vec<Value>, stop: Option<&str>, usage: Option<Value>) -> Completed {
    let text: String = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let calls: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            json!({
                "id": b["id"],
                "type": "function",
                "function": {"name": b["name"], "arguments": b["input"].to_string()},
            })
        })
        .collect();
    let mut message = json!({"role": "assistant", "content": text, RAW: blocks});
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    Completed {
        message,
        finish_reason: finish(stop),
        usage,
    }
}

/// Token counts, in chat-completions names: the prompt includes what was
/// read from and written to the cache.
#[derive(Debug, Default, Clone, Copy)]
struct Usage {
    input: u64,
    output: u64,
}

impl Usage {
    fn start(&mut self, u: &Value) {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        self.input =
            n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
    }

    /// `message_delta` carries the running output count.
    fn delta(&mut self, u: &Value) {
        if let Some(n) = u["output_tokens"].as_u64() {
            self.output = n;
        }
    }

    fn value(self) -> Value {
        json!({
            "prompt_tokens": self.input,
            "completion_tokens": self.output,
            "total_tokens": self.input + self.output,
        })
    }
}

/// Builds the reply from streamed Messages events (the `data:` payloads
/// [`SseDecoder`](crate::stream::SseDecoder) yields).
#[derive(Debug, Default)]
pub struct Accumulator {
    blocks: Vec<Value>,
    /// Tool input JSON, by block index, until its block stops.
    partial: Vec<String>,
    stop: Option<String>,
    usage: Usage,
    /// Tool inputs that weren't valid JSON, by call id: the loop gets the
    /// raw text (and reports the error to the model as that call's result),
    /// while the block sent back keeps an empty object.
    malformed: Vec<(String, String)>,
    started: bool,
    done: bool,
}

impl Accumulator {
    /// Feed one event; returns any reply text it added.
    pub fn feed(&mut self, payload: &str) -> Result<Option<String>, String> {
        let e: Value = serde_json::from_str(payload).map_err(|err| format!("{err}: {payload}"))?;
        let index = e["index"].as_u64().unwrap_or(0) as usize;
        match e["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.started = true;
                self.usage.start(&e["message"]["usage"]);
            }
            "content_block_start" => {
                while self.blocks.len() <= index {
                    self.blocks.push(Value::Null);
                    self.partial.push(String::new());
                }
                let mut block = e["content_block"].clone();
                if block["type"] == "tool_use" {
                    // The input arrives as JSON fragments.
                    block["input"] = json!({});
                }
                self.blocks[index] = block;
            }
            "content_block_delta" => {
                let block = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| format!("delta for block {index} before its start"))?;
                let d = &e["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append(block, "text", t);
                        return Ok((!t.is_empty()).then(|| t.to_owned()));
                    }
                    "input_json_delta" => {
                        self.partial[index].push_str(d["partial_json"].as_str().unwrap_or(""));
                    }
                    "thinking_delta" => {
                        append(block, "thinking", d["thinking"].as_str().unwrap_or(""))
                    }
                    "signature_delta" => {
                        append(block, "signature", d["signature"].as_str().unwrap_or(""))
                    }
                    // Citations and anything newer: not used by cosmo.
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(block) = self.blocks.get_mut(index)
                    && block["type"] == "tool_use"
                {
                    let raw = std::mem::take(&mut self.partial[index]);
                    // A malformed input is the loop's problem to report
                    // back (as a tool result), not a broken stream.
                    block["input"] = if raw.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&raw).unwrap_or_else(|_| {
                            let id = block["id"].as_str().unwrap_or("").to_owned();
                            self.malformed.push((id, raw));
                            json!({})
                        })
                    };
                }
            }
            "message_delta" => {
                if let Some(s) = e["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_owned());
                }
                self.usage.delta(&e["usage"]);
            }
            "message_stop" => self.done = true,
            "error" => return Err(format!("stream error: {}", e["error"])),
            // `ping` and anything newer.
            _ => {}
        }
        Ok(None)
    }

    /// The stream ended. A stream cut off before `message_stop` is an error,
    /// as on the chat-completions side: a half reply isn't acted on.
    pub fn finish(self) -> Result<Completed, String> {
        if !self.started || !self.done {
            return Err("the reply stream ended early".into());
        }
        let blocks = self.blocks.into_iter().filter(|b| !b.is_null()).collect();
        let mut done = completed(blocks, self.stop.as_deref(), Some(self.usage.value()));
        // `get_mut`, not indexing: indexing would add a null `tool_calls`.
        let calls = done
            .message
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut);
        for call in calls.into_iter().flatten() {
            if let Some((_, raw)) = self
                .malformed
                .iter()
                .find(|(id, _)| call["id"] == id.as_str())
            {
                call["function"]["arguments"] = json!(raw);
            }
        }
        Ok(done)
    }
}

fn append(block: &mut Value, key: &str, s: &str) {
    let obj: &mut Map<String, Value> = match block.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    let entry = obj.entry(key).or_insert_with(|| json!(""));
    let joined = format!("{}{s}", entry.as_str().unwrap_or(""));
    *entry = Value::String(joined);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weather_tool() -> Value {
        json!({"type": "function", "function": {
            "name": "pause_media", "description": "Pause playback",
            "parameters": {"type": "object", "properties": {}}}})
    }

    #[test]
    fn a_request_carries_system_tools_and_the_history() {
        let history = vec![json!({"role": "user", "content": "pause the music"})];
        let body = request("claude-haiku-4-5", "SYS", &history, &[weather_tool()], true);
        assert_eq!(body["system"][0]["text"], "SYS");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["name"], "pause_media");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(
            body["messages"][0],
            json!({"role": "user", "content": [
            {"type": "text", "text": "pause the music"}]})
        );
        assert!(body["max_tokens"].as_u64().unwrap() > 0);
        // Not to a proxy: it may not know the field.
        let body = request("m", "SYS", &history, &[], false);
        assert!(body["system"][0].get("cache_control").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn tool_results_and_the_next_user_turn_share_one_message() {
        let raw = json!([{"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "t1", "name": "a", "input": {}},
            {"type": "tool_use", "id": "t2", "name": "b", "input": {}}]);
        let history = vec![
            json!({"role": "user", "content": "do two things"}),
            json!({"role": "assistant", "content": "", "tool_calls": [], RAW: raw}),
            json!({"role": "tool", "tool_call_id": "t1", "content": "ok"}),
            json!({"role": "tool", "tool_call_id": "t2", "content": "HELD"}),
            json!({"role": "user", "content": "and then?"}),
        ];
        let m = messages(&history);
        assert_eq!(m.len(), 3, "{m:#?}");
        assert_eq!(
            m[1]["content"], raw,
            "the blocks go back verbatim, signature and all"
        );
        let results = m[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(results[1]["content"], "HELD");
        assert_eq!(results[2]["text"], "and then?");
    }

    #[test]
    fn a_history_written_in_chat_shape_still_translates() {
        let m = assistant_blocks(&json!({"role": "assistant", "content": null,
            "tool_calls": [{"id": "c", "function": {"name": "f", "arguments": "{\"x\":1}"}}]}));
        assert_eq!(
            m,
            vec![json!({"type": "tool_use", "id": "c", "name": "f", "input": {"x": 1}})]
        );
    }

    fn feed_all(events: &[Value]) -> (Accumulator, String) {
        let mut acc = Accumulator::default();
        let mut text = String::new();
        for e in events {
            if let Some(t) = acc.feed(&e.to_string()).unwrap() {
                text.push_str(&t);
            }
        }
        (acc, text)
    }

    #[test]
    fn a_streamed_tool_call_comes_out_in_chat_shape() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 100, "cache_read_input_tokens": 900}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "On "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "it."}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "launch_app", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"app\": \"fire"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "fox\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "ping"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
            json!({"type": "message_stop"}),
        ];
        let (acc, text) = feed_all(&events);
        assert_eq!(text, "On it.");
        let done = acc.finish().unwrap();
        assert_eq!(done.finish_reason, "tool_calls");
        assert_eq!(done.message["content"], "On it.");
        let call = &done.message["tool_calls"][0];
        assert_eq!(call["id"], "toolu_1");
        assert_eq!(call["function"]["name"], "launch_app");
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"app": "firefox"}));
        assert_eq!(done.message[RAW][1]["input"], json!({"app": "firefox"}));
        let u = done.usage.unwrap();
        assert_eq!(
            (u["prompt_tokens"].as_u64(), u["completion_tokens"].as_u64()),
            (Some(1000), Some(30))
        );
    }

    #[test]
    fn thinking_survives_with_its_signature_and_a_reply_ends_the_turn() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 5}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "abc"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Done."}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
            json!({"type": "message_stop"}),
        ];
        let done = feed_all(&events).0.finish().unwrap();
        assert_eq!(done.finish_reason, "stop");
        assert_eq!(done.message["content"], "Done.");
        assert_eq!(
            done.message[RAW][0],
            json!({"type": "thinking", "thinking": "", "signature": "abc"})
        );
        assert!(done.message.get("tool_calls").is_none());
    }

    #[test]
    fn a_malformed_tool_input_reaches_the_loop_raw() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"app\": "}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ];
        let done = feed_all(&events).0.finish().unwrap();
        let args = done.message["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert!(serde_json::from_str::<Value>(args).is_err(), "{args}");
        assert_eq!(
            done.message[RAW][0]["input"],
            json!({}),
            "what goes back stays valid"
        );
    }

    #[test]
    fn a_cut_off_or_failed_stream_is_an_error() {
        let start = json!({"type": "message_start", "message": {"usage": {}}});
        assert!(feed_all(std::slice::from_ref(&start)).0.finish().is_err());
        let mut acc = Accumulator::default();
        assert!(
            acc.feed(&json!({"type": "error", "error": {"type": "overloaded_error"}}).to_string())
                .is_err()
        );
    }

    #[test]
    fn a_whole_response_translates_too() {
        let v = json!({"type": "message", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Hi."}],
            "usage": {"input_tokens": 7, "output_tokens": 2}});
        let done = from_message(&v).unwrap();
        assert_eq!(done.message["content"], "Hi.");
        assert_eq!(done.finish_reason, "stop");
        assert_eq!(done.usage.unwrap()["total_tokens"], 9);
    }
}
