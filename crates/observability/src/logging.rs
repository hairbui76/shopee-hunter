//! Global tracing subscriber setup.

use thiserror::Error;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = LoggingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "pretty" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            other => Err(LoggingError::InvalidFormat(other.to_string())),
        }
    }
}

#[derive(Debug, Error)]
pub enum LoggingError {
    #[error("invalid log format {0:?} (expected \"pretty\" or \"json\")")]
    InvalidFormat(String),
    #[error("invalid log filter {0:?}: {1}")]
    InvalidFilter(String, String),
    #[error("global subscriber already installed")]
    AlreadyInstalled,
}

/// Install the global subscriber. `level` is an env-filter expression such as
/// `info` or `info,shopee_hunter_collectors=debug`; `RUST_LOG` overrides it.
pub fn init(level: &str, format: LogFormat) -> Result<(), LoggingError> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(v) => EnvFilter::try_new(v),
        Err(_) => EnvFilter::try_new(level),
    }
    .map_err(|e| LoggingError::InvalidFilter(level.to_string(), e.to_string()))?;

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true);

    let result = match format {
        LogFormat::Pretty => builder.try_init(),
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
    };
    result.map_err(|_| LoggingError::AlreadyInstalled)
}
