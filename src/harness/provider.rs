use super::{
    control::Control,
    model::{Block, Event, Profile, Provider, Turn, Usage},
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct Reply {
    pub turn: Turn,
    pub stop: String,
    pub usage: Usage,
}

pub fn block_text(block: &Block) -> Option<String> {
    match block {
        Block::Text { text } => Some(text.clone()),
        Block::Context { source, text } => Some(format!(
            "<workspace-context source={source:?}>\n{text}\n</workspace-context>"
        )),
        _ => None,
    }
}

pub fn request(profile: &Profile, system: &str, turns: &[Turn], tools: &[ToolDef]) -> Value {
    if profile.provider == Provider::Anthropic {
        let mut messages: Vec<Value> = Vec::new();
        for turn in turns {
            let content: Vec<Value> = if let Some(wire) = &turn.wire {
                wire.get("anthropic")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| anthropic_blocks(turn))
            } else {
                anthropic_blocks(turn)
            };
            if let Some(last) = messages.last_mut().filter(|m| m["role"] == turn.role) {
                last["content"].as_array_mut().unwrap().extend(content);
            } else {
                messages.push(json!({"role":turn.role,"content":content}));
            }
        }
        let mut body = json!({"model":profile.model,"max_tokens":4096,"stream":true,"cache_control":{"type":"ephemeral"},
            "system":[{"type":"text","text":system,"cache_control":{"type":"ephemeral"}}],
            "messages":messages,
            "tools": tools.iter().map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters})).collect::<Vec<_>>()});
        if tools.is_empty() {
            body.as_object_mut().unwrap().remove("tools");
        }
        body
    } else {
        let mut messages = vec![json!({"role":"system","content":system})];
        for turn in turns {
            if let Some(wire) = turn.wire.as_ref().and_then(|v| v.get("openai")) {
                messages.push(wire.clone());
                continue;
            }
            let text = turn
                .blocks
                .iter()
                .filter_map(block_text)
                .collect::<Vec<_>>()
                .join("\n\n");
            let calls: Vec<_> = turn.blocks.iter().filter_map(|b| if let Block::ToolCall{id,name,input} = b {
                Some(json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}}))
            } else { None }).collect();
            if !text.is_empty() || !calls.is_empty() {
                let mut message = json!({"role":turn.role,"content":text});
                if !calls.is_empty() {
                    message["tool_calls"] = json!(calls);
                }
                messages.push(message);
            }
            for block in &turn.blocks {
                if let Block::ToolResult { id, text, error } = block {
                    messages.push(json!({"role":"tool","tool_call_id":id,"content":if *error {format!("ERROR: {text}")} else {text.clone()}}));
                }
            }
        }
        let mut body = json!({"model":profile.model,"messages":messages,"stream":true,
            "tools":tools.iter().map(|t| json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}})).collect::<Vec<_>>()});
        if profile.provider == Provider::OpenAI {
            body["stream_options"] = json!({"include_usage":true});
            body["max_completion_tokens"] = json!(4096);
        } else {
            body["max_tokens"] = json!(4096);
        }
        if tools.is_empty() {
            body.as_object_mut().unwrap().remove("tools");
        }
        body
    }
}

fn anthropic_blocks(turn: &Turn) -> Vec<Value> {
    turn.blocks
        .iter()
        .map(|block| match block {
            Block::ToolCall { id, name, input } => {
                json!({"type":"tool_use","id":id,"name":name,"input":input})
            }
            Block::ToolResult { id, text, error } => {
                json!({"type":"tool_result","tool_use_id":id,"content":text,"is_error":error})
            }
            block => json!({"type":"text","text":block_text(block).unwrap_or_default()}),
        })
        .collect()
}

#[derive(Default)]
pub struct Sse {
    pending: Vec<u8>,
    data: Vec<String>,
}

impl Sse {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 4_000_000 {
            return Err("SSE event exceeded 4 MB".into());
        }
        let mut events = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)
                .map_err(|e| e.to_string())?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
        Ok(events)
    }
}

#[derive(Default)]
struct Accumulator {
    text: String,
    calls: BTreeMap<u64, Value>,
    arguments: BTreeMap<u64, String>,
    content: BTreeMap<u64, Value>,
    usage: Usage,
    stop: String,
    done: bool,
}

