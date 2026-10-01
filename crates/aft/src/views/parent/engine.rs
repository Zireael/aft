//! Per-child search engines for a parent folder's `aft_search`.
//!
//! `aft_search` ranks with lanes and fusion that belong to one root, so a
//! parent answer is assembled from each child's own engine rather than from a
//! second ranking of the parent's making. Each child gets an [`AppContext`]
//! that is never configured or bound: the worker installs the child's loaded
//! trigram snapshot and, when semantic search is on, the semantic index built
//! from the child's admitted view vectors. Nothing in that context builds,
//! loads or writes an index; a query only runs the engine over what is
//! installed. Every child's ranking is therefore the ranking a direct search
//! of that child produces over the same content.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

use super::query::{attach_gaps, Gaps};
use super::{display_relative, lock, read, Child, ParentSession, Plane};

/// One child's search engine context and what is installed in it.
pub(crate) struct ChildEngine {
    ctx: AppContext,
    /// Address of the trigram snapshot currently installed.
    trigram: usize,
    /// Name of the semantic generation currently installed.
    semantic: Option<String>,
}

impl ChildEngine {
    fn new(session: &ParentSession, child: &Child) -> Self {
        let mut config = crate::config::Config::default();
        config.project_root = Some(child.root.clone());
        // The engine reads the indexes installed below, never a view of its own.
        config.views.enabled = false;
        config.indexes.trigram = true;
        config.indexes.semantic = session.planes.semantic;
        config.indexes.callgraph = false;
        config.semantic = session.semantic_config.clone();
        Self {
            ctx: AppContext::new(Box::new(crate::parser::TreeSitterProvider::new()), config),
            trigram: 0,
            semantic: None,
        }
    }

    fn install_trigram(&mut self, child: &Child) {
        let Some(trigram) = read(&child.trigram).ready().cloned() else {
            return;
        };
        let address = Arc::as_ptr(&trigram.index) as usize;
        if address == self.trigram {
            return;
        }
        // `SearchIndex` shares its postings through `Arc`s, so the copy the
        // engine owns costs only the per-file tables.
        *self
            .ctx
            .search_index()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((*trigram.index).clone());
        self.trigram = address;
    }

    fn install_semantic(&mut self, session: &ParentSession, child: &Child) {
        let Plane::Ready(semantic) = read(&session.semantic).clone() else {
            return;
        };
        let Some(view) = read(&child.semantic).ready().cloned() else {
            return;
        };
        let name = view.generation.name().to_owned();
        if self.semantic.as_deref() == Some(name.as_str()) {
            return;
        }
        // The overlay index scores the vectors admitted from the child's view
        // with the same function a session bound to the child uses.
        let overlay = match semantic
            .plane
            .overlay(&view.access, &child.root, &view.snapshot)
        {
            Ok(overlay) => overlay,
            Err(error) => {
                crate::slog_warn!(
                    "parent folder search engine has no semantic index for {}: {}",
                    child.root.display(),
                    error
                );
                return;
            }
        };
        *self
            .ctx
            .semantic_index()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((*overlay.index).clone());
        *self
            .ctx
            .semantic_index_status()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            crate::context::SemanticIndexStatus::ready();
        self.semantic = Some(name);
    }
}

/// Creates the child's engine once its trigram snapshot is loaded, and
/// installs the child's current snapshots into it. Runs on the worker.
pub(super) fn sync(session: &ParentSession, child: &Child) {
    if !session.planes.trigram && !session.planes.semantic {
        return;
    }
    let mut engine = lock(&child.engine);
    if engine.is_none() {
        if !child.trigram_ready() {
            return;
        }
        *engine = Some(ChildEngine::new(session, child));
    }
    if let Some(engine) = engine.as_mut() {
        engine.install_trigram(child);
        engine.install_semantic(session, child);
    }
}

/// Installs the child's current trigram snapshot after file changes.
pub(super) fn sync_trigram(child: &Child) {
    if let Some(engine) = lock(&child.engine).as_mut() {
        engine.install_trigram(child);
    }
}

// ---------------------------------------------------------------------------
// The parent's aft_search
// ---------------------------------------------------------------------------

/// The page size `aft_search` uses when the request names none.
const DEFAULT_TOP_K: usize = 10;

fn wire_score(row: &Value) -> f64 {
    row["score"].as_f64().unwrap_or(f64::MIN)
}

