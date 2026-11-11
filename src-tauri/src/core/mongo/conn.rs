//! MongoDB connections: the connection string without its password, client options and the
//! probe used by "Test connection".

use mongodb::bson::{Document, doc};
use mongodb::error::{Error as MongoError, ErrorKind, WriteFailure};
use mongodb::options::{
    ClientOptions, ConnectionString, Credential, HostInfo, ServerAddress, Tls, TlsOptions,
};

use crate::core::connections::{Connection, SslMode, Topology};
use crate::core::db::pool::{CONNECT_TIMEOUT, MAX_CONNECTIONS};
use crate::core::error::{AppError, ErrorCode};

/// MongoDB `AuthenticationFailed`.
const AUTH_FAILED: i32 = 18;

/// A connection string split into the part that may be saved and the password.
#[derive(Debug, PartialEq)]
pub struct ParsedUri {
    /// The string without the password. Safe to save in the connections file.
    pub redacted: String,
    pub password: Option<String>,
    /// For display only: comma-joined host list, or the SRV host.
    pub host: String,
    /// The port of the first host; 0 when the string has none (SRV, or the default port).
    pub port: u16,
    pub user: String,
    pub database: String,
    pub auth_source: String,
    pub ssl: SslMode,
}

fn invalid_uri(e: MongoError) -> AppError {
    AppError::invalid("Invalid connection string").with_detail(e.kind.to_string())
}

/// Validates `uri` with the driver's parser and removes the password from it.
pub fn parse_uri(uri: &str) -> Result<ParsedUri, AppError> {
    let cs = ConnectionString::parse(uri).map_err(invalid_uri)?;
    let srv = matches!(cs.host_info, HostInfo::DnsRecord(_));
    let (host, port) = match &cs.host_info {
        HostInfo::HostIdentifiers(hosts) => {
            let names: Vec<String> = hosts
                .iter()
                .map(|h| match h {
                    ServerAddress::Tcp { host, .. } => host.clone(),
                    other => other.to_string(),
                })
                .collect();
            let port = match hosts.first() {
                Some(ServerAddress::Tcp { port, .. }) => port.unwrap_or(0),
                _ => 0,
            };
            (names.join(","), port)
        }
        HostInfo::DnsRecord(h) => (h.clone(), 0),
        _ => (String::new(), 0),
    };
    let credential = cs.credential.as_ref();
    // `mongodb+srv://` turns TLS on unless the string turns it off.
    let tls_on = match cs.tls {
        Some(Tls::Enabled(_)) => true,
        Some(Tls::Disabled) => false,
        None => srv,
    };
    Ok(ParsedUri {
        redacted: without_password(uri),
        password: credential
            .and_then(|c| c.password.clone())
            .filter(|p| !p.is_empty()),
        host,
        port,
        user: credential
            .and_then(|c| c.username.clone())
            .unwrap_or_default(),
        database: cs.default_database.clone().unwrap_or_default(),
        auth_source: credential
            .and_then(|c| c.source.clone())
            .unwrap_or_default(),
        ssl: if tls_on {
            SslMode::Require
        } else {
            SslMode::Disable
        },
    })
}

/// `mongodb://u:p@h/db?x` → `mongodb://u@h/db?x`. Splits like the driver: the user info ends at
/// the last `@` before the options, so a password with an unencoded `@` still goes.
fn without_password(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_string();
    };
    let (pre_options, options) = match rest.split_once('?') {
        Some((p, o)) => (p, Some(o)),
        None => (rest, None),
    };
    let Some((user_info, hosts)) = pre_options.rsplit_once('@') else {
        return uri.to_string();
    };
    let user = user_info.split_once(':').map_or(user_info, |(u, _)| u);
    let mut out = format!("{scheme}://{user}@{hosts}");
    if let Some(o) = options {
        out.push('?');
        out.push_str(o);
    }
    out
}

/// Driver options for a saved connection. The password comes from the secret store only.
pub async fn client_options(
    c: &Connection,
    password: Option<&str>,
) -> Result<ClientOptions, AppError> {
    let mut o = if c.uri.is_empty() {
        let mut o = ClientOptions::builder()
            .hosts(vec![ServerAddress::Tcp {
                host: c.host.clone(),
                port: Some(c.port),
            }])
            .build();
        if !c.user.is_empty() {
            let mut cred = Credential::default();
            cred.username = Some(c.user.clone());
            cred.password = password.map(str::to_string);
            cred.source = Some(c.auth_source.clone()).filter(|s| !s.is_empty());
            o.credential = Some(cred);
        }
        // `Prefer` is refused when the connection is saved; if it still shows up, use TLS.
        o.tls = Some(match c.ssl {
            SslMode::Disable => Tls::Disabled,
            SslMode::Prefer | SslMode::Require => Tls::Enabled(TlsOptions::default()),
        });
        o
    } else {
        let mut cs = ConnectionString::parse(&c.uri).map_err(invalid_uri)?;
        if let (Some(cred), Some(pw)) = (cs.credential.as_mut(), password) {
            cred.password = Some(pw.to_string());
        }
        // Resolves `mongodb+srv://` hosts through DNS.
        ClientOptions::parse(cs).await?
    };
    // The string's own values win; else the same limits as the SQL pools.
    o.app_name.get_or_insert_with(|| "sandb".into());
    o.connect_timeout.get_or_insert(CONNECT_TIMEOUT);
    o.server_selection_timeout.get_or_insert(CONNECT_TIMEOUT);
    o.max_pool_size.get_or_insert(MAX_CONNECTIONS);
    Ok(o)
}

