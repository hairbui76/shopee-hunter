//! Voucher persistence: transaction-safe upsert with deterministic dedup,
//! observation recording, and version tracking.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::identity::IdentityBasis;
use shopee_hunter_domain::voucher::{Voucher, VoucherCandidate, VoucherStatus};
use shopee_hunter_domain::{SourceId, VoucherType};
use sqlx::Row;
use uuid::Uuid;

use crate::convert::*;
use crate::db::Database;
use crate::error::StorageError;

/// Result of ingesting a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// New logical voucher created.
    Created { voucher_id: Uuid },
    /// Existing voucher, meaningful change (new version hash).
    Updated {
        voucher_id: Uuid,
        changed_fields: Vec<String>,
    },
    /// Existing voucher, no meaningful change (only last_seen refreshed).
    Unchanged { voucher_id: Uuid },
}

impl UpsertOutcome {
    pub fn voucher_id(&self) -> Uuid {
        match self {
            Self::Created { voucher_id }
            | Self::Updated { voucher_id, .. }
            | Self::Unchanged { voucher_id } => *voucher_id,
        }
    }
}

pub struct VoucherRepository<'a> {
    db: &'a Database,
}

impl<'a> VoucherRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Ingest a candidate atomically: create/update the logical voucher,
    /// record the observation, and track new versions. Dedup is by identity
    /// key (unique constraint), so concurrent ingestion is transaction-safe.
    pub async fn upsert_candidate(
        &self,
        candidate: &VoucherCandidate,
        now: DateTime<Utc>,
    ) -> Result<UpsertOutcome, StorageError> {
        let identity = shopee_hunter_domain::identity::compute_identity(candidate);
        let version_hash = shopee_hunter_domain::identity::version_hash(candidate);
        let raw_hash = shopee_hunter_domain::identity::raw_hash(&candidate.raw_payload);

        let mut tx = self.db.pool().begin().await?;

        let existing = sqlx::query("SELECT id, version_hash FROM vouchers WHERE identity_key = ?")
            .bind(&identity.key)
            .fetch_optional(&mut *tx)
            .await?;

        let outcome = match existing {
            None => {
                let voucher = Voucher::from_candidate(candidate, now);
                insert_voucher(&mut tx, &voucher).await?;
                insert_version(
                    &mut tx,
                    voucher.id,
                    &version_hash,
                    &["created".to_string()],
                    now,
                )
                .await?;
                UpsertOutcome::Created {
                    voucher_id: voucher.id,
                }
            }
            Some(row) => {
                let id = str_to_uuid(row.get::<String, _>("id").as_str(), "vouchers.id")?;
                let old_version: String = row.get("version_hash");
                if old_version == version_hash {
                    sqlx::query("UPDATE vouchers SET last_seen_at = ? WHERE id = ?")
                        .bind(ts_to_str(now))
                        .bind(uuid_to_str(id))
                        .execute(&mut *tx)
                        .await?;
                    UpsertOutcome::Unchanged { voucher_id: id }
                } else {
                    let changed = changed_fields(&mut tx, id, candidate).await?;
                    update_voucher_fields(&mut tx, id, candidate, &version_hash, &raw_hash, now)
                        .await?;
                    insert_version(&mut tx, id, &version_hash, &changed, now).await?;
                    UpsertOutcome::Updated {
                        voucher_id: id,
                        changed_fields: changed,
                    }
                }
            }
        };

        // Record the observation (idempotent on the unique triple).
        let obs_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO voucher_observations
                (id, voucher_id, source, source_key, observed_at, source_updated_at,
                 raw_hash, normalized_hash, raw_payload, parser_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source, source_key, normalized_hash) DO NOTHING",
        )
        .bind(uuid_to_str(obs_id))
        .bind(uuid_to_str(outcome.voucher_id()))
        .bind(candidate.source.as_str())
        .bind(&candidate.source_key)
        .bind(ts_to_str(candidate.observed_at))
        .bind(Option::<String>::None)
        .bind(&raw_hash)
        .bind(&version_hash)
        .bind(candidate.raw_payload.to_string())
        .bind(&candidate.parser_version)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Voucher>, StorageError> {
        let row = sqlx::query("SELECT * FROM vouchers WHERE id = ?")
            .bind(uuid_to_str(id))
            .fetch_optional(self.db.pool())
            .await?;
        row.map(|r| row_to_voucher(&r)).transpose()
    }

    pub async fn find_by_identity(&self, key: &str) -> Result<Option<Voucher>, StorageError> {
        let row = sqlx::query("SELECT * FROM vouchers WHERE identity_key = ?")
            .bind(key)
            .fetch_optional(self.db.pool())
            .await?;
        row.map(|r| row_to_voucher(&r)).transpose()
    }

    pub async fn count(&self) -> Result<i64, StorageError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM vouchers")
            .fetch_one(self.db.pool())
            .await?;
        Ok(row.get::<i64, _>("n"))
    }

    pub async fn set_status(&self, id: Uuid, status: VoucherStatus) -> Result<(), StorageError> {
        sqlx::query("UPDATE vouchers SET status = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(uuid_to_str(id))
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}

type Tx<'t> = sqlx::Transaction<'t, sqlx::Any>;

async fn insert_voucher(tx: &mut Tx<'_>, v: &Voucher) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO vouchers
            (id, identity_key, identity_basis, source, source_key, code, promotion_id,
             signature, title, description, voucher_type, discount_type, discount_amount,
             discount_percent, max_discount, min_spend, start_at, end_at, scope,
             payment_method, landing_url, status, first_seen_at, last_seen_at,
             version_hash, raw_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid_to_str(v.id))
    .bind(&v.identity.key)
    .bind(v.identity.basis.as_str())
    .bind(v.source.as_str())
    .bind(&v.source_key)
    .bind(&v.code)
    .bind(&v.promotion_id)
    .bind(&v.signature)
    .bind(&v.title)
    .bind(&v.description)
    .bind(v.voucher_type.as_str())
    .bind(v.discount_type.map(|d| format!("{d:?}")))
    .bind(dec_to_str(v.discount_amount))
    .bind(dec_to_str(v.discount_percent))
    .bind(dec_to_str(v.max_discount))
    .bind(dec_to_str(v.min_spend))
    .bind(opt_ts_to_str(v.start_at))
    .bind(opt_ts_to_str(v.end_at))
    .bind(
        v.scope
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default()),
    )
    .bind(&v.payment_method)
    .bind(&v.landing_url)
    .bind(v.status.as_str())
    .bind(ts_to_str(v.first_seen_at))
    .bind(ts_to_str(v.last_seen_at))
    .bind(&v.version_hash)
    .bind(&v.raw_hash)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_voucher_fields(
    tx: &mut Tx<'_>,
    id: Uuid,
    c: &VoucherCandidate,
    version_hash: &str,
    raw_hash: &str,
    now: DateTime<Utc>,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE vouchers SET
            code = ?, promotion_id = ?, signature = ?, title = ?, description = ?,
            voucher_type = ?, discount_type = ?, discount_amount = ?, discount_percent = ?,
            max_discount = ?, min_spend = ?, start_at = ?, end_at = ?, scope = ?,
            payment_method = ?, landing_url = ?, last_seen_at = ?, version_hash = ?, raw_hash = ?
         WHERE id = ?",
    )
    .bind(&c.code)
    .bind(&c.promotion_id)
    .bind(&c.signature)
    .bind(&c.title)
    .bind(&c.description)
    .bind(c.voucher_type.as_str())
    .bind(c.discount_type.map(|d| format!("{d:?}")))
    .bind(dec_to_str(c.discount_amount))
    .bind(dec_to_str(c.discount_percent))
    .bind(dec_to_str(c.max_discount))
    .bind(dec_to_str(c.min_spend))
    .bind(opt_ts_to_str(c.start_at))
    .bind(opt_ts_to_str(c.end_at))
    .bind(
        c.scope
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default()),
    )
    .bind(&c.payment_method)
    .bind(&c.landing_url)
    .bind(ts_to_str(now))
    .bind(version_hash)
    .bind(raw_hash)
    .bind(uuid_to_str(id))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_version(
    tx: &mut Tx<'_>,
    voucher_id: Uuid,
    version_hash: &str,
    changed: &[String],
    now: DateTime<Utc>,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO voucher_versions (id, voucher_id, version_hash, changed_fields, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(voucher_id, version_hash) DO NOTHING",
    )
    .bind(uuid_to_str(Uuid::new_v4()))
    .bind(uuid_to_str(voucher_id))
    .bind(version_hash)
    .bind(serde_json::to_string(changed).unwrap_or_else(|_| "[]".into()))
    .bind(ts_to_str(now))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Compute which meaningful fields differ between stored voucher and candidate.
async fn changed_fields(
    tx: &mut Tx<'_>,
    id: Uuid,
    c: &VoucherCandidate,
) -> Result<Vec<String>, StorageError> {
    let row = sqlx::query("SELECT * FROM vouchers WHERE id = ?")
        .bind(uuid_to_str(id))
        .fetch_one(&mut **tx)
        .await?;
    let old = row_to_voucher(&row)?;
    let mut changed = Vec::new();
    if old.start_at != c.start_at {
        changed.push("start_at".into());
    }
    if old.end_at != c.end_at {
        changed.push("end_at".into());
    }
    if old.min_spend != c.min_spend {
        changed.push("min_spend".into());
    }
    if old.discount_amount != c.discount_amount {
        changed.push("discount_amount".into());
    }
    if old.max_discount != c.max_discount {
        changed.push("max_discount".into());
    }
    if old.scope != c.scope {
        changed.push("scope".into());
    }
    if old.signature != c.signature {
        changed.push("signature".into());
    }
    if changed.is_empty() {
        changed.push("other".into());
    }
    Ok(changed)
}

fn get_opt(row: &sqlx::any::AnyRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col).ok().flatten()
}

