//! `AppError`: the one error type every command returns. It serializes to `{code, message, detail?}`.

use serde::Serialize;

use crate::core::engine::{CONNECT_TIMEOUT, Engine};

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    AuthFailed,
    ConnectFailed,
    Timeout,
    SecretStore,
    Io,
    /// The server answered with an error that has no more specific code (e.g. unknown database).
    Database,
    Cancelled,
    /// The AI read guard (layer 1) refused the SQL before it reached the database.
    GuardRejected,
    /// The database refused a write inside a read-only AI read (layer 2).
    ReadOnlyViolation,
    /// The editor refused SQL on a read-only connection before it reached the database.
    ConnectionReadOnly,
    /// The AI server does not list the configured model.
    ModelNotFound,
    /// The AI server answered with an HTTP error other than 401/403.
    AiUpstream,
    /// The AI server broke the OpenAI protocol (bad JSON, a stream that ends early, too many steps).
    AiProtocol,
    /// The assistant proposed a write, but write mode is off or the connection is read-only.
    WriteBlocked,
    /// The user denied a proposed write, or nobody decided in time. Nothing ran.
    WriteDenied,
    /// The feature does not apply to this engine (e.g. Extended JSON export on a SQL connection).
    Unsupported,
    /// The dev bridge rejected the request because its bearer token was missing or wrong.
    Unauthorized,
    Internal,
}

impl ErrorCode {
    /// The HTTP status the dev bridge answers with. One function next to the list, so a new
    /// variant cannot compile without a status; the desktop app has no HTTP layer.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::Unauthorized => 401,
            ErrorCode::NotFound => 404,
            ErrorCode::InvalidInput | ErrorCode::Unsupported => 400,
            ErrorCode::AuthFailed | ErrorCode::Database | ErrorCode::ModelNotFound => 422,
            ErrorCode::ConnectFailed | ErrorCode::AiUpstream | ErrorCode::AiProtocol => 502,
            ErrorCode::Timeout => 504,
            ErrorCode::Cancelled | ErrorCode::WriteDenied => 409,
            ErrorCode::GuardRejected
            | ErrorCode::ReadOnlyViolation
            | ErrorCode::ConnectionReadOnly
            | ErrorCode::WriteBlocked => 403,
            ErrorCode::SecretStore | ErrorCode::Io | ErrorCode::Internal => 500,
        }
    }
}

/// The server's own error code and the vocabulary that names it. Comparisons are numeric or
/// exact (`engine` + `code`); the label a user or the model sees is built in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCode {
    /// The server that sent the code; it decides the label's words.
    pub engine: Engine,
    pub code: String,
}

impl ServerCode {
    pub fn new(engine: Engine, code: impl Into<String>) -> Self {
        Self {
            engine,
            code: code.into(),
        }
    }

    /// `SQLSTATE 22P02`, `MySQL error 1366`, `SQLite error 787`, `MongoDB error 11000`.
    pub fn label(&self) -> String {
        let prefix = match self.engine {
            Engine::Pg => "SQLSTATE",
            Engine::Mysql => "MySQL error",
            Engine::Sqlite => "SQLite error",
            Engine::Mongo => "MongoDB error",
        };
        format!("{prefix} {}", self.code)
    }
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub detail: Option<String>,
    /// The server's own error code when `message` is the server's text, e.g. SQLSTATE 22P02 or
    /// MongoDB 11000. Not serialized: it lets a caller name the error without the text.
    #[serde(skip)]
    pub server_code: Option<ServerCode>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
            server_code: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, what)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled, "Query cancelled")
    }

    /// A chat run the user stopped, or one whose receiver went away.
    pub fn stopped() -> Self {
        Self::new(ErrorCode::Cancelled, "Stopped")
    }

    /// A SQL-only path reached a connection of another engine. The routing layer resolves the
    /// engine first, so this is a caller bug, not a user error.
    pub fn not_sql() -> Self {
        Self::internal("Not a SQL connection")
    }

    /// The opener of a connection gave up after `CONNECT_TIMEOUT`.
    pub fn connect_timeout() -> Self {
        Self::new(
            ErrorCode::Timeout,
            format!(
                "No answer from the server within {} s",
                CONNECT_TIMEOUT.as_secs()
            ),
        )
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new(ErrorCode::Io, "File system error").with_detail(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::new(ErrorCode::Io, "Could not read or write a data file")
            .with_detail(e.to_string())
    }
}

