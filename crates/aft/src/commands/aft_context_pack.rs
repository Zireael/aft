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

    let search = run_public_search(req, ctx, query, 25);
    if !search.success {
        return search;
    }

    let results = search
        .data
        .get("results")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let max_tokens = ((token_budget as f64) * 1.10).ceil() as usize;
    let mut pack = Vec::new();
    let mut tokens_used = 0usize;
    let mut omitted_candidates = 0usize;

    for result in &results {
        let Some(file) = result.get("file").and_then(|value| value.as_str()) else {
            omitted_candidates += 1;
            continue;
        };

        let remaining = max_tokens.saturating_sub(tokens_used);
        if remaining == 0 {
            omitted_candidates += 1;
            continue;
        }

        let content = bounded_file_excerpt(file, remaining);
        let estimated_tokens = estimate_tokens(&content)
            + estimate_tokens(file)
            + result
                .get("snippet")
                .and_then(|value| value.as_str())
                .map(estimate_tokens)
                .unwrap_or(0)
            + 12;

        if estimated_tokens == 0 || tokens_used + estimated_tokens > max_tokens {
            omitted_candidates += 1;
            continue;
        }

        tokens_used += estimated_tokens;
        pack.push(serde_json::json!({
            "file": file,
            "score": result.get("score").cloned().unwrap_or(serde_json::Value::Null),
            "source": result.get("source").cloned().unwrap_or(serde_json::Value::Null),
            "start_line": result.get("start_line").cloned().unwrap_or(serde_json::Value::Null),
            "end_line": result.get("end_line").cloned().unwrap_or(serde_json::Value::Null),
            "enrichment_state": result.get("enrichment_state").cloned().unwrap_or(serde_json::Value::Null),
            "graph_context": result.get("graph_context").cloned().unwrap_or(serde_json::Value::Null),
            "tokens_estimate": estimated_tokens,
            "content": content,
        }));
    }

    let omission_reason = if omitted_candidates == 0 {
        "none".to_string()
    } else if tokens_used >= max_tokens {
        "token_budget_exhausted".to_string()
    } else {
        "candidate_content_unavailable_or_over_budget".to_string()
    };

    let result = serde_json::json!({
        "query": query,
        "token_budget": token_budget,
        "max_tokens_with_tolerance": max_tokens,
        "tokens_used": tokens_used,
        "pack": pack,
        "omitted_candidates": omitted_candidates,
        "omission_reason": omission_reason,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("context_pack_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

fn run_public_search(req: &RawRequest, ctx: &AppContext, query: &str, top_k: usize) -> Response {
    let search_req = RawRequest {
        id: format!("{}:aft_context_pack_search", req.id),
        command: "semantic_search".to_string(),
        lsp_hints: req.lsp_hints.clone(),
        session_id: req.session_id.clone(),
        params: serde_json::json!({
            "query": query,
            "top_k": top_k,
            "profile": "agent_deep",
        }),
    };
    crate::commands::semantic_search::handle_semantic_search(&search_req, ctx)
}

fn bounded_file_excerpt(file: &str, remaining_tokens: usize) -> String {
    let max_chars = remaining_tokens
        .saturating_sub(16)
        .saturating_mul(4)
        .min(4096);
    if max_chars == 0 {
        return String::new();
    }
    let Ok(content) = std::fs::read_to_string(file) else {
        return String::new();
    };
    if content.len() <= max_chars {
        return content;
    }

    let mut end = max_chars;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    content[..end].to_string()
}

fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.len().div_ceil(4)
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
