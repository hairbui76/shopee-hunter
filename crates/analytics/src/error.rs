//! Analytics error type.

use shopee_hunter_storage::StorageError;

/// Anything that can go wrong while computing source analytics.
///
/// Analytics is read-only and advisory: a failure here must degrade the
/// operator's visibility, never the watcher's ability to collect or claim.
#[derive(Debug, thiserror::Error)]
pub enum AnalyticsError {
    /// A database or decode failure from the storage layer.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// A persisted value could not be interpreted.
    #[error("analytics decode error in {field}: {reason}")]
    Decode {
        /// Column that could not be read.
        field: &'static str,
        /// What went wrong.
        reason: String,
    },
}

impl From<sqlx::Error> for AnalyticsError {
    fn from(err: sqlx::Error) -> Self {
        Self::Storage(StorageError::Sqlx(err))
    }
}
