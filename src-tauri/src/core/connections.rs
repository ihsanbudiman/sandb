//! Saved connections: the wire model, the field defaults of each engine and the target database
//! of a request. Storage and secrets live in `catalog`, the engine in `engine`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::engine::Engine;
use crate::core::error::AppError;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Production,
    Staging,
    #[default]
    Local,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

/// The kind of MongoDB deployment, from the last successful test. Only a replica set or a
/// sharded cluster has transactions.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    Standalone,
    ReplicaSet,
    Sharded,
}

/// A saved connection. The password is never part of it: it lives in the secret store only.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    /// Server version from the last successful test, e.g. `PostgreSQL 16.4`. Empty if never tested.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    /// `main` for SQLite.
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub ssl: SslMode,
    /// SQLite database file; empty for other engines.
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub ai_write: bool,
    /// Blocks every write on this connection, user queries included. Missing in files saved
    /// before phase 3b, so `false`.
    #[serde(default)]
    pub read_only: bool,
    /// MongoDB connection string WITHOUT the password; empty when the fields are used. The
    /// password of the string lives in the secret store, like every other password.
    #[serde(default)]
    pub uri: String,
    /// MongoDB `authSource` for the fields; empty is the driver default.
    #[serde(default)]
    pub auth_source: String,
    /// MongoDB only, from the last successful test.
    #[serde(default)]
    pub topology: Option<Topology>,
}

impl Connection {
    /// The one rule for "may this saved connection write": AI writes, grid saves, exports and
    /// the editor pre-check ask this instead of reading `read_only` themselves. The session
    /// barrier in `db::pool` reads the setting directly: it installs the barrier, not decides
    /// an operation.
    pub fn may_write(&self) -> bool {
        !self.read_only
    }
}

/// The one `ConnectionReadOnly` refusal, for both engines. `reason` names what the operation
/// would have done and is shown after the colon.
pub fn read_only_refusal(conn: &Connection, reason: Option<&str>) -> AppError {
    let message = format!("Connection \"{}\" is read-only", conn.name);
    AppError::new(
        crate::core::error::ErrorCode::ConnectionReadOnly,
        match reason {
            Some(r) => format!("{message}: {r}"),
            None => message,
        },
    )
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInput {
    pub name: String,
    pub engine: Engine,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub env: Env,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub host: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub port: u16,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub database: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub user: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub ssl: SslMode,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub file: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub ai_write: bool,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub read_only: bool,
    /// MongoDB connection string as typed, password included: `catalog::normalize` takes the
    /// password out.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub uri: String,
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub auth_source: String,
    /// Topology reported by a test run in the dialog. `None` keeps the stored one.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub topology: Option<Topology>,
    /// Version reported by a test run in the dialog. `None` keeps the stored one.
    #[serde(default)]
    #[cfg_attr(test, ts(optional = nullable))]
    pub version: Option<String>,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub server_version: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub topology: Option<Topology>,
}

/// The saved connection an input becomes, keeping what only a test can learn (version, topology).
pub(crate) fn build(
    id: String,
    i: ConnectionInput,
    prev_version: &str,
    prev_topology: Option<Topology>,
) -> Connection {
    Connection {
        id,
        name: i.name,
        engine: i.engine,
        version: i.version.unwrap_or_else(|| prev_version.to_string()),
        env: i.env,
        host: i.host,
        port: i.port,
        database: i.database,
        user: i.user,
        ssl: i.ssl,
        file: i.file,
        ai_write: i.ai_write,
        read_only: i.read_only,
        topology: match i.engine {
            Engine::Mongo => i.topology.or(prev_topology),
            _ => None,
        },
        uri: i.uri,
        auth_source: i.auth_source,
    }
}

/// The dialog's copy of a saved connection.
pub(crate) fn input_of(c: &Connection) -> ConnectionInput {
    ConnectionInput {
        name: c.name.clone(),
        engine: c.engine,
        env: c.env,
        host: c.host.clone(),
        port: c.port,
        database: c.database.clone(),
        user: c.user.clone(),
        ssl: c.ssl,
        file: c.file.clone(),
        ai_write: c.ai_write,
        read_only: c.read_only,
        version: Some(c.version.clone()),
        uri: c.uri.clone(),
        auth_source: c.auth_source.clone(),
        topology: c.topology,
    }
}

/// The database a request targets: the given one, else the connection's own. Always `main` on SQLite.
pub fn target_database(conn: &Connection, database: Option<&str>) -> String {
    match (conn.engine, database.map(str::trim)) {
        (Engine::Sqlite, _) => "main".into(),
        (_, Some(d)) if !d.is_empty() => d.to_string(),
        _ => conn.database.clone(),
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Whether a connection test or a duplicate can reuse the saved password: nothing that would
/// send it to another server changed.
pub(crate) fn same_target(src: &Connection, target: &ConnectionInput) -> bool {
    // `uri` is saved without the password, so a changed host list or SRV host shows here.
    src.engine == target.engine
        && src.host == target.host
        && src.port == target.port
        && src.user == target.user
        && src.file == target.file
        && src.uri == target.uri
}

/// A required field of a network engine, so the dialog can say which one is missing.
pub(crate) fn require(value: &str, message: &str) -> Result<(), AppError> {
    if value.is_empty() {
        Err(AppError::invalid(message))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(engine: Engine, database: &str) -> Connection {
        Connection {
            id: "c1".into(),
            name: "x".into(),
            engine,
            version: String::new(),
            env: Env::Local,
            host: String::new(),
            port: 0,
            database: database.into(),
            user: String::new(),
            ssl: SslMode::Prefer,
            file: String::new(),
            ai_write: false,
            read_only: false,
            uri: String::new(),
            auth_source: String::new(),
            topology: None,
        }
    }

    #[test]
    fn target_database_prefers_the_request() {
        let c = conn(Engine::Pg, "shop");
        assert_eq!(target_database(&c, Some("other")), "other");
        assert_eq!(target_database(&c, Some("  ")), "shop");
        assert_eq!(target_database(&c, None), "shop");
        assert_eq!(target_database(&conn(Engine::Sqlite, "shop"), Some("x")), "main");
    }

    #[test]
    fn expand_home_only_touches_tilde_prefix() {
        assert_eq!(expand_home("/a/b.db"), PathBuf::from("/a/b.db"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/x.db"), PathBuf::from(home).join("x.db"));
        }
    }

    #[test]
    fn build_keeps_the_tested_version_and_topology() {
        let mut i = input_of(&conn(Engine::Mongo, ""));
        i.name = "new".into();
        i.version = None;
        let prev = Topology::ReplicaSet;
        let c = build("id".into(), i, "MongoDB 8.0", Some(prev));
        assert_eq!(c.version, "MongoDB 8.0");
        assert_eq!(c.topology, Some(prev));
        let mut i = input_of(&conn(Engine::Pg, ""));
        i.version = None;
        let c = build("id".into(), i, "PostgreSQL 16", Some(prev));
        assert_eq!(c.version, "PostgreSQL 16");
        assert_eq!(c.topology, None);
    }

    #[test]
    fn connection_json_shape_is_camel_case() {
        let v = serde_json::to_value(conn(Engine::Pg, "shop")).unwrap();
        for key in ["authSource", "readOnly", "aiWrite", "topology"] {
            assert!(v.get(key).is_some(), "{key} missing from {v}");
        }
    }
}
