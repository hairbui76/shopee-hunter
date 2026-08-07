//! Single registry of every Shopee path this project talks to.
//!
//! Nothing outside this module may hard-code a Shopee path. Centralising them
//! here is what makes an upstream path change a one-file edit (AGENTS.md,
//! "Working with unstable external behavior").
//!
//! Every constant below is an **unstable private endpoint**: it is not part of
//! any published contract, it can change or disappear without notice, and it
//! must always be exercised through fixtures plus an opt-in live smoke check.

use crate::error::ClientError;

/// UNSTABLE: private endpoint, observed 2026-08-08 (community-documented in the
/// reference repositories, not yet re-verified against a live capture).
/// `POST`, JSON body — see [`crate::plan::ClaimPlan`] for the body shape.
pub const SAVE_VOUCHER_PATH: &str = "/api/v2/voucher_wallet/save_voucher";

/// UNSTABLE: private endpoint, observed 2026-08-08. `GET`, no body.
/// Used only as a low-impact authenticated session health probe.
pub const ACCOUNT_INFO_PATH: &str = "/api/v4/account/basic/get_account_info";

/// UNSTABLE: private endpoint, observed 2026-08-08. `POST`, JSON body.
/// Reserved for a future discovery collector; not yet called by any worker.
pub const BATCH_GET_VOUCHERS_PATH: &str = "/api/v2/promotion/get_batch_vouchers";

/// HTTP verb an endpoint expects. Kept as a tiny local enum so the registry
/// stays independent of the transport crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    /// Read-only request.
    Get,
    /// Mutating or body-carrying request.
    Post,
}

/// Logical key for a Shopee endpoint. Callers reference endpoints by key and
/// let the registry resolve the absolute URL, so no path string ever escapes
/// this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// Save (claim) a voucher into the account wallet. Account-mutating.
    SaveVoucher,
    /// Fetch minimal account info; used as the session health probe.
    AccountInfo,
    /// Batch voucher metadata lookup; reserved for discovery.
    BatchGetVouchers,
}

impl Endpoint {
    /// Path component of the endpoint, always starting with `/`.
    pub fn path(self) -> &'static str {
        match self {
            Self::SaveVoucher => SAVE_VOUCHER_PATH,
            Self::AccountInfo => ACCOUNT_INFO_PATH,
            Self::BatchGetVouchers => BATCH_GET_VOUCHERS_PATH,
        }
    }

    /// Verb the endpoint expects.
    pub fn method(self) -> HttpMethod {
        match self {
            Self::SaveVoucher | Self::BatchGetVouchers => HttpMethod::Post,
            Self::AccountInfo => HttpMethod::Get,
        }
    }

    /// Whether calling this endpoint mutates account state.
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::SaveVoucher)
    }

    /// Stable short name for logs and metrics labels.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SaveVoucher => "save_voucher",
            Self::AccountInfo => "account_info",
            Self::BatchGetVouchers => "batch_get_vouchers",
        }
    }

    /// Every endpoint known to this crate, for exhaustive tests and warmup.
    pub const ALL: &'static [Endpoint] = &[
        Endpoint::SaveVoucher,
        Endpoint::AccountInfo,
        Endpoint::BatchGetVouchers,
    ];
}

/// Absolute URLs for every endpoint, resolved once at construction.
///
/// URLs are pre-joined so the claim hot path performs no string formatting.
#[derive(Debug, Clone)]
pub struct EndpointRegistry {
    base_url: String,
    save_voucher: String,
    account_info: String,
    batch_get_vouchers: String,
}

impl EndpointRegistry {
    /// Build a registry from a base URL such as `https://shopee.vn`.
    ///
    /// The base URL must be `https`. Plain `http` is accepted **only** for
    /// loopback hosts, which exist so wiremock-backed tests can run without
    /// terminating TLS; production configuration can never reach that branch
    /// because Shopee is not served from loopback.
    ///
    /// Any trailing slash is trimmed so joins never produce `//`.
    pub fn new(base_url: &str) -> Result<Self, ClientError> {
        let trimmed = base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(ClientError::InvalidConfig {
                detail: "base_url is empty".to_string(),
            });
        }

