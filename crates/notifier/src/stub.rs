//! In-memory [`Notifier`] test double.
//!
//! Lives in the library (not behind `#[cfg(test)]`) so integration tests and
//! other crates can drive the outbox worker without a network. It never opens
//! a socket, so it is also a safe default for dry-run deployments.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::NotifierError;
use crate::notifier::Notifier;

/// One recorded delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMessage {
    pub chat_id: String,
    pub text: String,
}

#[derive(Debug, Default)]
struct Inner {
    sent: Mutex<Vec<SentMessage>>,
    attempts: AtomicUsize,
    remaining_failures: AtomicUsize,
    always_fail: AtomicBool,
}

/// Records messages instead of sending them; can be told to fail.
#[derive(Debug, Clone, Default)]
pub struct StubNotifier {
    inner: Arc<Inner>,
}

impl StubNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail the first `count` sends, then succeed.
    pub fn failing(count: usize) -> Self {
        let stub = Self::new();
        stub.inner.remaining_failures.store(count, Ordering::SeqCst);
        stub
    }

    /// Fail every send (dead-letter paths).
    pub fn always_failing() -> Self {
        let stub = Self::new();
        stub.inner.always_fail.store(true, Ordering::SeqCst);
        stub
    }

    /// Successfully delivered messages, in order.
    pub fn messages(&self) -> Vec<SentMessage> {
        self.lock_sent().clone()
    }

    /// Number of successful deliveries.
    pub fn delivered(&self) -> usize {
        self.lock_sent().len()
    }

    /// Number of `send` calls, including failed ones.
    pub fn attempts(&self) -> usize {
        self.inner.attempts.load(Ordering::SeqCst)
    }

    pub fn last_text(&self) -> Option<String> {
        self.lock_sent().last().map(|m| m.text.clone())
    }

    /// Poison-tolerant: a panicking test must not cascade into unrelated
    /// assertions, and this double holds no invariants worth protecting.
    fn lock_sent(&self) -> std::sync::MutexGuard<'_, Vec<SentMessage>> {
        self.inner
            .sent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait::async_trait]
impl Notifier for StubNotifier {
    async fn send(&self, chat_id: &str, text: &str) -> Result<(), NotifierError> {
        self.inner.attempts.fetch_add(1, Ordering::SeqCst);

        if self.inner.always_fail.load(Ordering::SeqCst) {
            return Err(NotifierError::transport("stub notifier always fails"));
        }
        let remaining = self.inner.remaining_failures.load(Ordering::SeqCst);
        if remaining > 0 {
            self.inner
                .remaining_failures
                .store(remaining - 1, Ordering::SeqCst);
            return Err(NotifierError::transport("stub notifier scripted failure"));
        }

        self.lock_sent().push(SentMessage {
            chat_id: chat_id.to_string(),
            text: text.to_string(),
        });
        Ok(())
    }

    fn name(&self) -> &str {
        "stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_successful_sends_only() {
        let stub = StubNotifier::failing(1);
        assert!(stub.send("chat", "first").await.is_err());
        stub.send("chat", "second").await.expect("second succeeds");

        assert_eq!(stub.attempts(), 2);
        assert_eq!(stub.delivered(), 1);
        assert_eq!(stub.last_text().as_deref(), Some("second"));
        assert_eq!(
            stub.messages()[0],
            SentMessage {
                chat_id: "chat".into(),
                text: "second".into()
            }
        );
    }

    #[tokio::test]
    async fn always_failing_never_records() {
        let stub = StubNotifier::always_failing();
        for _ in 0..3 {
            assert!(stub.send("chat", "x").await.is_err());
        }
        assert_eq!(stub.attempts(), 3);
        assert_eq!(stub.delivered(), 0);
    }
}
