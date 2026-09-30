//! Streaming chat completions (phase-5 spec §5.1), as two pure pieces:
//!
//! - [`SseDecoder`]: bytes as they arrive → the `data:` payloads of complete
//!   server-sent events. Network chunks split lines, events and even UTF-8
//!   characters anywhere; only complete lines are decoded.
//! - [`Accumulator`]: those payloads → text deltas as they come, and at the
//!   end the same assistant message a non-streamed response would have
//!   held, with tool calls rebuilt from their fragments. The tool loop
//!   after it is unchanged, so the gate sees exactly what it saw before.

use serde_json::{Value, json};

/// Splits a server-sent-event byte stream into `data:` payloads.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    /// `data:` lines of the event in progress (an event may have several).
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed bytes; returns the payloads of every event they completed.
    /// `[DONE]` is returned like any other payload.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // A blank line dispatches the event.
                if !self.data.is_empty() {
                    out.push(std::mem::take(&mut self.data).join("\n"));
                }
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_owned());
            }
            // Comments (`:`) and other fields (`event:`, `id:`) are ignored.
        }
        out
    }

    /// The stream ended: an event still missing its blank line counts.
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            if let Some(v) = rest.trim_end().strip_prefix("data:") {
                self.data.push(v.trim_start().to_owned());
            }
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

#[derive(Debug, Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// Builds the response from `chat.completion.chunk` objects.
#[derive(Debug, Default)]
pub struct Accumulator {
    content: String,
    calls: Vec<PartialCall>,
    finish: Option<String>,
    usage: Option<Value>,
    done: bool,
}

/// A streamed response, reassembled.
#[derive(Debug, Clone, PartialEq)]
pub struct Completed {
    /// The assistant message, shaped as in a non-streamed response.
    pub message: Value,
    pub finish_reason: String,
    pub usage: Option<Value>,
}

