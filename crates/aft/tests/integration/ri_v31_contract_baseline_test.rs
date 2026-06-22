use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::helpers::AftProcess;

fn setup_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create project dir");
    let source_file = project.path().join("src/lib.rs");
    fs::create_dir_all(source_file.parent().expect("source parent")).expect("create source dir");
    fs::write(
        &source_file,
        r#"
pub struct SemanticBackendConfig {
    pub model: String,
}

pub fn build_semantic_backend_config() -> SemanticBackendConfig {
    SemanticBackendConfig { model: "local".to_string() }
}

pub fn needle_symbol() -> &'static str {
    "needle_symbol target"
}

pub fn call_needle_symbol() -> &'static str {
    needle_symbol()
}
"#,
    )
    .expect("write source file");
    project
}

fn setup_context_budget_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create project dir");
    let src_dir = project.path().join("src");
    fs::create_dir_all(&src_dir).expect("create source dir");
    for index in 0..30 {
        fs::write(
            src_dir.join(format!("budget_{index:02}.rs")),
            format!(
                r#"
pub fn budget_needle_{index:02}() -> &'static str {{
    "BudgetNeedle target {index:02}"
}}
"#
            ),
        )
        .expect("write budget fixture");
    }
    project
}

fn setup_ranking_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create project dir");
    let src_dir = project.path().join("src");
    let tests_dir = project.path().join("tests");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&tests_dir).expect("create tests dir");
    fs::write(
        src_dir.join("candidate_entry_definition.rs"),
        r#"
/// CandidateEntry represents a retrieval candidate definition.
pub struct CandidateEntry {
    pub file: String,
}
"#,
    )
    .expect("write definition fixture");
    fs::write(
        src_dir.join("candidate_entry_reference.rs"),
        r#"
pub fn use_candidate_entry(value: CandidateEntry) -> String {
    value.file
}
"#,
    )
    .expect("write reference fixture");
    fs::write(
        tests_dir.join("candidate_entry_test.rs"),
        r#"
#[test]
fn candidate_entry_fixture_mentions_candidate_entry() {
    let _name = "CandidateEntry";
}
"#,
    )
    .expect("write test fixture");
    fs::write(
        tests_dir.join("diagnostic_import_test.rs"),
        r#"
#[test]
fn keeps_test_diagnostic_context() {
    let _diagnostic = "E0433 unresolved import";
}
"#,
    )
    .expect("write diagnostic test fixture");
    fs::write(
        src_dir.join("diagnostic_reference.rs"),
        r#"
pub fn diagnostic_reference() -> &'static str {
    "E0433 unresolved import"
}
"#,
    )
    .expect("write diagnostic source fixture");
    project
}

fn send(aft: &mut AftProcess, request: Value) -> Value {
    aft.send(&serde_json::to_string(&request).expect("serialize request"))
}

fn aft_binary() -> PathBuf {
    std::env::var_os("AFT_TEST_AFT_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_aft")))
}

fn configure_contract_project(aft: &mut AftProcess, project_root: &Path, storage_dir: &Path) {
    configure_contract_project_with_ri(aft, project_root, storage_dir, None);
}

fn configure_contract_project_with_ri(
    aft: &mut AftProcess,
    project_root: &Path,
    storage_dir: &Path,
    retrieval_intelligence_v2: Option<bool>,
) {
    configure_contract_project_with_options(
        aft,
        project_root,
        storage_dir,
        retrieval_intelligence_v2,
        true,
    );
}

fn configure_contract_project_with_options(
    aft: &mut AftProcess,
    project_root: &Path,
    storage_dir: &Path,
    retrieval_intelligence_v2: Option<bool>,
    fts5_enabled: bool,
) {
    let mut request = json!({
        "id": "cfg-ri-v31-contract",
        "command": "configure",
        "harness": "opencode",
        "project_root": project_root.display().to_string(),
        "storage_dir": storage_dir.display().to_string(),
        "search_index": true,
        "semantic_search": false,
        "fts5": {
            "enabled": fts5_enabled,
            "auto_index": fts5_enabled,
            "index_on_start": fts5_enabled
        }
    });

    if let Some(enabled) = retrieval_intelligence_v2 {
        request["intelligence"] = json!({
            "retrieval_intelligence_v2": enabled
        });
    }

    let response = send(aft, request);

    assert_eq!(
        response["success"], true,
        "configure must succeed before contract assertions run: {response:?}"
    );
}

