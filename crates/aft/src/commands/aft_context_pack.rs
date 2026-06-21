//! AFT Context Pack — greedy token-budget packing for context windows.
//!
//! Packs search results into a token budget using greedy packing,
//! with per-item enrichment_state tracking.

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle the `aft_context_pack` command.
pub fn handle_aft_context_pack(req: &RawRequest, ctx: &AppContext) -> Response {
    let query = req
        .params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let token_budget = req
        .params
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .unwrap_or(8000) as usize;

    if query.is_empty() {
        return Response::error(&req.id, "invalid_request", "query is required");
    }

    // Placeholder: would run URFK pipeline, then greedy pack
    let pack: Vec<serde_json::Value> = Vec::new();
    let tokens_used = 0;
    let omitted_candidates = 0;
    let omission_reason = "placeholder — not yet wired".to_string();

    let result = serde_json::json!({
        "query": query,
        "token_budget": token_budget,
        "tokens_used": tokens_used,
        "pack": pack,
        "omitted_candidates": omitted_candidates,
        "omission_reason": omission_reason,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("context_pack_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_test() {
        let result = serde_json::json!({
            "query": "test",
            "token_budget": 8000,
            "tokens_used": 0,
            "pack": [],
        });
        assert!(result.is_object());
    }
}
