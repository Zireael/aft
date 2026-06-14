//! Integration tests for FTS5 commands through the binary protocol.
//!
//! These tests exercise the full command-loop: spawn aft → configure with
//! FTS5 enabled → index → search → find → read → doctor.

use std::fs;
use std::path::Path;

use serde_json::json;

use super::helpers::AftProcess;

fn configure_with_fts5(aft: &mut AftProcess, project: &Path, storage: &Path) {
    let response = aft.send(
        &json!({
            "id": "cfg-fts5",
            "command": "configure",
            "harness": "opencode",
            "project_root": project,
            "storage_dir": storage,
            "fts5": {
                "enabled": true,
                "max_results": 10,
            },
        })
        .to_string(),
    );
    assert_eq!(
        response["success"], true,
        "configure with fts5 failed: {response:?}"
    );
}

fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

// ---------------------------------------------------------------------------
// Index lifecycle
// ---------------------------------------------------------------------------

#[test]
fn fts5_index_status_empty_project() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    let resp = aft.send(
        &json!({
            "id": "fts5-index-status",
            "command": "fts5_index",
            "action": "status",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "fts5_index status failed: {resp:?}");
    assert_eq!(resp["exists"], false, "expected no index yet: {resp:?}");

    assert!(aft.shutdown().success());
}

#[test]
fn fts5_index_update_builds_index() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    // Write a small Rust file
    write_file(
        dir.path(),
        "src/lib.rs",
        r#"pub struct Config {
    pub name: String,
    pub enabled: bool,
}

pub fn get_config() -> Config {
    Config { name: "test".into(), enabled: true }
}
"#,
    );

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build the index
    let resp = aft.send(
        &json!({
            "id": "fts5-index-update",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "fts5_index update failed: {resp:?}");
    assert!(
        resp["files_processed"].as_i64().unwrap() >= 1,
        "expected at least 1 file processed: {resp:?}"
    );
    assert!(
        resp["symbols_extracted"].as_i64().unwrap() >= 2,
        "expected at least 2 symbols extracted: {resp:?}"
    );

    // Verify index exists now
    let resp = aft.send(
        &json!({
            "id": "fts5-index-status-after",
            "command": "fts5_index",
            "action": "status",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "status failed: {resp:?}");
    assert_eq!(resp["exists"], true, "index should exist: {resp:?}");
    assert!(
        resp["file_count"].as_i64().unwrap() >= 1,
        "expected at least 1 file: {resp:?}"
    );
    assert!(
        resp["symbol_count"].as_i64().unwrap() >= 2,
        "expected at least 2 symbols: {resp:?}"
    );
    // Verify text field is present
    assert!(
        resp["text"].as_str().unwrap_or("").contains("FTS5 Index"),
        "expected text field: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[test]
fn fts5_search_finds_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    write_file(
        dir.path(),
        "src/config.rs",
        r#"pub struct SemanticBackendConfig {
    pub backend: String,
    pub model: String,
}

pub fn get_backend_config() -> SemanticBackendConfig {
    SemanticBackendConfig {
        backend: "fastembed".into(),
        model: "all-MiniLM-L6-v2".into(),
    }
}
"#,
    );

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build index
    let resp = aft.send(
        &json!({
            "id": "fts5-index",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );
    assert_eq!(resp["success"], true, "index failed: {resp:?}");

    // Search for the symbol
    let resp = aft.send(
        &json!({
            "id": "fts5-search",
            "command": "fts5_search",
            "query": "SemanticBackendConfig",
            "scope": "symbols",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "search failed: {resp:?}");
    assert!(
        resp["total"].as_i64().unwrap() >= 1,
        "expected at least 1 result: {resp:?}"
    );

    // Verify results contain expected fields
    let results = resp["results"].as_array().unwrap();
    let first = &results[0];
    assert_eq!(first["symbol_name"], "SemanticBackendConfig");
    assert_eq!(first["symbol_kind"], "struct");
    assert!(first["file_path"].as_str().unwrap().contains("config.rs"));
    assert!(
        first["score"].as_f64().unwrap() > 0.0,
        "score should be positive: {resp:?}"
    );
    // Verify text field
    assert!(
        resp["text"].as_str().unwrap_or("").contains("FTS5 Search"),
        "expected text field: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

#[test]
fn fts5_search_empty_index_returns_warning() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    let resp = aft.send(
        &json!({
            "id": "fts5-search-empty",
            "command": "fts5_search",
            "query": "anything",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "search should succeed: {resp:?}");
    assert_eq!(resp["total"], 0, "expected 0 results: {resp:?}");
    assert!(
        resp["warning"].as_str().unwrap_or("").contains("empty"),
        "expected empty warning: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Find symbol
// ---------------------------------------------------------------------------

#[test]
fn fts5_find_symbol_exact_match() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    write_file(
        dir.path(),
        "src/lib.rs",
        r#"pub fn helper_function() -> i32 { 42 }
pub fn another_helper() -> String { "hello".into() }
"#,
    );

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build index
    aft.send(
        &json!({
            "id": "fts5-index",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );

    // Find exact symbol
    let resp = aft.send(
        &json!({
            "id": "fts5-find",
            "command": "fts5_find_symbol",
            "name": "helper_function",
            "mode": "exact",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "find failed: {resp:?}");
    assert!(
        resp["total"].as_i64().unwrap() >= 1,
        "expected at least 1 result: {resp:?}"
    );

    let results = resp["results"].as_array().unwrap();
    assert_eq!(results[0]["symbol_name"], "helper_function");
    assert_eq!(results[0]["symbol_kind"], "function");
    // Verify text field
    assert!(
        resp["text"]
            .as_str()
            .unwrap_or("")
            .contains("FTS5 Find Symbol"),
        "expected text field: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Read symbol
// ---------------------------------------------------------------------------

#[test]
fn fts5_read_symbol_returns_source() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    write_file(
        dir.path(),
        "src/lib.rs",
        r#"pub fn my_func() -> i32 {
    let x = 42;
    x + 1
}
"#,
    );

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build index
    aft.send(
        &json!({
            "id": "fts5-index",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );

    // Find symbol first to get its ID
    let find_resp = aft.send(
        &json!({
            "id": "fts5-find",
            "command": "fts5_find_symbol",
            "name": "my_func",
            "mode": "exact",
        })
        .to_string(),
    );
    assert_eq!(find_resp["success"], true, "find failed: {find_resp:?}");

    let symbol_id = find_resp["results"][0]["symbol_id"].as_i64().unwrap();

    // Read symbol by ID
    let resp = aft.send(
        &json!({
            "id": "fts5-read",
            "command": "fts5_read_symbol",
            "symbol_id": symbol_id,
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "read failed: {resp:?}");
    assert_eq!(resp["symbol_name"], "my_func");
    assert_eq!(resp["symbol_kind"], "function");
    assert!(
        resp["body"]
            .as_str()
            .unwrap_or("")
            .contains("pub fn my_func()"),
        "expected body to contain function: {resp:?}"
    );
    // Verify text field
    assert!(
        resp["text"]
            .as_str()
            .unwrap_or("")
            .contains("FTS5 Read Symbol"),
        "expected text field: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

#[test]
fn fts5_doctor_reports_health() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    write_file(dir.path(), "src/lib.rs", "pub fn foo() {}\n");

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build index
    aft.send(
        &json!({
            "id": "fts5-index",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );

    // Run doctor
    let resp = aft.send(
        &json!({
            "id": "fts5-doctor",
            "command": "fts5_doctor",
        })
        .to_string(),
    );

    assert_eq!(resp["success"], true, "doctor failed: {resp:?}");
    assert_eq!(resp["compiled"], true, "should be compiled: {resp:?}");
    assert_eq!(resp["enabled"], true, "should be enabled: {resp:?}");
    assert!(resp["config"].is_object(), "should have config: {resp:?}");
    assert!(resp["index"].is_object(), "should have index: {resp:?}");
    assert!(
        resp["index"]["exists"].as_bool().unwrap(),
        "index should exist: {resp:?}"
    );
    assert!(
        resp["index"]["file_count"].as_i64().unwrap() >= 1,
        "should have files: {resp:?}"
    );
    // Verify text field
    assert!(
        resp["text"].as_str().unwrap_or("").contains("FTS5 Doctor"),
        "expected text field: {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Regression: short identifiers
// ---------------------------------------------------------------------------

#[test]
fn fts5_search_short_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();

    write_file(
        dir.path(),
        "src/lib.rs",
        r#"pub fn process(items: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for item in items {
        if !item.is_empty() {
            result.push(item.clone());
        }
    }
    result
}
"#,
    );

    let mut aft = AftProcess::spawn();
    configure_with_fts5(&mut aft, dir.path(), storage.path());

    // Build index
    aft.send(
        &json!({
            "id": "fts5-index",
            "command": "fts5_index",
            "action": "update",
        })
        .to_string(),
    );

    // Search for "process" — should find the function
    let resp = aft.send(
        &json!({
            "id": "fts5-search-process",
            "command": "fts5_search",
            "query": "process",
        })
        .to_string(),
    );
    assert_eq!(resp["success"], true, "search failed: {resp:?}");
    assert!(
        resp["total"].as_i64().unwrap() >= 1,
        "should find 'process': {resp:?}"
    );

    // Search for "items" — should find the parameter
    let resp = aft.send(
        &json!({
            "id": "fts5-search-items",
            "command": "fts5_search",
            "query": "items",
        })
        .to_string(),
    );
    assert_eq!(resp["success"], true, "search failed: {resp:?}");
    assert!(
        resp["total"].as_i64().unwrap() >= 1,
        "should find 'items': {resp:?}"
    );

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Disabled state
// ---------------------------------------------------------------------------

#[test]
fn fts5_commands_return_disabled_when_not_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    // Configure WITHOUT fts5 enabled
    let response = aft.send(
        &json!({
            "id": "cfg-no-fts5",
            "command": "configure",
            "harness": "opencode",
            "project_root": dir.path(),
            "storage_dir": storage.path(),
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");

    // All FTS5 commands should return disabled error
    for (cmd, params) in [
        ("fts5_search", json!({"query": "test"})),
        ("fts5_index", json!({"action": "status"})),
        ("fts5_find_symbol", json!({"name": "foo"})),
        ("fts5_read_symbol", json!({"symbol_id": 1})),
        ("fts5_doctor", json!({})),
    ] {
        let resp = aft.send(
            &json!({
                "id": format!("disabled-{cmd}"),
                "command": cmd,
                "params": params,
            })
            .to_string(),
        );
        assert_eq!(
            resp["success"], false,
            "{cmd} should return disabled: {resp:?}"
        );
        assert_eq!(
            resp["code"], "fts5_disabled",
            "{cmd} should have fts5_disabled code: {resp:?}"
        );
    }

    assert!(aft.shutdown().success());
}
