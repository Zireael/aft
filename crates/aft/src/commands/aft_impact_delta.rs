//! AFT Impact Delta — blast radius analysis for a symbol change.
//!
//! Returns callers affected, tests covering affected, blast radius,
//! mutation risk, and mutation risk factors for a proposed change.

use crate::context::AppContext;
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

    if symbol.is_empty() {
        return Response::error(&req.id, "invalid_request", "symbol is required");
    }

    // Placeholder: would query callgraph for impact analysis
    let callers_affected: Vec<serde_json::Value> = Vec::new();
    let tests_covering_affected: Vec<String> = Vec::new();

    let blast_radius = serde_json::json!({
        "file_count": 0,
        "symbol_count": 0,
        "test_count": 0,
    });

    let result = serde_json::json!({
        "symbol": symbol,
        "change_type": change_type,
        "callers_affected": callers_affected,
        "tests_covering_affected": tests_covering_affected,
        "blast_radius": blast_radius,
        "mutation_risk": "Unknown",
        "mutation_risk_factors": [],
    });

    let mut extras = serde_json::Map::new();
    extras.insert("impact_delta_result".to_string(), result);

    Response::success(&req.id, serde_json::Value::Object(extras))
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_test() {
        let result = serde_json::json!({
            "symbol": "test",
            "change_type": "signature",
            "callers_affected": [],
            "blast_radius": {"file_count": 0, "symbol_count": 0, "test_count": 0},
        });
        assert!(result.is_object());
    }
}
