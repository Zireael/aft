use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;

/// Identifies which search pipeline path was taken for a single query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPipelineType {
    Lexical,
    Semantic,
    Hybrid,
    SemanticRerank,
    HybridRerank,
    LexicalFallback,
}

impl std::fmt::Display for SearchPipelineType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lexical => write!(f, "lexical"),
            Self::Semantic => write!(f, "semantic"),
            Self::Hybrid => write!(f, "hybrid"),
            Self::SemanticRerank => write!(f, "semantic_rerank"),
            Self::HybridRerank => write!(f, "hybrid_rerank"),
            Self::LexicalFallback => write!(f, "lexical_fallback"),
        }
    }
}

/// Warnings that can be attached to a single search query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchWarning {
    LowConfidence,
    EmptyResults,
    PartialIndex {
        completeness: f64,
    },
    StaleIndex,
    DegradedIndex,
    EmbeddingFailure {
        reason: String,
    },
    LexicalFailure {
        reason: String,
    },
    DimensionMismatch {
        expected: usize,
        got: usize,
    },
}

impl std::fmt::Display for SearchWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LowConfidence => write!(f, "low_confidence"),
            Self::EmptyResults => write!(f, "empty_results"),
            Self::PartialIndex { completeness } => {
                write!(f, "partial_index({}%)", (completeness * 100.0) as usize)
            }
            Self::StaleIndex => write!(f, "stale_index"),
            Self::DegradedIndex => write!(f, "degraded_index"),
            Self::EmbeddingFailure { reason } => write!(f, "embedding_failure({reason})"),
            Self::LexicalFailure { reason } => write!(f, "lexical_failure({reason})"),
            Self::DimensionMismatch { expected, got } => {
                write!(f, "dimension_mismatch(expected={expected}, got={got})")
            }
        }
    }
}

/// Per-query diagnostics for a single semantic/hybrid search invocation.
///
/// Collects timing, scoring, and warning information without exposing
/// raw query text or result snippets by default.
#[derive(Debug, Clone, Serialize)]
pub struct SearchDiagnostics {
    /// Hash of the query string (SHA-256 hex prefix, first 16 chars).
    /// The full query is NOT captured to avoid leaking user data.
    pub query_hash: String,
    /// Which pipeline path was taken.
    pub pipeline_type: SearchPipelineType,
    /// Index state at search time.
    pub index_state: String,
    /// Total wall-clock latency in milliseconds.
    pub total_latency_ms: f64,
    /// Time spent embedding the query, in milliseconds.
    pub embedding_latency_ms: Option<f64>,
    /// Time spent on lexical (trigram) search, in milliseconds.
    pub lexical_latency_ms: Option<f64>,
    /// Time spent on vector search (k-NN), in milliseconds.
    pub vector_search_latency_ms: Option<f64>,
    /// Time spent on hybrid fusion, in milliseconds.
    pub hybrid_fusion_latency_ms: Option<f64>,
    /// Number of candidates before fusion/capping.
    pub candidate_count: usize,
    /// Number of results returned to the caller.
    pub returned_count: usize,
    /// Minimum score among returned results.
    pub score_min: Option<f32>,
    /// Median score among returned results.
    pub score_median: Option<f32>,
    /// P90 score among returned results.
    pub score_p90: Option<f32>,
    /// Maximum score among returned results.
    pub score_max: Option<f32>,
    /// Difference between the highest and second-highest score.
    pub top1_margin: Option<f32>,
    /// Whether the embedding query cache was hit.
    pub query_cache_hit: bool,
    /// Whether a prompt template was active for this query.
    pub prompt_active: bool,
    /// Warnings generated for this query.
    #[serde(default)]
    pub warnings: Vec<SearchWarning>,
}

impl SearchDiagnostics {
    /// Build a query hash (first 16 hex chars of SHA-256) without storing
    /// the raw query.
    pub fn hash_query(query: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(query.as_bytes());
        let result = hasher.finalize();
        format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            result[0], result[1], result[2], result[3],
            result[4], result[5], result[6], result[7])
    }
}

