//! SearchPlan types for Retrieval Intelligence v1.
//!
//! Defines the intent-aware search plan schema (§A.1) with safety-lane
//! fallback chain, lane weights, and diagnostic structures. Types only —
//! no wiring into search dispatch yet (see t1c).
//!
//! Key invariants:
//! - QueryIntent is a weighting prior, NOT a hard router.
//! - FTS5Body safety lane weight >= 0.1 for ALL intents in auto-mode.
//! - active_safety_lane is one of: FTS5Body, TrigramBody, DegradedLiteralBodyScan.
//! - active_safety_lane is included in mandatory_lanes for auto-mode plans.

use std::collections::HashMap;

use crate::query_shape::{QueryKind, QueryShape};

// Re-export ContextBudget from the context_budget module to avoid duplication.
pub use crate::context_budget::ContextBudget;

// ---------------------------------------------------------------------------
// QueryIntent — derived from QueryShape.kind
// ---------------------------------------------------------------------------

/// Intent classification for weighting purposes only.
/// This is a prior over lane weights, NOT a hard routing decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum QueryIntent {
    /// Free-form natural language description of desired code.
    NaturalLanguage,
    /// Exact symbol name (e.g. `parseConfig`, `MyStruct`).
    ExactSymbol,
    /// Symbol prefix or stem (e.g. `parse_*`, `My`).
    SymbolPrefix,
    /// File path or path-like query.
    PathLookup,
    /// Literal text / substring search.
    Literal,
    /// Regex or pattern query.
    Regex,
    /// Error code, hex code, or diagnostic message.
    DiagnosticError,
    /// Related code / call graph traversal query.
    RelatedCode,
    /// Mixed intent with multiple signal types.
    Mixed,
}

// ---------------------------------------------------------------------------
// LaneKind — retrieval lane identifiers
// ---------------------------------------------------------------------------

/// Identifies a retrieval lane in the search plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LaneKind {
    /// Trigram / grep-style lexical search.
    Trigram,
    /// FTS5 symbol-indexed search.
    FTS5Symbol,
    /// FTS5 body/content search (safety lane).
    FTS5Body,
    /// FTS5 path-indexed search.
    FTS5Path,
    /// FTS5 documentation search.
    FTS5Docs,
    /// Semantic / embedding-based search.
    Semantic,
    /// Exact symbol match (exact definition lookup).
    SymbolExact,
    /// Graph-based expansion (callers/callees).
    GraphExpansion,
    /// Trigram body search — fallback safety lane when FTS5 unavailable.
    TrigramBody,
    /// Degraded literal body scan — last-resort error-condition fallback.
    DegradedLiteralBodyScan,
}

// ---------------------------------------------------------------------------
// Supporting types per §A.1
// ---------------------------------------------------------------------------

/// A single retrieval lane plan.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RetrieverPlan {
    pub lane: LaneKind,
    pub weight: f32,
    pub max_candidates: usize,
    pub is_safety_lane: bool,
    /// If exceeded: emit lane_timeout=true, continue other lanes.
    pub latency_budget_ms: Option<u64>,
}

/// A lane that was explicitly suppressed with a reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SuppressedLane {
    pub lane: LaneKind,
    pub reason: String,
}

/// Fusion strategy for combining lane results.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FusionPlan {
    /// Reciprocal Rank Fusion k parameter (default 60).
    pub rrf_k: u32,
    /// Maximum exact hits promoted to Group A (default 5).
    pub exact_hit_floor_n: usize,
}

impl Default for FusionPlan {
    fn default() -> Self {
        Self {
            rrf_k: 60,
            exact_hit_floor_n: 5,
        }
    }
}

