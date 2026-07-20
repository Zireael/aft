use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::candidate::{CandidateProvenance, CandidateSet};
use crate::context::{AppContext, CallgraphStoreAccess, SemanticIndexStatus};
use crate::context_budget::{ContextBudget, ContextBudgetResult, EnrichPool};
use crate::grep_executor::{self, GrepParams};
use crate::pattern_compile::{self, CompileOpts, CompileResult};
use crate::protocol::{RawRequest, Response};
use crate::query_shape::{self, QueryKind, QueryShape};
#[cfg(feature = "semantic-fts5")]
use crate::retrieval::fts5_adapter::Fts5Adapter;
use crate::retrieval::fusion::RRFFusionEngine;
use crate::retrieval::graph_enrichment::enrich_with_graph_context;
use crate::retrieval::graph_expansion::GraphExpansionAdapter;
use crate::retrieval::ranking_features::{apply_ranking_features, RankingFeaturesConfig};
use crate::retrieval::semantic_adapter::SemanticAdapter;
use crate::retrieval::trigram_adapter::TrigramAdapter;
use crate::ril_indexer::GraphHealth;
use crate::search_index::{
    sort_grep_matches_by_mtime_desc, GrepMatch, GrepResult, IndexStatus, SearchIndex,
};
use crate::search_plan::{
    FeatureFlagState, LaneKind, SafetyLaneContext, SearchPlan, SearchPlanBuilder,
};
use crate::semantic_diagnostics::{
    format_diagnostics_prefix, score_statistics, top1_margin, PhaseTimer, SearchDiagnostics,
    SearchPipelineType, SearchWarning,
};
use crate::semantic_index::{is_onnx_runtime_unavailable, EmbeddingModel, SemanticResult};
use crate::semantic_rerank::{rerank_candidates, RerankOutcome};
use crate::symbols::SymbolKind;
use crate::telemetry::{CandidateScoreRow, FusionScoreRow};

const DEFAULT_TOP_K: usize = 10;
const MAX_TOP_K: usize = 100;
const HYBRID_LEXICAL_BOOST: f32 = 1.1;
const LEXICAL_ONLY_SCORE_CEILING: f32 = 0.25;
const LEXICAL_ENUMERATION_LIMIT: usize = 50;
const SEMANTIC_OVERFETCH_MULTIPLIER: usize = 3;
const SEMANTIC_OVERFETCH_FLOOR: usize = 10;
const DEGRADED_GREP_FILE_LIMIT: usize = 5_000;
const DEGRADED_GREP_RESULT_LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct HybridResult {
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub exported: bool,
    pub score: f32,
    pub source: &'static str,
    pub semantic_score: Option<f32>,
    pub lexical_score: Option<f32>,
    pub hybrid_boosted: bool,
    pub snippet: String,
    pub provenance: Option<CandidateProvenance>,
    pub is_exact_hit: bool,
    pub exact_hit_floor_applied: bool,
    pub graph_context: Option<serde_json::Value>,
    pub enrichment_state: &'static str,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SearchHint {
    Regex,
    Literal,
    Semantic,
    #[default]
    Auto,
}

#[derive(Debug, Deserialize)]
struct SemanticSearchParams {
    query: String,
    #[serde(default = "default_top_k", alias = "topK")]
    top_k: usize,
    #[serde(default)]
    hint: SearchHint,
    /// Optional context profile: "agent_fast", "agent_deep", "symbol_exact".
    /// When absent with retrieval_intelligence_v2 enabled, defaults to "agent_fast".
    #[serde(default)]
    profile: Option<String>,
    /// Enable token-budgeted context filtering for this request. This activates
    /// the Retrieval Intelligence pipeline even when the global feature flag is
    /// off.
    #[serde(default)]
    context_budget_enabled: Option<bool>,
    /// Optional request-level override for total context tokens.
    #[serde(default)]
    context_total_tokens: Option<usize>,
    /// Optional request-level override for tokens per enriched candidate.
    #[serde(default)]
    context_per_candidate_tokens: Option<usize>,
    /// Optional request-level soft overflow for the final included candidate.
    #[serde(default)]
    context_soft_overflow_tokens: Option<usize>,
}

impl SemanticSearchParams {
    fn context_budget_requested(&self) -> bool {
        self.context_budget_enabled.unwrap_or(false)
            || self.context_total_tokens.is_some()
            || self.context_per_candidate_tokens.is_some()
            || self.context_soft_overflow_tokens.is_some()
    }

    fn apply_context_budget_overrides(&self, budget: &mut ContextBudget) {
        if let Some(total_tokens) = self.context_total_tokens {
            budget.total_tokens = total_tokens;
        }
        if let Some(per_candidate_tokens) = self.context_per_candidate_tokens {
            budget.per_candidate_tokens = per_candidate_tokens.max(1);
        }
        if let Some(soft_overflow_tokens) = self.context_soft_overflow_tokens {
            budget.soft_overflow_tokens = soft_overflow_tokens;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Regex,
    Literal,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone)]
struct LexicalCollection {
    files: Vec<(PathBuf, f32)>,
    ready: bool,
    engine_capped: bool,
}

pub fn handle_semantic_search(req: &RawRequest, ctx: &AppContext) -> Response {
    let mut params = match serde_json::from_value::<SemanticSearchParams>(req.params.clone()) {
        Ok(params) => params,
        Err(error) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("semantic_search: invalid params: {error}"),
            );
        }
    };

    if params.query.trim().is_empty() {
        return Response::error(&req.id, "invalid_request", "query must be non-empty");
    }

    // Strip a single pair of surrounding paired quotes from the literal needle.
    // Many agents and humans reach for the GitHub-code-search / `rg -F "..."`
    // convention of quoting a phrase, but AFT does pure substring matching by
    // default, so the quotes themselves become part of the needle and silently
    // produce zero results. Strip only matched leading+trailing pairs of `"`
    // or `'` (no escape handling — agents that genuinely want literal quotes
    // can pass `\"foo\"`-style content which won't be a balanced outer pair).
    params.query = strip_surrounding_quotes(params.query);
    if params.query.trim().is_empty() {
        return Response::error(&req.id, "invalid_request", "query must be non-empty");
    }

    let top_k = params.top_k.clamp(1, MAX_TOP_K);
    let project_root = grep_executor::project_root(ctx);
    let shape = query_shape::classify(&params.query);
    let semantic_status_snapshot = ctx.semantic_index_status().borrow().clone();
    let semantic_status = semantic_status_label(&semantic_status_snapshot);
    let mut warnings = Vec::new();

    let lexical_ready = search_index_ready(ctx);
    let mode = choose_mode(
        params.hint,
        &params.query,
        &shape,
        lexical_ready,
        &mut warnings,
    );

    // Build SearchPlan when retrieval_intelligence_v2 is enabled (flag-gated).
    // When flag is false, search_plan_debug is None and output is byte-identical to baseline.
    let ri_v2_enabled = retrieval_intelligence_v2_enabled(ctx) || params.context_budget_requested();
    let search_plan = if ri_v2_enabled {
        let fts5_available = ctx.config().fts5.enabled;
        let safety_ctx = SafetyLaneContext {
            fts5_available,
            search_index_ready: lexical_ready,
        };
        let profile = params.profile.as_deref().unwrap_or("agent_fast");
        match SearchPlanBuilder::from_query_shape_with_profile(&shape, &safety_ctx, profile) {
            Ok(mut plan) => {
                plan.feature_flag_state = FeatureFlagState::On;
                plan.rerank.enabled = ctx.config().semantic.rerank_enabled;
                plan.rerank.max_candidates = ctx.config().semantic.rerank_max_candidates;
                params.apply_context_budget_overrides(&mut plan.context_budget);
                Some(plan)
            }
            Err(error) => {
                return Response::error(&req.id, "invalid_request", error);
            }
        }
    } else {
        None
    };

    match mode {
        SearchMode::Regex | SearchMode::Literal => handle_grep_search(
            req,
            ctx,
            &params.query,
            top_k,
            &shape,
            mode,
            semantic_status,
            warnings,
            &project_root,
            search_plan.as_ref(),
        ),
        SearchMode::Semantic | SearchMode::Hybrid => handle_semantic_or_hybrid_search(
            req,
            ctx,
            params,
            top_k,
            shape,
            mode,
            semantic_status_snapshot,
            semantic_status,
            warnings,
            &project_root,
            search_plan.as_ref(),
        ),
    }
}

fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

fn retrieval_intelligence_v2_enabled(ctx: &AppContext) -> bool {
    match std::env::var("RETRIEVAL_INTELLIGENCE_V2") {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => ctx.config().intelligence.retrieval_intelligence_v2,
        },
        Err(_) => ctx.config().intelligence.retrieval_intelligence_v2,
    }
}

fn semantic_candidate_limit(top_k: usize) -> usize {
    top_k
        .saturating_mul(SEMANTIC_OVERFETCH_MULTIPLIER)
        .clamp(SEMANTIC_OVERFETCH_FLOOR, MAX_TOP_K)
}

fn choose_mode(
    hint: SearchHint,
    query: &str,
    shape: &QueryShape,
    lexical_ready: bool,
    warnings: &mut Vec<String>,
) -> SearchMode {
    match hint {
        SearchHint::Regex => {
            if shape.kind == QueryKind::NaturalLanguage {
                warnings.push(
                    "hint:'regex' was provided for a natural-language-looking query; interpreting it as regex.".to_string(),
                );
            }
            SearchMode::Regex
        }
        SearchHint::Literal => {
            if literal_tokens_all_short(query) {
                warnings.push(
                    "Literal query with tokens shorter than 3 chars requires per-file scan; latency may be slow on large repos.".to_string(),
                );
            }
            SearchMode::Literal
        }
        SearchHint::Semantic => {
            if shape.kind == QueryKind::Regex {
                warnings.push(
                    "hint:'semantic' was provided for a regex-looking query; skipping lexical/regex matching.".to_string(),
                );
            }
            SearchMode::Semantic
        }
        SearchHint::Auto => {
            if shape.kind == QueryKind::Regex {
                return SearchMode::Regex;
            }
            if shape.kind != QueryKind::NaturalLanguage && extracted_tokens_all_short(query, shape)
            {
                warnings.push(
                    "Auto mode is using literal full-file scan for all-short exact tokens because the trigram index cannot rank tokens shorter than 3 chars.".to_string(),
                );
                return SearchMode::Literal;
            }
            if shape.kind == QueryKind::NaturalLanguage {
                // Short NL concepts (e.g. "parse imports", "retry backoff") are
                // frequently literal code tokens the trigram lane nails exactly.
                // Run them as Hybrid so lexical still contributes; only longer
                // NL phrases go pure semantic. One extra trigram lookup.
                let word_count = query.split_whitespace().count();
                if lexical_ready && word_count <= 2 {
                    return SearchMode::Hybrid;
                }
                return SearchMode::Semantic;
            }
            if lexical_ready {
                SearchMode::Hybrid
            } else {
                warnings.push(
                    "Lexical trigram index is unavailable; using semantic search only.".to_string(),
                );
                SearchMode::Semantic
            }
        }
    }
}

fn handle_grep_search(
    req: &RawRequest,
    ctx: &AppContext,
    query: &str,
    top_k: usize,
    shape: &QueryShape,
    mode: SearchMode,
    semantic_status: &'static str,
    mut warnings: Vec<String>,
    project_root: &Path,
    search_plan: Option<&SearchPlan>,
) -> Response {
    let literal = mode == SearchMode::Literal;
    let compiled = match pattern_compile::compile(
        query,
        CompileOpts {
            literal,
            ..CompileOpts::default()
        },
    ) {
        CompileResult::Ok(compiled) => compiled,
        CompileResult::InvalidPattern { message, .. } => {
            return Response::error_with_data(
                &req.id,
                "invalid_pattern",
                message,
                serde_json::json!({"pattern": query}),
            );
        }
        CompileResult::UnsupportedSyntax { feature, .. } => {
            return Response::error_with_data(
                &req.id,
                "unsupported_pattern",
                format!(
                    "Pattern uses regex syntax not supported by AFT's engine: {feature}. Use hint:'literal' or rewrite without {feature}."
                ),
                serde_json::json!({"pattern": query, "feature": feature}),
            );
        }
    };

    let scope = match grep_executor::resolve_grep_scope(ctx, None, top_k, &req.id) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = GrepParams {
        include: Vec::new(),
        exclude: Vec::new(),
        max_results: top_k,
    };
    let result = grep_executor::execute(ctx, &compiled, &scope, &params);
    if result.fully_degraded {
        warnings.push(degraded_warning(ctx));
    }

    let result_source = if literal { "literal" } else { "regex" };
    let result_values = result
        .matches
        .iter()
        .map(|grep_match| grep_match_to_json(grep_match, result_source))
        .collect::<Vec<_>>();
    let interpreted_as = interpreted_as_label(mode);
    let text = format_grep_search_text(&result, project_root, interpreted_as);
    let mut extras = serde_json::Map::new();
    if let Some(plan) = search_plan {
        extras.insert("search_plan_debug".to_string(), search_plan_to_json(plan));
    }
    search_response(
        req,
        SearchResponseParts {
            query,
            interpreted_as,
            query_kind: query_kind_label(shape.kind),
            semantic_status,
            status: "ready",
            complete: true,
            text,
            results: result_values,
            more_available: result.truncated || result.total_matches > result.matches.len(),
            engine_capped: result.engine_capped,
            fully_degraded: result.fully_degraded,
            warnings,
            extras,
        },
    )
}