impl Accumulator {
    /// Take one payload. Returns the text it added, if any.
    pub fn feed(&mut self, payload: &str) -> Result<Option<String>, String> {
        if payload.trim() == "[DONE]" {
            self.done = true;
            return Ok(None);
        }
        let chunk: Value = serde_json::from_str(payload)
            .map_err(|e| format!("bad stream chunk ({e}): {payload}"))?;
        if let Some(err) = chunk.get("error") {
            return Err(format!("the API reported an error mid-stream: {err}"));
        }
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(usage.clone());
        }
        let Some(choice) = chunk["choices"].get(0) else {
            return Ok(None); // the usage-only chunk
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish = Some(reason.to_owned());
        }
        let delta = &choice["delta"];
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_u64().unwrap_or(0) as usize;
            if self.calls.len() <= index {
                self.calls.resize(index + 1, PartialCall::default());
            }
            let slot = &mut self.calls[index];
            if let Some(id) = call["id"].as_str() {
                slot.id.push_str(id);
            }
            if let Some(name) = call["function"]["name"].as_str() {
                slot.name.push_str(name);
            }
            if let Some(args) = call["function"]["arguments"].as_str() {
                slot.arguments.push_str(args);
            }
        }
        Ok(delta["content"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| {
                self.content.push_str(t);
                t.to_owned()
            }))
    }

    /// The whole response. Fails if the stream never said how it finished:
    /// a cut-off stream must not become a half-built tool call.
    pub fn finish(self) -> Result<Completed, String> {
        let Some(finish_reason) = self.finish else {
            return Err(if self.done {
                "the stream ended without a finish reason".into()
            } else {
                "the stream was cut off before the response finished".into()
            });
        };
        let mut message = json!({
            "role": "assistant",
            "content": if self.content.is_empty() { Value::Null } else { Value::String(self.content) },
        });
        if !self.calls.is_empty() {
            message["tool_calls"] = self
                .calls
                .into_iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments },
                    })
                })
                .collect();
        }
        Ok(Completed {
            message,
            finish_reason,
            usage: self.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: Value, finish: Option<&str>) -> String {
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}).to_string()
    }

    #[test]
    fn events_survive_arbitrary_splits_including_inside_a_character() {
        let stream = "data: {\"a\":\"café\"}\n\n: keep-alive\n\ndata: [DONE]\n\n".as_bytes();
        for cut in 0..stream.len() {
            let mut d = SseDecoder::default();
            let mut got = d.push(&stream[..cut]);
            got.extend(d.push(&stream[cut..]));
            assert_eq!(got, ["{\"a\":\"café\"}", "[DONE]"], "cut at {cut}");
        }
    }

    #[test]
    fn crlf_and_multi_line_data_and_a_missing_final_blank_line() {
        let mut d = SseDecoder::default();
        assert_eq!(d.push(b"data: one\r\ndata: two\r\n\r\n"), ["one\ntwo"]);
        assert!(d.push(b"data: tail").is_empty());
        assert_eq!(d.finish().as_deref(), Some("tail"));
    }

    #[test]
    fn text_streams_and_reassembles() {
        let mut a = Accumulator::default();
        assert_eq!(
            a.feed(&chunk(json!({"role": "assistant", "content": ""}), None))
                .unwrap(),
            None
        );
        assert_eq!(
            a.feed(&chunk(json!({"content": "Hello"}), None))
                .unwrap()
                .as_deref(),
            Some("Hello")
        );
        assert_eq!(
            a.feed(&chunk(json!({"content": " there."}), None))
                .unwrap()
                .as_deref(),
            Some(" there.")
        );
        a.feed(&chunk(json!({}), Some("stop"))).unwrap();
        a.feed(&json!({"choices": [], "usage": {"total_tokens": 42}}).to_string())
            .unwrap();
        a.feed("[DONE]").unwrap();
        let done = a.finish().unwrap();
        assert_eq!(done.message["content"], "Hello there.");
        assert_eq!(done.finish_reason, "stop");
        assert_eq!(done.usage.unwrap()["total_tokens"], 42);
    }

    #[test]
    fn tool_calls_are_rebuilt_from_fragments_by_index() {
        let mut a = Accumulator::default();
        let call = |index: u64, v: Value| {
            let mut c = v;
            c["index"] = json!(index);
            chunk(json!({"tool_calls": [c]}), None)
        };
        a.feed(&call(0, json!({"id": "call_a", "type": "function", "function": {"name": "run_in_", "arguments": ""}}))).unwrap();
        a.feed(&call(
            0,
            json!({"function": {"name": "terminal", "arguments": "{\"comm"}}),
        ))
        .unwrap();
        a.feed(&call(
            1,
            json!({"id": "call_b", "function": {"name": "list_windows", "arguments": "{}"}}),
        ))
        .unwrap();
        a.feed(&call(
            0,
            json!({"function": {"arguments": "and\":\"htop\"}"}}),
        ))
        .unwrap();
        a.feed(&chunk(json!({}), Some("tool_calls"))).unwrap();
        let done = a.finish().unwrap();
        let calls = done.message["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["id"], "call_a");
        assert_eq!(calls[0]["function"]["name"], "run_in_terminal");
        assert_eq!(calls[0]["function"]["arguments"], "{\"command\":\"htop\"}");
        assert_eq!(calls[1]["function"]["name"], "list_windows");
        assert!(done.message["content"].is_null());
    }

    #[test]
    fn a_cut_off_stream_is_an_error_not_a_half_built_call() {
        let mut a = Accumulator::default();
        a.feed(&chunk(json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "run_in_terminal", "arguments": "{\"command\":\"rm"}}]}), None)).unwrap();
        assert!(a.finish().unwrap_err().contains("cut off"));
    }

    #[test]
    fn errors_mid_stream_and_garbage_are_reported() {
        let mut a = Accumulator::default();
        assert!(
            a.feed(r#"{"error":{"message":"overloaded"}}"#)
                .unwrap_err()
                .contains("overloaded")
        );
        assert!(Accumulator::default().feed("not json").is_err());
    }
}
