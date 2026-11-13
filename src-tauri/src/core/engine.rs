//! Engine identity, the connection settings both engines share, and the dialect facts every
//! caller must agree on.
//!
//! `Engine` is the value a saved connection carries on the wire. SQL modules never take it: the
//! routing layer resolves it once per operation into a `SqlEngine`, which has no MongoDB variant.
//! Everything a dialect decides (label, quoting, placeholders, the MySQL system schemas, the
//! SQLite pragmas a read may run) is a method here, so a caller cannot hold its own copy.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Bound for opening any database connection, SQL pool or MongoDB client.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Pooled connections per SQL connection and database.
pub const MAX_CONNECTIONS: u32 = 5;

/// `mysql` covers MySQL and MariaDB (same driver; the version string tells them apart).
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Pg,
    Mysql,
    Sqlite,
    Mongo,
}

impl Engine {
    pub fn default_port(self) -> u16 {
        match self {
            Engine::Pg => 5432,
            Engine::Mysql => 3306,
            Engine::Sqlite => 0,
            Engine::Mongo => 27017,
        }
    }

    /// The SQL engine behind this connection. `None` for MongoDB: its operations are entered
    /// through the routing layer, never by a SQL module.
    pub fn sql(self) -> Option<SqlEngine> {
        match self {
            Engine::Pg => Some(SqlEngine::Pg),
            Engine::Mysql => Some(SqlEngine::Mysql),
            Engine::Sqlite => Some(SqlEngine::Sqlite),
            Engine::Mongo => None,
        }
    }

    /// The name shown to the user and the model. The SQL engines answer through the descriptor;
    /// MongoDB's is here because no SQL module may name it.
    pub fn label(self) -> &'static str {
        self.sql().map_or("MongoDB", SqlEngine::label)
    }

    /// The engine's rows can hold a number as text. MongoDB has no JSON form for an int64
    /// beyond 2^53 or a Decimal128, so its grid and chart rows carry them as text; a chart
    /// converts the finite ones before it draws them. The SQL drivers send JSON numbers.
    pub fn numbers_as_text(self) -> bool {
        self == Engine::Mongo
    }
}

/// A MySQL/MariaDB system schema: hidden from the browser, and an AI read may not run inside it.
#[derive(Debug, Clone, Copy)]
pub struct SystemSchema {
    pub name: &'static str,
    /// Its tables expose other sessions' SQL or users' credentials, so an AI read may not see them.
    pub private: bool,
}

/// The MySQL system schemas, in the order the browser's `NOT IN` list shows them.
const MYSQL_SYSTEM_SCHEMAS: &[SystemSchema] = &[
    SystemSchema {
        name: "information_schema",
        private: false,
    },
    SystemSchema {
        name: "mysql",
        private: true,
    },
    SystemSchema {
        name: "performance_schema",
        private: true,
    },
    SystemSchema {
        name: "sys",
        private: true,
    },
];

/// SQLite pragmas that only describe the schema: the AI guard's allow-list and the authorizer's
/// list are this one, so they cannot disagree.
const SQLITE_READ_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "foreign_key_list",
];

/// A connection engine that runs SQL. Every `db`, `sql`, `guard` and `conn_ro` function takes
/// this type, so a MongoDB arm cannot be written there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlEngine {
    Pg,
    Mysql,
    Sqlite,
}

impl SqlEngine {
    pub fn engine(self) -> Engine {
        match self {
            SqlEngine::Pg => Engine::Pg,
            SqlEngine::Mysql => Engine::Mysql,
            SqlEngine::Sqlite => Engine::Sqlite,
        }
    }

    pub fn default_port(self) -> u16 {
        self.engine().default_port()
    }

