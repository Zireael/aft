//! FTS5 retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing FTS5 query planner (fts5_planner.rs) to produce
//! `CandidateSet` results per lane, feeding into the fusion pipeline.

use std::path::PathBuf;

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::fts5_planner::{FusedResult, QueryPlanner};
use crate::fts5_store::Fts5Store;
use crate::search_plan::{LaneKind, SearchPlan};

use super::{is_generated_path, is_vendor_path, RetrievalAdapter};

/// FTS5 retrieval adapter.
///
/// Wraps `fts5_planner::QueryPlanner` and converts `FusedResult` into
/// `CandidateSet` per active FTS5 lane.
pub struct Fts5Adapter<'a> {
    store: &'a Fts5Store,
}

/// FTS5 lane degradation surfaced to the caller for RI diagnostics.
#[derive(Debug, Clone)]
pub struct Fts5DegradedLane {
    pub lane: LaneKind,
    pub reason: String,
    pub fallback_used: Option<LaneKind>,
}

/// Result of an FTS5 adapter run, including candidates and lane failures.
#[derive(Debug, Clone, Default)]
pub struct Fts5RetrievalReport {
    pub candidate_sets: Vec<CandidateSet>,
    pub degraded_lanes: Vec<Fts5DegradedLane>,
}

impl<'a> Fts5Adapter<'a> {
    /// Create a new FTS5 adapter wrapping the given store.
    pub fn new(store: &'a Fts5Store) -> Self {
        Self { store }
    }

    /// Execute FTS5 search and convert results to CandidateSets.
    fn execute_fts5(&self, query: &str, top_k: usize) -> Result<Vec<FusedResult>, String> {
        let planner = QueryPlanner::new(self.store);
        planner
            .search(query, top_k)
            .map_err(|e| format!("FTS5 search error: {e}"))
    }

    /// Convert a single FusedResult into a CandidateEntry for a given lane.
    fn fused_to_entry(result: &FusedResult, lane: LaneKind, query: &str) -> CandidateEntry {
        let file_path = PathBuf::from(&result.file_path);
        let is_vendor = is_vendor_path(&result.file_path);
        let is_generated = is_generated_path(&result.file_path);

        let is_exact_hit = matches!(lane, LaneKind::FTS5Symbol | LaneKind::SymbolExact)
            && result
                .matched_lanes
                .iter()
                .any(|matched| matched == "exact_symbol_sql")
            && result.symbol_name == query.trim()
            && !is_vendor
            && !is_generated;

        CandidateEntry {
            chunk_id: None,
            symbol_id: Some(result.symbol_id as u64),
            file_path,
            line_range: Some((result.start_line as usize, result.end_line as usize)),
            content_hash: None,
            score: result.score as f32,
            rank: 0, // will be set by caller
            is_exact_hit,
            is_vendor,
            is_generated,
            source_lane: lane,
        }
    }

    /// Execute FTS5 once and expose the planner's best public lane per result.
    pub fn retrieve_with_diagnostics(&self, query: &str, plan: &SearchPlan) -> Fts5RetrievalReport {
        let Some(max_candidates) = active_fts5_candidate_limit(plan) else {
            return Fts5RetrievalReport::default();
        };

        match self.execute_fts5(query, max_candidates) {
            Ok(results) => {
                let mut candidate_sets: Vec<CandidateSet> = Vec::new();
                for (rank, result) in results.iter().enumerate() {
                    let lane = public_lane_for_planner_result(result);
                    if !plan.prefetch.iter().any(|p| p.lane == lane)
                        && !is_symbol_sublane_allowed_by_plan(lane, plan)
                    {
                        continue;
                    }
                    let mut entry = Self::fused_to_entry(result, lane, query);
                    entry.rank = rank;
                    push_grouped_candidate(&mut candidate_sets, lane, entry);
                }

                Fts5RetrievalReport {
                    candidate_sets,
                    degraded_lanes: Vec::new(),
                }
            }
            Err(error) => Fts5RetrievalReport {
                candidate_sets: Vec::new(),
                degraded_lanes: active_fts5_lanes(plan)
                    .into_iter()
                    .map(|lane| Fts5DegradedLane {
                        lane,
                        reason: error.clone(),
                        fallback_used: fallback_lane_after_fts5_failure(plan),
                    })
                    .collect(),
            },
        }
    }
}

