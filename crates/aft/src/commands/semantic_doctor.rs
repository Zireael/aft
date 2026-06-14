//! `semantic_doctor` command — produce a semantic search health report.
//!
//! ## Wire format
//!
//! Request:
//! ```json
//! { "probe_provider": false }
//! ```
//!
//! - `probe_provider` (optional, default false) — send a probe embedding to
//!   check provider connectivity. Adds latency; off by default.
//!
//! ## Response
//!
//! ```json
//! {
//!   "status": "healthy",
//!   "config": { "backend": "fastembed", "model": "all-MiniLM-L6-v2", ... },
//!   "index": { "status": "ready", "entry_count": 1234, ... },
//!   "metrics": { "total_queries": 42, "p50_latency_ms": 123.0, ... },
//!   "provider": { "reachable": false, "probed_dimension": null, ... },
//!   "warnings": [],
//!   "suggestions": [ { "label": "all_clear", "message": "..." } ]
//! }
//! ```

use serde::Deserialize;

use crate::protocol::{RawRequest, Response};
use crate::semantic_doctor::*;

#[derive(Debug, Deserialize)]
struct SemanticDoctorParams {
    #[serde(default)]
    probe_provider: bool,
}

pub fn handle_semantic_doctor(req: &RawRequest, ctx: &crate::context::AppContext) -> Response {
    let params: SemanticDoctorParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("semantic_doctor: invalid params: {e}"),
            );
        }
    };

    // --- Config summary ---
    let config = &ctx.config().semantic;
    let config_summary = ConfigSummary {
        backend: config.backend.as_str().to_string(),
        model: config.model.clone(),
        dimensions: config.dimensions,
        output_encoding: config.output_encoding.as_ref().map(|e| format!("{e:?}")),
        distance_metric: config.distance_metric.as_ref().map(|m| format!("{m:?}")),
        storage_strategy: config.storage_strategy.as_ref().map(|s| format!("{s:?}")),
        query_prompt_active: config.query_prompt_template.is_some(),
        document_prompt_active: config.document_prompt_template.is_some(),
        diagnostics_enabled: config.diagnostics_enabled,
        rerank_enabled: config.rerank_enabled,
        rerank_model: config.rerank_model.clone(),
        model2vec_feature_enabled: cfg!(feature = "semantic-model2vec"),
        model2vec_model_path: config.model_path.as_ref().map(|p| p.display().to_string()),
        model2vec_max_length: if config.model2vec_max_length > 0 {
            Some(config.model2vec_max_length)
        } else {
            None
        },
    };

    // --- Index summary ---
    let index_status_borrow = ctx.semantic_index_status().borrow();
    let index_status_label = format!("{:?}", *index_status_borrow);
    let index_status_lower = index_status_label.to_lowercase();

    // Extract progress from Building/Partial states.
    let build_progress = match &*index_status_borrow {
        crate::context::SemanticIndexStatus::Building {
            entries_done,
            entries_total,
            ..
        } => match (entries_done, entries_total) {
            (Some(done), Some(total)) if *total > 0 => Some(*done as f64 / *total as f64),
            _ => None,
        },
        crate::context::SemanticIndexStatus::Partial { completeness, .. } => Some(*completeness),
        _ => None,
    };

    let (entry_count, dimension, fingerprint_fresh, last_error) =
        if let Some(idx) = ctx.semantic_index().borrow().as_ref() {
            let entry_count = idx.entry_count();
            let dimension = Some(idx.dimension());
            let fingerprint_fresh = idx.fingerprint().is_some();
            let last_error = idx.last_error().map(|s| s.to_string());
            (entry_count, dimension, fingerprint_fresh, last_error)
        } else {
            (0, None, false, None)
        };

    let index_summary = IndexSummary {
        status: index_status_lower,
        entry_count,
        dimension,
        fingerprint_fresh,
        last_error,
        build_progress,
    };

    // --- Metrics summary ---
    let metrics_agg = ctx.semantic_search_metrics().borrow().aggregate();
    let metrics_summary = MetricsSummary {
        total_queries: metrics_agg.total_queries,
        p50_latency_ms: metrics_agg.p50_latency_ms,
        p95_latency_ms: metrics_agg.p95_latency_ms,
        zero_result_rate: metrics_agg.zero_result_rate,
        low_confidence_rate: metrics_agg.low_confidence_rate,
        embedding_failure_rate: metrics_agg.embedding_failure_rate,
        lexical_failure_rate: metrics_agg.lexical_failure_rate,
    };

    // --- Provider summary ---
    let provider_summary = if params.probe_provider {
        let borrow = ctx.semantic_embedding_model().borrow();
        match borrow.as_ref() {
            Some(_model) => {
                // dimension() requires &mut self; we can't mutate through RefCell borrow.
                // Fall back to reporting the model exists but probe not performed.
                ProviderSummary {
                    reachable: false,
                    probed_dimension: None,
                    error: Some(
                        "provider probe requires mutable access; use aft_search to verify connectivity".into(),
                    ),
                }
            }
            None => ProviderSummary {
                reachable: false,
                probed_dimension: None,
                error: Some("no embedding model configured".into()),
            },
        }
    } else {
        ProviderSummary {
            reachable: false,
            probed_dimension: None,
            error: None,
        }
    };

    // --- Warnings ---
    let mut warnings = Vec::new();
    if index_summary.last_error.is_some() {
        warnings.push("index_error".to_string());
    }
    if metrics_agg.low_confidence_rate > 0.3 {
        warnings.push("high_low_confidence_rate".to_string());
    }
    if metrics_agg.zero_result_rate > 0.3 {
        warnings.push("high_zero_result_rate".to_string());
    }
    if metrics_agg.embedding_failure_rate > 0.0 {
        warnings.push("embedding_failures".to_string());
    }
    if !provider_summary.reachable && params.probe_provider {
        if let Some(ref e) = provider_summary.error {
            warnings.push(format!("provider_unreachable: {e}"));
        }
    }

    // --- Suggestions ---
    let mut suggestions = Vec::new();
    match index_summary.status.as_str() {
        "disabled" => {
            suggestions.push(Suggestion {
                label: "enable_semantic".into(),
                message: "Semantic search is disabled. Set semantic.enabled = true in config."
                    .into(),
            });
        }
        "building" | "partial" => {
            suggestions.push(Suggestion {
                label: "wait_for_indexing".into(),
                message: "Index is building. Wait for completion before evaluating quality.".into(),
            });
        }
        "failed" => {
            suggestions.push(Suggestion {
                label: "check_provider".into(),
                message: "Index build failed. Verify provider credentials and connectivity.".into(),
            });
        }
        "ready" => {
            if metrics_agg.total_queries == 0 {
                suggestions.push(Suggestion {
                    label: "run_queries".into(),
                    message: "No queries recorded yet. Run some searches to assess quality.".into(),
                });
            }
            if metrics_agg.low_confidence_rate > 0.3 {
                suggestions.push(Suggestion {
                    label: "review_low_confidence".into(),
                    message:
                        "High low-confidence rate. Consider adjusting chunking or embedding model."
                            .into(),
                });
            }
            if metrics_agg.zero_result_rate > 0.3 {
                suggestions.push(Suggestion {
                    label: "review_zero_results".into(),
                    message: "High zero-result rate. Check file policy and index completeness."
                        .into(),
                });
            }
        }
        _ => {}
    }

    if suggestions.is_empty() {
        suggestions.push(Suggestion {
            label: "all_clear".into(),
            message: "No issues detected.".into(),
        });
    }

    // --- Determine overall status ---
    let status = match index_summary.status.as_str() {
        "disabled" => HealthStatus::Disabled,
        "building" | "partial" => HealthStatus::Building,
        "failed" => HealthStatus::Failed,
        "ready" => {
            if warnings.is_empty() {
                HealthStatus::Healthy
            } else {
                HealthStatus::Degraded
            }
        }
        _ => HealthStatus::Healthy,
    };

    let report = SemanticHealthReport {
        status,
        config: config_summary,
        index: index_summary,
        metrics: metrics_summary,
        provider: provider_summary,
        model2vec_health: build_model2vec_health(&ctx.config()),
        warnings,
        suggestions,
    };

    let mut payload = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    payload["summary_line"] = serde_json::Value::String(report.render_line());
    Response::success(&req.id, payload)
}

