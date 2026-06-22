//! Ranking features for Retrieval Intelligence v1.
//!
//! Applies additive score adjustments to FusedCandidates after ExactHitFloor
//! and before reranking. Features are independently toggled via config.

use crate::candidate::FusedCandidate;
use crate::context::AppContext;
use crate::search_plan::{QueryIntent, SearchPlan};
use crate::symbols::Symbol;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AppliedRankingFeature {
    pub feature: &'static str,
    pub delta: f32,
    pub evidence: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RankingFeatureReport {
    pub file: String,
    pub line_range: Option<(usize, usize)>,
    pub original_score: f32,
    pub final_score: f32,
    pub metadata_status: &'static str,
    pub applied: Vec<AppliedRankingFeature>,
}

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
    ctx: &AppContext,
    candidates: &mut [FusedCandidate],
    query: &str,
    plan: &SearchPlan,
    config: &RankingFeaturesConfig,
) -> Vec<RankingFeatureReport> {
    let query_lower = query.to_lowercase();
    let intent = plan.intent;
    let is_test_query = query_lower.contains("test")
        || query_lower.contains("spec")
        || query_lower.contains("fixture")
        || query_lower.contains("mock");

    // Track files in top-3 for same-file coherence
    let mut top_files: Vec<std::path::PathBuf> = Vec::new();

    let mut reports = Vec::with_capacity(candidates.len());

    for (i, candidate) in candidates.iter_mut().enumerate() {
        let original_score = candidate.final_score;
        let mut delta: f32 = 0.0;
        let mut applied = Vec::new();
        let metadata = candidate_symbol_metadata(ctx, candidate, query);

        // (a) ExactDefinitionBoost +0.25
        if config.exact_definition_boost {
            if candidate.is_exact_hit && metadata.exact_definition {
                delta += 0.25;
                applied.push(AppliedRankingFeature {
                    feature: "exact_definition_boost",
                    delta: 0.25,
                    evidence: metadata
                        .matched_symbol
                        .clone()
                        .unwrap_or_else(|| "exact symbol metadata".to_string()),
                });
            }
        }

        // (b) IdentifierStemMatchBoost +0.15
        if config.identifier_stem_match_boost {
            if let Some(symbol_name) = metadata.identifier_stem_match.as_ref() {
                delta += 0.15;
                applied.push(AppliedRankingFeature {
                    feature: "identifier_stem_match_boost",
                    delta: 0.15,
                    evidence: symbol_name.clone(),
                });
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
                    applied.push(AppliedRankingFeature {
                        feature: "path_base_match_boost",
                        delta: 0.10,
                        evidence: base.to_string(),
                    });
                }
            }
        }

        // (d) DocCommentBoost +0.10 — NL queries where doc comment contains query terms
        if config.doc_comment_boost {
            if matches!(
                intent,
                QueryIntent::NaturalLanguage | QueryIntent::RelatedCode
            ) {
                if let Some(evidence) = doc_comment_evidence(candidate, &query_lower) {
                    delta += 0.10;
                    applied.push(AppliedRankingFeature {
                        feature: "doc_comment_boost",
                        delta: 0.10,
                        evidence,
                    });
                }
            }
        }

        // (e) SameFileCoherenceBoost +0.08
        if config.same_file_coherence_boost {
            if i < 3 {
                top_files.push(candidate.file_path.clone());
            } else if top_files.contains(&candidate.file_path) {
                delta += 0.08;
                applied.push(AppliedRankingFeature {
                    feature: "same_file_coherence_boost",
                    delta: 0.08,
                    evidence: "same file as top-3 candidate".to_string(),
                });
            }
        }

        // (f) TestExamplePenalty -0.20
        // WARNING 5: DISABLED when QueryIntent requests tests
        if config.test_example_penalty {
            let is_diagnostic_error =
                matches!(intent, QueryIntent::DiagnosticError) || contains_diagnostic_token(query);
            let queries_tests = is_test_query || is_diagnostic_error;

            if !is_diagnostic_error && !queries_tests {
                if candidate.is_vendor || candidate.is_generated {
                    // Don't double-penalize: ExactHitFloor already excludes vendor from Group A
                } else {
                    // Check if file looks like a test file
                    let path_str = candidate.file_path.to_string_lossy().to_lowercase();
                    if path_str.contains("test") || path_str.contains("spec") {
                        delta -= 0.20;
                        applied.push(AppliedRankingFeature {
                            feature: "test_example_penalty",
                            delta: -0.20,
                            evidence: path_str,
                        });
                    }
                }
            }
        }

        candidate.final_score += delta;
        reports.push(RankingFeatureReport {
            file: candidate.file_path.display().to_string(),
            line_range: candidate.line_range,
            original_score,
            final_score: candidate.final_score,
            metadata_status: metadata.status,
            applied,
        });
    }

    reports
}

#[derive(Debug, Default)]
struct CandidateMetadata {
    status: &'static str,
    matched_symbol: Option<String>,
    exact_definition: bool,
    identifier_stem_match: Option<String>,
}

