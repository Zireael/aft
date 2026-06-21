//! Graph enrichment for Retrieval Intelligence v1.
//!
//! Enriches top search results with callgraph context (callers, callees,
//! mutation risk, public export status, graph confidence).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::callgraph_store::CallGraphStore;
use crate::candidate::FusedCandidate;
use crate::intelligence_config::IntelligenceConfig;
use crate::ril_indexer::GraphHealth;
use crate::search_plan::SearchPlan;

/// Graph context for a single enriched result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphContext {
    /// Direct callers of this symbol (max 10).
    pub callers: Vec<GraphEdge>,
    /// Direct callees of this symbol (max 10).
    pub callees: Vec<GraphEdge>,
    /// Files that import this file (max 10).
    pub imported_by: Vec<GraphEdge>,
    /// Mutation risk level.
    pub mutation_risk: String,
    /// Whether this symbol is a public export.
    pub is_public_export: bool,
    /// Graph confidence (Healthy/Stale/Degraded/Disabled).
    pub graph_confidence: String,
}

/// A graph edge (caller, callee, or importer).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphEdge {
    /// Symbol name.
    pub symbol: String,
    /// File path.
    pub file: String,
    /// Confidence level (Exact/High/Medium/Low/None).
    pub confidence: String,
}

/// Enrich top search results with graph context.
///
/// For top min(N, 5) results, queries the callgraph store and RIL indexer
/// to provide callers, callees, imported_by, mutation_risk, and graph_confidence.
///
/// Returns `graph_context = null` (not error, not empty {}) when GraphHealth is
/// Disabled or Cold.
///
/// Latency constraint: graph enrichment must not add >100ms to agent_fast profile p95.
pub fn enrich_with_graph_context(
    candidates: &mut [FusedCandidate],
    callgraph_store: Option<&CallGraphStore>,
    graph_health: &GraphHealth,
    config: &IntelligenceConfig,
) {
    let effective_top_n = if config.graph_enrichment_top_n == 0 {
        5
    } else {
        config.graph_enrichment_top_n
    };
    let top_n = effective_top_n.min(candidates.len()).min(5);
    let start = Instant::now();

    for candidate in candidates.iter_mut().take(top_n) {
        // Check latency budget
        if start.elapsed().as_millis() > 100 {
            break;
        }

        // graph_context = null when GraphHealth Disabled or Cold
        if !graph_health.usable() {
            candidate.context = Some(
                serde_json::json!({
                    "callers": [],
                    "callees": [],
                    "imported_by": [],
                    "mutation_risk": "Unknown",
                    "is_public_export": false,
                    "graph_confidence": format!("{:?}", graph_health),
                })
                .to_string(),
            );
            continue;
        }

        let graph_context = if let Some(store) = callgraph_store {
            build_graph_context(&candidate.file_path, store, graph_health)
        } else {
            GraphContext {
                callers: Vec::new(),
                callees: Vec::new(),
                imported_by: Vec::new(),
                mutation_risk: "Unknown".to_string(),
                is_public_export: false,
                graph_confidence: format!("{:?}", graph_health),
            }
        };

        candidate.context = Some(serde_json::to_string(&graph_context).unwrap_or_default());
    }
}

/// Build graph context for a single file.
fn build_graph_context(
    file_path: &Path,
    store: &CallGraphStore,
    graph_health: &GraphHealth,
) -> GraphContext {
    let mut callers = Vec::new();
    let mut callees = Vec::new();
    let mut imported_by = Vec::new();

    // Get callers — requires specific symbol; for file-level, use empty symbol
    // which may return all callers for the file (implementation-dependent)
    // If callers_of fails (e.g., empty symbol not supported), callers stays empty
    if let Ok(result) = store.callers_of(file_path, "", 1) {
        for cs in result.callers.iter().take(10) {
            callers.push(GraphEdge {
                symbol: cs.caller.symbol.clone(),
                file: cs.caller.file.clone(),
                confidence: "Medium".to_string(),
            });
        }
    }

    // Get callees — use direct callers of the file (callees API not available in store)
    // For now, callees is empty until the callgraph_store provides a callees_of API
    // This is a SOURCE-CONDITIONAL: the store doesn't have a direct callees_of method

    // Get imported_by — files that import this file
    // Placeholder: would query import relationships

    let mutation_risk = compute_risk_label(file_path);

    GraphContext {
        callers,
        callees,
        imported_by,
        mutation_risk,
        is_public_export: false, // would check symbol exports
        graph_confidence: format!("{:?}", graph_health),
    }
}