fn configure_contract_project_with_callgraph(
    aft: &mut AftProcess,
    project_root: &Path,
    storage_dir: &Path,
    retrieval_intelligence_v2: bool,
    callgraph_store: bool,
) {
    let response = send(
        aft,
        json!({
            "id": "cfg-ri-v31-graph",
            "command": "configure",
            "harness": "opencode",
            "project_root": project_root.display().to_string(),
            "storage_dir": storage_dir.display().to_string(),
            "search_index": true,
            "semantic_search": false,
            "callgraph_store": callgraph_store,
            "fts5": {
                "enabled": false,
                "auto_index": false,
                "index_on_start": false
            },
            "intelligence": {
                "retrieval_intelligence_v2": retrieval_intelligence_v2
            }
        }),
    );

    assert_eq!(
        response["success"], true,
        "configure with graph settings must succeed before contract assertions run: {response:?}"
    );
}

fn configure_contract_project_with_raw_telemetry(
    aft: &mut AftProcess,
    project_root: &Path,
    storage_dir: &Path,
) {
    let response = send(
        aft,
        json!({
            "id": "cfg-ri-v31-raw-telemetry",
            "command": "configure",
            "harness": "opencode",
            "project_root": project_root.display().to_string(),
            "storage_dir": storage_dir.display().to_string(),
            "search_index": true,
            "semantic_search": false,
            "fts5": {
                "enabled": false,
                "auto_index": false,
                "index_on_start": false
            },
            "intelligence": {
                "retrieval_intelligence_v2": true,
                "telemetry": {
                    "telemetry_store_query": "raw"
                }
            }
        }),
    );

    assert_eq!(
        response["success"], true,
        "configure with raw telemetry must succeed before contract assertions run: {response:?}"
    );
}

fn configure_contract_project_with_rerank(
    aft: &mut AftProcess,
    project_root: &Path,
    storage_dir: &Path,
    retrieval_intelligence_v2: Option<bool>,
    fts5_enabled: bool,
    rerank_enabled: bool,
) {
    let mut request = json!({
        "id": "cfg-ri-v31-rerank",
        "command": "configure",
        "harness": "opencode",
        "project_root": project_root.display().to_string(),
        "storage_dir": storage_dir.display().to_string(),
        "search_index": true,
        "semantic_search": false,
        "semantic": {
            "rerank_enabled": rerank_enabled,
            "rerank_base_url": "http://127.0.0.1:9",
            "rerank_timeout_ms": 1,
            "rerank_max_candidates": 20
        },
        "fts5": {
            "enabled": fts5_enabled,
            "auto_index": fts5_enabled,
            "index_on_start": fts5_enabled
        }
    });

    if let Some(enabled) = retrieval_intelligence_v2 {
        request["intelligence"] = json!({
            "retrieval_intelligence_v2": enabled
        });
    }

    let response = send(aft, request);

    assert_eq!(
        response["success"], true,
        "configure with rerank must succeed before contract assertions run: {response:?}"
    );
}

fn assert_response_id(response: &Value, id: &str) {
    assert_eq!(
        response["id"], id,
        "public NDJSON requests must preserve the required id field: {response:?}"
    );
}

fn result_rank_ending_with(results: &[Value], suffix: &str) -> Option<usize> {
    results.iter().position(|result| {
        result["file"]
            .as_str()
            .is_some_and(|path| path.replace('\\', "/").ends_with(suffix))
    })
}

fn ranking_report_ending_with(response: &Value, suffix: &str) -> Option<Value> {
    response["retrieval_intelligence_provenance"]["ranking_features"]
        .as_array()?
        .iter()
        .find(|report| {
            report["file"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with(suffix))
        })
        .cloned()
}

fn assert_search_finds_fixture(aft: &mut AftProcess) -> Value {
    let response = send(
        aft,
        json!({
            "id": "search-fixture",
            "command": "semantic_search",
            "query": "needle_symbol",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "search-fixture");
    assert_eq!(
        response["success"], true,
        "public semantic_search should succeed for the fixture before diagnostic contract checks: {response:?}"
    );

    let results = response["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|result| result["file"]
            .as_str()
            .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))),
        "fixture sanity check failed; expected semantic_search to find src/lib.rs: {response:?}"
    );

    response
}

fn assert_search_plan_contract(plan: &Value) {
    for key in [
        "intent",
        "lane_weights",
        "mandatory_lanes",
        "suppressed_lanes",
        "prefetch",
        "fusion",
        "rerank",
        "context_budget",
        "diagnostics_level",
        "active_safety_lane",
        "feature_flag_state",
    ] {
        assert!(
            plan.get(key).is_some(),
            "search_plan_debug must expose '{key}': {plan:?}"
        );
    }

    assert!(
        plan["prefetch"]
            .as_array()
            .is_some_and(|prefetch| !prefetch.is_empty()),
        "search_plan_debug.prefetch must include runnable lane plans: {plan:?}"
    );
    assert_eq!(
        plan["feature_flag_state"], "On",
        "active RI v2 search plans must expose feature_flag_state=On: {plan:?}"
    );
}

