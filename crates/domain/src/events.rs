//! Domain/application events. Serialized into the notification outbox and
//! consumed by the notifier; also drive scheduling/ranking reactions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claim::ClaimResultClass;
use crate::ids::SourceId;
use crate::session::SessionState;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DomainEvent {
    VoucherDiscovered {
        voucher_id: Uuid,
        source: SourceId,
        version_hash: String,
    },
    VoucherUpdated {
        voucher_id: Uuid,
        source: SourceId,
        version_hash: String,
        changed_fields: Vec<String>,
    },
    VoucherUpcoming {
        voucher_id: Uuid,
        starts_at: DateTime<Utc>,
    },
    ClaimSucceeded {
        voucher_id: Uuid,
        attempt_id: Uuid,
        already_saved: bool,
    },
    ClaimFailed {
        voucher_id: Uuid,
        attempt_id: Uuid,
        result_class: ClaimResultClass,
        terminal: bool,
    },
    SessionStateChanged {
        from: SessionState,
        to: SessionState,
        reason: String,
    },
    CollectorDegraded {
        source: SourceId,
        detail: String,
    },
    ServiceUnhealthy {
        service: String,
        detail: String,
    },
}

impl DomainEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::VoucherDiscovered { .. } => "voucher.discovered",
            Self::VoucherUpdated { .. } => "voucher.updated",
            Self::VoucherUpcoming { .. } => "voucher.upcoming",
            Self::ClaimSucceeded { .. } => "claim.succeeded",
            Self::ClaimFailed { .. } => "claim.failed",
            Self::SessionStateChanged { .. } => "session.state_changed",
            Self::CollectorDegraded { .. } => "collector.degraded",
            Self::ServiceUnhealthy { .. } => "service.unhealthy",
        }
    }

    /// Stable idempotency key: the same logical occurrence must produce the
    /// same key so notification delivery can deduplicate.
    pub fn idempotency_key(&self) -> String {
        match self {
            Self::VoucherDiscovered {
                voucher_id,
                version_hash,
                ..
            } => format!("voucher.discovered:{voucher_id}:{version_hash}"),
            Self::VoucherUpdated {
                voucher_id,
                version_hash,
                ..
            } => format!("voucher.updated:{voucher_id}:{version_hash}"),
            Self::VoucherUpcoming {
                voucher_id,
                starts_at,
            } => format!("voucher.upcoming:{voucher_id}:{}", starts_at.timestamp()),
            Self::ClaimSucceeded { attempt_id, .. } => format!("claim.succeeded:{attempt_id}"),
            Self::ClaimFailed { attempt_id, .. } => format!("claim.failed:{attempt_id}"),
            Self::SessionStateChanged { from, to, .. } => {
                format!("session.state_changed:{}:{}", from.as_str(), to.as_str())
            }
            Self::CollectorDegraded { source, detail } => {
                format!("collector.degraded:{source}:{detail}")
            }
            Self::ServiceUnhealthy { service, detail } => {
                format!("service.unhealthy:{service}:{detail}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_keys_are_stable_and_version_scoped() {
        let id = Uuid::new_v4();
        let a = DomainEvent::VoucherDiscovered {
            voucher_id: id,
            source: SourceId::new("feed"),
            version_hash: "v1".into(),
        };
        let b = DomainEvent::VoucherDiscovered {
            voucher_id: id,
            source: SourceId::new("other"),
            version_hash: "v1".into(),
        };
        assert_eq!(a.idempotency_key(), b.idempotency_key());

        let c = DomainEvent::VoucherUpdated {
            voucher_id: id,
            source: SourceId::new("feed"),
            version_hash: "v2".into(),
            changed_fields: vec![],
        };
        assert_ne!(a.idempotency_key(), c.idempotency_key());
    }

    #[test]
    fn events_serialize_round_trip() {
        let e = DomainEvent::ClaimFailed {
            voucher_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            result_class: ClaimResultClass::RateLimited,
            terminal: false,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