        let parsed = reqwest::Url::parse(trimmed).map_err(|_| ClientError::InvalidConfig {
            detail: "base_url is not a valid absolute URL".to_string(),
        })?;

        match parsed.scheme() {
            "https" => {}
            "http" if is_loopback_host(parsed.host_str()) => {}
            "http" => {
                return Err(ClientError::InvalidConfig {
                    detail: "base_url must use https (plain http is only allowed for loopback \
                             test servers)"
                        .to_string(),
                })
            }
            _ => {
                return Err(ClientError::InvalidConfig {
                    detail: "base_url must use the https scheme".to_string(),
                })
            }
        }

        if parsed.host_str().is_none() {
            return Err(ClientError::InvalidConfig {
                detail: "base_url has no host".to_string(),
            });
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(ClientError::InvalidConfig {
                detail: "base_url must not carry a query string or fragment".to_string(),
            });
        }

        let base_url = trimmed.to_string();
        Ok(Self {
            save_voucher: format!("{base_url}{SAVE_VOUCHER_PATH}"),
            account_info: format!("{base_url}{ACCOUNT_INFO_PATH}"),
            batch_get_vouchers: format!("{base_url}{BATCH_GET_VOUCHERS_PATH}"),
            base_url,
        })
    }

    /// Normalised base URL, without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Absolute URL for an endpoint key.
    pub fn url(&self, endpoint: Endpoint) -> &str {
        match endpoint {
            Endpoint::SaveVoucher => &self.save_voucher,
            Endpoint::AccountInfo => &self.account_info,
            Endpoint::BatchGetVouchers => &self.batch_get_vouchers,
        }
    }

    /// Absolute URL of the voucher save endpoint.
    pub fn save_voucher(&self) -> &str {
        &self.save_voucher
    }

    /// Absolute URL of the session probe endpoint.
    pub fn account_info(&self) -> &str {
        &self.account_info
    }

    /// Absolute URL of the batch voucher metadata endpoint.
    pub fn batch_get_vouchers(&self) -> &str {
        &self.batch_get_vouchers
    }
}

fn is_loopback_host(host: Option<&str>) -> bool {
    match host {
        Some(h) => h == "localhost" || h == "::1" || h == "[::1]" || h.starts_with("127."),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_absolute_urls_without_double_slashes() {
        let reg = EndpointRegistry::new("https://shopee.vn/").expect("valid base url");
        assert_eq!(reg.base_url(), "https://shopee.vn");
        assert_eq!(
            reg.save_voucher(),
            "https://shopee.vn/api/v2/voucher_wallet/save_voucher"
        );
        assert_eq!(
            reg.account_info(),
            "https://shopee.vn/api/v4/account/basic/get_account_info"
        );
        assert_eq!(
            reg.batch_get_vouchers(),
            "https://shopee.vn/api/v2/promotion/get_batch_vouchers"
        );
        for endpoint in Endpoint::ALL {
            assert!(!reg.url(*endpoint).contains("//api"));
            assert!(reg.url(*endpoint).starts_with("https://shopee.vn/api/"));
        }
    }

    #[test]
    fn rejects_non_https_and_malformed_base_urls() {
        for bad in [
            "http://shopee.vn",
            "ftp://shopee.vn",
            "shopee.vn",
            "",
            "   ",
            "https://shopee.vn/?a=b",
            "https://shopee.vn/#frag",
        ] {
            assert!(
                EndpointRegistry::new(bad).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn allows_plain_http_only_for_loopback_test_servers() {
        for good in ["http://127.0.0.1:8080", "http://localhost:9", "http://[::1]"] {
            assert!(EndpointRegistry::new(good).is_ok(), "{good} should be ok");
        }
        assert!(EndpointRegistry::new("http://10.0.0.1").is_err());
    }

    #[test]
    fn endpoint_metadata_is_consistent() {
        assert_eq!(Endpoint::SaveVoucher.method(), HttpMethod::Post);
        assert_eq!(Endpoint::AccountInfo.method(), HttpMethod::Get);
        assert!(Endpoint::SaveVoucher.is_mutating());
        assert!(!Endpoint::AccountInfo.is_mutating());
        for endpoint in Endpoint::ALL {
            assert!(endpoint.path().starts_with('/'));
            assert!(!endpoint.as_str().is_empty());
        }
    }
}