// These tests intentionally assert the PRD contract through the public NDJSON
// path. Valid JSON is not enough: a placeholder response can be syntactically
// valid while still being useless to an agent trying to diagnose retrieval.

#[test]
fn retrieval_intelligence_env_flag_activates_public_search_plan_contract() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let ri_flag = std::ffi::OsStr::new("true");
    let mut aft = AftProcess::spawn_with_env(&[("RETRIEVAL_INTELLIGENCE_V2", ri_flag)]);

    configure_contract_project(&mut aft, project.path(), storage.path());

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-search-plan",
            "command": "semantic_search",
            "query": "SemanticBackendConfig",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-search-plan");
    assert_eq!(
        response["success"], true,
        "search should succeed: {response:?}"
    );

    let plan = response
        .get("search_plan_debug")
        .expect("RETRIEVAL_INTELLIGENCE_V2=true must activate RI v2 search_plan_debug");
    assert_search_plan_contract(plan);
}

#[test]
fn retrieval_intelligence_config_flag_activates_public_search_plan_contract() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_ri(&mut aft, project.path(), storage.path(), Some(true));

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-config-search-plan",
            "command": "semantic_search",
            "query": "SemanticBackendConfig",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-config-search-plan");
    assert_eq!(
        response["success"], true,
        "search should succeed: {response:?}"
    );

    let plan = response.get("search_plan_debug").expect(
        "intelligence.retrieval_intelligence_v2=true must activate RI v2 search_plan_debug",
    );
    assert_search_plan_contract(plan);
}

#[test]
fn retrieval_intelligence_trigram_safety_lane_returns_public_candidates_when_semantic_and_fts5_disabled(
) {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-trigram-safety",
            "command": "semantic_search",
            "query": "SemanticBackendConfig",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-trigram-safety");
    assert_eq!(
        response["success"], true,
        "trigram safety-lane search should succeed: {response:?}"
    );

    let plan = response
        .get("search_plan_debug")
        .expect("RI v2 trigram safety-lane search must expose search_plan_debug");
    assert_search_plan_contract(plan);
    assert_eq!(
        plan["active_safety_lane"], "TrigramBody",
        "FTS5 disabled with search_index ready should activate TrigramBody safety lane: {plan:?}"
    );

    let results = response["results"].as_array().expect("results array");
    assert!(
        results.iter().any(|result| result["file"]
            .as_str()
            .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))),
        "RI v2 trigram safety lane must return real public candidates from the existing lexical index: {response:?}"
    );

    let provenance = response
        .get("retrieval_intelligence_provenance")
        .expect("RI v2 trigram safety-lane search must expose retrieval provenance");
    let lanes = provenance["lane_contributions"]
        .as_array()
        .expect("lane_contributions array");
    assert!(
        lanes
            .iter()
            .any(|candidate| candidate["lanes"]
                .as_array()
                .is_some_and(|candidate_lanes| candidate_lanes
                    .iter()
                    .any(|lane| lane["lane"] == "TrigramBody"))),
        "RI v2 trigram safety lane must report TrigramBody provenance: {provenance:?}"
    );
}

#[test]
fn retrieval_intelligence_reports_fts5_degraded_lanes_when_store_unavailable() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        true,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-fts5-degraded",
            "command": "semantic_search",
            "query": "SemanticBackendConfig works",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-fts5-degraded");
    assert_eq!(
        response["success"], true,
        "RI v2 search should degrade instead of failing when FTS5 is configured but unavailable: {response:?}"
    );

    let provenance = response
        .get("retrieval_intelligence_provenance")
        .expect("RI v2 FTS5 degraded search must expose retrieval provenance");
    let degraded_lanes = provenance["degraded_lanes"]
        .as_array()
        .expect("degraded_lanes array");
    assert!(
        degraded_lanes
            .iter()
            .any(|lane| lane["lane"] == "FTS5Body" && lane["fallback_used"] == "TrigramBody"),
        "FTS5Body failure must be visible with the trigram fallback used: {provenance:?}"
    );
    assert!(
        provenance["lane_contributions"]
            .as_array()
            .is_some_and(|contributions| contributions.iter().any(|candidate| candidate["lanes"]
                .as_array()
                .is_some_and(|lanes| lanes.iter().any(|lane| lane["lane"] == "TrigramBody")))),
        "degraded FTS5 search must still report real TrigramBody candidate provenance: {provenance:?}"
    );
}

