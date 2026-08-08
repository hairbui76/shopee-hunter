//! Typed application settings loaded from environment variables.
//!
//! Every tunable (interval, timeout, retry budget, feature flag) lives here;
//! worker code must not read `std::env` directly or embed magic constants.

use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required setting {0}")]
    Missing(&'static str),
    #[error("invalid value for {key}: {reason}")]
    Invalid { key: &'static str, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Development,
    Production,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub app: AppSettings,
    pub database: DatabaseSettings,
    pub shopee: ShopeeSettings,
    pub session: SessionSettings,
    pub collectors: CollectorSettings,
    pub scheduler: SchedulerSettings,
    pub claim: ClaimSettings,
    pub telegram: TelegramSettings,
    pub observability: ObservabilitySettings,
}

#[derive(Debug, Clone)]
pub struct AppSettings {
    pub env: AppEnv,
    pub log_level: String,
    pub log_format: String,
}

#[derive(Debug, Clone)]
pub struct DatabaseSettings {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone)]
pub struct ShopeeSettings {
    pub base_url: String,
    pub profile_path: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct SessionSettings {
    pub health_interval: Duration,
    pub cookie_store_path: String,
    pub enable_browser_refresh: bool,
}

#[derive(Debug, Clone)]
pub struct CollectorSettings {
    pub default_interval: Duration,
    pub timeout: Duration,
    pub max_concurrency: usize,
    pub enable_shopee_page: bool,
    pub enable_external_feed: bool,
    pub external_feed_url: Option<String>,
    /// Accesstrade coupon feed (real Shopee VN voucher source).
    pub enable_accesstrade: bool,
    pub accesstrade_token: Option<String>,
    pub accesstrade_merchant: String,
    pub enable_manual: bool,
    pub enable_replay: bool,
    pub replay_fixture_dir: String,
    /// Per-source hourly request ceiling (Phase 19 source budget).
    pub source_hourly_budget: u32,
}

#[derive(Debug, Clone)]
pub struct SchedulerSettings {
    pub coarse_tick: Duration,
    pub preflight: Duration,
    pub precision_window: Duration,
    pub stale_after: Duration,
}

#[derive(Debug, Clone)]
pub struct ClaimSettings {
    pub enable_auto_claim: bool,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub min_score: i64,
}

#[derive(Debug, Clone)]
pub struct TelegramSettings {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
    pub admin_chat_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ObservabilitySettings {
    pub healthcheck_bind: SocketAddr,
    pub metrics_enabled: bool,
    /// Shared secret required for mutating admin endpoints. When empty, those
    /// endpoints are disabled entirely (read-only health remains available).
    pub admin_token: String,
}

pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

fn get(lookup: Lookup, key: &'static str) -> Option<String> {
    lookup(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn require(lookup: Lookup, key: &'static str) -> Result<String, ConfigError> {
    get(lookup, key).ok_or(ConfigError::Missing(key))
}

fn parse<T: std::str::FromStr>(
    lookup: Lookup,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError>
where
    T::Err: std::fmt::Display,
{
    match get(lookup, key) {
        None => Ok(default),
        Some(raw) => raw.parse().map_err(|e: T::Err| ConfigError::Invalid {
            key,
            reason: e.to_string(),
        }),
    }
}

fn parse_bool(lookup: Lookup, key: &'static str, default: bool) -> Result<bool, ConfigError> {
    match get(lookup, key) {
        None => Ok(default),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError::Invalid {
                key,
                reason: format!("expected boolean, got {other:?}"),
            }),
        },
    }
}

fn secs(lookup: Lookup, key: &'static str, default_secs: u64) -> Result<Duration, ConfigError> {
    Ok(Duration::from_secs(parse(lookup, key, default_secs)?))
}

fn millis(lookup: Lookup, key: &'static str, default_ms: u64) -> Result<Duration, ConfigError> {
    Ok(Duration::from_millis(parse(lookup, key, default_ms)?))
}

impl Settings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|key| std::env::var(key).ok())
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let env = match get(lookup, "APP_ENV").as_deref() {
            None | Some("development") => AppEnv::Development,
            Some("production") => AppEnv::Production,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    key: "APP_ENV",
                    reason: format!("expected development|production, got {other:?}"),
                })
            }
        };

        let telegram_enabled = parse_bool(lookup, "ENABLE_TELEGRAM", false)?;
        let telegram = TelegramSettings {
            enabled: telegram_enabled,
            bot_token: if telegram_enabled {
                require(lookup, "TELEGRAM_BOT_TOKEN")?
            } else {
                get(lookup, "TELEGRAM_BOT_TOKEN").unwrap_or_default()
            },
            chat_id: if telegram_enabled {
                require(lookup, "TELEGRAM_CHAT_ID")?
            } else {
                get(lookup, "TELEGRAM_CHAT_ID").unwrap_or_default()
            },
            admin_chat_ids: get(lookup, "TELEGRAM_ADMIN_CHAT_IDS")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        };

        let settings = Self {
            app: AppSettings {
                env,
                log_level: get(lookup, "LOG_LEVEL").unwrap_or_else(|| "info".into()),
                log_format: get(lookup, "LOG_FORMAT").unwrap_or_else(|| match env {
                    AppEnv::Development => "pretty".into(),
                    AppEnv::Production => "json".into(),
                }),
            },
            database: DatabaseSettings {
                url: require(lookup, "DATABASE_URL")?,
                max_connections: parse(lookup, "DATABASE_MAX_CONNECTIONS", 5u32)?,
            },
            shopee: ShopeeSettings {
                base_url: get(lookup, "SHOPEE_BASE_URL")
                    .unwrap_or_else(|| "https://shopee.vn".into()),
                profile_path: get(lookup, "SHOPEE_PROFILE_PATH")
                    .unwrap_or_else(|| "/var/lib/shopee-hunter/browser-profile".into()),
                request_timeout: millis(lookup, "SHOPEE_REQUEST_TIMEOUT_MS", 10_000)?,
                connect_timeout: millis(lookup, "SHOPEE_CONNECT_TIMEOUT_MS", 5_000)?,
            },
            session: SessionSettings {
                health_interval: secs(lookup, "SESSION_HEALTH_INTERVAL_SECS", 300)?,
                cookie_store_path: get(lookup, "SESSION_COOKIE_STORE_PATH")
                    .unwrap_or_else(|| "/var/lib/shopee-hunter/session/cookies.json".into()),
                enable_browser_refresh: parse_bool(
                    lookup,
                    "ENABLE_BROWSER_SESSION_REFRESH",
                    false,
                )?,
            },
            collectors: CollectorSettings {
                default_interval: secs(lookup, "COLLECTOR_DEFAULT_INTERVAL_SECS", 120)?,
                timeout: secs(lookup, "COLLECTOR_TIMEOUT_SECS", 30)?,
                max_concurrency: parse(lookup, "COLLECTOR_MAX_CONCURRENCY", 4usize)?,
                enable_shopee_page: parse_bool(lookup, "ENABLE_SHOPEE_PAGE_COLLECTOR", false)?,
                enable_external_feed: parse_bool(lookup, "ENABLE_EXTERNAL_FEED_COLLECTOR", false)?,
                external_feed_url: get(lookup, "EXTERNAL_FEED_URL"),
                enable_accesstrade: parse_bool(lookup, "ENABLE_ACCESSTRADE_COLLECTOR", false)?,
                accesstrade_token: get(lookup, "ACCESSTRADE_TOKEN"),
                accesstrade_merchant: get(lookup, "ACCESSTRADE_MERCHANT")
                    .unwrap_or_else(|| "shopee".into()),
                enable_manual: parse_bool(lookup, "ENABLE_MANUAL_COLLECTOR", true)?,
                enable_replay: parse_bool(lookup, "ENABLE_REPLAY_COLLECTOR", false)?,
                replay_fixture_dir: get(lookup, "REPLAY_FIXTURE_DIR")
                    .unwrap_or_else(|| "tests/fixtures/replay".into()),
                source_hourly_budget: parse(lookup, "COLLECTOR_SOURCE_HOURLY_BUDGET", 720u32)?,
            },
            scheduler: SchedulerSettings {
                coarse_tick: secs(lookup, "SCHEDULER_COARSE_TICK_SECS", 15)?,
                preflight: secs(lookup, "SCHEDULER_PREFLIGHT_SECS", 600)?,
                precision_window: secs(lookup, "SCHEDULER_PRECISION_WINDOW_SECS", 10)?,
                stale_after: secs(lookup, "SCHEDULER_STALE_AFTER_SECS", 300)?,
            },
            claim: ClaimSettings {
                enable_auto_claim: parse_bool(lookup, "ENABLE_AUTO_CLAIM", false)?,
                max_retries: parse(lookup, "CLAIM_MAX_RETRIES", 3u32)?,
                retry_base_delay: millis(lookup, "CLAIM_RETRY_BASE_DELAY_MS", 500)?,
                min_score: parse(lookup, "CLAIM_MIN_SCORE", 0i64)?,
            },
            telegram,
            observability: ObservabilitySettings {
                healthcheck_bind: parse(
                    lookup,
                    "HEALTHCHECK_BIND_ADDR",
                    "127.0.0.1:8686".parse().expect("static default addr"),
                )?,
                metrics_enabled: parse_bool(lookup, "METRICS_ENABLED", true)?,
                admin_token: get(lookup, "ADMIN_TOKEN").unwrap_or_default(),
            },
        };

        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !self.database.url.starts_with("sqlite:") && !self.database.url.starts_with("postgres:")
        {
            return Err(ConfigError::Invalid {
                key: "DATABASE_URL",
                reason: "must start with sqlite: or postgres:".into(),
            });
        }
        if self.database.max_connections == 0 {
            return Err(ConfigError::Invalid {
                key: "DATABASE_MAX_CONNECTIONS",
                reason: "must be >= 1".into(),
            });
        }
        if !self.shopee.base_url.starts_with("https://") {
            return Err(ConfigError::Invalid {
                key: "SHOPEE_BASE_URL",
                reason: "must be an https URL".into(),
            });
        }
        if self.collectors.max_concurrency == 0 {
            return Err(ConfigError::Invalid {
                key: "COLLECTOR_MAX_CONCURRENCY",
                reason: "must be >= 1".into(),
            });
        }
        if self.scheduler.precision_window >= self.scheduler.preflight {
            return Err(ConfigError::Invalid {
                key: "SCHEDULER_PRECISION_WINDOW_SECS",
                reason: "precision window must be shorter than preflight window".into(),
            });
        }
        if self.telegram.enabled && self.telegram.bot_token.contains("CHANGE_ME") {
            return Err(ConfigError::Invalid {
                key: "TELEGRAM_BOT_TOKEN",
                reason: "placeholder value while ENABLE_TELEGRAM=true".into(),
            });
        }
        if self.collectors.enable_external_feed && self.collectors.external_feed_url.is_none() {
            return Err(ConfigError::Invalid {
                key: "EXTERNAL_FEED_URL",
                reason: "required when ENABLE_EXTERNAL_FEED_COLLECTOR=true".into(),
            });
        }
        if self.collectors.enable_accesstrade
            && self
                .collectors
                .accesstrade_token
                .as_deref()
                .map(|t| t.trim().is_empty() || t.contains("CHANGE_ME"))
                .unwrap_or(true)
        {
            return Err(ConfigError::Invalid {
                key: "ACCESSTRADE_TOKEN",
                reason: "required (non-placeholder) when ENABLE_ACCESSTRADE_COLLECTOR=true".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base_env() -> HashMap<String, String> {
        HashMap::from([(
            "DATABASE_URL".to_string(),
            "sqlite://data/test.db?mode=rwc".to_string(),
        )])
    }

    fn load(map: &HashMap<String, String>) -> Result<Settings, ConfigError> {
        Settings::from_lookup(&|k| map.get(k).cloned())
    }

    #[test]
    fn minimal_env_loads_with_defaults() {
        let s = load(&base_env()).expect("should load");
        assert_eq!(s.app.env, AppEnv::Development);
        assert_eq!(s.collectors.default_interval, Duration::from_secs(120));
        assert!(!s.claim.enable_auto_claim);
        assert!(!s.telegram.enabled);
        assert_eq!(s.observability.healthcheck_bind.port(), 8686);
    }

    #[test]
    fn missing_database_url_fails() {
        let err = load(&HashMap::new()).unwrap_err();
        assert!(matches!(err, ConfigError::Missing("DATABASE_URL")));
    }

    #[test]
    fn telegram_enabled_requires_real_token() {
        let mut env = base_env();
        env.insert("ENABLE_TELEGRAM".into(), "true".into());
        env.insert("TELEGRAM_BOT_TOKEN".into(), "123:CHANGE_ME".into());
        env.insert("TELEGRAM_CHAT_ID".into(), "42".into());
        assert!(load(&env).is_err());

        env.insert("TELEGRAM_BOT_TOKEN".into(), "123:realtoken".into());
        let s = load(&env).expect("should load");
        assert!(s.telegram.enabled);
    }

    #[test]
    fn invalid_bool_and_addr_are_rejected() {
        let mut env = base_env();
        env.insert("ENABLE_AUTO_CLAIM".into(), "maybe".into());
        assert!(load(&env).is_err());

        let mut env = base_env();
        env.insert("HEALTHCHECK_BIND_ADDR".into(), "not-an-addr".into());
        assert!(load(&env).is_err());
    }

    #[test]
    fn precision_window_must_be_inside_preflight() {
        let mut env = base_env();
        env.insert("SCHEDULER_PREFLIGHT_SECS".into(), "10".into());
        env.insert("SCHEDULER_PRECISION_WINDOW_SECS".into(), "10".into());
        assert!(load(&env).is_err());
    }

    #[test]
    fn admin_chat_ids_parse_as_list() {
        let mut env = base_env();
        env.insert("TELEGRAM_ADMIN_CHAT_IDS".into(), "1, 2,3 ,".into());
        let s = load(&env).expect("should load");
        assert_eq!(s.telegram.admin_chat_ids, vec!["1", "2", "3"]);
    }

    #[test]
    fn accesstrade_enabled_requires_a_real_token() {
        let mut env = base_env();
        env.insert("ENABLE_ACCESSTRADE_COLLECTOR".into(), "true".into());
        // no token → rejected
        assert!(load(&env).is_err());
        // placeholder → rejected
        env.insert("ACCESSTRADE_TOKEN".into(), "CHANGE_ME".into());
        assert!(load(&env).is_err());
        // real token → loads, merchant defaults to shopee
        env.insert("ACCESSTRADE_TOKEN".into(), "real-token-xyz".into());
        let s = load(&env).expect("should load");
        assert!(s.collectors.enable_accesstrade);
        assert_eq!(s.collectors.accesstrade_merchant, "shopee");
        assert_eq!(
            s.collectors.accesstrade_token.as_deref(),
            Some("real-token-xyz")
        );
    }
}