impl Accumulator {
    fn push(&mut self, data: &str, anthropic: bool) -> Result<String, String> {
        if data == "[DONE]" {
            self.done = true;
            return Ok(String::new());
        }
        let value: Value =
            serde_json::from_str(data).map_err(|e| format!("Invalid stream event: {e}"))?;
        if let Some(error) = value.get("error") {
            return Err(format!("Provider stream error: {error}"));
        }
        let mut delta = String::new();
        if anthropic {
            let index = value["index"].as_u64().unwrap_or(0);
            match value["type"].as_str().unwrap_or_default() {
                "message_start" => self.usage = anthropic_usage(&value["message"]["usage"]),
                "content_block_start" => {
                    self.content.insert(index, value["content_block"].clone());
                }
                "content_block_delta" => {
                    let d = &value["delta"];
                    if let Some(text) = d["text"].as_str() {
                        delta = text.into();
                        let block = self
                            .content
                            .get_mut(&index)
                            .ok_or("Delta without content block")?;
                        let mut text_before =
                            block["text"].as_str().unwrap_or_default().to_string();
                        text_before.push_str(text);
                        block["text"] = json!(text_before);
                    }
                    if let Some(part) = d["partial_json"].as_str() {
                        self.arguments.entry(index).or_default().push_str(part);
                    }
                    for key in ["thinking", "signature"] {
                        if let Some(part) = d[key].as_str() {
                            let block = self
                                .content
                                .get_mut(&index)
                                .ok_or("Delta without content block")?;
                            let prior = block[key].as_str().unwrap_or_default();
                            block[key] = json!(format!("{prior}{part}"));
                        }
                    }
                }
                "message_delta" => {
                    self.stop = value["delta"]["stop_reason"]
                        .as_str()
                        .unwrap_or_default()
                        .into();
                    self.usage.output = value["usage"]["output_tokens"].as_u64().unwrap_or(0);
                }
                "message_stop" => self.done = true,
                _ => {}
            }
        } else {
            if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
                self.usage = Usage {
                    input: usage["prompt_tokens"].as_u64().unwrap_or(0),
                    output: usage["completion_tokens"].as_u64().unwrap_or(0),
                    cached: usage["prompt_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                };
            }
            if let Some(choice) = value["choices"].as_array().and_then(|v| v.first()) {
                if let Some(stop) = choice["finish_reason"].as_str() {
                    self.stop = stop.into();
                }
                let d = &choice["delta"];
                delta = d["content"]
                    .as_str()
                    .or(d["refusal"].as_str())
                    .unwrap_or_default()
                    .into();
                self.text.push_str(&delta);
                if let Some(calls) = d["tool_calls"].as_array() {
                    for call in calls {
                        let index = call["index"].as_u64().ok_or("Tool delta without index")?;
                        let out = self.calls.entry(index).or_insert_with(|| json!({"type":"function","id":"","function":{"name":"","arguments":""}}));
                        for key in ["id", "type", "extra_content"] {
                            if !call[key].is_null() {
                                out[key] = call[key].clone();
                            }
                        }
                        for key in ["name", "arguments"] {
                            if let Some(part) = call["function"][key].as_str() {
                                let prior = out["function"][key].as_str().unwrap_or_default();
                                out["function"][key] = json!(format!("{prior}{part}"));
                            }
                        }
                    }
                }
            }
        }
        Ok(delta)
    }

    fn finish(mut self, anthropic: bool) -> Result<Reply, String> {
        if !self.done || self.stop.is_empty() {
            return Err(
                "Provider stream ended before a complete reply. Partial output discarded.".into(),
            );
        }
        let mut blocks = Vec::new();
        let wire = if anthropic {
            for (index, input) in self.arguments {
                self.content.get_mut(&index).ok_or("Missing tool block")?["input"] =
                    serde_json::from_str(&input)
                        .map_err(|e| format!("Invalid complete tool arguments: {e}"))?;
            }
            let content: Vec<_> = self.content.into_values().collect();
            for block in &content {
                match block["type"].as_str() {
                    Some("text") => blocks.push(Block::Text {
                        text: block["text"].as_str().unwrap_or_default().into(),
                    }),
                    Some("tool_use") => blocks.push(Block::ToolCall {
                        id: required(block, "id")?,
                        name: required(block, "name")?,
                        input: block["input"].clone(),
                    }),
                    _ => {}
                }
            }
            json!({"anthropic":content})
        } else {
            if !self.text.is_empty() {
                blocks.push(Block::Text {
                    text: self.text.clone(),
                });
            }
            let calls: Vec<_> = self.calls.into_values().collect();
            for call in &calls {
                blocks.push(Block::ToolCall {
                    id: required(call, "id")?,
                    name: required(&call["function"], "name")?,
                    input: serde_json::from_str(
                        call["function"]["arguments"].as_str().unwrap_or_default(),
                    )
                    .map_err(|e| format!("Invalid complete tool arguments: {e}"))?,
                });
            }
            let mut message = json!({"role":"assistant","content":self.text});
            if !calls.is_empty() {
                message["tool_calls"] = json!(calls);
            }
            json!({"openai":message})
        };
        Ok(Reply {
            turn: Turn {
                role: "assistant".into(),
                blocks,
                wire: Some(wire),
            },
            stop: self.stop,
            usage: self.usage,
        })
    }
}

fn required(value: &Value, key: &str) -> Result<String, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing {key} in provider response"))
}

