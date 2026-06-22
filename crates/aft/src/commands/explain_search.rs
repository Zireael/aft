//! Explain Search — diagnostic tool for retrieval intelligence.
//!
//! Re-runs a search query and returns detailed diagnostics about which lanes
//! fired, candidate scores, and ranking decisions.

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};
use crate::telemetry::hash_query;

/// Handle the `explain_search` command.
///
/// Returns detailed diagnostics about query routing, lane activation,
/// candidate scores, and ranking decisions.
pub fn handle_explain_search(req: &RawRequest, ctx: &AppContext) -> Response {
    let query = req
        .params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if query.is_empty() {
        return Response::error(&req.id, "invalid_request", "query is required");
    }

    let start = std::time::Instant::now();

    // Build query shape
    let shape = crate::query_shape::classify(query);

    // Build SearchPlan
    let fts5_available = ctx.config().fts5.enabled;
    let safety_ctx = SafetyLaneContext {
        fts5_available,
        search_index_ready: true,
    };
    let plan = SearchPlanBuilder::from_query_shape(&shape, &safety_ctx);

    // Compute diagnostics
    let query_kind = format!("{:?}", shape.kind);
    let active_safety_lane = format!("{:?}", plan.active_safety_lane);

    // Lane weights
    let lane_weights: Vec<serde_json::Value> = plan
        .lane_weights
        .iter()
        .map(|(lane, weight)| {
            serde_json::json!({
                "lane": format!("{:?}", lane),
                "weight": weight,
            })
        })
        .collect();

    let search_response = run_live_search(req, ctx, query, 100);
    if !search_response.success {
        return Response::error(
            &req.id,
            "diagnostic_search_failed",
            format!(
                "explain_search could not run live semantic_search diagnostics: {}",
                search_response
                    .data
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("semantic_search failed")
            ),
        );
    }

    let results = search_response
        .data
        .get("results")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let per_lane_candidates = observed_lane_counts(&results, &plan);
    let top_10_rrf_scores: Vec<serde_json::Value> = results
        .iter()
        .take(10)
        .enumerate()
        .map(|(index, result)| {
            serde_json::json!({
                "rank": index + 1,
                "file": result.get("file").cloned().unwrap_or(serde_json::Value::Null),
                "score": result.get("score").cloned().unwrap_or(serde_json::Value::Null),
                "rrf_score": result.get("score").cloned().unwrap_or(serde_json::Value::Null),
                "source": result.get("source").cloned().unwrap_or(serde_json::Value::Null),
                "is_exact_hit": result.get("is_exact_hit").cloned().unwrap_or(serde_json::Value::Bool(false)),
                "exact_hit_floor_applied": result.get("exact_hit_floor_applied").cloned().unwrap_or(serde_json::Value::Bool(false)),
                "lanes": result
                    .get("provenance")
                    .and_then(|provenance| provenance.get("lanes"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            })
        })
        .collect();

    let provenance = search_response
        .data
        .get("retrieval_intelligence_provenance")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let degraded_lanes = provenance
        .get("degraded_lanes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let context_budget = provenance
        .get("context_budget")
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "total_tokens": plan.context_budget.total_tokens,
                "per_candidate_tokens": plan.context_budget.per_candidate_tokens,
                "enrich_pool": format!("{:?}", plan.context_budget.enrich_pool),
            })
        });
    let reranker_skipped_reason = provenance
        .get("context_budget")
        .and_then(|budget| budget.get("reranker_skipped_reason"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let latency_ms = start.elapsed().as_millis() as f64;

    // Hash query for telemetry correlation
    let query_hash = hash_query(
        query,
        &ctx.config()
            .intelligence
            .telemetry
            .telemetry_query_hash_salt,
    );

    let result = serde_json::json!({
        "query_intent": query_kind,
        "query_hash": query_hash,
        "active_safety_lane": active_safety_lane,
        "lane_weights": lane_weights,
        "per_lane_candidates": per_lane_candidates,
        "top_10_rrf_scores": top_10_rrf_scores,
        "reranker_skipped_reason": reranker_skipped_reason,
        "context_budget_used": context_budget,
        "degraded_lanes": degraded_lanes,
        "observed_result_count": results.len(),
        "telemetry": {
            "persist_enabled": ctx.config().intelligence.telemetry.telemetry_persist,
            "db_available": ctx.db().is_some(),
            "query_storage": ctx.config().intelligence.telemetry.telemetry_store_query,
        },
        "latency_ms": latency_ms,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("explain_search_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

fn run_live_search(req: &RawRequest, ctx: &AppContext, query: &str, top_k: usize) -> Response {
    let search_req = RawRequest {
        id: format!("{}:explain-search-rerun", req.id),
        command: "semantic_search".to_string(),
        lsp_hints: req.lsp_hints.clone(),
        session_id: req.session_id.clone(),
        params: serde_json::json!({
            "query": query,
            "top_k": top_k,
        }),
    };
    crate::commands::semantic_search::handle_semantic_search(&search_req, ctx)
}

fn observed_lane_counts(
    results: &[serde_json::Value],
    plan: &crate::search_plan::SearchPlan,
) -> Vec<serde_json::Value> {
    plan.prefetch
        .iter()
        .map(|p| {
            let lane_name = format!("{:?}", p.lane);
            let observed_candidates = results
                .iter()
                .filter(|result| {
                    result
                        .get("provenance")
                        .and_then(|provenance| provenance.get("lanes"))
                        .and_then(|lanes| lanes.as_array())
                        .is_some_and(|lanes| {
                            lanes.iter().any(|lane| {
                                lane.get("lane").and_then(|value| value.as_str())
                                    == Some(lane_name.as_str())
                            })
                        })
                })
                .count();
            serde_json::json!({
                "lane": lane_name,
                "max_candidates": p.max_candidates,
                "weight": p.weight,
                "is_safety_lane": p.is_safety_lane,
                "observed_candidates": observed_candidates,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_shape_from_identifier() {
        let shape = crate::query_shape::classify("SemanticBackendConfig");
        assert_eq!(format!("{:?}", shape.kind), "Identifier");
    }

    #[test]
    fn query_shape_from_nl() {
        let shape = crate::query_shape::classify("how does retry backoff work");
        assert_eq!(format!("{:?}", shape.kind), "NaturalLanguage");
    }
}