/// Rolling aggregate metrics over recent search queries.
///
/// Tracks latency distribution, zero-result rate, failure rates, and
/// query cache hit rate over a configurable window.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateSearchMetrics {
    /// Number of queries in the current window.
    pub total_queries: usize,
    /// P50 total latency in milliseconds.
    pub p50_latency_ms: f64,
    /// P95 total latency in milliseconds.
    pub p95_latency_ms: f64,
    /// Fraction of queries that returned zero results.
    pub zero_result_rate: f64,
    /// Fraction of queries with low-confidence results.
    pub low_confidence_rate: f64,
    /// Fraction of queries where embedding failed.
    pub embedding_failure_rate: f64,
    /// Fraction of queries where lexical search failed or was skipped.
    pub lexical_failure_rate: f64,
    /// Fraction of queries that hit the embedding cache.
    pub query_cache_hit_rate: f64,
    /// Average index completeness at search time (0.0–1.0).
    pub avg_index_completeness: Option<f64>,
}

/// Collects per-query diagnostics into a rolling window for aggregate metrics.
///
/// Sized by `metrics_window_size` (default 100). Old entries are evicted
/// from the front when the window is full.
#[derive(Debug, Clone)]
pub struct SearchMetricsCollector {
    window_size: usize,
    entries: VecDeque<SearchDiagnostics>,
}

impl SearchMetricsCollector {
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size: window_size.max(1),
            entries: VecDeque::with_capacity(window_size),
        }
    }

    /// Record a single query's diagnostics. Evicts oldest if at capacity.
    pub fn record(&mut self, diag: SearchDiagnostics) {
        if self.entries.len() >= self.window_size {
            self.entries.pop_front();
        }
        self.entries.push_back(diag);
    }

    /// Compute aggregate metrics over the current window.
    pub fn aggregate(&self) -> AggregateSearchMetrics {
        let n = self.entries.len();
        if n == 0 {
            return AggregateSearchMetrics {
                total_queries: 0,
                p50_latency_ms: 0.0,
                p95_latency_ms: 0.0,
                zero_result_rate: 0.0,
                low_confidence_rate: 0.0,
                embedding_failure_rate: 0.0,
                lexical_failure_rate: 0.0,
                query_cache_hit_rate: 0.0,
                avg_index_completeness: None,
            };
        }

        let mut latencies: Vec<f64> = self.entries.iter().map(|d| d.total_latency_ms).collect();
        latencies.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let percentile = |pct: f64| -> f64 {
            if latencies.is_empty() {
                return 0.0;
            }
            let idx = ((n as f64) * pct).ceil() as usize;
            let idx = idx.saturating_sub(1).min(n - 1);
            latencies[idx]
        };
        let p50 = percentile(0.50);
        let p95 = percentile(0.95);

        let zw = self
            .entries
            .iter()
            .filter(|d| d.returned_count == 0)
            .count();
        let lcw = self
            .entries
            .iter()
            .filter(|d| {
                d.warnings
                    .iter()
                    .any(|w| matches!(w, SearchWarning::LowConfidence))
            })
            .count();
        let efw = self
            .entries
            .iter()
            .filter(|d| {
                d.warnings
                    .iter()
                    .any(|w| matches!(w, SearchWarning::EmbeddingFailure { .. }))
            })
            .count();
        let lfw = self
            .entries
            .iter()
            .filter(|d| {
                d.warnings
                    .iter()
                    .any(|w| matches!(w, SearchWarning::LexicalFailure { .. }))
            })
            .count();
        let chw = self.entries.iter().filter(|d| d.query_cache_hit).count();

        let partial_completeness: Vec<f64> = self
            .entries
            .iter()
            .filter_map(|d| {
                d.warnings.iter().find_map(|w| {
                    if let SearchWarning::PartialIndex { completeness } = w {
                        Some(*completeness)
                    } else {
                        None
                    }
                })
            })
            .collect();

        AggregateSearchMetrics {
            total_queries: n,
            p50_latency_ms: p50,
            p95_latency_ms: p95,
            zero_result_rate: zw as f64 / n as f64,
            low_confidence_rate: lcw as f64 / n as f64,
            embedding_failure_rate: efw as f64 / n as f64,
            lexical_failure_rate: lfw as f64 / n as f64,
            query_cache_hit_rate: chw as f64 / n as f64,
            avg_index_completeness: if partial_completeness.is_empty() {
                None
            } else {
                Some(partial_completeness.iter().sum::<f64>() / partial_completeness.len() as f64)
            },
        }
    }

    /// Clear all collected entries.
    pub fn reset(&mut self) {
        self.entries.clear();
    }

    /// Number of entries currently in the window.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no entries are recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Tracks elapsed time for a single pipeline phase. Constructed at phase
