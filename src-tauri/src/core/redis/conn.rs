//! Connection string parsing, secure RESP3 connections and the probe used by Test connection.

use std::net::Ipv6Addr;

use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::cluster::ClusterClientBuilder;
use redis::cluster_async::ClusterConnection;
use redis::cluster_routing::{RoutingInfo, SingleNodeRoutingInfo};
use redis::sentinel::{SentinelClientBuilder, SentinelServerType};
use redis::{
    AsyncConnectionConfig, Client, Cmd, ConnectionAddr, ErrorKind, IntoConnectionInfo,
    ProtocolVersion, RedisConnectionInfo, RedisError, RedisResult, TlsMode, Value,
};
use serde::Serialize;
use url::Url;

use crate::core::connections::{Connection, RedisMode, SslMode};
use crate::core::engine::{CONNECT_TIMEOUT, Engine};
use crate::core::error::{AppError, ErrorCode, ServerCode};

/// The parts of a Redis connection string that may be sent to the UI or saved.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisUriParts {
    pub redacted: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub ssl: SslMode,
}

pub fn parse_uri(uri: &str) -> Result<(RedisUriParts, Option<String>), AppError> {
    let uri = uri.trim();
    let scheme_error =
        || AppError::invalid("A Redis connection string starts with redis:// or rediss://");
    let (scheme, rest) = uri.split_once("://").ok_or_else(scheme_error)?;
    let ssl = if scheme.eq_ignore_ascii_case("redis") {
        SslMode::Disable
    } else if scheme.eq_ignore_ascii_case("rediss") {
        SslMode::Require
    } else {
        return Err(scheme_error());
    };
    let invalid_uri = || AppError::invalid("Invalid connection string");
    let base = uri.split(['?', '#']).next().unwrap_or_default();
    let address_and_path = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, db) = address_and_path
        .split_once('/')
        .unwrap_or((address_and_path, ""));
    // The URL parser silently removes some whitespace and accepts an empty port.
    let address = authority.rsplit('@').next().unwrap_or_default();
    if address.is_empty() {
        return Err(AppError::invalid("The connection string has no host"));
    }
    if base.contains(|c: char| c.is_whitespace() || c.is_control()) || address.ends_with(':') {
        return Err(invalid_uri());
    }
    let mut url = Url::parse(base).map_err(|_| invalid_uri())?;
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| AppError::invalid("The connection string has no host"))?;
    let port = url.port().unwrap_or_else(|| Engine::Redis.default_port());
    let valid_host = matches!(url.host(), Some(url::Host::Ipv6(_))) || valid_hostname(host);
    if !valid_host || port == 0 {
        return Err(invalid_uri());
    }
    let host = host.to_owned();
    let db = parse_database(db)?;
    // No query or fragment reaches the driver (in particular, no `#insecure`). Its URL
    // decoder also handles escaped user info. Never forward errors that could name the URI.
    let info = url
        .clone()
        .into_connection_info()
        .map_err(|_| invalid_uri())?;
    let user = info
        .redis_settings()
        .username()
        .unwrap_or_default()
        .to_owned();
    let password = info.redis_settings().password().map(str::to_owned);
    url.set_password(None).map_err(|_| invalid_uri())?;
    let parts = RedisUriParts {
        redacted: url.into(),
        host,
        port,
        user,
        database: db.to_string(),
        ssl,
    };
    Ok((parts, password))
}

pub fn parse_database(db: &str) -> Result<u32, AppError> {
    if db.is_empty() {
        return Ok(0);
    }
    if db.bytes().all(|b| b.is_ascii_digit())
        && let Ok(db) = db.parse()
    {
        return Ok(db);
    }
    Err(AppError::invalid(
        "Database must be a number, for example 0",
    ))
}

pub fn mode(c: &Connection) -> RedisMode {
    c.redis.as_ref().map_or(RedisMode::Standalone, |s| s.mode)
}

pub fn nodes(c: &Connection) -> Result<Vec<(String, u16)>, AppError> {
    parse_nodes(&c.host, c.port)
}

/// The host list in field mode; the driver takes IPv6 addresses without brackets.
pub fn parse_nodes(hosts: &str, port: u16) -> Result<Vec<(String, u16)>, AppError> {
    hosts
        .split(',')
        .map(str::trim)
        .map(|entry| {
            parse_node(entry, port).ok_or_else(|| {
                AppError::invalid(format!(
                    "Host list entry \"{entry}\" is not host or host:port"
                ))
            })
        })
        .collect()
}