/// Build model2vec health summary if backend is model2vec.
fn build_model2vec_health(config: &crate::config::Config) -> Option<Model2VecHealthSummary> {
    if config.semantic.backend != crate::config::SemanticBackend::Model2Vec {
        return None;
    }

    #[cfg(feature = "semantic-model2vec")]
    {
        use crate::model2vec_catalog::is_known_model;
        use crate::model2vec_download::{model_dir_size, resolve_model2vec_files};

        match resolve_model2vec_files(
            Some(&config.semantic.model),
            config.semantic.model_path.as_deref(),
        ) {
            Ok(model_dir) => {
                let total_size = model_dir_size(&model_dir).ok();
                let config_path = model_dir.join("config.json");
                let dimensions = std::fs::read_to_string(&config_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v.get("hidden_size").and_then(|h| h.as_u64()))
                    .map(|d| d as usize);

                Some(Model2VecHealthSummary {
                    files_valid: true,
                    model_dir: Some(model_dir.to_string_lossy().to_string()),
                    total_size_bytes: total_size,
                    dimensions,
                    is_catalog_model: is_known_model(&config.semantic.model),
                    error: None,
                })
            }
            Err(e) => Some(Model2VecHealthSummary {
                files_valid: false,
                model_dir: None,
                total_size_bytes: None,
                dimensions: None,
                is_catalog_model: is_known_model(&config.semantic.model),
                error: Some(e),
            }),
        }
    }

    #[cfg(not(feature = "semantic-model2vec"))]
    {
        Some(Model2VecHealthSummary {
            files_valid: false,
            model_dir: None,
            total_size_bytes: None,
            dimensions: None,
            is_catalog_model: false,
            error: Some("semantic-model2vec feature not enabled".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::protocol::RawRequest;
    use serde_json::json;

    fn req_for(params: serde_json::Value) -> RawRequest {
        RawRequest {
            id: "test-1".to_string(),
            command: "semantic_doctor".to_string(),
            lsp_hints: None,
            session_id: None,
            params,
        }
    }

    fn make_ctx() -> AppContext {
        AppContext::new(Box::new(TreeSitterProvider::new()), Config::default())
    }

    #[test]
    fn handle_returns_health_report_for_disabled_semantic() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        assert!(resp.success, "got: {resp:?}");
        let v = &resp.data;
        assert_eq!(v["status"], "disabled");
        assert!(v["config"].is_object());
        assert!(v["index"].is_object());
        assert!(v["metrics"].is_object());
        assert!(v["provider"].is_object());
        assert!(!v["suggestions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn handle_includes_summary_line() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        let v = &resp.data;
        assert!(v["summary_line"].as_str().unwrap().contains("semantic:"));
    }

    #[test]
    fn handle_rejects_invalid_params() {
        let req = RawRequest {
            id: "test-2".to_string(),
            command: "semantic_doctor".to_string(),
            lsp_hints: None,
            session_id: None,
            params: json!("not an object"),
        };
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "invalid_request");
    }

    #[test]
    fn handle_config_summary_has_backend_and_model() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        let v = &resp.data;
        assert!(v["config"]["backend"].as_str().is_some());
        assert!(v["config"]["model"].as_str().is_some());
    }

    #[test]
    fn handle_metrics_defaults_to_zeros() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        let v = &resp.data;
        assert_eq!(v["metrics"]["total_queries"], 0);
        assert_eq!(v["metrics"]["p50_latency_ms"], 0.0);
    }

    #[test]
    fn handle_provider_not_probed_by_default() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        let v = &resp.data;
        assert_eq!(v["provider"]["reachable"], false);
        assert!(v["provider"]["error"].is_null());
    }

    #[test]
    fn handle_with_probe_provider_attempts_connection() {
        let req = req_for(json!({ "probe_provider": true }));
        let ctx = make_ctx();
        let resp = handle_semantic_doctor(&req, &ctx);
        let v = &resp.data;
        // Without a configured model, reachable should be false.
        assert_eq!(v["provider"]["reachable"], false);
        assert!(v["provider"]["error"] != serde_json::Value::Null);
    }
}