fn handle_semantic_or_hybrid_search(
    req: &RawRequest,
    ctx: &AppContext,
    params: SemanticSearchParams,
    top_k: usize,
    shape: QueryShape,
    mode: SearchMode,
    status: SemanticIndexStatus,
    semantic_status: &'static str,
    mut warnings: Vec<String>,
    project_root: &Path,
    search_plan: Option<&SearchPlan>,
) -> Response {
    let ri_v2_enabled = retrieval_intelligence_v2_enabled(ctx);
    let lexical = if mode == SearchMode::Hybrid {
        collect_lexical_files(ctx, &params.query, &shape)
    } else {
        LexicalCollection {
            files: Vec::new(),
            ready: search_index_ready(ctx),
            engine_capped: false,
        }
    };

    match status {
        SemanticIndexStatus::Disabled => {
            return semantic_unavailable_or_fallback_response(
                req,
                ctx,
                &params,
                mode,
                &shape,
                "disabled",
                "disabled",
                "Semantic search is not enabled.".to_string(),
                lexical,
                warnings,
                project_root,
                top_k,
                search_plan,
            );
        }
        SemanticIndexStatus::Failed(error) => {
            return semantic_unavailable_or_fallback_response(
                req,
                ctx,
                &params,
                mode,
                &shape,
                "unavailable",
                "unavailable",
                format!("Semantic search unavailable: {error}"),
                lexical,
                warnings,
                project_root,
                top_k,
                search_plan,
            );
        }
        SemanticIndexStatus::Building {
            stage,
            files,
            entries_done,
            entries_total,
        } => {
            let mut detail = format!("Semantic index is still building (stage: {}).", stage);
            if let Some(files) = files {
                detail.push_str(&format!(" files: {}", files));
            }
            if let Some(entries_done) = entries_done {
                detail.push_str(&format!(" entries done: {}", entries_done));
            }
            if let Some(entries_total) = entries_total {
                detail.push_str(&format!(" / {}", entries_total));
            }

            if natural_language_degraded_fallback_available(params.hint, mode, &shape) {
                return semantic_unavailable_grep_fallback_response(
                    req,
                    ctx,
                    &params,
                    &shape,
                    "building",
                    detail,
                    warnings,
                    project_root,
                    top_k,
                    search_plan,
                );
            }

            let lexical_count = lexical.files.len();
            let lexical_engine_capped = lexical.engine_capped;
            let results = fuse_hybrid_results(
                Vec::new(),
                lexical.files,
                &shape,
                top_k,
                ctx.config().semantic.max_results_per_file,
            );
            let result_values = results.iter().map(result_to_json).collect::<Vec<_>>();
            let note = building_lexical_note(lexical.ready);
            let mut extras = serde_json::Map::new();
            extras.insert("stage".to_string(), serde_json::json!(stage));
            extras.insert("files".to_string(), serde_json::json!(files));
            extras.insert("entries_done".to_string(), serde_json::json!(entries_done));
            extras.insert(
                "entries_total".to_string(),
                serde_json::json!(entries_total),
            );
            extras.insert("note".to_string(), serde_json::json!(note));
            extras.insert("semantic_rebuilding".to_string(), serde_json::json!(true));
            extras.insert(
                "lexical_only_fallback".to_string(),
                serde_json::json!(lexical.ready),
            );
            if let Some(plan) = search_plan {
                extras.insert("search_plan_debug".to_string(), search_plan_to_json(plan));
            }

            return search_response(
                req,
                SearchResponseParts {
                    query: &params.query,
                    // While semantic rebuilds, only the lexical lane produced
                    // these results (semantic input is empty here). Report
                    // "lexical" when it ran; the "building" status + the
                    // semantic_rebuilding/lexical_only_fallback extras tell the
                    // agent semantic results are still coming.
                    interpreted_as: fallback_executed_label(mode, lexical.ready),
                    query_kind: query_kind_label(shape.kind),
                    semantic_status: "building",
                    status: "building",
                    complete: false,
                    text: format_building_lexical_text(
                        &detail,
                        &results,
                        project_root,
                        lexical.ready,
                    ),
                    results: result_values,
                    more_available: lexical_count > top_k || lexical_engine_capped,
                    engine_capped: lexical_engine_capped,
                    fully_degraded: false,
                    warnings,
                    extras,
                },
            );
        }
        SemanticIndexStatus::Partial {
            entries_done,
            entries_total,
            completeness,
            ..
        } => {
            warnings.push(format!(
                "semantic index partially built ({:.0}%, {}/{})",
                completeness * 100.0,
                entries_done,
                entries_total
            ));
        }
        SemanticIndexStatus::Ready { refreshing } => {
            if !refreshing.is_empty() {
                warnings.push(format!(
                    "{} file(s) refreshing; results for those files may be temporarily missing",
                    refreshing.len()
                ));
            }
        }
    }

    if !semantic_index_loaded(ctx) {
        return semantic_unavailable_or_fallback_response(
            req,
            ctx,
            &params,
            mode,
            &shape,
            "unavailable",
            "not_ready",
            "Semantic index is not ready yet.".to_string(),
            lexical,
            warnings,
            project_root,
            top_k,
            search_plan,
        );
    }

    // URKF pipeline branch: when retrieval_intelligence_v2 is enabled AND
    // a search plan was built, use the unified retrieval pipeline (adapters +
    // RRF fusion) instead of the legacy embed → search → fuse → rerank path.
    if ri_v2_enabled {
        if let Some(plan) = search_plan {
            let (results, provenance) = run_urfk_pipeline(
                &params.query,
                top_k,
                plan,
                ctx,
                mode,
                shape,
                params.profile.as_deref().unwrap_or("agent_fast"),
                &lexical,
            );
            let result_values = results.iter().map(result_to_json).collect::<Vec<_>>();
            let mut extras = serde_json::Map::new();
            extras.insert("search_plan_debug".to_string(), search_plan_to_json(plan));
            if let Some(prov) = provenance {
                extras.insert("retrieval_intelligence_provenance".to_string(), prov);
            }
            warnings.push(
                "ri_v2 pipeline: in-development retrieval intelligence pipeline.".to_string(),
            );
            return search_response(
                req,
                SearchResponseParts {
                    query: &params.query,
                    interpreted_as: interpreted_as_label(mode),
                    query_kind: query_kind_label(shape.kind),
                    semantic_status,
                    status: "ready",
                    complete: true,
                    text: format_semantic_text(
                        &results,
                        project_root,
                        results.len() > top_k,
                        false,
                    ),
                    results: result_values,
                    more_available: results.len() > top_k,
                    engine_capped: false,
                    fully_degraded: false,
                    warnings,
                    extras,
                },
            );
        }
    }

    let diagnostics_enabled = ctx.config().semantic.diagnostics_enabled();
    let query_hash = SearchDiagnostics::hash_query(&params.query);
    let index_state = semantic_status.to_string();
    let mut diag_warnings = collect_index_diag_warnings(ctx);

    let embedding_timer = PhaseTimer::start();
    let (query_vector, query_cache_hit) = match embed_query(&params.query, ctx) {
        Ok(query_vector) => query_vector,
        Err(error) => {
            if params.hint == SearchHint::Semantic
                || !semantic_degraded_fallback_available(&params, mode, &shape, &lexical)
            {
                return semantic_error_response(&req.id, &error);
            }

            return semantic_unavailable_or_fallback_response(
                req,
                ctx,
                &params,
                mode,
                &shape,
                "unavailable",
                "unavailable",
                format!("Semantic search unavailable: {error}"),
                lexical,
                warnings,
                project_root,
                top_k,
                search_plan,
            );
        }
    };
    let embedding_latency_ms = embedding_timer.stop();

    let semantic_limit = semantic_candidate_limit(top_k);
    let semantic_fetch_limit = semantic_limit.saturating_add(1);
    let vector_search_timer = PhaseTimer::start();
    let mut semantic_results = {
        let semantic_index = ctx.semantic_index().borrow();
        semantic_index
            .as_ref()
            .map(|index| index.search(&query_vector, semantic_fetch_limit))
            .unwrap_or_default()
    };
    let vector_search_latency_ms = vector_search_timer.stop();
    let semantic_more_available = semantic_results.len() > semantic_limit;
    if semantic_more_available {
        semantic_results.truncate(semantic_limit);
    }

    let has_semantic = !semantic_results.is_empty();
    let has_lexical = !lexical.files.is_empty();
    let max_results_per_file = ctx.config().semantic.max_results_per_file;
    // When reranking is enabled, overfetch candidates through fusion so the
    // reranker sees the full pool (up to rerank_max_candidates) rather than
    // only the final top_k.  Truncation to top_k happens after reranking.
    let rerank_enabled = ctx.config().semantic.rerank_enabled;
    let fusion_limit = if rerank_enabled {
        ctx.config().semantic.rerank_max_candidates.max(top_k)
    } else {
        top_k
    };
    let fusion_timer = PhaseTimer::start();
    let mut results = fuse_hybrid_results(
        semantic_results,
        lexical.files,
        &shape,
        fusion_limit.saturating_add(1),
        max_results_per_file,
    );
    let hybrid_fusion_latency_ms = fusion_timer.stop();
    let fused_more_available = results.len() > fusion_limit;
    // Do NOT truncate to top_k here — the reranker needs the full pool.
    // Truncation happens after reranking (or after the rerank block).
    let mut more_available =
        fused_more_available || semantic_more_available || lexical.engine_capped;
    let pipeline_type = match (has_semantic, has_lexical) {
        (true, true) => SearchPipelineType::Hybrid,
        (true, false) => SearchPipelineType::Semantic,
        (false, true) => {
            diag_warnings.push(SearchWarning::EmptyResults);
            SearchPipelineType::LexicalFallback
        }
        (false, false) => {
            diag_warnings.push(SearchWarning::EmptyResults);
            SearchPipelineType::Semantic
        }
    };

    // ROOT CAUSE FIX: Enrich rerank pool BEFORE reranking (WARNING 2).
    // When retrieval_intelligence_v2 is enabled AND enrich_pool==RerankPool AND rerank enabled,
    // enrich all candidates with context BEFORE rerank_candidates() so the reranker receives
    // non-empty snippets. PathOnly candidates are excluded from reranker input.
    let mut context_budget_result = ContextBudgetResult::default();
    let ri_v2 = retrieval_intelligence_v2_enabled(ctx);
    let enrich_before_rerank = ri_v2
        && rerank_enabled
        && search_plan
            .as_ref()
            .map(|p| p.context_budget.enrich_pool == EnrichPool::RerankPool)
            .unwrap_or(false);

    if enrich_before_rerank {
        let budget = &search_plan.as_ref().unwrap().context_budget;
        context_budget_result = enrich_context_pool(&mut results, budget);
        // If enriched ratio is below threshold, skip reranker entirely
        if context_budget_result.reranker_skipped_reason.is_some() {
            // Reranker will be skipped — fall through to Skipped path
        }
    }

    let rerank_timer = PhaseTimer::start();
    let rerank_latency_ms;
    // Skip reranker when context budget says to (insufficient enriched ratio or zero enriched)
    let rerank_skipped_by_budget = context_budget_result.reranker_skipped_reason.is_some();
    results = if rerank_skipped_by_budget {
        rerank_latency_ms = rerank_timer.stop();
        context_budget_result.reranker_input_candidate_count = 0;
        context_budget_result.path_only_reranker_input_count = 0;
        results
    } else {
        let (reranked_results, outcome, input_count, path_only_input_count) =
            rerank_enriched_subset(&ctx.config().semantic, &params.query, results);
        context_budget_result.reranker_input_candidate_count = input_count;
        context_budget_result.path_only_reranker_input_count = path_only_input_count;
        match outcome {
            RerankOutcome::ReRanked(indices) => {
                rerank_latency_ms = rerank_timer.stop();
                let oob_count = indices.iter().filter(|&&i| i >= input_count).count();
                if oob_count > 0 && diagnostics_enabled {
                    diag_warnings.push(SearchWarning::RerankerFailure {
                        reason: format!(
                            "reranker returned {} out-of-bounds indices (max {})",
                            oob_count, input_count
                        ),
                    });
                }
                reranked_results
            }
            RerankOutcome::Skipped => {
                rerank_latency_ms = rerank_timer.stop();
                reranked_results
            }
            RerankOutcome::Failed(error) => {
                rerank_latency_ms = rerank_timer.stop();
                diag_warnings.push(SearchWarning::RerankerFailure { reason: error });
                reranked_results
            }
        }
    };

    // Truncate to top_k after reranking so the reranker sees the full pool
    // but the caller only receives the requested number of results.
    // If the pre-truncation pool exceeded top_k, signal more_available so the
    // agent knows additional results were dropped.
    if results.len() > top_k {
        more_available = true;
    }
    results.truncate(top_k);

    let scores: Vec<f32> = results.iter().map(|result| result.score).collect();
    let low_conf_threshold = ctx.config().semantic.low_confidence_threshold;
    if !scores.is_empty() && scores.iter().all(|score| *score < low_conf_threshold) {
        diag_warnings.push(SearchWarning::LowConfidence);
    }

    // No score threshold: silent filtering produced "0 results" even when the
    // model had reasonable matches the agent could have judged. Surface every
    // hit so the caller can decide.

    // Read display snippets from source on the fly (top 3 only, rank-budgeted)
    // so both the text rendering and the JSON `results` carry fresh, correctly
    // sized previews. Drives the conditional zoom hint.
    let snippets_incomplete = enrich_snippets_from_source(&mut results);

    let candidate_count = scores.len();
    let returned_count = results.len();
    let score_stats = score_statistics(&scores);
    let margin = top1_margin(&scores);
    let prompt_active = ctx.config().semantic.query_prompt_template.is_some();
    let output_mode = ctx.config().semantic.output_mode;
    let deduped_warnings = ctx
        .semantic_warning_dedup()
        .borrow_mut()
        .filter_for_output(&diag_warnings);
    let diagnostics_prefix = format_diagnostics_prefix(
        output_mode,
        &deduped_warnings,
        pipeline_type,
        embedding_latency_ms
            + vector_search_latency_ms
            + hybrid_fusion_latency_ms
            + rerank_latency_ms,
        Some(score_stats),
        candidate_count,
        returned_count,
        Some(embedding_latency_ms),
        Some(vector_search_latency_ms),
        None,
        Some(hybrid_fusion_latency_ms),
        Some(rerank_latency_ms),
    );

    let mut base_text =
        format_semantic_text(&results, project_root, more_available, snippets_incomplete);
    if let Some(prefix) = diagnostics_prefix {
        base_text = format!("{prefix}\n\n{base_text}");
    }

    if diagnostics_enabled {
        ctx.init_diagnostics_logger();
        let (score_min, score_median, score_p90, score_max) = score_stats;
        let diag = SearchDiagnostics {
            query_hash,
            pipeline_type,
            index_state,
            total_latency_ms: embedding_latency_ms
                + vector_search_latency_ms
                + hybrid_fusion_latency_ms
                + rerank_latency_ms,
            embedding_latency_ms: Some(embedding_latency_ms),
            lexical_latency_ms: None,
            vector_search_latency_ms: Some(vector_search_latency_ms),
            hybrid_fusion_latency_ms: Some(hybrid_fusion_latency_ms),
            rerank_latency_ms: Some(rerank_latency_ms),
            candidate_count,
            returned_count,
            score_min,
            score_median,
            score_p90,
            score_max,
            top1_margin: margin,
            query_cache_hit,
            prompt_active,
            warnings: diag_warnings,
        };
        ctx.semantic_search_metrics()
            .borrow_mut()
            .record(diag.clone());
        if let Some(logger) = ctx.semantic_diagnostics_logger().borrow_mut().as_mut() {
            logger.record(&diag, Some(&params.query), None);
        }
    }

    search_response(
        req,
        SearchResponseParts {
            query: &params.query,
            interpreted_as: interpreted_as_label(mode),
            query_kind: query_kind_label(shape.kind),
            semantic_status,
            status: "ready",
            complete: true,
            text: base_text,
            results: results.iter().map(result_to_json).collect::<Vec<_>>(),
            more_available,
            engine_capped: lexical.engine_capped,
            fully_degraded: false,
            warnings,
            extras: {
                let mut extras = serde_json::Map::new();
                if let Some(plan) = search_plan {
                    extras.insert("search_plan_debug".to_string(), search_plan_to_json(plan));
                }
                // Add context budget diagnostics when RI v2 is active
                if ri_v2 && rerank_enabled {
                    extras.insert(
                        "rerank_pool_size".to_string(),
                        serde_json::json!(context_budget_result.rerank_pool_size),
                    );
                    extras.insert(
                        "enriched_candidate_count".to_string(),
                        serde_json::json!(context_budget_result.enriched_candidate_count),
                    );
                    extras.insert(
                        "context_exhausted".to_string(),
                        serde_json::json!(context_budget_result.context_exhausted),
                    );
                    extras.insert(
                        "unenriched_candidate_count".to_string(),
                        serde_json::json!(context_budget_result.unenriched_candidate_count),
                    );
                    extras.insert(
                        "path_only_candidate_count".to_string(),
                        serde_json::json!(context_budget_result.path_only_candidate_count),
                    );
                    extras.insert(
                        "skipped_candidate_count".to_string(),
                        serde_json::json!(context_budget_result.skipped_candidate_count),
                    );
                    extras.insert(
                        "reranker_input_candidate_count".to_string(),
                        serde_json::json!(context_budget_result.reranker_input_candidate_count),
                    );
                    extras.insert(
                        "path_only_reranker_input_count".to_string(),
                        serde_json::json!(context_budget_result.path_only_reranker_input_count),
                    );
                    extras.insert(
                        "reranker_skipped_reason".to_string(),
                        serde_json::json!(context_budget_result.reranker_skipped_reason),
                    );
                }
                // Add retrieval intelligence provenance when RI v2 is active
                if ri_v2 {
                    if let Some(plan) = search_plan {
                        let lane_contributions: Vec<serde_json::Value> = plan
                            .lane_weights
                            .iter()
                            .filter(|(_, &w)| w > 0.0)
                            .map(|(lane, weight)| {
                                serde_json::json!({
                                    "lane": format!("{:?}", lane),
                                    "weight": weight,
                                })
                            })
                            .collect();
                        extras.insert(
                            "retrieval_intelligence_provenance".to_string(),
                            serde_json::json!({
                                "lane_contributions": lane_contributions,
                                "intent": format!("{:?}", plan.intent),
                                "active_safety_lane": format!("{:?}", plan.active_safety_lane),
                            }),
                        );
                    }
                }
                extras
            },
        },
    )
}

