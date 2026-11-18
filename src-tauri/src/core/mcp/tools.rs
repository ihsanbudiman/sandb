//! The MCP tool list and the tool calls, built on the chat tools.
//!
//! A client sees only the connections that the user shares in Settings > MCP. Every database
//! tool takes a `connection` id and then runs the chat's own tool, so a read obeys the same
//! guard, read-only session, privacy, row cap and time limit as a chat. Nothing here writes.

use serde_json::{Map, Value, json};

use crate::core::ai;
use crate::core::ai::event::ChatEvent;
use crate::core::ai::tools::ToolContext;
use crate::core::connections::target_database;
use crate::core::error::AppError;
use crate::core::mcp::settings::{self, McpSettings};
use crate::core::ops;
use crate::core::result::Source;
use crate::core::state::AppState;

/// The database tools, in the order the client sees them. Each keeps its chat description and
/// parameters; MCP adds `connection` and `database`.
const DB_TOOLS: [&str; 4] = [
    "list_tables",
    "describe_table",
    "run_read_query",
    "run_mongo_read",
];

/// One tool, for `tools/list`.
pub struct McpTool {
    pub name: &'static str,
    pub description: String,
    pub schema: Value,
}

/// One call: the text the client shows and whether the call failed.
pub struct McpCall {
    pub text: String,
    pub is_error: bool,
}

/// Every tool, in the order the client sees them.
pub fn list() -> Vec<McpTool> {
    let mut tools = vec![
        McpTool {
            name: "list_connections",
            description: "List the database connections that sandb shares with MCP clients: id, \
                          name, engine, environment, default database and read-only flag. Call \
                          this first: every other tool takes a connection id from this list."
                .into(),
            schema: json!({"type": "object", "properties": {}}),
        },
        McpTool {
            name: "list_databases",
            description: "List the databases on the server of one connection.".into(),
            schema: json!({
                "type": "object",
                "properties": {"connection": connection_param()},
                "required": ["connection"],
            }),
        },
    ];
    for name in DB_TOOLS {
        let (description, mut schema) =
            ai::tools::definition(name).expect("the chat table has every MCP database tool");
        let properties = schema["properties"]
            .as_object_mut()
            .expect("every chat tool takes an object");
        properties.insert("connection".into(), connection_param());
        properties.insert("database".into(), database_param());
        match schema["required"].as_array_mut() {
            Some(required) => required.push(json!("connection")),
            None => schema["required"] = json!(["connection"]),
        }
        tools.push(McpTool {
            name,
            description,
            schema,
        });
    }
    tools
}

/// The `connection` argument of every database tool.
fn connection_param() -> Value {
    json!({"type": "string", "description": "Connection id from list_connections."})
}

/// The `database` argument: MongoDB needs it when the connection has no default database.
fn database_param() -> Value {
    json!({
        "type": "string",
        "description": "Database name. Default: the connection's own database. Required for a MongoDB connection without one; see list_databases."
    })
}

/// Runs one tool. A failure before the chat tool runs is the `{"error": …}` text too, so a
/// client always sees why the call failed.
pub async fn call(state: &AppState, name: &str, mut args: Map<String, Value>) -> McpCall {
    let mcp = match settings::load(state) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    if name == "list_connections" {
        return shared(state, &mcp);
    }
    if name != "list_databases" && !DB_TOOLS.contains(&name) {
        return failed(AppError::not_found(format!("Unknown tool {name}")));
    }
    let Some(id) = args
        .get("connection")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return failed(AppError::invalid(
            "The tool needs \"connection\", an id from list_connections",
        ));
    };
    // The same answer for a missing connection and an id that is not shared: a client learns
    // neither which ids exist nor which one it may not use.
    if !mcp.connections.contains(&id) {
        return failed(not_shared(&id));
    }
    let Ok(conn) = state.connection(&id) else {
        return failed(not_shared(&id));
    };
    if name == "list_databases" {
        return match ops::list_databases(state, &id).await {
            Ok(databases) => McpCall {
                text: json!({"databases": databases}).to_string(),
                is_error: false,
            },
            Err(e) => failed(e),
        };
    }
    let database = target_database(&conn, args.get("database").and_then(Value::as_str));
    if conn.engine.sql().is_none() && database.is_empty() {
        return failed(AppError::invalid(format!(
            "Pass \"database\" for {}: the connection has no default database. Call list_databases",
            conn.name
        )));
    }
    // The chat settings decide the privacy of an error message, the row cap and the time limit.
    let ai = match ai::settings::load(state) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    args.remove("connection");
    args.remove("database");
    // The chat tool picks its source by `source`; here there is one, and MCP chose it.
    args.remove("source");
    let source = Source {
        id: "db1".into(),
        conn,
        database,
        schema: None,
        allow_writes: false,
    };
    let out = ai::tools::execute(
        &ToolContext {
            state,
            sources: &[source],
            settings: &ai,
        },
        "mcp",
        name,
        &Value::Object(args).to_string(),
    )
    .await;
    McpCall {
        text: out.content,
        is_error: !out
            .events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolResult { ok: true, .. })),
    }
}

