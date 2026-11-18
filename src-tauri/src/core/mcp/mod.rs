//! The MCP server: external agents (Claude Code, Cursor, Claude Desktop) read the connections
//! that the user shares in Settings > MCP, over Streamable HTTP on `127.0.0.1` with a bearer
//! token. Schema and read tools only; every read goes through the chat's read guard.

pub mod server;
pub mod settings;
pub mod tools;

pub use server::{StateHandle, apply, new_token, save, status};
pub use settings::{DEFAULT_MCP_PORT, McpSettings, McpStatus};
