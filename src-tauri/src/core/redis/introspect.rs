//! Browse the Redis keyspace and inspect individual keys without reading their complete values.

use std::collections::{BTreeSet, HashSet};

use futures_util::future::join_all;
use redis::aio::ConnectionLike;
use redis::{FromRedisValue, Pipeline, RedisError, RedisResult, Value};

use crate::core::connections::{Connection, RedisMode, SslMode};
use crate::core::error::AppError;
use crate::core::result::{
    ColumnInfo, KeyList, RedisKeyInfo, SchemaSnapshot, SnapshotTable, TableInfo, TableKind,
    TableProperties,
};

use super::client::{Access, connection};
use super::conn::{RedisConn, mode, parse_database};
use super::key::{parse_arg, quote_arg};
use super::{FIELD_SAMPLE, KEY_LIST_MAX, SCAN_COUNT};

const CLUSTER_BATCH: usize = 64;
const STREAM_SAMPLE: usize = 10;

type Node = (String, u16);

pub async fn databases(access: &impl Access, conn: &Connection) -> Result<Vec<String>, AppError> {
    if mode(conn) == RedisMode::Cluster {
        return Ok(vec!["0".to_owned()]);
    }
    let mut client = connection(access, &conn.id, &conn.database).await?;
    let info: String = decode(client.query(redis::cmd("INFO").arg("keyspace")).await?)?;
    Ok(parse_keyspace(&info, parse_database(&conn.database)?)
        .into_iter()
        .map(|index| index.to_string())
        .collect())
}

fn parse_keyspace(info: &str, selected: u32) -> BTreeSet<u32> {
    let mut databases = BTreeSet::from([selected]);
    databases.extend(info.lines().filter_map(|line| {
        let (name, stats) = line.split_once(':')?;
        stats.strip_prefix("keys=")?;
        let index = name.strip_prefix("db")?;
        if !index.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        index.parse::<u32>().ok()
    }));
    databases
}

pub async fn keys(
    access: &impl Access,
    conn: &Connection,
    db: &str,
    pattern: &str,
    limit: usize,
) -> Result<KeyList, AppError> {
    let mut client = connection(access, &conn.id, db).await?;
    let names = scan(&mut client, conn.ssl, pattern, limit).await?;
    // A full result is marked capped even if the last page happens to end at the limit.
    let truncated = limit != 0 && names.len() == limit;
    let types = keyed_batch(
        &mut client,
        names.iter().map(|key| ("TYPE", key.as_slice())).collect(),
        |reply| decode::<String>(reply?),
    )
    .await?;
    let sizes = keyed_batch(
        &mut client,
        names
            .iter()
            .zip(&types)
            .filter_map(|(key, kind)| Some((size_command(kind)?, key.as_slice())))
            .collect(),
        |reply| match reply {
            // A key can change type between TYPE and the length command.
            Err(error) if error.code() == Some("WRONGTYPE") => Ok(None),
            reply => decode::<Option<i64>>(reply?),
        },
    )
    .await?;
    let mut sizes = sizes.into_iter();
    let keys = names
        .into_iter()
        .zip(types)
        // SCAN can return a key that expires before TYPE runs.
        .filter(|(_, kind)| kind != "none")
        .map(|(key, kind)| TableInfo {
            name: quote_arg(&key),
            kind: TableKind::Table,
            row_estimate: size_command(&kind).and_then(|_| sizes.next().flatten()),
            has_rowid: None,
            partition_of: None,
            redis_type: Some(kind),
        })
        .collect();
    Ok(KeyList { keys, truncated })
}

pub async fn snapshot(
    access: &impl Access,
    conn: &Connection,
    db: &str,
) -> Result<SchemaSnapshot, AppError> {
    let mut client = connection(access, &conn.id, db).await?;
    let names = scan(&mut client, conn.ssl, "*", KEY_LIST_MAX).await?;
    Ok(SchemaSnapshot {
        database: db.to_owned(),
        truncated: names.len() == KEY_LIST_MAX,
        tables: names
            .into_iter()
            .map(|name| SnapshotTable {
                schema: None,
                name: quote_arg(&name),
                kind: TableKind::Table,
                columns: Vec::new(),
            })
            .collect(),
    })
}

