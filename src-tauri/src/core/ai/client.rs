//! AI HTTP client for two wire formats. OpenAI-compatible: `GET /models` and a streaming
//! `POST /chat/completions`. Anthropic-compatible: `GET /models` and a streaming `POST /messages`.
//! The caller always builds an OpenAI chat body; the Anthropic path translates it.
//! Every error leaves out the API key, also when the server echoes it back.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::core::error::{AppError, ErrorCode};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the response headers and for each stream chunk. Local servers can take
/// long to load a model before the first token.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MODELS_TIMEOUT: Duration = Duration::from_secs(15);
/// Characters of an error body kept in the `ai_upstream` detail.
const BODY_DETAIL_CHARS: usize = 500;
/// Bytes of an error body read at most.
const ERROR_BODY_BYTES: usize = 64 * 1024;
/// Longest SSE line; a longer one is treated as a broken stream.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Highest tool call index accepted in one answer.
const MAX_TOOL_CALLS: usize = 64;
/// `anthropic-version` header of every Anthropic request.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens`; this leaves room for a long report.
const ANTHROPIC_MAX_TOKENS: u32 = 8192;

/// The wire format of the AI server; the `provider` value in the settings JSON.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    OpenAi,
    Anthropic,
}

impl<'de> Deserialize<'de> for Provider {
    /// Settings files saved before this was an enum hold `ollama`, `custom` and other labels;
    /// every label but `anthropic` means an OpenAI-compatible server.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = String::deserialize(deserializer)?;
        Ok(if label.trim() == "anthropic" {
            Self::Anthropic
        } else {
            Self::OpenAi
        })
    }
}

impl Provider {
    /// Model-id suffixes this provider may add to a name the user wrote. Ollama lists `llama3`
    /// as `llama3:latest` and accepts either name; Anthropic lists exact ids.
    pub fn model_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::OpenAi => &[":latest"],
            Self::Anthropic => &[],
        }
    }
}

/// One tool call of a model answer. `arguments` is the raw JSON text the model sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

