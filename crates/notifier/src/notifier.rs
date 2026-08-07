//! The delivery boundary.
//!
//! Everything above this trait (formatting, the outbox worker) is transport
//! agnostic, so Telegram can be swapped or supplemented without touching
//! business logic (ROADMAP Phase 8, "Outbox preparation").

use crate::error::NotifierError;

#[async_trait::async_trait]
pub trait Notifier: Send + Sync {
    /// Deliver one already-rendered message.
    ///
    /// Implementations own their retry budget and must return a classified
    /// [`NotifierError`] rather than retrying forever.
    async fn send(&self, chat_id: &str, text: &str) -> Result<(), NotifierError>;

    /// Short identifier for logs and metrics.
    fn name(&self) -> &str {
        "notifier"
    }
}
