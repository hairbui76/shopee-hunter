//! Strongly typed identifiers.

use serde::{Deserialize, Serialize};

/// Identifier of a discovery source (collector), e.g. `"shopee-page"`,
/// `"external-feed"`, `"manual"`, `"replay"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SourceId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}