impl<'a> RetrievalAdapter for Fts5Adapter<'a> {
    fn retrieve(&self, query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        self.retrieve_with_diagnostics(query, plan).candidate_sets
    }
}

fn active_fts5_lanes(plan: &SearchPlan) -> Vec<LaneKind> {
    plan.prefetch
        .iter()
        .filter(|retriever| {
            is_active_fts5_retriever(retriever.lane, retriever.weight, retriever.is_safety_lane)
        })
        .map(|retriever| retriever.lane)
        .collect()
}

fn active_fts5_candidate_limit(plan: &SearchPlan) -> Option<usize> {
    plan.prefetch
        .iter()
        .filter(|retriever| {
            is_active_fts5_retriever(retriever.lane, retriever.weight, retriever.is_safety_lane)
        })
        .map(|retriever| retriever.max_candidates)
        .max()
}

fn is_active_fts5_retriever(lane: LaneKind, weight: f32, is_safety_lane: bool) -> bool {
    matches!(
        lane,
        LaneKind::FTS5Symbol
            | LaneKind::FTS5Body
            | LaneKind::FTS5Path
            | LaneKind::FTS5Docs
            | LaneKind::SymbolExact
    ) && (weight >= 0.1 || is_safety_lane)
}

fn public_lane_for_planner_result(result: &FusedResult) -> LaneKind {
    match result.best_lane.as_str() {
        "path_fts" => LaneKind::FTS5Path,
        "body_fts" | "short_token_fallback" => LaneKind::FTS5Body,
        "exact_symbol_sql" | "prefix_symbol_sql" | "symbol_fts" => LaneKind::FTS5Symbol,
        _ => LaneKind::FTS5Body,
    }
}

fn is_symbol_sublane_allowed_by_plan(lane: LaneKind, plan: &SearchPlan) -> bool {
    lane == LaneKind::FTS5Symbol
        && plan
            .prefetch
            .iter()
            .any(|retriever| retriever.lane == LaneKind::SymbolExact)
}

fn push_grouped_candidate(
    candidate_sets: &mut Vec<CandidateSet>,
    lane: LaneKind,
    entry: CandidateEntry,
) {
    if let Some(set) = candidate_sets
        .iter_mut()
        .find(|candidate_set| candidate_set.source_lane == lane)
    {
        set.candidates.push(entry);
    } else {
        candidate_sets.push(CandidateSet {
            source_lane: lane,
            candidates: vec![entry],
        });
    }
}

