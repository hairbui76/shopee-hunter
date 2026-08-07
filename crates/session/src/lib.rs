//! Shopee session lifecycle (ROADMAP Phases 9-10).
//!
//! Owns authentication state, classifies session health from low-impact
//! probes, gates the claim worker (claims are refused unless the session is
//! positively healthy), and manages cookie material securely. Browser-backed
//! bootstrap/refresh is feature-gated and never on the claim hot path.

pub mod cookies;
pub mod health;
pub mod manager;

#[cfg(feature = "browser")]
pub mod browser;

pub use cookies::{CookieStore, CookieStoreError};
pub use health::{ClientProber, SessionHealthWorker, SessionProber};
pub use manager::{ClaimGate, SessionManager, SessionSnapshot};