    /// The name shown to the user, in the guard's refusal and to the model.
    pub fn label(self) -> &'static str {
        match self {
            SqlEngine::Pg => "PostgreSQL",
            SqlEngine::Mysql => "MySQL/MariaDB",
            SqlEngine::Sqlite => "SQLite",
        }
    }

    pub fn quote(self, name: &str) -> String {
        match self {
            SqlEngine::Mysql => format!("`{}`", name.replace('`', "``")),
            SqlEngine::Pg | SqlEngine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// The bind placeholder for the `index`-th value of a `data_type` column. PostgreSQL binds
    /// with a cast to the column's own type, so text representations of bigint/numeric/uuid/…
    /// keep their meaning; the others use anonymous `?`.
    pub fn placeholder(self, index: usize, data_type: &str) -> String {
        match self {
            SqlEngine::Pg => format!("CAST(${index} AS {})", pg_base_type(data_type)),
            SqlEngine::Mysql | SqlEngine::Sqlite => "?".into(),
        }
    }

    pub fn text_cast_type(self) -> &'static str {
        match self {
            SqlEngine::Pg => "text",
            SqlEngine::Mysql => "CHAR",
            SqlEngine::Sqlite => "TEXT",
        }
    }

    pub fn system_schemas(self) -> &'static [SystemSchema] {
        match self {
            SqlEngine::Mysql => MYSQL_SYSTEM_SCHEMAS,
            SqlEngine::Pg | SqlEngine::Sqlite => &[],
        }
    }

    /// Whether `name` is a system schema the browser hides and an AI read may not run inside.
    pub fn is_system_schema(self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.system_schemas()
            .iter()
            .any(|s| s.name == name.as_str())
    }

    /// Whether `name` is a system schema whose tables expose other sessions or credentials.
    pub fn is_private_schema(self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.system_schemas()
            .iter()
            .any(|s| s.private && s.name == name.as_str())
    }

    pub fn read_pragmas(self) -> &'static [&'static str] {
        match self {
            SqlEngine::Sqlite => SQLITE_READ_PRAGMAS,
            SqlEngine::Pg | SqlEngine::Mysql => &[],
        }
    }

    pub fn is_read_pragma(self, name: &str) -> bool {
        self.read_pragmas().contains(&name)
    }

    /// The server version string says MariaDB (`11.4.2-MariaDB-ubu2404`). One test, because the
    /// timeout variable name and the version label both depend on the answer.
    pub fn is_mariadb(version: &str) -> bool {
        version.to_ascii_lowercase().contains("mariadb")
    }

    pub fn begin_transaction(self) -> &'static str {
        match self {
            SqlEngine::Mysql => "START TRANSACTION",
            SqlEngine::Pg | SqlEngine::Sqlite => "BEGIN",
        }
    }

    pub fn upsert_assignment(self, column: &str) -> String {
        match self {
            SqlEngine::Mysql => format!("{column} = VALUES({column})"),
            SqlEngine::Pg | SqlEngine::Sqlite => format!("{column} = EXCLUDED.{column}"),
        }
    }

    pub fn empty_insert(self, relation: &str) -> String {
        match self {
            SqlEngine::Mysql => format!("INSERT INTO {relation} () VALUES ()"),
            SqlEngine::Pg | SqlEngine::Sqlite => {
                format!("INSERT INTO {relation} DEFAULT VALUES")
            }
        }
    }

    /// The `INSERT` verb for an export. `skip`: duplicate rows are ignored.
    pub fn insert_verb(self, skip: bool) -> &'static str {
        match (skip, self) {
            (false, _) => "INSERT INTO",
            (true, SqlEngine::Mysql) => "INSERT IGNORE INTO",
            (true, SqlEngine::Sqlite) => "INSERT OR IGNORE INTO",
            (true, SqlEngine::Pg) => "INSERT INTO",
        }
    }
}

