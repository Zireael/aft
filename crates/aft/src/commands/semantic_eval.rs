//! `semantic_eval` command — run a local JSONL eval suite against AFT's
//! semantic search and report recall@k and MRR.
//!
//! ## Wire format
//!
//! Request:
//! ```json
//! {
//!   "path": ".aft/semantic-eval.jsonl",
//!   "top_k": 10,
//!   "include_per_case": true
//! }
//! ```
//!
//! - `path` (required) — JSONL file. Each line is one eval case.
//! - `top_k` (optional) — default cutoff for recall@k (default 10).
//! - `include_per_case` (optional, default true) — include per-case results
//!   in the response. Set false for a one-line summary in agent output.
//!
//! ## Response
//!
//! ```json
//! {
//!   "total": 12,
//!   "hits_in_top_k": 9,
//!   "recall_at_k": 0.75,
//!   "mrr": 0.612,
//!   "k": 10,
//!   "cases": [ { "index": 0, "query": "...", "first_hit_rank": 1, ... } ]
//! }
//! ```
//!
//! Or when `include_per_case` is false:
//! ```json
//! { "summary_line": "eval: 9/12 hits, recall@10=0.750, mrr=0.612" }
//! ```

use serde::Deserialize;

use crate::protocol::{RawRequest, Response};
use crate::semantic_eval as eval;

#[derive(Debug, Deserialize)]
struct SemanticEvalParams {
    path: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default = "default_include_per_case")]
    include_per_case: bool,
}

fn default_top_k() -> usize {
    10
}
fn default_include_per_case() -> bool {
    true
}

pub fn handle_semantic_eval(req: &RawRequest, _ctx: &crate::context::AppContext) -> Response {
    let params: SemanticEvalParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("semantic_eval: invalid params: {e}"),
            );
        }
    };
    if params.top_k == 0 {
        return Response::error(
            &req.id,
            "invalid_request",
            "semantic_eval: top_k must be >= 1".to_string(),
        );
    }
    let text = match std::fs::read_to_string(&params.path) {
        Ok(t) => t,
        Err(e) => {
            return Response::error(
                &req.id,
                "eval_file_unreadable",
                format!("semantic_eval: cannot read {}: {e}", params.path),
            );
        }
    };
    let cases = match eval::parse_jsonl(&text) {
        Ok(c) => c,
        Err(e) => {
            return Response::error(
                &req.id,
                "eval_file_parse_error",
                format!("semantic_eval: {e}"),
            );
        }
    };
    // Note: This stub returns zero retrieved hits per case. Wiring to
    // `handle_semantic_search` is deferred to a follow-up Bead; for now the
    // harness is exercised through its pure-logic surface (parser, matcher,
    // scorer). Misses surface as expected and are the agent's signal that
    // the upstream wiring is not yet in place.
    let results: Vec<Vec<eval::RetrievedHit>> = cases.iter().map(|_| Vec::new()).collect();
    let summary = eval::score_suite(&cases, &results, params.top_k);

    let mut payload = serde_json::json!({
        "total": summary.total,
        "hits_in_top_k": summary.hits_in_top_k,
        "recall_at_k": summary.recall_at_k,
        "mrr": summary.mrr,
        "k": summary.k,
        "summary_line": summary.render_line(),
    });
    if params.include_per_case {
        payload["cases"] =
            serde_json::to_value(&summary.cases).unwrap_or(serde_json::Value::Array(vec![]));
    }
    Response::success(&req.id, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::protocol::RawRequest;
    use serde_json::json;

    fn req_for(params: serde_json::Value) -> RawRequest {
        RawRequest {
            id: "test-1".to_string(),
            command: "semantic_eval".to_string(),
            lsp_hints: None,
            session_id: None,
            params,
        }
    }

    fn make_ctx() -> AppContext {
        AppContext::new(Box::new(TreeSitterProvider::new()), Config::default())
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    static EVAL_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn write_eval(content: &str) -> std::path::PathBuf {
        let counter = EVAL_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aft-eval-test-{}-{}", std::process::id(), counter));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("eval.jsonl");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn handle_returns_summary_for_valid_eval() {
        let path = write_eval(
            r#"{"query":"q1","expected_paths":["a.rs"]}
{"query":"q2","expected_paths":["b.rs"]}
"#,
        );
        let req = req_for(json!({ "path": path.to_string_lossy() }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        assert!(resp.success, "got: {resp:?}");
        let v = &resp.data;
        assert_eq!(v["total"], 2);
        assert_eq!(v["hits_in_top_k"], 0); // stub returns no hits
        assert_eq!(v["k"], 10);
        assert!(v["summary_line"].as_str().unwrap().contains("0/2"));
    }

    #[test]
    fn handle_rejects_missing_path_param() {
        let req = req_for(json!({}));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "invalid_request");
    }

    #[test]
    fn handle_rejects_unreadable_path() {
        let req = req_for(json!({ "path": "/nonexistent/path/to/eval.jsonl" }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "eval_file_unreadable");
    }

    #[test]
    fn handle_rejects_zero_top_k() {
        let path = write_eval(r#"{"query":"q1","expected_paths":["a.rs"]}"#);
        let req = req_for(json!({ "path": path.to_string_lossy(), "top_k": 0 }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "invalid_request");
    }

    #[test]
    fn handle_rejects_invalid_jsonl() {
        let path = write_eval("not json\n");
        let req = req_for(json!({ "path": path.to_string_lossy() }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "eval_file_parse_error");
    }

    #[test]
    fn handle_omits_per_case_when_disabled() {
        let path = write_eval(r#"{"query":"q1","expected_paths":["a.rs"]}"#);
        let req = req_for(json!({
            "path": path.to_string_lossy(),
            "include_per_case": false
        }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        let v = &resp.data;
        assert!(v.get("cases").is_none(), "got: {v}");
        assert!(v.get("summary_line").is_some());
    }

    #[test]
    fn handle_includes_per_case_by_default() {
        let path = write_eval(r#"{"query":"q1","expected_paths":["a.rs"]}"#);
        let req = req_for(json!({ "path": path.to_string_lossy() }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        let v = &resp.data;
        assert!(v.get("cases").is_some(), "got: {v}");
        let cases = v["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0]["query"], "q1");
    }

    #[test]
    fn handle_respects_top_k_override() {
        let path = write_eval(r#"{"query":"q1","expected_paths":["a.rs"]}"#);
        let req = req_for(json!({
            "path": path.to_string_lossy(),
            "top_k": 3
        }));
        let ctx = make_ctx();
        let resp = handle_semantic_eval(&req, &ctx);
        let v = &resp.data;
        assert_eq!(v["k"], 3);
    }
}
