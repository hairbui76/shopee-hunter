use std::str::FromStr;
use std::time::Duration;

use sqlx::any::{AnyConnectOptions, AnyPoolOptions};
use sqlx::migrate::Migrator;
use sqlx::{AnyPool, ConnectOptions};

use crate::error::StorageError;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    Sqlite,
    Postgres,
}

/// Owns the shared connection pool. Cloneable: the pool is reference-counted.
#[derive(Clone)]
pub struct Database {
    pool: AnyPool,
    kind: DbKind,
}

impl Database {
    /// Connect using a URL (`sqlite://…` or `postgres://…`), applying
    /// migrations. Ensures SQLite parent directories and WAL mode.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, StorageError> {
        sqlx::any::install_default_drivers();

        let kind = if url.starts_with("sqlite:") {
            DbKind::Sqlite
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            DbKind::Postgres
        } else {
            return Err(StorageError::UnsupportedUrl(url.to_string()));
        };

        if kind == DbKind::Sqlite {
            ensure_sqlite_parent_dir(url);
        }

        let connect_options = AnyConnectOptions::from_str(url)
            .map_err(StorageError::Sqlx)?
            .disable_statement_logging();

        let pool = AnyPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(connect_options)
            .await?;

        if kind == DbKind::Sqlite {
            // WAL improves concurrent read/write for the single-process service.
            sqlx::query("PRAGMA journal_mode=WAL;")
                .execute(&pool)
                .await?;
            sqlx::query("PRAGMA foreign_keys=ON;")
                .execute(&pool)
                .await?;
            sqlx::query("PRAGMA busy_timeout=5000;")
                .execute(&pool)
                .await?;
        }

        MIGRATOR.run(&pool).await?;

        Ok(Self { pool, kind })
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    pub fn kind(&self) -> DbKind {
        self.kind
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

fn ensure_sqlite_parent_dir(url: &str) {
    // sqlite://data/x.db?mode=rwc  ->  data/x.db
    let path = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:")
        .split('?')
        .next()
        .unwrap_or_default();
    if path.is_empty() || path == ":memory:" {
        return;
    }
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
}
