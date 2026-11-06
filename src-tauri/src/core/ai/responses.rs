//! Public Responses wire format for ChatGPT plan usage.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::ai::client::{Completion, MAX_TOOL_CALLS, ToolCall};
use crate::core::ai::tools;
use crate::core::error::{AppError, ErrorCode};

pub const BASE: &str = "https://api.openai.com/v1";
const MAX_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Model {
    pub slug: String,
    pub display_name: String,
    pub visibility: String,
}

#[derive(Deserialize)]
pub(super) struct ModelList {
    pub models: Vec<Model>,
}

pub(super) fn body(model: &str, instructions: &str, definitions: &Value) -> Value {
    let tools: Vec<Value> = definitions
        .as_array()
        .into_iter()
        .flatten()
        .map(|definition| {
            let mut function = definition["function"].clone();
            function["type"] = json!("function");
            function["strict"] = json!(false);
            function
        })
        .collect();
    json!({
        "model": model, "instructions": instructions, "input": [], "store": false, "stream": true,
        "tools": [{"type": "namespace", "name": "sandb", "description": "Database tools for sandb.", "tools": tools}]
    })
}

fn protocol(message: &'static str) -> AppError {
    AppError::new(ErrorCode::AiProtocol, message)
}

pub(super) fn upstream(body: &Value) -> AppError {
    let error = body
        .get("response")
        .and_then(|response| response.get("error"))
        .or_else(|| body.get("error"))
        .filter(|value| value.is_object())
        .unwrap_or(body);
    let mut result = AppError::new(
        ErrorCode::AiUpstream,
        "ChatGPT could not complete the response",
    );
    result.upstream_code = error.get("code").and_then(Value::as_str).map(str::to_owned);
    result.server_code = result.upstream_code.clone();
    result.upstream_param = error
        .get("param")
        .and_then(Value::as_str)
        .map(str::to_owned);
    result.detail = Some(
        body.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| body.to_string()),
    );
    result.upstream_body = Some(body.clone());
    result
}

fn field<'a>(item: &'a Value, name: &str) -> Result<&'a str, AppError> {
    item[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| protocol("ChatGPT returned a missing or invalid output field"))
}

fn index(event: &Value) -> Result<usize, AppError> {
    event["output_index"]
        .as_u64()
        .filter(|i| *i < MAX_ITEMS as u64)
        .map(|i| i as usize)
        .ok_or_else(|| protocol("ChatGPT returned an invalid output index"))
}

fn validate_item(item: &Value, complete: bool) -> Result<(), AppError> {
    field(item, "id")?;
    if complete
        && item
            .get("status")
            .is_some_and(|status| !status.is_null() && status != "completed")
    {
        return Err(protocol("ChatGPT returned unfinished output"));
    }
    if !complete
        && item.get("status").is_some_and(|status| {
            !status.is_null() && !matches!(status.as_str(), Some("in_progress" | "completed"))
        })
    {
        return Err(protocol("ChatGPT returned invalid output status"));
    }
    match item["type"].as_str() {
        Some("reasoning") => {
            if !item["summary"].is_array()
                || item["summary"].as_array().is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part["type"] != "summary_text" || !part["text"].is_string())
                })
                || item
                    .get("encrypted_content")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err(protocol("ChatGPT returned invalid reasoning output"));
            }
        }
        Some("message") => {
            if item["role"] != "assistant"
                || !item["content"].is_array()
                || item.get("phase").is_some_and(|phase| {
                    !phase.is_null()
                        && !matches!(phase.as_str(), Some("commentary" | "final_answer"))
                })
            {
                return Err(protocol("ChatGPT returned an invalid assistant message"));
            }
            for content in item["content"].as_array().expect("validated content") {
                match content["type"].as_str() {
                    Some("output_text") if content["text"].is_string() => {}
                    Some("refusal") if content["refusal"].is_string() => {}
                    _ => return Err(protocol("ChatGPT returned unsupported message content")),
                }
            }
        }
        Some("function_call") => {
            field(item, "call_id")?;
            let name = field(item, "name")?;
            let definitions = tools::definitions(&[]);
            if item["namespace"] != "sandb"
                || !definitions
                    .as_array()
                    .expect("shared tool definitions")
                    .iter()
                    .any(|definition| definition["function"]["name"] == name)
            {
                return Err(protocol("ChatGPT returned an unsupported local function"));
            }
            let arguments = item["arguments"]
                .as_str()
                .ok_or_else(|| protocol("ChatGPT returned invalid function arguments"))?;
            if arguments.len() > MAX_TEXT_BYTES {
                return Err(protocol("ChatGPT function arguments are too large"));
            }
            if complete {
                let parsed: Value = serde_json::from_str(arguments)
                    .map_err(|_| protocol("ChatGPT returned invalid function arguments"))?;
                if !parsed.is_object() {
                    return Err(protocol("ChatGPT function arguments must be an object"));
                }
            }
        }
        _ => return Err(protocol("ChatGPT returned unsupported output")),
    }
    Ok(())
}

