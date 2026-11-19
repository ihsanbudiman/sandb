//! Redis engine: connection settings and the RESP3 connections used by Redis operations.
//! An operation takes [`client::Access`], the slice of state it needs, instead of `AppState`.
//! `core::ops` routes operations here; `catalog` and `commands` use [`conn`] for settings and tests.

use std::time::Duration;

pub mod ai;
pub mod client;
pub(crate) mod command;
pub mod conn;
pub mod edit;
pub mod export;
pub mod guard;
pub mod introspect;
mod key;
pub mod parse;
pub mod reply;
pub mod run;
pub mod script;

pub const KEY_LIST_MAX: usize = 1_000;
pub const SCAN_COUNT: usize = 1_000;
pub const FIELD_SAMPLE: usize = 100;
pub const VIEW_MAX_ELEMENTS: usize = 10_000;
pub const SCRIPT_MAX_ELEMENTS: usize = 10_000;
pub const EXPORT_BATCH: usize = 1_000;
pub const KILL_TIMEOUT: Duration = Duration::from_secs(2);
