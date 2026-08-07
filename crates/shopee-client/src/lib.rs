//! Shopee transport and response-interpretation layer (ROADMAP Phase 11).
//!
//! This crate is the project's **anti-corruption layer**. Every assumption
//! about Shopee — paths, request bodies, envelope shapes, error codes, message
//! wording, login walls — lives here and nowhere else. No other crate may parse
//! raw Shopee JSON, and no other crate may hard-code a Shopee path.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`endpoints`] | The single registry of Shopee paths and absolute URLs. |
//! | [`error`] | Typed transport errors and `reqwest` classification. |
//! | [`dto`] | Schema-tolerant boundary DTOs; nothing escapes as a domain type. |
//! | [`classify`] | Response → [`shopee_hunter_domain::ClaimResultClass`] tables. |
//! | [`plan`] | Immutable, pre-serialized [`ClaimPlan`] built before `T=0`. |
//! | [`client`] | The one long-lived [`ShopeeClient`]. |
//!
//! # What this crate does not do
//!
//! It does not decide whether a voucher *should* be claimed (claim policy), it
//! does not own session state transitions (session manager), and it does not
//! persist anything (storage). It executes an approved plan and reports a
//! classified result.
//!
//! # Stability
//!
//! Every Shopee path and response assumption is marked `UNSTABLE` at its
//! definition and is covered by a sanitized fixture under
//! `tests/fixtures/shopee/`. When upstream changes, the expected blast radius
//! is [`endpoints`] plus the tables in [`classify`].
//!
//! # Secret handling
//!
//! Session cookies enter through [`SecretString`], whose `Debug`/`Display` both
//! print `[REDACTED]`, and are stored as a `sensitive`-marked header value.
//! Diagnostics keep whitelisted fields only: upstream code, HTTP status, and a
//! redacted, length-capped message excerpt. No header value is ever logged.
//!
//! # Typical use
//!
//! ```no_run
//! use shopee_hunter_client::{ClaimPlan, SecretString, ShopeeClient, ShopeeClientConfig};
//! # async fn run(voucher: &shopee_hunter_domain::Voucher) -> Result<(), Box<dyn std::error::Error>> {
//! // Once, at startup:
//! let client = ShopeeClient::new(ShopeeClientConfig::default())?;
//! client.set_cookie_header(SecretString::new(std::env::var("SHOPEE_COOKIE")?));
//!
//! // Preflight, before the precision window:
//! let plan = ClaimPlan::for_voucher(voucher)?;
//! let probe = client.probe_session().await?;
//! client.warm_connection().await?;
//!
//! // At T=0:
//! if probe.probe.to_session_state().allows_claims() {
//!     let outcome = client.execute_claim(&plan).await?;
//!     println!("{}", outcome.classified.class.as_str());
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod classify;
pub mod client;
pub mod dto;
pub mod endpoints;
pub mod error;
pub mod plan;

pub use classify::{
    classify_probe_response, classify_save_response, Classified, Diagnostic, SessionProbe,
    MAX_MESSAGE_EXCERPT,
};
pub use client::{
    ClaimOutcome, ProbeOutcome, SecretString, ShopeeClient, ShopeeClientConfig, DEFAULT_BASE_URL,
    DEFAULT_USER_AGENT,
};
pub use dto::{AccountInfoData, ShopeeEnvelope};
pub use endpoints::{Endpoint, EndpointRegistry, HttpMethod};
pub use error::{classify_reqwest_error, ClientError};
pub use plan::{ClaimIdentifier, ClaimPlan, PlanError};
