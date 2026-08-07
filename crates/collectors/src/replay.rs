//! Development/test collector that replays recorded JSON fixtures through the
//! real normalization pipeline. Lets fixture data create real voucher records
//! without contacting any live source.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use shopee_hunter_domain::voucher::VoucherCandidate;
use shopee_hunter_domain::SourceId;

use crate::contract::{
    CollectionContext, CollectionResult, CollectorError, PartialFailure, VoucherCollector,
};

/// Replays candidates either from an in-memory list or from a fixtures dir of
/// `*.json` files (each a single `VoucherCandidate` or an array of them).
pub struct ReplayCollector {
    name: String,
    fixtures_dir: Option<PathBuf>,
    inline: Vec<VoucherCandidate>,
}

impl ReplayCollector {
    pub fn from_candidates(name: impl Into<String>, candidates: Vec<VoucherCandidate>) -> Self {
        Self {
            name: name.into(),
            fixtures_dir: None,
            inline: candidates,
        }
    }

    pub fn from_dir(name: impl Into<String>, dir: impl AsRef<Path>) -> Self {
        Self {
            name: name.into(),
            fixtures_dir: Some(dir.as_ref().to_path_buf()),
            inline: Vec::new(),
        }
    }

    fn load_dir(
        &self,
        dir: &Path,
    ) -> Result<(Vec<VoucherCandidate>, Vec<PartialFailure>), CollectorError> {
        let mut candidates = Vec::new();
        let mut failures = Vec::new();
        let entries = std::fs::read_dir(dir)
            .map_err(|e| CollectorError::Config(format!("read fixtures dir {dir:?}: {e}")))?;
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        paths.sort();

        for path in paths {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    failures.push(PartialFailure {
                        source_key: Some(path.display().to_string()),
                        reason: format!("read error: {e}"),
                    });
                    continue;
                }
            };
            // Accept either a single candidate or an array.
            match serde_json::from_str::<Vec<VoucherCandidate>>(&text) {
                Ok(mut batch) => candidates.append(&mut batch),
                Err(_) => match serde_json::from_str::<VoucherCandidate>(&text) {
                    Ok(one) => candidates.push(one),
                    Err(e) => failures.push(PartialFailure {
                        source_key: Some(path.display().to_string()),
                        reason: format!("parse error: {e}"),
                    }),
                },
            }
        }
        Ok((candidates, failures))
    }
}

#[async_trait]
impl VoucherCollector for ReplayCollector {
    fn name(&self) -> &str {
        &self.name
    }

    fn source_id(&self) -> SourceId {
        SourceId::new(&self.name)
    }

    async fn collect(
        &self,
        context: &CollectionContext,
    ) -> Result<CollectionResult, CollectorError> {
        let (candidates, partial_failures) = match &self.fixtures_dir {
            Some(dir) => self.load_dir(dir)?,
            None => (self.inline.clone(), Vec::new()),
        };
        Ok(CollectionResult {
            candidates,
            fetched_at: Some(context.now),
            source_latency: Some(std::time::Duration::from_millis(0)),
            partial_failures,
            rate_limit: None,
        })
    }
}