async fn scan(
    client: &mut RedisConn,
    ssl: SslMode,
    pattern: &str,
    limit: usize,
) -> Result<BTreeSet<Vec<u8>>, AppError> {
    let mut keys = BTreeSet::new();
    if limit == 0 {
        return Ok(keys);
    }
    let nodes = if matches!(client, RedisConn::Cluster(_)) {
        primary_nodes(client, ssl).await?
    } else {
        Vec::new()
    };
    let single = matches!(client, RedisConn::Single(_)).then_some(None);
    for node in single.into_iter().chain(nodes.iter().map(Some)) {
        let mut cursor = 0_u64;
        loop {
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(SCAN_COUNT);
            let reply = match node {
                Some((host, port)) => client.query_node(&cmd, host, *port).await?,
                None => client.query(&cmd).await?,
            };
            let (next, page): (u64, Vec<Vec<u8>>) = decode(reply)?;
            for key in page {
                // SCAN can return duplicates; cap the number of unique keys retained.
                if keys.insert(key) && keys.len() == limit {
                    return Ok(keys);
                }
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
    }
    Ok(keys)
}

async fn primary_nodes(client: &mut RedisConn, ssl: SslMode) -> Result<Vec<Node>, AppError> {
    let nodes = match client.query(redis::cmd("CLUSTER").arg("SHARDS")).await {
        Ok(reply) => parse_shards(&reply, ssl == SslMode::Require)?,
        Err(error) if unknown_command(&error) => {
            let info: String = decode(client.query(redis::cmd("CLUSTER").arg("NODES")).await?)?;
            parse_cluster_nodes(&info)
        }
        Err(error) => return Err(error.into()),
    };
    if nodes.is_empty() {
        return Err(AppError::internal(
            "Redis Cluster returned no scannable primaries",
        ));
    }
    Ok(nodes)
}

fn parse_shards(reply: &Value, tls: bool) -> Result<Vec<Node>, AppError> {
    let bad_reply = || AppError::internal("Unexpected CLUSTER SHARDS reply");
    let mut primaries = BTreeSet::new();
    for shard in reply.as_sequence().ok_or_else(bad_reply)? {
        let nodes = map_field(shard, "nodes")
            .and_then(Value::as_sequence)
            .ok_or_else(bad_reply)?;
        for node in nodes {
            if map_field(node, "role").and_then(text) != Some("master")
                || matches!(
                    map_field(node, "health").and_then(text),
                    Some("failed" | "loading")
                )
            {
                continue;
            }
            let port_field = if tls { "tls-port" } else { "port" };
            let port = match map_field(node, port_field) {
                Some(Value::Int(value)) => u16::try_from(*value).ok().filter(|&n| n != 0),
                _ => None,
            };
            // The driver's address routing uses the preferred endpoint from CLUSTER SLOTS.
            // If no endpoint is advertised, use the IP when one is available.
            let host = ["endpoint", "ip"]
                .into_iter()
                .find_map(|field| map_field(node, field).and_then(text).and_then(usable_host));
            match (host, port) {
                (Some(host), Some(port)) => {
                    primaries.insert((host.to_owned(), port));
                }
                _ => {
                    return Err(AppError::internal(
                        "Redis Cluster primary has no usable address",
                    ));
                }
            }
        }
    }
    Ok(primaries.into_iter().collect())
}

fn map_field<'a>(map: &'a Value, field: &str) -> Option<&'a Value> {
    map.as_map_iter()?
        .find_map(|(key, value)| (text(key) == Some(field)).then_some(value))
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::BulkString(bytes) => std::str::from_utf8(bytes).ok(),
        Value::SimpleString(text) => Some(text),
        _ => None,
    }
}

