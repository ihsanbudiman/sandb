//! Layer 1 of the AI safeguards: classifies AI-written SQL before it reaches a database.
//! Policies only; the facts come from `core::sql::facts`.

pub mod classify;
#[cfg(test)]
mod tests;
