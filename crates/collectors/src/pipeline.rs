//! Normalization → identity → persistence pipeline shared by all collectors.
//!
//! This is the discovery→alert hot path: it validates, upserts, and returns
//! the domain events that must be enqueued for notification, keeping heavy
//! work off the caller's critical section.

use chrono::{DateTime, Utc};
use shopee_hunter_domain::events::DomainEvent;
use shopee_hunter_domain::validation::validate_candidate;
use shopee_hunter_domain::voucher::VoucherCandidate;
use shopee_hunter_storage::{Database, UpsertOutcome, VoucherRepository};

use crate::contract::PartialFailure;

#[derive(Debug, Default, Clone)]
pub struct PipelineOutcome {
    pub new_count: usize,
    pub updated_count: usize,
    pub unchanged_count: usize,
    pub rejected: Vec<PartialFailure>,
    /// Events to enqueue for notification (discovered/updated).
    pub events: Vec<DomainEvent>,
}

/// Validate and persist a batch of candidates, returning the events that
/// downstream code should enqueue. Invalid candidates are rejected (recorded)
/// rather than failing the whole batch, so one bad item cannot block others.
pub async fn ingest_candidates(
    db: &Database,
    candidates: &[VoucherCandidate],
    now: DateTime<Utc>,
) -> Result<PipelineOutcome, shopee_hunter_storage::StorageError> {
    let repo = VoucherRepository::new(db);
    let mut outcome = PipelineOutcome::default();

    for candidate in candidates {
        if let Err(issues) = validate_candidate(candidate) {
            outcome.rejected.push(PartialFailure {
                source_key: Some(candidate.source_key.clone()),
                reason: issues
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            });
            continue;
        }

        match repo.upsert_candidate(candidate, now).await? {
            UpsertOutcome::Created { voucher_id } => {
                outcome.new_count += 1;
                outcome.events.push(DomainEvent::VoucherDiscovered {
                    voucher_id,
                    source: candidate.source.clone(),
                    version_hash: shopee_hunter_domain::identity::version_hash(candidate),
                });
            }
            UpsertOutcome::Updated {
                voucher_id,
                changed_fields,
            } => {
                outcome.updated_count += 1;
                outcome.events.push(DomainEvent::VoucherUpdated {
                    voucher_id,
                    source: candidate.source.clone(),
                    version_hash: shopee_hunter_domain::identity::version_hash(candidate),
                    changed_fields,
                });
            }
            UpsertOutcome::Unchanged { .. } => {
                outcome.unchanged_count += 1;
            }
        }
    }

    Ok(outcome)
}

/// Convenience alias for callers that just need a UTC-now ingest.
pub async fn ingest_now(
    db: &Database,
    candidates: &[VoucherCandidate],
) -> Result<PipelineOutcome, shopee_hunter_storage::StorageError> {
    ingest_candidates(db, candidates, Utc::now()).await
}