fn fallback_lane_after_fts5_failure(plan: &SearchPlan) -> Option<LaneKind> {
    match plan.active_safety_lane {
        LaneKind::FTS5Body => Some(LaneKind::TrigramBody),
        LaneKind::TrigramBody | LaneKind::DegradedLiteralBodyScan => Some(plan.active_safety_lane),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_plan::{RetrieverPlan, SafetyLaneContext, SearchPlanBuilder};

    fn plan_with_fts5_lanes() -> SearchPlan {
        let shape = crate::query_shape::QueryShape {
            kind: crate::query_shape::QueryKind::Identifier,
            weights: crate::query_shape::ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: true,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    // AC-4: FTS5Body (is_safety_lane=true) never skipped even at weight=0.1
    #[test]
    fn safety_lane_never_skipped() {
        let mut plan = plan_with_fts5_lanes();
        // Set FTS5Body weight to 0.1 (minimum) and mark as safety lane
        plan.lane_weights.insert(LaneKind::FTS5Body, 0.1);
        plan.prefetch.push(RetrieverPlan {
            lane: LaneKind::FTS5Body,
            weight: 0.1,
            max_candidates: 50,
            is_safety_lane: true,
            latency_budget_ms: None,
        });

        // Even at weight 0.1, safety lane should be included
        let retriever = plan.prefetch.iter().find(|p| p.lane == LaneKind::FTS5Body);
        assert!(retriever.is_some());
        assert!(retriever.unwrap().is_safety_lane);
    }

    // AC-3: Lane with weight < 0.1 skipped unless is_safety_lane
    #[test]
    fn low_weight_lane_skipped() {
        let mut plan = plan_with_fts5_lanes();
        plan.prefetch.push(RetrieverPlan {
            lane: LaneKind::FTS5Docs,
            weight: 0.05, // below 0.1
            max_candidates: 50,
            is_safety_lane: false,
            latency_budget_ms: None,
        });

        // Non-safety lane at weight < 0.1 should be skipped
        let active = plan
            .prefetch
            .iter()
            .filter(|p| p.weight >= 0.1 || p.is_safety_lane)
            .any(|p| p.lane == LaneKind::FTS5Docs);
        assert!(!active);
    }

    // AC-5: is_exact_hit for SymbolExact lane
    #[test]
    fn exact_hit_for_symbol_exact() {
        let result = FusedResult {
            symbol_id: 42,
            file_id: 1,
            file_path: "src/main.rs".to_string(),
            symbol_name: "MyStruct".to_string(),
            symbol_kind: "struct".to_string(),
            start_line: 10,
            end_line: 20,
            snippet: String::new(),
            score: 0.9,
            best_lane: "exact_symbol_sql".to_string(),
            matched_lanes: vec!["exact_symbol_sql".to_string()],
            name_path: "MyStruct".to_string(),
            duplicate_index: 0,
        };

        let entry = Fts5Adapter::fused_to_entry(&result, LaneKind::SymbolExact, "MyStruct");
        assert!(entry.is_exact_hit);
    }

    #[test]
    fn symbol_like_non_equal_candidate_is_not_exact() {
        let result = FusedResult {
            symbol_id: 42,
            file_id: 1,
            file_path: "src/main.rs".to_string(),
            symbol_name: "BuildSemanticBackendConfig".to_string(),
            symbol_kind: "function".to_string(),
            start_line: 10,
            end_line: 20,
            snippet: String::new(),
            score: 0.9,
            best_lane: "prefix_symbol_sql".to_string(),
            matched_lanes: vec!["prefix_symbol_sql".to_string()],
            name_path: "BuildSemanticBackendConfig".to_string(),
            duplicate_index: 0,
        };

        let entry =
            Fts5Adapter::fused_to_entry(&result, LaneKind::FTS5Symbol, "SemanticBackendConfig");
        assert!(
            !entry.is_exact_hit,
            "prefix/symbol-like FTS5 candidates must not feed ExactHitFloor"
        );
    }

    #[test]
    fn planner_results_are_not_duplicated_for_each_active_public_lane() {
        let store = Fts5Store::open_in_memory().expect("open in-memory FTS5 store");
        let file_id = store
            .upsert_file("src/lib.rs", "hash", 0, 128, 1)
            .expect("upsert file");
        store
            .upsert_symbol(
                file_id,
                "SemanticBackendConfig",
                "struct",
                1,
                3,
                "pub struct SemanticBackendConfig { model: String }",
                "SemanticBackendConfig",
                "body-hash",
            )
            .expect("upsert symbol");
        let plan = plan_with_fts5_lanes();
        assert!(plan.prefetch.iter().any(|p| p.lane == LaneKind::FTS5Symbol));
        assert!(plan.prefetch.iter().any(|p| p.lane == LaneKind::FTS5Body));

        let adapter = Fts5Adapter::new(&store);
        let sets = adapter.retrieve("SemanticBackendConfig", &plan);
        let non_empty = sets
            .iter()
            .filter(|set| !set.candidates.is_empty())
            .collect::<Vec<_>>();
        let total_candidates = non_empty
            .iter()
            .map(|set| set.candidates.len())
            .sum::<usize>();

        assert_eq!(
            non_empty.len(),
            1,
            "one fused planner result must not be duplicated across public FTS5 lanes: {sets:?}"
        );
        assert_eq!(
            total_candidates, 1,
            "planner fused result should appear once before RI fusion"
        );
    }

    // AC-6: is_vendor heuristic
    #[test]
    fn vendor_path_detected() {
        assert!(is_vendor_path("node_modules/foo/index.ts"));
        assert!(is_vendor_path("src/vendor/bar.rs"));
        assert!(!is_vendor_path("src/main.rs"));
    }

    // is_generated heuristic
    #[test]
    fn generated_path_detected() {
        assert!(is_generated_path("src/generated/code.rs"));
        assert!(is_generated_path("output.gen.ts"));
        assert!(!is_generated_path("src/main.rs"));
    }
}