#[test]
fn retrieval_intelligence_returns_enriched_urfk_snippets_from_final_candidates() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-final-enriched-results",
            "command": "semantic_search",
            "query": "SemanticBackendConfig",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-final-enriched-results");
    assert_eq!(
        response["success"], true,
        "RI v2 search should return final enriched URFK results: {response:?}"
    );

    let results = response["results"].as_array().expect("results array");
    let fixture_result = results
        .iter()
        .find(|result| {
            result["file"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))
        })
        .expect("expected fixture result");
    assert_eq!(
        fixture_result["source"], "ri_v2",
        "public result source must reflect the RI v2 final pipeline: {response:?}"
    );
    assert!(
        fixture_result["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("pub struct SemanticBackendConfig")),
        "returned result must carry enriched source snippet from final URFK candidates, not stale fused output: {response:?}"
    );

    let provenance = response
        .get("retrieval_intelligence_provenance")
        .expect("RI v2 final result search must expose retrieval provenance");
    assert!(
        provenance.get("context_budget").is_some(),
        "provenance must include context budget/rerank skip diagnostics: {provenance:?}"
    );
}

#[test]
fn retrieval_intelligence_result_contract_exposes_canonical_provenance_only() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-output-contract",
            "command": "semantic_search",
            "query": "SemanticBackendConfig",
            "top_k": 5
        }),
    );
    assert_response_id(&response, "ri-v2-output-contract");
    assert_eq!(
        response["success"], true,
        "RI v2 search should succeed for output contract assertions: {response:?}"
    );

    let run_provenance = response
        .get("retrieval_intelligence_provenance")
        .expect("canonical run-level key must be retrieval_intelligence_provenance");
    assert!(
        response.get("urfk_provenance").is_none(),
        "stale urfk_provenance alias must not be emitted: {response:?}"
    );
    assert!(
        run_provenance.get("lane_contributions").is_some(),
        "run-level provenance must include lane contributions: {run_provenance:?}"
    );

    let results = response["results"].as_array().expect("results array");
    let fixture_result = results
        .iter()
        .find(|result| {
            result["file"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))
        })
        .expect("expected fixture result");
    let result_object = fixture_result.as_object().expect("result object");

    for key in [
        "provenance",
        "is_exact_hit",
        "exact_hit_floor_applied",
        "is_graph_expansion",
        "enrichment_state",
        "graph_context",
    ] {
        assert!(
            result_object.contains_key(key),
            "RI v2 result must expose per-result '{key}', not only debug extras: {fixture_result:?}"
        );
    }

    assert!(
        fixture_result["provenance"]["lanes"]
            .as_array()
            .is_some_and(
                |lanes| !lanes.is_empty() && lanes.iter().any(|lane| lane["lane"] == "TrigramBody")
            ),
        "per-result provenance must include contributing retrieval lanes: {fixture_result:?}"
    );
    assert!(
        fixture_result["is_exact_hit"].is_boolean(),
        "is_exact_hit must be machine-readable boolean: {fixture_result:?}"
    );
    assert!(
        fixture_result["exact_hit_floor_applied"].is_boolean(),
        "exact_hit_floor_applied must be machine-readable boolean: {fixture_result:?}"
    );
    assert_eq!(
        fixture_result["enrichment_state"], "enriched",
        "result enrichment_state must reflect actual context enrichment: {fixture_result:?}"
    );
}

