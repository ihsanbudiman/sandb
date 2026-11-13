//! Backend core. No Tauri types here: the app, the dev bridge and the tests share it.

pub mod ai;
pub mod cache;
pub mod catalog;
pub mod cell;
pub mod conn_ro;
pub mod connections;
pub mod db;
pub mod engine;
pub mod error;
pub mod export_file;
pub mod guard;
pub mod mongo;
pub mod ops;
pub mod query_registry;
pub mod result;
pub mod secrets;
pub mod sql;
pub mod state;
pub mod store;