struct SearchResponseParts<'a> {
    query: &'a str,
    interpreted_as: &'static str,
    query_kind: &'static str,
    semantic_status: &'static str,
    status: &'static str,
    complete: bool,
    text: String,
    results: Vec<serde_json::Value>,
    more_available: bool,
    engine_capped: bool,
    fully_degraded: bool,
    warnings: Vec<String>,
    extras: serde_json::Map<String, serde_json::Value>,
}

impl<'a> SearchResponseParts<'a> {
    fn result_count(&self) -> usize {
        self.results.len()
    }
}

fn search_response(req: &RawRequest, parts: SearchResponseParts<'_>) -> Response {
    let mut object = serde_json::Map::new();
    object.insert("status".to_string(), serde_json::json!(parts.status));
    object.insert("complete".to_string(), serde_json::json!(parts.complete));
    object.insert("text".to_string(), serde_json::json!(parts.text));
    object.insert("query".to_string(), serde_json::json!(parts.query));
    object.insert(
        "interpreted_as".to_string(),
        serde_json::json!(parts.interpreted_as),
    );
    object.insert(
        "query_kind".to_string(),
        serde_json::json!(parts.query_kind),
    );
    object.insert(
        "result_count".to_string(),
        serde_json::json!(parts.result_count()),
    );
    object.insert(
        "results".to_string(),
        serde_json::Value::Array(parts.results),
    );
    object.insert(
        "more_available".to_string(),
        serde_json::json!(parts.more_available),
    );
    object.insert(
        "engine_capped".to_string(),
        serde_json::json!(parts.engine_capped),
    );
    object.insert(
        "fully_degraded".to_string(),
        serde_json::json!(parts.fully_degraded),
    );
    object.insert(
        "semantic_status".to_string(),
        serde_json::json!(parts.semantic_status),
    );
    if !parts.warnings.is_empty() {
        object.insert("warnings".to_string(), serde_json::json!(parts.warnings));
    }
    for (key, value) in parts.extras {
        object.insert(key, value);
    }
    Response::success(&req.id, serde_json::Value::Object(object))
}

fn semantic_unavailable_or_fallback_response(
    req: &RawRequest,
    ctx: &AppContext,
    params: &SemanticSearchParams,
    mode: SearchMode,
    shape: &QueryShape,
    semantic_status: &'static str,
    unavailable_status: &'static str,
    detail: String,
    lexical: LexicalCollection,
    mut warnings: Vec<String>,
    project_root: &Path,
    top_k: usize,
    search_plan: Option<&SearchPlan>,
) -> Response {
    if params.hint == SearchHint::Semantic {
        return semantic_unavailable_response(&req.id, detail);
    }

    let lexical_ready = mode == SearchMode::Hybrid && lexical.ready;
    if lexical_ready {
        if let Some(plan) = search_plan {
            let (results, provenance) = run_urfk_pipeline(
                &params.query,
                top_k,
                plan,
                ctx,
                mode,
                *shape,
                params.profile.as_deref().unwrap_or("agent_fast"),
                &lexical,
            );
            if !results.is_empty() {
                let result_values = results.iter().map(result_to_json).collect::<Vec<_>>();
                let mut extras = semantic_unavailable_extras(true, search_plan);
                if let Some(prov) = provenance {
                    extras.insert("retrieval_intelligence_provenance".to_string(), prov);
                }
                warnings.push(
                    "Semantic search unavailable; returning RI v2 trigram safety-lane results."
                        .to_string(),
                );
                return search_response(
                    req,
                    SearchResponseParts {
                        query: &params.query,
                        interpreted_as: fallback_executed_label(mode, true),
                        query_kind: query_kind_label(shape.kind),
                        semantic_status,
                        status: "ready",
                        complete: false,
                        text: format_semantic_text(
                            &results,
                            project_root,
                            results.len() > top_k,
                            false,
                        ),
                        results: result_values,
                        more_available: results.len() > top_k,
                        engine_capped: lexical.engine_capped,
                        fully_degraded: false,
                        warnings,
                        extras,
                    },
                );
            }
        }
    }
    if lexical_ready {
        let lexical_count = lexical.files.len();
        let lexical_engine_capped = lexical.engine_capped;
        let results = fuse_hybrid_results(
            Vec::new(),
            lexical.files,
            shape,
            top_k,
            ctx.config().semantic.max_results_per_file,
        );
        let result_values = results.iter().map(result_to_json).collect::<Vec<_>>();
        warnings.push(
            "Semantic search unavailable; returning lexical-only fallback results.".to_string(),
        );

        return search_response(
            req,
            SearchResponseParts {
                query: &params.query,
                // The trigram lexical lane produced these results; semantic
                // never ran. Report what executed, not the routed mode.
                interpreted_as: fallback_executed_label(mode, true),
                query_kind: query_kind_label(shape.kind),
                semantic_status,
                status: "ready",
                complete: false,
                text: format_lexical_unavailable_text(&detail, &results, project_root),
                results: result_values,
                more_available: lexical_count > top_k || lexical_engine_capped,
                engine_capped: lexical_engine_capped,
                fully_degraded: false,
                warnings,
                extras: semantic_unavailable_extras(true, search_plan),
            },
        );
    }

    if semantic_degraded_fallback_available(params, mode, shape, &lexical) {
        return semantic_unavailable_grep_fallback_response(
            req,
            ctx,
            params,
            shape,
            semantic_status,
            detail,
            warnings,
            project_root,
            top_k,
            search_plan,
        );
    }

    let mut extras = semantic_unavailable_extras(false, search_plan);
    if mode == SearchMode::Hybrid {
        extras.insert("lexical_unavailable".to_string(), serde_json::json!(true));
    }

    search_response(
        req,
        SearchResponseParts {
            query: &params.query,
            interpreted_as: interpreted_as_label(mode),
            query_kind: query_kind_label(shape.kind),
            semantic_status,
            status: unavailable_status,
            complete: false,
            text: detail,
            results: Vec::new(),
            more_available: false,
            engine_capped: lexical.engine_capped,
            fully_degraded: false,
            warnings,
            extras,
        },
    )
}

fn semantic_unavailable_response(request_id: &str, detail: String) -> Response {
    Response::error(request_id, "semantic_unavailable", detail)
}

fn semantic_unavailable_extras(
    lexical_only_fallback: bool,
    search_plan: Option<&SearchPlan>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut extras = serde_json::Map::new();
    extras.insert("semantic_unavailable".to_string(), serde_json::json!(true));
    extras.insert(
        "lexical_only_fallback".to_string(),
        serde_json::json!(lexical_only_fallback),
    );
    if let Some(plan) = search_plan {
        extras.insert("search_plan_debug".to_string(), search_plan_to_json(plan));
        extras.insert(
            "retrieval_intelligence_diagnostic".to_string(),
            serde_json::json!({
                "semantic_prerequisite": "unavailable",
                "executed_path": if lexical_only_fallback {
                    "lexical_only_fallback"
                } else {
                    "semantic_unavailable"
                },
            }),
        );
    }
    extras
}

fn semantic_degraded_fallback_available(
    params: &SemanticSearchParams,
    mode: SearchMode,
    shape: &QueryShape,
    lexical: &LexicalCollection,
) -> bool {
    if natural_language_degraded_fallback_available(params.hint, mode, shape) {
        return true;
    }

    params.hint != SearchHint::Semantic
        && mode == SearchMode::Semantic
        && !lexical.ready
        && shape.weights.should_use_lexical
}

fn natural_language_degraded_fallback_available(
    hint: SearchHint,
    mode: SearchMode,
    shape: &QueryShape,
) -> bool {
    hint != SearchHint::Semantic
        && mode == SearchMode::Semantic
        && shape.kind == QueryKind::NaturalLanguage
}

fn semantic_unavailable_grep_fallback_response(
    req: &RawRequest,
    ctx: &AppContext,
    params: &SemanticSearchParams,
    shape: &QueryShape,
    semantic_status: &'static str,
    detail: String,
    mut warnings: Vec<String>,
    project_root: &Path,
    top_k: usize,
    search_plan: Option<&SearchPlan>,
) -> Response {
    let result = match execute_degraded_grep_fallback(&params.query, project_root, top_k, &req.id) {
        Ok(result) => result,
        Err(response) => return response,
    };
    if result.fully_degraded {
        warnings.push(degraded_warning(ctx));
    }
    warnings
        .push("Semantic search unavailable; returning lexical-only fallback results.".to_string());

    let result_values = result
        .matches
        .iter()
        .map(|grep_match| grep_match_to_json(grep_match, "literal"))
        .collect::<Vec<_>>();
    let more_available = result.truncated || result.total_matches > result.matches.len();

    search_response(
        req,
        SearchResponseParts {
            query: &params.query,
            // This path ran a literal grep scan over the corpus (the results are
            // GrepLine entries), so report "literal" — not the routed
            // semantic/hybrid mode that never executed.
            interpreted_as: "literal",
            query_kind: query_kind_label(shape.kind),
            semantic_status,
            status: "ready",
            complete: false,
            text: format_grep_lexical_unavailable_text(&detail, &result, project_root),
            results: result_values,
            more_available,
            engine_capped: result.engine_capped,
            fully_degraded: result.fully_degraded,
            warnings,
            extras: semantic_unavailable_extras(true, search_plan),
        },
    )
}

fn execute_degraded_grep_fallback(
    query: &str,
    project_root: &Path,
    top_k: usize,
    request_id: &str,
) -> Result<GrepResult, Response> {
    let compiled = match pattern_compile::compile(
        query,
        CompileOpts {
            literal: true,
            ..CompileOpts::default()
        },
    ) {
        CompileResult::Ok(compiled) => compiled,
        CompileResult::InvalidPattern { message, .. } => {
            return Err(Response::error_with_data(
                request_id,
                "invalid_pattern",
                message,
                serde_json::json!({"pattern": query}),
            ));
        }
        CompileResult::UnsupportedSyntax { feature, .. } => {
            return Err(Response::error_with_data(
                request_id,
                "unsupported_pattern",
                format!(
                    "Pattern uses regex syntax not supported by AFT's engine: {feature}. Use hint:'literal' or rewrite without {feature}."
                ),
                serde_json::json!({"pattern": query, "feature": feature}),
            ));
        }
    };

    let max_results = top_k.clamp(1, DEGRADED_GREP_RESULT_LIMIT);
    let (files, file_cap_reached) = collect_degraded_grep_files(project_root);
    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut files_searched = 0usize;
    let mut files_with_matches = 0usize;
    let mut truncated = false;
    let mut engine_capped = file_cap_reached;

    for file in files {
        if truncated {
            engine_capped = true;
            break;
        }

        let Some(content) = crate::search_index::read_searchable_text(&file) else {
            continue;
        };
        files_searched += 1;

        if search_degraded_grep_file(
            &file,
            &content,
            &compiled,
            max_results,
            &mut total_matches,
            &mut truncated,
            &mut matches,
        ) {
            files_with_matches += 1;
        }
    }

    if truncated {
        engine_capped = true;
    }
    sort_grep_matches_by_mtime_desc(&mut matches, project_root);

    Ok(GrepResult {
        matches,
        total_matches,
        files_searched,
        files_with_matches,
        index_status: IndexStatus::Fallback,
        truncated,
        fully_degraded: true,
        engine_capped,
    })
}

fn collect_degraded_grep_files(project_root: &Path) -> (Vec<PathBuf>, bool) {
    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".aftignore")
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry
                .file_type()
                .map_or(false, |file_type| file_type.is_dir())
            {
                return !matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "venv"
                        | ".venv"
                        | ".git"
                        | "__pycache__"
                        | ".tox"
                        | "dist"
                        | "build"
                        // AFT storage and benchmark caches — these contain
                        // SQLite databases, binary embeddings, and JSON
                        // reports that are never source code.  Without this
                        // filter, `read_searchable_text` reads multi-GB of
                        // binary data and the handler times out.
                        | ".aft"
                        | ".bench-cache"
                        | ".aft-bench"
                        | ".beads"
                        | ".pi"
                );
            }
            true
        })
        .build();

    let mut files = Vec::new();
    for entry in walker.filter_map(|entry| entry.ok()) {
        if !entry
            .file_type()
            .map_or(false, |file_type| file_type.is_file())
        {
            continue;
        }
        if files.len() >= DEGRADED_GREP_FILE_LIMIT {
            return (files, true);
        }
        files.push(entry.into_path());
    }

    (files, false)
}