fn anthropic_usage(value: &Value) -> Usage {
    let cached = value["cache_read_input_tokens"].as_u64().unwrap_or(0);
    Usage {
        input: value["input_tokens"].as_u64().unwrap_or(0)
            + cached
            + value["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        output: value["output_tokens"].as_u64().unwrap_or(0),
        cached,
    }
}

pub fn retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500 | 502 | 503 | 504 | 529)
}

fn retry_delay(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<f64>() {
        return seconds
            .is_finite()
            .then(|| Duration::from_secs_f64(seconds.clamp(0.0, 300.0)));
    }
    httpdate::parse_http_date(value).ok().map(|date| {
        date.duration_since(std::time::SystemTime::now())
            .unwrap_or_default()
            .min(Duration::from_secs(300))
    })
}

pub async fn complete(
    profile: &Profile,
    body: &Value,
    run: &str,
    round: usize,
    store: &Store,
    control: Arc<Control>,
    events: &mpsc::Sender<Event>,
) -> Result<Reply, String> {
    let anthropic = profile.provider == Provider::Anthropic;
    let url = format!(
        "{}/{}",
        profile.endpoint.trim_end_matches('/'),
        if anthropic {
            "messages"
        } else {
            "chat/completions"
        }
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last = String::new();
    let capture_id = format!("{round:04}-{}", super::model::id());
    for attempt in 0..3 {
        control.checkpoint().await?;
        let _ = events.send(Event::ResetStream(run.into()));
        store.save(
            &format!("runs/{run}/requests/{capture_id}-{attempt}.json"),
            body,
        )?;
        let mut req = client.post(&url).json(body);
        if !profile.key_env.is_empty() {
            let key = std::env::var(&profile.key_env)
                .map_err(|_| format!("Missing {}", profile.key_env))?;
            req = if anthropic {
                req.header("x-api-key", key)
            } else {
                req.bearer_auth(key)
            };
        }
        if anthropic {
            req = req.header("anthropic-version", "2023-06-01");
        }
        let mut delay =
            Duration::from_millis((1_u64 << attempt) * 1000 + (super::model::now() % 7) * 97);
        let response = tokio::select! { result = req.send() => result, _ = control.cancelled() => return Err("Stopped by user".into()) };
        let result = match response {
            Ok(mut response) if response.status().is_success() => {
                let mut sse = Sse::default();
                let mut acc = Accumulator::default();
                let mut capture = Vec::new();
                let mut size = 0;
                let read = async {
                    'stream: while let Some(chunk) = response
                        .chunk()
                        .await
                        .map_err(|e| format!("Stream interrupted: {e}"))?
                    {
                        size += chunk.len();
                        if size > 4_000_000 {
                            return Err("Reply exceeded 4 MB".into());
                        }
                        for data in sse.push(&chunk)? {
                            let delta = acc.push(&data, anthropic)?;
                            capture.push(data);
                            if !delta.is_empty() {
                                let _ = events.send(Event::Delta {
                                    run: run.into(),
                                    text: delta,
                                });
                            }
                            if acc.done {
                                break 'stream;
                            }
                        }
                    }
                    acc.finish(anthropic)
                };
                let result = tokio::select! { result = read => result, _ = control.cancelled() => return Err("Stopped by user".into()) };
                store.save(
                    &format!("runs/{run}/responses/{capture_id}-{attempt}.json"),
                    &capture,
                )?;
                result
            }
            Ok(mut response) => {
                let status = response.status().as_u16();
                if let Some(after) = response
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(retry_delay)
                {
                    delay = after;
                }
                let mut bytes = Vec::new();
                loop {
                    let chunk = tokio::select! {result=response.chunk()=>result.map_err(|e|e.to_string())?,_=control.cancelled()=>return Err("Stopped by user".into())};
                    let Some(chunk) = chunk else {
                        break;
                    };
                    let count = chunk.len().min(16_000_usize.saturating_sub(bytes.len()));
                    bytes.extend_from_slice(&chunk[..count]);
                    if bytes.len() >= 16_000 {
                        break;
                    }
                }
                let mut detail = String::from_utf8_lossy(&bytes).into_owned();
                if let Ok(key) = std::env::var(&profile.key_env)
                    && !key.is_empty()
                {
                    detail = detail.replace(&key, "[redacted credential]");
                }
                let error = format!("Provider returned HTTP {status}: {detail}");
                store.save(
                    &format!("runs/{run}/responses/{capture_id}-{attempt}.json"),
                    &json!({"error":error,"status":status}),
                )?;
                if !retryable(status) {
                    return Err(error);
                }
                Err(error)
            }
            Err(e) => Err(format!("Provider connection failed: {e}")),
        };
        match result {
            Ok(reply) => return Ok(reply),
            Err(error) => {
                last = error;
                store.save(
                    &format!("runs/{run}/failures/{capture_id}-{attempt}.json"),
                    &json!({"error":last,"retry":attempt+1}),
                )?;
                let _ = events.send(Event::ResetStream(run.into()));
                if attempt < 2 {
                    let _ = events.send(Event::Delta {
                        run: run.into(),
                        text: format!("Retrying request ({}/3)…", attempt + 2),
                    });
                    tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = control.cancelled() => return Err("Stopped by user".into()) }
                }
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_handles_utf8_split_across_network_chunks_and_multiline_data() {
        let mut parser = Sse::default();
        let input = "data: {\"text\":\"你好\"}\r\n\r\ndata: one\ndata: two\n\n".as_bytes();
        let mut events = Vec::new();
        for byte in input {
            events.extend(parser.push(&[*byte]).unwrap());
        }
        assert_eq!(events, vec!["{\"text\":\"你好\"}", "one\ntwo"]);
    }

    #[test]
    fn a_partial_stream_never_produces_an_executable_tool_call() {
        let mut acc = Accumulator::default();
        acc.push(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"write_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}).to_string(),false).unwrap();
        assert!(acc.finish(false).is_err());
    }

    #[test]
    fn complete_tool_arguments_are_validated_and_thought_signatures_are_preserved() {
        let mut acc = Accumulator::default();
        acc.push(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"read_file","arguments":"{\"path\":"},"extra_content":{"google":{"thought_signature":"opaque_signature"}}}]},"finish_reason":null}]}).to_string(),false).unwrap();
        acc.push(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]},"finish_reason":"tool_calls"}]}).to_string(),false).unwrap();
        acc.push("[DONE]", false).unwrap();
        let reply = acc.finish(false).unwrap();
        assert!(matches!(&reply.turn.blocks[0],Block::ToolCall{input,..} if input["path"]=="x"));
        let profile = Profile {
            provider: Provider::Gemini,
            ..Profile::default()
        };
        let request = request(&profile, "stable", &[reply.turn], &[]);
        assert_eq!(
            request["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "opaque_signature"
        );
    }

    #[test]
    fn anthropic_tool_result_pairs_and_cache_prefix_survive_serialization() {
        let mut acc = Accumulator::default();
        for event in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":20}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"read_file","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"partial_json":"{\"path\":\"README.md\"}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
            json!({"type":"message_stop"}),
        ] {
            acc.push(&event.to_string(), true).unwrap();
        }
        let reply = acc.finish(true).unwrap();
        assert_eq!(reply.usage.input, 30);
        assert_eq!(reply.usage.cached, 20);
        let profile = Profile {
            provider: Provider::Anthropic,
            ..Profile::default()
        };
        let mut turns = vec![Turn::text("user", "read"), reply.turn];
        let before = request(&profile, "stable", &turns, &[]);
        turns.push(Turn {
            role: "user".into(),
            blocks: vec![Block::ToolResult {
                id: "call".into(),
                text: "actual contents".into(),
                error: false,
            }],
            wire: None,
        });
        let after = request(&profile, "stable", &turns, &[]);
        assert_eq!(before["system"], after["system"]);
        assert_eq!(
            before["messages"],
            json!(after["messages"].as_array().unwrap()[..2])
        );
        assert_eq!(after["messages"][2]["content"][0]["tool_use_id"], "call");
    }

    #[test]
    fn permanent_errors_are_not_retried() {
        assert!(!retryable(400));
        assert!(!retryable(401));
        assert!(!retryable(403));
        assert!(retryable(429));
        assert!(retryable(529));
    }

    #[test]
    fn history_caching_and_both_retry_after_formats_are_supported() {
        let profile = Profile {
            provider: Provider::Anthropic,
            ..Profile::default()
        };
        let body = request(&profile, "stable", &[Turn::text("user", "first")], &[]);
        assert_eq!(body["cache_control"], json!({"type":"ephemeral"}));
        assert_eq!(retry_delay("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(retry_delay("nan"), None);
        let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(15));
        let delay = retry_delay(&date).unwrap();
        assert!((13..=15).contains(&delay.as_secs()));
        assert_eq!(retry_delay("invalid"), None);
    }
}