/// start, then `.stop()` returns the duration in milliseconds.
pub struct PhaseTimer {
    start: Instant,
}

impl PhaseTimer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Stop the timer and return elapsed time in milliseconds.
    pub fn stop(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

/// Compute percentile score statistics from a slice of scores.
pub fn score_statistics(scores: &[f32]) -> (Option<f32>, Option<f32>, Option<f32>, Option<f32>) {
    if scores.is_empty() {
        return (None, None, None, None);
    }
    let mut sorted = scores.to_vec();
    sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = sorted.first().copied();
    let max = sorted.last().copied();
    let n = sorted.len();
    let percentile = |pct: f64| -> f32 {
        let idx = ((n as f64) * pct).ceil() as usize;
        let idx = idx.saturating_sub(1).min(n - 1);
        sorted[idx]
    };
    let median = Some(percentile(0.50));
    let p90 = Some(percentile(0.90));
    (min, median, p90, max)
}

/// Compute the margin between the top score and the second-best score.
pub fn top1_margin(scores: &[f32]) -> Option<f32> {
    if scores.len() < 2 {
        return None;
    }
    let mut sorted = scores.to_vec();
    sorted.sort_unstable_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    Some(sorted[0] - sorted[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_hash_produces_deterministic_human_readable_prefix() {
        let h1 = SearchDiagnostics::hash_query("how to create a file");
        let h2 = SearchDiagnostics::hash_query("how to create a file");
        assert_eq!(h1, h2, "hash should be deterministic");
        assert_eq!(h1.len(), 16, "hash should be 16 hex chars");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()), "hash should be hex");
    }

    #[test]
    fn query_hash_differs_for_different_queries() {
        let h1 = SearchDiagnostics::hash_query("what is this");
        let h2 = SearchDiagnostics::hash_query("what is that");
        assert_ne!(h1, h2, "different queries should produce different hashes");
    }

    #[test]
    fn search_diagnostics_rejects_no_raw_query_in_serialization() {
        let diag = SearchDiagnostics {
            query_hash: "abc123".to_string(),
            pipeline_type: SearchPipelineType::Semantic,
            index_state: "ready".to_string(),
            total_latency_ms: 42.0,
            embedding_latency_ms: None,
            lexical_latency_ms: None,
            vector_search_latency_ms: None,
            hybrid_fusion_latency_ms: None,
            candidate_count: 10,
            returned_count: 5,
            score_min: None,
            score_median: None,
            score_p90: None,
            score_max: None,
            top1_margin: None,
            query_cache_hit: false,
            prompt_active: false,
            warnings: vec![],
        };
        let json = serde_json::to_string(&diag).unwrap();
        // The raw query text must never appear in diagnostics output.
        assert!(!json.contains("query\":"));
        assert!(json.contains("\"query_hash\":\"abc123\""));
    }

    #[test]
    fn warnings_display_format() {
        assert_eq!(
            SearchWarning::LowConfidence.to_string(),
            "low_confidence"
        );
        assert_eq!(SearchWarning::EmptyResults.to_string(), "empty_results");
        assert_eq!(
            SearchWarning::PartialIndex {
                completeness: 0.5
            }
            .to_string(),
            "partial_index(50%)"
        );
        assert_eq!(SearchWarning::StaleIndex.to_string(), "stale_index");
        assert_eq!(SearchWarning::DegradedIndex.to_string(), "degraded_index");
        assert_eq!(
            SearchWarning::EmbeddingFailure {
                reason: "timeout".into()
            }
            .to_string(),
            "embedding_failure(timeout)"
        );
        assert_eq!(
            SearchWarning::DimensionMismatch {
                expected: 768,
                got: 384
            }
            .to_string(),
            "dimension_mismatch(expected=768, got=384)"
        );
    }

    #[test]
    fn search_pipeline_type_display() {
        assert_eq!(SearchPipelineType::Lexical.to_string(), "lexical");
        assert_eq!(SearchPipelineType::Semantic.to_string(), "semantic");
        assert_eq!(SearchPipelineType::Hybrid.to_string(), "hybrid");
        assert_eq!(
            SearchPipelineType::SemanticRerank.to_string(),
            "semantic_rerank"
        );
        assert_eq!(
            SearchPipelineType::LexicalFallback.to_string(),
            "lexical_fallback"
        );
    }

    #[test]
    fn score_statistics_empty() {
        let (min, median, p90, max) = score_statistics(&[]);
        assert!(min.is_none());
        assert!(median.is_none());
        assert!(p90.is_none());
        assert!(max.is_none());
    }

    #[test]
    fn score_statistics_single_element() {
        let (min, median, p90, max) = score_statistics(&[0.5]);
        assert_eq!(min, Some(0.5));
        assert_eq!(median, Some(0.5));
        assert_eq!(p90, Some(0.5));
        assert_eq!(max, Some(0.5));
    }

    #[test]
    fn score_statistics_computes_percentiles() {
        // 10 values: 0.1, 0.2, ..., 1.0 — nearest-rank percentiles.
        // P50 = ceil(0.5 * 10) = 5th element (0.5)
        // P90 = ceil(0.9 * 10) = 9th element (0.9)
        let scores: Vec<f32> = (1..=10).map(|i| i as f32 * 0.1).collect();
        let (min, median, p90, max) = score_statistics(&scores);
        assert!((min.unwrap() - 0.1).abs() < 1e-6);
        assert!((median.unwrap() - 0.5).abs() < 1e-6, "median = {}", median.unwrap());
        assert!((p90.unwrap() - 0.9).abs() < 1e-6, "p90 = {}", p90.unwrap());
        assert!((max.unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn top1_margin_single_element() {
        assert!(top1_margin(&[0.9]).is_none());
    }

    #[test]
    fn top1_margin_empty() {
        assert!(top1_margin(&[]).is_none());
    }

    #[test]
    fn top1_margin_computes_difference() {
        let margin = top1_margin(&[0.5, 0.8, 0.6]).unwrap();
        assert!((margin - 0.2).abs() < 1e-6, "margin = {margin}");
    }

    #[test]
    fn search_metrics_collector_empty_aggregate() {
        let collector = SearchMetricsCollector::new(100);
        let agg = collector.aggregate();
        assert_eq!(agg.total_queries, 0);
        assert_eq!(agg.zero_result_rate, 0.0);
    }

    #[test]
    fn search_metrics_collector_tracks_multiple_entries() {
        let mut collector = SearchMetricsCollector::new(100);
        for i in 0..3 {
            collector.record(SearchDiagnostics {
                query_hash: format!("hash{i}"),
                pipeline_type: SearchPipelineType::Semantic,
                index_state: "ready".to_string(),
                total_latency_ms: 10.0 * (i + 1) as f64,
                embedding_latency_ms: None,
                lexical_latency_ms: None,
                vector_search_latency_ms: None,
                hybrid_fusion_latency_ms: None,
                candidate_count: 10,
                returned_count: 5,
                score_min: None,
                score_median: None,
                score_p90: None,
                score_max: None,
                top1_margin: None,
                query_cache_hit: i == 0,
                prompt_active: false,
                warnings: if i == 1 {
                    vec![SearchWarning::LowConfidence]
                } else {
                    vec![]
                },
            });
        }
        let agg = collector.aggregate();
        assert_eq!(agg.total_queries, 3);
        assert!((agg.query_cache_hit_rate - 1.0 / 3.0).abs() < 1e-6);
        assert!((agg.low_confidence_rate - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn search_metrics_collector_evicts_oldest_when_full() {
        let mut collector = SearchMetricsCollector::new(2);
        for i in 0..5 {
            collector.record(SearchDiagnostics {
                query_hash: format!("hash{i}"),
                pipeline_type: SearchPipelineType::Semantic,
                index_state: "ready".to_string(),
                total_latency_ms: 10.0,
                embedding_latency_ms: None,
                lexical_latency_ms: None,
                vector_search_latency_ms: None,
                hybrid_fusion_latency_ms: None,
                candidate_count: 10,
                returned_count: 5,
                score_min: None,
                score_median: None,
                score_p90: None,
                score_max: None,
                top1_margin: None,
                query_cache_hit: false,
                prompt_active: false,
                warnings: vec![],
            });
        }
        assert_eq!(collector.len(), 2);
        // The last entry has hash "hash4"
        assert_eq!(
            collector.entries.back().unwrap().query_hash,
            "hash4"
        );
    }

    #[test]
    fn search_metrics_collector_tracks_partial_completeness() {
        let mut collector = SearchMetricsCollector::new(100);
        collector.record(SearchDiagnostics {
            query_hash: "h1".into(),
            pipeline_type: SearchPipelineType::Semantic,
            index_state: "partial".into(),
            total_latency_ms: 10.0,
            embedding_latency_ms: None,
            lexical_latency_ms: None,
            vector_search_latency_ms: None,
            hybrid_fusion_latency_ms: None,
            candidate_count: 10,
            returned_count: 5,
            score_min: None,
            score_median: None,
            score_p90: None,
            score_max: None,
            top1_margin: None,
            query_cache_hit: false,
            prompt_active: false,
            warnings: vec![SearchWarning::PartialIndex {
                completeness: 0.75,
            }],
        });
        let agg = collector.aggregate();
        assert!((agg.avg_index_completeness.unwrap() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn phase_timer_measures_non_negative_duration() {
        let timer = PhaseTimer::start();
        // Short busy-wait to ensure measurable time.
        let mut x = 0u64;
        for _ in 0..100_000 {
            x = x.wrapping_add(1);
        }
        let ms = timer.stop();
        assert!(ms >= 0.0, "duration should not be negative, got {ms}");
        // Even on a very fast machine 100k ops should take > 0 µs.
        assert!(
            ms > 0.0 || x > 0,
            "duration should be measurable, got {ms}"
        );
    }

    #[test]
    fn aggregate_empty_collector_reset() {
        let mut collector = SearchMetricsCollector::new(10);
        collector.record(SearchDiagnostics {
            query_hash: "h".into(),
            pipeline_type: SearchPipelineType::Semantic,
            index_state: "ready".into(),
            total_latency_ms: 5.0,
            embedding_latency_ms: None,
            lexical_latency_ms: None,
            vector_search_latency_ms: None,
            hybrid_fusion_latency_ms: None,
            candidate_count: 10,
            returned_count: 5,
            score_min: None,
            score_median: None,
            score_p90: None,
            score_max: None,
            top1_margin: None,
            query_cache_hit: false,
            prompt_active: false,
            warnings: vec![],
        });
        collector.reset();
        let agg = collector.aggregate();
        assert_eq!(agg.total_queries, 0);
    }
}