/// What `chat_stream` reports while the answer streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent<'a> {
    Text(&'a str),
    /// The name of a tool call, once per call, before its arguments finish streaming.
    ToolStarting(&'a str),
}

pub struct AiClient {
    http: reqwest::Client,
    provider: Provider,
    base: String,
    key: Option<String>,
    idle: Duration,
}

impl AiClient {
    /// `base_url`: the API root without a trailing `/`. `key`: sent as a bearer token
    /// (OpenAI) or as `x-api-key` (Anthropic) when present.
    pub fn new(provider: Provider, base_url: &str, key: Option<String>) -> Result<Self, AppError> {
        // reqwest is built without a bundled crypto provider; use ring, as sqlx and the updater
        // do. An error only means a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| {
                AppError::internal("Could not start the HTTP client").with_detail(chain(&e))
            })?;
        Ok(Self {
            http,
            provider,
            base: base_url.trim_end_matches('/').to_string(),
            key: key.filter(|k| !k.is_empty()),
            idle: DEFAULT_IDLE_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub fn with_idle_timeout(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Model ids from `GET {base}/models`.
    pub async fn list_models(&self) -> Result<Vec<String>, AppError> {
        #[derive(Deserialize)]
        struct ModelList {
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }
        let work = async {
            // Anthropic pages the list at 20 by default.
            let url = match self.provider {
                Provider::OpenAi => format!("{}/models", self.base),
                Provider::Anthropic => format!("{}/models?limit=1000", self.base),
            };
            let req = self.auth(self.http.get(url));
            let resp = req.send().await.map_err(|e| self.send_error(e))?;
            let resp = self.check_status(resp).await?;
            let bytes = resp.bytes().await.map_err(|e| self.send_error(e))?;
            let list: ModelList = serde_json::from_slice(&bytes).map_err(|e| {
                AppError::new(
                    ErrorCode::AiProtocol,
                    "The AI server sent an unexpected model list",
                )
                .with_detail(e.to_string())
            })?;
            Ok(list.data.into_iter().map(|m| m.id).collect())
        };
        let res = match tokio::time::timeout(MODELS_TIMEOUT, work).await {
            Ok(r) => r,
            Err(_) => Err(timeout_error(MODELS_TIMEOUT)),
        };
        res.map_err(|e| self.redact(e))
    }

    /// Streams the answer to `body`, an OpenAI chat body that asks for `stream: true`.
    /// `on_event` gets each text delta and each tool call name; returning false stops the
    /// stream as a cancel does.
    pub async fn chat_stream(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        on_event: impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<Completion, AppError> {
        self.stream_inner(body, cancel, on_event)
            .await
            .map_err(|e| self.redact(e))
    }

    async fn stream_inner(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        mut on_event: impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<Completion, AppError> {
        let (path, body) = match self.provider {
            Provider::OpenAi => ("chat/completions", serde_json::to_vec(body)?),
            Provider::Anthropic => ("messages", serde_json::to_vec(&anthropic_body(body))?),
        };
        let req = self
            .auth(self.http.post(format!("{}/{path}", self.base)))
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let send = async {
            let resp = req.send().await.map_err(|e| self.send_error(e))?;
            self.check_status(resp).await
        };
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AppError::stopped()),
            r = tokio::time::timeout(self.idle, send) => r.map_err(|_| timeout_error(self.idle))??,
        };
        let mut stream = resp.bytes_stream();
        let mut lines = LineBuffer::default();
        let mut acc = Accumulator {
            provider: self.provider,
            ..Default::default()
        };
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::stopped()),
                n = tokio::time::timeout(self.idle, stream.next()) => n,
            };
            let chunk = match next {
                Err(_) => return Err(timeout_error(self.idle)),
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return Err(AppError::new(
                        ErrorCode::AiProtocol,
                        "The AI server stream broke off",
                    )
                    .with_detail(chain(&e.without_url())));
                }
                Ok(Some(Ok(b))) => b,
            };
            for line in lines.push(&chunk)? {
                if acc.line(&line, &mut on_event)? {
                    return acc.finish(&mut on_event);
                }
            }
        }
        if let Some(line) = lines.rest()?
            && acc.line(&line, &mut on_event)?
        {
            return acc.finish(&mut on_event);
        }
        // Some servers close without `[DONE]`; a finish reason still marks a whole answer.
        if acc.out.finish_reason.is_some() {
            return acc.finish(&mut on_event);
        }
        Err(AppError::new(
            ErrorCode::AiProtocol,
            "The AI server stream ended before the answer was complete",
        ))
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (self.provider, &self.key) {
            (Provider::OpenAi, Some(k)) => req.header(AUTHORIZATION, format!("Bearer {k}")),
            (Provider::OpenAi, None) => req,
            (Provider::Anthropic, Some(k)) => req
                .header("x-api-key", k)
                .header("anthropic-version", ANTHROPIC_VERSION),
            (Provider::Anthropic, None) => req.header("anthropic-version", ANTHROPIC_VERSION),
        }
    }

    fn send_error(&self, e: reqwest::Error) -> AppError {
        let e = e.without_url();
        if e.is_timeout() {
            return AppError::new(ErrorCode::Timeout, "The AI server did not answer in time")
                .with_detail(chain(&e));
        }
        let message = if e.is_connect() {
            format!("Cannot reach the AI server at {}", self.base)
        } else {
            "The connection to the AI server failed".to_string()
        };
        AppError::new(ErrorCode::ConnectFailed, message).with_detail(chain(&e))
    }

    async fn check_status(&self, resp: reqwest::Response) -> Result<reqwest::Response, AppError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(AppError::new(
                ErrorCode::AuthFailed,
                "The AI server refused the API key",
            ));
        }
        let body =
            match tokio::time::timeout(self.idle.min(MODELS_TIMEOUT), read_capped(resp)).await {
                Ok(body) => body,
                // The server refused the request but did not send its explanation in time; saying
                // so is different from a server that sent no body at all.
                Err(_) => {
                    return Err(AppError::new(
                        ErrorCode::AiUpstream,
                        format!("The AI server answered HTTP {}", status.as_u16()),
                    )
                    .with_detail("The error details did not arrive in time"));
                }
            };
        let err = AppError::new(
            ErrorCode::AiUpstream,
            format!("The AI server answered HTTP {}", status.as_u16()),
        );
        let body = body.trim();
        Err(if body.is_empty() {
            err
        } else {
            err.with_detail(body)
        })
    }

    /// Replaces the key in the message and detail, for servers that echo it in an error, then
    /// cuts a server text detail to `BODY_DETAIL_CHARS`. The cut comes last so that it can
    /// never leave a part of the key.
    fn redact(&self, mut e: AppError) -> AppError {
        if let Some(k) = &self.key {
            e.message = e.message.replace(k.as_str(), "***");
            e.detail = e.detail.map(|d| d.replace(k.as_str(), "***"));
        }
        if e.code == ErrorCode::AiUpstream {
            e.detail = e
                .detail
                .map(|d| d.chars().take(BODY_DETAIL_CHARS).collect());
        }
        e
    }
}

