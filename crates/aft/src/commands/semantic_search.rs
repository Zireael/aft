use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::context::{AppContext, SemanticIndexStatus};
use crate::protocol::{RawRequest, Response};
use crate::query_shape::{self, QueryKind, QueryShape};
use crate::search_index::SearchIndex;
use crate::semantic_diagnostics::{
    format_diagnostics_prefix, score_statistics, top1_margin, PhaseTimer, SearchDiagnostics,
    SearchPipelineType, SearchWarning,
};
use crate::semantic_index::{
    is_onnx_runtime_unavailable, is_semantic_indexed_extension, EmbeddingModel, SemanticResult,
};
use crate::semantic_rerank::{rerank_candidates, RerankOutcome};
use crate::slog_info;
use crate::symbols::SymbolKind;

const DEFAULT_TOP_K: usize = 10;
const MAX_TOP_K: usize = 100;
const HYBRID_LEXICAL_BOOST: f32 = 1.1;
const LEXICAL_ONLY_SCORE_CEILING: f32 = 0.25;

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
    pub snippet: String,
}

#[derive(Debug, Deserialize)]
struct SemanticSearchParams {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

pub fn handle_semantic_search(req: &RawRequest, ctx: &AppContext) -> Response {
    let _pipeline_timer = PhaseTimer::start();
    let diagnostics_enabled = ctx.config().semantic.diagnostics_enabled();

    let params = match serde_json::from_value::<SemanticSearchParams>(req.params.clone()) {
        Ok(params) => params,
        Err(error) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("semantic_search: invalid params: {error}"),
            );
        }
    };

    let query_hash = SearchDiagnostics::hash_query(&params.query);
    let mut warnings: Vec<SearchWarning> = Vec::new();

    // Reject empty or whitespace-only queries early.
    if params.query.trim().is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "semantic_search: query must not be empty",
        );
    }

    // Snapshot index state for diagnostics.
    let index_state = {
        let status = ctx.semantic_index_status().borrow();
        match &*status {
            SemanticIndexStatus::Disabled => "disabled".to_string(),
            SemanticIndexStatus::Building { .. } => "building".to_string(),
            SemanticIndexStatus::Failed(_) => "failed".to_string(),
            SemanticIndexStatus::Partial { completeness, .. } => {
                warnings.push(SearchWarning::PartialIndex {
                    completeness: *completeness,
                });
                "partial".to_string()
            }
            SemanticIndexStatus::Ready => "ready".to_string(),
        }
    };

    // Warn if the distance metric changed since the index was built.
    // Metric changes affect scoring/ranking but do not trigger re-embedding.
    let config_metric = ctx
        .config()
        .semantic
        .distance_metric
        .as_ref()
        .map(|m| {
            serde_json::to_value(m)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "cosine".to_string())
        })
        .unwrap_or_else(|| "cosine".to_string());
    if let Some(idx) = ctx.semantic_index().borrow().as_ref() {
        if let Some(fp) = idx.fingerprint() {
            if !fp.distance_metric.is_empty() && fp.distance_metric != config_metric {
                warnings.push(SearchWarning::DistanceMetricChanged {
                    previous: fp.distance_metric.clone(),
                    current: config_metric,
                });
            }
        }
    }

    match &*ctx.semantic_index_status().borrow() {
        SemanticIndexStatus::Disabled => {
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "disabled",
                    "text": "Semantic search is not enabled.",
                }),
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
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "building",
                    "text": detail,
                    "stage": stage,
                    "files": files,
                    "entries_done": entries_done,
                    "entries_total": entries_total,
                }),
            );
        }
        SemanticIndexStatus::Failed(error) => {
            return semantic_error_response(&req.id, error);
        }
        SemanticIndexStatus::Partial {
            stage: _,
            entries_done,
            entries_total,
            completeness,
        } => {
            // Index is usable but still building — allow search but flag results
            // as potentially incomplete. Fall through to normal search below.
            let pct = (*completeness * 100.0) as usize;
            slog_info!(
                "semantic search: index partially built ({}%, {}/{})",
                pct,
                entries_done,
                entries_total
            );
        }
        SemanticIndexStatus::Ready => {}
    }

    let embedding_timer = PhaseTimer::start();
    let (query_vector, query_cache_hit) = match embed_query(&params.query, ctx) {
        Ok(result) => result,
        Err(error) => {
            if diagnostics_enabled {
                warnings.push(SearchWarning::EmbeddingFailure {
                    reason: error.clone(),
                });
            }
            return semantic_error_response(&req.id, &error);
        }
    };
    let embedding_latency_ms = embedding_timer.stop();

    let project_root = ctx
        .config()
        .project_root
        .clone()
        .unwrap_or_else(|| env::current_dir().unwrap_or_default());
    let project_root = std::fs::canonicalize(&project_root).unwrap_or(project_root);

    let vector_search_timer = PhaseTimer::start();
    let semantic_results = {
        let semantic_index = ctx.semantic_index().borrow();
        let Some(index) = semantic_index.as_ref() else {
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "not_ready",
                    "text": "Semantic index is not ready yet.",
                }),
            );
        };
        index.search(&query_vector, params.top_k.clamp(50, MAX_TOP_K))
    };
    let vector_search_latency_ms = vector_search_timer.stop();

    let lexical_timer = PhaseTimer::start();
    let shape = query_shape::classify(&params.query);
    let lexical_files = if shape.weights.should_use_lexical {
        let tokens = query_shape::extract_tokens(&params.query, &shape);
        let token_refs = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        let query_trigrams = SearchIndex::query_trigrams_from_tokens(&token_refs);
        ctx.search_index()
            .borrow()
            .as_ref()
            .filter(|index| index.ready)
            .map(|index| {
                index.lexical_rank(&query_trigrams, Some(&is_semantic_indexed_extension), 50)
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let lexical_latency_ms = lexical_timer.stop();

    // Determine pipeline type.
    let has_semantic = !semantic_results.is_empty();
    let has_lexical = !lexical_files.is_empty();
    let pipeline_type = match (has_semantic, has_lexical) {
        (true, true) => SearchPipelineType::Hybrid,
        (true, false) => SearchPipelineType::Semantic,
        (false, true) => {
            warnings.push(SearchWarning::EmptyResults);
            SearchPipelineType::LexicalFallback
        }
        (false, false) => {
            warnings.push(SearchWarning::EmptyResults);
            SearchPipelineType::Semantic
        }
    };

    let max_results_per_file = ctx.config().semantic.max_results_per_file;
    let fusion_timer = PhaseTimer::start();
    let results = fuse_hybrid_results(
        semantic_results,
        lexical_files,
        &shape,
        params.top_k.min(MAX_TOP_K),
        max_results_per_file,
    );
    let hybrid_fusion_latency_ms = fusion_timer.stop();

    // Reranking pipeline (optional, config-dependent).
    let rerank_timer = PhaseTimer::start();
    let rerank_latency_ms;
    let (reranked, _rerank_failed) =
        match rerank_candidates(&ctx.config().semantic, &params.query, &results) {
            RerankOutcome::ReRanked(indices) => {
                rerank_latency_ms = rerank_timer.stop();
                // Apply reranked order, then append any missing indices in original order.
                let n = results.len();
                let mut used = vec![false; n];
                let oob_count = indices.iter().filter(|&&i| i >= n).count();
                if oob_count > 0 && diagnostics_enabled {
                    warnings.push(SearchWarning::RerankerFailure {
                        reason: format!(
                            "reranker returned {} out-of-bounds indices (max {})",
                            oob_count, n
                        ),
                    });
                }
                let mut reranked: Vec<HybridResult> = indices
                    .iter()
                    .filter_map(|&i| {
                        if i < n && !used[i] {
                            used[i] = true;
                            Some(results[i].clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                // Append missing IDs in original order.
                for (i, result) in results.iter().enumerate() {
                    if !used[i] {
                        reranked.push(result.clone());
                    }
                }
                (reranked, false)
            }
            RerankOutcome::Skipped => {
                rerank_latency_ms = rerank_timer.stop();
                (results.clone(), false)
            }
            RerankOutcome::Failed(e) => {
                rerank_latency_ms = rerank_timer.stop();
                if diagnostics_enabled {
                    warnings.push(SearchWarning::RerankerFailure { reason: e });
                }
                (results.clone(), true)
            }
        };

    // If all results have low scores, flag low confidence.
    let scores: Vec<f32> = reranked.iter().map(|r| r.score).collect();
    let low_conf_threshold = ctx.config().semantic.low_confidence_threshold;
    if !scores.is_empty() && scores.iter().all(|s| *s < low_conf_threshold) {
        warnings.push(SearchWarning::LowConfidence);
    }

    // No score threshold: silent filtering produced "0 results" even when the
    // model had reasonable matches the agent could have judged. Surface every
    // hit with its score so the caller can decide.

    // NOTE: Do NOT overwrite the index status here. A search command must not
    // change the lifecycle state — that is the build/refresh pipeline's job.

    // Compute query statistics (always needed for output mode and diagnostics).
    let candidate_count = scores.len();
    let returned_count = reranked.len();
    let score_stats = score_statistics(&scores);
    let margin = top1_margin(&scores);
    let total_latency_ms = _pipeline_timer.stop();
    let prompt_active = ctx.config().semantic.query_prompt_template.is_some();

    // Format diagnostics prefix for tool output.
    // Deduplicate warnings for display — first occurrence visible,
    // repeated occurrences within 60s suppressed. Full warnings still
    // go to diagnostics recording below.
    let output_mode = ctx.config().semantic.output_mode;
    let deduped_warnings = ctx
        .semantic_warning_dedup()
        .borrow_mut()
        .filter_for_output(&warnings);
    let diagnostics_prefix = format_diagnostics_prefix(
        output_mode,
        &deduped_warnings,
        pipeline_type,
        total_latency_ms,
        Some(score_stats),
        candidate_count,
        returned_count,
        Some(embedding_latency_ms),
        Some(vector_search_latency_ms),
        Some(lexical_latency_ms),
        Some(hybrid_fusion_latency_ms),
        Some(rerank_latency_ms),
    );

    // Build tool output text.
    let base_text = format_semantic_text(&reranked, &project_root);
    let text = match &diagnostics_prefix {
        Some(prefix) => format!("{}\n\n{}", prefix, base_text),
        None => base_text,
    };

    // Record diagnostics if enabled (metrics + JSONL, independent of output_mode).
    if diagnostics_enabled {
        // Lazily init JSONL logger.
        ctx.init_diagnostics_logger();

        let (score_min, score_median, score_p90, score_max) = score_stats;
        let diag = SearchDiagnostics {
            query_hash,
            pipeline_type,
            index_state,
            total_latency_ms,
            embedding_latency_ms: Some(embedding_latency_ms),
            lexical_latency_ms: Some(lexical_latency_ms),
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
            warnings: warnings.clone(),
        };
        ctx.semantic_search_metrics()
            .borrow_mut()
            .record(diag.clone());

        // Write to JSONL if logger is active.
        if let Some(logger) = ctx.semantic_diagnostics_logger().borrow_mut().as_mut() {
            logger.record(&diag, Some(&params.query), None);
        }
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "status": "ready",
            "text": text,
            "results": reranked.iter().map(result_to_json).collect::<Vec<_>>(),
        }),
    )
}

fn default_top_k() -> usize {
    DEFAULT_TOP_K
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
        .embed_query_cached(query, semantic_config.query_prompt_template.as_deref())
        .map_err(|error| format!("failed to embed query: {error}"))?;
    if let Some(index) = ctx.semantic_index().borrow().as_ref() {
        if index.dimension() != query_vector.len() {
            return Err(format!(
                "semantic embedding dimension mismatch: query backend returned {}, index expects {}. Rebuild the semantic index for the active backend/model.",
                query_vector.len(),
                index.dimension()
            ));
        }
    }

    Ok((query_vector, query_cache_hit))
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

    let mut results: Vec<HybridResult> = if lexical_files.is_empty() {
        semantic
            .into_iter()
            .map(|result| hybrid_from_semantic(result, "semantic", None))
            .collect()
    } else if semantic.is_empty() {
        lexical_files
            .into_iter()
            .map(|(file, score)| lexical_only_result(file, score, shape))
            .collect()
    } else {
        let lexical_top_files: HashMap<PathBuf, f32> =
            lexical_files.iter().take(20).cloned().collect();
        let mut hybrid: Vec<HybridResult> = semantic
            .into_iter()
            .map(|result| {
                if let Some(&lexical_score) = lexical_top_files.get(&result.file) {
                    hybrid_from_semantic(result, "hybrid", Some(lexical_score))
                } else {
                    hybrid_from_semantic(result, "semantic", None)
                }
            })
            .collect();

        let semantic_files: HashSet<PathBuf> =
            hybrid.iter().map(|result| result.file.clone()).collect();
        for (file, score) in lexical_files.iter().take(20) {
            if !semantic_files.contains(file) {
                hybrid.push(lexical_only_result(file.clone(), *score, shape));
            }
        }
        hybrid
    };

    sort_cap_and_truncate(&mut results, top_k, max_results_per_file);
    results
}

/// Sort by score descending, apply per-file cap, re-sort, then truncate.
fn sort_cap_and_truncate(
    results: &mut Vec<HybridResult>,
    top_k: usize,
    max_results_per_file: usize,
) {
    let cmp = |a: &HybridResult, b: &HybridResult| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    };
    results.sort_by(&cmp);
    let capped = cap_per_file(std::mem::take(results), max_results_per_file);
    *results = capped;
    results.sort_by(&cmp);
    results.truncate(top_k);
}

fn hybrid_from_semantic(
    result: SemanticResult,
    source: &'static str,
    lexical_score: Option<f32>,
) -> HybridResult {
    let semantic_score = result.score;
    let score = if source == "hybrid" {
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
        source,
        semantic_score: Some(semantic_score),
        lexical_score,
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
        snippet: "[lexical match — use aft_zoom or read for context]".to_string(),
    }
}

fn shape_dependent_lexical_only_weight(shape: &QueryShape) -> f32 {
    match shape.kind {
        QueryKind::Identifier => 0.8,
        QueryKind::Path | QueryKind::ErrorCode | QueryKind::Mixed => 0.5,
        QueryKind::NaturalLanguage => 0.0,
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

fn format_semantic_text(results: &[HybridResult], project_root: &Path) -> String {
    if results.is_empty() {
        return "Found 0 semantic result(s). [index: ready]".to_string();
    }

    let mut groups: BTreeMap<String, Vec<&HybridResult>> = BTreeMap::new();

    for result in results {
        let display_path = result
            .file
            .strip_prefix(project_root)
            .unwrap_or(&result.file)
            .display()
            .to_string();
        groups.entry(display_path).or_default().push(result);
    }

    let sections = groups
        .into_iter()
        .map(|(file, file_results)| {
            let mut section = file;

            for result in file_results {
                if result.source == "lexical" {
                    section.push_str(&format!(" [lexical match — score: {:.3}]", result.score));
                } else if matches!(result.kind, SymbolKind::FileSummary) {
                    section.push_str(&format!(
                        "\n{} [{}] [file summary] score {:.3} source {}",
                        result.name,
                        symbol_kind_label(&result.kind),
                        result.score,
                        result.source
                    ));
                } else {
                    section.push_str(&format!(
                        "\n{} [{}] lines {}-{} score {:.3} source {}",
                        result.name,
                        symbol_kind_label(&result.kind),
                        display_line_number(result.start_line),
                        display_line_number(result.end_line),
                        result.score,
                        result.source
                    ));
                }

                if !result.snippet.trim().is_empty() {
                    for line in result.snippet.lines() {
                        section.push_str("\n    ");
                        section.push_str(line);
                    }
                }
            }

            section
        })
        .collect::<Vec<_>>();

    format!(
        "{}\n\nFound {} semantic result(s). [index: ready]",
        sections.join("\n\n"),
        results.len()
    )
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

    serde_json::json!({
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
        "snippet": result.snippet,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        }];

        let text = format_semantic_text(&results, project_root);

        assert!(text.contains("index [file-summary] [file summary] score 0.750 source semantic"));
        assert!(!text.contains("lines 1-1"));
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

    // ── fuse_hybrid_results tests ──────────────────────────────────────

    fn make_semantic_result(
        file: &str,
        name: &str,
        score: f32,
    ) -> crate::semantic_index::SemanticResult {
        crate::semantic_index::SemanticResult {
            file: PathBuf::from(file),
            name: name.to_string(),
            kind: SymbolKind::Function,
            start_line: 0,
            end_line: 10,
            exported: false,
            snippet: String::new(),
            score,
            source: "semantic",
        }
    }

    fn make_nl_shape() -> crate::query_shape::QueryShape {
        crate::query_shape::classify("how to handle authentication in the middleware layer")
    }

    fn make_id_shape() -> crate::query_shape::QueryShape {
        crate::query_shape::classify("handleAuthRequest")
    }

    fn make_path_shape() -> crate::query_shape::QueryShape {
        crate::query_shape::classify("src/auth/session.ts")
    }

    #[test]
    fn fuse_empty_semantic_and_empty_lexical_returns_empty() {
        let shape = make_nl_shape();
        let results = fuse_hybrid_results(vec![], vec![], &shape, 10, 2);
        assert!(results.is_empty());
    }

    #[test]
    fn fuse_semantic_only_returns_semantic_results() {
        let shape = make_nl_shape();
        let semantic = vec![make_semantic_result("a.rs", "func_a", 0.9)];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 10, 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "semantic");
        assert_eq!(results[0].name, "func_a");
    }

    #[test]
    fn fuse_lexical_only_returns_lexical_results() {
        let shape = make_id_shape();
        let lexical = vec![(PathBuf::from("b.rs"), 0.8)];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "lexical");
    }

    #[test]
    fn fuse_hybrid_marks_files_in_both_as_hybrid() {
        let shape = make_nl_shape();
        let semantic = vec![make_semantic_result("a.rs", "func_a", 0.9)];
        let lexical = vec![(PathBuf::from("a.rs"), 0.5)];
        let results = fuse_hybrid_results(semantic, lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, "hybrid");
        assert!(results[0].lexical_score.is_some());
    }

    #[test]
    fn fuse_hybrid_boost_applied() {
        let shape = make_nl_shape();
        let semantic = vec![make_semantic_result("a.rs", "func_a", 0.8)];
        let lexical = vec![(PathBuf::from("a.rs"), 0.5)];
        let results = fuse_hybrid_results(semantic, lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        // Score should be semantic * HYBRID_LEXICAL_BOOST = 0.8 * 1.1 = 0.88
        assert!((results[0].score - 0.88).abs() < 1e-5);
    }

    #[test]
    fn fuse_lexical_only_score_capped() {
        let shape = make_id_shape();
        // A very high lexical score should be capped at LEXICAL_ONLY_SCORE_CEILING (0.25).
        let lexical = vec![(PathBuf::from("a.rs"), 10.0)];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        assert!(results[0].score <= 0.25 + 1e-6);
    }

    #[test]
    fn fuse_natural_language_lexical_only_weight_zero() {
        let shape = make_nl_shape();
        let lexical = vec![(PathBuf::from("a.rs"), 0.9)];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        // NL queries weight lexical at 0.0, so score = 0.9 * 0.0 = 0.0.
        assert!(results[0].score.abs() < 1e-6);
    }

    #[test]
    fn fuse_top_k_truncates_results() {
        let shape = make_nl_shape();
        let semantic = vec![
            make_semantic_result("a.rs", "f1", 0.9),
            make_semantic_result("b.rs", "f2", 0.8),
            make_semantic_result("c.rs", "f3", 0.7),
        ];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 2, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "f1");
        assert_eq!(results[1].name, "f2");
    }

    #[test]
    fn fuse_zero_top_k_returns_empty() {
        let shape = make_nl_shape();
        let semantic = vec![make_semantic_result("a.rs", "f1", 0.9)];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 0, 2);
        assert!(results.is_empty());
    }

    #[test]
    fn fuse_cap_per_file_limits_results() {
        let shape = make_nl_shape();
        // 3 results from the same file — cap_per_file(2) should keep only 2.
        let semantic = vec![
            make_semantic_result("a.rs", "f1", 0.9),
            make_semantic_result("a.rs", "f2", 0.8),
            make_semantic_result("a.rs", "f3", 0.7),
        ];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 10, 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn fuse_results_sorted_by_score_desc() {
        let shape = make_nl_shape();
        let semantic = vec![
            make_semantic_result("a.rs", "f_low", 0.3),
            make_semantic_result("b.rs", "f_high", 0.9),
            make_semantic_result("c.rs", "f_mid", 0.6),
        ];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 10, 2);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].name, "f_high");
        assert_eq!(results[1].name, "f_mid");
        assert_eq!(results[2].name, "f_low");
    }

    #[test]
    fn fuse_lexical_only_path_shape_weight() {
        let shape = make_path_shape();
        let lexical = vec![(PathBuf::from("a.rs"), 0.9)];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert_eq!(results.len(), 1);
        // Path shape weights lexical at 0.5, capped at 0.25.
        assert!(results[0].score <= 0.25 + 1e-6);
    }

    #[test]
    fn fuse_lexical_only_sorted_by_score_desc() {
        let shape = make_nl_shape();
        let lexical = vec![
            (PathBuf::from("c.rs"), 0.3),
            (PathBuf::from("a.rs"), 0.9),
            (PathBuf::from("b.rs"), 0.6),
        ];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert_eq!(results.len(), 3);
        // Lexical-only results should be sorted by score descending.
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
    }

    #[test]
    fn fuse_lexical_only_cap_per_file() {
        let shape = make_nl_shape();
        // 3 results from the same file — cap_per_file(2) should keep only 2.
        let lexical = vec![
            (PathBuf::from("a.rs"), 0.9),
            (PathBuf::from("a.rs"), 0.8),
            (PathBuf::from("a.rs"), 0.7),
        ];
        let results = fuse_hybrid_results(vec![], lexical, &shape, 10, 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn fuse_custom_max_results_per_file() {
        let shape = make_nl_shape();
        // 4 results from the same file — cap_per_file(3) should keep 3.
        let semantic = vec![
            make_semantic_result("a.rs", "f1", 0.9),
            make_semantic_result("a.rs", "f2", 0.8),
            make_semantic_result("a.rs", "f3", 0.7),
            make_semantic_result("a.rs", "f4", 0.6),
        ];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 10, 3);
        assert_eq!(results.len(), 3);
        let expected_file = std::path::Path::new("a.rs");
        assert!(results.iter().all(|r| r.file == expected_file));
    }

    #[test]
    fn fuse_max_results_per_file_one_allows_many_files() {
        let shape = make_nl_shape();
        // One result per file — cap_per_file(1) should keep all since each file has only 1.
        let semantic = vec![
            make_semantic_result("a.rs", "f1", 0.9),
            make_semantic_result("b.rs", "f2", 0.8),
            make_semantic_result("c.rs", "f3", 0.7),
        ];
        let results = fuse_hybrid_results(semantic, vec![], &shape, 10, 1);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn result_to_json_semantic_result() {
        let result = HybridResult {
            file: PathBuf::from("/project/src/lib.ts"),
            name: "processData".to_string(),
            kind: SymbolKind::Function,
            start_line: 14,
            end_line: 42,
            exported: true,
            snippet: "function processData() {}".to_string(),
            score: 0.85,
            source: "hybrid",
            semantic_score: Some(0.8),
            lexical_score: Some(0.5),
        };
        let json = result_to_json(&result);
        assert_eq!(json["name"], "processData");
        assert_eq!(json["source"], "hybrid");
        assert_eq!(json["start_line"], 15); // 1-indexed
        assert_eq!(json["end_line"], 43); // 1-indexed
                                          // f32→JSON promotes to f64, exposing IEEE 754 precision artifacts
                                          // (e.g. 0.8f32 → 0.800000011920929). Use approximate comparison.
        assert!((json["semantic_score"].as_f64().unwrap() - 0.8).abs() < 1e-6);
        assert!((json["lexical_score"].as_f64().unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(json["snippet"], "function processData() {}");
    }

    #[test]
    fn result_to_json_lexical_result() {
        let result = HybridResult {
            file: PathBuf::from("/project/src/config.ts"),
            name: String::new(),
            kind: SymbolKind::FileSummary,
            start_line: 0,
            end_line: 0,
            exported: false,
            snippet: String::new(),
            score: 0.25,
            source: "lexical",
            semantic_score: None,
            lexical_score: Some(1.5),
        };
        let json = result_to_json(&result);
        assert_eq!(json["location"], "[lexical match]");
        assert!(json["semantic_score"].is_null());
        assert_eq!(json["lexical_score"], 1.5);
    }

    #[test]
    fn format_semantic_text_empty_results() {
        let text = format_semantic_text(&[], Path::new("/project"));
        assert!(text.contains("0 semantic result(s)"));
    }

    #[test]
    fn format_semantic_text_groups_by_file() {
        let results = vec![
            HybridResult {
                file: PathBuf::from("/project/src/a.ts"),
                name: "fn1".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 10,
                exported: false,
                snippet: String::new(),
                score: 0.9,
                source: "semantic",
                semantic_score: Some(0.9),
                lexical_score: None,
            },
            HybridResult {
                file: PathBuf::from("/project/src/a.ts"),
                name: "fn2".to_string(),
                kind: SymbolKind::Function,
                start_line: 15,
                end_line: 25,
                exported: false,
                snippet: String::new(),
                score: 0.7,
                source: "semantic",
                semantic_score: Some(0.7),
                lexical_score: None,
            },
        ];
        let text = format_semantic_text(&results, Path::new("/project"));
        // Both should appear under the same file heading.
        assert!(text.contains("fn1"));
        assert!(text.contains("fn2"));
        assert!(text.contains("2 semantic result(s)"));
    }

    #[test]
    fn warning_dedup_key_stability_across_warning_kinds() {
        // Verify that the dedup key function produces stable, distinct keys
        // for each warning kind.
        let warnings = vec![
            SearchWarning::LowConfidence,
            SearchWarning::EmptyResults,
            SearchWarning::PartialIndex { completeness: 0.5 },
            SearchWarning::PartialIndex { completeness: 0.8 },
            SearchWarning::StaleIndex,
            SearchWarning::DegradedIndex,
            SearchWarning::EmbeddingFailure {
                reason: "timeout".into(),
            },
            SearchWarning::EmbeddingFailure {
                reason: "network".into(),
            },
            SearchWarning::LexicalFailure {
                reason: "skip".into(),
            },
            SearchWarning::DimensionMismatch {
                expected: 768,
                got: 384,
            },
            SearchWarning::RerankerFailure {
                reason: "parse_error".into(),
            },
            SearchWarning::DistanceMetricChanged {
                previous: "cosine".into(),
                current: "dot_product".into(),
            },
        ];
        // Each warning *kind* produces a unique dedup key. PartialIndex
        // variants with different completeness values intentionally share
        // the same key (completeness is excluded from dedup), so 12
        // entries → 11 unique keys.
        let mut keys: Vec<String> = warnings
            .iter()
            .map(crate::semantic_diagnostics::warning_dedup_key)
            .collect();
        keys.sort();
        keys.dedup();
        assert_eq!(
            keys.len(),
            11,
            "12 warnings should produce 11 unique dedup keys (PartialIndex shares one)"
        );
    }

    #[test]
    fn sort_cap_and_truncate_respects_max_results_per_file() {
        let results = vec![
            HybridResult {
                file: PathBuf::from("a.rs"),
                name: "f1".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 2,
                exported: true,
                snippet: String::new(),
                score: 0.9,
                source: "semantic",
                semantic_score: Some(0.9),
                lexical_score: None,
            },
            HybridResult {
                file: PathBuf::from("a.rs"),
                name: "f2".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 2,
                exported: true,
                snippet: String::new(),
                score: 0.8,
                source: "semantic",
                semantic_score: Some(0.8),
                lexical_score: None,
            },
            HybridResult {
                file: PathBuf::from("a.rs"),
                name: "f3".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 2,
                exported: true,
                snippet: String::new(),
                score: 0.7,
                source: "semantic",
                semantic_score: Some(0.7),
                lexical_score: None,
            },
            HybridResult {
                file: PathBuf::from("b.rs"),
                name: "g1".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 2,
                exported: true,
                snippet: String::new(),
                score: 0.6,
                source: "semantic",
                semantic_score: Some(0.6),
                lexical_score: None,
            },
        ];
        let mut results = results;
        sort_cap_and_truncate(&mut results, 10, 2); // cap_per_file=2
                                                    // a.rs should have at most 2 results, b.rs should have 1.
        let expected_a = std::path::Path::new("a.rs");
        let expected_b = std::path::Path::new("b.rs");
        let a_count = results.iter().filter(|r| r.file == expected_a).count();
        let b_count = results.iter().filter(|r| r.file == expected_b).count();
        assert!(
            a_count <= 2,
            "a.rs should have at most 2 results, got {a_count}"
        );
        assert_eq!(b_count, 1, "b.rs should have 1 result");
    }
}
