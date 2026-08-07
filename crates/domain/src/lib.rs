//! Source-independent domain model for shopee-hunter.
//!
//! This crate must stay transport-free: no HTTP, browser, database, or
//! Telegram dependencies. Collectors, clients, and storage adapt *to* these
//! types at their boundaries.

pub mod claim;
pub mod clock;
pub mod events;
pub mod identity;
pub mod ids;
pub mod schedule;
pub mod session;
pub mod validation;
pub mod voucher;

pub use claim::{ClaimDecision, ClaimResultClass, RetryClass};
pub use clock::{Clock, SystemClock};
pub use events::DomainEvent;
pub use identity::{IdentityBasis, VoucherIdentity};
pub use ids::SourceId;
pub use schedule::{JobStatus, ScheduleAction};
pub use session::SessionState;
pub use voucher::{
    DiscountType, Voucher, VoucherCandidate, VoucherScope, VoucherStatus, VoucherType,
};