fn search_degraded_grep_file(
    file: &Path,
    content: &str,
    compiled: &pattern_compile::CompiledPattern,
    max_results: usize,
    total_matches: &mut usize,
    truncated: &mut bool,
    matches: &mut Vec<GrepMatch>,
) -> bool {
    let line_starts = grep_executor::line_starts(content);
    let mut seen_lines = HashSet::new();
    let mut matched_this_file = false;

    match compiled {
        pattern_compile::CompiledPattern::Literal(literal) => {
            let Some(needle) = std::str::from_utf8(&literal.needle).ok() else {
                return false;
            };
            let haystack = if literal.case_insensitive_ascii {
                Cow::Owned(content.to_ascii_lowercase())
            } else {
                Cow::Borrowed(content)
            };

            for (offset, matched) in haystack.match_indices(needle) {
                let match_text = content[offset..offset + matched.len()].to_string();
                let (counted, should_continue) = record_degraded_grep_match(
                    file,
                    content,
                    &line_starts,
                    &mut seen_lines,
                    offset,
                    match_text,
                    max_results,
                    total_matches,
                    truncated,
                    matches,
                );
                matched_this_file |= counted;
                if !should_continue {
                    break;
                }
            }
        }
        pattern_compile::CompiledPattern::Regex { compiled, .. } => {
            for matched in compiled.find_iter(content.as_bytes()) {
                let (counted, should_continue) = record_degraded_grep_match(
                    file,
                    content,
                    &line_starts,
                    &mut seen_lines,
                    matched.start(),
                    String::from_utf8_lossy(matched.as_bytes()).into_owned(),
                    max_results,
                    total_matches,
                    truncated,
                    matches,
                );
                matched_this_file |= counted;
                if !should_continue {
                    break;
                }
            }
        }
    }

    matched_this_file
}

fn record_degraded_grep_match(
    file: &Path,
    content: &str,
    line_starts: &[usize],
    seen_lines: &mut HashSet<u32>,
    offset: usize,
    match_text: String,
    max_results: usize,
    total_matches: &mut usize,
    truncated: &mut bool,
    matches: &mut Vec<GrepMatch>,
) -> (bool, bool) {
    let (line, column, line_text) = grep_executor::line_details(content, line_starts, offset);
    if !seen_lines.insert(line) {
        return (false, true);
    }

    *total_matches += 1;
    if matches.len() >= max_results {
        *truncated = true;
        return (true, false);
    }

    matches.push(GrepMatch {
        file: file.to_path_buf(),
        line,
        column,
        line_text,
        match_text,
    });
    (true, true)
}

fn semantic_index_loaded(ctx: &AppContext) -> bool {
    ctx.semantic_index().borrow().is_some()
}

/// URKF pipeline: Unified Retrieval with Known-key Fusion.
///
/// Collects candidates from retrieval lane adapters (FTS5, semantic, trigram),
/// fuses with RRF + ExactHitFloor, applies ContextBudget enrichment and
/// reranking, and returns public-search results with provenance metadata.
fn run_urfk_pipeline(
    query: &str,
    top_k: usize,
    plan: &SearchPlan,
    ctx: &AppContext,
    _mode: SearchMode,
    shape: QueryShape,
    profile: &str,
    lexical: &LexicalCollection,
) -> (Vec<HybridResult>, Option<serde_json::Value>) {
    use crate::retrieval::RetrievalAdapter;

    // 1. Collect candidate sets from adapters
    let mut candidate_sets: Vec<CandidateSet> = Vec::new();
    let mut degraded_lanes: Vec<serde_json::Value> = Vec::new();
    let mut fts5_degraded_to_trigram_body = false;

    // Run FTS5Adapter if FTS5 is available
    #[cfg(feature = "semantic-fts5")]
    {
        if ctx.config().fts5.enabled {
            let project_root = grep_executor::project_root(ctx);
            let db_path = project_root.join(".aft").join("fts5.sqlite");
            match crate::fts5_store::Fts5Store::open(&db_path) {
                Ok(store) => {
                    let fts5_adapter = Fts5Adapter::new(&store);
                    let report = fts5_adapter.retrieve_with_diagnostics(query, plan);
                    candidate_sets.extend(report.candidate_sets);
                    degraded_lanes.extend(report.degraded_lanes.into_iter().map(|lane| {
                        if lane.fallback_used == Some(LaneKind::TrigramBody) {
                            fts5_degraded_to_trigram_body = true;
                        }
                        serde_json::json!({
                            "lane": format!("{:?}", lane.lane),
                            "reason": lane.reason,
                            "fallback_used": lane.fallback_used.map(|fallback| format!("{:?}", fallback)),
                        })
                    }));
                }
                Err(error) => {
                    degraded_lanes.extend(fts5_plan_degraded_lanes(
                        plan,
                        format!("FTS5 store unavailable: {error}"),
                    ));
                    if plan.active_safety_lane == LaneKind::FTS5Body {
                        fts5_degraded_to_trigram_body = true;
                    }
                }
            }
        }
    }
    #[cfg(not(feature = "semantic-fts5"))]
    {
        if ctx.config().fts5.enabled {
            degraded_lanes.extend(fts5_plan_degraded_lanes(
                plan,
                "FTS5 support is not compiled into this binary".to_string(),
            ));
            if plan.active_safety_lane == LaneKind::FTS5Body {
                fts5_degraded_to_trigram_body = true;
            }
        }
    }

    // Run SemanticAdapter if semantic index is ready
    if semantic_index_loaded(ctx) {
        if let Ok((query_vector, _query_cache_hit)) = embed_query(query, ctx) {
            let semantic_limit = plan
                .prefetch
                .iter()
                .find(|p| p.lane == crate::search_plan::LaneKind::Semantic)
                .map(|p| p.max_candidates)
                .unwrap_or(50);
            let semantic_results = {
                let semantic_index = ctx.semantic_index().borrow();
                semantic_index
                    .as_ref()
                    .map(|index| index.search(&query_vector, semantic_limit))
                    .unwrap_or_default()
            };
            let semantic_adapter = SemanticAdapter::from_results(semantic_results);
            candidate_sets.extend(semantic_adapter.retrieve(query, plan));
        }
    }

    // Run TrigramAdapter if search index is ready
    if lexical.ready {
        let trigram_adapter = TrigramAdapter::from_ranked_files(lexical.files.clone());
        let fallback_plan = if fts5_degraded_to_trigram_body {
            let mut cloned = plan.clone();
            cloned.active_safety_lane = LaneKind::TrigramBody;
            Some(cloned)
        } else {
            None
        };
        let trigram_plan = fallback_plan.as_ref().unwrap_or(plan);
        candidate_sets.extend(trigram_adapter.retrieve(query, trigram_plan));
    }

    // 2. Fuse with RRF + ExactHitFloor, then apply graph expansion/enrichment
    // before converting to public results.
    let mut fused = RRFFusionEngine::fuse(plan, candidate_sets.clone());
    let graph_health = apply_graph_intelligence(ctx, plan, &mut candidate_sets, &mut fused);
    let ranking_config = RankingFeaturesConfig {
        exact_definition_boost: plan.ranking_profile.exact_definition_boost,
        identifier_stem_match_boost: plan.ranking_profile.stem_match_boost,
        path_base_match_boost: plan.ranking_profile.path_base_match_boost,
        doc_comment_boost: plan.ranking_profile.doc_comment_boost,
        same_file_coherence_boost: plan.ranking_profile.same_file_coherence_boost,
        test_example_penalty: plan.ranking_profile.test_example_penalty,
    };
    let ranking_feature_reports =
        apply_ranking_features(ctx, &mut fused, query, plan, &ranking_config);
    fused.sort_by(|left, right| {
        right
            .final_score
            .partial_cmp(&left.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .rrf_score
                    .partial_cmp(&left.rrf_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.file_path.cmp(&right.file_path))
    });

    // 3. Map FusedCandidates to HybridResult for enrich/rerank
    let mut hybrid_results: Vec<HybridResult> = fused
        .iter()
        .map(|fc| {
            let (start_line, end_line) = fc
                .line_range
                .map(|(start, end)| (start.saturating_sub(1), end.saturating_sub(1)))
                .unwrap_or((0, 0));
            HybridResult {
                file: fc.file_path.clone(),
                name: String::new(),
                kind: crate::symbols::SymbolKind::FileSummary,
                start_line: start_line as u32,
                end_line: end_line as u32,
                exported: false,
                score: fc.final_score,
                source: "ri_v2",
                semantic_score: None,
                lexical_score: None,
                hybrid_boosted: false,
                snippet: String::new(),
                provenance: Some(fc.provenance.clone()),
                is_exact_hit: fc.is_exact_hit,
                exact_hit_floor_applied: fc.exact_hit_floor_applied,
                graph_context: graph_context_from_fused_candidate(fc),
                enrichment_state: "not_enriched",
            }
        })
        .collect();

    // 4. Apply ContextBudget enrichment + rerank (reuse existing flow)
    let budget = &plan.context_budget;
    let mut context_budget_result = enrich_context_pool(&mut hybrid_results, budget);

    let rerank_enabled = plan.rerank.enabled;
    if rerank_enabled && context_budget_result.reranker_skipped_reason.is_none() {
        let (reranked_results, _outcome, input_count, path_only_input_count) =
            rerank_enriched_subset(&ctx.config().semantic, query, hybrid_results);
        context_budget_result.reranker_input_candidate_count = input_count;
        context_budget_result.path_only_reranker_input_count = path_only_input_count;
        hybrid_results = reranked_results;
    } else if rerank_enabled {
        context_budget_result.reranker_input_candidate_count = 0;
        context_budget_result.path_only_reranker_input_count = 0;
    }

    // Truncate to top_k
    hybrid_results.truncate(top_k);

    if let Err(error) = persist_retrieval_telemetry(
        ctx,
        query,
        &shape,
        profile,
        plan,
        &context_budget_result,
        &fused,
    ) {
        crate::slog_warn!("failed to persist retrieval telemetry: {}", error);
    }

    // 5. Build provenance data for diagnostics.
    let provenance = serde_json::json!({
        "lane_contributions": hybrid_results.iter().filter_map(|result| {
            fused_candidate_for_hybrid_result(result, &fused).map(|fc| {
            serde_json::json!({
                "file": fc.file_path.to_str().unwrap_or(""),
                "lanes": fc.provenance.lanes.iter().map(|l| serde_json::json!({
                    "lane": format!("{:?}", l.lane),
                    "rank": l.rank_in_lane,
                    "score": l.score_in_lane,
                })).collect::<Vec<_>>(),
            })
            })
        }).collect::<Vec<_>>(),
        "degraded_lanes": degraded_lanes,
        "context_budget": {
            "rerank_pool_size": context_budget_result.rerank_pool_size,
            "enriched_candidate_count": context_budget_result.enriched_candidate_count,
            "context_exhausted": context_budget_result.context_exhausted,
            "unenriched_candidate_count": context_budget_result.unenriched_candidate_count,
            "path_only_candidate_count": context_budget_result.path_only_candidate_count,
            "skipped_candidate_count": context_budget_result.skipped_candidate_count,
            "reranker_input_candidate_count": context_budget_result.reranker_input_candidate_count,
            "path_only_reranker_input_count": context_budget_result.path_only_reranker_input_count,
            "reranker_skipped_reason": context_budget_result.reranker_skipped_reason,
        },
        "graph": {
            "health": graph_health.label(),
        },
        "ranking_features": ranking_feature_reports,
    });

    (hybrid_results, Some(provenance))
}

fn apply_graph_intelligence(
    ctx: &AppContext,
    plan: &SearchPlan,
    candidate_sets: &mut Vec<CandidateSet>,
    fused: &mut Vec<crate::candidate::FusedCandidate>,
) -> GraphHealth {
    let config = ctx.config();
    let intelligence = config.intelligence.clone();
    if !config.callgraph_store || !intelligence.graph.enabled {
        let health = GraphHealth::Disabled;
        enrich_with_graph_context(fused, None, &health, &intelligence);
        return health;
    }

    match ctx.callgraph_store_for_ops() {
        CallgraphStoreAccess::Ready(store) => {
            let health = GraphHealth::Healthy;
            let expanded =
                GraphExpansionAdapter::expand(fused, Some(&*store), &health, &intelligence);
            if !expanded.is_empty() {
                candidate_sets.extend(expanded);
                *fused = RRFFusionEngine::fuse(plan, candidate_sets.clone());
            }
            enrich_with_graph_context(fused, Some(&*store), &health, &intelligence);
            health
        }
        CallgraphStoreAccess::Building | CallgraphStoreAccess::Unavailable => {
            let health = GraphHealth::Cold;
            enrich_with_graph_context(fused, None, &health, &intelligence);
            health
        }
        CallgraphStoreAccess::Error(error) => {
            crate::slog_warn!(
                "callgraph store unavailable for RI graph enrichment: {}",
                error
            );
            let health = GraphHealth::Corrupt;
            enrich_with_graph_context(fused, None, &health, &intelligence);
            health
        }
    }
}

fn persist_retrieval_telemetry(
    ctx: &AppContext,
    query: &str,
    shape: &QueryShape,
    profile: &str,
    plan: &SearchPlan,
    context_budget_result: &ContextBudgetResult,
    fused: &[crate::candidate::FusedCandidate],
) -> Result<(), String> {
    let telemetry_config = ctx.config().intelligence.telemetry.clone();
    if !telemetry_config.telemetry_persist || !telemetry_sample_allows(&telemetry_config, query) {
        return Ok(());
    }

    let Some(db) = ctx.db() else {
        return Ok(());
    };
    let conn = db
        .lock()
        .map_err(|_| "retrieval telemetry database lock poisoned".to_string())?;

    crate::telemetry::init_telemetry_schema(&conn)?;
    let backend_config = search_plan_to_json(plan).to_string();
    let run_id = crate::telemetry::write_retrieval_run(
        &conn,
        &telemetry_config,
        query,
        query_kind_label(shape.kind),
        0.0,
        profile,
        &backend_config,
        context_budget_result.context_exhausted,
        context_budget_result.reranker_skipped_reason.as_deref(),
    )?;

    let row_limit = telemetry_config.max_rows_per_run;
    let candidate_rows = fused
        .iter()
        .flat_map(|candidate| {
            candidate
                .provenance
                .lanes
                .iter()
                .map(move |lane| CandidateScoreRow {
                    chunk_id: telemetry_candidate_id(candidate),
                    source_lane: format!("{:?}", lane.lane),
                    raw_rank: lane.rank_in_lane as u32,
                    raw_score: lane.score_in_lane,
                    normalized_score: lane.rrf_contribution,
                    is_exact_hit: candidate.is_exact_hit,
                    exact_hit_floor_applied: candidate.exact_hit_floor_applied,
                })
        })
        .take(row_limit)
        .collect::<Vec<_>>();
    crate::telemetry::write_candidate_scores(&conn, &run_id, &candidate_rows)?;

    let fusion_rows = fused
        .iter()
        .take(row_limit)
        .map(|candidate| FusionScoreRow {
            chunk_id: telemetry_candidate_id(candidate),
            rrf_score: candidate.rrf_score,
            exact_hit_floor_applied: candidate.exact_hit_floor_applied,
            final_score: candidate.final_score,
            provenance_json: Some(candidate_provenance_to_json(&candidate.provenance).to_string()),
        })
        .collect::<Vec<_>>();
    crate::telemetry::write_fusion_scores(&conn, &run_id, &fusion_rows)?;

    Ok(())
}

fn telemetry_sample_allows(
    config: &crate::intelligence_config::TelemetryConfig,
    query: &str,
) -> bool {
    if config.sampling_rate >= 1.0 {
        return true;
    }
    if config.sampling_rate <= 0.0 {
        return false;
    }

    let hash = crate::telemetry::hash_query(query, &config.telemetry_query_hash_salt);
    let prefix = hash.get(..16).unwrap_or(&hash);
    let sample = u64::from_str_radix(prefix, 16).unwrap_or(0) as f64 / u64::MAX as f64;
    sample <= config.sampling_rate
}

fn telemetry_candidate_id(candidate: &crate::candidate::FusedCandidate) -> Option<String> {
    candidate
        .chunk_id
        .map(|id| format!("chunk:{id}"))
        .or_else(|| candidate.symbol_id.map(|id| format!("symbol:{id}")))
        .or_else(|| {
            candidate
                .line_range
                .map(|(start, end)| format!("{}:{start}-{end}", candidate.file_path.display()))
        })
        .or_else(|| Some(candidate.file_path.display().to_string()))
}

fn fused_candidate_for_hybrid_result<'a>(
    result: &HybridResult,
    fused: &'a [crate::candidate::FusedCandidate],
) -> Option<&'a crate::candidate::FusedCandidate> {
    fused.iter().find(|candidate| {
        let line_range = candidate.line_range.unwrap_or((0, 0));
        candidate.file_path == result.file
            && line_range.0.saturating_sub(1) as u32 == result.start_line
            && line_range.1.saturating_sub(1) as u32 == result.end_line
    })
}