fn parse_cluster_nodes(info: &str) -> Vec<Node> {
    let mut nodes = BTreeSet::new();
    for line in info.lines() {
        let mut columns = line.split_ascii_whitespace().skip(1);
        let (Some(address), Some(flags)) = (columns.next(), columns.next()) else {
            continue;
        };
        let mut primary = false;
        let mut unavailable = false;
        for flag in flags.split(',') {
            primary |= flag == "master";
            unavailable |= matches!(flag, "fail" | "handshake" | "noaddr");
        }
        if !primary || unavailable {
            continue;
        }
        let address = address.split(['@', ',']).next().unwrap_or_default();
        let Some((host, port)) = address.rsplit_once(':') else {
            continue;
        };
        if let Some(host) = usable_host(host)
            && let Ok(port) = port.parse::<u16>()
            && port != 0
        {
            nodes.insert((host.to_owned(), port));
        }
    }
    nodes.into_iter().collect()
}

fn usable_host(host: &str) -> Option<&str> {
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if host.contains(':') {
        let ip = host.parse::<std::net::Ipv6Addr>().ok()?;
        return (!ip.is_unspecified()).then_some(host);
    }
    (!matches!(host, "" | "0.0.0.0")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')))
    .then_some(host)
}

async fn keyed_batch<T>(
    client: &mut RedisConn,
    commands: Vec<(&'static str, &[u8])>,
    decode_reply: impl Fn(RedisResult<Value>) -> Result<T, AppError>,
) -> Result<Vec<T>, AppError> {
    let capacity = commands.len();
    match client {
        RedisConn::Single(conn) => {
            let mut pipe = Pipeline::with_capacity(capacity);
            for (command, key) in commands {
                pipe.cmd(command).arg(key);
            }
            if pipe.is_empty() {
                return Ok(Vec::new());
            }
            // Keep individual Redis errors available for the size decoder.
            conn.req_packed_commands(&pipe, 0, pipe.len())
                .await?
                .into_iter()
                .map(|reply| decode_reply(reply.extract_error()))
                .collect()
        }
        RedisConn::Cluster(cluster) => {
            let mut replies = Vec::with_capacity(capacity);
            for commands in commands.chunks(CLUSTER_BATCH) {
                let batch = commands.iter().map(|&(command, key)| {
                    let mut client = RedisConn::Cluster(cluster.clone());
                    let decode_reply = &decode_reply;
                    async move { decode_reply(client.query(redis::cmd(command).arg(key)).await) }
                });
                for reply in join_all(batch).await {
                    replies.push(reply?);
                }
            }
            Ok(replies)
        }
    }
}

pub(super) fn size_command(kind: &str) -> Option<&'static str> {
    match kind {
        "string" => Some("STRLEN"),
        "hash" => Some("HLEN"),
        "list" => Some("LLEN"),
        "set" => Some("SCARD"),
        "zset" => Some("ZCARD"),
        "stream" => Some("XLEN"),
        _ => None,
    }
}

pub async fn columns(
    access: &impl Access,
    conn: &Connection,
    db: &str,
    key: &str,
) -> Result<Vec<ColumnInfo>, AppError> {
    let bytes = parse_arg(key)?;
    let mut client = connection(access, &conn.id, db).await?;
    let kind = key_type(&mut client, key, &bytes).await?;
    let value = || column("value", "string", false);
    let member = || column("member", "string", true);
    match kind.as_str() {
        "hash" => {
            let reply = client
                .query(
                    redis::cmd("HSCAN")
                        .arg(bytes.as_ref())
                        .arg(0)
                        .arg("COUNT")
                        .arg(FIELD_SAMPLE),
                )
                .await?;
            let (_, fields): (u64, Value) = decode(reply)?;
            let fields = fields
                .into_map_iter()
                .map_err(|_| AppError::internal("Unexpected Redis HSCAN reply"))?
                .take(FIELD_SAMPLE)
                .map(|(field, _)| decode::<Vec<u8>>(field))
                .collect::<Result<BTreeSet<_>, _>>()?;
            let sample_size = fields.len() as u32;
            let mut columns = Vec::with_capacity(fields.len() + 2);
            columns.extend([column("field", "string", true), value()]);
            columns.extend(fields.into_iter().map(|field| ColumnInfo {
                sample_size: Some(sample_size),
                ..column(quote_arg(&field), "hash field", false)
            }));
            Ok(columns)
        }
        "list" => Ok(vec![column("index", "integer", true), value()]),
        "set" => Ok(vec![member()]),
        "zset" => Ok(vec![member(), column("score", "double", false)]),
        "stream" => {
            let reply = client
                .query(
                    redis::cmd("XREVRANGE")
                        .arg(bytes.as_ref())
                        .arg("+")
                        .arg("-")
                        .arg("COUNT")
                        .arg(STREAM_SAMPLE),
                )
                .await?;
            let Value::Array(entries) = reply else {
                return Err(AppError::internal("Unexpected Redis XREVRANGE reply"));
            };
            let mut seen = HashSet::new();
            let mut columns = vec![column("id", "stream id", true)];
            for entry in entries {
                let (_, fields): (Value, Value) = decode(entry)?;
                let fields = fields
                    .into_map_iter()
                    .map_err(|_| AppError::internal("Unexpected Redis XREVRANGE field list"))?;
                for (field, _) in fields {
                    let field: Vec<u8> = decode(field)?;
                    if !seen.contains(field.as_slice()) {
                        columns.push(column(quote_arg(&field), "string", false));
                        seen.insert(field);
                    }
                }
            }
            Ok(columns)
        }
        _ => Ok(vec![value()]),
    }
}

fn column(name: impl Into<String>, data_type: &str, pk: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: data_type.to_owned(),
        nullable: false,
        default: None,
        is_pk: pk,
        pk_ordinal: pk.then_some(1),
        frequency: None,
        sample_size: None,
    }
}

