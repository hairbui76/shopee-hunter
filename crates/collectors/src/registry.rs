//! Registry of enabled collectors.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::contract::VoucherCollector;

#[derive(Default)]
pub struct CollectorRegistry {
    collectors: BTreeMap<String, Arc<dyn VoucherCollector>>,
}

impl CollectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a collector. Later registration with the same name replaces
    /// the earlier one (last-wins), which keeps composition-root wiring simple.
    pub fn register(&mut self, collector: Arc<dyn VoucherCollector>) {
        self.collectors
            .insert(collector.name().to_string(), collector);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn VoucherCollector>> {
        self.collectors.get(name).cloned()
    }

    pub fn all(&self) -> Vec<Arc<dyn VoucherCollector>> {
        self.collectors.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.collectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }
}