#[test]
fn retrieval_intelligence_tiny_context_budget_skips_reranker_with_path_only_accounting() {
    let project = setup_context_budget_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_rerank(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
        true,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ri-v2-context-budget-tiny",
            "command": "semantic_search",
            "query": "BudgetNeedle",
            "top_k": 20,
            "profile": "agent_fast"
        }),
    );
    assert_response_id(&response, "ri-v2-context-budget-tiny");
    assert_eq!(
        response["success"], true,
        "RI v2 tiny-budget search should succeed without contacting the reranker: {response:?}"
    );

    assert_eq!(
        response["search_plan_debug"]["rerank"]["enabled"], true,
        "public semantic.rerank_enabled must flow into the RI SearchPlan: {response:?}"
    );

    let context_budget = &response["retrieval_intelligence_provenance"]["context_budget"];
    assert_eq!(
        context_budget["context_exhausted"], true,
        "tiny context budget must report exhaustion: {context_budget:?}"
    );
    assert_eq!(
        context_budget["reranker_skipped_reason"], "insufficient_enriched_ratio",
        "tiny context budget must skip reranker with explicit reason: {context_budget:?}"
    );
    assert_eq!(
        context_budget["path_only_reranker_input_count"], 0,
        "PathOnly candidates must never enter content reranker input: {context_budget:?}"
    );
    assert_eq!(
        context_budget["reranker_input_candidate_count"], 0,
        "reranker input must be empty when budget diagnostics skip reranking: {context_budget:?}"
    );

    let pool_size = context_budget["rerank_pool_size"]
        .as_u64()
        .expect("rerank_pool_size number");
    let enriched = context_budget["enriched_candidate_count"]
        .as_u64()
        .expect("enriched_candidate_count number");
    let unenriched = context_budget["unenriched_candidate_count"]
        .as_u64()
        .expect("unenriched_candidate_count number");
    let skipped = context_budget["skipped_candidate_count"]
        .as_u64()
        .expect("skipped_candidate_count number");
    assert_eq!(
        enriched + unenriched + skipped,
        pool_size,
        "context budget accounting must cover the full rerank pool: {context_budget:?}"
    );
    assert!(
        enriched > 0 && unenriched > 0,
        "tiny budget fixture must include both enriched and PathOnly candidates: {context_budget:?}"
    );

    let results = response["results"].as_array().expect("results array");
    assert!(
        results
            .iter()
            .any(|result| result["enrichment_state"] == "path_only"),
        "PathOnly candidates must remain visible in final RI results: {response:?}"
    );
    assert!(
        results
            .iter()
            .any(|result| result["enrichment_state"] == "enriched"),
        "tiny-budget result set must still include enriched candidates: {response:?}"
    );
}

#[test]
fn retrieval_intelligence_default_off_omits_public_search_plan_debug() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());

    let response = assert_search_finds_fixture(&mut aft);
    assert!(
        response.get("search_plan_debug").is_none(),
        "default search output must not expose RI v2 debug fields: {response:?}"
    );
}

#[test]
fn explain_search_reports_observed_scores_for_public_results() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());
    assert_search_finds_fixture(&mut aft);

    let response = send(
        &mut aft,
        json!({
            "id": "explain-real",
            "command": "explain_search",
            "query": "needle_symbol"
        }),
    );
    assert_response_id(&response, "explain-real");
    assert_eq!(
        response["success"], true,
        "explain_search should succeed: {response:?}"
    );

    let scores = response["explain_search_result"]["top_10_rrf_scores"]
        .as_array()
        .expect("top_10_rrf_scores array");
    assert!(
        !scores.is_empty(),
        "explain_search must report observed retrieval/fusion scores when semantic_search finds candidates: {response:?}"
    );
}

#[test]
fn why_missed_reports_present_file_candidate_from_public_search() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let expected_file = project.path().join("src/lib.rs");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());
    assert_search_finds_fixture(&mut aft);

    let response = send(
        &mut aft,
        json!({
            "id": "why-present",
            "command": "why_missed",
            "query": "needle_symbol",
            "expected_file": expected_file.display().to_string()
        }),
    );
    assert_response_id(&response, "why-present");
    assert_eq!(
        response["success"], true,
        "why_missed should succeed: {response:?}"
    );

    let result = &response["why_missed_result"];
    assert_eq!(
        result["was_in_candidate_pool"], true,
        "why_missed must inspect the real candidate pool for files that semantic_search can already return: {response:?}"
    );
    assert!(
        result["pool_rank_if_present"].as_u64().is_some()
            || result["final_rank_if_present"].as_u64().is_some(),
        "why_missed must expose candidate or final rank for a present file: {response:?}"
    );
}

#[test]
fn why_missed_reports_specific_stage_for_absent_file() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let expected_file = project.path().join("src/missing.rs");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());
    assert_search_finds_fixture(&mut aft);

    let response = send(
        &mut aft,
        json!({
            "id": "why-absent",
            "command": "why_missed",
            "query": "needle_symbol",
            "expected_file": expected_file.display().to_string()
        }),
    );
    assert_response_id(&response, "why-absent");
    assert_eq!(
        response["success"], true,
        "why_missed absent-file diagnostic should succeed: {response:?}"
    );

    let result = &response["why_missed_result"];
    assert_eq!(
        result["was_in_candidate_pool"], false,
        "absent file must not be reported as present: {response:?}"
    );
    assert!(
        result["miss_stage"]
            .as_str()
            .is_some_and(|stage| !stage.is_empty()),
        "why_missed must classify a concrete miss stage: {response:?}"
    );
    assert!(
        result["suggested_fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("top_k") || fix.contains("project_root")),
        "why_missed must return a stage-specific suggestion, not generic filler: {response:?}"
    );
}

