//! Export live Redis values as redis-cli commands. This is not a snapshot.

use std::borrow::Cow;
use std::path::Path;

use redis::Value;
use tokio::io::AsyncWriteExt;

use crate::core::connections::{Connection, RedisMode, target_database};
use crate::core::error::AppError;
use crate::core::export_file::ExportFile;
use crate::core::result::{ExportResult, TableExportRequest};

use super::client::{self, Access};
use super::conn::{RedisConn, mode};
use super::introspect::{decode, key_type, missing_key, size_command};
use super::key::{bare_arg, parse_arg, push_quoted_body};
use super::{EXPORT_BATCH, SCRIPT_MAX_ELEMENTS, run};

const PREVIEW_MAX_BYTES: usize = 8 * 1024 * 1024;
const STRING_CHUNK: usize = 64 * 1024;

pub async fn key_script(
    access: &impl Access,
    conn: &Connection,
    database: &str,
    key: &str,
) -> Result<String, AppError> {
    let mut client = open(access, conn, database).await?;
    let key = Key::read(&mut client, key).await?;
    if key.length > SCRIPT_MAX_ELEMENTS {
        return Err(AppError::invalid(format!(
            "The key has {} elements. Use Export script… for big keys.",
            key.length
        )));
    }
    // SCAN can repeat items, so only the byte limit applies after this live size check.
    let mut script = Script::new(Output::Preview(String::new()));
    render(&mut client, &mut script, &key).await?;
    let Output::Preview(text) = script.output else {
        unreachable!("key_script writes a preview")
    };
    Ok(text)
}

pub async fn export_key(
    access: &impl Access,
    conn: &Connection,
    req: TableExportRequest,
    path: &Path,
) -> Result<ExportResult, AppError> {
    let mut client = open(access, conn, &req.database).await?;
    let key = Key::read(&mut client, &req.table).await?;
    let mut file = ExportFile::create(path).await?;
    let rows = render(&mut client, &mut Script::new(Output::File(&mut file)), &key).await?;
    file.finish(rows as u64).await
}

async fn open(
    access: &impl Access,
    conn: &Connection,
    database: &str,
) -> Result<RedisConn, AppError> {
    let database = run::database_index(
        &target_database(conn, Some(database)),
        mode(conn) == RedisMode::Cluster,
    )?;
    client::connection(access, &conn.id, &database).await
}

struct Key<'a> {
    token: &'a str,
    bytes: Cow<'a, [u8]>,
    kind: String,
    length: usize,
}

impl<'a> Key<'a> {
    async fn read(client: &mut RedisConn, token: &'a str) -> Result<Self, AppError> {
        let bytes = parse_arg(token)?;
        let kind = key_type(client, token, &bytes).await?;
        let size = size_command(&kind).ok_or_else(|| {
            AppError::invalid(format!("Script export does not support type {kind}"))
        })?;
        let length = decode(client.query(redis::cmd(size).arg(bytes.as_ref())).await?)?;
        Ok(Self {
            token,
            bytes,
            kind,
            length,
        })
    }

    fn cmd(&self, name: &str) -> redis::Cmd {
        let mut cmd = redis::cmd(name);
        cmd.arg(self.bytes.as_ref());
        cmd
    }
}

enum Output<'a, 'file> {
    Preview(String),
    File(&'a mut ExportFile<'file>),
}

impl Output<'_, '_> {
    fn check_bytes(&self, bytes: usize) -> Result<(), AppError> {
        if let Self::Preview(text) = self
            && bytes > PREVIEW_MAX_BYTES - text.len()
        {
            return Err(AppError::invalid(
                "The script is larger than 8 MiB. Use Export script… for big keys.",
            ));
        }
        Ok(())
    }

    async fn write(&mut self, text: &str) -> Result<(), AppError> {
        self.check_bytes(text.len())?;
        match self {
            Self::Preview(out) => out.push_str(text),
            Self::File(file) => file.out().write_all(text.as_bytes()).await?,
        }
        Ok(())
    }
}

struct Script<'a, 'file> {
    output: Output<'a, 'file>,
    escaped: String,
}

impl<'a, 'file> Script<'a, 'file> {
    fn new(output: Output<'a, 'file>) -> Self {
        Self {
            output,
            escaped: String::new(),
        }
    }

    async fn command(&mut self, name: &str, key: &Key<'_>) -> Result<(), AppError> {
        self.output.write(name).await?;
        self.output.write(" ").await?;
        // parse_arg checked that the key name is already one valid redis-cli token.
        self.output.write(key.token).await
    }

    async fn arg(&mut self, bytes: &[u8]) -> Result<(), AppError> {
        self.output.write(" ").await?;
        self.output.check_bytes(bytes.len())?;
        if let Some(text) = bare_arg(bytes) {
            self.output.write(text).await
        } else {
            self.output.write("\"").await?;
            self.quoted_body(bytes).await?;
            self.output.write("\"").await
        }
    }

