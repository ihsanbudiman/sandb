//! MCP settings in `settings.json`, and the bearer token in the secret store.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::error::AppError;
use crate::core::secrets::MCP_TOKEN_ACCOUNT;
use crate::core::state::AppState;

/// The `settings.json` section the MCP settings live in.
const SECTION: &str = "mcp";
pub const DEFAULT_MCP_PORT: u16 = 58124;

/// Missing fields take their default, so older or partial files still load.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpSettings {
    pub enabled: bool,
    /// The port on `127.0.0.1`.
    pub port: u16,
    /// Ids of the saved connections that MCP clients can use, in the order of the settings page.
    pub connections: Vec<String>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_MCP_PORT,
            connections: Vec::new(),
        }
    }
}

/// The settings and the server state, as the settings page sees them.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    #[serde(flatten)]
    pub settings: McpSettings,
    pub token: String,
    /// `http://127.0.0.1:{port}/mcp`, also when stopped, for the config snippets.
    pub url: String,
    pub running: bool,
    /// The last start failure (for example, a port in use); `None` while running or off.
    pub error: Option<String>,
}

impl McpSettings {
    /// Refuses a privileged port; keeps the first instance of each connection id and drops the
    /// ids of connections that no longer exist.
    pub(crate) fn normalized(mut self, state: &AppState) -> Result<Self, AppError> {
        if self.port < 1024 {
            return Err(AppError::invalid("The port must be between 1024 and 65535"));
        }
        let saved = state.list_connections();
        let mut seen = std::collections::HashSet::new();
        self.connections
            .retain(|id| saved.iter().any(|c| &c.id == id) && seen.insert(id.clone()));
        Ok(self)
    }
}

pub fn load(state: &AppState) -> Result<McpSettings, AppError> {
    state.store.load_section(SECTION)
}

pub(crate) fn save(state: &AppState, s: &McpSettings) -> Result<(), AppError> {
    state.store.save_section(SECTION, s)
}

/// The bearer token; the first call makes and stores one.
pub fn token(state: &AppState) -> Result<String, AppError> {
    match state.secrets().get(MCP_TOKEN_ACCOUNT)? {
        Some(t) if !t.is_empty() => Ok(t),
        _ => set_new_token(state),
    }
}

/// Replaces the stored token with a new one and returns it.
pub(crate) fn set_new_token(state: &AppState) -> Result<String, AppError> {
    let t = new_token();
    state.secrets().set(MCP_TOKEN_ACCOUNT, &t)?;
    Ok(t)
}

/// 64 hex characters, 244 random bits.
fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::secrets::MemoryStore;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let s = AppState::new(dir.path(), Box::new(MemoryStore::default())).unwrap();
        (dir, s)
    }

    #[test]
    fn a_fresh_install_is_off_with_no_shared_connections() {
        let (_d, s) = state();
        assert_eq!(load(&s).unwrap(), McpSettings::default());
    }

    #[test]
    fn privileged_ports_are_refused() {
        let (_d, s) = state();
        let e = McpSettings {
            port: 80,
            ..Default::default()
        }
        .normalized(&s)
        .unwrap_err();
        assert!(e.message.contains("1024"));
    }

    #[test]
    fn the_token_is_stable_until_replaced() {
        let (_d, s) = state();
        let t = token(&s).unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(token(&s).unwrap(), t);
        let n = set_new_token(&s).unwrap();
        assert_ne!(n, t);
        assert_eq!(token(&s).unwrap(), n);
    }
}