async fn read_capped(resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(Ok(b)) = stream.next().await {
        out.extend_from_slice(&b);
        if out.len() >= ERROR_BODY_BYTES {
            out.truncate(ERROR_BODY_BYTES);
            break;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn timeout_error(limit: Duration) -> AppError {
    AppError::new(
        ErrorCode::Timeout,
        format!(
            "The AI server sent nothing for {} seconds",
            limit.as_secs_f64()
        ),
    )
}

fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

fn protocol(message: &str, detail: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::AiProtocol, message).with_detail(detail)
}

/// Splits the byte stream into lines; a line can span chunks.
#[derive(Default)]
struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, AppError> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            out.push(utf8(&line[..line.len() - 1])?);
        }
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(protocol(
                "The AI server sent a malformed stream",
                "line too long",
            ));
        }
        Ok(out)
    }

    /// The last line when the stream ended without a newline.
    fn rest(&mut self) -> Result<Option<String>, AppError> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        let line = std::mem::take(&mut self.buf);
        utf8(&line).map(Some)
    }
}

fn utf8(line: &[u8]) -> Result<String, AppError> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8(line.to_vec())
        .map_err(|e| protocol("The AI server sent a malformed stream", e.to_string()))
}

#[derive(Default)]
struct Accumulator {
    provider: Provider,
    out: Completion,
    /// Anthropic: (content block index, tool call position) of each `tool_use` block.
    tool_blocks: Vec<(u64, usize)>,
    /// Per tool call position: its name went out as `StreamEvent::ToolStarting`.
    announced: Vec<bool>,
}