pub async fn properties(
    access: &impl Access,
    conn: &Connection,
    db: &str,
    key: &str,
) -> Result<TableProperties, AppError> {
    let bytes = parse_arg(key)?;
    let bytes = bytes.as_ref();
    let mut client = connection(access, &conn.id, db).await?;
    let key_type = key_type(&mut client, key, bytes).await?;
    let ttl_ms: i64 = decode(client.query(redis::cmd("PTTL").arg(bytes)).await?)?;
    if ttl_ms == -2 {
        return Err(missing_key(key));
    }
    let length = match size_command(&key_type) {
        Some(cmd) => Some(decode(client.query(redis::cmd(cmd).arg(bytes)).await?)?),
        None => None,
    };
    let encoding = optional(
        client
            .query(redis::cmd("OBJECT").arg("ENCODING").arg(bytes))
            .await,
    )?;
    let memory_bytes = optional(
        client
            .query(redis::cmd("MEMORY").arg("USAGE").arg(bytes))
            .await,
    )?;
    Ok(TableProperties {
        redis: Some(RedisKeyInfo {
            key_type,
            encoding,
            ttl_ms: (ttl_ms >= 0).then_some(ttl_ms),
            length,
            memory_bytes,
        }),
        ..TableProperties::default()
    })
}

pub(super) async fn key_type(
    client: &mut RedisConn,
    key: &str,
    bytes: &[u8],
) -> Result<String, AppError> {
    let kind: String = decode(client.query(redis::cmd("TYPE").arg(bytes)).await?)?;
    if kind == "none" {
        return Err(missing_key(key));
    }
    Ok(kind)
}

pub(super) fn missing_key(key: &str) -> AppError {
    AppError::not_found(if key.starts_with('"') {
        format!("Key {key} does not exist")
    } else {
        format!("Key \"{key}\" does not exist")
    })
}