fn candidate_symbol_metadata(
    ctx: &AppContext,
    candidate: &FusedCandidate,
    query: &str,
) -> CandidateMetadata {
    let Ok(symbols) = ctx.provider().list_symbols(&candidate.file_path) else {
        return CandidateMetadata {
            status: "unavailable",
            ..CandidateMetadata::default()
        };
    };

    let query_tokens = identifier_tokens(query);
    let matching_symbols = symbols
        .iter()
        .filter(|symbol| candidate_intersects_symbol(candidate, symbol))
        .collect::<Vec<_>>();

    if matching_symbols.is_empty() {
        return CandidateMetadata {
            status: "no_matching_symbol_metadata",
            ..CandidateMetadata::default()
        };
    }

    let exact = matching_symbols
        .iter()
        .find(|symbol| symbol.name == query.trim())
        .map(|symbol| render_symbol(symbol));
    let stem_match = matching_symbols
        .iter()
        .find(|symbol| {
            let symbol_lower = symbol.name.to_ascii_lowercase();
            query_tokens
                .iter()
                .any(|token| token.len() > 2 && symbol_lower.contains(token))
        })
        .map(|symbol| render_symbol(symbol));

    CandidateMetadata {
        status: "symbol_metadata",
        exact_definition: exact.is_some(),
        matched_symbol: exact.clone().or_else(|| stem_match.clone()),
        identifier_stem_match: stem_match,
    }
}

fn candidate_intersects_symbol(candidate: &FusedCandidate, symbol: &Symbol) -> bool {
    let Some((candidate_start, candidate_end)) = candidate.line_range else {
        return false;
    };
    let symbol_start = symbol.range.start_line as usize + 1;
    let symbol_end = symbol.range.end_line as usize + 1;
    candidate_start <= symbol_end && candidate_end >= symbol_start
}

fn render_symbol(symbol: &Symbol) -> String {
    if symbol.scope_chain.is_empty() {
        symbol.name.clone()
    } else {
        format!("{}::{}", symbol.scope_chain.join("::"), symbol.name)
    }
}

fn identifier_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn contains_diagnostic_token(query: &str) -> bool {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| {
            let upper = token.to_ascii_uppercase();
            (upper.len() >= 5
                && upper.starts_with('E')
                && upper[1..].chars().all(|c| c.is_ascii_digit()))
                || (upper.len() >= 6
                    && upper.starts_with("TS")
                    && upper[2..].chars().all(|c| c.is_ascii_digit()))
                || upper.starts_with("ERR_")
        })
}

fn doc_comment_evidence(candidate: &FusedCandidate, query_lower: &str) -> Option<String> {
    let (start, _) = candidate.line_range?;
    let content = std::fs::read_to_string(&candidate.file_path).ok()?;
    let lines = content.lines().collect::<Vec<_>>();
    let first_line = start.saturating_sub(1).min(lines.len());
    let window_start = first_line.saturating_sub(4).min(first_line);
    let query_tokens = identifier_tokens(query_lower);
    for line in &lines[window_start..first_line.min(lines.len())] {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("///")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("/**")
            || trimmed.starts_with('*'))
        {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if query_tokens
            .iter()
            .any(|token| token.len() > 2 && lower.contains(token))
        {
            return Some(trimmed.chars().take(120).collect());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateProvenance, LaneContribution};
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::query_shape::{QueryKind, QueryShape, ShapeWeights};
    use crate::search_plan::{LaneKind, SafetyLaneContext, SearchPlanBuilder};
    use std::path::PathBuf;

    fn ctx() -> AppContext {
        AppContext::new(Box::new(TreeSitterProvider::new()), Config::default())
    }

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
        let dir = tempfile::tempdir().expect("temp dir");
        let def_path = dir.path().join("def.rs");
        std::fs::write(&def_path, "pub struct MyStruct;\n").expect("write definition");
        let mut candidates = vec![
            make_candidate(&def_path.display().to_string(), 0.5, true, false, false), // exact hit
            make_candidate("ref.rs", 0.5, false, false, false),                       // reference
        ];

        let ctx = ctx();
        apply_ranking_features(&ctx, &mut candidates, "MyStruct", &plan, &config);

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

        let ctx = ctx();
        apply_ranking_features(&ctx, &mut candidates, "E0433", &plan, &config);

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

        let ctx = ctx();
        apply_ranking_features(&ctx, &mut candidates, "MyStruct", &plan, &config);

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
        let config = RankingFeaturesConfig {
            exact_definition_boost: false,
            ..Default::default()
        };
        let mut candidates = vec![make_candidate("def.rs", 0.5, true, false, false)];

        let ctx = ctx();
        apply_ranking_features(&ctx, &mut candidates, "MyStruct", &plan, &config);

        // Should NOT have boost
        assert!(
            (candidates[0].final_score - 0.5).abs() < 0.01,
            "boost should be disabled, got {}",
            candidates[0].final_score
        );
    }
}
