use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aft::config::Config;
use aft::inspect::scanners::dead_code::run_dead_code_scan;
use aft::inspect::{
    CallgraphExport, CallgraphOutboundCall, CallgraphSnapshot, InspectCategory, InspectJob,
    InspectScanSuccess, JobKey,
};
use aft::parser::SymbolCache;
use serde_json::json;

fn fixture_project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project root");

    let paths = files
        .iter()
        .map(|(relative, contents)| {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, contents).expect("write fixture file");
            path
        })
        .collect::<Vec<_>>();

    (temp_dir, root, paths)
}

fn job(
    root: &Path,
    scope_files: Vec<PathBuf>,
    callgraph_snapshot: Option<CallgraphSnapshot>,
) -> InspectJob {
    InspectJob {
        job_id: 1,
        key: JobKey::for_project_category(InspectCategory::DeadCode),
        category: InspectCategory::DeadCode,
        scope_files,
        project_root: root.to_path_buf(),
        inspect_dir: root.join(".aft-cache").join("inspect"),
        config: Arc::new(Config {
            project_root: Some(root.to_path_buf()),
            ..Config::default()
        }),
        symbol_cache: Arc::new(RwLock::new(SymbolCache::new())),
        callgraph_snapshot: callgraph_snapshot.map(Arc::new),
    }
}

fn snapshot(
    files: Vec<PathBuf>,
    exported_symbols: Vec<CallgraphExport>,
    outbound_calls: Vec<CallgraphOutboundCall>,
    entry_points: Vec<PathBuf>,
) -> CallgraphSnapshot {
    CallgraphSnapshot {
        generated_at: None,
        files,
        exported_symbols,
        outbound_calls,
        entry_points: entry_points.into_iter().collect::<BTreeSet<_>>(),
    }
}

fn export(root: &Path, file: &str, symbol: &str, kind: &str, line: u32) -> CallgraphExport {
    CallgraphExport {
        file: root.join(file),
        symbol: symbol.to_string(),
        kind: kind.to_string(),
        line,
    }
}

fn outbound(root: &Path, caller_file: &str, target: &str, line: u32) -> CallgraphOutboundCall {
    CallgraphOutboundCall {
        caller_file: root.join(caller_file),
        target: target.to_string(),
        line,
    }
}

fn target(root: &Path, file: &str, symbol: &str) -> String {
    format!("{}::{symbol}", root.join(file).display())
}

fn scan(job: InspectJob) -> InspectScanSuccess {
    run_dead_code_scan(&job).outcome.expect("scan succeeds")
}

#[test]
fn inspect_dead_code_unavailable_callgraph_returns_empty_result() {
    let (_temp_dir, root, paths) = fixture_project(&[("src/foo.ts", "export function foo() {}\n")]);

    let success = scan(job(&root, paths, None));

    assert!(success.contributions.is_empty());
    assert_eq!(success.aggregate["count"], 0);
    assert_eq!(success.aggregate["by_language"], json!({}));
    assert_eq!(success.aggregate["callgraph_available"], false);
    assert_eq!(success.aggregate["drill_down_capped"], false);
}

#[test]
fn inspect_dead_code_reports_exported_uncalled_function() {
    let (_temp_dir, root, paths) =
        fixture_project(&[("src/foo.ts", "export function unused() {}\n")]);
    let graph = snapshot(
        paths.clone(),
        vec![export(&root, "src/foo.ts", "unused", "function", 1)],
        Vec::new(),
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 1);
    assert_eq!(success.aggregate["by_language"]["typescript"], 1);
    assert_eq!(
        success.aggregate["items"].as_array().expect("items").len(),
        1
    );
    assert_eq!(
        success.aggregate["items"][0],
        json!({"file": "src/foo.ts", "symbol": "unused", "kind": "function", "line": 1})
    );
}

#[test]
fn inspect_dead_code_does_not_report_export_reachable_from_entry_point() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("src/foo.ts", "export function used() {}\n"),
        ("src/main.ts", "export function main() {\n  used();\n}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/foo.ts", "used", "function", 1),
            export(&root, "src/main.ts", "main", "function", 1),
        ],
        vec![outbound(
            &root,
            "src/main.ts",
            &target(&root, "src/foo.ts", "used"),
            2,
        )],
        vec![root.join("src/main.ts")],
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
    assert!(success.aggregate["items"]
        .as_array()
        .expect("items")
        .is_empty());
}

#[test]
fn inspect_dead_code_keeps_multi_hop_entry_point_reachability_alive() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("src/entry.ts", "export function entry() {\n  b();\n}\n"),
        ("src/b.ts", "export function b() {\n  c();\n}\n"),
        ("src/c.ts", "export function c() {}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/entry.ts", "entry", "function", 1),
            export(&root, "src/b.ts", "b", "function", 1),
            export(&root, "src/c.ts", "c", "function", 1),
        ],
        vec![
            outbound(&root, "src/entry.ts", &target(&root, "src/b.ts", "b"), 2),
            outbound(&root, "src/b.ts", &target(&root, "src/c.ts", "c"), 2),
        ],
        vec![root.join("src/entry.ts")],
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_keeps_same_name_exports_distinct() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("src/entry.ts", "export function main() {\n  foo();\n}\n"),
        ("src/dead.ts", "export function foo() {}\n"),
        ("src/alive.ts", "export function foo() {}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/entry.ts", "main", "function", 1),
            export(&root, "src/dead.ts", "foo", "function", 1),
            export(&root, "src/alive.ts", "foo", "function", 1),
        ],
        vec![outbound(
            &root,
            "src/entry.ts",
            &target(&root, "src/alive.ts", "foo"),
            2,
        )],
        vec![root.join("src/entry.ts")],
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 1);
    assert_eq!(
        success.aggregate["items"][0],
        json!({"file": "src/dead.ts", "symbol": "foo", "kind": "function", "line": 1})
    );
}

