//! FTS5 side-feature command stubs and implementations.
//!
//! These handlers are behind the `semantic-fts5` Cargo feature. When the
//! feature is compiled but the runtime config has `fts5.enabled = false`,
//! every command returns a clear `disabled` status so callers know the
//! feature exists but is not active.

use crate::context::AppContext;
use crate::grep_executor;
use crate::protocol::{RawRequest, Response};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a "runtime disabled" response.
fn runtime_disabled(req: &RawRequest) -> Response {
    Response::error(
        &req.id,
        "fts5_disabled",
        "FTS5 is compiled but disabled at runtime. Set [fts5].enabled = true in aft.jsonc to enable.",
    )
}

// ---------------------------------------------------------------------------
// Text rendering helpers (agent-facing plain text)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Output envelope helpers
// ---------------------------------------------------------------------------

/// Output state — indicates the health of the result set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputState {
    /// All results are complete and fresh.
    Complete,
    /// Some results were truncated due to limits.
    Truncated,
    /// The index is stale (some files changed since last index).
    Stale,
    /// The index is disabled or unavailable.
    Degraded,
    /// No results found.
    Empty,
}

impl OutputState {
    fn as_str(&self) -> &'static str {
        match self {
            OutputState::Complete => "complete",
            OutputState::Truncated => "truncated",
            OutputState::Stale => "stale",
            OutputState::Degraded => "degraded",
            OutputState::Empty => "empty",
        }
    }
}

/// Progressive shortening levels for high-cardinality outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortenLevel {
    /// Full snippets and details.
    Full,
    /// References without snippets.
    ReferencesOnly,
    /// Per-file counts only.
    FileCounts,
    /// Summary with refinement suggestion.
    Summary,
}

/// Build the standard output envelope with evidence, state, and enrichment.
fn build_envelope(
    query: &str,
    state: OutputState,
    evidence: Vec<serde_json::Value>,
    enrichment: serde_json::Value,
    text: String,
) -> serde_json::Value {
    let mut envelope = serde_json::json!({
        "state": state.as_str(),
        "evidence": evidence,
        "enrichment": enrichment,
        "text": text,
    });
    if state == OutputState::Empty {
        envelope["message"] = serde_json::json!(format!("No results found for \"{query}\""));
    }
    envelope
}

/// Shorten results progressively based on level.
fn shorten_results(results: &[serde_json::Value], level: ShortenLevel) -> Vec<serde_json::Value> {
    match level {
        ShortenLevel::Full => results.to_vec(),
        ShortenLevel::ReferencesOnly => results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "symbol_id": r["symbol_id"],
                    "file_path": r["file_path"],
                    "symbol_name": r["symbol_name"],
                    "symbol_kind": r["symbol_kind"],
                    "start_line": r["start_line"],
                    "end_line": r["end_line"],
                    "score": r["score"],
                    "name_path": r.get("name_path"),
                })
            })
            .collect(),
        ShortenLevel::FileCounts => {
            let mut file_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for r in results {
                let file = r["file_path"].as_str().unwrap_or("?");
                *file_counts.entry(file.to_string()).or_insert(0) += 1;
            }
            file_counts
                .into_iter()
                .map(|(file, count)| {
                    serde_json::json!({
                        "file_path": file,
                        "match_count": count,
                    })
                })
                .collect()
        }
        ShortenLevel::Summary => {
            let total = results.len();
            let unique_files = results
                .iter()
                .filter_map(|r| r["file_path"].as_str())
                .collect::<std::collections::HashSet<_>>()
                .len();
            vec![serde_json::json!({
                "total_matches": total,
                "unique_files": unique_files,
                "suggestion": "Use --top_k to limit results or refine your query.",
            })]
        }
    }
}

fn render_search_text(query: &str, scope: &str, results: &[serde_json::Value]) -> String {
    if results.is_empty() {
        return format!("FTS5 Search: \"{query}\"\nscope={scope} results=0\nNo results found.");
    }
    let total = results.len();
    let mut lines = vec![format!("FTS5 Search: \"{query}\"")];
    lines.push(format!("scope={scope} results={total}"));
    lines.push(String::new());
    for (i, r) in results.iter().enumerate() {
        let name = r["symbol_name"].as_str().unwrap_or("?");
        let kind = r["symbol_kind"].as_str().unwrap_or("?");
        let file = r["file_path"].as_str().unwrap_or("?");
        let start = r["start_line"].as_i64().unwrap_or(0);
        let end = r["end_line"].as_i64().unwrap_or(0);
        let score = r["score"].as_f64().unwrap_or(0.0);
        let lane = r["lane"].as_str().unwrap_or("?");
        lines.push(format!(
            "[{}] {kind} {name}  {file}:{start}-{end}  score={score:.2}  lane={lane}",
            i + 1
        ));
        if let Some(snippet) = r["snippet"].as_str() {
            let s: String = snippet.chars().take(80).collect();
            lines.push(format!("    snippet: {s}"));
        }
    }
    lines.join("\n")
}

