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
        return Response::success(
            &req.id,
            serde_json::json!({
                "exists": false,
                "message": "No FTS5 index found. Run fts5_index with action=update to create.",
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

    // Check for stale files
    let stale = store.stale_files(project_root).unwrap_or_default();
    let stale_count = stale.len();

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

    Response::success(
        &req.id,
        serde_json::json!({
            "action": if rebuild { "rebuild" } else { "update" },
            "files_processed": stats.files_processed,
            "files_added": stats.files_added,
            "files_updated": stats.files_updated,
            "files_removed": stats.files_removed,
            "symbols_extracted": stats.symbols_extracted,
            "files_failed": stats.files_failed,
            "db_path": db_path.display().to_string(),
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
            serde_json::json!({
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
            })
        })
        .collect();

    let total = json_results.len();

    Response::success(
        &req.id,
        serde_json::json!({
            "results": json_results,
            "total": total,
            "query": params.query,
            "scope": params.scope,
            "complete": true,
        }),
    )
}

// ---------------------------------------------------------------------------
// fts5_find_symbol
// ---------------------------------------------------------------------------

/// `fts5_find_symbol` — look up a symbol by name in the FTS5 index.
pub fn handle_fts5_find_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "status": "stub",
            "message": "fts5_find_symbol is compiled and enabled but not yet implemented.",
        }),
    )
}

// ---------------------------------------------------------------------------
// fts5_read_symbol
// ---------------------------------------------------------------------------

/// `fts5_read_symbol` — read canonical source for a symbol by result/symbol id.
pub fn handle_fts5_read_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "status": "stub",
            "message": "fts5_read_symbol is compiled and enabled but not yet implemented.",
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
}