fn parse_node(entry: &str, port: u16) -> Option<(String, u16)> {
    let (host, port) = if let Some(ipv6) = entry.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']')?;
        host.parse::<Ipv6Addr>().ok()?;
        let port = if suffix.is_empty() {
            port
        } else {
            parse_port(suffix.strip_prefix(':')?)?
        };
        (host, port)
    } else {
        let (host, port) = match entry.split_once(':') {
            Some((host, p)) => (host, parse_port(p)?),
            None => (entry, port),
        };
        if !valid_hostname(host) {
            return None;
        }
        (host, port)
    };
    (port != 0).then(|| (host.to_owned(), port))
}

fn parse_port(port: &str) -> Option<u16> {
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port.parse().ok()
}

fn valid_hostname(host: &str) -> bool {
    host.strip_suffix('.')
        .unwrap_or(host)
        .split('.')
        .all(|label| {
            label.bytes().any(|b| b.is_ascii_alphanumeric())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
}

#[derive(Clone)]
pub enum RedisConn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl RedisConn {
    pub async fn query(&mut self, cmd: &Cmd) -> RedisResult<Value> {
        let value = match self {
            Self::Single(conn) => conn.req_packed_command(cmd).await?,
            Self::Cluster(conn) => conn.req_packed_command(cmd).await?,
        };
        command_reply(value)
    }

    pub async fn query_node(&mut self, cmd: &Cmd, host: &str, port: u16) -> RedisResult<Value> {
        match self {
            Self::Single(_) => self.query(cmd).await,
            Self::Cluster(conn) => command_reply(
                conn.route_command(
                    cmd.clone(),
                    RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress {
                        host: host.to_owned(),
                        port,
                    }),
                )
                .await?,
            ),
        }
    }
}

fn command_reply(value: Value) -> RedisResult<Value> {
    // Only the command itself failed. An array can also contain Redis error values as data.
    match value {
        Value::ServerError(err) => Err(err.into()),
        value => Ok(value),
    }
}

fn address(host: String, port: u16, tls: bool) -> ConnectionAddr {
    if tls {
        ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            tls_params: None,
        }
    } else {
        ConnectionAddr::Tcp(host, port)
    }
}

pub async fn open(c: &Connection, password: Option<&str>, db: i64) -> Result<RedisConn, AppError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::time::timeout(CONNECT_TIMEOUT, connect(c, password, db))
        .await
        .map_err(|_| AppError::connect_timeout())?
}

async fn connect(c: &Connection, password: Option<&str>, db: i64) -> Result<RedisConn, AppError> {
    let tls = c.ssl == SslMode::Require;
    let mut addrs = nodes(c)?
        .into_iter()
        .map(|(host, port)| address(host, port, tls));
    let user = (!c.user.is_empty()).then_some(c.user.as_str());
    let password = password.filter(|p| !p.is_empty());
    let protocol = ProtocolVersion::RESP3;
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(None);

    match mode(c) {
        RedisMode::Standalone => {
            let addr = addrs
                .next()
                .ok_or_else(|| AppError::invalid("Host is required"))?;
            let mut settings = RedisConnectionInfo::default()
                .set_db(db)
                .set_protocol(protocol);
            if let Some(user) = user {
                settings = settings.set_username(user);
            }
            if let Some(password) = password {
                settings = settings.set_password(password);
            }
            let info = addr.into_connection_info()?.set_redis_settings(settings);
            let conn = Client::open(info)?
                .get_multiplexed_async_connection_with_config(&config)
                .await?;
            Ok(RedisConn::Single(conn))
        }
        RedisMode::Cluster => {
            // Leaving the response timeout unset disables it both per node and overall.
            // A dropped reply cannot tell us whether a command ran. Never replay it.
            let mut builder = ClusterClientBuilder::new(addrs)
                .retries(0)
                .connection_timeout(CONNECT_TIMEOUT)
                .use_protocol(protocol);
            if tls {
                builder = builder.tls(TlsMode::Secure);
            }
            if let Some(user) = user {
                builder = builder.username(user);
            }
            if let Some(password) = password {
                builder = builder.password(password);
            }
            let conn = builder.build()?.get_async_connection().await?;
            Ok(RedisConn::Cluster(conn))
        }
        RedisMode::Sentinel => {
            let settings = c
                .redis
                .as_ref()
                .ok_or_else(|| AppError::internal("Missing Redis settings"))?;
            let mut builder = SentinelClientBuilder::new(
                addrs,
                &settings.sentinel_master,
                SentinelServerType::Master,
            )?
            .set_client_to_redis_db(db)
            .set_client_to_redis_protocol(protocol)
            .set_client_to_sentinel_protocol(protocol);
            if tls {
                builder = builder
                    .set_client_to_redis_tls_mode(TlsMode::Secure)
                    .set_client_to_sentinel_tls_mode(TlsMode::Secure);
            }
            if let Some(user) = user {
                builder = builder.set_client_to_redis_username(user);
                if settings.sentinel_auth {
                    builder = builder.set_client_to_sentinel_username(user);
                }
            }
            if let Some(password) = password {
                builder = builder.set_client_to_redis_password(password);
                if settings.sentinel_auth {
                    builder = builder.set_client_to_sentinel_password(password);
                }
            }
            let conn = builder
                .build()?
                .get_async_connection_with_config(&config)
                .await?;
            Ok(RedisConn::Single(conn))
        }
    }
}

