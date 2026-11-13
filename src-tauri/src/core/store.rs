//! JSON files in the data dir: `connections.json` and `settings.json`. The settings file holds
//! opaque sections: a module owns its key and its type and asks the store for that section only.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::core::connections::Connection;
use crate::core::error::AppError;

pub const CONNECTIONS_FILE: &str = "connections.json";
pub const SETTINGS_FILE: &str = "settings.json";

#[derive(Serialize, serde::Deserialize)]
struct ConnectionsFile<C> {
    version: u32,
    connections: Vec<C>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Entry<'a> {
    Known(&'a Connection),
    Unknown(&'a serde_json::Value),
}

pub struct Store {
    dir: PathBuf,
    /// Entries of `connections.json` that this version cannot read, e.g. an engine that a newer
    /// version added. They stay out of the list, and each save writes them back unchanged, so an
    /// older version neither fails to start nor deletes them.
    unknown: Mutex<Vec<serde_json::Value>>,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, AppError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            unknown: Mutex::default(),
        })
    }

    pub fn load_connections(&self) -> Result<Vec<Connection>, AppError> {
        let Some(file) =
            read_json::<ConnectionsFile<serde_json::Value>>(&self.dir.join(CONNECTIONS_FILE))?
        else {
            return Ok(Vec::new());
        };
        let mut known = Vec::with_capacity(file.connections.len());
        let mut unknown = Vec::new();
        for entry in file.connections {
            match Connection::deserialize(&entry) {
                Ok(c) => known.push(c),
                Err(e) => {
                    eprintln!("sandb: hiding a saved connection this version cannot read: {e}");
                    unknown.push(entry);
                }
            }
        }
        *self.unknown.lock().unwrap_or_else(|e| e.into_inner()) = unknown;
        Ok(known)
    }

    pub fn save_connections(&self, connections: &[Connection]) -> Result<(), AppError> {
        let unknown = self.unknown.lock().unwrap_or_else(|e| e.into_inner());
        let file = ConnectionsFile {
            version: 1,
            connections: connections
                .iter()
                .map(Entry::Known)
                .chain(unknown.iter().map(Entry::Unknown))
                .collect(),
        };
        write_json_atomic(&self.dir.join(CONNECTIONS_FILE), &file)
    }

    /// One section of `settings.json`, by the name the module that owns it chose. A missing
    /// section takes the type's default, so older or partial files still load.
    pub fn load_section<T: DeserializeOwned + Default>(&self, key: &str) -> Result<T, AppError> {
        let file: Map<String, Value> =
            read_json(&self.dir.join(SETTINGS_FILE))?.unwrap_or_default();
        match file.get(key) {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Ok(T::default()),
        }
    }

    /// Replaces one section of `settings.json`; the sections of other modules stay as they are.
    pub fn save_section<T: Serialize>(&self, key: &str, value: &T) -> Result<(), AppError> {
        let mut file: Map<String, Value> =
            read_json(&self.dir.join(SETTINGS_FILE))?.unwrap_or_default();
        file.insert(key.to_string(), serde_json::to_value(value)?);
        write_json_atomic(&self.dir.join(SETTINGS_FILE), &Value::Object(file))
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
    use crate::core::connections::{Env, SslMode};
    use crate::core::engine::Engine;

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

    #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Section {
        model: String,
        max_rows: u32,
    }

    #[test]
    fn missing_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        assert!(s.load_connections().unwrap().is_empty());
        assert_eq!(s.load_section::<Section>("test").unwrap(), Section::default());
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
    fn sections_round_trip_and_keep_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path()).unwrap();
        let section = Section {
            model: "qwen2.5:7b".into(),
            max_rows: 42,
        };
        s.save_section("test", &section).unwrap();
        assert!(dir.path().join(SETTINGS_FILE).exists());
        assert_eq!(s.load_section::<Section>("test").unwrap(), section);
        s.save_section("other", &serde_json::json!({"kept": true})).unwrap();
        s.save_section("test", &section).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(SETTINGS_FILE)).unwrap())
                .unwrap();
        assert_eq!(raw["other"]["kept"], true);
        assert_eq!(raw["test"]["max_rows"], 42);
    }

    /// Files written before phase 4 hold `{}`: a missing section takes its default.
    #[test]
    fn empty_settings_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SETTINGS_FILE), b"{}").unwrap();
        let s = Store::new(dir.path()).unwrap();
        assert_eq!(s.load_section::<Section>("test").unwrap(), Section::default());
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

    /// A newer version can save a connection this version cannot read (0.11.0 aborted at launch
    /// on a `mongo` entry). The app must start with the other connections, and a save must not
    /// delete the unreadable entry, so the newer version still has it after a downgrade.
    #[test]
    fn unreadable_connection_is_hidden_and_kept_on_save() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(CONNECTIONS_FILE),
            r#"{"version": 1, "connections": [
                {"id": "new", "name": "future", "engine": "oracle", "host": "h", "port": 1521, "database": "d", "user": "u"},
                {"id": "a", "name": "old", "engine": "pg", "host": "127.0.0.1", "port": 5432, "database": "shop", "user": "u"}
            ]}"#,
        )
        .unwrap();
        let s = Store::new(dir.path()).unwrap();
        let list = s.load_connections().unwrap();
        let ids: Vec<_> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);

        s.save_connections(&[sample("b")]).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(CONNECTIONS_FILE)).unwrap()).unwrap();
        let saved = raw["connections"].as_array().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0]["id"], "b");
        assert_eq!(saved[1]["engine"], "oracle");
        assert_eq!(saved[1]["port"], 1521);
        let ids: Vec<_> = Store::new(dir.path())
            .unwrap()
            .load_connections()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["b"]);
    }
}
