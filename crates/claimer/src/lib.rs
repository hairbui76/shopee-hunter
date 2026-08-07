//! Claim policy and controlled execution (ROADMAP Phases 14-15).
//!
//! The decision to claim is explicit, testable, and configurable (policy). The
//! executor performs the account-mutating save only when policy allows and the
//! session is verified, records an append-only audit trail, classifies the
//! response, and applies a bounded, response-aware retry policy. Never an
//! unbounded loop.

pub mod policy;
pub mod retry;
pub mod service;

pub use policy::{evaluate, PolicyInputs};
pub use retry::{next_step, RetryStep};
pub use service::{
    ClaimExecutor, ClaimService, ClaimServiceConfig, ClaimServiceError, ExecOutcome,
};
