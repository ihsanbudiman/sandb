//! The AI assistant: settings, an OpenAI-compatible client, the tools, the system prompt and
//! the chat loop.

pub mod approval;
pub mod client;
pub mod event;
pub mod prompt;
pub mod session;
pub mod settings;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tools;

use crate::core::error::AppError;
use crate::core::result::ReadLimits;
use crate::core::state::AppState;

/// The row and time limits of one AI read, from the saved settings. `run_ai_read` and the chat
/// tools use this, so both obey the numbers the settings page shows. A missing settings section
/// takes the defaults.
pub fn read_limits(state: &AppState) -> Result<ReadLimits, AppError> {
    Ok(settings::load(state)?.read_limits())
}