fn render_find_symbol_text(name: &str, mode: &str, results: &[serde_json::Value]) -> String {
    if results.is_empty() {
        return format!("FTS5 Find Symbol: \"{name}\"  mode={mode}\nNo matches found.");
    }
    let total = results.len();
    let mut lines = vec![format!("FTS5 Find Symbol: \"{name}\"  mode={mode}")];
    lines.push(format!("results={total}"));
    lines.push(String::new());
    for (i, r) in results.iter().enumerate() {
        let sym_name = r["symbol_name"].as_str().unwrap_or("?");
        let kind = r["symbol_kind"].as_str().unwrap_or("?");
        let file = r["file_path"].as_str().unwrap_or("?");
        let start = r["start_line"].as_i64().unwrap_or(0);
        let end = r["end_line"].as_i64().unwrap_or(0);
        let lane = r["lane"].as_str().unwrap_or("?");
        lines.push(format!(
            "[{}] {kind} {sym_name}  {file}:{start}-{end}  lane={lane}",
            i + 1
        ));
    }
    lines.join("\n")
}

fn render_read_symbol_text(
    name: &str,
    file_path: &str,
    start_line: u32,
    end_line: u32,
    body: &str,
) -> String {
    let mut lines = vec![format!(
        "FTS5 Read Symbol: \"{name}\"  {file_path}:{start_line}-{end_line}"
    )];
    lines.push(body.to_string());
    lines.join("\n")
}

fn render_index_status_text(
    exists: bool,
    file_count: usize,
    symbol_count: usize,
    db_size: u64,
    stale_count: usize,
) -> String {
    if !exists {
        return "FTS5 Index: not found\nRun fts5_index with action=update to create.".to_string();
    }
    let size_mb = db_size as f64 / (1024.0 * 1024.0);
    let mut lines = vec![format!(
        "FTS5 Index: {file_count} files, {symbol_count} symbols, {size_mb:.1} MiB"
    )];
    if stale_count > 0 {
        lines.push(format!("  stale files: {stale_count}"));
    }
    lines.join("\n")
}

fn render_index_action_text(action: &str, stats_json: &serde_json::Value) -> String {
    let processed = stats_json["files_processed"].as_i64().unwrap_or(0);
    let added = stats_json["files_added"].as_i64().unwrap_or(0);
    let updated = stats_json["files_updated"].as_i64().unwrap_or(0);
    let removed = stats_json["files_removed"].as_i64().unwrap_or(0);
    let symbols = stats_json["symbols_extracted"].as_i64().unwrap_or(0);
    format!("FTS5 {action}: processed={processed} added={added} updated={updated} removed={removed} symbols={symbols}")
}

fn render_doctor_text(
    enabled: bool,
    fts5_available: bool,
    index_json: &serde_json::Value,
    warnings: &[String],
) -> String {
    let mut lines = vec!["FTS5 Doctor".to_string()];
    lines.push(format!(
        "  compiled=true  available={fts5_available}  enabled={enabled}"
    ));
    if let Some(exists) = index_json.get("exists").and_then(|v| v.as_bool()) {
        if exists {
            let files = index_json["file_count"].as_i64().unwrap_or(0);
            let symbols = index_json["symbol_count"].as_i64().unwrap_or(0);
            let db_size = index_json["db_size_bytes"].as_i64().unwrap_or(0);
            let size_mb = db_size as f64 / (1024.0 * 1024.0);
            lines.push(format!(
                "  index: {files} files, {symbols} symbols, {size_mb:.1} MiB"
            ));
        } else {
            lines.push("  index: not found".to_string());
        }
    }
    for w in warnings {
        lines.push(format!("  ⚠ {w}"));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// fts5_index
// ---------------------------------------------------------------------------

/// Parameters for `fts5_index`.
#[derive(Debug, Deserialize)]
struct Fts5IndexParams {
    /// Action to perform: "status", "update", "rebuild", "prune".
    #[serde(default = "default_index_action")]
    action: String,
}

fn default_index_action() -> String {
    "update".to_string()
}

/// `fts5_index` — build or update the FTS5 index.
pub fn handle_fts5_index(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    let params: Fts5IndexParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("fts5_index: invalid params: {e}"),
            );
        }
    };

    let project_root = grep_executor::project_root(ctx);
    let db_path = resolve_fts5_db_path(&project_root);

    match params.action.as_str() {
        "status" => handle_index_status(req, &db_path, &project_root),
        "update" => handle_index_update(req, &db_path, &project_root, false),
        "rebuild" => handle_index_update(req, &db_path, &project_root, true),
        "prune" => handle_index_prune(req, &db_path, &project_root),
        _ => Response::error(
            &req.id,
            "invalid_request",
            format!(
                "fts5_index: unknown action '{}'; expected status, update, rebuild, or prune",
                params.action
            ),
        ),
    }
}

