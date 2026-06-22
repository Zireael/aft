//! AFT Impact Delta — blast radius analysis for a symbol change.
//!
//! Returns callers affected, tests covering affected, blast radius,
//! mutation risk, and mutation risk factors for a proposed change.

use std::collections::BTreeSet;
use std::path::Path;

use crate::commands::callgraph_store_adapter::impact_result;
use crate::context::{AppContext, CallgraphStoreAccess};
use crate::mutation_risk::classify_mutation_risk;
use crate::protocol::{RawRequest, Response};

/// Handle the `aft_impact_delta` command.
pub fn handle_aft_impact_delta(req: &RawRequest, ctx: &AppContext) -> Response {
    let symbol = req
        .params
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let change_type = req
        .params
        .get("change_type")
        .and_then(|v| v.as_str())
        .unwrap_or("signature");
    let depth = req
        .params
        .get("depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .min(25) as usize;

    if symbol.is_empty() {
        return Response::error(&req.id, "invalid_request", "symbol is required");
    }

    let config = ctx.config();
    if !config.callgraph_store || !config.intelligence.graph.enabled {
        return degraded_impact_response(
            &req.id,
            symbol,
            change_type,
            "disabled",
            "callgraph_store or intelligence.graph is disabled",
        );
    }
    drop(config);

    let store = match ctx.callgraph_store_for_ops() {
        CallgraphStoreAccess::Ready(store) => store,
        CallgraphStoreAccess::Building => {
            return degraded_impact_response(
                &req.id,
                symbol,
                change_type,
                "building",
                "callgraph store is still building",
            )
        }
        CallgraphStoreAccess::Unavailable => {
            return degraded_impact_response(
                &req.id,
                symbol,
                change_type,
                "unavailable",
                "callgraph store is unavailable",
            )
        }
        CallgraphStoreAccess::Error(error) => {
            return degraded_impact_response(
                &req.id,
                symbol,
                change_type,
                "corrupt",
                &format!("callgraph store error: {error}"),
            )
        }
    };

    let target_file = match req.params.get("file").and_then(|value| value.as_str()) {
        Some(file) => match ctx.validate_path(&req.id, Path::new(file)) {
            Ok(path) => path,
            Err(response) => return response,
        },
        None => match resolve_symbol_file(&store, symbol) {
            Some(file) => file.into(),
            None => {
                return degraded_impact_response(
                    &req.id,
                    symbol,
                    change_type,
                    "healthy",
                    "symbol was not found in the callgraph store",
                )
            }
        },
    };

    let impact = match impact_result(&store, &target_file, symbol, depth) {
        Ok(impact) => impact,
        Err(error) => {
            return degraded_impact_response(
                &req.id,
                symbol,
                change_type,
                "healthy",
                &format!("impact query failed: {error}"),
            )
        }
    };

    let callers_affected = impact
        .callers
        .iter()
        .map(|caller| {
            serde_json::json!({
                "symbol": caller.caller_symbol,
                "file": caller.caller_file,
                "line": caller.line,
                "signature": caller.signature,
                "is_entry_point": caller.is_entry_point,
                "call_expression": caller.call_expression,
                "parameters": caller.parameters,
                "approximate": caller.approximate,
                "resolved_by": caller.resolved_by,
            })
        })
        .collect::<Vec<_>>();

    let callees = store
        .nodes_for(Path::new(&impact.file), &impact.symbol)
        .ok()
        .and_then(|nodes| nodes.into_iter().next())
        .and_then(|node| store.outgoing_calls_of(&node).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|site| {
            serde_json::json!({
                "symbol": site.target_symbol,
                "file": site.target_file,
                "line": site.line,
                "resolved": site.resolved,
                "provenance": site.provenance,
            })
        })
        .collect::<Vec<_>>();

    let mut affected_files = BTreeSet::new();
    for caller in &impact.callers {
        affected_files.insert(caller.caller_file.clone());
    }
    affected_files.insert(impact.file.clone());

    let tests_covering_affected = affected_files
        .iter()
        .filter(|file| {
            let lower = file.to_ascii_lowercase();
            lower.contains("test") || lower.contains("spec")
        })
        .cloned()
        .collect::<Vec<_>>();

    let risk = classify_mutation_risk(&impact.file, None, true);
    let graph_informed_risk = if impact.total_affected > 0 || !callees.is_empty() {
        "Medium"
    } else {
        risk.level.label()
    };
    let mutation_risk_factors = risk
        .reasons
        .iter()
        .map(|reason| {
            serde_json::json!({
                "code": reason.code,
                "message": reason.message,
                "weight": reason.weight,
            })
        })
        .chain(std::iter::once(serde_json::json!({
            "code": "callgraph_affected_callers",
            "message": format!("{} caller(s) are affected at depth {}", impact.total_affected, depth),
            "weight": if impact.total_affected > 0 { 0.4 } else { 0.0 },
        })))
        .collect::<Vec<_>>();

    let blast_radius = serde_json::json!({
        "file_count": affected_files.len(),
        "symbol_count": impact.total_affected + 1,
        "test_count": tests_covering_affected.len(),
        "depth_limited": impact.depth_limited,
        "truncated": impact.truncated,
    });

    let result = serde_json::json!({
        "symbol": impact.symbol,
        "file": impact.file,
        "signature": impact.signature,
        "change_type": change_type,
        "callers_affected": callers_affected,
        "callees": callees,
        "tests_covering_affected": tests_covering_affected,
        "blast_radius": blast_radius,
        "mutation_risk": graph_informed_risk,
        "mutation_risk_factors": mutation_risk_factors,
        "graph": {
            "health": "healthy",
            "degraded_reason": null,
        },
    });

    let mut extras = serde_json::Map::new();
    extras.insert("impact_delta_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

fn resolve_symbol_file(
    store: &crate::callgraph_store::CallGraphStore,
    symbol: &str,
) -> Option<String> {
    let mut nodes = store.nodes_matching(symbol).ok()?;
    nodes.sort_by(|left, right| {
        let left_exact = left.name == symbol || left.symbol == symbol;
        let right_exact = right.name == symbol || right.symbol == symbol;
        right_exact
            .cmp(&left_exact)
            .then(left.file.cmp(&right.file))
            .then(left.line.cmp(&right.line))
    });
    nodes.into_iter().next().map(|node| node.file)
}

fn degraded_impact_response(
    id: &str,
    symbol: &str,
    change_type: &str,
    graph_health: &str,
    reason: &str,
) -> Response {
    let result = serde_json::json!({
        "symbol": symbol,
        "change_type": change_type,
        "callers_affected": [],
        "callees": [],
        "tests_covering_affected": [],
        "blast_radius": {
            "file_count": 0,
            "symbol_count": 0,
            "test_count": 0,
        },
        "mutation_risk": "Unavailable",
        "mutation_risk_factors": [],
        "graph": {
            "health": graph_health,
            "degraded_reason": reason,
        },
    });

    let mut extras = serde_json::Map::new();
    extras.insert("impact_delta_result".to_string(), result);
    Response::success(id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    use super::degraded_impact_response;

    #[test]
    fn degraded_response_reports_reason() {
        let response = degraded_impact_response("1", "test", "signature", "disabled", "off");
        assert_eq!(
            response.data["impact_delta_result"]["graph"]["degraded_reason"],
            "off"
        );
    }
}
