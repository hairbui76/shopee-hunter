//! Minimal in-process metrics registry with Prometheus text exposition.
//!
//! Deliberately dependency-free: counters, gauges, and fixed-bucket latency
//! histograms cover every metric the roadmap requires. If the project ever
//! needs more, swap this module for a metrics facade behind the same calls.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Default latency buckets in milliseconds.
const DEFAULT_BUCKETS_MS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];

#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    counters: RwLock<BTreeMap<String, Arc<AtomicU64>>>,
    gauges: RwLock<BTreeMap<String, Arc<AtomicI64>>>,
    histograms: RwLock<BTreeMap<String, Arc<Histogram>>>,
}

pub struct Histogram {
    buckets: Vec<f64>,
    counts: Vec<AtomicU64>,
    sum_micros: AtomicU64,
    total: AtomicU64,
}

impl Histogram {
    fn new(buckets: &[f64]) -> Self {
        Self {
            buckets: buckets.to_vec(),
            counts: buckets.iter().map(|_| AtomicU64::new(0)).collect(),
            sum_micros: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }

    pub fn observe_ms(&self, value_ms: f64) {
        for (i, bound) in self.buckets.iter().enumerate() {
            if value_ms <= *bound {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.total.fetch_add(1, Ordering::Relaxed);
        self.sum_micros
            .fetch_add((value_ms * 1000.0).max(0.0) as u64, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
}

/// Build the metric storage key: `name` or `name{k="v",...}` with sorted labels.
fn key(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let mut sorted: Vec<_> = labels.to_vec();
    sorted.sort_unstable();
    let mut out = String::with_capacity(name.len() + 16 * sorted.len());
    out.push_str(name);
    out.push('{');
    for (i, (k, v)) in sorted.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{k}=\"{}\"",
            v.replace('\\', "\\\\").replace('"', "\\\"")
        );
    }
    out.push('}');
    out
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counter(&self, name: &str, labels: &[(&str, &str)]) -> Arc<AtomicU64> {
        let k = key(name, labels);
        if let Some(c) = self.inner.counters.read().expect("metrics lock").get(&k) {
            return Arc::clone(c);
        }
        let mut map = self.inner.counters.write().expect("metrics lock");
        Arc::clone(map.entry(k).or_insert_with(|| Arc::new(AtomicU64::new(0))))
    }

    pub fn inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.counter(name, labels).fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(&self, name: &str, labels: &[(&str, &str)], delta: u64) {
        self.counter(name, labels)
            .fetch_add(delta, Ordering::Relaxed);
    }

    pub fn gauge(&self, name: &str, labels: &[(&str, &str)]) -> Arc<AtomicI64> {
        let k = key(name, labels);
        if let Some(g) = self.inner.gauges.read().expect("metrics lock").get(&k) {
            return Arc::clone(g);
        }
        let mut map = self.inner.gauges.write().expect("metrics lock");
        Arc::clone(map.entry(k).or_insert_with(|| Arc::new(AtomicI64::new(0))))
    }

    pub fn set_gauge(&self, name: &str, labels: &[(&str, &str)], value: i64) {
        self.gauge(name, labels).store(value, Ordering::Relaxed);
    }

    pub fn histogram(&self, name: &str, labels: &[(&str, &str)]) -> Arc<Histogram> {
        let k = key(name, labels);
        if let Some(h) = self.inner.histograms.read().expect("metrics lock").get(&k) {
            return Arc::clone(h);
        }
        let mut map = self.inner.histograms.write().expect("metrics lock");
        Arc::clone(
            map.entry(k)
                .or_insert_with(|| Arc::new(Histogram::new(DEFAULT_BUCKETS_MS))),
        )
    }

    pub fn observe_ms(&self, name: &str, labels: &[(&str, &str)], value_ms: f64) {
        self.histogram(name, labels).observe_ms(value_ms);
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        for (k, v) in self.inner.counters.read().expect("metrics lock").iter() {
            let _ = writeln!(out, "{k} {}", v.load(Ordering::Relaxed));
        }
        for (k, v) in self.inner.gauges.read().expect("metrics lock").iter() {
            let _ = writeln!(out, "{k} {}", v.load(Ordering::Relaxed));
        }
        for (k, h) in self.inner.histograms.read().expect("metrics lock").iter() {
            let (name, labels) = match k.split_once('{') {
                Some((n, rest)) => (n, format!(",{}", rest.trim_end_matches('}'))),
                None => (k.as_str(), String::new()),
            };
            for (i, bound) in h.buckets.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{name}_bucket{{le=\"{bound}\"{labels}}} {}",
                    h.counts[i].load(Ordering::Relaxed)
                );
            }
            let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"{labels}}} {}", h.count());
            let _ = writeln!(
                out,
                "{name}_sum{} {}",
                if labels.is_empty() {
                    String::new()
                } else {
                    format!("{{{}}}", labels.trim_start_matches(','))
                },
                h.sum_micros.load(Ordering::Relaxed) as f64 / 1000.0
            );
            let _ = writeln!(
                out,
                "{name}_count{} {}",
                if labels.is_empty() {
                    String::new()
                } else {
                    format!("{{{}}}", labels.trim_start_matches(','))
                },
                h.count()
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_gauges_render() {
        let m = Metrics::new();
        m.inc("collector_runs_total", &[("source", "replay")]);
        m.inc("collector_runs_total", &[("source", "replay")]);
        m.set_gauge("pending_jobs", &[], 7);

        let text = m.render_prometheus();
        assert!(text.contains("collector_runs_total{source=\"replay\"} 2"));
        assert!(text.contains("pending_jobs 7"));
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let m = Metrics::new();
        m.observe_ms("request_latency_ms", &[], 3.0);
        m.observe_ms("request_latency_ms", &[], 40.0);
        m.observe_ms("request_latency_ms", &[], 9999.0);

        let h = m.histogram("request_latency_ms", &[]);
        assert_eq!(h.count(), 3);
        let text = m.render_prometheus();
        assert!(text.contains("request_latency_ms_bucket{le=\"5\"} 1"));
        assert!(text.contains("request_latency_ms_bucket{le=\"50\"} 2"));
        assert!(text.contains("request_latency_ms_bucket{le=\"+Inf\"} 3"));
        assert!(text.contains("request_latency_ms_count 3"));
    }

    #[test]
    fn same_labels_different_order_share_a_series() {
        let m = Metrics::new();
        m.inc("x_total", &[("a", "1"), ("b", "2")]);
        m.inc("x_total", &[("b", "2"), ("a", "1")]);
        let text = m.render_prometheus();
        assert!(text.contains("x_total{a=\"1\",b=\"2\"} 2"));
    }
}