#[test]
fn inspect_dead_code_reports_unreachable_cycle_exports() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("src/a.ts", "export function a() {\n  b();\n}\n"),
        ("src/b.ts", "export function b() {\n  a();\n}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/a.ts", "a", "function", 1),
            export(&root, "src/b.ts", "b", "function", 1),
        ],
        vec![
            outbound(&root, "src/a.ts", &target(&root, "src/b.ts", "b"), 2),
            outbound(&root, "src/b.ts", &target(&root, "src/a.ts", "a"), 2),
        ],
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    let dead_symbols = success.aggregate["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().expect("file").to_string(),
                item["symbol"].as_str().expect("symbol").to_string(),
            )
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(success.aggregate["count"], 2);
    assert_eq!(success.aggregate["by_language"]["typescript"], 2);
    assert_eq!(
        dead_symbols,
        BTreeSet::from([
            ("src/a.ts".to_string(), "a".to_string()),
            ("src/b.ts".to_string(), "b".to_string()),
        ])
    );
}

#[test]
fn inspect_dead_code_does_not_report_entry_point_exports() {
    let (_temp_dir, root, paths) =
        fixture_project(&[("src/main.ts", "export function main() {}\n")]);
    let graph = snapshot(
        paths.clone(),
        vec![export(&root, "src/main.ts", "main", "function", 1)],
        Vec::new(),
        vec![root.join("src/main.ts")],
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_does_not_report_package_json_main_export() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("package.json", "{\"main\":\"src/public.ts\"}\n"),
        ("src/public.ts", "export function publicApi() {}\n"),
    ]);
    let source_files = vec![root.join("src/public.ts")];
    let graph = snapshot(
        source_files.clone(),
        vec![export(&root, "src/public.ts", "publicApi", "function", 1)],
        Vec::new(),
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_resolves_extensionless_package_json_main_export() {
    let (_temp_dir, root, paths) = fixture_project(&[
        ("package.json", "{\"main\":\"src/index\"}\n"),
        ("src/index.ts", "export function publicApi() {}\n"),
    ]);
    let source_files = vec![root.join("src/index.ts")];
    let graph = snapshot(
        source_files,
        vec![export(&root, "src/index.ts", "publicApi", "function", 1)],
        Vec::new(),
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 0);
}

#[test]
fn inspect_dead_code_caps_drill_down_after_one_hundred_items() {
    let source = (0..101)
        .map(|index| format!("export function unused_{index}() {{}}\n"))
        .collect::<String>();
    let (_temp_dir, root, paths) = fixture_project(&[("src/many.ts", &source)]);
    let exports = (0..101)
        .map(|index| {
            export(
                &root,
                "src/many.ts",
                &format!("unused_{index}"),
                "function",
                index + 1,
            )
        })
        .collect::<Vec<_>>();
    let graph = snapshot(paths.clone(), exports, Vec::new(), Vec::new());

    let success = scan(job(&root, paths, Some(graph)));

    assert_eq!(success.aggregate["count"], 101);
    assert_eq!(success.aggregate["by_language"]["typescript"], 101);
    assert_eq!(
        success.aggregate["items"].as_array().expect("items").len(),
        100
    );
    assert_eq!(success.aggregate["drill_down_capped"], true);
}

#[test]
fn inspect_dead_code_contribution_shape_matches_contract() {
    let (_temp_dir, root, paths) = fixture_project(&[
        (
            "src/foo.ts",
            "export class Foo {}\nexport function helper() { return Bar(); }\n",
        ),
        ("src/bar.ts", "export function Bar() {}\n"),
    ]);
    let graph = snapshot(
        paths.clone(),
        vec![
            export(&root, "src/foo.ts", "Foo", "class", 1),
            export(&root, "src/foo.ts", "helper", "function", 2),
            export(&root, "src/bar.ts", "Bar", "function", 1),
        ],
        vec![
            outbound(&root, "src/foo.ts", &target(&root, "src/bar.ts", "Bar"), 2),
            outbound(&root, "src/foo.ts", "external_dependency", 3),
        ],
        Vec::new(),
    );

    let success = scan(job(&root, paths, Some(graph)));
    let contribution = success
        .contributions
        .iter()
        .find(|contribution| contribution.file_path == root.join("src/foo.ts"))
        .expect("foo contribution");

    assert_eq!(
        contribution.contribution,
        json!({
            "file": "src/foo.ts",
            "exports": [
                {"symbol": "Foo", "kind": "class", "line": 1, "is_entry_point": false},
                {"symbol": "helper", "kind": "function", "line": 2, "is_entry_point": false}
            ],
            "internal_calls": [
                {"file": "src/bar.ts", "symbol": "Bar", "line": 2}
            ]
        })
    );
}