/// Rewrites a child row's paths to be relative to the parent folder. The
/// `location` field is rewritten only when it begins with the row's file:
/// some rows carry a description there instead of a path.
fn prefix_row(row: &mut Value, parent: &Path, child: &Child) {
    let Some(file) = row["file"].as_str().map(str::to_owned) else {
        return;
    };
    let path = Path::new(&file);
    let prefixed = if path.is_absolute() {
        display_relative(parent, path)
    } else {
        child
            .relative
            .join(&file)
            .to_string_lossy()
            .replace('\\', "/")
    };
    if let Some(Value::String(location)) = row.get_mut("location") {
        if let Some(rest) = location.strip_prefix(file.as_str()) {
            *location = format!("{prefixed}{rest}");
        }
    }
    row["file"] = Value::String(prefixed);
}

/// Merges the children's ranked lists without reordering any one of them:
/// at each step the child whose next row has the highest engine score
/// (fused score, or the lane score the engine reports instead) gives its
/// next row. Ties prefer the higher semantic similarity, which uses the same
/// model in every child, then the earlier child.
fn merge(mut lists: Vec<VecDeque<Value>>, limit: usize) -> Vec<Value> {
    let mut merged = Vec::new();
    while merged.len() < limit {
        let best = lists
            .iter()
            .enumerate()
            .filter_map(|(index, list)| list.front().map(|row| (index, row)))
            .max_by(|(left_index, left), (right_index, right)| {
                wire_score(left)
                    .total_cmp(&wire_score(right))
                    .then_with(|| {
                        left["semantic_score"]
                            .as_f64()
                            .unwrap_or(f64::MIN)
                            .total_cmp(&right["semantic_score"].as_f64().unwrap_or(f64::MIN))
                    })
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, _)| index);
        let Some(index) = best else {
            break;
        };
        if let Some(row) = lists[index].pop_front() {
            merged.push(row);
        }
    }
    merged
}

fn render_text(query: &str, page: &[Value], more_available: bool) -> String {
    if page.is_empty() {
        return format!("No results for \"{query}\" in this parent folder's repositories.");
    }
    let mut lines =
        page.iter()
            .map(|row| {
                let location = format!(
                    "{}:{}-{}",
                    row["file"].as_str().unwrap_or_default(),
                    row["start_line"].as_u64().unwrap_or(0),
                    row["end_line"].as_u64().unwrap_or(0)
                );
                let mut line = format!("{location} {}", row["name"].as_str().unwrap_or_default());
                if let Some(first) = row["snippet"].as_str().and_then(|snippet| {
                    snippet.lines().map(str::trim).find(|line| !line.is_empty())
                }) {
                    line.push_str(&format!("\n  {first}"));
                }
                line
            })
            .collect::<Vec<_>>();
    if more_available {
        lines.push("(More results are available: raise topK or pass offset.)".into());
    }
    lines.join("\n")
}

