//! JSON files in the data dir: `connections.json` and `settings.json`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::ai::settings::AiSettings;
use crate::core::connections::Connection;
use crate::core::error::AppError;

pub const CONNECTIONS_FILE: &str = "connections.json";
pub const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ConnectionsFile {
    version: u32,
    connections: Vec<Connection>,
}

/// App settings. Missing sections take their default, so older files still load.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub ai: AiSettings,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, AppError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn load_connections(&self) -> Result<Vec<Connection>, AppError> {
        Ok(
            read_json::<ConnectionsFile>(&self.dir.join(CONNECTIONS_FILE))?
                .map(|f| f.connections)
                .unwrap_or_default(),
        )
    }

    pub fn save_connections(&self, connections: &[Connection]) -> Result<(), AppError> {
        let file = ConnectionsFile {
            version: 1,
            connections: connections.to_vec(),
        };
        write_json_atomic(&self.dir.join(CONNECTIONS_FILE), &file)
    }

    pub fn load_settings(&self) -> Result<Settings, AppError> {
        Ok(read_json(&self.dir.join(SETTINGS_FILE))?.unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), AppError> {
        write_json_atomic(&self.dir.join(SETTINGS_FILE), settings)
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, AppError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    Ok(write_atomic(path, &bytes, false)?)
}

/// Write to a temp file in the same dir, fsync, then rename, so a crash never leaves a half-written file.
/// `private`: the file is created readable by the user only (Unix); the rename keeps that mode.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("data");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let res = (|| -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::connections::{Engine, Env, SslMode};

    fn sample(id: &str) -> Connection {
        Connection {
            id: id.into(),
            name: format!("conn {id}"),
            engine: Engine::Pg,
            version: "PostgreSQL 16.4".into(),
            env: Env::Staging,
            host: "127.0.0.1".into(),
            port: 55416,
            database: "shop".into(),
            user: "sandb_ro".into(),
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
    fn missing_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        assert!(s.load_connections().unwrap().is_empty());
        assert_eq!(s.load_settings().unwrap(), Settings::default());
    }

    #[test]
    fn connections_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path().join("nested")).unwrap();
        let list = vec![sample("a"), sample("b")];
        s.save_connections(&list).unwrap();
        assert_eq!(s.load_connections().unwrap(), list);
        // A second Store on the same dir sees the same data (it is really on disk).
        let s2 = Store::new(dir.path().join("nested")).unwrap();
        assert_eq!(s2.load_connections().unwrap(), list);
    }

    #[test]
    fn atomic_write_replaces_file_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        s.save_connections(&[sample("a")]).unwrap();
        s.save_connections(&[sample("b")]).unwrap();
        let ids: Vec<_> = s
            .load_connections()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["b"]);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![CONNECTIONS_FILE.to_string()]);
    }

    #[test]
    fn failed_write_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        s.save_connections(&[sample("a")]).unwrap();
        // Rename onto a directory fails: the original content must survive.
        let target = dir.path().join("blocked.json");
        fs::create_dir(&target).unwrap();
        assert!(write_json_atomic(&target, &serde_json::json!({"x": 1})).is_err());
        assert_eq!(s.load_connections().unwrap()[0].id, "a");
        let tmp_left = fs::read_dir(dir.path())
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!tmp_left, "temp file must be removed after a failed write");
    }

    #[test]
    fn corrupt_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONNECTIONS_FILE), b"{not json").unwrap();
        let s = Store::new(dir.path()).unwrap();
        assert_eq!(
            s.load_connections().unwrap_err().code,
            crate::core::error::ErrorCode::Io
        );
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        let mut settings = Settings::default();
        settings.ai.model = "qwen2.5:7b".into();
        settings.ai.max_rows = 42;
        s.save_settings(&settings).unwrap();
        assert!(dir.path().join(SETTINGS_FILE).exists());
        assert_eq!(s.load_settings().unwrap(), settings);
    }

    /// Files written before phase 4 hold `{}`: they load with the default AI settings.
    #[test]
    fn empty_settings_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SETTINGS_FILE), b"{}").unwrap();
        let s = Store::new(dir.path()).unwrap();
        assert_eq!(s.load_settings().unwrap(), Settings::default());
    }

    /// Files written before phase 3b have no `readOnly`; a missing value means false, so old
    /// connections stay writable instead of silently becoming read-only.
    #[test]
    fn connection_without_read_only_loads_as_writable() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(CONNECTIONS_FILE),
            r#"{"version": 1, "connections": [{"id": "a", "name": "old", "engine": "pg", "host": "127.0.0.1", "port": 5432, "database": "shop", "user": "u"}]}"#,
        )
        .unwrap();
        let s = Store::new(dir.path()).unwrap();
        let list = s.load_connections().unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].read_only);
        // Saved again, the field is written out so the file format is self-describing.
        s.save_connections(&list).unwrap();
        let raw = fs::read_to_string(dir.path().join(CONNECTIONS_FILE)).unwrap();
        assert!(raw.contains(r#""readOnly": false"#), "{raw}");
    }
}
