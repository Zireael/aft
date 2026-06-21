//! Explain Search — diagnostic tool for retrieval intelligence.
//!
//! Re-runs a search query and returns detailed diagnostics about
//! which lanes fired, candidate scores, and ranking decisions.

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::query_shape::QueryShape;
use crate::search_plan::{LaneKind, SafetyLaneContext, SearchPlan, SearchPlanBuilder};
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

    // Per-lane candidate count
    let per_lane_candidates: Vec<serde_json::Value> = plan
        .prefetch
        .iter()
        .map(|p| {
            serde_json::json!({
                "lane": format!("{:?}", p.lane),
                "max_candidates": p.max_candidates,
                "weight": p.weight,
                "is_safety_lane": p.is_safety_lane,
            })
        })
        .collect();

    // Top 10 RRF scores (placeholder — actual scores come from fusion engine)
    let top_10_rrf_scores: Vec<serde_json::Value> = Vec::new();

    // Degraded lanes
    let degraded_lanes: Vec<serde_json::Value> = plan
        .prefetch
        .iter()
        .filter(|p| p.weight < 0.01)
        .map(|p| {
            serde_json::json!({
                "lane": format!("{:?}", p.lane),
                "reason": "weight_below_threshold",
                "fallback_used": format!("{:?}", plan.active_safety_lane),
            })
        })
        .collect();

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
        "reranker_skipped_reason": null,
        "context_budget_used": {
            "total_tokens": plan.context_budget.total_tokens,
            "per_candidate_tokens": plan.context_budget.per_candidate_tokens,
            "enrich_pool": format!("{:?}", plan.context_budget.enrich_pool),
        },
        "degraded_lanes": degraded_lanes,
        "latency_ms": latency_ms,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("explain_search_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_shape_from_identifier() {
        let shape = QueryShape::from_query("SemanticBackendConfig");
        assert_eq!(format!("{:?}", shape.kind), "Identifier");
    }

    #[test]
    fn query_shape_from_nl() {
        let shape = QueryShape::from_query("how does retry backoff work");
        assert_eq!(format!("{:?}", shape.kind), "NaturalLanguage");
    }
}
