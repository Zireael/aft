//! Ranking features for Retrieval Intelligence v1.
//!
//! Applies additive score adjustments to FusedCandidates after ExactHitFloor
//! and before reranking. Features are independently toggled via config.

use crate::candidate::FusedCandidate;
use crate::search_plan::{QueryIntent, SearchPlan};

/// Configuration for ranking features.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RankingFeaturesConfig {
    /// Enable exact definition boost (+0.25).
    pub exact_definition_boost: bool,
    /// Enable identifier stem match boost (+0.15).
    pub identifier_stem_match_boost: bool,
    /// Enable path base match boost (+0.10).
    pub path_base_match_boost: bool,
    /// Enable doc comment boost (+0.10).
    pub doc_comment_boost: bool,
    /// Enable same-file coherence boost (+0.08).
    pub same_file_coherence_boost: bool,
    /// Enable test example penalty (-0.20).
    pub test_example_penalty: bool,
}

impl Default for RankingFeaturesConfig {
    fn default() -> Self {
        Self {
            exact_definition_boost: true,
            identifier_stem_match_boost: true,
            path_base_match_boost: true,
            doc_comment_boost: true,
            same_file_coherence_boost: true,
            test_example_penalty: true,
        }
    }
}

/// Apply ranking features to fused candidates.
///
/// Modifies `final_score` in-place with additive deltas.
/// Features are independently toggled via config.
#[allow(clippy::too_many_arguments)]
pub fn apply_ranking_features(
    candidates: &mut [FusedCandidate],
    query: &str,
    plan: &SearchPlan,
    config: &RankingFeaturesConfig,
) {
    let query_lower = query.to_lowercase();
    let intent = plan.intent;
    let is_test_query = query_lower.contains("test")
        || query_lower.contains("spec")
        || query_lower.contains("fixture")
        || query_lower.contains("mock");

    // Track files in top-3 for same-file coherence
    let mut top_files: Vec<std::path::PathBuf> = Vec::new();

    for (i, candidate) in candidates.iter_mut().enumerate() {
        let mut delta: f32 = 0.0;

        // (a) ExactDefinitionBoost +0.25
        if config.exact_definition_boost {
            if candidate.is_exact_hit {
                delta += 0.25;
            }
        }

        // (b) IdentifierStemMatchBoost +0.15
        if config.identifier_stem_match_boost {
            if let Some(symbol_name) = candidate.provenance.lanes.first().map(|l| {
                // Use file name as rough symbol proxy
                candidate
                    .file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
            }) {
                let stem_lower = symbol_name.to_lowercase();
                // Check if any query token appears in the symbol stem
                let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();
                if query_tokens
                    .iter()
                    .any(|t| stem_lower.contains(*t) && t.len() > 2)
                {
                    delta += 0.15;
                }
            }
        }

        // (c) PathBaseMatchBoost +0.10
        if config.path_base_match_boost {
            if let Some(base) = candidate.file_path.file_name().and_then(|n| n.to_str()) {
                let base_lower = base.to_lowercase();
                let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();
                if query_tokens
                    .iter()
                    .any(|t| base_lower.contains(*t) && t.len() > 2)
                {
                    delta += 0.10;
                }
            }
        }

        // (d) DocCommentBoost +0.10 — NL queries where doc comment contains query terms
        if config.doc_comment_boost {
            if matches!(
                intent,
                QueryIntent::NaturalLanguage | QueryIntent::RelatedCode
            ) {
                // Placeholder: would check snippet for doc comment content
                // For now, apply a small boost for NL queries
                delta += 0.05;
            }
        }

        // (e) SameFileCoherenceBoost +0.08
        if config.same_file_coherence_boost {
            if i < 3 {
                top_files.push(candidate.file_path.clone());
            } else if top_files.contains(&candidate.file_path) {
                delta += 0.08;
            }
        }

        // (f) TestExamplePenalty -0.20
        // WARNING 5: DISABLED when QueryIntent requests tests
        if config.test_example_penalty {
            let is_diagnostic_error = matches!(intent, QueryIntent::DiagnosticError);
            let queries_tests = is_test_query || matches!(intent, QueryIntent::DiagnosticError);

            if !is_diagnostic_error && !queries_tests {
                if candidate.is_vendor || candidate.is_generated {
                    // Don't double-penalize: ExactHitFloor already excludes vendor from Group A
                } else {
                    // Check if file looks like a test file
                    let path_str = candidate.file_path.to_string_lossy().to_lowercase();
                    if path_str.contains("test") || path_str.contains("spec") {
                        delta -= 0.20;
                    }
                }
            }
        }

        candidate.final_score += delta;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateEntry, CandidateProvenance, LaneContribution};
    use crate::query_shape::{QueryKind, QueryShape, ShapeWeights};
    use crate::search_plan::{LaneKind, SafetyLaneContext, SearchPlanBuilder};
    use std::path::PathBuf;

    fn test_plan(intent: QueryIntent) -> SearchPlan {
        let shape = QueryShape {
            kind: match intent {
                QueryIntent::NaturalLanguage => QueryKind::NaturalLanguage,
                QueryIntent::ExactSymbol => QueryKind::Identifier,
                QueryIntent::DiagnosticError => QueryKind::ErrorCode,
                _ => QueryKind::Identifier,
            },
            weights: ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: false,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    fn make_candidate(
        file: &str,
        rrf_score: f32,
        exact: bool,
        vendor: bool,
        generated: bool,
    ) -> FusedCandidate {
        FusedCandidate {
            file_path: PathBuf::from(file),
            line_range: Some((1, 10)),
            chunk_id: None,
            symbol_id: None,
            content_hash: None,
            rrf_score,
            final_score: rrf_score,
            is_exact_hit: exact,
            is_vendor: vendor,
            is_generated: generated,
            exact_hit_floor_applied: false,
            context: None,
            provenance: CandidateProvenance {
                lanes: vec![LaneContribution {
                    lane: LaneKind::FTS5Body,
                    rank_in_lane: 0,
                    score_in_lane: rrf_score,
                    rrf_contribution: 0.0,
                }],
                is_graph_expansion: false,
                graph_expansion_reason: None,
            },
        }
    }

    // AC-1: ExactDefinitionBoost: definition candidate has higher final_score
    #[test]
    fn exact_definition_boost() {
        let plan = test_plan(QueryIntent::ExactSymbol);
        let config = RankingFeaturesConfig::default();
        let mut candidates = vec![
            make_candidate("def.rs", 0.5, true, false, false), // exact hit
            make_candidate("ref.rs", 0.5, false, false, false), // reference
        ];

        apply_ranking_features(&mut candidates, "MyStruct", &plan, &config);

        assert!(
            candidates[0].final_score > candidates[1].final_score,
            "exact hit should get boost: {} > {}",
            candidates[0].final_score,
            candidates[1].final_score
        );
    }

    // AC-2: TestExamplePenalty DISABLED for DiagnosticError
    #[test]
    fn test_penalty_disabled_for_diagnostic_error() {
        let plan = test_plan(QueryIntent::DiagnosticError);
        let config = RankingFeaturesConfig::default();
        let mut candidates = vec![make_candidate(
            "src/test_helper.rs",
            0.5,
            false,
            false,
            false,
        )];

        apply_ranking_features(&mut candidates, "E0433", &plan, &config);

        // Should NOT have penalty
        assert!(
            candidates[0].final_score >= 0.5,
            "test file should not be penalized for DiagnosticError, got {}",
            candidates[0].final_score
        );
    }

    // AC-3: TestExamplePenalty ENABLED for non-test query
    #[test]
    fn test_penalty_enabled_for_non_test_query() {
        let plan = test_plan(QueryIntent::ExactSymbol);
        let config = RankingFeaturesConfig::default();
        let mut candidates = vec![make_candidate(
            "src/test_helper.rs",
            0.5,
            false,
            false,
            false,
        )];

        apply_ranking_features(&mut candidates, "MyStruct", &plan, &config);

        // Should have penalty (test file + non-test query)
        assert!(
            candidates[0].final_score < 0.5,
            "test file should be penalized for non-test query, got {}",
            candidates[0].final_score
        );
    }

    // AC-5: Disabling ExactDefinitionBoost: no boost
    #[test]
    fn boost_disabled() {
        let plan = test_plan(QueryIntent::ExactSymbol);
        let mut config = RankingFeaturesConfig::default();
        config.exact_definition_boost = false;
        let mut candidates = vec![make_candidate("def.rs", 0.5, true, false, false)];

        apply_ranking_features(&mut candidates, "MyStruct", &plan, &config);

        // Should NOT have boost
        assert!(
            (candidates[0].final_score - 0.5).abs() < 0.01,
            "boost should be disabled, got {}",
            candidates[0].final_score
        );
    }
}