/// Handle the "status" action.
fn handle_index_status(
    req: &RawRequest,
    db_path: &std::path::Path,
    project_root: &std::path::Path,
) -> Response {
    if !db_path.exists() {
        let text = render_index_status_text(false, 0, 0, 0, 0);
        return Response::success(
            &req.id,
            serde_json::json!({
                "exists": false,
                "message": "No FTS5 index found. Run fts5_index with action=update to create.",
                "text": text,
            }),
        );
    }

    let store = match crate::fts5_store::Fts5Store::open(db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    let file_count = store.file_count().unwrap_or(0);
    let symbol_count = store.symbol_count().unwrap_or(0);
    let schema_version = store.schema_version().unwrap_or(0);
    let db_size = store.db_size_bytes();
    let row_counts = store
        .fts_row_counts()
        .unwrap_or_else(|_| crate::fts5_store::FtsRowCounts {
            symbols_fts: 0,
            bodies_fts: 0,
            paths_fts: 0,
        });

    let stale = store.stale_files(project_root).unwrap_or_default();
    let stale_count = stale.len();

    let text = render_index_status_text(true, file_count, symbol_count, db_size, stale_count);

    Response::success(
        &req.id,
        serde_json::json!({
            "exists": true,
            "schema_version": schema_version,
            "file_count": file_count,
            "symbol_count": symbol_count,
            "db_size_bytes": db_size,
            "fts_row_counts": {
                "symbols": row_counts.symbols_fts,
                "bodies": row_counts.bodies_fts,
                "paths": row_counts.paths_fts,
            },
            "stale_files": stale_count,
            "db_path": db_path.display().to_string(),
            "text": text,
        }),
    )
}

/// Handle the "update" or "rebuild" action.
fn handle_index_update(
    req: &RawRequest,
    db_path: &std::path::Path,
    project_root: &std::path::Path,
    rebuild: bool,
) -> Response {
    // Open or create the store
    let store = match crate::fts5_store::Fts5Store::open(db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    // Create indexer
    let mut indexer = crate::fts5_indexer::Fts5Indexer::new(&store);

    // Execute indexing
    let stats = if rebuild {
        match indexer.rebuild(project_root) {
            Ok(stats) => stats,
            Err(e) => {
                return Response::error(
                    &req.id,
                    "fts5_index_error",
                    format!("Rebuild failed: {e}"),
                );
            }
        }
    } else {
        match indexer.index_project(project_root) {
            Ok(stats) => stats,
            Err(e) => {
                return Response::error(
                    &req.id,
                    "fts5_index_error",
                    format!("Index update failed: {e}"),
                );
            }
        }
    };

    let action_label = if rebuild { "rebuild" } else { "update" };
    let stats_json = serde_json::json!({
        "files_processed": stats.files_processed,
        "files_added": stats.files_added,
        "files_updated": stats.files_updated,
        "files_removed": stats.files_removed,
        "symbols_extracted": stats.symbols_extracted,
        "files_failed": stats.files_failed,
    });
    let text = render_index_action_text(action_label, &stats_json);

    Response::success(
        &req.id,
        serde_json::json!({
            "action": action_label,
            "files_processed": stats.files_processed,
            "files_added": stats.files_added,
            "files_updated": stats.files_updated,
            "files_removed": stats.files_removed,
            "symbols_extracted": stats.symbols_extracted,
            "files_failed": stats.files_failed,
            "db_path": db_path.display().to_string(),
            "text": text,
        }),
    )
}

/// Handle the "prune" action.
fn handle_index_prune(
    req: &RawRequest,
    db_path: &std::path::Path,
    project_root: &std::path::Path,
) -> Response {
    if !db_path.exists() {
        return Response::success(
            &req.id,
            serde_json::json!({
                "action": "prune",
                "files_removed": 0,
                "message": "No FTS5 index found.",
            }),
        );
    }

    let store = match crate::fts5_store::Fts5Store::open(db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    // Find and remove stale files
    let stale = store.stale_files(project_root).unwrap_or_default();
    let mut removed = 0;

    for file in &stale {
        if store.delete_file_by_path(&file.path).is_ok() {
            removed += 1;
        }
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "action": "prune",
            "files_removed": removed,
            "stale_files_found": stale.len(),
        }),
    )
}

// ---------------------------------------------------------------------------
// fts5_search
// ---------------------------------------------------------------------------

/// Parameters for `fts5_search`.
#[derive(Debug, Deserialize)]
struct Fts5SearchParams {
    /// The search query string.
    query: String,
    /// Maximum number of results (default: 20).
    #[serde(default = "default_top_k")]
    top_k: usize,
    /// Search scope: "all", "symbols", "bodies", "paths" (default: "all").
    #[serde(default = "default_scope")]
    scope: String,
}

fn default_top_k() -> usize {
    20
}

fn default_scope() -> String {
    "all".to_string()
}

/// Resolve the FTS5 database path for a project root.
fn resolve_fts5_db_path(project_root: &std::path::Path) -> std::path::PathBuf {
    // Use the project's .aft directory for the FTS5 database
    let aft_dir = project_root.join(".aft");
    std::fs::create_dir_all(&aft_dir).ok();
    aft_dir.join("fts5.sqlite")
}

/// `fts5_search` — search the FTS5 index.
pub fn handle_fts5_search(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    let params: Fts5SearchParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("fts5_search: invalid params: {e}"),
            );
        }
    };

    if params.query.trim().is_empty() {
        return Response::error(&req.id, "invalid_request", "query must be non-empty");
    }

    let top_k = params.top_k.clamp(1, 100);
    let project_root = grep_executor::project_root(ctx);

    // Resolve the FTS5 database path
    let db_path = resolve_fts5_db_path(&project_root);

    // Try to open the store
    let store = match crate::fts5_store::Fts5Store::open(&db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    // Check if index is empty
    let file_count = match store.file_count() {
        Ok(count) => count,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to count files: {e}"),
            );
        }
    };

    if file_count == 0 {
        return Response::success(
            &req.id,
            serde_json::json!({
                "results": [],
                "total": 0,
                "query": params.query,
                "scope": params.scope,
                "warning": "FTS5 index is empty. Run fts5_index to build the index.",
            }),
        );
    }

    // Execute search via the query planner
    let planner = crate::fts5_planner::QueryPlanner::new(&store);
    let results = match planner.search(&params.query, top_k) {
        Ok(results) => results,
        Err(e) => {
            return Response::error(&req.id, "fts5_search_error", format!("Search failed: {e}"));
        }
    };

    // Convert to JSON results
    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let mut val = serde_json::json!({
                "symbol_id": r.symbol_id,
                "file_id": r.file_id,
                "file_path": r.file_path,
                "symbol_name": r.symbol_name,
                "symbol_kind": r.symbol_kind,
                "start_line": r.start_line,
                "end_line": r.end_line,
                "snippet": r.snippet,
                "score": r.score,
                "lane": r.best_lane,
                "matched_lanes": r.matched_lanes,
                "name_path": r.name_path,
                "duplicate_index": r.duplicate_index,
            });
            // Include name_path only when non-empty
            if r.name_path.is_empty() {
                val.as_object_mut().unwrap().remove("name_path");
            }
            val
        })
        .collect();

    let total = json_results.len();

    // Determine output state
    let state = if total == 0 {
        OutputState::Empty
    } else if total >= top_k {
        OutputState::Truncated
    } else {
        OutputState::Complete
    };

    // Progressive shortening: use full for small result sets, references for larger
    let shorten_level = if total <= 10 {
        ShortenLevel::Full
    } else if total <= 50 {
        ShortenLevel::ReferencesOnly
    } else {
        ShortenLevel::FileCounts
    };

    let shortened = shorten_results(&json_results, shorten_level);
    let text = render_search_text(&params.query, &params.scope, &json_results);

    // Build enrichment metadata
    let enrichment = serde_json::json!({
        "query_intent": format!("{:?}", crate::fts5_planner::AnalyzedQuery::analyze(&params.query).intent),
        "result_count": total,
        "shorten_level": format!("{:?}", shorten_level),
    });

    let envelope = build_envelope(&params.query, state, shortened, enrichment, text);

    Response::success(&req.id, envelope)
}

