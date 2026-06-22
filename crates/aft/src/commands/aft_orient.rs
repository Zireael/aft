//! AFT Orient — orientation command for retrieval intelligence.
//!
//! Returns primary files, entry symbols, dependency symbols, test hints,
//! config hints, and a deterministic orientation summary for a query.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::context::{AppContext, CallgraphStoreAccess};
use crate::protocol::{RawRequest, Response};
use crate::symbols::Symbol;

/// Handle the `aft_orient` command.
pub fn handle_aft_orient(req: &RawRequest, ctx: &AppContext) -> Response {
    let start = std::time::Instant::now();
    let query = req
        .params
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let depth = req
        .params
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;

    if query.is_empty() {
        return Response::error(&req.id, "invalid_request", "query is required");
    }

    let search = run_public_search(req, ctx, query, 10);
    if !search.success {
        return search;
    }

    let results = search
        .data
        .get("results")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut seen_files = BTreeSet::new();
    let primary_files: Vec<String> = results
        .iter()
        .filter_map(|result| result.get("file").and_then(|value| value.as_str()))
        .filter(|file| seen_files.insert((*file).to_string()))
        .take(5)
        .map(ToString::to_string)
        .collect();

    let entry_symbols = entry_symbols_for_files(ctx, query, &primary_files, 8);
    let (graph_health, dependency_symbols) =
        graph_orientation_facts(ctx, &primary_files, &entry_symbols, depth);

    // Test hints (path heuristic)
    let test_hints: Vec<String> = primary_files
        .iter()
        .filter(|file| {
            let p = file.to_lowercase();
            p.contains("test") || p.contains("spec")
        })
        .take(5)
        .cloned()
        .collect();

    // Config hints (path heuristic)
    let config_hints: Vec<String> = primary_files
        .iter()
        .filter(|file| {
            let p = file.to_lowercase();
            p.contains("config") || p.ends_with(".toml") || p.ends_with(".json")
        })
        .take(5)
        .cloned()
        .collect();

    let orientation_summary = if let Some(top_file) = primary_files.first() {
        let symbol_part = entry_symbols
            .first()
            .map(|symbol| format!("Entry symbol `{symbol}`"))
            .unwrap_or_else(|| "No exported entry symbol was extracted".to_string());
        let related_part = if dependency_symbols.is_empty() {
            format!("Graph state is {graph_health}; no dependency symbols were reported.")
        } else {
            format!(
                "Graph state is {graph_health}; related symbols include {}.",
                dependency_symbols
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        format!(
            "{symbol_part} is oriented around {top_file}. {} primary file(s) matched. {related_part}",
            primary_files.len()
        )
    } else {
        format!("No primary files matched `{query}`; graph state is {graph_health}.")
    };

    let latency_ms = start.elapsed().as_millis() as f64;

    let result = serde_json::json!({
        "primary_files": primary_files,
        "entry_symbols": entry_symbols,
        "dependency_symbols": dependency_symbols,
        "test_hints": test_hints,
        "config_hints": config_hints,
        "orientation_summary": orientation_summary,
        "graph": {
            "health": graph_health,
        },
        "latency_ms": latency_ms,
    });

    let mut extras = serde_json::Map::new();
    extras.insert("orient_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

fn run_public_search(req: &RawRequest, ctx: &AppContext, query: &str, top_k: usize) -> Response {
    let search_req = RawRequest {
        id: format!("{}:aft_orient_search", req.id),
        command: "semantic_search".to_string(),
        lsp_hints: req.lsp_hints.clone(),
        session_id: req.session_id.clone(),
        params: serde_json::json!({
            "query": query,
            "top_k": top_k,
            "profile": "agent_fast",
        }),
    };
    crate::commands::semantic_search::handle_semantic_search(&search_req, ctx)
}

fn entry_symbols_for_files(
    ctx: &AppContext,
    query: &str,
    files: &[String],
    limit: usize,
) -> Vec<String> {
    let query_lower = query.to_ascii_lowercase();
    let mut seen = BTreeSet::new();
    let mut preferred = Vec::new();
    let mut fallback = Vec::new();

    for file in files {
        let path = PathBuf::from(file);
        let Ok(symbols) = ctx.provider().list_symbols(&path) else {
            continue;
        };
        for symbol in symbols {
            let rendered = render_symbol(&symbol);
            if !seen.insert(rendered.clone()) {
                continue;
            }
            if symbol.name.to_ascii_lowercase().contains(&query_lower)
                || query_lower.contains(&symbol.name.to_ascii_lowercase())
            {
                preferred.push(rendered);
            } else {
                fallback.push(rendered);
            }
        }
    }

    preferred.into_iter().chain(fallback).take(limit).collect()
}

fn render_symbol(symbol: &Symbol) -> String {
    if symbol.scope_chain.is_empty() {
        symbol.name.clone()
    } else {
        format!("{}::{}", symbol.scope_chain.join("::"), symbol.name)
    }
}

fn graph_orientation_facts(
    ctx: &AppContext,
    files: &[String],
    symbols: &[String],
    depth: usize,
) -> (String, Vec<String>) {
    let config = ctx.config();
    if !config.callgraph_store || !config.intelligence.graph.enabled {
        return ("disabled".to_string(), Vec::new());
    }
    drop(config);

    let store = match ctx.callgraph_store_for_ops() {
        CallgraphStoreAccess::Ready(store) => store,
        CallgraphStoreAccess::Building | CallgraphStoreAccess::Unavailable => {
            return ("cold".to_string(), Vec::new())
        }
        CallgraphStoreAccess::Error(_) => return ("corrupt".to_string(), Vec::new()),
    };

    let mut related = BTreeSet::new();
    for file in files.iter().take(5) {
        for symbol in symbols.iter().take(5) {
            let short_symbol = symbol.rsplit("::").next().unwrap_or(symbol);
            let Ok(nodes) = store.nodes_for(Path::new(file), short_symbol) else {
                continue;
            };
            for node in nodes.into_iter().take(3) {
                if let Ok(callers) = store.callers_of(Path::new(&node.file), &node.symbol, depth) {
                    for site in callers.callers.into_iter().take(5) {
                        related.insert(site.caller.symbol);
                    }
                }
                if let Ok(callees) = store.outgoing_calls_of(&node) {
                    for site in callees.into_iter().take(5) {
                        related.insert(site.target_symbol);
                    }
                }
            }
        }
    }

    (
        "healthy".to_string(),
        related.into_iter().take(10).collect(),
    )
}