impl Accumulator {
    /// Handles one line. `Ok(true)` at `[DONE]` (OpenAI) or `message_stop` (Anthropic).
    fn line(
        &mut self,
        line: &str,
        on_event: &mut impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<bool, AppError> {
        // Other SSE fields (`event:`, `id:`, `retry:`) and `:` comments carry nothing we use.
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(false);
        };
        let data = data.trim();
        if data.is_empty() {
            return Ok(false);
        }
        if data == "[DONE]" {
            return Ok(true);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| protocol("The AI server sent a malformed stream chunk", e.to_string()))?;
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| err.to_string());
            return Err(AppError::new(
                ErrorCode::AiUpstream,
                "The AI server reported an error during the answer",
            )
            .with_detail(msg));
        }
        if self.provider == Provider::Anthropic {
            return self.anthropic_event(&v, on_event);
        }
        let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            // Usage-only and keep-alive chunks have no choices.
            return Ok(false);
        };
        let delta = choice.get("delta");
        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str)
            && !text.is_empty()
        {
            self.out.text.push_str(text);
            if !on_event(StreamEvent::Text(text)) {
                return Err(AppError::stopped());
            }
        }
        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for (pos, call) in calls.iter().enumerate() {
                self.tool_call(pos, call, on_event)?;
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.out.finish_reason = Some(reason.to_string());
        }
        Ok(false)
    }

    /// Merges one tool call fragment. Fragments of one call share its `index`. A server that
    /// omits `index` sends each call whole with its `id`, so a new `id` starts a new call.
    /// The name is announced with the first argument fragment, since a few servers split it
    /// over fragments before the arguments start.
    fn tool_call(
        &mut self,
        pos: usize,
        call: &Value,
        on_event: &mut impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<(), AppError> {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let calls = &mut self.out.tool_calls;
        let index = match (call.get("index").and_then(Value::as_u64), id) {
            (Some(i), _) => i as usize,
            (None, Some(id)) => match calls.iter().position(|c| c.id == id) {
                Some(i) => i,
                None if calls.last().is_some_and(|c| !c.id.is_empty()) => calls.len(),
                None => pos,
            },
            (None, None) => pos,
        };
        if index >= MAX_TOOL_CALLS {
            return Err(protocol(
                "The AI server sent a malformed tool call",
                format!("tool call index {index}"),
            ));
        }
        if calls.len() <= index {
            calls.resize_with(index + 1, ToolCall::default);
        }
        let slot = &mut calls[index];
        if let Some(id) = id {
            slot.id = id.to_string();
        }
        let func = call.get("function");
        if let Some(name) = func.and_then(|f| f.get("name")).and_then(Value::as_str)
            && slot.name != name
        {
            // Most servers send the name once; a few repeat it whole or split it.
            if slot.name.is_empty() || !name.starts_with(slot.name.as_str()) {
                slot.name.push_str(name);
            } else {
                slot.name = name.to_string();
            }
        }
        let args = func
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        slot.arguments.push_str(args);
        if !args.is_empty() {
            self.announce(index, on_event)?;
        }
        Ok(())
    }

    /// Sends the name of the tool call at `pos` once, when it has one.
    fn announce(
        &mut self,
        pos: usize,
        on_event: &mut impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<(), AppError> {
        let name = self.out.tool_calls[pos].name.as_str();
        if name.is_empty() || self.announced.get(pos).copied().unwrap_or(false) {
            return Ok(());
        }
        if self.announced.len() <= pos {
            self.announced.resize(pos + 1, false);
        }
        self.announced[pos] = true;
        if !on_event(StreamEvent::ToolStarting(name)) {
            return Err(AppError::stopped());
        }
        Ok(())
    }

    /// Handles one Anthropic stream event. `Ok(true)` at `message_stop`.
    fn anthropic_event(
        &mut self,
        v: &Value,
        on_event: &mut impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<bool, AppError> {
        let index = v.get("index").and_then(Value::as_u64);
        let text = match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "content_block_start" => {
                let block = &v["content_block"];
                if block["type"] == "tool_use" {
                    let Some(index) = index else {
                        return Err(protocol(
                            "The AI server sent a malformed tool call",
                            "tool_use block without index",
                        ));
                    };
                    let calls = &mut self.out.tool_calls;
                    if calls.len() >= MAX_TOOL_CALLS {
                        return Err(protocol(
                            "The AI server sent a malformed tool call",
                            format!("more than {MAX_TOOL_CALLS} tool calls"),
                        ));
                    }
                    let pos = calls.len();
                    self.tool_blocks.push((index, pos));
                    calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                        arguments: String::new(),
                    });
                    // Anthropic sends the whole name here, before any input.
                    self.announce(pos, on_event)?;
                    None
                } else {
                    block.get("text").and_then(Value::as_str)
                }
            }
            "content_block_delta" => {
                let delta = &v["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => delta.get("text").and_then(Value::as_str),
                    "input_json_delta" => {
                        let slot = self
                            .tool_blocks
                            .iter()
                            .find(|(i, _)| Some(*i) == index)
                            .map(|&(_, pos)| pos);
                        let Some(pos) = slot else {
                            return Err(protocol(
                                "The AI server sent a malformed tool call",
                                "input_json_delta for an unknown block",
                            ));
                        };
                        if let Some(part) = delta.get("partial_json").and_then(Value::as_str) {
                            self.out.tool_calls[pos].arguments.push_str(part);
                        }
                        None
                    }
                    // Thinking and signature deltas are not part of the answer.
                    _ => None,
                }
            }
            "message_delta" => {
                if let Some(reason) = v["delta"].get("stop_reason").and_then(Value::as_str) {
                    self.out.finish_reason = Some(reason.to_string());
                }
                None
            }
            "message_stop" => return Ok(true),
            // message_start, content_block_stop, ping.
            _ => None,
        };
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.out.text.push_str(text);
            if !on_event(StreamEvent::Text(text)) {
                return Err(AppError::stopped());
            }
        }
        Ok(false)
    }

    /// The whole answer. Names not yet announced (a call without argument fragments) go out
    /// here, before the caller sees the tool calls.
    fn finish(
        mut self,
        on_event: &mut impl FnMut(StreamEvent<'_>) -> bool,
    ) -> Result<Completion, AppError> {
        for (i, call) in self.out.tool_calls.iter_mut().enumerate() {
            if call.name.is_empty() {
                return Err(protocol(
                    "The AI server sent a malformed tool call",
                    format!("tool call {i} has no name"),
                ));
            }
            if call.id.is_empty() {
                call.id = format!("call_{i}");
            }
            // Anthropic streams no input JSON for a tool without arguments.
            if self.provider == Provider::Anthropic && call.arguments.trim().is_empty() {
                call.arguments = "{}".into();
            }
        }
        for pos in 0..self.out.tool_calls.len() {
            self.announce(pos, on_event)?;
        }
        Ok(self.out)
    }
}

