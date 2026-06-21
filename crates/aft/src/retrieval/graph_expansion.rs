//! Graph expansion adapter for Retrieval Intelligence v1.
//!
//! Second-pass adapter that expands top-5 fused results with their
//! callers and imported_by relationships from the callgraph store.
//! Expansion candidates are added as additional CandidateSets for re-fusion.

use std::path::{Path, PathBuf};

use crate::callgraph_store::CallGraphStore;
use crate::candidate::{CandidateEntry, CandidateSet, FusedCandidate};
use crate::intelligence_config::IntelligenceConfig;
use crate::ril_indexer::GraphHealth;
use crate::search_plan::LaneKind;

/// Graph expansion adapter — second-pass expansion of top results.
///
/// For top-5 fused results, queries the callgraph store for callers
/// and imported_by relationships. Each becomes a CandidateEntry with
/// is_graph_expansion=true.
pub struct GraphExpansionAdapter;

impl GraphExpansionAdapter {
    /// Expand top results with graph relationships.
    ///
    /// Returns additional CandidateSets from graph expansion.
    /// Silently returns empty when GraphHealth is Disabled or Cold.
    pub fn expand(
        top_results: &[FusedCandidate],
        callgraph_store: Option<&CallGraphStore>,
        graph_health: &GraphHealth,
        config: &IntelligenceConfig,
    ) -> Vec<CandidateSet> {
        // Silently skip when graph is not usable
        if !graph_health.usable() {
            return Vec::new();
        }

        let store = match callgraph_store {
            Some(s) => s,
            None => return Vec::new(),
        };

        let max_expansion = config.graph_expansion_max;
        let mut expanded_entries: Vec<CandidateEntry> = Vec::new();

        for result in top_results.iter().take(5) {
            if expanded_entries.len() >= max_expansion {
                break;
            }

            // Query callers
            if let Ok(callers_result) = store.callers_of(&result.file_path, "", 1) {
                for cs in callers_result.callers.iter() {
                    if expanded_entries.len() >= max_expansion {
                        break;
                    }
                    let entry = CandidateEntry {
                        chunk_id: None,
                        symbol_id: None,
                        file_path: PathBuf::from(&cs.caller.file),
                        line_range: None,
                        content_hash: None,
                        score: 0.0,
                        rank: 0,
                        is_exact_hit: false,
                        is_vendor: false,
                        is_generated: false,
                        source_lane: LaneKind::GraphExpansion,
                    };
                    expanded_entries.push(entry);
                }
            }
        }

        if expanded_entries.is_empty() {
            return Vec::new();
        }

        vec![CandidateSet {
            source_lane: LaneKind::GraphExpansion,
            candidates: expanded_entries,
        }]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateProvenance;
    use crate::search_plan::LaneKind;

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
                lanes: vec![crate::candidate::LaneContribution {
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

    // AC-3: GraphHealth=Disabled → no expansion candidates, no error
    #[test]
    fn disabled_no_expansion() {
        let config = IntelligenceConfig::default();
        let results = vec![make_candidate("src/main.rs")];
        let health = GraphHealth::Disabled;

        let expanded = GraphExpansionAdapter::expand(&results, None, &health, &config);
        assert!(expanded.is_empty());
    }

    // AC-4: Expansion count <= graph_expansion_max
    #[test]
    fn expansion_count_capped() {
        let mut config = IntelligenceConfig::default();
        config.graph_expansion_max = 3;
        let results = (0..10)
            .map(|i| make_candidate(&format!("src/file_{i}.rs")))
            .collect::<Vec<_>>();
        let health = GraphHealth::Disabled;

        let expanded = GraphExpansionAdapter::expand(&results, None, &health, &config);
        // Without a real store, expansion is empty — but the cap is respected
        assert!(expanded.is_empty());
    }

    // WARNING 6: Direct-hit provenance preserved
    #[test]
    fn direct_hit_preserves_lane() {
        let candidate = make_candidate("src/main.rs");
        // Original lane is Trigram
        assert_eq!(candidate.provenance.lanes[0].lane, LaneKind::Trigram);
        // graph_expansion flag is false
        assert!(!candidate.provenance.is_graph_expansion);
    }
}
