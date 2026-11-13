//! The `comment` of one run's server operations. `$currentOp` finds an operation by
//! `command.comment`, so the comment is the only handle a cancel has on a request the driver is
//! waiting for. Every operation gets one; a collection `drop` is the one the driver cannot
//! comment, and `write` handles that arm explicitly.

use mongodb::bson::{Bson, Document, doc};

/// The comment of one run's operations: its query id, or the grid-edit marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpComment<'a>(&'a str);

impl<'a> OpComment<'a> {
    pub fn new(text: &'a str) -> Self {
        Self(text)
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }

    pub fn tag(self) -> Bson {
        Bson::String(self.0.to_string())
    }

    /// Puts the comment on a command document. A `comment` the user wrote wins: it is the one
    /// the user asked the server to see.
    pub fn apply(self, command: &mut Document) {
        if !command.contains_key("comment") {
            command.insert("comment", self.0);
        }
    }

    /// The `$currentOp` stage that selects this run's operations.
    pub fn current_op(self) -> Document {
        doc! { "$match": { "command.comment": self.0 } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_comment_wins() {
        let mut command = doc! { "ping": 1 };
        OpComment::new("q1").apply(&mut command);
        assert_eq!(command.get_str("comment"), Ok("q1"));
        let mut own = doc! { "ping": 1, "comment": "mine" };
        OpComment::new("q1").apply(&mut own);
        assert_eq!(own.get_str("comment"), Ok("mine"));
    }

    #[test]
    fn the_matcher_names_the_comment() {
        assert_eq!(
            OpComment::new("q1").current_op(),
            doc! { "$match": { "command.comment": "q1" } }
        );
        assert_eq!(OpComment::new("q1").tag(), Bson::String("q1".into()));
    }
}