#[test]
fn aft_context_pack_returns_non_empty_pack_for_public_hits() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());
    assert_search_finds_fixture(&mut aft);

    let response = send(
        &mut aft,
        json!({
            "id": "context-pack-real",
            "command": "aft_context_pack",
            "query": "needle_symbol",
            "token_budget": 512
        }),
    );
    assert_response_id(&response, "context-pack-real");
    assert_eq!(
        response["success"], true,
        "aft_context_pack should succeed: {response:?}"
    );

    let result = &response["context_pack_result"];
    assert!(
        result["tokens_used"].as_u64().is_some_and(|tokens| tokens > 0),
        "aft_context_pack must spend budget when relevant public search results exist: {response:?}"
    );
    assert!(
        result["pack"].as_array().is_some_and(|pack| !pack.is_empty()),
        "aft_context_pack must return pack items when relevant public search results exist: {response:?}"
    );
    assert!(
        !result["omission_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("placeholder"),
        "aft_context_pack must not report placeholder success: {response:?}"
    );
}

#[test]
fn aft_orient_returns_real_files_instead_of_unknown_placeholder() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project(&mut aft, project.path(), storage.path());
    assert_search_finds_fixture(&mut aft);

    let response = send(
        &mut aft,
        json!({
            "id": "orient-real",
            "command": "aft_orient",
            "query": "needle_symbol",
            "depth": 2
        }),
    );
    assert_response_id(&response, "orient-real");
    assert_eq!(
        response["success"], true,
        "aft_orient should succeed: {response:?}"
    );

    let result = &response["orient_result"];
    assert!(
        result["primary_files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs")))),
        "aft_orient must orient from real retrieval/symbol data when public search finds the file: {response:?}"
    );
    assert!(
        !result["orientation_summary"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown is implemented in unknown"),
        "aft_orient must not present a placeholder summary as success: {response:?}"
    );
}

#[test]
fn aft_impact_delta_returns_graph_backed_blast_radius_for_known_symbol() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_callgraph(&mut aft, project.path(), storage.path(), true, true);

    let response = send(
        &mut aft,
        json!({
            "id": "impact-delta-real",
            "command": "aft_impact_delta",
            "symbol": "needle_symbol",
            "change_type": "signature",
            "depth": 2
        }),
    );
    assert_response_id(&response, "impact-delta-real");
    assert_eq!(
        response["success"], true,
        "aft_impact_delta should succeed: {response:?}"
    );

    let result = &response["impact_delta_result"];
    assert_eq!(
        result["graph"]["health"], "healthy",
        "fixture callgraph store should be healthy for impact delta: {response:?}"
    );
    assert!(
        result["callers_affected"]
            .as_array()
            .is_some_and(|callers| callers.iter().any(|caller| caller["symbol"]
                .as_str()
                .is_some_and(|symbol| symbol.contains("call_needle_symbol")))),
        "aft_impact_delta must report real graph callers for a known function: {response:?}"
    );
    assert!(
        result["blast_radius"]["symbol_count"]
            .as_u64()
            .is_some_and(|count| count > 1),
        "aft_impact_delta must report non-empty blast-radius data: {response:?}"
    );
    assert!(
        result["mutation_risk"]
            .as_str()
            .is_some_and(|risk| risk != "Unknown"),
        "aft_impact_delta must not hardcode unknown mutation risk for healthy graph fixtures: {response:?}"
    );
}

#[test]
fn ranking_features_public_path_promotes_exact_definition_above_references() {
    let project = setup_ranking_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ranking-definition",
            "command": "semantic_search",
            "query": "CandidateEntry",
            "top_k": 10
        }),
    );
    assert_response_id(&response, "ranking-definition");
    assert_eq!(
        response["success"], true,
        "ranking feature public search should succeed: {response:?}"
    );

    let results = response["results"].as_array().expect("results array");
    let definition_rank = result_rank_ending_with(results, "src/candidate_entry_definition.rs")
        .expect("definition result should be present");
    let reference_rank = result_rank_ending_with(results, "src/candidate_entry_reference.rs")
        .expect("reference result should be present");
    assert!(
        definition_rank < reference_rank,
        "exact definition should rank above reference after production ranking features: {response:?}"
    );

    let report = ranking_report_ending_with(&response, "src/candidate_entry_definition.rs")
        .expect("definition ranking diagnostics should be present");
    assert!(
        report["applied"].as_array().is_some_and(|features| features
            .iter()
            .any(|feature| feature["feature"] == "exact_definition_boost")),
        "ranking diagnostics must expose exact_definition_boost on public result path: {response:?}"
    );
}

