//! `model_cache` commands — manage locally cached model2vec models.
//!
//! ## Wire format
//!
//! `model_cache_list`:
//! ```json
//! { }
//! ```
//! Response: `{ "models": [{ "repo_id": "...", "path": "...", "size_bytes": 123 }] }`
//!
//! `model_cache_remove`:
//! ```json
//! { "repo_id": "minishlab/potion-code-16M" }
//! ```
//! Response: `{ "removed": true }` or `{ "removed": false, "error": "..." }`
//!
//! `model_cache_info`:
//! ```json
//! { "repo_id": "minishlab/potion-code-16M" }
//! ```
//! Response (found): `{ "found": true, "repo_id": "...", "path": "...", "downloaded_at": 1234567890, "size_bytes": 123 }`
//! Response (not found): `{ "found": false }`
//!
//! `model_cache_check_update`:
//! ```json
//! { "repo_id": "minishlab/potion-code-16M" }
//! ```
//! Response: `{ "update_available": true, "message": "..." }` or `{ "update_available": false }`

use serde::Deserialize;
use serde::Serialize;

use crate::model2vec_download::{
    check_for_update, get_model_version_info, list_cached_models, remove_cached_model,
};
use crate::protocol::{RawRequest, Response};

#[derive(Debug, Deserialize)]
struct RepoIdParams {
    repo_id: String,
}

fn repo_id_params(req: &RawRequest) -> Result<String, Response> {
    let params: RepoIdParams = serde_json::from_value(req.params.clone())
        .map_err(|e| Response::error(&req.id, "invalid_request", format!("invalid params: {e}")))?;

    // Validate repo_id format up front so info/check_update can return a clear
    // invalid_request error instead of silently treating a malformed id as "not found".
    if crate::repo_id::split_hf_repo_id(&params.repo_id).is_err() {
        return Err(Response::error(
            &req.id,
            "invalid_request",
            format!(
                "invalid repo_id '{}': expected 'owner/name'",
                params.repo_id
            ),
        ));
    }

    Ok(params.repo_id)
}

pub fn handle_model_cache_list(req: &RawRequest, _ctx: &crate::context::AppContext) -> Response {
    let models = list_cached_models()
        .into_iter()
        .map(|(repo_id, path, size_bytes)| ModelEntry {
            repo_id,
            path: path.to_string_lossy().to_string(),
            size_bytes,
        })
        .collect::<Vec<_>>();

    Response::success(&req.id, serde_json::json!({ "models": models }))
}

#[derive(Serialize)]
struct ModelEntry {
    repo_id: String,
    path: String,
    size_bytes: u64,
}

pub fn handle_model_cache_remove(req: &RawRequest, _ctx: &crate::context::AppContext) -> Response {
    let repo_id = match repo_id_params(req) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    match remove_cached_model(&repo_id) {
        Ok(()) => Response::success(&req.id, serde_json::json!({ "removed": true })),
        Err(error) => Response::error(&req.id, "model_cache_error", error),
    }
}

pub fn handle_model_cache_info(req: &RawRequest, _ctx: &crate::context::AppContext) -> Response {
    let repo_id = match repo_id_params(req) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    match get_model_version_info(&repo_id) {
        Some(info) => Response::success(
            &req.id,
            serde_json::json!({
                "found": true,
                "repo_id": info.repo_id,
                "path": info.model_dir.to_string_lossy().to_string(),
                "downloaded_at": info.downloaded_at,
                "size_bytes": info.total_size_bytes,
            }),
        ),
        None => Response::success(&req.id, serde_json::json!({ "found": false })),
    }
}

pub fn handle_model_cache_check_update(
    req: &RawRequest,
    _ctx: &crate::context::AppContext,
) -> Response {
    let repo_id = match repo_id_params(req) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    match check_for_update(&repo_id) {
        Some(message) => Response::success(
            &req.id,
            serde_json::json!({
                "update_available": true,
                "message": message,
            }),
        ),
        None => Response::success(&req.id, serde_json::json!({ "update_available": false })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use crate::protocol::RawRequest;
    use serde_json::json;

    fn make_ctx() -> AppContext {
        AppContext::new(Box::new(TreeSitterProvider::new()), Config::default())
    }

    #[test]
    fn handle_model_cache_info_rejects_missing_repo_id() {
        let req = RawRequest {
            id: "test-1".to_string(),
            command: "model_cache_info".to_string(),
            lsp_hints: None,
            session_id: None,
            params: json!({}),
        };
        let ctx = make_ctx();
        let resp = handle_model_cache_info(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "invalid_request");
    }

    #[test]
    fn handle_model_cache_info_rejects_invalid_repo_id() {
        let req = RawRequest {
            id: "test-1".to_string(),
            command: "model_cache_info".to_string(),
            lsp_hints: None,
            session_id: None,
            params: json!({"repo_id": "no-slash"}),
        };
        let ctx = make_ctx();
        let resp = handle_model_cache_info(&req, &ctx);
        assert!(!resp.success);
        assert_eq!(resp.data["code"], "invalid_request");
    }
}