pub async fn probe(c: &Connection, password: Option<&str>) -> Result<String, AppError> {
    let db = parse_database(&c.database)?;
    // A nonzero SELECT against a cluster would fail before we could identify the deployment.
    // Test uses a fresh, uncached connection, so selecting its database cannot affect other runs.
    let mut conn = open(c, password, 0).await?;
    if mode(c) != RedisMode::Cluster {
        let cluster = info(&mut conn, "cluster").await?;
        if info_field(&cluster, "cluster_enabled") == Some("1") {
            return Err(AppError::invalid(
                "This server runs in cluster mode. Choose Cluster.",
            ));
        }
        if db != 0 {
            conn.query(redis::cmd("SELECT").arg(db)).await?;
        }
    }
    let server = info(&mut conn, "server").await?;
    if let Some(version) = info_field(&server, "valkey_version") {
        Ok(format!("Valkey {version}"))
    } else if let Some(version) = info_field(&server, "redis_version") {
        Ok(format!("Redis {version}"))
    } else {
        Err(AppError::new(
            ErrorCode::Database,
            "The server did not report its version",
        ))
    }
}

async fn info(conn: &mut RedisConn, section: &str) -> Result<String, AppError> {
    let mut cmd = redis::cmd("INFO");
    cmd.arg(section);
    let value = match conn {
        // Letting the cluster driver route INFO could aggregate replies from multiple nodes.
        RedisConn::Cluster(conn) => command_reply(
            conn.route_command(cmd, RoutingInfo::SingleNode(SingleNodeRoutingInfo::Random))
                .await?,
        )?,
        RedisConn::Single(_) => conn.query(&cmd).await?,
    };
    Ok(redis::from_redis_value(value).map_err(RedisError::from)?)
}

fn info_field<'a>(info: &'a str, name: &str) -> Option<&'a str> {
    info.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == name).then_some(value)
    })
}

impl From<RedisError> for AppError {
    fn from(error: RedisError) -> Self {
        redis_error(error)
    }
}

