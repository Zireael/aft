//! Local context savings and enrichment observability ledger.
//!
//! Records local-only metrics for enrichment, compression, cache hits, output size,
//! and stale/degraded states. No remote telemetry is introduced.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Global metrics ledger instance (lazy-initialized).
static METRICS_LEDGER: OnceLock<MetricsLedger> = OnceLock::new();

/// Whether metrics collection is enabled.
static METRICS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Get or initialize the global metrics ledger.
pub fn ledger() -> &'static MetricsLedger {
    METRICS_LEDGER.get_or_init(MetricsLedger::new)
}

/// Enable or disable metrics collection.
pub fn set_metrics_enabled(enabled: bool) {
    METRICS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if metrics collection is enabled.
pub fn is_metrics_enabled() -> bool {
    METRICS_ENABLED.load(Ordering::Relaxed)
}

/// Context savings and enrichment metrics.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ContextMetrics {
    /// Tool name (e.g., "read", "grep", "aft_search").
    pub tool: String,
    /// Exact content result size in characters.
    pub result_chars: u64,
    /// Sidecar size in characters (outline, imports, tests).
    pub sidecar_chars: u64,
    /// Full-file chars avoided (when using ranged reads or snippets).
    pub chars_avoided: u64,
    /// Snippet chars returned (when using zoom or outline).
    pub snippet_chars: u64,
    /// Cache hits (when result was served from cache).
    pub cache_hits: u32,
    /// Cache misses (when result had to be computed).
    pub cache_misses: u32,
    /// Enrichment latency in microseconds.
    pub enrichment_latency_us: u64,
    /// Stale results dropped (when cache was invalidated).
    pub stale_dropped: u32,
    /// Compression ratio (output_size / input_size, 0.0-1.0).
    pub compression_ratio: f64,
    /// Timeout events.
    pub timeouts: u32,
    /// Circuit breaker trips.
    pub circuit_breaker_trips: u32,
}

/// Aggregated metrics for a reporting period.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AggregatedMetrics {
    /// Total tool invocations.
    pub total_invocations: u64,
    /// Total result characters.
    pub total_result_chars: u64,
    /// Total sidecar characters.
    pub total_sidecar_chars: u64,
    /// Total characters avoided.
    pub total_chars_avoided: u64,
    /// Total snippet characters.
    pub total_snippet_chars: u64,
    /// Total cache hits.
    pub total_cache_hits: u64,
    /// Total cache misses.
    pub total_cache_misses: u64,
    /// Average enrichment latency in microseconds.
    pub avg_enrichment_latency_us: f64,
    /// Total stale results dropped.
    pub total_stale_dropped: u64,
    /// Average compression ratio.
    pub avg_compression_ratio: f64,
    /// Total timeouts.
    pub total_timeouts: u64,
    /// Total circuit breaker trips.
    pub total_circuit_breaker_trips: u64,
    /// Context savings ratio (chars_avoided / (result_chars + chars_avoided)).
    pub context_savings_ratio: f64,
    /// Per-tool breakdown.
    pub per_tool: BTreeMap<String, ContextMetrics>,
}

/// Local metrics ledger for context savings and enrichment observability.
pub struct MetricsLedger {
    /// Whether metrics are enabled.
    enabled: AtomicBool,
    /// Total invocations.
    total_invocations: AtomicU64,
    /// Per-tool accumulated metrics.
    per_tool: std::sync::Mutex<BTreeMap<String, ContextMetrics>>,
}