fn graph_context_from_fused_candidate(
    candidate: &crate::candidate::FusedCandidate,
) -> Option<serde_json::Value> {
    candidate
        .context
        .as_deref()
        .and_then(|context| serde_json::from_str(context).ok())
}

fn fts5_plan_degraded_lanes(plan: &SearchPlan, reason: String) -> Vec<serde_json::Value> {
    plan.prefetch
        .iter()
        .filter(|retriever| {
            matches!(
                retriever.lane,
                LaneKind::FTS5Symbol
                    | LaneKind::FTS5Body
                    | LaneKind::FTS5Path
                    | LaneKind::FTS5Docs
                    | LaneKind::SymbolExact
            ) && (retriever.weight >= 0.1 || retriever.is_safety_lane)
        })
        .map(|retriever| {
            serde_json::json!({
                "lane": format!("{:?}", retriever.lane),
                "reason": reason,
                "fallback_used": if plan.active_safety_lane == LaneKind::FTS5Body {
                    Some("TrigramBody")
                } else {
                    None
                },
            })
        })
        .collect()
}

fn collect_lexical_files(ctx: &AppContext, query: &str, shape: &QueryShape) -> LexicalCollection {
    let search_index = ctx.search_index().borrow();
    let Some(index) = search_index.as_ref().filter(|index| index.ready) else {
        return LexicalCollection {
            files: Vec::new(),
            ready: false,
            engine_capped: false,
        };
    };

    // No `should_use_lexical` gate here: collect_lexical_files is only called
    // when choose_mode picked Hybrid, which already means we want the lexical
    // lane. The shape weight was a second, conflicting gate that suppressed
    // lexical for short NL concepts routed to Hybrid.
    //
    // NL shapes yield no tokens from extract_tokens (their words aren't code
    // identifiers), but a short NL concept routed to Hybrid (e.g. "parse
    // imports") is exactly the case where the literal words should hit the
    // trigram lane — so use the short-NL extractor there.
    let tokens = if shape.kind == QueryKind::NaturalLanguage {
        query_shape::extract_short_nl_lexical_tokens(query)
    } else {
        query_shape::extract_tokens(query, shape)
    };
    let token_refs = tokens.iter().map(String::as_str).collect::<Vec<_>>();
    let query_trigrams = SearchIndex::query_trigrams_from_tokens(&token_refs);
    // No extension filter: the trigram index already covers the project's text
    // files. Gating the lexical candidate set on the *semantic* extension
    // allow-list made named config/doc files (Cargo.toml, README.md,
    // package.json) structurally unreachable in hybrid mode — exactly the
    // literal-filename hits the lexical lane exists to catch.
    let ranked = index.lexical_rank_with_stats(&query_trigrams, None, LEXICAL_ENUMERATION_LIMIT);
    LexicalCollection {
        files: ranked.files,
        ready: true,
        engine_capped: ranked.engine_capped,
    }
}

fn search_index_ready(ctx: &AppContext) -> bool {
    ctx.search_index()
        .borrow()
        .as_ref()
        .is_some_and(|index| index.ready)
}

fn embed_query(query: &str, ctx: &AppContext) -> Result<(Vec<f32>, bool), String> {
    let mut model_ref = ctx.semantic_embedding_model().borrow_mut();
    let semantic_config = ctx.config().semantic.clone();

    if model_ref.is_none() {
        *model_ref = Some(EmbeddingModel::from_config(&semantic_config)?);
    }

    let model = model_ref
        .as_mut()
        .ok_or_else(|| "embedding model was not initialized".to_string())?;
    let (query_vector, query_cache_hit) = model
        .embed_query_cached(
            query,
            semantic_config.effective_query_prompt_template().as_deref(),
        )
        .map_err(|error| format!("failed to embed query: {error}"))?;

    if let Some(index) = ctx.semantic_index().borrow().as_ref() {
        if !index.is_empty() && index.dimension() != query_vector.len() {
            return Err(format!(
                "semantic embedding dimension mismatch: query backend returned {}, index expects {}. Rebuild the semantic index for the active backend/model.",
                query_vector.len(),
                index.dimension()
            ));
        }
    }

    Ok((query_vector, query_cache_hit))
}

fn collect_index_diag_warnings(ctx: &AppContext) -> Vec<SearchWarning> {
    let mut warnings = Vec::new();
    let config_metric = ctx
        .config()
        .semantic
        .distance_metric
        .as_ref()
        .map(|metric| {
            serde_json::to_value(metric)
                .ok()
                .and_then(|value| value.as_str().map(String::from))
                .unwrap_or_else(|| "cosine".to_string())
        })
        .unwrap_or_else(|| "cosine".to_string());
    if let Some(index) = ctx.semantic_index().borrow().as_ref() {
        if let Some(fingerprint) = index.fingerprint() {
            if !fingerprint.distance_metric.is_empty()
                && fingerprint.distance_metric != config_metric
            {
                warnings.push(SearchWarning::DistanceMetricChanged {
                    previous: fingerprint.distance_metric.clone(),
                    current: config_metric,
                });
            }
        }
    }
    warnings
}

