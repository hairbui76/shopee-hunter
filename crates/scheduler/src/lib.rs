//! Durable, restart-safe scheduling with precision execution (ROADMAP
//! Phases 12-13).
//!
//! Database state is authoritative (see `shopee_hunter_storage`); Tokio timers
//! are only the execution mechanism. Persisted times are wall-clock UTC; the
//! final short wait uses a monotonic `tokio::time::Instant` deadline so it is
//! independent of ordinary wall-clock corrections.

pub mod precision;
pub mod report;
pub mod scheduler;

pub use precision::{monotonic_delay, ExecutionReport, PrecisionRunner};
pub use report::{PrecisionReport, PrecisionStats};
pub use scheduler::{ReconstructReport, Scheduler, SchedulerConfig};
