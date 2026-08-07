//! Modular voucher discovery. Collectors implement a common contract, run
//! under a supervisor that isolates failures and enforces per-source timeouts,
//! and feed candidates through the shared normalization/persistence pipeline.
//!
//! Collectors never claim vouchers or send notifications; they only emit
//! normalized candidates plus source metadata (see AGENTS.md subsystem rules).

pub mod contract;
pub mod external_feed;
pub mod pipeline;
pub mod provenance;
pub mod registry;
pub mod replay;
pub mod supervisor;

pub use contract::{
    CollectionContext, CollectionResult, CollectorError, PartialFailure, RateLimitHint,
    SharedSourceHealth, SourceHealth, SourceHealthState, VoucherCollector,
};
pub use external_feed::{ExternalFeedCollector, PARSER_VERSION as EXTERNAL_FEED_PARSER_VERSION};
pub use pipeline::{ingest_candidates, PipelineOutcome};
pub use provenance::{merge as merge_candidates, SourceConfidence, SourceRegistry};
pub use registry::CollectorRegistry;
pub use replay::ReplayCollector;
pub use supervisor::{CollectorSupervisor, SupervisedSource};