/// PG `28P01` invalid_password, `28000` invalid_authorization_specification.
/// MySQL/MariaDB: 1045 access denied (bad password), 1044 access denied to database, 1698 auth plugin denied.
pub(crate) fn is_auth_error(sqlstate: Option<&str>, mysql_number: Option<u16>) -> bool {
    matches!(sqlstate, Some("28P01" | "28000")) || matches!(mysql_number, Some(1044 | 1045 | 1698))
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        let detail = e.to_string();
        match &e {
            sqlx::Error::Database(db) => {
                let mysql_number = db
                    .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                    .map(|m| m.number());
                if is_auth_error(db.code().as_deref(), mysql_number) {
                    AppError::new(
                        ErrorCode::AuthFailed,
                        "Authentication failed. Check the user and password.",
                    )
                } else {
                    let mut e = AppError::new(ErrorCode::Database, db.message().to_string());
                    e.server_code = server_code_label(&**db, mysql_number);
                    e
                }
                .with_detail(detail)
            }
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::Protocol(_) => {
                AppError::new(ErrorCode::ConnectFailed, "Could not connect to the server")
                    .with_detail(detail)
            }
            sqlx::Error::PoolTimedOut => AppError::new(
                ErrorCode::Timeout,
                "Timed out while connecting to the server",
            )
            .with_detail(detail),
            sqlx::Error::Configuration(_) => {
                AppError::new(ErrorCode::InvalidInput, "Invalid connection settings")
                    .with_detail(detail)
            }
            _ => AppError::internal("Unexpected database error").with_detail(detail),
        }
    }
}