pub fn fuse_hybrid_results(
    semantic: Vec<SemanticResult>,
    lexical_files: Vec<(PathBuf, f32)>,
    shape: &QueryShape,
    top_k: usize,
    max_results_per_file: usize,
) -> Vec<HybridResult> {
    if top_k == 0 {
        return Vec::new();
    }

    if lexical_files.is_empty() {
        return semantic
            .into_iter()
            .map(|result| hybrid_from_semantic(result, None))
            .take(top_k)
            .collect();
    }

    if semantic.is_empty() {
        return lexical_files
            .into_iter()
            .take(top_k)
            .map(|(file, score)| lexical_only_result(file, score, shape))
            .collect();
    }

    // Use every collected lexical candidate, not a hidden sub-cap. The lexical
    // lane already bounds enumeration at LEXICAL_ENUMERATION_LIMIT upstream and
    // returns candidates pre-ranked by score; an additional `.take(20)` here
    // silently dropped candidates 21..=50 from both the semantic-boost map and
    // the standalone-lexical results without that loss being reflected in
    // `more_available`/`engine_capped`. The final output is already bounded by
    // cap_per_file + truncate(top_k), so honoring all collected candidates is
    // both more correct and honest about what was considered.
    let lexical_top_files: HashMap<PathBuf, f32> = lexical_files.iter().cloned().collect();
    let mut results: Vec<HybridResult> = semantic
        .into_iter()
        .map(|result| {
            let lexical_score = lexical_top_files.get(&result.file).copied();
            hybrid_from_semantic(result, lexical_score)
        })
        .collect();

    let semantic_files: HashSet<PathBuf> =
        results.iter().map(|result| result.file.clone()).collect();
    for (file, score) in &lexical_files {
        if !semantic_files.contains(file) {
            results.push(lexical_only_result(file.clone(), *score, shape));
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut results = cap_per_file(results, max_results_per_file.max(1));
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
    results.truncate(top_k);
    results
}

fn hybrid_from_semantic(result: SemanticResult, lexical_score: Option<f32>) -> HybridResult {
    let semantic_score = result.score;
    let hybrid_boosted = lexical_score.is_some();
    let score = if hybrid_boosted {
        semantic_score * HYBRID_LEXICAL_BOOST
    } else {
        semantic_score
    };

    HybridResult {
        file: result.file,
        name: result.name,
        kind: result.kind,
        start_line: result.start_line,
        end_line: result.end_line,
        exported: result.exported,
        snippet: result.snippet,
        score,
        source: if result.source == "ri_v2" {
            "ri_v2"
        } else {
            "semantic"
        },
        semantic_score: Some(semantic_score),
        lexical_score,
        hybrid_boosted,
        provenance: None,
        is_exact_hit: false,
        exact_hit_floor_applied: false,
        graph_context: None,
        enrichment_state: "not_applicable",
    }
}

fn lexical_only_result(file: PathBuf, lexical_score: f32, shape: &QueryShape) -> HybridResult {
    HybridResult {
        file,
        name: String::new(),
        kind: SymbolKind::FileSummary,
        start_line: 0,
        end_line: 0,
        exported: false,
        // Lexical scores are not cosine-normalized and can exceed the semantic
        // lane's score scale. Keep lexical-only files visible without letting
        // broad trigram overlaps evict strong semantic matches.
        score: (lexical_score * shape_dependent_lexical_only_weight(shape))
            .min(LEXICAL_ONLY_SCORE_CEILING),
        source: "lexical",
        semantic_score: None,
        lexical_score: Some(lexical_score),
        hybrid_boosted: false,
        snippet: "[lexical match — use aft_zoom or read for context]".to_string(),
        provenance: None,
        is_exact_hit: false,
        exact_hit_floor_applied: false,
        graph_context: None,
        enrichment_state: "not_applicable",
    }
}

fn shape_dependent_lexical_only_weight(shape: &QueryShape) -> f32 {
    match shape.kind {
        QueryKind::Identifier => 0.8,
        QueryKind::Path | QueryKind::ErrorCode | QueryKind::Mixed => 0.5,
        QueryKind::NaturalLanguage | QueryKind::Regex => 0.0,
    }
}

fn cap_per_file(results: Vec<HybridResult>, cap: usize) -> Vec<HybridResult> {
    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    let mut capped = Vec::new();
    for result in results {
        let count = counts.entry(result.file.clone()).or_insert(0);
        if *count < cap {
            *count += 1;
            capped.push(result);
        }
    }
    capped
}

fn rerank_enriched_subset(
    config: &crate::config::SemanticBackendConfig,
    query: &str,
    results: Vec<HybridResult>,
) -> (Vec<HybridResult>, RerankOutcome, usize, usize) {
    let enriched_indices = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| (result.enrichment_state == "enriched").then_some(index))
        .collect::<Vec<_>>();
    let path_only_input_count = enriched_indices
        .iter()
        .filter(|&&index| results[index].enrichment_state == "path_only")
        .count();
    let reranker_input_count = enriched_indices.len();
    let enriched_results = enriched_indices
        .iter()
        .map(|&index| results[index].clone())
        .collect::<Vec<_>>();

    match rerank_candidates(config, query, &enriched_results) {
        RerankOutcome::ReRanked(indices) => {
            let n = enriched_results.len();
            let mut used = vec![false; n];
            let mut reordered = indices
                .iter()
                .filter_map(|&index| {
                    if index < n && !used[index] {
                        used[index] = true;
                        Some(enriched_results[index].clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            for (index, result) in enriched_results.iter().enumerate() {
                if !used[index] {
                    reordered.push(result.clone());
                }
            }
            for (index, result) in results.iter().enumerate() {
                if !enriched_indices.contains(&index) {
                    reordered.push(result.clone());
                }
            }
            (
                reordered,
                RerankOutcome::ReRanked(indices),
                reranker_input_count,
                path_only_input_count,
            )
        }
        RerankOutcome::Skipped => (
            results,
            RerankOutcome::Skipped,
            reranker_input_count,
            path_only_input_count,
        ),
        RerankOutcome::Failed(error) => (
            results,
            RerankOutcome::Failed(error),
            reranker_input_count,
            path_only_input_count,
        ),
    }
}

fn semantic_error_response(request_id: &str, error: &str) -> Response {
    if is_onnx_runtime_unavailable(error) {
        return Response::error(
            request_id,
            "semantic_search_unavailable",
            format!("Semantic search unavailable: {error}"),
        );
    }

    Response::error(
        request_id,
        "semantic_search_failed",
        format!("semantic_search: {error}"),
    )
}

fn format_lexical_unavailable_text(
    detail: &str,
    results: &[HybridResult],
    project_root: &Path,
) -> String {
    if results.is_empty() {
        return format!(
            "{detail}\nSemantic search unavailable; lexical-only fallback returned 0 result(s). [semantic: unavailable]"
        );
    }

    format!(
        "{detail}\nSemantic search unavailable; returning lexical-only fallback results.\n\n{}\n\nFound {} lexical fallback result(s). [semantic: unavailable]",
        format_result_sections(results, project_root),
        results.len()
    )
}

fn format_grep_lexical_unavailable_text(
    detail: &str,
    result: &GrepResult,
    project_root: &Path,
) -> String {
    if result.matches.is_empty() {
        return format!(
            "{detail}\nSemantic search unavailable; lexical-only fallback returned 0 result(s). [semantic: unavailable]"
        );
    }

    format!(
        "{detail}\nSemantic search unavailable; returning lexical-only fallback results.\n\n{}\n\nFound {} lexical fallback result(s). [semantic: unavailable]",
        crate::commands::grep::format_grep_text(result, project_root),
        result.matches.len()
    )
}

fn building_lexical_note(lexical_index_ready: bool) -> &'static str {
    if lexical_index_ready {
        "Semantic index is rebuilding; results are lexical-only fallback results from the trigram index."
    } else {
        "Semantic index is rebuilding; lexical fallback is unavailable because the trigram index is not ready."
    }
}

fn format_building_lexical_text(
    detail: &str,
    results: &[HybridResult],
    project_root: &Path,
    lexical_index_ready: bool,
) -> String {
    let note = building_lexical_note(lexical_index_ready);
    if results.is_empty() {
        return format!(
            "{detail}\n{note}\nFound 0 lexical fallback result(s). [semantic: rebuilding]"
        );
    }

    format!(
        "{detail}\n{note}\n\n{}\n\nFound {} lexical fallback result(s). [semantic: rebuilding]",
        format_result_sections(results, project_root),
        results.len()
    )
}

/// Top semantic cosine below this floor means the embedder found nothing
/// genuinely relevant — the query likely whiffed. We don't show the raw score
/// (uncalibrated for ranking), but its absolute floor is a real signal: an
/// all-weak result set looks identical to a strong one without it.
const WEAK_MATCH_COSINE_FLOOR: f32 = 0.35;

/// True when the best result's raw semantic cosine is below the weak floor.
/// Uses `semantic_score` (the raw cosine), not the fused `score`. Lexical-only
/// top results have no cosine and are not flagged here (lexical relevance is
/// judged differently).
fn results_are_low_confidence(results: &[HybridResult]) -> bool {
    results
        .first()
        .and_then(|r| r.semantic_score)
        .is_some_and(|cosine| cosine < WEAK_MATCH_COSINE_FLOOR)
}

fn format_semantic_text(
    results: &[HybridResult],
    project_root: &Path,
    more_available: bool,
    snippets_incomplete: bool,
) -> String {
    if results.is_empty() {
        return "Found 0 results.".to_string();
    }

    let mut text = format_result_sections(results, project_root);
    // Drop the unconditional "[index: ready]" tag — it was pure per-call tax on
    // the common path. Degraded/building/unavailable paths carry their own
    // distinct "[semantic: ...]" labels, so absence of a label means ready.
    text.push_str(&format!("\n\nFound {} result(s).", results.len()));
    if more_available {
        text.push_str(" More results available; raise topK to see more.");
    }
    // Recover the "did the search whiff" signal we lost by hiding the score:
    // one coarse flag when the top match is weak, so the agent reformulates or
    // falls back to grep instead of trusting a uniformly-weak ranking.
    if results_are_low_confidence(results) {
        text.push_str("\nTop match is weak — consider rephrasing or using grep for exact terms.");
    }
    // Only when snippet content was actually withheld (omitted for rank 4+, or
    // truncated within the top 3) — so the hint appears exactly when it's
    // actionable, not on every search.
    if snippets_incomplete {
        text.push_str("\nZoom any result for full source: aft_zoom <file> <symbol>.");
    }
    text
}

fn format_grep_search_text(
    result: &GrepResult,
    project_root: &Path,
    interpreted_as: &str,
) -> String {
    let base = crate::commands::grep::format_grep_text(result, project_root);
    format!("{base}\n[interpreted_as: {interpreted_as}]")
}

/// Snippet line budget by global rank (0-based). The fused score is an
/// uncalibrated, scale-mixed artifact (raw cosine for semantic-only hits,
/// cosine×boost for lexically-co-matched hits), so it is NOT shown to the
/// agent — position conveys rank. We spend snippet tokens by rank instead: the
/// top hit is disproportionately likely to be the final answer (a fuller
/// Replace each result's display snippet with source lines read on the fly from
/// disk. Snippets are display-only (they never affect embeddings), so reading
/// them at query time keeps the on-disk index free of display text.
/// Rank 0 gets a fuller preview (20 lines); ranks 1-2 get 5 lines; rank 3+
/// shows header only. Lexical rows keep their placeholder and file summaries
/// keep the generated summary. Returns true when any snippet was truncated or
/// omitted, so the caller emits the zoom hint only when it is actionable.
///
/// NOTE: The old `snippet_line_budget(rank)` function has been removed as part
/// of the Retrieval Intelligence v1 (T3a). The budget logic is now inlined
/// here and will be replaced by ContextBudget in a later wiring Bead.
fn enrich_snippets_from_source(results: &mut [HybridResult]) -> bool {
    // Cache reads so two top-3 hits in the same file read it once.
    let mut file_lines: HashMap<PathBuf, Option<Vec<String>>> = HashMap::new();
    let mut incomplete = false;

    for (rank, result) in results.iter_mut().enumerate() {
        if result.source == "lexical" || matches!(result.kind, SymbolKind::FileSummary) {
            continue;
        }

        // Inlined budget: rank 0 → 20 lines, ranks 1-2 → 5 lines, rank 3+ → 0.
        let budget = match rank {
            0 => 20,
            1 | 2 => 5,
            _ => 0,
        };
        if budget == 0 {
            // Header-only tier: a real body means there is more to see.
            if result.end_line >= result.start_line {
                incomplete = true;
            }
            result.snippet = String::new();
            continue;
        }

        let lines = file_lines.entry(result.file.clone()).or_insert_with(|| {
            std::fs::read_to_string(&result.file)
                .ok()
                .map(|content| content.lines().map(str::to_string).collect())
        });

        let Some(lines) = lines else {
            // File unreadable or gone — no snippet beats a stale one.
            result.snippet = String::new();
            continue;
        };

        // start_line/end_line are 0-based inclusive; +1 makes an exclusive bound.
        let start = (result.start_line as usize).min(lines.len());
        let end = ((result.end_line as usize) + 1).min(lines.len());
        if start >= end {
            result.snippet = String::new();
            continue;
        }

        let range_len = end - start;
        let shown = range_len.min(budget);
        let mut snippet = lines[start..start + shown].join("\n");
        let remaining = range_len - shown;
        if remaining > 0 {
            // "lines" is load-bearing: a bare "+N more" reads as "N more
            // results" to a weak model, prompting a wrong topK bump. This is
            // N more lines of THIS symbol's body — zoom to see them.
            snippet.push_str(&format!("\n+{remaining} more lines"));
            incomplete = true;
        }
        result.snippet = snippet;
    }

    incomplete
}

/// Enrich the rerank pool with context BEFORE reranking (ROOT CAUSE fix).
///
/// This function runs over the full rerank pool in rank order, reading source
/// content for each candidate up to the per_candidate_tokens budget. Candidates
/// that exceed the budget get a PathOnly fallback string.
///
/// PathOnly candidates are marked for exclusion from the content reranker.
/// Returns a ContextBudgetResult with exhaustion status.
///
/// This replaces the old flow where enrich_snippets_from_source ran AFTER
/// rerank_candidates(), leaving the reranker with empty snippets for rank 3+.
fn enrich_context_pool(
    results: &mut [HybridResult],
    budget: &ContextBudget,
) -> ContextBudgetResult {
    let mut total_tokens_used: usize = 0;
    let mut enriched_count: usize = 0;
    let mut unenriched_count: usize = 0;
    let mut path_only_count: usize = 0;
    let mut skipped_count: usize = 0;
    let mut file_lines_cache: HashMap<PathBuf, Option<Vec<String>>> = HashMap::new();

    for result in results.iter_mut() {
        let has_line_range =
            result.end_line >= result.start_line && (result.start_line > 0 || result.end_line > 0);

        // Lexical whole-file matches and true file-summary results don't get enriched.
        if result.source == "lexical"
            || (matches!(result.kind, SymbolKind::FileSummary) && !has_line_range)
        {
            result.enrichment_state = "skipped_unsupported";
            skipped_count += 1;
            continue;
        }

        let next_tokens_used = total_tokens_used.saturating_add(budget.per_candidate_tokens);
        let fits_strict = next_tokens_used <= budget.total_tokens;
        let fits_soft = total_tokens_used < budget.total_tokens
            && next_tokens_used
                <= budget
                    .total_tokens
                    .saturating_add(budget.soft_overflow_tokens);

        // Check if budget is exhausted. Soft overflow allows exactly the
        // candidate that crosses the cap; later candidates remain PathOnly.
        if !fits_strict && !fits_soft {
            // Budget exhausted: PathOnly fallback
            let fallback = match (result.start_line, result.end_line) {
                (start, end) if end >= start => {
                    format!(
                        "{}:{}-{} [budget_exhausted]",
                        result.file.display(),
                        start,
                        end
                    )
                }
                _ => format!("{} [budget_exhausted]", result.file.display()),
            };
            result.snippet = fallback;
            result.enrichment_state = "path_only";
            unenriched_count += 1;
            path_only_count += 1;
            continue;
        }

        // Enrich: read source lines
        let lines = file_lines_cache
            .entry(result.file.clone())
            .or_insert_with(|| {
                std::fs::read_to_string(&result.file)
                    .ok()
                    .map(|content| content.lines().map(str::to_string).collect())
            });

        if let Some(lines) = lines {
            let start = (result.start_line as usize).min(lines.len());
            let end = ((result.end_line as usize) + 1).min(lines.len());
            if start < end {
                let max_lines = budget.per_candidate_tokens / 4; // rough estimate: ~4 tokens/line
                let shown = (end - start).min(max_lines);
                let mut snippet = lines[start..start + shown].join("\n");
                let remaining = (end - start) - shown;
                if remaining > 0 {
                    snippet.push_str(&format!("\n+{remaining} more lines"));
                }
                result.snippet = snippet;
                result.enrichment_state = "enriched";
                enriched_count += 1;
                total_tokens_used = next_tokens_used;
            } else {
                // No source lines available: PathOnly fallback
                result.snippet = format!("{} [budget_exhausted]", result.file.display());
                result.enrichment_state = "path_only";
                unenriched_count += 1;
                path_only_count += 1;
            }
        } else {
            // File unreadable: PathOnly fallback
            result.snippet = format!("{} [budget_exhausted]", result.file.display());
            result.enrichment_state = "path_only";
            unenriched_count += 1;
            path_only_count += 1;
        }
    }

    let pool_size = results.len();
    let context_exhausted = path_only_count > 0;

    let reranker_skipped_reason = if pool_size == 0 {
        Some("no_candidates".to_string())
    } else if enriched_count == 0 {
        Some("no_enriched_candidates".to_string())
    } else {
        let ratio = enriched_count as f32 / pool_size as f32;
        if ratio < budget.rerank_min_enriched_ratio {
            Some("insufficient_enriched_ratio".to_string())
        } else {
            None
        }
    };

    ContextBudgetResult {
        rerank_pool_size: pool_size,
        enriched_candidate_count: enriched_count,
        context_exhausted,
        unenriched_candidate_count: unenriched_count,
        path_only_candidate_count: path_only_count,
        skipped_candidate_count: skipped_count,
        reranker_input_candidate_count: 0,
        path_only_reranker_input_count: 0,
        reranker_skipped_reason,
    }
}

fn format_result_sections(results: &[HybridResult], project_root: &Path) -> String {
    // Results arrive sorted by fused score desc. Group by file preserving
    // first-appearance order so the most relevant file's group renders first.
    // A BTreeMap would re-sort groups alphabetically by path and scramble the
    // ranking the agent relies on to read most-relevant-first. Snippets are
    // already budgeted by enrich_snippets_from_source; render them verbatim.
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&HybridResult>> = HashMap::new();

    for result in results.iter() {
        let display_path = result
            .file
            .strip_prefix(project_root)
            .unwrap_or(&result.file)
            .display()
            .to_string();
        if !groups.contains_key(&display_path) {
            group_order.push(display_path.clone());
        }
        groups.entry(display_path).or_default().push(result);
    }

    group_order
        .iter()
        .map(|file| {
            let mut section = file.clone();

            // Three distinct indent levels disambiguate the three roles for a
            // weak model at a glance: file path at col 0 (with its `/` and
            // extension), symbol header at 2 spaces, snippet body at 6. Without
            // this, file paths and symbol headers were both at col 0 and could
            // only be told apart by parsing the "[kind] lines X-Y" suffix.
            for result in &groups[file] {
                if result.source == "lexical" {
                    // Whole-file lexical match (no specific symbol).
                    section.push_str(" [lexical match]");
                    continue;
                }
                if matches!(result.kind, SymbolKind::FileSummary) {
                    section.push_str(&format!("\n  {} [file summary]", result.name));
                } else {
                    section.push_str(&format!(
                        "\n  {} [{}] lines {}-{}",
                        result.name,
                        symbol_kind_label(&result.kind),
                        display_line_number(result.start_line),
                        display_line_number(result.end_line),
                    ));
                }
                if !result.snippet.trim().is_empty() {
                    for line in result.snippet.lines() {
                        section.push_str("\n      ");
                        section.push_str(line);
                    }
                }
            }

            section
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn result_to_json(result: &HybridResult) -> serde_json::Value {
    let is_file_level = matches!(result.kind, SymbolKind::FileSummary);
    let (start_line, end_line) = if is_file_level {
        (serde_json::Value::Null, serde_json::Value::Null)
    } else {
        (
            serde_json::json!(display_line_number(result.start_line)),
            serde_json::json!(display_line_number(result.end_line)),
        )
    };

    let mut object = serde_json::json!({
        "file": result.file.display().to_string(),
        "name": result.name,
        "kind": result.kind,
        "start_line": start_line,
        "end_line": end_line,
        "location": if result.source == "lexical" { "[lexical match]" } else if is_file_level { "[file summary]" } else { "line range" },
        "score": result.score,
        "source": result.source,
        "semantic_score": result.semantic_score,
        "lexical_score": result.lexical_score,
        "hybrid_boosted": result.hybrid_boosted,
        "snippet": result.snippet,
    });

    if result.source == "ri_v2" || result.provenance.is_some() {
        let object = object
            .as_object_mut()
            .expect("result_to_json creates a JSON object");
        object.insert(
            "provenance".to_string(),
            result
                .provenance
                .as_ref()
                .map(candidate_provenance_to_json)
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "lanes": [],
                        "is_graph_expansion": false,
                        "graph_expansion_reason": null,
                    })
                }),
        );
        object.insert(
            "is_exact_hit".to_string(),
            serde_json::json!(result.is_exact_hit),
        );
        object.insert(
            "exact_hit_floor_applied".to_string(),
            serde_json::json!(result.exact_hit_floor_applied),
        );
        object.insert(
            "is_graph_expansion".to_string(),
            serde_json::json!(result
                .provenance
                .as_ref()
                .is_some_and(|provenance| provenance.is_graph_expansion)),
        );
        object.insert(
            "graph_context".to_string(),
            result
                .graph_context
                .clone()
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "enrichment_state".to_string(),
            serde_json::json!(result.enrichment_state),
        );
    }

    object
}

fn candidate_provenance_to_json(provenance: &CandidateProvenance) -> serde_json::Value {
    serde_json::json!({
        "lanes": provenance.lanes.iter().map(|lane| serde_json::json!({
            "lane": format!("{:?}", lane.lane),
            "rank": lane.rank_in_lane,
            "score": lane.score_in_lane,
            "rrf_contribution": lane.rrf_contribution,
        })).collect::<Vec<_>>(),
        "is_graph_expansion": provenance.is_graph_expansion,
        "graph_expansion_reason": provenance.graph_expansion_reason.clone(),
    })
}

fn grep_match_to_json(grep_match: &GrepMatch, source: &'static str) -> serde_json::Value {
    serde_json::json!({
        "kind": "GrepLine",
        "source": source,
        "file": grep_match.file.display().to_string(),
        "line": grep_match.line,
        "column": grep_match.column,
        "line_text": grep_match.line_text,
        "match_text": grep_match.match_text,
    })
}

/// Convert a SearchPlan to a JSON value for the search_plan_debug field.
fn search_plan_to_json(plan: &SearchPlan) -> serde_json::Value {
    let lane_weights: serde_json::Map<String, serde_json::Value> = plan
        .lane_weights
        .iter()
        .map(|(lane, weight)| {
            let key = format!("{lane:?}");
            (key, serde_json::json!(weight))
        })
        .collect();

    let mandatory_lanes = plan
        .mandatory_lanes
        .iter()
        .map(|lane| serde_json::json!(format!("{lane:?}")))
        .collect::<Vec<_>>();

    let suppressed_lanes = plan
        .suppressed_lanes
        .iter()
        .map(|suppressed| {
            serde_json::json!({
                "lane": format!("{:?}", suppressed.lane),
                "reason": suppressed.reason.as_str(),
            })
        })
        .collect::<Vec<_>>();

    let prefetch = plan
        .prefetch
        .iter()
        .map(|p| {
            serde_json::json!({
                "lane": format!("{:?}", p.lane),
                "weight": p.weight,
                "max_candidates": p.max_candidates,
                "is_safety_lane": p.is_safety_lane,
                "latency_budget_ms": p.latency_budget_ms,
            })
        })
        .collect::<Vec<_>>();

    let max_candidates_per_lane: serde_json::Map<String, serde_json::Value> = plan
        .prefetch
        .iter()
        .map(|p| {
            let key = format!("{:?}", p.lane);
            (key, serde_json::json!(p.max_candidates))
        })
        .collect();

    serde_json::json!({
        "intent": format!("{:?}", plan.intent),
        "lane_weights": lane_weights,
        "mandatory_lanes": mandatory_lanes,
        "suppressed_lanes": suppressed_lanes,
        "prefetch": prefetch,
        "fusion": {
            "rrf_k": plan.fusion.rrf_k,
            "exact_hit_floor_n": plan.fusion.exact_hit_floor_n,
        },
        "ranking_profile": {
            "exact_definition_boost": plan.ranking_profile.exact_definition_boost,
            "stem_match_boost": plan.ranking_profile.stem_match_boost,
            "path_base_match_boost": plan.ranking_profile.path_base_match_boost,
            "doc_comment_boost": plan.ranking_profile.doc_comment_boost,
            "same_file_coherence_boost": plan.ranking_profile.same_file_coherence_boost,
            "test_example_penalty": plan.ranking_profile.test_example_penalty,
        },
        "context_budget": {
            "total_tokens": plan.context_budget.total_tokens,
            "per_candidate_tokens": plan.context_budget.per_candidate_tokens,
            "min_candidate_chars": plan.context_budget.min_candidate_chars,
            "soft_overflow_tokens": plan.context_budget.soft_overflow_tokens,
            "mode": format!("{:?}", plan.context_budget.mode),
            "enrich_pool": format!("{:?}", plan.context_budget.enrich_pool),
            "rerank_min_enriched_ratio": plan.context_budget.rerank_min_enriched_ratio,
        },
        "rerank": {
            "enabled": plan.rerank.enabled,
            "max_candidates": plan.rerank.max_candidates,
        },
        "diagnostics_level": format!("{:?}", plan.diagnostics),
        "active_safety_lane": format!("{:?}", plan.active_safety_lane),
        "feature_flag_state": format!("{:?}", plan.feature_flag_state),
        "context_mode": format!("{:?}", plan.context_budget.mode),
        "enrich_pool": format!("{:?}", plan.context_budget.enrich_pool),
        "max_candidates_per_lane": max_candidates_per_lane,
    })
}

fn display_line_number(line: u32) -> u32 {
    line.saturating_add(1)
}

fn symbol_kind_label(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::TypeAlias => "type_alias",
        SymbolKind::Variable => "variable",
        SymbolKind::Heading => "heading",
        SymbolKind::FileSummary => "file-summary",
    }
}

fn semantic_status_label(status: &SemanticIndexStatus) -> &'static str {
    match status {
        SemanticIndexStatus::Ready { .. } => "ready",
        SemanticIndexStatus::Partial { .. } => "partial",
        SemanticIndexStatus::Building { .. } => "building",
        SemanticIndexStatus::Disabled => "disabled",
        SemanticIndexStatus::Failed(_) => "unavailable",
    }
}

fn interpreted_as_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Regex => "regex",
        SearchMode::Literal => "literal",
        SearchMode::Semantic => "semantic",
        SearchMode::Hybrid => "hybrid",
    }
}