fn redis_error(error: RedisError) -> AppError {
    let kind = error.kind();
    let code = error.code();
    // redis-rs wraps a Cluster bootstrap failure as I/O, leaving its original error in detail.
    let initial_failure = if kind == ErrorKind::Io {
        error.detail().unwrap_or_default()
    } else {
        ""
    };
    if kind == ErrorKind::AuthenticationFailed
        || matches!(code, Some("WRONGPASS" | "NOAUTH"))
        || initial_failure.starts_with("\"WRONGPASS\"")
        || initial_failure.starts_with("\"NOAUTH\"")
        || initial_failure.ends_with(" - AuthenticationFailed")
        || initial_failure.contains(" - AuthenticationFailed: ")
    {
        return AppError::new(
            ErrorCode::AuthFailed,
            "Authentication failed. Check the user and password.",
        );
    }
    if kind == ErrorKind::RESP3NotSupported || initial_failure.ends_with(" - RESP3NotSupported") {
        return AppError::invalid("sandb needs Redis 6 or later (RESP3)");
    }
    if kind == ErrorKind::MasterNameNotFoundBySentinel {
        return AppError::invalid("The sentinels do not know this master name");
    }
    if error.is_io_error()
        || error.is_connection_refusal()
        || error.is_timeout()
        || matches!(
            kind,
            ErrorKind::ClusterConnectionNotFound | ErrorKind::EmptySentinelList
        )
    {
        return AppError::new(ErrorCode::ConnectFailed, "Could not connect to the server")
            .with_detail(error.to_string());
    }
    if let Some(code) = code {
        let code = ServerCode::new(Engine::Redis, code);
        let message = error.detail().unwrap_or("The server returned an error");
        let mut app = AppError::new(ErrorCode::Database, message).with_detail(code.label());
        app.server_code = Some(code);
        return app;
    }
    if kind == ErrorKind::InvalidClientConfig {
        return AppError::invalid(error.to_string());
    }
    AppError::internal("Unexpected database error").with_detail(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_secure_uri_and_discards_secret_and_options() {
        let (parts, password) =
            parse_uri("rediss://u%40name:p%3A%40ss@[::1]:6380/03?password=p%3A%40ss#insecure")
                .unwrap();
        assert_eq!(parts.redacted, "rediss://u%40name@[::1]:6380/03");
        assert_eq!(parts.host, "[::1]");
        assert_eq!(parts.port, 6380);
        assert_eq!(parts.user, "u@name");
        assert_eq!(parts.database, "3");
        assert_eq!(parts.ssl, SslMode::Require);
        assert_eq!(password.as_deref(), Some("p:@ss"));
    }

    #[test]
    fn supports_password_without_user_and_defaults_to_db_zero() {
        let (parts, password) = parse_uri("redis://:secret@localhost/").unwrap();
        assert_eq!(parts.redacted, "redis://localhost/");
        assert_eq!(parts.port, 6379);
        assert!(parts.user.is_empty());
        assert_eq!(parts.database, "0");
        assert_eq!(parts.ssl, SslMode::Disable);
        assert_eq!(password.as_deref(), Some("secret"));
    }

    #[test]
    fn rejects_malformed_urls_without_exposing_user_info() {
        for (uri, message) in [
            (
                "http://user:secret@host",
                "A Redis connection string starts with redis:// or rediss://",
            ),
            ("redis://user:secret@/", "The connection string has no host"),
            ("redis://user:secret@host:0", "Invalid connection string"),
            (
                "redis://user:secret@[not-ipv6]",
                "Invalid connection string",
            ),
            ("redis://user:secret@host:", "Invalid connection string"),
            (
                "redis://user:secret@host//3",
                "Database must be a number, for example 0",
            ),
            (
                "redis://user:secret@host/+3",
                "Database must be a number, for example 0",
            ),
            (
                "redis://user:secret@host/../3",
                "Database must be a number, for example 0",
            ),
            (
                "redis://user:secret@host/4294967296",
                "Database must be a number, for example 0",
            ),
            ("redis://user:secret%ff@host", "Invalid connection string"),
        ] {
            assert_eq!(parse_uri(uri).unwrap_err(), AppError::invalid(message));
        }
    }

    #[test]
    fn parses_nodes_with_and_without_explicit_ports() {
        assert_eq!(
            parse_nodes(" cache-1 , cache-2:6380, [::1]:6381 , [2001:db8::2] ", 6379).unwrap(),
            vec![
                ("cache-1".into(), 6379),
                ("cache-2".into(), 6380),
                ("::1".into(), 6381),
                ("2001:db8::2".into(), 6379),
            ],
        );
    }

    #[test]
    fn rejects_malformed_node_entries() {
        for entry in [
            "",
            "cache,",
            "cache:0",
            "cache:65536",
            "cache:+1",
            "cache:",
            ":6379",
            "[::1",
            "[localhost]:6380",
            "[::1]junk",
            "::1",
            "bad host",
            "user@host",
            "host/path",
            "host..name",
        ] {
            assert!(parse_nodes(entry, 6379).is_err(), "accepted {entry:?}");
        }
    }

    #[test]
    fn maps_cluster_bootstrap_auth_and_resp3_errors() {
        let wrap = |error: RedisError| {
            RedisError::from((
                ErrorKind::Io,
                "Failed to create initial connections",
                error.to_string(),
            ))
        };
        let wrong_password =
            redis::make_extension_error("WRONGPASS".into(), Some("bad credentials".into()));
        assert_eq!(
            redis_error(wrap(wrong_password)).code,
            ErrorCode::AuthFailed
        );
        let resp3 = RedisError::from((ErrorKind::RESP3NotSupported, "RESP3 is not supported"));
        assert_eq!(
            redis_error(wrap(resp3)),
            AppError::invalid("sandb needs Redis 6 or later (RESP3)"),
        );
    }
}