struct PendingItem {
    item: Value,
    fragmented: bool,
    arguments_done: bool,
    done: Option<Value>,
}

#[derive(Default)]
pub(super) struct Stream {
    items: BTreeMap<usize, PendingItem>,
    text: BTreeMap<(usize, usize), String>,
    done_text: BTreeMap<(usize, usize), String>,
}

impl Stream {
    pub(super) fn event(
        &mut self,
        out: &mut Completion,
        event: &Value,
        on_text: &mut impl FnMut(&str) -> bool,
    ) -> Result<bool, AppError> {
        match event["type"].as_str() {
            Some("response.output_text.delta") => {
                for name in ["output_index", "content_index"] {
                    if event
                        .get(name)
                        .is_some_and(|v| v.as_u64().is_none_or(|i| i >= MAX_ITEMS as u64))
                    {
                        return Err(protocol("ChatGPT sent an invalid text index"));
                    }
                }
                let delta = event["delta"]
                    .as_str()
                    .ok_or_else(|| protocol("ChatGPT sent an invalid text delta"))?;
                if out.text.len() + delta.len() > MAX_TEXT_BYTES {
                    return Err(protocol("ChatGPT text is too large"));
                }
                let key = text_key(event)?;
                if self.done_text.contains_key(&key) {
                    return Err(protocol(
                        "ChatGPT sent text after its content was completed",
                    ));
                }
                self.text.entry(key).or_default().push_str(delta);
                out.text.push_str(delta);
                if !delta.is_empty() && !on_text(delta) {
                    return Err(AppError::new(ErrorCode::Cancelled, "Stopped"));
                }
            }
            Some("response.output_text.done") => {
                let text = event["text"]
                    .as_str()
                    .ok_or_else(|| protocol("ChatGPT sent invalid completed text"))?;
                let key = text_key(event)?;
                if text.len() > MAX_TEXT_BYTES
                    || self.done_text.contains_key(&key)
                    || self.text.get(&key).is_some_and(|deltas| deltas != text)
                {
                    return Err(protocol("ChatGPT completed text disagrees with its deltas"));
                }
                self.done_text.insert(key, text.to_owned());
            }
            Some("response.output_item.added") => {
                let index = index(event)?;
                let item = &event["item"];
                validate_item(item, false)?;
                if self.items.contains_key(&index)
                    || self.items.values().any(|old| {
                        old.item["id"] == item["id"]
                            || (item["type"] == "function_call"
                                && old.item["call_id"] == item["call_id"])
                    })
                {
                    return Err(protocol("ChatGPT repeated an output item"));
                }
                if item["type"] == "function_call"
                    && self
                        .items
                        .values()
                        .filter(|old| old.item["type"] == "function_call")
                        .count()
                        >= MAX_TOOL_CALLS
                {
                    return Err(protocol("ChatGPT returned too many function calls"));
                }
                self.items.insert(
                    index,
                    PendingItem {
                        item: item.clone(),
                        fragmented: false,
                        arguments_done: false,
                        done: None,
                    },
                );
            }
            Some(
                "response.function_call_arguments.delta" | "response.function_call_arguments.done",
            ) => {
                let pending = self
                    .items
                    .get_mut(&index(event)?)
                    .ok_or_else(|| protocol("ChatGPT sent arguments for an unknown function"))?;
                if pending.item["type"] != "function_call"
                    || pending.item["id"] != field(event, "item_id")?
                    || pending.done.is_some()
                    || pending.arguments_done
                {
                    return Err(protocol("ChatGPT sent inconsistent function arguments"));
                }
                if event["type"] == "response.function_call_arguments.delta" {
                    let delta = event["delta"]
                        .as_str()
                        .ok_or_else(|| protocol("ChatGPT sent an invalid argument delta"))?;
                    let arguments = pending.item["arguments"]
                        .as_str()
                        .expect("validated arguments");
                    if arguments.len() + delta.len() > MAX_TEXT_BYTES {
                        return Err(protocol("ChatGPT function arguments are too large"));
                    }
                    pending.item["arguments"] = json!(format!("{arguments}{delta}"));
                    pending.fragmented = true;
                } else {
                    let arguments = event["arguments"]
                        .as_str()
                        .ok_or_else(|| protocol("ChatGPT sent invalid completed arguments"))?;
                    if arguments.len() > MAX_TEXT_BYTES
                        || ((pending.fragmented
                            || pending.item["arguments"]
                                .as_str()
                                .is_some_and(|s| !s.is_empty()))
                            && pending.item["arguments"] != arguments)
                    {
                        return Err(protocol(
                            "ChatGPT completed arguments disagree with their fragments",
                        ));
                    }
                    pending.item["arguments"] = json!(arguments);
                    pending.arguments_done = true;
                }
            }
            Some("response.output_item.done") => {
                let item = &event["item"];
                validate_item(item, true)?;
                let pending = self
                    .items
                    .get_mut(&index(event)?)
                    .ok_or_else(|| protocol("ChatGPT completed an unknown output item"))?;
                if pending.done.is_some() {
                    return Err(protocol("ChatGPT repeated completed output"));
                }
                check_pending(pending, item)?;
                pending.done = Some(item.clone());
            }
            Some("response.failed" | "error") => return Err(upstream(event)),
            Some("response.incomplete") => {
                let mut error = upstream(event);
                error.code = ErrorCode::AiProtocol;
                return Err(error);
            }
            Some("response.completed") => return self.complete(out, &event["response"], on_text),
            Some(_) => {}
            None => return Err(protocol("ChatGPT stream event has no type")),
        }
        Ok(false)
    }