/// Honest `interpreted_as` for a response built on a semantic-unavailable
/// fallback path. The query may have been *routed* as semantic/hybrid, but if
/// semantic never executed, the field must report what actually produced the
/// results — otherwise an agent reads "hybrid" and trusts a semantic ranking
/// that never ran. `lexical_ran` is true when the lexical (trigram) lane
/// produced the returned results; otherwise we report the routed mode (the
/// attempt), with the `semantic_unavailable`/`status` fields conveying that it
/// could not run.
fn fallback_executed_label(mode: SearchMode, lexical_ran: bool) -> &'static str {
    if lexical_ran {
        "lexical"
    } else {
        interpreted_as_label(mode)
    }
}

fn query_kind_label(kind: QueryKind) -> &'static str {
    match kind {
        QueryKind::Identifier => "Identifier",
        QueryKind::Mixed => "Mixed",
        QueryKind::ErrorCode => "ErrorCode",
        QueryKind::Path => "Path",
        QueryKind::Regex => "Regex",
        QueryKind::NaturalLanguage => "NaturalLanguage",
    }
}

/// Strip a single matched pair of surrounding `"` or `'` from a literal
/// query, matching the convention agents and humans bring from GitHub code
/// search, `rg -F "..."`, and most search engines. Only strips ONE pair, and
/// only when leading + trailing match — `'foo"` is left alone, and pre-stripped
/// queries like `foo` are returned unchanged.
fn strip_surrounding_quotes(query: String) -> String {
    let trimmed = query.trim();
    if trimmed.len() < 2 {
        return query;
    }
    let first = trimmed.chars().next().unwrap();
    let last = trimmed.chars().next_back().unwrap();
    if (first == '"' || first == '\'') && first == last {
        let mut chars = trimmed.chars();
        chars.next();
        chars.next_back();
        return chars.as_str().to_string();
    }
    query
}

fn literal_tokens_all_short(query: &str) -> bool {
    let tokens = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    !tokens.is_empty() && tokens.iter().all(|token| token.len() < 3)
}

fn extracted_tokens_all_short(query: &str, shape: &QueryShape) -> bool {
    let tokens = query_shape::extract_tokens(query, shape);
    !tokens.is_empty() && tokens.iter().all(|token| token.len() < 3)
}

pub fn humanize_degraded_reasons(reasons: &[String]) -> Vec<String> {
    reasons.iter().map(|code| humanize_one(code)).collect()
}

fn humanize_one(code: &str) -> String {
    if code == "home_root" {
        return "Project root is set to your home directory; large file-system indexes are disabled to avoid scanning the whole home tree.".into();
    }
    if let Some(threshold) = code.strip_prefix("search_too_many_files:") {
        return format!(
            "Project source-file count exceeds search_index threshold ({} files); trigram index disabled. Narrow project_root or open a smaller subdirectory.",
            threshold
        );
    }
    if code == "watcher_unavailable" {
        return "file watcher unavailable; continuing without live external-change invalidation"
            .to_string();
    }
    format!("(Degraded: {})", code)
}

