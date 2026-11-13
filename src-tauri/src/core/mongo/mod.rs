//! MongoDB engine: the mongosh parser, the guard, the read and write paths, schema browsing,
//! grid edits and exports. Nothing here knows the app: an operation takes [`client::Access`], the
//! slice of state it needs, so `core::ops` is the only module that routes an operation into this
//! one; `catalog` and `commands` read connection settings through [`conn`].

use std::time::Duration;

pub mod ai;
pub mod client;
pub mod command;
pub mod comment;
pub mod conn;
pub mod dump;
pub mod edit;
pub mod export;
pub mod flatten;
pub mod guard;
pub mod introspect;
pub mod options;
pub mod parse;
pub mod reply;
pub mod run;
pub mod schema;
pub mod value;
pub mod write;

/// How long a cancelled write may still answer, so the run can tell whether it applied.
pub(crate) const WRITE_REPLY_WAIT: Duration = Duration::from_secs(2);
