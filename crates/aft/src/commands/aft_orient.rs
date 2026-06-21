//! AFT Orient — orientation command for retrieval intelligence.
//!
//! Returns primary files, entry symbols, dependency symbols, test hints,
//! config hints, and a deterministic orientation summary for a query.

use crate::candidate::FusedCandidate;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::retrieval::RetrievalAdapter;
use crate::retrieval::{apply_ranking_features, RRFFusionEngine, RankingFeaturesConfig};
use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};

/// Handle the `aft_orient` command.
pub fn handle_aft_orient(req: &RawRequest, ctx: &AppContext) -> Response {
    let start = std::time::Instant::now();
    let query = req
        .params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let depth = req
        .params
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;

    if query.is_empty() {
        return Response::error(&req.id, "invalid_request", "query is required");
    }

    // Build query shape and plan
    let shape = crate::query_shape::classify(query);
    let fts5_available = ctx.config().fts5.enabled;
    let safety_ctx = SafetyLaneContext {
        fts5_available,
        search_index_ready: true,
    };
    let plan = SearchPlanBuilder::from_query_shape(&shape, &safety_ctx);

    // Collect candidates from adapters
    let mut candidate_sets = Vec::new();

    // Trigram adapter (always available)
    let trig = crate::retrieval::TrigramAdapter::new();
    candidate_sets.extend(trig.retrieve(query, &plan));

    // Fuse with RRF
    let mut fused = RRFFusionEngine::fuse(&plan, candidate_sets);

    // Apply ranking features
    let ranking_config = RankingFeaturesConfig::default();
    apply_ranking_features(&mut fused, query, &plan, &ranking_config);

    // Enrich with graph context (disabled — placeholder until wired)
    // enrich_with_graph_context(&mut fused, None, &graph_health, &ctx.config().intelligence);

    // Extract primary files (top 5)
    let primary_files: Vec<String> = fused
        .iter()
        .take(5)
        .map(|c| c.file_path.to_string_lossy().to_string())
        .collect();

    // Entry symbols (file stems as rough proxy)
    let entry_symbols: Vec<String> = fused
        .iter()
        .take(5)
        .filter_map(|c| {
            c.file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Dependency symbols (empty for now — would query callgraph)
    let dependency_symbols: Vec<String> = Vec::new();

    // Test hints (path heuristic)
    let test_hints: Vec<String> = fused
        .iter()
        .filter(|c| {
            let p = c.file_path.to_string_lossy().to_lowercase();
            p.contains("test") || p.contains("spec")
        })
        .take(5)
        .map(|c| c.file_path.to_string_lossy().to_string())
        .collect();

    // Config hints (path heuristic)
    let config_hints: Vec<String> = fused
        .iter()
        .filter(|c| {
            let p = c.file_path.to_string_lossy().to_lowercase();
            p.contains("config") || p.ends_with(".toml") || p.ends_with(".json")
        })
        .take(5)
        .map(|c| c.file_path.to_string_lossy().to_string())
        .collect();

    // Orientation summary (deterministic template)
    let top_file = primary_files
        .first()
        .map(|f| f.as_str())
        .unwrap_or("unknown");
    let top_symbol = entry_symbols
        .first()
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let second_context = if primary_files.len() > 1 {
        let second_file = &primary_files[1];
        format!("It also involves {second_file}.")
    } else {
        String::new()
    };
    let orientation_summary =
        format!("{top_symbol} is implemented in {top_file}. {second_context}");

    let latency_ms = start.elapsed().as_millis() as f64;

    let result = serde_json::json!({
        "primary_files": primary_files,
        "entry_symbols": entry_symbols,
        "dependency_symbols": dependency_symbols,
        "test_hints": test_hints,
        "config_hints": config_hints,
        "orientation_summary": orientation_summary,
        "latency_ms": latency_ms,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("orient_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_test() {
        // Basic compilation test
        let result = serde_json::json!({
            "primary_files": [],
            "entry_symbols": [],
            "orientation_summary": "unknown is implemented in unknown. ",
        });
        assert!(result.is_object());
    }
}