/// `list_connections`: the shared connections only, in the order the user chose, and without
/// host, port, user, file or uri, which a client does not need to use the tools.
fn shared(state: &AppState, mcp: &McpSettings) -> McpCall {
    let saved = state.list_connections();
    let list: Vec<Value> = mcp
        .connections
        .iter()
        .filter_map(|id| saved.iter().find(|c| &c.id == id))
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "engine": c.engine,
                "env": c.env,
                "database": c.database,
                "readOnly": c.read_only,
                "version": c.version,
            })
        })
        .collect();
    McpCall {
        text: Value::Array(list).to_string(),
        is_error: false,
    }
}

fn not_shared(id: &str) -> AppError {
    AppError::not_found(format!(
        "Connection {id} is not shared with MCP clients. Use an id from list_connections"
    ))
}

/// A failed call, in the shape the chat tools use for the model's view of an error.
fn failed(e: AppError) -> McpCall {
    McpCall {
        text: json!({"error": {"code": e.code, "message": e.message}}).to_string(),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ai::test_support::sqlite_conn;
    use crate::core::mcp::settings::DEFAULT_MCP_PORT;
    use crate::core::secrets::MemoryStore;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        (dir, st)
    }

    /// Shares `ids` with MCP clients, in that order.
    fn share(st: &AppState, ids: &[&str]) {
        settings::save(
            st,
            &McpSettings {
                enabled: false,
                port: DEFAULT_MCP_PORT,
                connections: ids.iter().map(|id| (*id).to_string()).collect(),
            },
        )
        .unwrap();
    }

    fn read_args(id: &str, sql: &str) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("connection".into(), json!(id));
        args.insert("sql".into(), json!(sql));
        args
    }

    /// A client sees only the allowlisted connections, and an id it may not use gives the same
    /// answer as an id that does not exist.
    #[tokio::test]
    async fn only_shared_connections_are_visible() {
        let (dir, st) = state();
        let a = sqlite_conn(&st, dir.path(), "a", 3).await;
        let b = sqlite_conn(&st, dir.path(), "b", 3).await;
        share(&st, &[a.as_str()]);

        let out = call(&st, "list_connections", Map::new()).await;
        assert!(!out.is_error, "{}", out.text);
        let list: Value = serde_json::from_str(&out.text).unwrap();
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 1, "{}", out.text);
        assert_eq!(list[0]["id"], a);
        assert_eq!(list[0]["name"], "a");
        assert_eq!(list[0]["engine"], "sqlite");
        assert_eq!(list[0]["database"], "main");
        assert_eq!(list[0]["readOnly"], false);

        let out = call(&st, "run_read_query", read_args(&b, "SELECT 1")).await;
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("not shared"), "{}", out.text);
    }

    /// MCP reads run through the chat's read path: the guard refuses a write, and nothing ran.
    #[tokio::test]
    async fn reads_use_the_guard() {
        let (dir, st) = state();
        let a = sqlite_conn(&st, dir.path(), "a", 3).await;
        share(&st, &[a.as_str()]);

        let count = "SELECT count(*) AS n FROM t";
        let out = call(&st, "run_read_query", read_args(&a, count)).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains('3'), "{}", out.text);

        let out = call(&st, "run_read_query", read_args(&a, "DELETE FROM t")).await;
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("guard_rejected"), "{}", out.text);

        let out = call(&st, "run_read_query", read_args(&a, count)).await;
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains('3'), "{}", out.text);
    }

    /// The list is fixed and read-only: no write tool, and every database tool picks a
    /// connection.
    #[test]
    fn tool_list_is_read_only() {
        let tools = list();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "list_connections",
                "list_databases",
                "list_tables",
                "describe_table",
                "run_read_query",
                "run_mongo_read"
            ]
        );
        for tool in &tools[1..] {
            let required = tool.schema["required"].as_array().unwrap();
            assert!(
                required.iter().any(|v| v.as_str() == Some("connection")),
                "{}",
                tool.name
            );
        }
    }
}
