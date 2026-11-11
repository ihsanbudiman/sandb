//! Backend core. No Tauri types here: the app, the dev bridge and the tests share it.

pub mod ai;
pub mod conn_ro;
pub mod connections;
pub mod db;
pub mod error;
pub mod guard;
pub mod mongo;
pub mod secrets;
pub mod sql;
pub mod state;
pub mod store;