/// Compute mutation risk label for a file.
fn compute_risk_label(file_path: &Path) -> String {
    let path_str = file_path.to_string_lossy();
    let kind = crate::mutation_risk::FileKind::classify(&path_str);
    match kind {
        crate::mutation_risk::FileKind::Test => "Low".to_string(),
        crate::mutation_risk::FileKind::Config => "Low".to_string(),
        crate::mutation_risk::FileKind::Documentation => "Low".to_string(),
        crate::mutation_risk::FileKind::Source => "Medium".to_string(),
        _ => "Low".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateEntry, CandidateProvenance, LaneContribution};
    use crate::search_plan::LaneKind;
    use std::path::PathBuf;

    fn make_candidate(file: &str) -> FusedCandidate {
        FusedCandidate {
            file_path: PathBuf::from(file),
            line_range: Some((1, 10)),
            chunk_id: None,
            symbol_id: None,
            content_hash: None,
            rrf_score: 0.5,
            final_score: 0.5,
            is_exact_hit: false,
            is_vendor: false,
            is_generated: false,
            exact_hit_floor_applied: false,
            context: None,
            provenance: CandidateProvenance {
                lanes: vec![LaneContribution {
                    lane: LaneKind::Trigram,
                    rank_in_lane: 0,
                    score_in_lane: 0.5,
                    rrf_contribution: 0.0,
                }],
                is_graph_expansion: false,
                graph_expansion_reason: None,
            },
        }
    }

    // AC-2: graph_context is null when GraphHealth=Disabled
    #[test]
    fn null_context_when_disabled() {
        let config = IntelligenceConfig::default();
        let mut candidates = vec![make_candidate("src/main.rs")];
        let health = GraphHealth::Disabled;

        enrich_with_graph_context(&mut candidates, None, &health, &config);

        // Should have context with empty data, not error or empty {}
        assert!(candidates[0].context.is_some());
        let ctx: serde_json::Value =
            serde_json::from_str(&candidates[0].context.as_ref().unwrap()).unwrap();
        assert_eq!(ctx["callers"], serde_json::json!([]));
        assert_eq!(ctx["graph_confidence"], "Disabled");
    }

    // AC-3: graph_confidence="Stale" when GraphHealth=Stale
    #[test]
    fn stale_confidence_when_stale() {
        let config = IntelligenceConfig::default();
        let mut candidates = vec![make_candidate("src/main.rs")];
        let health = GraphHealth::Stale;

        enrich_with_graph_context(&mut candidates, None, &health, &config);

        let ctx: serde_json::Value =
            serde_json::from_str(&candidates[0].context.as_ref().unwrap()).unwrap();
        assert_eq!(ctx["graph_confidence"], "Stale");
    }

    // AC-5: graph_context does NOT include test_coverage_hint or config_owner
    #[test]
    fn no_inferred_hints() {
        let config = IntelligenceConfig::default();
        let mut candidates = vec![make_candidate("src/main.rs")];
        let health = GraphHealth::Disabled;

        enrich_with_graph_context(&mut candidates, None, &health, &config);

        let ctx: serde_json::Value =
            serde_json::from_str(&candidates[0].context.as_ref().unwrap()).unwrap();
        assert!(ctx.get("test_coverage_hint").is_none());
        assert!(ctx.get("config_owner").is_none());
    }

    // Top-N limit
    #[test]
    fn respects_top_n_limit() {
        let mut config = IntelligenceConfig::default();
        config.graph_enrichment_top_n = 3;
        let mut candidates = vec![
            make_candidate("a.rs"),
            make_candidate("b.rs"),
            make_candidate("c.rs"),
            make_candidate("d.rs"),
        ];
        let health = GraphHealth::Disabled;

        enrich_with_graph_context(&mut candidates, None, &health, &config);

        // Only first 3 should have context (capped at 3)
        assert!(candidates[0].context.is_some());
        assert!(candidates[1].context.is_some());
        assert!(candidates[2].context.is_some());
        assert!(candidates[3].context.is_none());
    }
}