/// Ranking profile controlling boost/penalty application.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RankingProfile {
    /// Enable exact-definition boost.
    pub exact_definition_boost: bool,
    /// Enable identifier stem match boost.
    pub stem_match_boost: bool,
    /// Enable path base match boost.
    pub path_base_match_boost: bool,
    /// Enable doc-comment boost (NL queries only).
    pub doc_comment_boost: bool,
    /// Enable same-file coherence boost.
    pub same_file_coherence_boost: bool,
    /// Enable test/example/stub penalty (disabled when QueryIntent requests tests).
    pub test_example_penalty: bool,
}

/// Reranker plan.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankPlan {
    /// Whether reranking is enabled.
    pub enabled: bool,
    /// Maximum candidates to send to reranker.
    pub max_candidates: usize,
}

/// Diagnostic verbosity level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiagnosticLevel {
    /// No diagnostics.
    #[default]
    Off,
    /// Basic lane provenance and timing.
    Basic,
    /// Full lane weights, scores, and candidate details.
    Full,
}

/// Feature flag state for diagnostics output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FeatureFlagState {
    /// Feature flag is off (old behavior).
    #[default]
    Off,
    /// Feature flag is on (new behavior).
    On,
}

// ---------------------------------------------------------------------------
// SearchPlan — the complete plan per §A.1
// ---------------------------------------------------------------------------

/// Complete search plan for a single query, per §A.1 schema contract.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchPlan {
    /// Derived intent (weighting prior only).
    pub intent: QueryIntent,
    /// Lane weights: lane → weight (higher = more influence).
    pub lane_weights: HashMap<LaneKind, f32>,
    /// Lanes that must run regardless of weight.
    pub mandatory_lanes: Vec<LaneKind>,
    /// Lanes explicitly suppressed with reason.
    pub suppressed_lanes: Vec<SuppressedLane>,
    /// Prefetch lanes to run before main fusion.
    pub prefetch: Vec<RetrieverPlan>,
    /// Fusion strategy.
    pub fusion: FusionPlan,
    /// Ranking profile (boosts/penalties).
    pub ranking_profile: RankingProfile,
    /// Context budget for candidate content.
    pub context_budget: ContextBudget,
    /// Reranker configuration.
    pub rerank: RerankPlan,
    /// Diagnostic verbosity.
    pub diagnostics: DiagnosticLevel,
    /// The active safety lane (FTS5Body | TrigramBody | DegradedLiteralBodyScan).
    pub active_safety_lane: LaneKind,
    /// Feature flag state for diagnostics.
    pub feature_flag_state: FeatureFlagState,
}

// ---------------------------------------------------------------------------
// DegradedLiteralBodyScan limits
// ---------------------------------------------------------------------------

/// Hard limits for the DegradedLiteralBodyScan last-resort fallback.
pub const DEGRADED_MAX_FILES: usize = 100;
pub const DEGRADED_MAX_RESULTS: usize = 50;
pub const DEGRADED_MAX_TIME_MS: u64 = 250;

// ---------------------------------------------------------------------------
// Safety lane weight table (ADR-001 v3.1)
// ---------------------------------------------------------------------------

