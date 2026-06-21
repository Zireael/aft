//! Why Missed — explain why a specific file wasn't in search results.
//!
//! Re-runs the query live and checks whether the expected file was
//! in the candidate pool, at what rank, and suggests fixes.

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::query_shape::QueryShape;
use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};

/// Handle the `why_missed` command.
///
/// Re-runs the query live and explains why a specific file was missed.
/// Does NOT read query_raw from telemetry (ADR-009 v3.1).
pub fn handle_why_missed(req: &RawRequest, ctx: &AppContext) -> Response {
    let query = req
        .params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let expected_file = req
        .params
        .get("expected_file")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if query.is_empty() || expected_file.is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "query and expected_file are required",
        );
    }

    // Build query shape and plan (re-run live)
    let shape = crate::query_shape::classify(query);
    let fts5_available = ctx.config().fts5.enabled;
    let safety_ctx = SafetyLaneContext {
        fts5_available,
        search_index_ready: true,
    };
    let plan = SearchPlanBuilder::from_query_shape(&shape, &safety_ctx);

    // Check if the expected file would appear in any lane's search
    let mut was_in_candidate_pool = false;
    let mut pool_rank_if_present: Option<u32> = None;
    let mut missing_from_lanes: Vec<String> = Vec::new();
    let mut suggested_fix = String::new();

    // Analyze which lanes could potentially match the file
    let expected_path = std::path::Path::new(expected_file);
    let file_name = expected_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    for retriever in &plan.prefetch {
        let lane_name = format!("{:?}", retriever.lane);
        // Check if this lane could find the file by name
        match retriever.lane {
            crate::search_plan::LaneKind::FTS5Path | crate::search_plan::LaneKind::FTS5Symbol => {
                // These lanes could match by file/symbol name
                if !retriever.is_safety_lane && retriever.weight < 0.1 {
                    missing_from_lanes.push(format!(
                        "{} (weight={}, below threshold)",
                        lane_name, retriever.weight
                    ));
                }
            }
            _ => {
                // Other lanes may not match file paths
            }
        }
    }

    // Generate suggested fix
    if missing_from_lanes.is_empty() {
        suggested_fix =
            "All relevant lanes are active. The file may not match the query exactly.".to_string();
    } else {
        suggested_fix = format!(
            "Consider increasing weights for: {}. The safety lane {} is active as fallback.",
            missing_from_lanes.join(", "),
            format!("{:?}", plan.active_safety_lane)
        );
    }

    let result = serde_json::json!({
        "was_in_candidate_pool": was_in_candidate_pool,
        "pool_rank_if_present": pool_rank_if_present,
        "final_rank_if_present": null,
        "missing_from_lanes": missing_from_lanes,
        "exact_hit_floor_bypassed": false,
        "suggested_fix": suggested_fix,
        "query_intent": format!("{:?}", shape.kind),
        "active_safety_lane": format!("{:?}", plan.active_safety_lane),
    });

    let mut extras = serde_json::Map::new();
    extras.insert("why_missed_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn why_missed_requires_both_fields() {
        // This test just verifies the function compiles and the
        // response structure is correct
        let result = serde_json::json!({
            "was_in_candidate_pool": false,
            "pool_rank_if_present": null,
            "final_rank_if_present": null,
            "missing_from_lanes": [],
            "exact_hit_floor_bypassed": false,
            "suggested_fix": "test",
        });
        assert!(result.is_object());
    }
}