    fn complete(
        &self,
        out: &mut Completion,
        response: &Value,
        on_text: &mut impl FnMut(&str) -> bool,
    ) -> Result<bool, AppError> {
        if response["status"] != "completed" || !response["error"].is_null() {
            return Err(protocol("ChatGPT did not complete the response"));
        }
        field(response, "id")?;
        let items = response["output"]
            .as_array()
            .ok_or_else(|| protocol("ChatGPT omitted completed output"))?;
        if items.len() > MAX_ITEMS {
            return Err(protocol("ChatGPT returned too many output items"));
        }
        // A successful terminal envelope can carry an empty snapshot; only item.done events can supply the output.
        let items: BTreeMap<usize, &Value> = if items.is_empty() {
            self.items
                .iter()
                .map(|(index, pending)| {
                    pending
                        .done
                        .as_ref()
                        .map(|item| (*index, item))
                        .ok_or_else(|| protocol("ChatGPT omitted provisional output"))
                })
                .collect::<Result<_, _>>()?
        } else {
            items.iter().enumerate().collect()
        };
        for (index, pending) in &self.items {
            let item = items
                .get(index)
                .ok_or_else(|| protocol("ChatGPT omitted provisional output"))?;
            check_pending(pending, item)?;
        }
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut ids = HashSet::new();
        for item in items.values() {
            validate_item(item, true)?;
            if !ids.insert(field(item, "id")?) {
                return Err(protocol("ChatGPT repeated an output ID"));
            }
            match item["type"].as_str() {
                Some("reasoning") => {}
                Some("message") => {
                    for content in item["content"].as_array().expect("validated content") {
                        let part = content[if content["type"] == "refusal" {
                            "refusal"
                        } else {
                            "text"
                        }]
                        .as_str()
                        .expect("validated text");
                        if text.len() + part.len() > MAX_TEXT_BYTES {
                            return Err(protocol("ChatGPT text is too large"));
                        }
                        text.push_str(part);
                    }
                }
                Some("function_call") => {
                    let id = field(item, "call_id")?;
                    if calls.len() >= MAX_TOOL_CALLS
                        || calls.iter().any(|call: &ToolCall| call.id == id)
                    {
                        return Err(protocol(
                            "ChatGPT returned duplicate or excessive function calls",
                        ));
                    }
                    calls.push(ToolCall {
                        id: id.to_owned(),
                        name: field(item, "name")?.to_owned(),
                        arguments: field(item, "arguments")?.to_owned(),
                    });
                }
                _ => unreachable!("validated output type"),
            }
        }
        for ((index, content), deltas) in &self.text {
            if *index != usize::MAX {
                let final_text = items
                    .get(index)
                    .and_then(|item| item.get("content"))
                    .and_then(|parts| parts.get(*content))
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol("ChatGPT text delta targets missing output"))?;
                if !final_text.starts_with(deltas) {
                    return Err(protocol(
                        "ChatGPT output disagrees with its indexed text deltas",
                    ));
                }
            }
        }
        for ((index, content), done) in &self.done_text {
            let final_text = if *index == usize::MAX {
                text.as_str()
            } else {
                items
                    .get(index)
                    .and_then(|item| item.get("content"))
                    .and_then(|parts| parts.get(*content))
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol("ChatGPT omitted completed text"))?
            };
            if done != final_text {
                return Err(protocol(
                    "ChatGPT terminal text disagrees with completed text",
                ));
            }
        }
        if !text.starts_with(&out.text) {
            return Err(protocol("ChatGPT completed text disagrees with its stream"));
        }
        if text.len() > out.text.len() && !on_text(&text[out.text.len()..]) {
            return Err(AppError::new(ErrorCode::Cancelled, "Stopped"));
        }
        out.text = text;
        out.tool_calls = calls;
        out.output = items
            .into_values()
            .cloned()
            .map(|mut item| {
                if let Some(object) = item.as_object_mut() {
                    object.remove("created_by");
                }
                item
            })
            .collect();
        out.finish_reason = Some("completed".into());
        Ok(true)
    }
}

