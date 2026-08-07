//! Owner-facing notifications (ROADMAP Phase 8 + Phase 16 outbox worker).
//!
//! The crate is split so that *what to say* is independent of *how it is sent*
//! and *when it is sent*:
//!
//! ```text
//! DomainEvent ──► format ──► RenderedMessage ──► Notifier ──► Telegram
//!                   ▲                               ▲
//!            pure, testable                  transport boundary
//!                                                   │
//!                        notification_outbox ──► OutboxNotifierWorker
//! ```
//!
//! * [`format`] renders every [`shopee_hunter_domain::events::DomainEvent`]
//!   into plain text, scrubbing secrets and bounding length. It performs no
//!   I/O, so message content is unit-testable.
//! * [`Notifier`] is the delivery boundary: [`TelegramNotifier`] for
//!   production, [`StubNotifier`] for tests and dry runs.
//! * [`OutboxNotifierWorker`] drains the durable outbox, so notifications
//!   survive crashes and a Telegram outage never blocks core transactions.
//!
//! Nothing in this crate logs or sends cookies, tokens, session material, or
//! raw upstream payloads; see [`format::scrub`] and
//! [`TelegramNotifier`]'s token handling.

pub mod error;
pub mod format;
pub mod notifier;
pub mod outbox;
pub mod stub;
pub mod telegram;

pub use error::NotifierError;
pub use format::{
    category_for, render, render_event, MessageCategory, RenderedMessage, MAX_MESSAGE_CHARS,
};
pub use notifier::Notifier;
pub use outbox::{DrainStats, OutboxNotifierWorker, OutboxWorkerConfig, OutboxWorkerError};
pub use stub::{SentMessage, StubNotifier};
pub use telegram::{RetryPolicy, TelegramNotifier, TELEGRAM_API_BASE};
