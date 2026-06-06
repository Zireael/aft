//! Semantic search health report.
//!
//! Gathers configuration, index state, search metrics, and provider status
//! into a single [`SemanticHealthReport`] that the `semantic_doctor` command
//! can serialize as JSON or render as a human-readable summary.

use serde::Serialize;

/// Top-level health verdict derived from the constituent signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Semantic search is disabled in config.
    Disabled,
    /// Index is building or refreshing — usable but not final.
    Building,
    /// Index is fully ready with no warnings.
    Healthy,
    /// Index is ready but recent searches show degraded quality.
    Degraded,
    /// Index build or provider connection has failed.
    Failed,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "disabled"),
            Self::Building => write!(f, "building"),
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded => write!(f, "degraded"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Configuration summary (secrets redacted).
#[derive(Debug, Clone, Serialize)]
pub struct ConfigSummary {
    pub backend: String,
    pub model: String,
    pub dimensions: Option<usize>,
    pub output_encoding: Option<String>,
    pub distance_metric: Option<String>,
    pub storage_strategy: Option<String>,
    pub query_prompt_active: bool,
    pub document_prompt_active: bool,
    pub diagnostics_enabled: bool,
    pub rerank_enabled: bool,
    pub rerank_model: Option<String>,
    /// Whether the `semantic-model2vec` Cargo feature is compiled in.
    pub model2vec_feature_enabled: bool,
    /// Local model path for model2vec backend (if configured).
    pub model2vec_model_path: Option<String>,
    /// Max token length for model2vec truncation (if configured).
    pub model2vec_max_length: Option<usize>,
}

/// Index health state.
#[derive(Debug, Clone, Serialize)]
pub struct IndexSummary {
    /// Live lifecycle label: "disabled", "building", "partial", "ready", "failed".
    pub status: String,
    /// Number of indexed chunks/entries.
    pub entry_count: usize,
    /// Embedding dimension.
    pub dimension: Option<usize>,
    /// Whether the index fingerprint matches the current config.
    pub fingerprint_fresh: bool,
    /// Error message if the index is in a failed state.
    pub last_error: Option<String>,
    /// Build progress when building (0.0–1.0).
    pub build_progress: Option<f64>,
}

/// Search quality metrics over the recent window.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSummary {
    /// Number of queries in the rolling window.
    pub total_queries: usize,
    /// Median latency in milliseconds.
    pub p50_latency_ms: f64,
    /// 95th percentile latency in milliseconds.
    pub p95_latency_ms: f64,
    /// Fraction of queries returning zero results (0.0–1.0).
    pub zero_result_rate: f64,
    /// Fraction of queries flagged low-confidence (0.0–1.0).
    pub low_confidence_rate: f64,
    /// Fraction of queries with embedding failures (0.0–1.0).
    pub embedding_failure_rate: f64,
    /// Fraction of queries with lexical failures (0.0–1.0).
    pub lexical_failure_rate: f64,
}

/// Provider connectivity status.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderSummary {
    /// Whether a probe embedding succeeded.
    pub reachable: bool,
    /// Provider-reported dimension (if probe succeeded).
    pub probed_dimension: Option<usize>,
    /// Error message if the probe failed.
    pub error: Option<String>,
}

/// Actionable suggestion for the user.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// Short label for the suggestion (e.g. "wait_for_indexing").
    pub label: String,
    /// Human-readable explanation.
    pub message: String,
}

/// Complete semantic search health report.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticHealthReport {
    /// Overall health verdict.
    pub status: HealthStatus,
    /// Config summary (secrets redacted).
    pub config: ConfigSummary,
    /// Index state.
    pub index: IndexSummary,
    /// Search quality metrics (empty window → zeros).
    pub metrics: MetricsSummary,
    /// Provider connectivity.
    pub provider: ProviderSummary,
    /// Active warnings from recent searches.
    pub warnings: Vec<String>,
    /// Actionable next steps for the user.
    pub suggestions: Vec<Suggestion>,
}