fn text_key(event: &Value) -> Result<(usize, usize), AppError> {
    let output = if event.get("output_index").is_some() {
        index(event)?
    } else {
        usize::MAX
    };
    let content = match event.get("content_index") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|i| *i < MAX_ITEMS as u64)
            .map(|i| i as usize)
            .ok_or_else(|| protocol("ChatGPT sent an invalid content index"))?,
    };
    Ok((output, content))
}

fn check_pending(pending: &PendingItem, item: &Value) -> Result<(), AppError> {
    for name in ["id", "type", "name", "namespace", "call_id"] {
        if pending
            .item
            .get(name)
            .is_some_and(|value| item.get(name) != Some(value))
        {
            return Err(protocol("ChatGPT completed output changed its identity"));
        }
    }
    if (pending.fragmented
        || pending.arguments_done
        || pending.item["arguments"]
            .as_str()
            .is_some_and(|s| !s.is_empty()))
        && pending.item["arguments"] != item["arguments"]
    {
        return Err(protocol(
            "ChatGPT completed arguments disagree with their fragments",
        ));
    }
    if let Some(done) = &pending.done {
        for (name, value) in done.as_object().expect("validated output object") {
            if name != "created_by" && item.get(name) != Some(value) {
                return Err(protocol(
                    "ChatGPT terminal output disagrees with completed items",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "responses_fixture.rs"]
mod tests;