/// A PostgreSQL type name without its typmod and with the unbounded form of the types whose bare
/// name means length one. `varchar(20)` → `varchar` (a cast to it would truncate);
/// `character`/`bit` → `bpchar`/`bit varying` (their bare forms mean length one).
pub(crate) fn pg_base_type(data_type: &str) -> String {
    let without_typmod = if let Some(open) = data_type.find('(')
        && let Some(close) = data_type[open..].find(')').map(|i| open + i)
    {
        format!("{}{}", &data_type[..open], &data_type[close + 1..])
    } else {
        data_type.to_string()
    };
    if let Some(scalar) = without_typmod.strip_suffix("[]") {
        let scalar = match scalar {
            "character" | "char" => "bpchar",
            "bit" => "bit varying",
            other => other,
        };
        format!("{scalar}[]")
    } else {
        match without_typmod.as_str() {
            "character" | "char" => "bpchar".into(),
            "bit" => "bit varying".into(),
            _ => without_typmod,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_and_quote_per_engine() {
        assert_eq!(SqlEngine::Pg.label(), "PostgreSQL");
        assert_eq!(SqlEngine::Mysql.label(), "MySQL/MariaDB");
        assert_eq!(SqlEngine::Sqlite.label(), "SQLite");
        assert_eq!(Engine::Mongo.label(), "MongoDB");
        assert_eq!(SqlEngine::Pg.quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(SqlEngine::Sqlite.quote("plain"), "\"plain\"");
        assert_eq!(SqlEngine::Mysql.quote("a`b"), "`a``b`");
    }

    #[test]
    fn placeholders_use_the_column_type_on_pg() {
        assert_eq!(
            SqlEngine::Pg.placeholder(2, "character varying(20)"),
            "CAST($2 AS character varying)"
        );
        assert_eq!(SqlEngine::Pg.placeholder(1, "char"), "CAST($1 AS bpchar)");
        assert_eq!(SqlEngine::Mysql.placeholder(1, "int"), "?");
        assert_eq!(SqlEngine::Sqlite.placeholder(3, "text"), "?");
    }

    #[test]
    fn system_schemas_are_one_list() {
        for name in [
            "mysql",
            "MySQL",
            "sys",
            "performance_schema",
            "information_schema",
        ] {
            assert!(SqlEngine::Mysql.is_system_schema(name), "{name}");
        }
        assert!(!SqlEngine::Mysql.is_system_schema("shop"));
        assert!(!SqlEngine::Pg.is_system_schema("mysql"));
        assert!(SqlEngine::Mysql.is_private_schema("mysql"));
        assert!(!SqlEngine::Mysql.is_private_schema("information_schema"));
        assert!(!SqlEngine::Mysql.is_private_schema("shop"));
        // The browser's list and the AI read's refusal share this data.
        assert_eq!(
            SqlEngine::Mysql
                .system_schemas()
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            ["information_schema", "mysql", "performance_schema", "sys"]
        );
    }

    #[test]
    fn pragmas_and_dialect_statements() {
        assert!(SqlEngine::Sqlite.is_read_pragma("table_info"));
        assert!(!SqlEngine::Sqlite.is_read_pragma("journal_mode"));
        assert!(!SqlEngine::Pg.is_read_pragma("table_info"));
        assert!(SqlEngine::is_mariadb("11.4.2-MariaDB-ubu2404"));
        assert!(!SqlEngine::is_mariadb("8.4.3"));
        assert_eq!(SqlEngine::Mysql.begin_transaction(), "START TRANSACTION");
        assert_eq!(SqlEngine::Pg.begin_transaction(), "BEGIN");
        assert_eq!(
            SqlEngine::Mysql.upsert_assignment("`id`"),
            "`id` = VALUES(`id`)"
        );
        assert_eq!(
            SqlEngine::Pg.upsert_assignment("\"id\""),
            "\"id\" = EXCLUDED.\"id\""
        );
        assert_eq!(
            SqlEngine::Mysql.empty_insert("`t`"),
            "INSERT INTO `t` () VALUES ()"
        );
        assert_eq!(
            SqlEngine::Sqlite.empty_insert("\"t\""),
            "INSERT INTO \"t\" DEFAULT VALUES"
        );
        assert_eq!(SqlEngine::Mysql.insert_verb(true), "INSERT IGNORE INTO");
        assert_eq!(SqlEngine::Sqlite.insert_verb(true), "INSERT OR IGNORE INTO");
        assert_eq!(SqlEngine::Pg.insert_verb(true), "INSERT INTO");
        assert_eq!(SqlEngine::Pg.insert_verb(false), "INSERT INTO");
    }

    #[test]
    fn pg_base_type_strips_typmod_and_keeps_unbounded_forms() {
        assert_eq!(pg_base_type("numeric(10,2)"), "numeric");
        assert_eq!(pg_base_type("varchar(20)"), "varchar");
        assert_eq!(pg_base_type("character"), "bpchar");
        assert_eq!(pg_base_type("bit(3)"), "bit varying");
        assert_eq!(pg_base_type("integer[]"), "integer[]");
        assert_eq!(pg_base_type("char[]"), "bpchar[]");
        assert_eq!(pg_base_type("geometry(Point,4326)"), "geometry");
    }
}
