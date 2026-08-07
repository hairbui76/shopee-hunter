//! Observability foundation: structured logging, secret redaction, service
//! health registry, metrics, and the long-running worker supervisor.
//!
//! Every other crate that runs a loop or logs external data goes through this
//! crate so redaction and health semantics stay consistent.

pub mod alerts;
pub mod health;
pub mod logging;
pub mod metrics;
pub mod redact;
pub mod worker;

pub use alerts::{Alert, AlertEvaluator, AlertInputs, AlertKind, AlertThresholds};
pub use health::{HealthHandle, HealthRegistry, ServiceHealth, ServiceState};
pub use metrics::Metrics;
pub use worker::{IterationError, WorkerConfig, WorkerSupervisor};