/// Returns lane weights for a given intent.
///
/// Invariants enforced:
/// - FTS5Body weight >= 0.1 for ALL intents (safety floor).
/// - Regex intent: FTS5Body = 0.1 (near-zero, documented).
fn weights_for_intent(intent: QueryIntent) -> Vec<(LaneKind, f32)> {
    match intent {
        QueryIntent::NaturalLanguage => vec![
            (LaneKind::Semantic, 1.5),
            (LaneKind::FTS5Body, 1.0), // safety lane
            (LaneKind::FTS5Docs, 0.8),
            (LaneKind::FTS5Symbol, 0.6),
            (LaneKind::Trigram, 0.4),
        ],
        QueryIntent::ExactSymbol => vec![
            (LaneKind::FTS5Symbol, 3.0),
            (LaneKind::Trigram, 2.0),
            (LaneKind::FTS5Body, 0.5), // safety floor
            (LaneKind::Semantic, 0.4),
        ],
        QueryIntent::SymbolPrefix => vec![
            (LaneKind::FTS5Symbol, 2.0),
            (LaneKind::Trigram, 1.5),
            (LaneKind::FTS5Body, 0.3), // safety floor
            (LaneKind::Semantic, 0.3),
        ],
        QueryIntent::PathLookup => vec![
            (LaneKind::Trigram, 3.0),
            (LaneKind::FTS5Path, 2.0),
            (LaneKind::FTS5Body, 0.2), // safety floor
            (LaneKind::Semantic, 0.1),
        ],
        QueryIntent::DiagnosticError => vec![
            (LaneKind::Trigram, 2.5),
            (LaneKind::FTS5Body, 1.0),
            (LaneKind::FTS5Symbol, 0.8),
            (LaneKind::Semantic, 0.3),
        ],
        QueryIntent::Regex => vec![
            (LaneKind::Trigram, 3.0),
            // FTS5Body safety floor at 0.1: near-zero for regex because regex
            // is inherently a trigram/grep pattern, but the safety lane must
            // always remain active in auto-mode per DP-003 / INV-001.
            (LaneKind::FTS5Body, 0.1),
            (LaneKind::Semantic, 0.05),
        ],
        QueryIntent::Literal => vec![
            (LaneKind::Trigram, 3.0),
            (LaneKind::FTS5Body, 0.5),
            (LaneKind::FTS5Symbol, 0.3),
            (LaneKind::Semantic, 0.2),
        ],
        QueryIntent::RelatedCode => vec![
            (LaneKind::GraphExpansion, 2.0),
            (LaneKind::Semantic, 1.0),
            (LaneKind::FTS5Body, 0.5), // safety floor
            (LaneKind::Trigram, 0.3),
        ],
        QueryIntent::Mixed => vec![
            (LaneKind::Semantic, 1.0),
            (LaneKind::FTS5Body, 1.0), // safety lane
            (LaneKind::FTS5Symbol, 1.0),
            (LaneKind::Trigram, 1.0),
            (LaneKind::FTS5Docs, 1.0),
            (LaneKind::FTS5Path, 1.0),
        ],
    }
}

// ---------------------------------------------------------------------------
// QueryIntent mapping from QueryKind
// ---------------------------------------------------------------------------

/// Map the existing QueryKind to the new QueryIntent.
///
/// This mapping is used during the transition period until QueryKind is
/// fully replaced by QueryIntent in the classify() path.
pub fn intent_for_kind(kind: QueryKind) -> QueryIntent {
    match kind {
        QueryKind::NaturalLanguage => QueryIntent::NaturalLanguage,
        QueryKind::Identifier => QueryIntent::ExactSymbol,
        QueryKind::ErrorCode => QueryIntent::DiagnosticError,
        QueryKind::Path => QueryIntent::PathLookup,
        QueryKind::Regex => QueryIntent::Regex,
        QueryKind::Mixed => QueryIntent::Mixed,
    }
}

// ---------------------------------------------------------------------------
// Safety lane resolution
// ---------------------------------------------------------------------------

/// Resolution context for determining the active safety lane.
#[derive(Debug, Clone, Copy)]
pub struct SafetyLaneContext {
    /// Whether FTS5 indexing is available and ready.
    pub fts5_available: bool,
    /// Whether the search index (trigram) is ready.
    pub search_index_ready: bool,
}

/// Resolve the active safety lane from the fallback chain:
/// FTS5Body → TrigramBody → DegradedLiteralBodyScan
pub fn resolve_safety_lane(ctx: &SafetyLaneContext) -> LaneKind {
    if ctx.fts5_available {
        LaneKind::FTS5Body
    } else if ctx.search_index_ready {
        LaneKind::TrigramBody
    } else {
        LaneKind::DegradedLiteralBodyScan
    }
}

// ---------------------------------------------------------------------------
// SearchPlanBuilder
// ---------------------------------------------------------------------------