#[test]
fn ranking_features_public_path_keeps_test_files_for_diagnostic_queries() {
    let project = setup_ranking_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );

    let response = send(
        &mut aft,
        json!({
            "id": "ranking-diagnostic",
            "command": "semantic_search",
            "query": "E0433 unresolved import",
            "top_k": 10
        }),
    );
    assert_response_id(&response, "ranking-diagnostic");
    assert_eq!(
        response["success"], true,
        "diagnostic ranking public search should succeed: {response:?}"
    );

    let results = response["results"].as_array().expect("results array");
    assert!(
        result_rank_ending_with(results, "tests/diagnostic_import_test.rs").is_some(),
        "diagnostic queries must retain test-file context in public results: {response:?}"
    );

    let report = ranking_report_ending_with(&response, "tests/diagnostic_import_test.rs")
        .expect("diagnostic test ranking report should be present");
    assert!(
        report["applied"].as_array().is_some_and(|features| features
            .iter()
            .all(|feature| feature["feature"] != "test_example_penalty")),
        "test/example penalty must be disabled for DiagnosticError intent: {response:?}"
    );
}

#[test]
fn retrieval_intelligence_disabled_graph_returns_null_context() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_callgraph(
        &mut aft,
        project.path(),
        storage.path(),
        true,
        false,
    );

    let response = assert_search_finds_fixture(&mut aft);
    let results = response["results"].as_array().expect("results array");
    let fixture_result = results
        .iter()
        .find(|result| {
            result["file"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))
        })
        .expect("expected fixture result");
    assert_eq!(
        fixture_result["graph_context"],
        serde_json::Value::Null,
        "disabled graph must serialize graph_context as null, not empty object/string: {response:?}"
    );
}

#[test]
fn retrieval_intelligence_healthy_graph_populates_public_context() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_callgraph(&mut aft, project.path(), storage.path(), true, true);

    let response = assert_search_finds_fixture(&mut aft);
    let graph_health = response["retrieval_intelligence_provenance"]["graph"]["health"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        graph_health, "healthy",
        "fixture callgraph store should be ready/healthy in the public RI path: {response:?}"
    );

    let results = response["results"].as_array().expect("results array");
    let fixture_result = results
        .iter()
        .find(|result| {
            result["file"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with("src/lib.rs"))
        })
        .expect("expected fixture result");
    let graph_context = fixture_result
        .get("graph_context")
        .and_then(|value| value.as_object())
        .expect("healthy graph should populate graph_context object");
    assert_eq!(
        graph_context
            .get("graph_confidence")
            .and_then(|value| value.as_str()),
        Some("Healthy"),
        "graph_context must expose healthy graph confidence: {fixture_result:?}"
    );
    assert!(
        graph_context
            .get("mutation_risk")
            .and_then(|value| value.as_str())
            .is_some_and(|risk| !risk.is_empty() && risk != "Unknown"),
        "graph_context must expose direct graph-derived risk facts: {fixture_result:?}"
    );
}

#[test]
fn semantic_search_persists_retrieval_telemetry_rows_through_public_path() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_options(
        &mut aft,
        project.path(),
        storage.path(),
        Some(true),
        false,
    );
    assert_search_finds_fixture(&mut aft);

    let status = aft.shutdown();
    assert!(status.success());

    let db_path = storage.path().join("aft.db");
    let conn = Connection::open(&db_path).expect("open configured aft.db");
    let run_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM retrieval_runs", [], |row| row.get(0))
        .expect("retrieval telemetry schema must be initialized through public search execution");
    assert!(
        run_count > 0,
        "semantic_search must persist retrieval_runs rows through the public path"
    );

    let (query_hash, query_raw): (String, Option<String>) = conn
        .query_row(
            "SELECT query_hash, query_raw FROM retrieval_runs ORDER BY CAST(timestamp AS INTEGER) DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("latest retrieval telemetry row should be readable");
    assert!(
        !query_hash.is_empty(),
        "default telemetry must populate query_hash"
    );
    assert_eq!(
        query_raw, None,
        "default telemetry mode must not persist raw query text"
    );

    let candidate_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM candidate_scores", [], |row| {
            row.get(0)
        })
        .expect("candidate telemetry should be queryable");
    assert!(
        candidate_count > 0,
        "RI v2 search must write candidate score telemetry through the public path"
    );
    let fusion_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM fusion_scores", [], |row| row.get(0))
        .expect("fusion telemetry should be queryable");
    assert!(
        fusion_count > 0,
        "RI v2 search must write fusion score telemetry through the public path"
    );

    let telemetry_schema: String = conn
        .query_row(
            "SELECT group_concat(sql, '\n') FROM sqlite_master WHERE type = 'table' AND name IN ('retrieval_runs', 'candidate_scores', 'fusion_scores')",
            [],
            |row| row.get(0),
        )
        .expect("telemetry schema should be inspectable");
    let telemetry_schema = telemetry_schema.to_ascii_lowercase();
    assert!(
        !telemetry_schema.contains("snippet") && !telemetry_schema.contains("body"),
        "telemetry tables must not include snippet/body persistence columns: {telemetry_schema}"
    );
}

