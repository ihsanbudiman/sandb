use std::fmt;

use crate::core::error::AppError;

/// A parse failure. Line and column start at 1; the column counts characters.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl From<ParseError> for AppError {
    fn from(error: ParseError) -> Self {
        AppError::invalid(error.to_string())
    }
}