impl SemanticHealthReport {
    /// One-line human-readable summary suitable for agent output.
    pub fn render_line(&self) -> String {
        format!(
            "semantic: {} | {} | {} queries, p50={:.0}ms | {} suggestions",
            self.status,
            self.index.status,
            self.metrics.total_queries,
            self.metrics.p50_latency_ms,
            self.suggestions.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_display() {
        assert_eq!(HealthStatus::Disabled.to_string(), "disabled");
        assert_eq!(HealthStatus::Building.to_string(), "building");
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
        assert_eq!(HealthStatus::Degraded.to_string(), "degraded");
        assert_eq!(HealthStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn health_status_serializes_snake_case() {
        let s = serde_json::to_value(&HealthStatus::Degraded).unwrap();
        assert_eq!(s, "degraded");
    }

    #[test]
    fn render_line_includes_key_fields() {
        let report = SemanticHealthReport {
            status: HealthStatus::Healthy,
            config: ConfigSummary {
                backend: "fastembed".into(),
                model: "all-MiniLM-L6-v2".into(),
                dimensions: Some(384),
                output_encoding: Some("float".into()),
                distance_metric: Some("cosine".into()),
                storage_strategy: Some("native_f32".into()),
                query_prompt_active: false,
                document_prompt_active: false,
                diagnostics_enabled: false,
                rerank_enabled: false,
                rerank_model: None,
                model2vec_feature_enabled: cfg!(feature = "semantic-model2vec"),
                model2vec_model_path: None,
                model2vec_max_length: None,
            },
            index: IndexSummary {
                status: "ready".into(),
                entry_count: 1234,
                dimension: Some(384),
                fingerprint_fresh: true,
                last_error: None,
                build_progress: None,
            },
            metrics: MetricsSummary {
                total_queries: 42,
                p50_latency_ms: 123.0,
                p95_latency_ms: 456.0,
                zero_result_rate: 0.05,
                low_confidence_rate: 0.1,
                embedding_failure_rate: 0.0,
                lexical_failure_rate: 0.0,
            },
            provider: ProviderSummary {
                reachable: true,
                probed_dimension: Some(384),
                error: None,
            },
            warnings: vec![],
            suggestions: vec![Suggestion {
                label: "all_clear".into(),
                message: "No issues detected.".into(),
            }],
        };
        let line = report.render_line();
        assert!(line.contains("healthy"));
        assert!(line.contains("ready"));
        assert!(line.contains("42 queries"));
    }

    #[test]
    fn config_summary_redacts_nothing_by_construction() {
        // ConfigSummary never holds raw API keys — it stores env var names only.
        let cs = ConfigSummary {
            backend: "openai_compatible".into(),
            model: "text-embedding-3-small".into(),
            dimensions: Some(1536),
            output_encoding: Some("float".into()),
            distance_metric: Some("cosine".into()),
            storage_strategy: Some("native_f32".into()),
            query_prompt_active: true,
            document_prompt_active: false,
            diagnostics_enabled: true,
            rerank_enabled: false,
            rerank_model: None,
            model2vec_feature_enabled: cfg!(feature = "semantic-model2vec"),
            model2vec_model_path: None,
            model2vec_max_length: None,
        };
        let json = serde_json::to_string(&cs).unwrap();
        assert!(!json.contains("api_key"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn index_summary_build_progress_only_when_building() {
        let building = IndexSummary {
            status: "building".into(),
            entry_count: 0,
            dimension: None,
            fingerprint_fresh: false,
            last_error: None,
            build_progress: Some(0.61),
        };
        assert_eq!(building.build_progress, Some(0.61));

        let ready = IndexSummary {
            status: "ready".into(),
            entry_count: 100,
            dimension: Some(384),
            fingerprint_fresh: true,
            last_error: None,
            build_progress: None,
        };
        assert!(ready.build_progress.is_none());
    }

    #[test]
    fn metrics_summary_zero_queries() {
        let m = MetricsSummary {
            total_queries: 0,
            p50_latency_ms: 0.0,
            p95_latency_ms: 0.0,
            zero_result_rate: 0.0,
            low_confidence_rate: 0.0,
            embedding_failure_rate: 0.0,
            lexical_failure_rate: 0.0,
        };
        assert_eq!(m.total_queries, 0);
    }

    #[test]
    fn suggestion_label_and_message_roundtrip() {
        let s = Suggestion {
            label: "wait_for_indexing".into(),
            message: "Index is building. Wait for completion.".into(),
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["label"], "wait_for_indexing");
        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("Index is building"));
    }
}