/// Builds a SearchPlan from a QueryShape.
pub struct SearchPlanBuilder;

impl SearchPlanBuilder {
    /// Build a SearchPlan from a QueryShape and safety-lane context.
    ///
    /// In auto-mode (no explicit strict_* hint):
    /// - active_safety_lane is included in mandatory_lanes.
    /// - All lane weights respect the safety floor.
    pub fn from_query_shape(shape: &QueryShape, safety_ctx: &SafetyLaneContext) -> SearchPlan {
        let intent = intent_for_kind(shape.kind);
        let active_safety_lane = resolve_safety_lane(safety_ctx);

        let raw_weights = weights_for_intent(intent);
        let mut lane_weights: HashMap<LaneKind, f32> = raw_weights.into_iter().collect();

        // Enforce safety floor: the active safety lane must be >= 0.1
        let safety_weight = lane_weights.entry(active_safety_lane).or_insert(0.0);
        if *safety_weight < 0.1 {
            *safety_weight = 0.1;
        }

        // mandatory_lanes always includes the safety lane in auto-mode
        let mandatory_lanes = vec![active_safety_lane];

        // Build RetrieverPlan entries for lanes with non-zero weight
        let prefetch: Vec<RetrieverPlan> = lane_weights
            .iter()
            .filter(|(_, &w)| w > 0.0)
            .map(|(&lane, &weight)| {
                let is_safety = lane == active_safety_lane;
                let max_candidates = match lane {
                    LaneKind::DegradedLiteralBodyScan => DEGRADED_MAX_RESULTS,
                    _ => 50,
                };
                let latency_budget_ms = match lane {
                    LaneKind::DegradedLiteralBodyScan => Some(DEGRADED_MAX_TIME_MS),
                    LaneKind::Semantic => Some(500),
                    _ => None,
                };
                RetrieverPlan {
                    lane,
                    weight,
                    max_candidates,
                    is_safety_lane: is_safety,
                    latency_budget_ms,
                }
            })
            .collect();

        SearchPlan {
            intent,
            lane_weights,
            mandatory_lanes,
            suppressed_lanes: Vec::new(),
            prefetch,
            fusion: FusionPlan::default(),
            ranking_profile: RankingProfile::default(),
            context_budget: ContextBudget {
                total_tokens: 4000,
                per_candidate_tokens: 300,
                min_candidate_chars: 80,
                mode: crate::context_budget::ContextMode::Auto,
                enrich_pool: crate::context_budget::EnrichPool::FusionPool,
                rerank_min_enriched_ratio: 0.5,
            },
            rerank: RerankPlan {
                enabled: false,
                max_candidates: 20,
            },
            diagnostics: DiagnosticLevel::Off,
            active_safety_lane,
            feature_flag_state: FeatureFlagState::Off,
        }
    }

    /// Resolve a profile string to a ContextBudget.
    ///
    /// Returns Err with a descriptive message for unknown profiles.
    pub fn resolve_profile(profile: &str) -> Result<ContextBudget, String> {
        match profile {
            "agent_fast" => Ok(ContextBudget::agent_fast()),
            "agent_deep" => Ok(ContextBudget::agent_deep()),
            "symbol_exact" => Ok(ContextBudget::symbol_exact()),
            other => Err(format!("unknown context profile: {other}")),
        }
    }

