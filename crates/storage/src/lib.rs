//! Persistence layer: a single [`Database`] over `sqlx::Any` (SQLite for
//! dev/replay, PostgreSQL for production) plus repository abstractions that
//! map rows to domain types at the boundary.
//!
//! Schema is portable SQL (TEXT timestamps/decimals/enums, INTEGER booleans)
//! so one migration set applies to both backends. Queries are runtime queries
//! to keep builds hermetic (no live DB required at compile time).

pub mod convert;
pub mod db;
pub mod error;
pub mod repositories;

pub use db::{Database, DbKind};
pub use error::StorageError;
pub use repositories::claim::{ClaimAttemptRecord, ClaimRepository};
pub use repositories::collector::{CollectorRunRecord, CollectorRunRepository, RunOutcome};
pub use repositories::health::HealthRepository;
pub use repositories::maintenance::{MaintenanceRepository, PruneReport, RetentionPolicy};
pub use repositories::outbox::{OutboxRepository, OutboxRow, OutboxStatus};
pub use repositories::schedule::{ScheduleJobRecord, ScheduleRepository};
pub use repositories::voucher::{UpsertOutcome, VoucherRepository};
