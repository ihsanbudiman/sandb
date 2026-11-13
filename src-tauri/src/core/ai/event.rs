//! The event schema of a chat stream. One streamed event per `ChatEvent`; every run ends with
//! exactly one `done` or `error`. The tool dispatcher and the chat loop both build these, so the
//! type lives on its own instead of in either module.

use serde::Serialize;
use serde_json::Value;

use crate::core::error::AppError;
use crate::core::result::{ChartSpec, WriteInfo};

/// What a tool call does, for the UI: it labels the step, decides which icon and actions it
/// gets, and says whether its text is a statement. The tool table sends it; the UI never matches
/// tool names.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// `list_tables`: a list of tables or collections.
    List,
    /// `describe_table`: one table's columns, or one collection's sampled fields.
    Describe,
    Read,
    /// A read whose rows the UI draws as a chart.
    Chart,
    /// A proposed write, approved or denied by the user.
    Write,
    /// A tool name this build does not know. The call still streams and fails as `not_found`;
    /// the UI shows it without the actions of a known kind.
    Other,
}

/// The typed part of a tool result, so the UI reads numbers instead of the English summary.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultData {
    /// Rows the tool read, or rows a write changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub rows: Option<u64>,
    /// Time the server took, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub ms: Option<u64>,
    /// A write ran and committed. Never `false`: a write that did not run is an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub committed: Option<bool>,
}

/// One streamed event. JSON: `{"type": "text", "delta": "…"}` and so on.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Text {
        delta: String,
    },
    /// The model has begun a tool call; its arguments are still streaming. Once per call,
    /// before that call's `tool_call`.
    ToolStarting {
        name: String,
        kind: ToolKind,
    },
    ToolCall {
        id: String,
        name: String,
        kind: ToolKind,
        /// The parsed arguments; `{}` when they are not a JSON object.
        args: serde_json::Map<String, Value>,
    },
    ToolResult {
        id: String,
        ok: bool,
        /// Short text for the UI: `12 tables`, `5 rows · 42 ms`, or the error message.
        summary: String,
        /// The same numbers, typed.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        result: Option<ToolResultData>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        columns: Option<Vec<String>>,
        /// At most `tools::UI_ROWS` rows, for the local UI only.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        rows: Option<Vec<Vec<Value>>>,
        /// More rows exist than `rows` holds.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        truncated: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        error: Option<AppError>,
    },
    /// A proposed write, sent before its `tool_result`. Nothing has run: the `tool_result`
    /// comes after the user approves (`approve_write`) or denies (`deny_write`) it.
    WritePreview {
        id: String,
        sql: String,
        info: WriteInfo,
    },
    /// The chart of a `render_chart` read, sent before its `tool_result`. For the local UI only;
    /// at most `tools::CHART_ROWS` rows.
    Chart {
        id: String,
        spec: ChartSpec,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        /// More rows exist than `rows` holds.
        truncated: bool,
    },
    Error {
        error: AppError,
    },
    Done,
}
