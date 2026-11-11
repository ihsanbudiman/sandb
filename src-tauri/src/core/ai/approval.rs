//! Proposed writes that wait for the user. The chat loop waits on one entry; `approve_write`
//! and `deny_write` send the decision.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::core::error::AppError;

/// How long a proposed write waits for the user. After that it counts as denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Chat id and tool call id. Models reuse call ids such as `call_1`, so the chat id is part of
/// the key.
type Key = (String, String);

#[derive(Default)]
pub struct Approvals {
    waiting: Mutex<HashMap<Key, oneshot::Sender<bool>>>,
}

/// One write that waits. Dropping it removes the entry, so a late decision gets `not_found`.
pub struct Waiting<'a> {
    approvals: &'a Approvals,
    key: Key,
    rx: oneshot::Receiver<bool>,
}

impl Waiting<'_> {
    /// True for approve. False for deny, and when the decision can never come.
    pub async fn decision(&mut self) -> bool {
        (&mut self.rx).await.unwrap_or(false)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.approvals.lock().remove(&self.key);
    }
}

impl Approvals {
    fn lock(&self) -> MutexGuard<'_, HashMap<Key, oneshot::Sender<bool>>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a write before its preview goes to the UI, so a fast click finds it.
    pub fn wait(&self, chat_id: &str, call_id: &str) -> Waiting<'_> {
        let (tx, rx) = oneshot::channel();
        let key = (chat_id.to_string(), call_id.to_string());
        self.lock().insert(key.clone(), tx);
        Waiting {
            approvals: self,
            key,
            rx,
        }
    }

    /// Sends the user's decision. `not_found` when no write waits under these ids: it timed
    /// out, or its chat stopped.
    pub fn decide(&self, chat_id: &str, call_id: &str, approve: bool) -> Result<(), AppError> {
        let key = (chat_id.to_string(), call_id.to_string());
        match self.lock().remove(&key) {
            Some(tx) => {
                let _ = tx.send(approve);
                Ok(())
            }
            None => Err(AppError::not_found(
                "This write no longer waits for a decision",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorCode;

    #[tokio::test]
    async fn a_decision_reaches_only_its_own_chat() {
        let a = Approvals::default();
        let mut w1 = a.wait("chat-1", "call_1");
        let mut w2 = a.wait("chat-2", "call_1");
        a.decide("chat-2", "call_1", true).unwrap();
        assert!(w2.decision().await);
        a.decide("chat-1", "call_1", false).unwrap();
        assert!(!w1.decision().await);
    }

    #[test]
    fn a_decision_after_the_wait_ended_is_not_found() {
        let a = Approvals::default();
        drop(a.wait("chat", "call_1"));
        let e = a.decide("chat", "call_1", true).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        // One decision only: a second click cannot approve again.
        let _w = a.wait("chat", "call_2");
        a.decide("chat", "call_2", false).unwrap();
        assert!(a.decide("chat", "call_2", true).is_err());
    }
}