#[test]
fn semantic_search_raw_query_telemetry_requires_explicit_opt_in() {
    let project = setup_project();
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    configure_contract_project_with_raw_telemetry(&mut aft, project.path(), storage.path());
    let query = "needle_symbol";
    let response = send(
        &mut aft,
        json!({
            "id": "search-raw-telemetry",
            "command": "semantic_search",
            "query": query,
            "top_k": 5
        }),
    );
    assert_response_id(&response, "search-raw-telemetry");
    assert_eq!(
        response["success"], true,
        "raw telemetry opt-in search should succeed: {response:?}"
    );

    let status = aft.shutdown();
    assert!(status.success());

    let conn = Connection::open(storage.path().join("aft.db")).expect("open configured aft.db");
    let query_raw: Option<String> = conn
        .query_row(
            "SELECT query_raw FROM retrieval_runs ORDER BY CAST(timestamp AS INTEGER) DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("latest retrieval telemetry row should be readable");
    assert_eq!(
        query_raw.as_deref(),
        Some(query),
        "raw query telemetry must be stored only after explicit opt-in"
    );
}

#[test]
fn telemetry_prune_command_removes_rows_older_than_retention() {
    let storage = tempfile::tempdir().expect("create storage dir");
    let db_path = storage.path().join("aft.db");
    let conn = Connection::open(&db_path).expect("open telemetry db");
    aft::telemetry::init_telemetry_schema(&conn).expect("init telemetry schema");
    conn.execute(
        "INSERT INTO retrieval_runs (
            run_id, query_hash, query_raw, query_kind, timestamp,
            latency_ms, profile, backend_config, context_exhausted, reranker_skipped_reason
        ) VALUES ('old-run', 'hash', NULL, 'identifier', '1', 0.0, 'agent_fast', '{}', 0, NULL)",
        [],
    )
    .expect("insert old run");
    conn.execute(
        "INSERT INTO candidate_scores (
            run_id, chunk_id, source_lane, raw_rank, raw_score,
            normalized_score, is_exact_hit, exact_hit_floor_applied
        ) VALUES ('old-run', 'src/lib.rs:1-1', 'TrigramBody', 0, 1.0, 1.0, 1, 0)",
        [],
    )
    .expect("insert old candidate score");
    conn.execute(
        "INSERT INTO fusion_scores (
            run_id, chunk_id, rrf_score, exact_hit_floor_applied, final_score, provenance_json
        ) VALUES ('old-run', 'src/lib.rs:1-1', 1.0, 0, 1.0, '{}')",
        [],
    )
    .expect("insert old fusion score");
    drop(conn);

    let output = Command::new(aft_binary())
        .arg("telemetry")
        .arg("prune")
        .arg("--storage-dir")
        .arg(storage.path())
        .arg("--retention-days")
        .arg("1")
        .output()
        .expect("run telemetry prune command");
    assert!(
        output.status.success(),
        "telemetry prune command should succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let conn = Connection::open(&db_path).expect("reopen telemetry db");
    let remaining_runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM retrieval_runs", [], |row| row.get(0))
        .expect("count remaining retrieval runs");
    let remaining_candidates: i64 = conn
        .query_row("SELECT COUNT(*) FROM candidate_scores", [], |row| {
            row.get(0)
        })
        .expect("count remaining candidate scores");
    let remaining_fusion: i64 = conn
        .query_row("SELECT COUNT(*) FROM fusion_scores", [], |row| row.get(0))
        .expect("count remaining fusion scores");
    assert_eq!(remaining_runs, 0, "old retrieval run should be pruned");
    assert_eq!(
        remaining_candidates, 0,
        "candidate scores for pruned run should be removed"
    );
    assert_eq!(
        remaining_fusion, 0,
        "fusion scores for pruned run should be removed"
    );
}