    /// Build a SearchPlan with a specific profile override.
    pub fn from_query_shape_with_profile(
        shape: &QueryShape,
        safety_ctx: &SafetyLaneContext,
        profile: &str,
    ) -> Result<SearchPlan, String> {
        let mut plan = Self::from_query_shape(shape, safety_ctx);
        plan.context_budget = Self::resolve_profile(profile)?;
        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_for(kind: QueryKind) -> QueryShape {
        QueryShape {
            kind,
            weights: crate::query_shape::ShapeWeights {
                semantic: 0.5,
                lexical: 0.5,
                should_use_lexical: false,
            },
        }
    }

    fn fts5_ctx() -> SafetyLaneContext {
        SafetyLaneContext {
            fts5_available: true,
            search_index_ready: true,
        }
    }

    fn no_fts5_ctx() -> SafetyLaneContext {
        SafetyLaneContext {
            fts5_available: false,
            search_index_ready: true,
        }
    }

    fn no_index_ctx() -> SafetyLaneContext {
        SafetyLaneContext {
            fts5_available: false,
            search_index_ready: false,
        }
    }

    // AC-2: from_query_shape handles all QueryKind variants without panic
    #[test]
    fn builder_handles_all_query_kinds() {
        for kind in [
            QueryKind::NaturalLanguage,
            QueryKind::Identifier,
            QueryKind::ErrorCode,
            QueryKind::Path,
            QueryKind::Regex,
            QueryKind::Mixed,
        ] {
            let shape = shape_for(kind);
            let plan = SearchPlanBuilder::from_query_shape(&shape, &fts5_ctx());
            assert_eq!(plan.intent, intent_for_kind(kind), "intent for {kind:?}");
        }
    }

    // AC-3: active_safety_lane is always one of the three valid lanes
    #[test]
    fn active_safety_lane_is_always_valid() {
        let valid = [
            LaneKind::FTS5Body,
            LaneKind::TrigramBody,
            LaneKind::DegradedLiteralBodyScan,
        ];
        for kind in [
            QueryKind::NaturalLanguage,
            QueryKind::Identifier,
            QueryKind::ErrorCode,
            QueryKind::Path,
            QueryKind::Regex,
            QueryKind::Mixed,
        ] {
            for ctx in [fts5_ctx(), no_fts5_ctx(), no_index_ctx()] {
                let shape = shape_for(kind);
                let plan = SearchPlanBuilder::from_query_shape(&shape, &ctx);
                assert!(
                    valid.contains(&plan.active_safety_lane),
                    "active_safety_lane {:?} not valid for {kind:?} with ctx {:?}",
                    plan.active_safety_lane,
                    ctx
                );
            }
        }
    }

    // AC-4: FTS5Body weight >= 0.1 for ALL intents
    #[test]
    fn fts5body_safety_floor_for_all_intents() {
        let intents = [
            QueryIntent::NaturalLanguage,
            QueryIntent::ExactSymbol,
            QueryIntent::SymbolPrefix,
            QueryIntent::PathLookup,
            QueryIntent::DiagnosticError,
            QueryIntent::Regex,
            QueryIntent::Literal,
            QueryIntent::RelatedCode,
            QueryIntent::Mixed,
        ];
        for intent in intents {
            let weights = weights_for_intent(intent);
            let fts5_body_weight = weights
                .iter()
                .find(|(lane, _)| *lane == LaneKind::FTS5Body)
                .map(|(_, w)| *w)
                .unwrap_or(0.0);
            assert!(
                fts5_body_weight >= 0.1,
                "FTS5Body weight {fts5_body_weight} < 0.1 for {intent:?}"
            );
        }
    }

    // AC-5: With FTS5 disabled: active_safety_lane = TrigramBody
    #[test]
    fn trigram_body_fallback_when_fts5_disabled() {
        for kind in [
            QueryKind::NaturalLanguage,
            QueryKind::Identifier,
            QueryKind::ErrorCode,
            QueryKind::Path,
            QueryKind::Regex,
            QueryKind::Mixed,
        ] {
            let shape = shape_for(kind);
            let plan = SearchPlanBuilder::from_query_shape(&shape, &no_fts5_ctx());
            assert_eq!(
                plan.active_safety_lane,
                LaneKind::TrigramBody,
                "TrigramBody fallback for {kind:?}"
            );
        }
    }

    // AC-6: Regex intent: FTS5Body weight == 0.1
    #[test]
    fn regex_fts5body_weight_is_safety_floor() {
        let weights = weights_for_intent(QueryIntent::Regex);
        let fts5_body = weights
            .iter()
            .find(|(lane, _)| *lane == LaneKind::FTS5Body)
            .expect("Regex must have FTS5Body weight");
        assert_eq!(
            fts5_body.1, 0.1,
            "Regex FTS5Body must be exactly 0.1 (safety floor)"
        );
    }

    // AC-7: ExactSymbol produces higher FTS5Symbol weight than NaturalLanguage
    #[test]
    fn exact_symbol_higher_fts5_symbol_weight() {
        let exact_weights = weights_for_intent(QueryIntent::ExactSymbol);
        let nl_weights = weights_for_intent(QueryIntent::NaturalLanguage);

        let exact_fts5_sym = exact_weights
            .iter()
            .find(|(lane, _)| *lane == LaneKind::FTS5Symbol)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        let nl_fts5_sym = nl_weights
            .iter()
            .find(|(lane, _)| *lane == LaneKind::FTS5Symbol)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);

        assert!(
            exact_fts5_sym > nl_fts5_sym,
            "ExactSymbol FTS5Symbol {exact_fts5_sym} must be > NaturalLanguage {nl_fts5_sym}"
        );
    }