/// Opens a client, reads the version and the topology, and shuts the client down.
pub async fn probe(c: &Connection, password: Option<&str>) -> Result<(String, Topology), AppError> {
    let client = mongodb::Client::with_options(client_options(c, password).await?)?;
    let admin = client.database("admin");
    let r = async {
        let hello = admin.run_command(doc! { "hello": 1 }).await?;
        let build = admin.run_command(doc! { "buildInfo": 1 }).await?;
        let version = build.get_str("version").unwrap_or("unknown");
        Ok::<_, AppError>((format!("MongoDB {version}"), topology_of(&hello)))
    }
    .await;
    // On a `mongodb+srv://` cluster (Atlas) the shutdown takes up to a minute, longer than the
    // test time limit, so it runs in the background.
    tokio::spawn(client.shutdown().into_future());
    r
}

/// From a `hello` reply: `setName` means a replica set member, `msg: isdbgrid` a mongos.
pub(crate) fn topology_of(hello: &Document) -> Topology {
    if hello.get_str("msg") == Ok("isdbgrid") {
        Topology::Sharded
    } else if hello.contains_key("setName") {
        Topology::ReplicaSet
    } else {
        Topology::Standalone
    }
}

impl From<MongoError> for AppError {
    fn from(e: MongoError) -> Self {
        mongo_error(&e.kind, e.to_string())
    }
}

/// Maps one driver error. `detail` is the driver's own text, which repeats the whole reply
/// (`lsid`, `$clusterTime`, the response as a hex buffer); it is kept only where it is the only
/// diagnostic. A server error already says what the server said, and `server_error` labels the
/// code, so the driver's text stays out and an error dialog shows one readable line.
fn mongo_error(kind: &ErrorKind, detail: String) -> AppError {
    // The flag: the message is already the server's own, so the driver's text would only repeat
    // it (twice: once as prose, once as the hex of the raw reply).
    let (error, from_server) = match kind {
        ErrorKind::Authentication { .. } => (
            AppError::new(
                ErrorCode::AuthFailed,
                "Authentication failed. Check the user and password.",
            ),
            false,
        ),
        ErrorKind::Command(c) if c.code == AUTH_FAILED => (
            AppError::new(
                ErrorCode::AuthFailed,
                "Authentication failed. Check the user and password.",
            ),
            true,
        ),
        ErrorKind::Command(c) => (server_error(c.code, &c.message), true),
        // A refused write (duplicate key, validation, ...) carries the server's own message.
        ErrorKind::Write(WriteFailure::WriteError(w)) => (server_error(w.code, &w.message), true),
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => {
            (server_error(w.code, &w.message), true)
        }
        ErrorKind::InsertMany(m) => match (&m.write_errors, &m.write_concern_error) {
            (Some(errs), _) if !errs.is_empty() => {
                (server_error(errs[0].code, &errs[0].message), true)
            }
            (_, Some(w)) => (server_error(w.code, &w.message), true),
            _ => (AppError::internal("Unexpected database error"), false),
        },
        ErrorKind::ServerSelection { .. }
        | ErrorKind::Io(_)
        | ErrorKind::DnsResolve { .. }
        | ErrorKind::ConnectionPoolCleared { .. } => (
            AppError::new(ErrorCode::ConnectFailed, "Could not connect to the server"),
            false,
        ),
        ErrorKind::InvalidTlsConfig { .. } => (
            AppError::new(ErrorCode::InvalidInput, "Invalid connection settings"),
            false,
        ),
        // Settings the driver refuses, or an operation argument such as an update without
        // `$` operators: the driver's message says which.
        ErrorKind::InvalidArgument { message, .. } => (
            AppError::new(ErrorCode::InvalidInput, message.clone()),
            false,
        ),
        _ => (AppError::internal("Unexpected database error"), false),
    };
    if from_server {
        error
    } else {
        error.with_detail(detail)
    }
}

/// A server error: the server's own message, with its code as the label (`server_code`, and the
/// detail the UI appends to the message).
pub(crate) fn server_error(code: i32, message: &str) -> AppError {
    let label = format!("MongoDB error {code}");
    let mut a = AppError::new(ErrorCode::Database, message).with_detail(label.clone());
    a.server_code = Some(label);
    a
}