/// Translates an OpenAI chat body to an Anthropic Messages body. System messages become
/// `system`; assistant tool calls become `tool_use` blocks; tool messages become `tool_result`
/// blocks. Turns of the same role merge, because Anthropic wants user and assistant to alternate.
fn anthropic_body(body: &Value) -> Value {
    let mut system: Vec<&str> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in body["messages"].as_array().into_iter().flatten() {
        let text = m["content"].as_str().unwrap_or("");
        let (role, blocks) = match m["role"].as_str().unwrap_or("") {
            "system" => {
                if !text.is_empty() {
                    system.push(text);
                }
                continue;
            }
            "tool" => (
                "user",
                vec![
                    json!({"type": "tool_result", "tool_use_id": m["tool_call_id"], "content": text}),
                ],
            ),
            "assistant" => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    let input = c["function"]["arguments"]
                        .as_str()
                        .and_then(|a| serde_json::from_str::<Value>(a).ok())
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    blocks.push(json!({"type": "tool_use", "id": c["id"], "name": c["function"]["name"], "input": input}));
                }
                ("assistant", blocks)
            }
            _ if text.is_empty() => continue,
            _ => ("user", vec![json!({"type": "text", "text": text})]),
        };
        if blocks.is_empty() {
            continue;
        }
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(content) = last["content"].as_array_mut() {
                    content.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let f = &t["function"];
            json!({"name": f["name"], "description": f["description"], "input_schema": f["parameters"]})
        })
        .collect();
    let mut out = json!({
        "model": body["model"],
        "max_tokens": ANTHROPIC_MAX_TOKENS,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        out["system"] = json!(system.join("\n\n"));
    }
    if !tools.is_empty() {
        out["tools"] = json!(tools);
        out["tool_choice"] = json!({"type": "auto"});
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ai::test_support::{
        Part, done, finish_chunk, head, serve, serve_raw, sse, text_chunk,
    };
    use serde_json::json;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    fn body() -> Value {
        json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": "hi"}]})
    }

    /// A stream event as text: the delta itself, or `tool_starting:<name>`.
    fn label(e: StreamEvent<'_>) -> String {
        match e {
            StreamEvent::Text(d) => d.to_string(),
            StreamEvent::ToolStarting(name) => format!("tool_starting:{name}"),
        }
    }

    async fn run(base: &str, key: Option<&str>) -> (Result<Completion, AppError>, Vec<String>) {
        let client = AiClient::new(Provider::OpenAi, base, key.map(String::from))
            .unwrap()
            .with_idle_timeout(Duration::from_millis(500));
        let mut events = Vec::new();
        let res = client
            .chat_stream(&body(), &CancellationToken::new(), |e| {
                events.push(label(e));
                true
            })
            .await;
        (res, events)
    }

    #[tokio::test]
    async fn streams_text_across_split_chunks() {
        let whole = format!(
            ": keep-alive\n\nevent: message\r\n{}{}{}{}",
            text_chunk("Hel").1.replace("\n\n", "\r\n\r\n"),
            text_chunk("lo").1,
            finish_chunk("stop").1,
            done().1,
        );
        // Cut the bytes at odd places so that lines span TCP writes.
        let cut: Vec<Part> = whole
            .as_bytes()
            .chunks(7)
            .map(|c| (1, String::from_utf8(c.to_vec()).unwrap()))
            .collect();
        let mut script = vec![head("200 OK", "text/event-stream")];
        script.extend(cut);
        let (base, mut reqs) = serve(script).await;
        let (res, deltas) = run(&base, Some("sk-test")).await;
        let c = res.unwrap();
        assert_eq!(c.text, "Hello");
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
        assert!(c.tool_calls.is_empty());
        assert_eq!(deltas.concat(), "Hello");
        let req = reqs.recv().await.unwrap();
        assert!(req.starts_with("POST /v1/chat/completions "), "{req}");
        assert!(req.contains("authorization: Bearer sk-test"), "{req}");
        assert!(req.contains(r#""stream":true"#), "{req}");
    }

    #[tokio::test]
    async fn anthropic_translates_the_body_and_assembles_text_and_tool_calls() {
        let ev = |v: Value| {
            (
                0,
                format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()),
            )
        };
        let script = vec![
            head("200 OK", "text/event-stream"),
            ev(json!({"type": "message_start", "message": {"id": "msg_1"}})),
            ev(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Look"}}),
            ),
            ev(json!({"type": "ping"})),
            ev(
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "run_read_query", "input": {}}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"sql\": \"SEL"}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "ECT 1\"}"}}),
            ),
            ev(
                json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "list_tables", "input": {}}}),
            ),
            ev(json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}})),
            ev(json!({"type": "message_stop"})),
        ];
        let (base, mut reqs) = serve(script).await;
        let client = AiClient::new(Provider::Anthropic, &base, Some("sk-ant".into())).unwrap();
        // A second step: the earlier tool call and its result must reach Anthropic as blocks.
        let body = json!({
            "model": "claude", "stream": true, "tool_choice": "auto",
            "tools": [{"type": "function", "function": {"name": "list_tables", "description": "d", "parameters": {"type": "object"}}}],
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "a", "type": "function", "function": {"name": "list_tables", "arguments": ""}},
                    {"id": "b", "type": "function", "function": {"name": "list_tables", "arguments": "{\"schema\":\"x\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "a", "content": "r1"},
                {"role": "tool", "tool_call_id": "b", "content": "r2"},
            ],
        });
        let mut events = Vec::new();
        let c = client
            .chat_stream(&body, &CancellationToken::new(), |e| {
                events.push(label(e));
                true
            })
            .await
            .unwrap();
        assert_eq!(
            events,
            [
                "Look",
                "tool_starting:run_read_query",
                "tool_starting:list_tables"
            ]
        );
        assert_eq!(c.text, "Look");
        assert_eq!(c.finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            c.tool_calls,
            vec![
                ToolCall {
                    id: "toolu_1".into(),
                    name: "run_read_query".into(),
                    arguments: r#"{"sql": "SELECT 1"}"#.into()
                },
                ToolCall {
                    id: "toolu_2".into(),
                    name: "list_tables".into(),
                    arguments: "{}".into()
                },
            ]
        );
        let req = reqs.recv().await.unwrap();
        assert!(req.starts_with("POST /v1/messages "), "{req}");
        assert!(req.contains("x-api-key: sk-ant"), "{req}");
        assert!(req.contains("anthropic-version: 2023-06-01"), "{req}");
        assert!(!req.to_ascii_lowercase().contains("authorization"), "{req}");
        let sent: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(
            sent,
            json!({
                "model": "claude", "max_tokens": ANTHROPIC_MAX_TOKENS, "stream": true, "system": "sys",
                "tools": [{"name": "list_tables", "description": "d", "input_schema": {"type": "object"}}],
                "tool_choice": {"type": "auto"},
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "a", "name": "list_tables", "input": {}},
                        {"type": "tool_use", "id": "b", "name": "list_tables", "input": {"schema": "x"}},
                    ]},
                    // Both results in one user turn: Anthropic wants the roles to alternate.
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "a", "content": "r1"},
                        {"type": "tool_result", "tool_use_id": "b", "content": "r2"},
                    ]},
                ],
            })
        );
    }

    #[tokio::test]
    async fn assembles_tool_call_fragments_by_index() {
        let call = |index: u64, id: Option<&str>, name: Option<&str>, args: &str| {
            let mut f = json!({"arguments": args});
            if let Some(n) = name {
                f["name"] = json!(n);
            }
            let mut c = json!({"index": index, "function": f});
            if let Some(i) = id {
                c["id"] = json!(i);
                c["type"] = json!("function");
            }
            sse(json!({"choices": [{"index": 0, "delta": {"tool_calls": [c]}}]}))
        };
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("Let me check. "),
            call(0, Some("call_a"), Some("run_read_query"), ""),
            call(0, None, None, "{\"sql\": \"SELECT"),
            call(1, Some("call_b"), Some("list_tables"), "{"),
            call(0, None, None, " 1\"}"),
            call(1, None, None, "}"),
            finish_chunk("tool_calls"),
            done(),
        ];
        let (base, _reqs) = serve(script).await;
        let (res, events) = run(&base, None).await;
        let c = res.unwrap();
        // Each name once, though call 0 came in three fragments.
        assert_eq!(
            events,
            [
                "Let me check. ",
                "tool_starting:run_read_query",
                "tool_starting:list_tables"
            ]
        );
        assert_eq!(c.text, "Let me check. ");
        assert_eq!(c.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            c.tool_calls,
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "run_read_query".into(),
                    arguments: "{\"sql\": \"SELECT 1\"}".into(),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "list_tables".into(),
                    arguments: "{}".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn tool_name_is_announced_before_the_arguments_finish() {
        let anthropic = |v: Value| {
            (
                0,
                format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()),
            )
        };
        let rest = |api: Provider| match api {
            Provider::OpenAi => vec![
                sse(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                    {"index": 0, "id": "call_a", "type": "function", "function": {"name": "run_read_query", "arguments": ""}}
                ]}}]})),
                sse(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "{\"sql\": "}}
                ]}}]})),
                (3_000, done().1),
            ],
            Provider::Anthropic => vec![
                anthropic(
                    json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "run_read_query", "input": {}}}),
                ),
                anthropic(
                    json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"sql\": "}}),
                ),
                (3_000, anthropic(json!({"type": "message_stop"})).1),
            ],
        };
        for api in [Provider::OpenAi, Provider::Anthropic] {
            let mut script = vec![head("200 OK", "text/event-stream")];
            script.extend(rest(api));
            let (base, _r) = serve(script).await;
            let client = AiClient::new(api, &base, None).unwrap();
            let started = std::time::Instant::now();
            let mut events = Vec::new();
            // Stopping at the name proves it came while the arguments were still streaming.
            let e = client
                .chat_stream(&body(), &CancellationToken::new(), |e| {
                    events.push(label(e));
                    false
                })
                .await
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::Cancelled, "{api:?}");
            assert_eq!(events, ["tool_starting:run_read_query"], "{api:?}");
            assert!(started.elapsed() < Duration::from_secs(2), "{api:?}");
        }
    }

    #[tokio::test]
    async fn tool_call_without_argument_fragments_is_announced_at_the_end() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_a", "type": "function", "function": {"name": "list_tables", "arguments": ""}}
            ]}}]})),
            finish_chunk("tool_calls"),
            done(),
        ];
        let (base, _r) = serve(script).await;
        let (res, events) = run(&base, None).await;
        assert_eq!(res.unwrap().tool_calls[0].name, "list_tables");
        assert_eq!(events, ["tool_starting:list_tables"]);
    }

    #[tokio::test]
    async fn missing_tool_call_id_gets_one() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"function": {"name": "list_tables", "arguments": "{}"}}
            ]}, "finish_reason": "tool_calls"}]})),
        ];
        let (base, _r) = serve(script).await;
        let c = run(&base, None).await.0.unwrap();
        assert_eq!(c.tool_calls[0].id, "call_0");
        assert_eq!(c.tool_calls[0].name, "list_tables");
    }

    #[tokio::test]
    async fn unindexed_tool_calls_split_on_a_new_id() {
        let call = |id: &str, name: &str, args: &str| {
            sse(json!({"choices": [{"delta": {"tool_calls": [
                {"id": id, "type": "function", "function": {"name": name, "arguments": args}}
            ]}}]}))
        };
        let script = vec![
            head("200 OK", "text/event-stream"),
            call("call_a", "list_tables", "{}"),
            call("call_b", "describe_table", "{\"table\": \"orders\"}"),
            finish_chunk("tool_calls"),
            done(),
        ];
        let (base, _r) = serve(script).await;
        let c = run(&base, None).await.0.unwrap();
        assert_eq!(
            c.tool_calls,
            vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "list_tables".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "describe_table".into(),
                    arguments: "{\"table\": \"orders\"}".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn http_500_is_ai_upstream_with_short_redacted_body() {
        let body = format!("{{\"error\": \"boom for sk-secret\"}}{}", "x".repeat(2000));
        let script = vec![
            head("500 Internal Server Error", "application/json"),
            (0, body),
        ];
        let (base, _r) = serve(script).await;
        let e = run(&base, Some("sk-secret")).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::AiUpstream);
        assert_eq!(e.message, "The AI server answered HTTP 500");
        let detail = e.detail.unwrap();
        assert!(
            detail.starts_with("{\"error\": \"boom for ***\"}"),
            "{detail}"
        );
        assert_eq!(detail.chars().count(), BODY_DETAIL_CHARS);
        assert!(!detail.contains("sk-secret"));
    }

    #[tokio::test]
    async fn http_401_and_403_are_auth_failed() {
        for status in ["401 Unauthorized", "403 Forbidden"] {
            let script = vec![
                head(status, "application/json"),
                (0, "{\"error\": \"bad key sk-secret\"}".into()),
            ];
            let (base, _r) = serve(script).await;
            let e = run(&base, Some("sk-secret")).await.0.unwrap_err();
            assert_eq!(e.code, ErrorCode::AuthFailed, "{status}");
            assert_eq!(e.message, "The AI server refused the API key");
            assert!(e.detail.is_none());
        }
    }

    #[tokio::test]
    async fn malformed_chunk_is_ai_protocol() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("ok"),
            (0, "data: {\"choices\": [\n\n".into()),
            done(),
        ];
        let (base, _r) = serve(script).await;
        let e = run(&base, None).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::AiProtocol);
        assert_eq!(e.message, "The AI server sent a malformed stream chunk");
    }

    #[tokio::test]
    async fn error_object_in_stream_is_ai_upstream() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            sse(json!({"error": {"message": "context too long"}})),
        ];
        let (base, _r) = serve(script).await;
        let e = run(&base, None).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::AiUpstream);
        assert_eq!(e.detail.as_deref(), Some("context too long"));
    }

    #[tokio::test]
    async fn early_end_is_ai_protocol() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("half an ans"),
        ];
        let (base, _r) = serve(script).await;
        let (res, deltas) = run(&base, None).await;
        let e = res.unwrap_err();
        assert_eq!(e.code, ErrorCode::AiProtocol);
        assert_eq!(
            e.message,
            "The AI server stream ended before the answer was complete"
        );
        assert_eq!(deltas, vec!["half an ans"]);
    }

    #[tokio::test]
    async fn finish_without_done_is_complete() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("hi"),
            finish_chunk("stop"),
        ];
        let (base, _r) = serve(script).await;
        assert_eq!(run(&base, None).await.0.unwrap().text, "hi");
    }

    #[tokio::test]
    async fn idle_stream_times_out() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("first"),
            (3_000, text_chunk("late").1),
        ];
        let (base, _r) = serve(script).await;
        let started = std::time::Instant::now();
        let e = run(&base, None).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn no_response_headers_times_out() {
        let (base, _r) = serve(vec![(3_000, String::new())]).await;
        let e = run(&base, None).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
    }

    #[tokio::test]
    async fn cancel_stops_the_stream() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("first"),
            (3_000, done().1),
        ];
        let (base, _r) = serve(script).await;
        let client = AiClient::new(Provider::OpenAi, &base, None).unwrap();
        let token = CancellationToken::new();
        let t = token.clone();
        let started = std::time::Instant::now();
        let e = client
            .chat_stream(&body(), &token, move |_| {
                t.cancel();
                true
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn receiver_gone_stops_like_cancel() {
        let script = vec![
            head("200 OK", "text/event-stream"),
            text_chunk("a"),
            text_chunk("b"),
            done(),
        ];
        let (base, _r) = serve(script).await;
        let client = AiClient::new(Provider::OpenAi, &base, None).unwrap();
        let mut n = 0;
        let e = client
            .chat_stream(&body(), &CancellationToken::new(), |_| {
                n += 1;
                false
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Cancelled);
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn refused_connection_is_connect_failed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let e = run(&format!("http://{addr}/v1"), None).await.0.unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectFailed);
        assert!(
            e.message
                .starts_with("Cannot reach the AI server at http://")
        );
    }

    #[tokio::test]
    async fn https_client_builds_without_a_provider_panic() {
        // A plain-text answer to the TLS hello fails the handshake; there must be no panic on a
        // missing crypto provider before that.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
                let _ = sock.shutdown().await;
            }
        });
        let client = AiClient::new(Provider::OpenAi, &format!("https://{addr}/v1"), None).unwrap();
        let e = client.list_models().await.unwrap_err();
        assert_eq!(e.code, ErrorCode::ConnectFailed, "{e:?}");
    }

    #[tokio::test]
    async fn lists_models() {
        let script = vec![
            head("200 OK", "application/json"),
            (
                0,
                json!({"object": "list", "data": [{"id": "a"}, {"id": "b", "owned_by": "x"}]})
                    .to_string(),
            ),
        ];
        let (base, mut reqs) = serve_raw(script).await;
        let client =
            AiClient::new(Provider::OpenAi, &format!("{base}/"), Some("k1".into())).unwrap();
        assert_eq!(client.list_models().await.unwrap(), vec!["a", "b"]);
        let req = reqs.recv().await.unwrap();
        assert!(req.starts_with("GET /v1/models "), "{req}");
        assert!(req.contains("authorization: Bearer k1"), "{req}");
    }

    #[tokio::test]
    async fn bad_model_list_is_ai_protocol() {
        let script = vec![head("200 OK", "application/json"), (0, "[1, 2]".into())];
        let (base, _r) = serve_raw(script).await;
        let e = AiClient::new(Provider::OpenAi, &base, None)
            .unwrap()
            .list_models()
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::AiProtocol);
    }

    #[tokio::test]
    async fn settings_test_checks_the_model() {
        use crate::core::ai::settings::{self, AiSettings};
        use crate::core::secrets::MemoryStore;
        use crate::core::state::AppState;

        let script = vec![
            head("200 OK", "application/json"),
            (0, json!({"data": [{"id": "mock-gpt"}]}).to_string()),
        ];
        let (base, mut reqs) = serve_raw(script).await;
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        settings::set_key(&st, Some("stored-key".into())).unwrap();
        let s = |model: &str| AiSettings {
            base_url: base.clone(),
            model: model.into(),
            ..Default::default()
        };

        let ok = settings::test(&st, s("mock-gpt"), None).await.unwrap();
        assert_eq!(ok.model_count, 1);
        assert!(reqs.recv().await.unwrap().contains("Bearer stored-key"));

        let e = settings::test(&st, s("wrong-model"), Some(String::new()))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::ModelNotFound);
        assert_eq!(e.detail.as_deref(), Some("mock-gpt"));
        assert!(!reqs.recv().await.unwrap().contains("authorization"));

        settings::test(&st, s("mock-gpt"), Some("typed".into()))
            .await
            .unwrap();
        assert!(reqs.recv().await.unwrap().contains("Bearer typed"));
    }
}