    // AC-8: active_safety_lane is in mandatory_lanes
    #[test]
    fn safety_lane_in_mandatory_lanes() {
        for kind in [
            QueryKind::NaturalLanguage,
            QueryKind::Identifier,
            QueryKind::ErrorCode,
            QueryKind::Path,
            QueryKind::Regex,
            QueryKind::Mixed,
        ] {
            for ctx in [fts5_ctx(), no_fts5_ctx(), no_index_ctx()] {
                let shape = shape_for(kind);
                let plan = SearchPlanBuilder::from_query_shape(&shape, &ctx);
                assert!(
                    plan.mandatory_lanes.contains(&plan.active_safety_lane),
                    "active_safety_lane {:?} not in mandatory_lanes for {kind:?}",
                    plan.active_safety_lane
                );
            }
        }
    }

    // AC-9: DegradedLiteralBodyScan has limits and degraded_lanes reason
    #[test]
    fn degraded_literal_has_limits() {
        let ctx = no_index_ctx();
        let shape = shape_for(QueryKind::NaturalLanguage);
        let plan = SearchPlanBuilder::from_query_shape(&shape, &ctx);

        assert_eq!(plan.active_safety_lane, LaneKind::DegradedLiteralBodyScan);

        let degraded_plan = plan
            .prefetch
            .iter()
            .find(|p| p.lane == LaneKind::DegradedLiteralBodyScan)
            .expect("DegradedLiteralBodyScan must be in prefetch");

        assert_eq!(degraded_plan.max_candidates, DEGRADED_MAX_RESULTS);
        assert_eq!(degraded_plan.latency_budget_ms, Some(DEGRADED_MAX_TIME_MS));
    }

    // DegradedLiteralBodyScan limits constants
    #[test]
    fn degraded_limits_are_bounded() {
        assert_eq!(DEGRADED_MAX_FILES, 100);
        assert_eq!(DEGRADED_MAX_RESULTS, 50);
        assert_eq!(DEGRADED_MAX_TIME_MS, 250);
    }

    // Serde round-trip
    #[test]
    fn search_plan_serde_roundtrip() {
        let shape = shape_for(QueryKind::NaturalLanguage);
        let plan = SearchPlanBuilder::from_query_shape(&shape, &fts5_ctx());
        let json = serde_json::to_string(&plan).unwrap();
        let deserialized: SearchPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.intent, plan.intent);
        assert_eq!(deserialized.active_safety_lane, plan.active_safety_lane);
        assert_eq!(deserialized.mandatory_lanes, plan.mandatory_lanes);
    }
}