#[cfg(test)]
mod tests {
    use mongodb::error::CommandError;

    use super::*;

    /// The driver's text for a server reply: `Kind: Command failed: ... server response:
    /// Some(RawDocumentBuf { data: "5c01..." })`.
    const DRIVER_TEXT: &str = "Kind: Command failed: Error code 67 (CannotCreateIndex): TTL \
        indexes are single-field indexes, labels: {}, source: None, server response: \
        Some(RawDocumentBuf { data: \"5c010000\" })";

    fn command_error(code: i32, message: &str) -> CommandError {
        mongodb::bson::from_document(doc! {
            "code": code,
            "codeName": "TestError",
            "errmsg": message,
        })
        .expect("a command error")
    }

    #[test]
    fn a_server_error_shows_the_errmsg_and_the_code_label_only() {
        let e = mongo_error(
            &ErrorKind::Command(command_error(67, "TTL indexes are single-field indexes")),
            DRIVER_TEXT.to_string(),
        );
        assert_eq!(e.code, ErrorCode::Database);
        assert_eq!(e.message, "TTL indexes are single-field indexes");
        assert_eq!(e.server_code.as_deref(), Some("MongoDB error 67"));
        // The dialog appends the detail, so it must not be the driver's dump.
        assert_eq!(e.detail.as_deref(), Some("MongoDB error 67"));
        let shown = format!("{}: {}", e.message, e.detail.unwrap_or_default());
        assert!(!shown.contains("RawDocumentBuf"), "{shown}");
    }

    #[test]
    fn a_driver_error_keeps_its_detail() {
        let e = mongo_error(
            &ErrorKind::Custom(std::sync::Arc::new("boom")),
            "Kind: Custom(boom)".to_string(),
        );
        assert_eq!(e.code, ErrorCode::Internal);
        assert_eq!(e.detail.as_deref(), Some("Kind: Custom(boom)"));
    }

    #[test]
    fn password_is_removed_and_returned() {
        let p = parse_uri("mongodb://ann:s%40cret@db1:27018,db2/shop?authSource=admin&tls=true")
            .unwrap();
        assert_eq!(
            p.redacted,
            "mongodb://ann@db1:27018,db2/shop?authSource=admin&tls=true"
        );
        assert_eq!(p.password.as_deref(), Some("s@cret"), "percent-decoded");
        assert_eq!(
            (
                p.host.as_str(),
                p.port,
                p.user.as_str(),
                p.database.as_str()
            ),
            ("db1,db2", 27018, "ann", "shop")
        );
        assert_eq!(p.auth_source, "admin");
        assert_eq!(p.ssl, SslMode::Require);
    }

    #[test]
    fn strings_without_password_are_kept_as_typed() {
        for uri in [
            "mongodb://localhost",
            "mongodb://ann@localhost:27017/?replicaSet=rs0",
            "mongodb://localhost/?appName=x@y",
        ] {
            let p = parse_uri(uri).unwrap();
            assert_eq!(p.redacted, uri);
            assert_eq!(p.password, None, "{uri}");
        }
        // An empty password is no password: nothing would be stored.
        assert_eq!(parse_uri("mongodb://ann:@h").unwrap().password, None);
        assert_eq!(
            parse_uri("mongodb://ann:@h").unwrap().redacted,
            "mongodb://ann@h"
        );
    }

    #[test]
    fn srv_turns_tls_on_unless_the_string_turns_it_off() {
        let p = parse_uri("mongodb+srv://u:p@cluster0.example.net/app").unwrap();
        assert_eq!(p.redacted, "mongodb+srv://u@cluster0.example.net/app");
        assert_eq!((p.host.as_str(), p.port), ("cluster0.example.net", 0));
        assert_eq!(p.ssl, SslMode::Require);
        let off = parse_uri("mongodb+srv://cluster0.example.net/?tls=false").unwrap();
        assert_eq!(off.ssl, SslMode::Disable);
        assert_eq!(parse_uri("mongodb://h").unwrap().ssl, SslMode::Disable);
    }

    #[test]
    fn bad_strings_are_invalid_input() {
        for uri in [
            "",
            "localhost:27017",
            "postgres://h/db",
            "mongodb://h:notaport",
            "mongodb://h/?tls=maybe",
        ] {
            assert_eq!(
                parse_uri(uri).unwrap_err().code,
                ErrorCode::InvalidInput,
                "{uri}"
            );
        }
    }

    #[test]
    fn topology_from_hello() {
        assert_eq!(
            topology_of(&doc! { "isWritablePrimary": true }),
            Topology::Standalone
        );
        assert_eq!(
            topology_of(&doc! { "setName": "rs0" }),
            Topology::ReplicaSet
        );
        assert_eq!(topology_of(&doc! { "msg": "isdbgrid" }), Topology::Sharded);
    }
}