    async fn quoted_body(&mut self, bytes: &[u8]) -> Result<(), AppError> {
        for chunk in bytes.chunks(STRING_CHUNK) {
            self.escaped.clear();
            push_quoted_body(&mut self.escaped, chunk);
            self.output.write(&self.escaped).await?;
        }
        Ok(())
    }

    async fn newline(&mut self) -> Result<(), AppError> {
        self.output.write("\n").await
    }
}

async fn render(
    client: &mut RedisConn,
    script: &mut Script<'_, '_>,
    key: &Key<'_>,
) -> Result<usize, AppError> {
    script.output.write("# ").await?;
    script.command(&key.kind, key).await?;
    script.newline().await?;
    script.command("DEL", key).await?;
    script.newline().await?;
    let rows = match key.kind.as_str() {
        "string" => {
            string(client, script, key).await?;
            1
        }
        "hash" => scan(client, script, key, "HSCAN", "HSET").await?,
        "list" => list(client, script, key).await?,
        "set" => scan(client, script, key, "SSCAN", "SADD").await?,
        "zset" => scan(client, script, key, "ZSCAN", "ZADD").await?,
        "stream" => stream(client, script, key).await?,
        _ => unreachable!("Key::read only admits the supported types"),
    };
    // Record the remaining TTL after reading the data. A missing key must not export a DEL.
    let ttl: i64 = decode(client.query(&key.cmd("PTTL")).await?)?;
    match ttl {
        -2 => return Err(missing_key(key.token)),
        -1 => (),
        ttl if ttl >= 0 => {
            script.command("PEXPIRE", key).await?;
            script.arg(ttl.to_string().as_bytes()).await?;
            script.newline().await?;
        }
        _ => return Err(unexpected("PTTL")),
    }
    if rows == 0 && key.kind != "stream" {
        return Err(changed());
    }
    Ok(rows)
}

async fn string(
    client: &mut RedisConn,
    script: &mut Script<'_, '_>,
    key: &Key<'_>,
) -> Result<(), AppError> {
    script.command("SET", key).await?;
    if key.length <= STRING_CHUNK {
        let reply = client
            .query(key.cmd("GETRANGE").arg(0).arg(STRING_CHUNK - 1))
            .await?;
        let bytes = bulk(&reply, "GETRANGE")?;
        if bytes.len() != key.length {
            return Err(changed());
        }
        script.arg(bytes).await?;
    } else {
        // Keep one SET command, but do not read or quote the complete string at once.
        script.output.write(" \"").await?;
        for start in (0..key.length).step_by(STRING_CHUNK) {
            let end = key.length.min(start + STRING_CHUNK);
            let reply = client
                .query(key.cmd("GETRANGE").arg(start).arg(end - 1))
                .await?;
            let bytes = bulk(&reply, "GETRANGE")?;
            if bytes.len() != end - start {
                return Err(changed());
            }
            script.quoted_body(bytes).await?;
        }
        script.output.write("\"").await?;
    }
    script.newline().await
}

// Count values, not SCAN pages. Compact encodings may return all values in one page.
struct Batch<'a> {
    command: &'a str,
    rows: usize,
}

impl Batch<'_> {
    async fn start_item(&self, script: &mut Script<'_, '_>, key: &Key<'_>) -> Result<(), AppError> {
        if self.rows.is_multiple_of(EXPORT_BATCH) {
            script.command(self.command, key).await?;
        }
        Ok(())
    }

    async fn finish_item(&mut self, script: &mut Script<'_, '_>) -> Result<(), AppError> {
        self.rows += 1;
        if self.rows.is_multiple_of(EXPORT_BATCH) {
            script.newline().await?;
        }
        Ok(())
    }

    async fn finish(self, script: &mut Script<'_, '_>) -> Result<usize, AppError> {
        if !self.rows.is_multiple_of(EXPORT_BATCH) {
            script.newline().await?;
        }
        Ok(self.rows)
    }
}

async fn scan(
    client: &mut RedisConn,
    script: &mut Script<'_, '_>,
    key: &Key<'_>,
    command: &str,
    restore: &str,
) -> Result<usize, AppError> {
    let mut batch = Batch {
        command: restore,
        rows: 0,
    };
    let mut cursor = 0u64;
    loop {
        let reply = client
            .query(key.cmd(command).arg(cursor).arg("COUNT").arg(EXPORT_BATCH))
            .await?;
        let (next, items): (u64, Value) = decode(reply)?;
        // SCAN can repeat entries. HSET, SADD and ZADD can replay them without duplicates.
        if command == "SSCAN" {
            for item in sequence(&items, command)? {
                batch.start_item(script, key).await?;
                script.arg(bulk(item, command)?).await?;
                batch.finish_item(script).await?;
            }
        } else {
            for (member, value) in items.as_map_iter().ok_or_else(|| unexpected(command))? {
                batch.start_item(script, key).await?;
                if command == "ZSCAN" {
                    score(script, value).await?;
                    script.arg(bulk(member, command)?).await?;
                } else {
                    script.arg(bulk(member, command)?).await?;
                    script.arg(bulk(value, command)?).await?;
                }
                batch.finish_item(script).await?;
            }
        }
        if next == 0 {
            return batch.finish(script).await;
        }
        cursor = next;
    }
}