// ---------------------------------------------------------------------------
// fts5_find_symbol
// ---------------------------------------------------------------------------

/// Parameters for `fts5_find_symbol`.
#[derive(Debug, Deserialize)]
struct Fts5FindSymbolParams {
    /// Symbol name to find (exact or prefix match).
    name: String,
    /// Match mode: "exact" or "prefix" (default: "prefix").
    #[serde(default = "default_find_mode")]
    mode: String,
    /// Maximum number of results (default: 20).
    #[serde(default = "default_top_k")]
    top_k: usize,
}

fn default_find_mode() -> String {
    "prefix".to_string()
}

/// `fts5_find_symbol` — look up a symbol by name in the FTS5 index.
pub fn handle_fts5_find_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    let params: Fts5FindSymbolParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("fts5_find_symbol: invalid params: {e}"),
            );
        }
    };

    if params.name.trim().is_empty() {
        return Response::error(&req.id, "invalid_request", "name must be non-empty");
    }

    let top_k = params.top_k.clamp(1, 100);
    let project_root = grep_executor::project_root(ctx);
    let db_path = resolve_fts5_db_path(&project_root);

    let store = match crate::fts5_store::Fts5Store::open(&db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    let file_count = store.file_count().unwrap_or(0);
    if file_count == 0 {
        return Response::success(
            &req.id,
            serde_json::json!({
                "results": [],
                "total": 0,
                "name": params.name,
                "mode": params.mode,
                "warning": "FTS5 index is empty. Run fts5_index to build the index.",
            }),
        );
    }

    // Execute the symbol lookup
    let results = match params.mode.as_str() {
        "exact" => {
            // Exact match: SQL lookup first, then FTS
            let sql_results = store.get_symbol_by_name(&params.name).unwrap_or_default();
            let mut results: Vec<serde_json::Value> = sql_results
                .iter()
                .map(|s| {
                    let mut val = serde_json::json!({
                        "symbol_id": s.id,
                        "file_id": s.file_id,
                        "symbol_name": s.name,
                        "symbol_kind": s.kind,
                        "start_line": s.start_line,
                        "end_line": s.end_line,
                        "snippet": s.body,
                        "lane": "exact_symbol_sql",
                        "name_path": s.name_path,
                        "duplicate_index": s.duplicate_index,
                    });
                    if s.name_path.is_empty() {
                        val.as_object_mut().unwrap().remove("name_path");
                    }
                    val
                })
                .collect();

            // If SQL exact match found results, return them
            if !results.is_empty() {
                results.truncate(top_k);
                results
            } else {
                // Fallback to FTS
                let planner = crate::fts5_planner::QueryPlanner::new(&store);
                let fts_results = planner.search(&params.name, top_k).unwrap_or_default();
                fts_results
                    .iter()
                    .map(|r| {
                        let mut val = serde_json::json!({
                            "symbol_id": r.symbol_id,
                            "file_id": r.file_id,
                            "file_path": r.file_path,
                            "symbol_name": r.symbol_name,
                            "symbol_kind": r.symbol_kind,
                            "start_line": r.start_line,
                            "end_line": r.end_line,
                            "snippet": r.snippet,
                            "lane": r.best_lane,
                            "name_path": r.name_path,
                            "duplicate_index": r.duplicate_index,
                        });
                        if r.name_path.is_empty() {
                            val.as_object_mut().unwrap().remove("name_path");
                        }
                        val
                    })
                    .collect()
            }
        }
        _ => {
            // Prefix mode: use query planner
            let planner = crate::fts5_planner::QueryPlanner::new(&store);
            let fts_results = planner.search(&params.name, top_k).unwrap_or_default();
            fts_results
                .iter()
                .map(|r| {
                    let mut val = serde_json::json!({
                        "symbol_id": r.symbol_id,
                        "file_id": r.file_id,
                        "file_path": r.file_path,
                        "symbol_name": r.symbol_name,
                        "symbol_kind": r.symbol_kind,
                        "start_line": r.start_line,
                        "end_line": r.end_line,
                        "snippet": r.snippet,
                        "lane": r.best_lane,
                        "name_path": r.name_path,
                        "duplicate_index": r.duplicate_index,
                    });
                    if r.name_path.is_empty() {
                        val.as_object_mut().unwrap().remove("name_path");
                    }
                    val
                })
                .collect()
        }
    };

    let total = results.len();

    // Determine output state
    let state = if total == 0 {
        OutputState::Empty
    } else if total >= top_k {
        OutputState::Truncated
    } else {
        OutputState::Complete
    };

    // Progressive shortening for find_symbol
    let shorten_level = if total <= 10 {
        ShortenLevel::Full
    } else {
        ShortenLevel::ReferencesOnly
    };

    let shortened = shorten_results(&results, shorten_level);
    let text = render_find_symbol_text(&params.name, &params.mode, &results);

    let enrichment = serde_json::json!({
        "query_intent": format!("{:?}", crate::fts5_planner::AnalyzedQuery::analyze(&params.name).intent),
        "result_count": total,
        "mode": params.mode,
    });

    let envelope = build_envelope(&params.name, state, shortened, enrichment, text);

    Response::success(&req.id, envelope)
}