fn degraded_warning(ctx: &AppContext) -> String {
    let mut text = "Lexical search ran in degraded full-file-scan mode.".to_string();
    let reasons = ctx.degraded_reasons();
    if !reasons.is_empty() {
        text.push_str(" Reasons: ");
        text.push_str(&humanize_degraded_reasons(&reasons).join("; "));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SemanticBackend, SemanticBackendConfig};
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::semantic_index::SemanticIndex;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::thread;

    fn semantic_request(query: &str, top_k: usize) -> RawRequest {
        serde_json::from_value(serde_json::json!({
            "id": "semantic-search-test",
            "command": "semantic_search",
            "query": query,
            "top_k": top_k,
        }))
        .expect("build semantic search request")
    }

    fn semantic_request_with_hint(query: &str, top_k: usize, hint: &str) -> RawRequest {
        serde_json::from_value(serde_json::json!({
            "id": "semantic-search-test",
            "command": "semantic_search",
            "query": query,
            "top_k": top_k,
            "hint": hint,
        }))
        .expect("build semantic search request")
    }

    fn semantic_request_with_context_budget(
        query: &str,
        top_k: usize,
        total_tokens: usize,
        per_candidate_tokens: usize,
        soft_overflow_tokens: usize,
    ) -> RawRequest {
        serde_json::from_value(serde_json::json!({
            "id": "semantic-search-test",
            "command": "semantic_search",
            "query": query,
            "top_k": top_k,
            "context_budget_enabled": true,
            "context_total_tokens": total_tokens,
            "context_per_candidate_tokens": per_candidate_tokens,
            "context_soft_overflow_tokens": soft_overflow_tokens,
        }))
        .expect("build semantic search request with context budget")
    }

    fn response_value(response: Response) -> serde_json::Value {
        serde_json::to_value(response).expect("serialize response")
    }

    fn test_context(project_root: &Path) -> AppContext {
        AppContext::new(
            Box::new(TreeSitterProvider::new()),
            Config {
                project_root: Some(project_root.to_path_buf()),
                ..Config::default()
            },
        )
    }

    fn start_mock_embedding_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind embedding server");
        let addr = listener.local_addr().expect("embedding server addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept embedding request");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let n = stream.read(&mut chunk).expect("read embedding request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        for line in String::from_utf8_lossy(&buf[..pos + 4]).lines() {
                            if let Some(value) = line.strip_prefix("Content-Length:") {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }

            let body = r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write embedding response");
        });

        (format!("http://{}", addr), handle)
    }

    #[test]
    fn short_nl_concept_routes_to_hybrid_when_lexical_ready() {
        // "parse imports" classifies as a two-word lowercase NL concept, but it
        // is a literal code phrase the trigram lane can hit. With lexical ready
        // it must route to Hybrid (run the lexical lane), not pure Semantic.
        let shape = query_shape::classify("parse imports");
        assert_eq!(shape.kind, QueryKind::NaturalLanguage);
        let mut warnings = Vec::new();
        let mode = choose_mode(
            SearchHint::Auto,
            "parse imports",
            &shape,
            true,
            &mut warnings,
        );
        assert_eq!(mode, SearchMode::Hybrid);
    }

    #[test]
    fn long_nl_phrase_stays_semantic() {
        // A longer NL phrase (>2 words) is a genuine concept query → pure
        // Semantic; the lexical lane would only add noise.
        let q = "how does the bridge resolve the binary";
        let shape = query_shape::classify(q);
        assert_eq!(shape.kind, QueryKind::NaturalLanguage);
        let mut warnings = Vec::new();
        let mode = choose_mode(SearchHint::Auto, q, &shape, true, &mut warnings);
        assert_eq!(mode, SearchMode::Semantic);
    }

    #[test]
    fn short_nl_extracts_lexical_tokens() {
        // The short-NL Hybrid path needs tokens; extract_tokens returns none for
        // NL, so collect_lexical_files uses the short-NL extractor.
        let tokens = query_shape::extract_short_nl_lexical_tokens("parse imports");
        assert_eq!(tokens, vec!["parse".to_string(), "imports".to_string()]);
        // Sub-3-char words are dropped (trigram floor).
        let tokens2 = query_shape::extract_short_nl_lexical_tokens("go to");
        assert!(tokens2.is_empty());
    }

    #[test]
    fn building_status_returns_lexical_fallback_results() {
        let project = tempfile::tempdir().expect("create project dir");
        let source_file = project.path().join("src/lib.rs");
        std::fs::create_dir_all(source_file.parent().expect("source parent"))
            .expect("create source dir");
        let source = "pub fn needle_symbol() -> bool { true }\n";
        std::fs::write(&source_file, source).expect("write source file");

        let ctx = test_context(project.path());
        let mut index = SearchIndex::new();
        index.index_file(&source_file, source.as_bytes());
        index.ready = true;
        *ctx.search_index().borrow_mut() = Some(index);
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Building {
            stage: "embedding".to_string(),
            files: Some(1),
            entries_done: Some(0),
            entries_total: Some(1),
        };

        let response = response_value(handle_semantic_search(
            &semantic_request("needle_symbol", 5),
            &ctx,
        ));

        assert_eq!(response["success"], true);
        assert_eq!(response["status"], "building");
        assert_eq!(response["semantic_status"], "building");
        // While semantic builds, only the lexical lane produced results — so
        // interpreted_as honestly reports "lexical", not the routed "hybrid"
        // mode that hasn't executed yet. The "building" status + note convey
        // that semantic results are still coming.
        assert_eq!(response["interpreted_as"], "lexical");
        assert!(response["note"]
            .as_str()
            .expect("note")
            .contains("lexical-only fallback"));
        assert!(response["text"]
            .as_str()
            .expect("text")
            .contains("lexical fallback"));
        let results = response["results"].as_array().expect("results array");
        assert!(
            results.iter().any(|result| {
                result["source"] == "lexical"
                    && result["file"]
                        .as_str()
                        .expect("file")
                        .ends_with("src/lib.rs")
            }),
            "expected lexical fallback result, got {results:?}"
        );
    }

    #[test]
    fn regex_query_runs_without_semantic_index() {
        let project = tempfile::tempdir().expect("create project dir");
        let source_file = project.path().join("src/lib.rs");
        std::fs::create_dir_all(source_file.parent().expect("source parent"))
            .expect("create source dir");
        std::fs::write(&source_file, "pub fn exported() {}\n").expect("write source file");
        let ctx = test_context(project.path());
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Disabled;

        let response = response_value(handle_semantic_search(
            &semantic_request_with_hint(".*exported", 5, "regex"),
            &ctx,
        ));

        assert_eq!(response["success"], true);
        assert_eq!(response["interpreted_as"], "regex");
        assert_eq!(response["query_kind"], "Regex");
        assert_eq!(response["semantic_status"], "disabled");
        assert_eq!(response["results"][0]["kind"], "GrepLine");
    }

    #[test]
    fn literal_hint_short_token_warns_and_runs_grep_line_results() {
        let project = tempfile::tempdir().expect("create project dir");
        let source_file = project.path().join("src/lib.rs");
        std::fs::create_dir_all(source_file.parent().expect("source parent"))
            .expect("create source dir");
        std::fs::write(&source_file, "id = 1\n").expect("write source file");
        let ctx = test_context(project.path());

        let response = response_value(handle_semantic_search(
            &semantic_request_with_hint("id", 5, "literal"),
            &ctx,
        ));

        assert_eq!(response["success"], true);
        assert_eq!(response["interpreted_as"], "literal");
        assert!(response["warnings"][0]
            .as_str()
            .expect("warning")
            .contains("shorter than 3"));
    }

    #[test]
    fn unsupported_regex_returns_specific_error() {
        let project = tempfile::tempdir().expect("create project dir");
        let ctx = test_context(project.path());

        let response = response_value(handle_semantic_search(
            &semantic_request_with_hint("(?=foo)", 5, "regex"),
            &ctx,
        ));

        assert_eq!(response["success"], false);
        assert_eq!(response["code"], "unsupported_pattern");
        assert!(response["message"]
            .as_str()
            .expect("message")
            .contains("lookaround"));
    }

    #[test]
    fn humanize_degraded_reason_messages() {
        let reasons = vec![
            "home_root".to_string(),
            "search_too_many_files:20000".to_string(),
            "watcher_unavailable".to_string(),
            "custom".to_string(),
        ];
        let human = humanize_degraded_reasons(&reasons);
        assert!(human[0].contains("home directory"));
        assert!(human[1].contains("search_index threshold (20000 files)"));
        assert!(human[1].contains("Narrow project_root"));
        assert_eq!(
            human[2],
            "file watcher unavailable; continuing without live external-change invalidation"
        );
        assert_eq!(human[3], "(Degraded: custom)");
        assert!(human.join("; ").contains("; "));
    }

    #[test]
    fn semantic_candidate_limit_scales_with_small_top_k() {
        assert_eq!(semantic_candidate_limit(1), SEMANTIC_OVERFETCH_FLOOR);
        assert_eq!(semantic_candidate_limit(5), 15);
        assert_eq!(semantic_candidate_limit(100), MAX_TOP_K);
    }

    #[test]
    fn empty_semantic_index_skips_query_dimension_check() {
        let project = tempfile::tempdir().expect("create project dir");
        let (base_url, handle) = start_mock_embedding_server();
        let ctx = AppContext::new(
            Box::new(TreeSitterProvider::new()),
            Config {
                project_root: Some(project.path().to_path_buf()),
                semantic: SemanticBackendConfig {
                    backend: SemanticBackend::OpenAiCompatible,
                    model: "test-embedding".to_string(),
                    base_url: Some(base_url),
                    api_key_env: None,
                    timeout_ms: 5_000,
                    max_batch_size: 64,
                    max_files: 20_000,
                    ..SemanticBackendConfig::default()
                },
                ..Config::default()
            },
        );
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::ready();
        *ctx.semantic_index().borrow_mut() =
            Some(SemanticIndex::new(project.path().to_path_buf(), 384));

        let response = response_value(handle_semantic_search(
            &semantic_request("anything", 5),
            &ctx,
        ));

        assert_eq!(
            response["success"], true,
            "response should not fail: {response:?}"
        );
        assert_eq!(response["status"], "ready");
        assert_eq!(response["semantic_status"], "ready");
        assert!(response["results"].as_array().expect("results").is_empty());
        handle.join().expect("embedding server thread");
    }

    #[test]
    fn request_context_budget_enables_public_token_budget_filtering() {
        let project = tempfile::tempdir().expect("create project dir");
        let (base_url, handle) = start_mock_embedding_server();
        let ctx = AppContext::new(
            Box::new(TreeSitterProvider::new()),
            Config {
                project_root: Some(project.path().to_path_buf()),
                semantic: SemanticBackendConfig {
                    backend: SemanticBackend::OpenAiCompatible,
                    model: "test-embedding".to_string(),
                    base_url: Some(base_url),
                    api_key_env: None,
                    timeout_ms: 5_000,
                    max_batch_size: 64,
                    max_files: 20_000,
                    ..SemanticBackendConfig::default()
                },
                ..Config::default()
            },
        );
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::ready();
        *ctx.semantic_index().borrow_mut() =
            Some(SemanticIndex::new(project.path().to_path_buf(), 384));

        let response = response_value(handle_semantic_search(
            &semantic_request_with_context_budget("anything", 5, 4096, 384, 128),
            &ctx,
        ));

        assert_eq!(
            response["success"], true,
            "response should not fail: {response:?}"
        );
        let budget = &response["search_plan_debug"]["context_budget"];
        assert_eq!(budget["total_tokens"], 4096);
        assert_eq!(budget["per_candidate_tokens"], 384);
        assert_eq!(budget["soft_overflow_tokens"], 128);
        handle.join().expect("embedding server thread");
    }

    #[test]
    fn file_summary_text_uses_summary_location_instead_of_line_range() {
        let project_root = Path::new("/project");
        let results = vec![HybridResult {
            file: PathBuf::from("/project/src/index.ts"),
            name: "index".to_string(),
            kind: SymbolKind::FileSummary,
            start_line: 0,
            end_line: 0,
            exported: false,
            snippet: String::new(),
            score: 0.75,
            source: "semantic",
            semantic_score: Some(0.75),
            lexical_score: None,
            hybrid_boosted: false,
            provenance: None,
            is_exact_hit: false,
            exact_hit_floor_applied: false,
            graph_context: None,
            enrichment_state: "not_applicable",
        }];

        let text = format_semantic_text(&results, project_root, false, false);

        // File-summary rows show "[file summary]" with no line range, and no
        // longer leak the internal score/source.
        assert!(text.contains("index [file summary]"));
        assert!(!text.contains("lines 1-1"));
        assert!(!text.contains("score"));
        assert!(!text.contains("source semantic"));
    }

    /// A symbol hit whose `file` points at a real on-disk file with `body_lines`
    /// lines starting at line 0, so enrich_snippets_from_source can read it. The
    /// stored `snippet` is left empty on purpose — enrichment fills it from disk.
    fn write_symbol_hit(
        dir: &Path,
        file_name: &str,
        name: &str,
        body_lines: usize,
    ) -> HybridResult {
        let path = dir.join(file_name);
        let body = (0..body_lines)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &body).expect("write symbol file");
        HybridResult {
            file: path,
            name: name.to_string(),
            kind: SymbolKind::Function,
            start_line: 0,
            end_line: (body_lines.saturating_sub(1)) as u32,
            exported: false,
            snippet: String::new(),
            score: 0.5,
            source: "semantic",
            semantic_score: Some(0.5),
            lexical_score: None,
            hybrid_boosted: false,
            provenance: None,
            is_exact_hit: false,
            exact_hit_floor_applied: false,
            graph_context: None,
            enrichment_state: "not_applicable",
        }
    }

    #[test]
    fn rows_omit_score_and_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut results = vec![write_symbol_hit(dir.path(), "a.rs", "foo", 2)];
        let incomplete = enrich_snippets_from_source(&mut results);
        let text = format_semantic_text(&results, dir.path(), false, incomplete);
        assert!(text.contains("foo [function] lines 1-2"));
        assert!(!text.contains("score"));
        assert!(!text.contains("source"));
    }

    #[test]
    fn snippets_are_rank_tiered_top_three_only_from_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Five hits, each a 30-line body, in distinct files so grouping does not
        // merge them. Rank order = vector order (already sorted). Budgets:
        // rank 0 = 20 lines (+10 more lines), ranks 1-2 = 5 lines (+25 more
        // lines), rank 3+ = header only.
        let mut results: Vec<HybridResult> = (0..5)
            .map(|i| write_symbol_hit(dir.path(), &format!("f{i}.rs"), &format!("fn{i}"), 30))
            .collect();
        let incomplete = enrich_snippets_from_source(&mut results);
        assert!(incomplete);
        let text = format_semantic_text(&results, dir.path(), false, incomplete);

        assert!(text.contains("fn0 [function]"));
        // "lines" wording is load-bearing (vs "+N more" reading as results).
        assert!(text.contains("+10 more lines"));
        assert!(text.contains("+25 more lines"));
        // Rank 0 genuinely shows MORE than ranks 1-2 (gradient not inverted).
        let body_lines =
            |r: &HybridResult| r.snippet.lines().filter(|l| l.starts_with("line")).count();
        assert_eq!(body_lines(&results[0]), 20);
        assert_eq!(body_lines(&results[1]), 5);
        // Ranks 3,4 → header only, no body lines.
        assert!(
            results[3].snippet.is_empty(),
            "rank 4+ must have no snippet"
        );
        assert!(
            results[4].snippet.is_empty(),
            "rank 4+ must have no snippet"
        );
        // Zoom hint present because snippets were withheld.
        assert!(text.contains("aft_zoom <file> <symbol>"));
    }

    #[test]
    fn weak_top_match_emits_low_confidence_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hit = write_symbol_hit(dir.path(), "a.rs", "foo", 2);
        // Top semantic cosine below the weak floor.
        hit.semantic_score = Some(0.22);
        hit.score = 0.22;
        let results = vec![hit];
        let text = format_semantic_text(&results, dir.path(), false, false);
        assert!(
            text.contains("Top match is weak"),
            "expected weak-match note, got: {text}"
        );
    }

    #[test]
    fn strong_top_match_has_no_low_confidence_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hit = write_symbol_hit(dir.path(), "a.rs", "foo", 2);
        hit.semantic_score = Some(0.72);
        hit.score = 0.72;
        let results = vec![hit];
        let text = format_semantic_text(&results, dir.path(), false, false);
        assert!(!text.contains("Top match is weak"), "got: {text}");
        // And no unconditional "[index: ready]" tax on the happy path.
        assert!(!text.contains("[index: ready]"), "got: {text}");
    }

    #[test]
    fn no_zoom_hint_when_all_snippets_fit() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two small symbols (3 lines each), both within their rank budget.
        let mut results = vec![
            write_symbol_hit(dir.path(), "a.rs", "foo", 3),
            write_symbol_hit(dir.path(), "b.rs", "bar", 3),
        ];
        let incomplete = enrich_snippets_from_source(&mut results);
        assert!(!incomplete);
        let text = format_semantic_text(&results, dir.path(), false, incomplete);
        assert!(!text.contains("+"), "no truncation marker expected: {text}");
        assert!(!text.contains("aft_zoom"), "no zoom hint expected: {text}");
    }

    #[test]
    fn enrich_handles_missing_file_gracefully() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut results = vec![HybridResult {
            file: dir.path().join("does-not-exist.rs"),
            name: "ghost".to_string(),
            kind: SymbolKind::Function,
            start_line: 0,
            end_line: 9,
            exported: false,
            snippet: String::new(),
            score: 0.5,
            source: "semantic",
            semantic_score: Some(0.5),
            lexical_score: None,
            hybrid_boosted: false,
            provenance: None,
            is_exact_hit: false,
            exact_hit_floor_applied: false,
            graph_context: None,
            enrichment_state: "not_applicable",
        }];
        // Must not panic; header renders, no snippet body.
        let _ = enrich_snippets_from_source(&mut results);
        assert!(results[0].snippet.is_empty());
        let text = format_result_sections(&results, dir.path());
        assert!(text.contains("ghost [function]"));
    }

    #[test]
    fn groups_render_in_rank_order_not_alphabetical() {
        let dir = tempfile::tempdir().expect("tempdir");
        // zzz.rs holds the top hit, aaa.rs the second. Alphabetical grouping
        // (the old BTreeMap bug) would put aaa.rs first; rank order keeps zzz.
        let results = vec![
            write_symbol_hit(dir.path(), "zzz.rs", "top", 1),
            write_symbol_hit(dir.path(), "aaa.rs", "second", 1),
        ];
        let text = format_result_sections(&results, dir.path());
        let zzz_at = text.find("zzz.rs").expect("zzz present");
        let aaa_at = text.find("aaa.rs").expect("aaa present");
        assert!(zzz_at < aaa_at, "top-ranked file must render first: {text}");
    }

    #[test]
    fn more_available_appends_raise_topk_note() {
        let dir = tempfile::tempdir().expect("tempdir");
        let results = vec![write_symbol_hit(dir.path(), "a.rs", "foo", 1)];
        let text = format_semantic_text(&results, dir.path(), true, false);
        assert!(text.contains("More results available; raise topK to see more."));
    }

    #[test]
    fn file_summary_json_uses_summary_location_instead_of_line_numbers() {
        let result = HybridResult {
            file: PathBuf::from("/project/src/index.ts"),
            name: "index".to_string(),
            kind: SymbolKind::FileSummary,
            start_line: 0,
            end_line: 0,
            exported: false,
            snippet: String::new(),
            score: 0.75,
            source: "semantic",
            semantic_score: Some(0.75),
            lexical_score: None,
            hybrid_boosted: false,
            provenance: None,
            is_exact_hit: false,
            exact_hit_floor_applied: false,
            graph_context: None,
            enrichment_state: "not_applicable",
        };

        let json = result_to_json(&result);

        assert_eq!(json["kind"], "file_summary");
        assert_eq!(json["location"], "[file summary]");
        assert!(json["start_line"].is_null());
        assert!(json["end_line"].is_null());
        assert_eq!(json["source"], "semantic");
        assert_eq!(json["semantic_score"], 0.75);
        assert!(json["lexical_score"].is_null());
    }
}