async fn score(script: &mut Script<'_, '_>, value: &Value) -> Result<(), AppError> {
    match value {
        Value::Double(n) if !n.is_nan() => script.arg(format!("{n:e}").as_bytes()).await,
        Value::Int(n) => script.arg(n.to_string().as_bytes()).await,
        _ => script.arg(bulk(value, "ZSCAN")?).await,
    }
}

async fn list(
    client: &mut RedisConn,
    script: &mut Script<'_, '_>,
    key: &Key<'_>,
) -> Result<usize, AppError> {
    let mut batch = Batch {
        command: "RPUSH",
        rows: 0,
    };
    for start in (0..key.length).step_by(EXPORT_BATCH) {
        let end = key.length.min(start + EXPORT_BATCH);
        let reply = client
            .query(key.cmd("LRANGE").arg(start).arg(end - 1))
            .await?;
        let items = sequence(&reply, "LRANGE")?;
        for item in items {
            batch.start_item(script, key).await?;
            script.arg(bulk(item, "LRANGE")?).await?;
            batch.finish_item(script).await?;
        }
        if items.len() < end - start {
            break;
        }
    }
    batch.finish(script).await
}

async fn stream(
    client: &mut RedisConn,
    script: &mut Script<'_, '_>,
    key: &Key<'_>,
) -> Result<usize, AppError> {
    let mut start = "-".to_owned();
    let mut rows = 0;
    while rows < key.length {
        let count = EXPORT_BATCH.min(key.length - rows);
        let reply = client
            .query(
                key.cmd("XRANGE")
                    .arg(&start)
                    .arg("+")
                    .arg("COUNT")
                    .arg(count),
            )
            .await?;
        let entries = sequence(&reply, "XRANGE")?;
        let mut last_id = None;
        for entry in entries {
            let [id, fields] = sequence(entry, "XRANGE")? else {
                return Err(unexpected("XRANGE"));
            };
            let id = bulk(id, "XRANGE")?;
            script.command("XADD", key).await?;
            script.arg(id).await?;
            let fields = sequence(fields, "XRANGE")?;
            if fields.is_empty() || !fields.len().is_multiple_of(2) {
                return Err(unexpected("XRANGE"));
            }
            for field in fields {
                script.arg(bulk(field, "XRANGE")?).await?;
            }
            script.newline().await?;
            last_id = Some(id);
            rows += 1;
        }
        let Some(last_id) = last_id.filter(|_| rows < key.length && entries.len() == count) else {
            break;
        };
        // Redis 6.0 has no exclusive XRANGE bound. Increment the last ID without rounding.
        let Some(next) = next_stream_id(last_id)? else {
            break;
        };
        start = next;
    }
    if rows == 0 {
        // XADD always adds an entry. MKSTREAM can create an empty stream without one.
        script.command("XGROUP CREATE", key).await?;
        script
            .output
            .write(" __sandb_export__ 0 MKSTREAM\n")
            .await?;
        script.command("XGROUP DESTROY", key).await?;
        script.output.write(" __sandb_export__\n").await?;
    }
    Ok(rows)
}

fn next_stream_id(id: &[u8]) -> Result<Option<String>, AppError> {
    let parsed = std::str::from_utf8(id).ok().and_then(|id| {
        let (ms, seq) = id.split_once('-')?;
        Some((ms.parse::<u64>().ok()?, seq.parse::<u64>().ok()?))
    });
    let (ms, seq) = parsed.ok_or_else(|| unexpected("XRANGE"))?;
    if let Some(seq) = seq.checked_add(1) {
        Ok(Some(format!("{ms}-{seq}")))
    } else {
        Ok(ms.checked_add(1).map(|ms| format!("{ms}-0")))
    }
}

fn sequence<'a>(value: &'a Value, command: &str) -> Result<&'a [Value], AppError> {
    match value {
        Value::Array(items) | Value::Set(items) => Ok(items),
        _ => Err(unexpected(command)),
    }
}

fn bulk<'a>(value: &'a Value, command: &str) -> Result<&'a [u8], AppError> {
    match value {
        Value::BulkString(bytes) => Ok(bytes),
        Value::SimpleString(text) => Ok(text.as_bytes()),
        _ => Err(unexpected(command)),
    }
}

fn unexpected(command: &str) -> AppError {
    AppError::internal(format!("Unexpected Redis {command} reply"))
}

fn changed() -> AppError {
    AppError::invalid("The key changed during script export. Try again.")
}
