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

/// `fts5_index` — build or update the FTS5 index.
///
/// Supported actions (to be implemented): `status`, `update`, `rebuild`,
/// `prune`, `vacuum`, `integrity_check`.
pub fn handle_fts5_index(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    // Stub: real implementation in a later bead.
    Response::success(
        &req.id,
        serde_json::json!({
            "status": "stub",
            "message": "fts5_index is compiled and enabled but not yet implemented.",
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

    Response::success(
        &req.id,
        serde_json::json!({
            "compiled": true,
            "fts5_available": crate::fts5_experimental::check_fts5_available(),
            "enabled": fts5_enabled,
            "config": {
                "auto_index": fts5_cfg.auto_index,
                "index_on_start": fts5_cfg.index_on_start,
                "max_results": fts5_cfg.max_results,
                "max_body_chars": fts5_cfg.max_body_chars,
                "max_body_lines": fts5_cfg.max_body_lines,
                "raw_fts_debug": fts5_cfg.raw_fts_debug,
            },
            "index": {
                "status": "not_implemented",
                "message": "Index lifecycle not yet implemented."
            },
            "warnings": if !fts5_enabled {
                vec!["FTS5 is compiled but disabled at runtime.".to_string()]
            } else {
                vec![]
            },
            "suggestions": if !fts5_enabled {
                vec!["Set [fts5].enabled = true in aft.jsonc.".to_string()]
            } else {
                vec!["FTS5 is enabled. Run fts5_index to build the index.".to_string()]
            },
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
        let mut config = Config::default();
        config.fts5.enabled = enabled;
        AppContext::new(Box::new(TreeSitterProvider::new()), config)
    }

    #[test]
    fn fts5_doctor_reports_compiled_and_disabled_by_default() {
        let req = req_for("fts5_doctor", json!({}));
        let ctx = make_ctx_with_fts5(false);
        let resp = handle_fts5_doctor(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        let data = &resp.data;
        assert_eq!(data["compiled"], true);
        assert_eq!(data["enabled"], false);
        assert!(data["config"].is_object());
        assert!(data["index"].is_object());
    }

    #[test]
    fn fts5_doctor_reports_enabled_when_configured() {
        let req = req_for("fts5_doctor", json!({}));
        let ctx = make_ctx_with_fts5(true);
        let resp = handle_fts5_doctor(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        let data = &resp.data;
        assert_eq!(data["compiled"], true);
        assert_eq!(data["enabled"], true);
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
    fn fts5_index_returns_stub_when_enabled() {
        let req = req_for("fts5_index", json!({}));
        let ctx = make_ctx_with_fts5(true);
        let resp = handle_fts5_index(&req, &ctx);
        assert!(resp.success, "expected success for stub, got: {resp:?}");
        assert_eq!(resp.data["status"], "stub");
    }

    #[test]
    fn fts5_search_returns_empty_when_index_empty() {
        let req = req_for("fts5_search", json!({ "query": "test" }));
        let ctx = make_ctx_with_fts5(true);
        let resp = handle_fts5_search(&req, &ctx);
        assert!(resp.success, "expected success, got: {resp:?}");
        assert_eq!(resp.data["total"], 0);
        assert!(resp.data["warning"].as_str().unwrap().contains("empty"));
    }

    #[test]
    fn fts5_search_rejects_empty_query() {
        let req = req_for("fts5_search", json!({ "query": "" }));
        let ctx = make_ctx_with_fts5(true);
        let resp = handle_fts5_search(&req, &ctx);
        assert!(!resp.success, "expected error for empty query");
    }
}