pub(super) fn search(req: &RawRequest, ctx: &AppContext, session: &ParentSession) -> Response {
    let Some(query) = req.params.get("query").and_then(Value::as_str) else {
        return Response::error(
            &req.id,
            "invalid_request",
            "semantic_search: missing required param 'query'",
        );
    };
    let top_k = req
        .params
        .get("top_k")
        .or_else(|| req.params.get("topK"))
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TOP_K);
    let offset = req
        .params
        .get("offset")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(0);
    let include_tests = req
        .params
        .get("include_tests")
        .or_else(|| req.params.get("includeTests"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let scope = match req.params.get("path").and_then(Value::as_str) {
        Some(raw) => match ctx.validate_path(&req.id, Path::new(raw)) {
            Ok(path) => {
                let path = if path.is_relative() {
                    session.root().join(path)
                } else {
                    path
                };
                std::fs::canonicalize(&path).unwrap_or(path)
            }
            Err(response) => return response,
        },
        None => session.root().to_path_buf(),
    };

    let mut gaps = Gaps::default();
    if !super::query::discovered(session, &mut gaps) {
        let mut body = json!({
            "status": "building",
            "complete": false,
            "text": "",
            "query": query,
            "include_tests": include_tests,
            "result_count": 0,
            "results": [],
            "more_available": false,
            "engine_capped": false,
            "fully_degraded": true,
        });
        attach_gaps(&mut body, gaps.into_values());
        return Response::success(&req.id, body);
    }
    let wanted = offset.saturating_add(top_k);
    // The model is lent to each engine in turn: the query is embedded once
    // (the model caches it) and no child starts a model of its own.
    let semantic = read(&session.semantic).clone();
    let mut model = semantic
        .ready()
        .and_then(|semantic| lock(&semantic.model).take());
    if let Some(reason) = semantic.gap_reason("semantic") {
        if session.planes.semantic {
            gaps.push("parent_semantic_unavailable", ".".into(), reason);
        }
    }
    let mut lists = Vec::new();
    let mut first: Option<Value> = None;
    let mut more_available = false;
    let mut engine_capped = false;
    let mut fully_degraded = true;
    let mut semantic_statuses = Vec::new();
    for child in session.children() {
        if !(child.root.starts_with(&scope) || scope.starts_with(&child.root)) {
            continue;
        }
        if let Some(reason) = read(&child.semantic).gap_reason("semantic") {
            if session.planes.semantic {
                gaps.child(&child, reason);
            }
        }
        let mut engine = lock(&child.engine);
        let Some(engine) = engine.as_mut() else {
            let reason = read(&child.trigram)
                .gap_reason("trigram")
                .unwrap_or_else(|| "the search engine of this repository is still loading".into());
            gaps.child(&child, reason);
            continue;
        };
        let mut params = json!({
            "id": req.id,
            "command": "semantic_search",
            "query": query,
            "top_k": wanted,
            "include_tests": include_tests,
        });
        if scope != session.root() && scope.starts_with(&child.root) {
            params["path"] = json!(scope);
        }
        let child_req: RawRequest = match serde_json::from_value(params) {
            Ok(request) => request,
            Err(error) => {
                gaps.child(&child, format!("aft_search: {error}"));
                continue;
            }
        };
        *engine.ctx.semantic_embedding_model().lock() = model.take();
        let response =
            crate::commands::semantic_search::handle_semantic_search(&child_req, &engine.ctx);
        model = engine.ctx.semantic_embedding_model().lock().take();
        if !response.success {
            gaps.child(
                &child,
                format!(
                    "aft_search failed in this repository: {}",
                    response.data["message"].as_str().unwrap_or("unknown error")
                ),
            );
            continue;
        }
        let data = response.data;
        if data["complete"] == false {
            gaps.child(
                &child,
                format!(
                    "aft_search: this repository's answer is incomplete (semantic status: {})",
                    data["semantic_status"].as_str().unwrap_or("unknown")
                ),
            );
        }
        more_available |= data["more_available"].as_bool().unwrap_or(false);
        engine_capped |= data["engine_capped"].as_bool().unwrap_or(false);
        fully_degraded &= data["fully_degraded"].as_bool().unwrap_or(false);
        if let Some(status) = data["semantic_status"].as_str() {
            semantic_statuses.push(status.to_owned());
        }
        let rows = data["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut row| {
                prefix_row(&mut row, session.root(), &child);
                row
            })
            .collect::<VecDeque<_>>();
        lists.push(rows);
        first.get_or_insert(data);
    }
    if let Some(semantic) = semantic.ready() {
        *lock(&semantic.model) = model;
    }
    gaps.scope(session, &scope);

    let total = lists.iter().map(VecDeque::len).sum::<usize>();
    let merged = merge(lists, wanted);
    more_available |= total > wanted;
    let page = merged.into_iter().skip(offset).collect::<Vec<_>>();
    let envelope = crate::list_envelope::ListEnvelope::new(
        page.len(),
        if more_available {
            crate::list_envelope::Total::AtLeast(offset + page.len() + 1)
        } else {
            crate::list_envelope::Total::Exact(total)
        },
        crate::list_surfaces::search::SEARCH_UNIT,
        if more_available {
            vec![crate::list_envelope::Reason::Cap]
        } else {
            Vec::new()
        },
        crate::list_surfaces::search::SEARCH_NARROW,
    );
    semantic_statuses.dedup();
    let semantic_status = match semantic_statuses.as_slice() {
        [single] => single.clone(),
        [] => "unavailable".to_string(),
        _ => "partial".to_string(),
    };
    let first = first.unwrap_or(Value::Null);
    let mut body = json!({
        "status": first["status"].as_str().unwrap_or("ready"),
        "complete": true,
        "text": render_text(query, &page, more_available),
        "query": query,
        "include_tests": include_tests,
        "interpreted_as": first["interpreted_as"],
        "query_kind": first["query_kind"],
        "result_count": page.len(),
        "results": page,
        "more_available": more_available,
        "engine_capped": engine_capped,
        "fully_degraded": fully_degraded,
        "semantic_status": semantic_status,
    });
    if let Some(object) = body.as_object_mut() {
        crate::list_surfaces::search::attach_projected_search_envelope(object, &envelope);
    }
    attach_gaps(&mut body, gaps.into_values());
    Response::success(&req.id, body)
}