pub(super) fn decode<T: FromRedisValue>(reply: Value) -> Result<T, AppError> {
    Ok(redis::from_redis_value(reply).map_err(RedisError::from)?)
}

fn optional<T: FromRedisValue>(reply: RedisResult<Value>) -> Result<Option<T>, AppError> {
    match reply {
        Err(error) if error.code() == Some("NOPERM") || unknown_command(&error) => Ok(None),
        reply => decode(reply?),
    }
}

fn unknown_command(error: &RedisError) -> bool {
    error.code() == Some("ERR")
        && error.detail().is_some_and(|detail| {
            ["unknown command", "unknown subcommand"]
                .into_iter()
                .any(|prefix| {
                    detail
                        .get(..prefix.len())
                        .is_some_and(|text| text.eq_ignore_ascii_case(prefix))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyspace_deduplicates_and_sorts_database_indices() {
        let info = "# Keyspace\r\n\
                    db10:keys=1,expires=0\r\n\
                    db1:keys=2\r\n\
                    db0:keys=10\r\n\
                    db+1:keys=1\r\n\
                    db4:unrelated=2\r\n\
                    db:keys=1\r\n\
                    db4294967296:keys=1\r\n";
        assert_eq!(parse_keyspace(info, 2), BTreeSet::from([0, 1, 2, 10]));
        assert_eq!(parse_keyspace(info, 10), BTreeSet::from([0, 1, 10]));
        assert_eq!(parse_keyspace("# Keyspace\r\n", 3), BTreeSet::from([3]));
    }

    #[test]
    fn cluster_nodes_selects_unique_reachable_primaries() {
        let info = "a 10.0.0.1:6379@16379,h1 myself,master - 0 0 1 connected 0-5000\n\
                    b 10.0.0.1:6379@16379 master - 0 0 1 connected 0-5000\n\
                    c [::1]:6380@16380 master - 0 0 2 connected 5001-10000\n\
                    d 10.0.0.3:6379@16379 slave a 0 0 3 connected\n\
                    e 10.0.0.4:6379@16379 master,fail - 0 0 4 disconnected\n\
                    f :0@0 master,noaddr - 0 0 5 disconnected\n\
                    g 10.0.0.5:0@0 master - 0 0 6 disconnected\n\
                    invalid entry";
        assert_eq!(
            parse_cluster_nodes(info),
            vec![("10.0.0.1".to_owned(), 6379), ("::1".to_owned(), 6380)]
        );
    }

    #[test]
    fn cluster_shards_uses_primary_endpoint_and_transport_port() {
        let text = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let primary = Value::Map(vec![
            (text("role"), text("master")),
            (text("health"), text("online")),
            (text("endpoint"), text("cache-1.example")),
            (text("ip"), text("10.0.0.1")),
            (text("port"), Value::Int(6379)),
            (text("tls-port"), Value::Int(6380)),
        ]);
        let replica = Value::Map(vec![(text("role"), text("replica"))]);
        let reply = Value::Array(vec![Value::Map(vec![(
            text("nodes"),
            Value::Array(vec![replica, primary]),
        )])]);
        assert_eq!(
            parse_shards(&reply, false).unwrap(),
            vec![("cache-1.example".to_owned(), 6379)]
        );
        assert_eq!(
            parse_shards(&reply, true).unwrap(),
            vec![("cache-1.example".to_owned(), 6380)]
        );
    }

    #[test]
    fn narrow_unknown_command_detection() {
        let kind = redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError);
        let unknown: RedisError = (
            kind,
            "ERR",
            "Unknown subcommand or wrong number of arguments for 'SHARDS'. Try CLUSTER HELP."
                .to_owned(),
        )
            .into();
        let other: RedisError = (
            kind,
            "ERR",
            "This instance has cluster support disabled".to_owned(),
        )
            .into();
        assert!(unknown_command(&unknown));
        assert!(!unknown_command(&other));
    }
}