impl MetricsLedger {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            total_invocations: AtomicU64::new(0),
            per_tool: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// Check if metrics collection is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) || is_metrics_enabled()
    }

    /// Enable or disable metrics collection.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        set_metrics_enabled(enabled);
    }

    /// Record a single tool invocation's metrics.
    pub fn record(&self, metrics: ContextMetrics) {
        if !self.is_enabled() {
            return;
        }

        self.total_invocations.fetch_add(1, Ordering::Relaxed);

        let mut per_tool = self.per_tool.lock().unwrap();
        let entry = per_tool.entry(metrics.tool.clone()).or_default();
        entry.result_chars += metrics.result_chars;
        entry.sidecar_chars += metrics.sidecar_chars;
        entry.chars_avoided += metrics.chars_avoided;
        entry.snippet_chars += metrics.snippet_chars;
        entry.cache_hits += metrics.cache_hits;
        entry.cache_misses += metrics.cache_misses;
        entry.enrichment_latency_us += metrics.enrichment_latency_us;
        entry.stale_dropped += metrics.stale_dropped;
        entry.compression_ratio += metrics.compression_ratio;
        entry.timeouts += metrics.timeouts;
        entry.circuit_breaker_trips += metrics.circuit_breaker_trips;
    }

    /// Get aggregated metrics for a reporting period.
    pub fn report(&self) -> AggregatedMetrics {
        let total_invocations = self.total_invocations.load(Ordering::Relaxed);
        let per_tool = self.per_tool.lock().unwrap().clone();

        let mut total_result_chars = 0u64;
        let mut total_sidecar_chars = 0u64;
        let mut total_chars_avoided = 0u64;
        let mut total_snippet_chars = 0u64;
        let mut total_cache_hits = 0u64;
        let mut total_cache_misses = 0u64;
        let mut total_enrichment_latency = 0u64;
        let mut total_stale_dropped = 0u64;
        let mut total_compression_ratio = 0.0;
        let mut total_timeouts = 0u64;
        let mut total_circuit_breaker_trips = 0u64;
        let mut tool_count = 0u64;

        for metrics in per_tool.values() {
            total_result_chars += metrics.result_chars;
            total_sidecar_chars += metrics.sidecar_chars;
            total_chars_avoided += metrics.chars_avoided;
            total_snippet_chars += metrics.snippet_chars;
            total_cache_hits += metrics.cache_hits as u64;
            total_cache_misses += metrics.cache_misses as u64;
            total_enrichment_latency += metrics.enrichment_latency_us;
            total_stale_dropped += metrics.stale_dropped as u64;
            total_compression_ratio += metrics.compression_ratio;
            total_timeouts += metrics.timeouts as u64;
            total_circuit_breaker_trips += metrics.circuit_breaker_trips as u64;
            tool_count += 1;
        }

        let avg_enrichment_latency = if total_invocations > 0 {
            total_enrichment_latency as f64 / total_invocations as f64
        } else {
            0.0
        };

        let avg_compression_ratio = if tool_count > 0 {
            total_compression_ratio / tool_count as f64
        } else {
            1.0
        };

        let context_savings_ratio = if total_result_chars + total_chars_avoided > 0 {
            total_chars_avoided as f64 / (total_result_chars + total_chars_avoided) as f64
        } else {
            0.0
        };

        AggregatedMetrics {
            total_invocations,
            total_result_chars,
            total_sidecar_chars,
            total_chars_avoided,
            total_snippet_chars,
            total_cache_hits,
            total_cache_misses,
            avg_enrichment_latency_us: avg_enrichment_latency,
            total_stale_dropped,
            avg_compression_ratio,
            total_timeouts,
            total_circuit_breaker_trips,
            context_savings_ratio,
            per_tool,
        }
    }

    /// Reset all metrics.
    pub fn reset(&self) {
        self.total_invocations.store(0, Ordering::Relaxed);
        self.per_tool.lock().unwrap().clear();
    }
}

/// A helper timer for measuring enrichment latency.
pub struct EnrichmentTimer {
    start: Instant,
    tool: String,
}

impl EnrichmentTimer {
    /// Start timing an enrichment operation.
    pub fn start(tool: &str) -> Self {
        Self {
            start: Instant::now(),
            tool: tool.to_string(),
        }
    }

    /// Finish timing and record the metric.
    pub fn finish(self, mut metrics: ContextMetrics) {
        metrics.enrichment_latency_us = self.start.elapsed().as_micros() as u64;
        metrics.tool = self.tool;
        ledger().record(metrics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_initializes() {
        let l = ledger();
        assert!(!l.is_enabled());
    }

    #[test]
    fn enable_disable_metrics() {
        let l = ledger();
        l.set_enabled(true);
        assert!(l.is_enabled());
        l.set_enabled(false);
        assert!(!l.is_enabled());
    }

    #[test]
    fn record_without_enable_does_nothing() {
        let l = ledger();
        l.set_enabled(false);
        l.record(ContextMetrics {
            tool: "test".to_string(),
            result_chars: 100,
            ..Default::default()
        });
        let report = l.report();
        assert_eq!(report.total_invocations, 0);
    }

    #[test]
    fn record_with_enable_accumulates() {
        let l = ledger();
        l.set_enabled(true);
        l.reset();

        l.record(ContextMetrics {
            tool: "read".to_string(),
            result_chars: 500,
            sidecar_chars: 50,
            chars_avoided: 200,
            cache_hits: 1,
            ..Default::default()
        });

        l.record(ContextMetrics {
            tool: "read".to_string(),
            result_chars: 300,
            chars_avoided: 100,
            ..Default::default()
        });

        let report = l.report();
        assert_eq!(report.total_invocations, 2);
        assert_eq!(report.total_result_chars, 800);
        assert_eq!(report.total_sidecar_chars, 50);
        assert_eq!(report.total_chars_avoided, 300);
        assert!(report.context_savings_ratio > 0.0);
    }

    #[test]
    fn context_savings_ratio_computed() {
        let l = ledger();
        l.set_enabled(true);
        l.reset();

        l.record(ContextMetrics {
            tool: "test".to_string(),
            result_chars: 100,
            chars_avoided: 100,
            ..Default::default()
        });

        let report = l.report();
        // 100 avoided / (100 result + 100 avoided) = 0.5
        assert!((report.context_savings_ratio - 0.5).abs() < 0.01);
    }

    #[test]
    fn reset_clears_metrics() {
        let l = ledger();
        l.set_enabled(true);

        l.record(ContextMetrics {
            tool: "test".to_string(),
            result_chars: 100,
            ..Default::default()
        });

        l.reset();
        let report = l.report();
        assert_eq!(report.total_invocations, 0);
    }

    #[test]
    fn per_tool_breakdown() {
        let l = ledger();
        l.set_enabled(true);
        l.reset();

        l.record(ContextMetrics {
            tool: "read".to_string(),
            result_chars: 100,
            ..Default::default()
        });

        l.record(ContextMetrics {
            tool: "grep".to_string(),
            result_chars: 200,
            ..Default::default()
        });

        let report = l.report();
        assert!(report.per_tool.contains_key("read"));
        assert!(report.per_tool.contains_key("grep"));
    }
}
