//! AI HTTP client: Chat Completions, Anthropic Messages, and OAuth Responses.
//! The caller builds an OpenAI chat body for API-key transports or a Responses body for ChatGPT.
//! Every error leaves out the API key, also when the server echoes it back.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::core::ai::{oauth::TokenLease, responses};
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
pub(super) const MAX_TOOL_CALLS: usize = 64;
/// `anthropic-version` header of every Anthropic request.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Anthropic requires `max_tokens`; this leaves room for a long report.
const ANTHROPIC_MAX_TOKENS: u32 = 8192;

/// The wire format of the AI server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Api {
    #[default]
    OpenAi,
    Anthropic,
    ChatGpt,
}

/// One tool call of a model answer. `arguments` is the raw JSON text the model sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A whole streamed answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    /// Backend replay only: retains encrypted reasoning and assistant phase without UI events.
    pub output: Vec<Value>,
}

pub struct AiClient {
    http: reqwest::Client,
    api: Api,
    base: String,
    key: Option<String>,
    idle: Duration,
    lease: Option<TokenLease>,
    redactions: Vec<String>,
}

impl AiClient {
    /// `base_url`: the API root without a trailing `/`. `key`: sent as a bearer token
    /// (OpenAI) or as `x-api-key` (Anthropic) when present.
    pub fn new(api: Api, base_url: &str, key: Option<String>) -> Result<Self, AppError> {
        // reqwest is built without a bundled crypto provider; use ring, as sqlx and the updater
        // do. An error only means a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
        if api == Api::ChatGpt {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }
        let http = builder.build().map_err(|e| {
            AppError::internal("Could not start the HTTP client").with_detail(chain(&e))
        })?;
        Ok(Self {
            http,
            api,
            base: if api == Api::ChatGpt {
                responses::BASE.to_owned()
            } else {
                base_url.trim_end_matches('/').to_string()
            },
            key: if api == Api::ChatGpt {
                None
            } else {
                key.filter(|k| !k.is_empty())
            },
            idle: DEFAULT_IDLE_TIMEOUT,
            lease: None,
            redactions: Vec::new(),
        })
    }

    pub fn chatgpt(lease: TokenLease) -> Result<Self, AppError> {
        let mut client = Self::new(Api::ChatGpt, responses::BASE, None)?;
        client.lease = Some(lease);
        client.check_session()?;
        Ok(client)
    }

    pub(crate) fn with_redaction(mut self, secret: Option<String>) -> Self {
        self.redactions.extend(secret.filter(|s| !s.is_empty()));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_base(mut self, base: &str) -> Self {
        self.base = base.to_owned();
        self
    }

    pub(crate) fn check_session(&self) -> Result<(), AppError> {
        if self.api == Api::ChatGpt {
            self.lease
                .as_ref()
                .ok_or_else(|| AppError::new(ErrorCode::AuthFailed, "Sign in to ChatGPT first"))?
                .token()?;
        }
        Ok(())
    }

    pub(crate) async fn session_cancelled(&self) {
        match &self.lease {
            Some(lease) => lease.cancellation().cancelled().await,
            None => std::future::pending().await,
        }
    }

    /// Replaces the wait limit for the response headers and for each stream chunk.
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
        if self.api == Api::ChatGpt {
            return Ok(self
                .chatgpt_models()
                .await?
                .into_iter()
                .map(|model| model.slug)
                .collect());
        }
        let list: ModelList = self.model_list().await?;
        Ok(list.data.into_iter().map(|m| m.id).collect())
    }

    pub async fn chatgpt_models(&self) -> Result<Vec<responses::Model>, AppError> {
        self.check_session()?;
        let list: responses::ModelList = self.model_list().await?;
        Ok(list
            .models
            .into_iter()
            .filter(|model| model.visibility == "list")
            .collect())
    }