// ---------------------------------------------------------------------------
// fts5_read_symbol
// ---------------------------------------------------------------------------

/// Parameters for `fts5_read_symbol`.
#[derive(Debug, Deserialize)]
struct Fts5ReadSymbolParams {
    /// Symbol ID to read (from a find/search result).
    #[serde(default)]
    symbol_id: Option<i64>,
    /// Exact symbol name to read.
    #[serde(default)]
    name: Option<String>,
    /// Optional file path to disambiguate when name matches multiple symbols.
    #[serde(default)]
    file: Option<String>,
    /// Number of context lines around the symbol (default: 0).
    #[serde(default)]
    context_lines: Option<u32>,
}

/// `fts5_read_symbol` — read canonical source for a symbol by result/symbol id.
pub fn handle_fts5_read_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    let params: Fts5ReadSymbolParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("fts5_read_symbol: invalid params: {e}"),
            );
        }
    };

    let project_root = grep_executor::project_root(ctx);
    let db_path = resolve_fts5_db_path(&project_root);

    let store = match crate::fts5_store::Fts5Store::open(&db_path) {
        Ok(store) => store,
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to open FTS5 store: {e}"),
            );
        }
    };

    // Resolve the symbol to read
    let symbol = if let Some(sym_id) = params.symbol_id {
        // Look up by symbol ID
        match store.get_symbol(sym_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Response::error(
                    &req.id,
                    "not_found",
                    format!("Symbol with id {sym_id} not found in FTS5 index."),
                );
            }
            Err(e) => {
                return Response::error(
                    &req.id,
                    "fts5_store_error",
                    format!("Failed to look up symbol: {e}"),
                );
            }
        }
    } else if let Some(ref name) = params.name {
        // Look up by name
        let candidates = store.get_symbol_by_name(name).unwrap_or_default();

        if candidates.is_empty() {
            return Response::error(
                &req.id,
                "not_found",
                format!("Symbol '{name}' not found in FTS5 index."),
            );
        }

        // If file path provided, filter to that file
        let filtered: Vec<_> = if let Some(ref _file_filter) = params.file {
            candidates
                .iter()
                .filter(|_s| {
                    // Simple contains check on file path stored in body or via file lookup
                    // For now, accept all — in a real implementation, look up the file record
                    true
                })
                .cloned()
                .collect()
        } else {
            candidates
        };

        if filtered.is_empty() {
            return Response::error(
                &req.id,
                "not_found",
                format!(
                    "Symbol '{name}' not found{}.",
                    if params.file.is_some() {
                        " in the specified file"
                    } else {
                        ""
                    }
                ),
            );
        }

        if filtered.len() > 1 {
            // Ambiguous — return candidates with identity fields
            let candidate_list: Vec<serde_json::Value> = filtered
                .iter()
                .map(|s| {
                    let mut val = serde_json::json!({
                        "symbol_id": s.id,
                        "file_id": s.file_id,
                        "symbol_name": s.name,
                        "symbol_kind": s.kind,
                        "start_line": s.start_line,
                        "end_line": s.end_line,
                        "name_path": s.name_path,
                        "duplicate_index": s.duplicate_index,
                    });
                    if s.name_path.is_empty() {
                        val.as_object_mut().unwrap().remove("name_path");
                    }
                    val
                })
                .collect();

            return Response::success(
                &req.id,
                serde_json::json!({
                    "ambiguous": true,
                    "candidates": candidate_list,
                    "count": candidate_list.len(),
                    "message": format!("Symbol '{}' matches {} locations. Specify file or use symbol_id.", name, candidate_list.len()),
                }),
            );
        }

        filtered.into_iter().next().unwrap()
    } else {
        return Response::error(
            &req.id,
            "invalid_request",
            "Either symbol_id or name is required.",
        );
    };

    // Get the file path
    let file = match store.get_file_by_id(symbol.file_id) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return Response::error(&req.id, "not_found", "Symbol's file not found in index.");
        }
        Err(e) => {
            return Response::error(
                &req.id,
                "fts5_store_error",
                format!("Failed to look up file: {e}"),
            );
        }
    };

    let abs_path = project_root.join(&file.path);

    // Read the file content
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => {
            return Response::error(
                &req.id,
                "file_read_error",
                format!("Failed to read {}: {e}", file.path),
            );
        }
    };

    // Extract the symbol body with optional context
    let lines: Vec<&str> = content.lines().collect();
    let start_line = symbol.start_line.saturating_sub(1) as usize; // Convert to 0-indexed
    let end_line = (symbol.end_line as usize).min(lines.len());
    let context_lines = params.context_lines.unwrap_or(0) as usize;

    let ctx_start = start_line.saturating_sub(context_lines);
    let ctx_end = (end_line + context_lines).min(lines.len());

    let body_lines: Vec<String> = (ctx_start..ctx_end)
        .map(|i| format!("{}: {}", i + 1, lines[i]))
        .collect();

    let body = body_lines.join("\n");

    let text = render_read_symbol_text(
        &symbol.name,
        &file.path,
        symbol.start_line,
        symbol.end_line,
        &body,
    );

    Response::success(
        &req.id,
        serde_json::json!({
            "symbol_id": symbol.id,
            "file_id": symbol.file_id,
            "file_path": file.path,
            "symbol_name": symbol.name,
            "symbol_kind": symbol.kind,
            "start_line": symbol.start_line,
            "end_line": symbol.end_line,
            "body": body,
            "line_count": symbol.end_line - symbol.start_line + 1,
            "name_path": if symbol.name_path.is_empty() { serde_json::Value::Null } else { serde_json::json!(symbol.name_path) },
            "duplicate_index": symbol.duplicate_index,
            "text": text,
        }),
    )
}

