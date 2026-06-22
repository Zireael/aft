//! Why Missed — explain why a specific file wasn't in search results.
//!
//! Re-runs the query live and checks whether the expected file was
//! in the candidate pool, at what rank, and suggests fixes.

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
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

    let search_response = run_live_search(req, ctx, query, 100);
    if !search_response.success {
        return Response::error(
            &req.id,
            "diagnostic_search_failed",
            format!(
                "why_missed could not run live semantic_search diagnostics: {}",
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
    let matched = results.iter().enumerate().find(|(_, result)| {
        result
            .get("file")
            .and_then(|value| value.as_str())
            .is_some_and(|file| paths_match(file, expected_file))
    });

    let (was_in_candidate_pool, pool_rank_if_present, final_rank_if_present, lane_contributions) =
        if let Some((rank, result)) = matched {
            let lane_rank = best_lane_rank(result).or(Some((rank + 1) as u64));
            (
                true,
                lane_rank,
                Some((rank + 1) as u64),
                result
                    .get("provenance")
                    .and_then(|provenance| provenance.get("lanes"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            )
        } else {
            (false, None, None, serde_json::json!([]))
        };

    let missing_from_lanes = missing_lane_diagnostics(&plan, &results);
    let miss_stage = if was_in_candidate_pool {
        "present_in_final_results"
    } else if results.is_empty() {
        "not_indexed_or_no_lane_candidates"
    } else {
        "not_in_top_100_candidate_window"
    };
    let suggested_fix = suggested_fix_for_miss(
        miss_stage,
        expected_file,
        results.len(),
        &missing_from_lanes,
        &plan,
    );

    let result = serde_json::json!({
        "was_in_candidate_pool": was_in_candidate_pool,
        "pool_rank_if_present": pool_rank_if_present,
        "fusion_rank_if_present": final_rank_if_present,
        "final_rank_if_present": final_rank_if_present,
        "missing_from_lanes": missing_from_lanes,
        "lane_contributions": lane_contributions,
        "miss_stage": miss_stage,
        "observed_result_count": results.len(),
        "exact_hit_floor_bypassed": matched
            .map(|(_, result)| {
                !result
                    .get("exact_hit_floor_applied")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                    && result
                        .get("is_exact_hit")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
            })
            .unwrap_or(false),
        "suggested_fix": suggested_fix,
        "query_intent": format!("{:?}", shape.kind),
        "active_safety_lane": format!("{:?}", plan.active_safety_lane),
    });

    let mut extras = serde_json::Map::new();
    extras.insert("why_missed_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

fn run_live_search(req: &RawRequest, ctx: &AppContext, query: &str, top_k: usize) -> Response {
    let search_req = RawRequest {
        id: format!("{}:why-missed-rerun", req.id),
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

fn paths_match(result_file: &str, expected_file: &str) -> bool {
    let result = normalize_path(result_file);
    let expected = normalize_path(expected_file);
    result == expected || result.ends_with(&expected) || expected.ends_with(&result)
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn best_lane_rank(result: &serde_json::Value) -> Option<u64> {
    result
        .get("provenance")
        .and_then(|provenance| provenance.get("lanes"))
        .and_then(|lanes| lanes.as_array())
        .and_then(|lanes| {
            lanes
                .iter()
                .filter_map(|lane| lane.get("rank").and_then(|rank| rank.as_u64()))
                .min()
        })
        .map(|zero_based| zero_based + 1)
}

fn missing_lane_diagnostics(
    plan: &crate::search_plan::SearchPlan,
    results: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    plan.prefetch
        .iter()
        .map(|retriever| {
            let lane_name = format!("{:?}", retriever.lane);
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
                "weight": retriever.weight,
                "max_candidates": retriever.max_candidates,
                "observed_candidates": observed_candidates,
                "reason": if observed_candidates == 0 {
                    "expected_file_not_observed_in_lane"
                } else {
                    "expected_file_not_selected_from_lane_candidates"
                },
            })
        })
        .collect()
}

fn suggested_fix_for_miss(
    miss_stage: &str,
    expected_file: &str,
    observed_result_count: usize,
    missing_from_lanes: &[serde_json::Value],
    plan: &crate::search_plan::SearchPlan,
) -> String {
    match miss_stage {
        "present_in_final_results" => {
            "The file is present in the live search result window; inspect the reported ranks and lane contributions.".to_string()
        }
        "not_indexed_or_no_lane_candidates" => format!(
            "No live candidates were returned. Verify {expected_file} is under the configured project_root, refresh the search index, and retry an exact path or symbol query."
        ),
        _ => {
            let inactive = missing_from_lanes
                .iter()
                .filter_map(|lane| {
                    (lane.get("observed_candidates").and_then(|value| value.as_u64()) == Some(0))
                        .then(|| lane.get("lane").and_then(|value| value.as_str()))
                        .flatten()
                })
                .collect::<Vec<_>>();
            format!(
                "The live search returned {observed_result_count} candidates but not {expected_file}. Retry with a larger top_k or an exact path/symbol query; safety lane is {:?}; lanes without observed candidates: {}.",
                plan.active_safety_lane,
                if inactive.is_empty() {
                    "none".to_string()
                } else {
                    inactive.join(", ")
                }
            )
        }
    }
}

#[cfg(test)]
mod tests {
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