fn row_to_voucher(row: &sqlx::any::AnyRow) -> Result<Voucher, StorageError> {
    let scope = get_opt(row, "scope")
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| StorageError::Decode {
            field: "vouchers.scope",
            reason: e.to_string(),
        })?;
    let basis = IdentityBasis::parse(&row.get::<String, _>("identity_basis")).ok_or(
        StorageError::Decode {
            field: "vouchers.identity_basis",
            reason: "unknown basis".into(),
        },
    )?;
    let status =
        VoucherStatus::parse(&row.get::<String, _>("status")).ok_or(StorageError::Decode {
            field: "vouchers.status",
            reason: "unknown status".into(),
        })?;
    let discount_type = parse_discount_type(row);

    Ok(Voucher {
        id: str_to_uuid(&row.get::<String, _>("id"), "vouchers.id")?,
        identity: shopee_hunter_domain::identity::VoucherIdentity {
            key: row.get("identity_key"),
            basis,
        },
        source: SourceId::new(row.get::<String, _>("source")),
        source_key: row.get("source_key"),
        code: get_opt(row, "code"),
        promotion_id: get_opt(row, "promotion_id"),
        signature: get_opt(row, "signature"),
        title: row.get("title"),
        description: get_opt(row, "description"),
        voucher_type: VoucherType::parse(&row.get::<String, _>("voucher_type")),
        discount_type,
        discount_amount: str_to_dec(get_opt(row, "discount_amount"), "vouchers.discount_amount")?,
        discount_percent: str_to_dec(
            get_opt(row, "discount_percent"),
            "vouchers.discount_percent",
        )?,
        max_discount: str_to_dec(get_opt(row, "max_discount"), "vouchers.max_discount")?,
        min_spend: str_to_dec(get_opt(row, "min_spend"), "vouchers.min_spend")?,
        start_at: opt_str_to_ts(get_opt(row, "start_at"), "vouchers.start_at")?,
        end_at: opt_str_to_ts(get_opt(row, "end_at"), "vouchers.end_at")?,
        scope,
        payment_method: get_opt(row, "payment_method"),
        landing_url: get_opt(row, "landing_url"),
        status,
        first_seen_at: str_to_ts(
            &row.get::<String, _>("first_seen_at"),
            "vouchers.first_seen_at",
        )?,
        last_seen_at: str_to_ts(
            &row.get::<String, _>("last_seen_at"),
            "vouchers.last_seen_at",
        )?,
        version_hash: row.get("version_hash"),
        raw_hash: row.get("raw_hash"),
    })
}

fn parse_discount_type(row: &sqlx::any::AnyRow) -> Option<shopee_hunter_domain::DiscountType> {
    use shopee_hunter_domain::DiscountType::*;
    match get_opt(row, "discount_type")?.as_str() {
        "FixedAmount" => Some(FixedAmount),
        "Percentage" => Some(Percentage),
        "Coins" => Some(Coins),
        "FreeShipping" => Some(FreeShipping),
        _ => None,
    }
}