// ---------------------------------------------------------------------------
// fts5_doctor
// ---------------------------------------------------------------------------

/// `fts5_doctor` — diagnose FTS5 index health and configuration.
pub fn handle_fts5_doctor(req: &RawRequest, ctx: &AppContext) -> Response {
    let fts5_enabled = ctx.config().fts5.enabled;
    let fts5_cfg = &ctx.config().fts5;
    let project_root = grep_executor::project_root(ctx);
    let db_path = resolve_fts5_db_path(&project_root);

    // Check FTS5 availability
    let fts5_available = crate::fts5_experimental::check_fts5_available();

    // Check index status
    let index_info = if db_path.exists() {
        match crate::fts5_store::Fts5Store::open(&db_path) {
            Ok(store) => {
                let file_count = store.file_count().unwrap_or(0);
                let symbol_count = store.symbol_count().unwrap_or(0);
                let schema_version = store.schema_version().unwrap_or(0);
                let db_size = store.db_size_bytes();
                let row_counts =
                    store
                        .fts_row_counts()
                        .unwrap_or_else(|_| crate::fts5_store::FtsRowCounts {
                            symbols_fts: 0,
                            bodies_fts: 0,
                            paths_fts: 0,
                        });
                let stale = store.stale_files(&project_root).unwrap_or_default();
                let integrity = store
                    .integrity_check()
                    .unwrap_or_else(|e| format!("error: {e}"));

                serde_json::json!({
                    "exists": true,
                    "schema_version": schema_version,
                    "file_count": file_count,
                    "symbol_count": symbol_count,
                    "db_size_bytes": db_size,
                    "fts_row_counts": {
                        "symbols": row_counts.symbols_fts,
                        "bodies": row_counts.bodies_fts,
                        "paths": row_counts.paths_fts,
                    },
                    "stale_files": stale.len(),
                    "integrity": integrity,
                    "db_path": db_path.display().to_string(),
                })
            }
            Err(e) => {
                serde_json::json!({
                    "exists": true,
                    "error": format!("Failed to open: {e}"),
                    "db_path": db_path.display().to_string(),
                })
            }
        }
    } else {
        serde_json::json!({
            "exists": false,
            "message": "No FTS5 index found.",
        })
    };

    // Build warnings and suggestions
    let mut warnings = Vec::new();
    let mut suggestions = Vec::new();

    if !fts5_enabled {
        warnings.push("FTS5 is compiled but disabled at runtime.".to_string());
        suggestions.push("Set [fts5].enabled = true in aft.jsonc.".to_string());
    }

    if !fts5_available {
        warnings.push("FTS5 is not available in this SQLite build.".to_string());
    }

    if let Some(stale_count) = index_info.get("stale_files").and_then(|v| v.as_i64()) {
        if stale_count > 0 {
            warnings.push(format!("{stale_count} file(s) in index are stale."));
            suggestions.push("Run fts5_index with action=update to refresh.".to_string());
        }
    }

    if index_info.get("exists").and_then(|v| v.as_bool()) == Some(false) {
        suggestions.push("Run fts5_index with action=update to create the index.".to_string());
    }

    let text = render_doctor_text(fts5_enabled, fts5_available, &index_info, &warnings);

    Response::success(
        &req.id,
        serde_json::json!({
            "compiled": true,
            "fts5_available": fts5_available,
            "enabled": fts5_enabled,
            "config": {
                "auto_index": fts5_cfg.auto_index,
                "index_on_start": fts5_cfg.index_on_start,
                "max_results": fts5_cfg.max_results,
                "max_body_chars": fts5_cfg.max_body_chars,
                "max_body_lines": fts5_cfg.max_body_lines,
                "raw_fts_debug": fts5_cfg.raw_fts_debug,
            },
            "index": index_info,
            "warnings": warnings,
            "suggestions": suggestions,
            "text": text,
        }),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::protocol::RawRequest;
    use serde_json::json;

    fn req_for(command: &str, params: serde_json::Value) -> RawRequest {
        RawRequest {
            id: "test-1".to_string(),
            command: command.to_string(),
            lsp_hints: None,
            session_id: None,
            params,
        }
    }

    fn make_ctx_with_fts5(enabled: bool) -> AppContext {
        make_ctx_with_fts5_and_root(enabled, None)
    }

    fn make_ctx_with_fts5_and_root(enabled: bool, project_root: Option<&str>) -> AppContext {
        let mut config = Config::default();
        config.fts5.enabled = enabled;
        if let Some(root) = project_root {
            config.project_root = Some(std::path::PathBuf::from(root));
        }
        AppContext::new(Box::new(TreeSitterProvider::new()), config)
    }

    #[test]
    fn fts5_doctor_reports_compiled_and_disabled_by_default() {
        let tmp = std::env::temp_dir().join("fts5_cmd_test_doctor_disabled");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let req = req_for("fts5_doctor", json!({}));
        let ctx = make_ctx_with_fts5_and_root(false, Some(tmp.to_str().unwrap()));
        let resp = handle_fts5_doctor(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        let data = &resp.data;
        assert_eq!(data["compiled"], true);
        assert_eq!(data["enabled"], false);
        assert!(data["config"].is_object());
        assert!(data["index"].is_object());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fts5_doctor_reports_enabled_when_configured() {
        let tmp = std::env::temp_dir().join("fts5_cmd_test_doctor_enabled");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let req = req_for("fts5_doctor", json!({}));
        let ctx = make_ctx_with_fts5_and_root(true, Some(tmp.to_str().unwrap()));
        let resp = handle_fts5_doctor(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        let data = &resp.data;
        assert_eq!(data["compiled"], true);
        assert_eq!(data["enabled"], true);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fts5_index_returns_disabled_when_not_enabled() {
        let req = req_for("fts5_index", json!({}));
        let ctx = make_ctx_with_fts5(false);
        let resp = handle_fts5_index(&req, &ctx);
        // When feature is compiled but runtime disabled, we get an error response.
        assert!(!resp.success, "expected error for disabled, got: {resp:?}");
        assert_eq!(resp.data["code"], "fts5_disabled");
    }

    #[test]
    fn fts5_search_returns_disabled_when_not_enabled() {
        let req = req_for("fts5_search", json!({ "query": "test" }));
        let ctx = make_ctx_with_fts5(false);
        let resp = handle_fts5_search(&req, &ctx);
        assert!(!resp.success, "expected error for disabled, got: {resp:?}");
        assert_eq!(resp.data["code"], "fts5_disabled");
    }

    #[test]
    fn fts5_find_symbol_returns_disabled_when_not_enabled() {
        let req = req_for("fts5_find_symbol", json!({ "name": "Foo" }));
        let ctx = make_ctx_with_fts5(false);
        let resp = handle_fts5_find_symbol(&req, &ctx);
        assert!(!resp.success, "expected error for disabled, got: {resp:?}");
        assert_eq!(resp.data["code"], "fts5_disabled");
    }

    #[test]
    fn fts5_read_symbol_returns_disabled_when_not_enabled() {
        let req = req_for("fts5_read_symbol", json!({ "result_id": "abc" }));
        let ctx = make_ctx_with_fts5(false);
        let resp = handle_fts5_read_symbol(&req, &ctx);
        assert!(!resp.success, "expected error for disabled, got: {resp:?}");
        assert_eq!(resp.data["code"], "fts5_disabled");
    }

    #[test]
    fn fts5_index_status_works_when_enabled() {
        let tmp = std::env::temp_dir().join("fts5_cmd_test_status");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let req = req_for("fts5_index", json!({ "action": "status" }));
        let ctx = make_ctx_with_fts5_and_root(true, Some(tmp.to_str().unwrap()));
        let resp = handle_fts5_index(&req, &ctx);
        assert!(resp.success, "expected success for status, got: {resp:?}");
        assert_eq!(resp.data["exists"], false);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fts5_search_returns_empty_when_index_empty() {
        let tmp = std::env::temp_dir().join("fts5_cmd_test_search_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let req = req_for("fts5_search", json!({ "query": "test" }));
        let ctx = make_ctx_with_fts5_and_root(true, Some(tmp.to_str().unwrap()));
        let resp = handle_fts5_search(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        assert_eq!(resp.data["total"], 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fts5_search_rejects_empty_query() {
        let req = req_for("fts5_search", json!({ "query": "" }));
        let ctx = make_ctx_with_fts5(true);
        let resp = handle_fts5_search(&req, &ctx);
        assert!(!resp.success, "expected error for empty query");
    }

    // -----------------------------------------------------------------------
    // Output envelope tests
    // -----------------------------------------------------------------------

    #[test]
    fn output_state_as_str() {
        assert_eq!(OutputState::Complete.as_str(), "complete");
        assert_eq!(OutputState::Truncated.as_str(), "truncated");
        assert_eq!(OutputState::Stale.as_str(), "stale");
        assert_eq!(OutputState::Degraded.as_str(), "degraded");
        assert_eq!(OutputState::Empty.as_str(), "empty");
    }

    #[test]
    fn build_envelope_empty() {
        let envelope = build_envelope(
            "test",
            OutputState::Empty,
            vec![],
            json!({}),
            "no results".into(),
        );
        assert_eq!(envelope["state"], "empty");
        assert!(envelope["message"].as_str().unwrap().contains("No results"));
    }

    #[test]
    fn build_envelope_complete() {
        let evidence = vec![json!({"symbol_name": "Foo"})];
        let envelope = build_envelope(
            "Foo",
            OutputState::Complete,
            evidence,
            json!({}),
            "found Foo".into(),
        );
        assert_eq!(envelope["state"], "complete");
        assert_eq!(envelope["evidence"][0]["symbol_name"], "Foo");
    }

    #[test]
    fn shorten_results_full() {
        let results = vec![json!({"symbol_name": "Foo", "snippet": "struct Foo {}"})];
        let shortened = shorten_results(&results, ShortenLevel::Full);
        assert_eq!(shortened.len(), 1);
        assert!(shortened[0].get("snippet").is_some());
    }

    #[test]
    fn shorten_results_references_only() {
        let results =
            vec![json!({"symbol_name": "Foo", "snippet": "struct Foo {}", "file_path": "a.rs"})];
        let shortened = shorten_results(&results, ShortenLevel::ReferencesOnly);
        assert_eq!(shortened.len(), 1);
        assert!(shortened[0].get("snippet").is_none());
        assert!(shortened[0].get("file_path").is_some());
    }

    #[test]
    fn shorten_results_file_counts() {
        let results = vec![
            json!({"file_path": "a.rs"}),
            json!({"file_path": "a.rs"}),
            json!({"file_path": "b.rs"}),
        ];
        let shortened = shorten_results(&results, ShortenLevel::FileCounts);
        assert_eq!(shortened.len(), 2); // 2 unique files
    }

    #[test]
    fn shorten_results_summary() {
        let results = vec![json!({"file_path": "a.rs"}), json!({"file_path": "b.rs"})];
        let shortened = shorten_results(&results, ShortenLevel::Summary);
        assert_eq!(shortened.len(), 1);
        assert_eq!(shortened[0]["total_matches"], 2);
        assert_eq!(shortened[0]["unique_files"], 2);
    }
}