fn server_code_label(
    db: &(dyn sqlx::error::DatabaseError + 'static),
    mysql_number: Option<u16>,
) -> Option<ServerCode> {
    if let Some(n) = mysql_number {
        return Some(ServerCode::new(Engine::Mysql, n.to_string()));
    }
    let code = db.code()?;
    let engine = if db.try_downcast_ref::<sqlx::sqlite::SqliteError>().is_some() {
        Engine::Sqlite
    } else {
        Engine::Pg
    };
    Some(ServerCode::new(engine, code.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::fmt;

    #[test]
    fn serializes_to_code_message_detail() {
        let e = AppError::new(ErrorCode::AuthFailed, "nope").with_detail("28P01");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "auth_failed", "message": "nope", "detail": "28P01"})
        );
    }

    #[test]
    fn detail_is_omitted_when_absent() {
        let v = serde_json::to_value(AppError::not_found("x")).unwrap();
        assert_eq!(v, serde_json::json!({"code": "not_found", "message": "x"}));
    }

    /// Every variant, in declaration order.
    const ALL: [ErrorCode; 20] = [
        ErrorCode::NotFound,
        ErrorCode::InvalidInput,
        ErrorCode::AuthFailed,
        ErrorCode::ConnectFailed,
        ErrorCode::Timeout,
        ErrorCode::SecretStore,
        ErrorCode::Io,
        ErrorCode::Database,
        ErrorCode::Cancelled,
        ErrorCode::GuardRejected,
        ErrorCode::ReadOnlyViolation,
        ErrorCode::ConnectionReadOnly,
        ErrorCode::ModelNotFound,
        ErrorCode::AiUpstream,
        ErrorCode::AiProtocol,
        ErrorCode::WriteBlocked,
        ErrorCode::WriteDenied,
        ErrorCode::Unsupported,
        ErrorCode::Unauthorized,
        ErrorCode::Internal,
    ];

    /// The wire name of each variant. The `match` is exhaustive, so a new variant does not
    /// compile until it is named here, and `ALL` must then list it.
    fn wire_name(c: ErrorCode) -> &'static str {
        match c {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidInput => "invalid_input",
            ErrorCode::AuthFailed => "auth_failed",
            ErrorCode::ConnectFailed => "connect_failed",
            ErrorCode::Timeout => "timeout",
            ErrorCode::SecretStore => "secret_store",
            ErrorCode::Io => "io",
            ErrorCode::Database => "database",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::GuardRejected => "guard_rejected",
            ErrorCode::ReadOnlyViolation => "read_only_violation",
            ErrorCode::ConnectionReadOnly => "connection_read_only",
            ErrorCode::ModelNotFound => "model_not_found",
            ErrorCode::AiUpstream => "ai_upstream",
            ErrorCode::AiProtocol => "ai_protocol",
            ErrorCode::WriteBlocked => "write_blocked",
            ErrorCode::WriteDenied => "write_denied",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Internal => "internal",
        }
    }

    #[test]
    fn every_code_serializes_to_its_wire_name() {
        for c in ALL {
            let name = wire_name(c);
            assert_eq!(serde_json::to_value(c).unwrap(), serde_json::json!(name));
            assert!(
                name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{name} is not snake_case"
            );
        }
    }

    #[test]
    fn auth_codes_for_each_engine() {
        assert!(is_auth_error(Some("28P01"), None));
        assert!(is_auth_error(Some("28000"), None));
        assert!(is_auth_error(Some("28000"), Some(1045)));
        assert!(is_auth_error(None, Some(1045)));
        assert!(!is_auth_error(Some("42P01"), None));
        assert!(!is_auth_error(Some("3D000"), None));
        assert!(!is_auth_error(None, Some(1049)));
    }

    /// Minimal fake so a `sqlx::Error::Database` with a chosen SQLSTATE can be built without a server.
    #[derive(Debug)]
    struct FakeDbError(&'static str);
    impl fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "fake {}", self.0)
        }
    }
    impl StdError for FakeDbError {}
    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "fake server error"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    #[test]
    fn maps_sqlx_errors() {
        let refused = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert_eq!(AppError::from(refused).code, ErrorCode::ConnectFailed);
        assert_eq!(
            AppError::from(sqlx::Error::PoolTimedOut).code,
            ErrorCode::Timeout
        );

        let bad_pw = sqlx::Error::Database(Box::new(FakeDbError("28P01")));
        let e = AppError::from(bad_pw);
        assert_eq!(e.code, ErrorCode::AuthFailed);
        assert!(e.detail.is_some());

        let other = sqlx::Error::Database(Box::new(FakeDbError("3D000")));
        let e = AppError::from(other);
        assert_eq!(e.code, ErrorCode::Database);
        assert_eq!(e.message, "fake server error");
        assert_eq!(e.server_code, Some(ServerCode::new(Engine::Pg, "3D000")));
        assert_eq!(AppError::invalid("x").server_code, None);

        let cfg = sqlx::Error::Configuration("bad".into());
        assert_eq!(AppError::from(cfg).code, ErrorCode::InvalidInput);
    }

    /// The one place the human-readable form of a server code is written.
    #[test]
    fn server_code_labels_name_their_server() {
        for (engine, code, label) in [
            (Engine::Pg, "22P02", "SQLSTATE 22P02"),
            (Engine::Mysql, "1366", "MySQL error 1366"),
            (Engine::Sqlite, "787", "SQLite error 787"),
            (Engine::Mongo, "11000", "MongoDB error 11000"),
        ] {
            assert_eq!(ServerCode::new(engine, code).label(), label);
        }
    }
}