    async fn model_list<T: serde::de::DeserializeOwned>(&self) -> Result<T, AppError> {
        let work = async {
            let url = match self.api {
                Api::OpenAi | Api::ChatGpt => format!("{}/models", self.base),
                Api::Anthropic => format!("{}/models?limit=1000", self.base),
            };
            let req = self.auth(self.http.get(url))?;
            let resp = req.send().await.map_err(|e| self.send_error(e))?;
            let mut resp = self.check_status(resp).await?;
            let mut bytes = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| self.send_error(e))? {
                if bytes.len() + chunk.len() > MAX_LINE_BYTES {
                    return Err(protocol(
                        "The AI server sent an oversized model list",
                        "model catalog is too large",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|e| {
                AppError::new(
                    ErrorCode::AiProtocol,
                    "The AI server sent an unexpected model list",
                )
                .with_detail(e.to_string())
            })
        };
        let res = tokio::select! {
            biased;
            _ = self.session_cancelled() => Err(cancelled()),
            result = tokio::time::timeout(MODELS_TIMEOUT, work) => match result {
                Ok(result) => result,
                Err(_) => Err(timeout_error(MODELS_TIMEOUT)),
            },
        };
        res.map_err(|e| self.redact(e))
    }

    /// Streams the answer to `body`, an OpenAI chat body that asks for `stream: true`.
    /// ChatGPT instead takes a Responses body. `on_text` false stops the stream like a cancel.
    pub async fn chat_stream(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        on_text: impl FnMut(&str) -> bool,
    ) -> Result<Completion, AppError> {
        self.stream_inner(body, cancel, on_text)
            .await
            .map_err(|e| self.redact(e))
    }

    async fn stream_inner(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        mut on_text: impl FnMut(&str) -> bool,
    ) -> Result<Completion, AppError> {
        let (path, body) = match self.api {
            Api::OpenAi => ("chat/completions", serde_json::to_vec(body)?),
            Api::Anthropic => ("messages", serde_json::to_vec(&anthropic_body(body))?),
            Api::ChatGpt => ("responses", serde_json::to_vec(body)?),
        };
        let req = self
            .auth(self.http.post(format!("{}/{path}", self.base)))?
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let send = async {
            let resp = req.send().await.map_err(|e| self.send_error(e))?;
            self.check_status(resp).await
        };
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            _ = self.session_cancelled() => return Err(cancelled()),
            r = tokio::time::timeout(self.idle, send) => r.map_err(|_| timeout_error(self.idle))??,
        };
        let mut lines = LineBuffer::default();
        let mut acc = Accumulator {
            api: self.api,
            http_status: Some(resp.status().as_u16()),
            request_id: resp
                .headers()
                .get("x-request-id")
                .and_then(|id| id.to_str().ok())
                .map(str::to_owned),
            ..Default::default()
        };
        let mut stream = resp.bytes_stream();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(cancelled()),
                _ = self.session_cancelled() => return Err(cancelled()),
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
                if cancel.is_cancelled() {
                    return Err(cancelled());
                }
                self.check_session()?;
                if acc.line(&line, &mut on_text)? {
                    return acc.finish();
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        self.check_session()?;
        if let Some(line) = lines.rest()?
            && acc.line(&line, &mut on_text)?
        {
            return acc.finish();
        }
        // Some servers close without `[DONE]`; a finish reason still marks a whole answer.
        if acc.out.finish_reason.is_some() {
            return acc.finish();
        }
        Err(AppError::new(
            ErrorCode::AiProtocol,
            "The AI server stream ended before the answer was complete",
        ))
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, AppError> {
        self.check_session()?;
        Ok(match (self.api, &self.key) {
            (Api::ChatGpt, _) => {
                req.bearer_auth(self.lease.as_ref().expect("checked lease").token()?)
            }
            (Api::OpenAi, Some(k)) => req.header(AUTHORIZATION, format!("Bearer {k}")),
            (Api::OpenAi, None) => req,
            (Api::Anthropic, Some(k)) => req
                .header("x-api-key", k)
                .header("anthropic-version", ANTHROPIC_VERSION),
            (Api::Anthropic, None) => req.header("anthropic-version", ANTHROPIC_VERSION),
        })
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
        if self.api == Api::ChatGpt {
            let request_id = resp
                .headers()
                .get("x-request-id")
                .and_then(|id| id.to_str().ok())
                .map(str::to_owned);
            let body = tokio::time::timeout(self.idle.min(MODELS_TIMEOUT), read_capped(resp))
                .await
                .unwrap_or_default();
            let body = serde_json::from_str(&body).unwrap_or(Value::String(body));
            let mut error = responses::upstream(&body);
            error.http_status = Some(status.as_u16());
            error.request_id = request_id;
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                error.code = ErrorCode::AuthFailed;
            }
            return Err(error);
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(AppError::new(
                ErrorCode::AuthFailed,
                if self.api == Api::ChatGpt {
                    "ChatGPT refused authorization or plan usage"
                } else {
                    "The AI server refused the API key"
                },
            ));
        }
        let body = tokio::time::timeout(self.idle.min(MODELS_TIMEOUT), read_capped(resp))
            .await
            .unwrap_or_default();
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
            e.redact(k);
        }
        if let Some(lease) = &self.lease {
            e = lease.redact(e);
        }
        for secret in &self.redactions {
            e.redact(secret);
        }
        if e.code == ErrorCode::AiUpstream {
            e.detail = e
                .detail
                .map(|d| d.chars().take(BODY_DETAIL_CHARS).collect());
        }
        e
    }
}

/// The start of an error body, at most `ERROR_BODY_BYTES`.
async fn read_capped(resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(Ok(b)) = stream.next().await {
        // Truncating inside a reflected credential could expose a prefix that redaction cannot match.
        if out.len() + b.len() > ERROR_BODY_BYTES {
            return "[AI error body exceeded 64 KiB]".into();
        }
        out.extend_from_slice(&b);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cancelled() -> AppError {
    AppError::new(ErrorCode::Cancelled, "Stopped")
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

/// The error and its sources, joined with `: `.
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
            if pos > MAX_LINE_BYTES {
                return Err(protocol(
                    "The AI server sent a malformed stream",
                    "line too long",
                ));
            }
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

/// Builds the answer from SSE lines.
#[derive(Default)]
struct Accumulator {
    api: Api,
    out: Completion,
    /// Anthropic: (content block index, tool call position) of each `tool_use` block.
    tool_blocks: Vec<(u64, usize)>,
    response_data: String,
    response_bytes: usize,
    responses: responses::Stream,
    http_status: Option<u16>,
    request_id: Option<String>,
}

impl Accumulator {
    fn line(
        &mut self,
        line: &str,
        on_text: &mut impl FnMut(&str) -> bool,
    ) -> Result<bool, AppError> {
        self.line_inner(line, on_text).map_err(|mut error| {
            if self.api == Api::ChatGpt {
                error.http_status = self.http_status;
                error.request_id = self.request_id.clone();
            }
            error
        })
    }

    /// Handles one line. `Ok(true)` only at the transport's successful terminal event.
    fn line_inner(
        &mut self,
        line: &str,
        on_text: &mut impl FnMut(&str) -> bool,
    ) -> Result<bool, AppError> {
        if self.api == Api::ChatGpt {
            if line.is_empty() {
                if self.response_data.is_empty() {
                    return Ok(false);
                }
                let data = std::mem::take(&mut self.response_data);
                let event: Value = serde_json::from_str(&data).map_err(|_| {
                    protocol(
                        "ChatGPT sent a malformed stream event",
                        "invalid event JSON",
                    )
                })?;
                return self.responses.event(&mut self.out, &event, on_text);
            }
            if let Some(data) = line.strip_prefix("data:") {
                self.response_bytes += data.len() + 1;
                if self.response_bytes > 32 * 1024 * 1024 {
                    return Err(protocol(
                        "ChatGPT response exceeded the stream limit",
                        "response too long",
                    ));
                }
                if self.response_data.len() + data.len() + 1 > MAX_LINE_BYTES {
                    return Err(protocol(
                        "ChatGPT sent an oversized stream event",
                        "event too long",
                    ));
                }
                self.response_data
                    .push_str(data.strip_prefix(' ').unwrap_or(data));
                self.response_data.push('\n');
            }
            return Ok(false);
        }
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
        if self.api == Api::Anthropic {
            return self.anthropic_event(&v, on_text);
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
            if !on_text(text) {
                return Err(cancelled());
            }
        }
        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for (pos, call) in calls.iter().enumerate() {
                self.tool_call(pos, call)?;
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.out.finish_reason = Some(reason.to_string());
        }
        Ok(false)
    }

    /// Merges one tool call fragment. Fragments of one call share its `index`. A server that
    /// omits `index` sends each call whole with its `id`, so a new `id` starts a new call.
    fn tool_call(&mut self, pos: usize, call: &Value) -> Result<(), AppError> {
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
        if let Some(args) = func
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
        {
            slot.arguments.push_str(args);
        }
        Ok(())
    }

    /// Handles one Anthropic stream event. `Ok(true)` at `message_stop`.
    fn anthropic_event(
        &mut self,
        v: &Value,
        on_text: &mut impl FnMut(&str) -> bool,
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
                    self.tool_blocks.push((index, calls.len()));
                    calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                        arguments: String::new(),
                    });
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
            if !on_text(text) {
                return Err(cancelled());
            }
        }
        Ok(false)
    }

    fn finish(self) -> Result<Completion, AppError> {
        let mut out = self.out;
        for (i, call) in out.tool_calls.iter_mut().enumerate() {
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
            if self.api == Api::Anthropic && call.arguments.trim().is_empty() {
                call.arguments = "{}".into();
            }
        }
        Ok(out)
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
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One part of a scripted response: wait `ms`, then write `bytes`.
    pub(crate) type Part = (u64, String);

    /// A tiny HTTP server on a free port. Every connection gets `script`, then the socket
    /// closes. The handle yields the raw requests received.
    pub(crate) async fn serve(
        script: Vec<Part>,
    ) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let script = script.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let req = read_request(&mut sock).await;
                    let _ = tx.send(req);
                    for (ms, bytes) in script {
                        if ms > 0 {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                        }
                        if sock.write_all(bytes.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = sock.flush().await;
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}/v1"), rx)
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(end) = text.find("\r\n\r\n") {
                let len = text[..end]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    pub(crate) fn head(status: &str, content_type: &str) -> Part {
        (
            0,
            format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n"
            ),
        )
    }

    pub(crate) fn sse(v: Value) -> Part {
        (0, format!("data: {v}\n\n"))
    }

    pub(crate) fn text_chunk(s: &str) -> Part {
        sse(json!({"choices": [{"index": 0, "delta": {"content": s}, "finish_reason": null}]}))
    }

    pub(crate) fn finish_chunk(reason: &str) -> Part {
        sse(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}))
    }

    pub(crate) fn done() -> Part {
        (0, "data: [DONE]\n\n".into())
    }

    fn body() -> Value {
        json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": "hi"}]})
    }

    async fn run(base: &str, key: Option<&str>) -> (Result<Completion, AppError>, Vec<String>) {
        let client = AiClient::new(Api::OpenAi, base, key.map(String::from))
            .unwrap()
            .with_idle_timeout(Duration::from_millis(500));
        let mut deltas = Vec::new();
        let res = client
            .chat_stream(&body(), &CancellationToken::new(), |d| {
                deltas.push(d.to_string());
                true
            })
            .await;
        (res, deltas)
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
        let client = AiClient::new(Api::Anthropic, &base, Some("sk-ant".into())).unwrap();
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
        let c = client
            .chat_stream(&body, &CancellationToken::new(), |_| true)
            .await
            .unwrap();
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
        let c = run(&base, None).await.0.unwrap();
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
        let client = AiClient::new(Api::OpenAi, &base, None).unwrap();
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
        let client = AiClient::new(Api::OpenAi, &base, None).unwrap();
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
        // Windows refusal may outlast the streaming test helper's 500 ms idle limit.
        let client = AiClient::new(Api::OpenAi, &format!("http://{addr}/v1"), None).unwrap();
        let e = client
            .chat_stream(&body(), &CancellationToken::new(), |_| true)
            .await
            .unwrap_err();
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
        let client = AiClient::new(Api::OpenAi, &format!("https://{addr}/v1"), None).unwrap();
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
        let (base, mut reqs) = serve(script).await;
        let client = AiClient::new(Api::OpenAi, &format!("{base}/"), Some("k1".into())).unwrap();
        assert_eq!(client.list_models().await.unwrap(), vec!["a", "b"]);
        let req = reqs.recv().await.unwrap();
        assert!(req.starts_with("GET /v1/models "), "{req}");
        assert!(req.contains("authorization: Bearer k1"), "{req}");
    }

    #[tokio::test]
    async fn bad_model_list_is_ai_protocol() {
        let script = vec![head("200 OK", "application/json"), (0, "[1, 2]".into())];
        let (base, _r) = serve(script).await;
        let e = AiClient::new(Api::OpenAi, &base, None)
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
        let (base, mut reqs) = serve(script).await;
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
