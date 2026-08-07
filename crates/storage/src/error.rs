use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("unsupported database URL: {0}")]
    UnsupportedUrl(String),
    #[error("data decode error in {field}: {reason}")]
    Decode { field: &'static str, reason: String },
    #[error("not found: {0}")]
    NotFound(String),
}
