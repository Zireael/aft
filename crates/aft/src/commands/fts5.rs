//! FTS5 side-feature command stubs.
//!
//! These handlers are behind the `semantic-fts5` Cargo feature. When the
//! feature is compiled but the runtime config has `fts5.enabled = false`,
//! every command returns a clear `disabled` status so callers know the
//! feature exists but is not active.
//!
//! The real implementations will replace these stubs in later beads.

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

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

/// `fts5_search` — search the FTS5 index.
pub fn handle_fts5_search(req: &RawRequest, ctx: &AppContext) -> Response {
    if !ctx.config().fts5.enabled {
        return runtime_disabled(req);
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "status": "stub",
            "message": "fts5_search is compiled and enabled but not yet implemented.",
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
}
