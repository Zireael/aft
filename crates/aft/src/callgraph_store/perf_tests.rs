//! Work-counting measurements and lock-in tests for the call-graph store's
//! cold build and incremental refresh.
//!
//! The counts (files parsed, resolution-config files read, SQL statements
//! compiled) are deterministic, unlike wall time on a shared machine, so the
//! lock-in tests assert on them. The ignored real-repository measurement also
//! writes the graph rows and query outputs to a directory, so two builds of
//! the store can be compared for identical results.

use super::*;
use rusqlite::ffi;
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::AtomicUsize;

extern "C" fn count_statement_compilations(
    counter: *mut c_void,
    action: c_int,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> c_int {
    // SQLite calls the authorizer while it compiles a statement, never while
    // it runs one, so a statement served from the prepared-statement cache is
    // not counted. Only the top-level DML actions are counted, which gives one
    // count per compiled statement (a sub-select adds one more).
    if matches!(
        action,
        ffi::SQLITE_SELECT | ffi::SQLITE_INSERT | ffi::SQLITE_UPDATE | ffi::SQLITE_DELETE
    ) {
        // SAFETY: the pointer is a leaked `AtomicUsize` that lives for the
        // rest of the process (see `install_compile_counter`).
        unsafe { &*(counter as *const AtomicUsize) }.fetch_add(1, AtomicOrdering::Relaxed);
    }
    ffi::SQLITE_OK
}

/// Count every SQL statement compiled on the store's connection from now on.
pub(super) fn install_compile_counter(store: &CallGraphStore) -> &'static AtomicUsize {
    let counter: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let conn = store.conn.lock().expect("callgraph store mutex poisoned");
    // SAFETY: the handle belongs to a live connection and the callback's data
    // pointer is never freed.
    let rc = unsafe {
        ffi::sqlite3_set_authorizer(
            conn.handle(),
            Some(count_statement_compilations),
            counter as *const AtomicUsize as *mut c_void,
        )
    };
    assert_eq!(rc, ffi::SQLITE_OK, "install SQLite authorizer");
    counter
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct WorkCounts {
    pub parses: usize,
    pub max_parses_of_one_file: usize,
    pub package_json_reads: usize,
    pub tsconfig_reads: usize,
    pub cargo_toml_reads: usize,
    pub config_reads: usize,
    pub statements_compiled: usize,
    pub wall: Duration,
}

pub(super) fn measure<T>(
    root: &Path,
    compiles: &AtomicUsize,
    run: impl FnOnce() -> T,
) -> (T, WorkCounts) {
    work_counts::reset_under(root);
    let compiled_before = compiles.load(AtomicOrdering::Relaxed);
    let started = Instant::now();
    let value = run();
    let wall = started.elapsed();
    let counts = WorkCounts {
        parses: work_counts::parses_under(root),
        max_parses_of_one_file: work_counts::max_parses_of_one_file_under(root),
        package_json_reads: work_counts::config_reads_under(root, Some("package.json")),
        tsconfig_reads: work_counts::config_reads_under(root, Some("tsconfig.json")),
        cargo_toml_reads: work_counts::config_reads_under(root, Some("Cargo.toml")),
        config_reads: work_counts::config_reads_under(root, None),
        statements_compiled: compiles.load(AtomicOrdering::Relaxed) - compiled_before,
        wall,
    };
    (value, counts)
}

const GRAPH_TABLES: &[&str] = &[
    "nodes",
    "refs",
    "edges",
    "file_dependencies",
    "dispatch_hints",
    "type_ref_names",
];

/// Every graph row, sorted, with the fixture root replaced so two copies of
/// the same tree compare equal.
fn dump_graph_rows(store: &CallGraphStore) -> String {
    let root = store.project_root().display().to_string();
    let conn = store.conn.lock().expect("callgraph store mutex poisoned");
    let mut out = String::new();
    for table in GRAPH_TABLES {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table}"))
            .expect("prepare dump");
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                let mut values = Vec::with_capacity(columns);
                for index in 0..columns {
                    values.push(match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => "NULL".to_string(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Real(value) => value.to_string(),
                        rusqlite::types::ValueRef::Text(value) => {
                            String::from_utf8_lossy(value).into_owned()
                        }
                        rusqlite::types::ValueRef::Blob(value) => format!("{value:?}"),
                    });
                }
                Ok(values.join("\u{1f}"))
            })
            .expect("query dump")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read dump");
        rows.sort();
        for row in rows {
            out.push_str(table);
            out.push('\t');
            out.push_str(&row.replace(&root, "<root>"));
            out.push('\n');
        }
    }
    out
}

/// `callers`, `call_tree` and `trace_to` output for an evenly spaced sample
/// of the stored symbols.
fn dump_query_outputs(store: &CallGraphStore, samples: usize) -> String {
    let root = store.project_root().display().to_string();
    let symbols = {
        let conn = store.conn.lock().expect("callgraph store mutex poisoned");
        let mut statement = conn
            .prepare(
                "SELECT DISTINCT file_path, scoped_name FROM nodes ORDER BY file_path, scoped_name",
            )
            .expect("prepare symbols");
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query symbols")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read symbols")
    };
    let step = (symbols.len() / samples.max(1)).max(1);
    let mut out = String::new();
    for (file, symbol) in symbols.iter().step_by(step) {
        let path = Path::new(file);
        out.push_str(&format!("== {file} {symbol}\n"));
        out.push_str(&format!(
            "callers: {:?}\n",
            store.callers_of(path, symbol, 2)
        ));
        out.push_str(&format!(
            "call_tree: {:?}\n",
            store.call_tree(path, symbol, 2)
        ));
        out.push_str(&format!(
            "trace_to: {:?}\n",
            store.trace_to(path, symbol, 3)
        ));
    }
    out.replace(&root, "<root>")
}

fn copy_tracked_sources(source: &Path, destination: &Path) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["ls-files", "-z"])
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "git ls-files failed");
    for rel in output.stdout.split(|byte| *byte == 0) {
        if rel.is_empty() {
            continue;
        }
        let rel = Path::new(std::str::from_utf8(rel).expect("utf-8 path"));
        let wanted = crate::parser::detect_language(rel).is_some()
            || resolution_config::resolution_config_kind(rel).is_some()
            || rel
                .file_name()
                .is_some_and(|name| name == ".gitignore" || name == ".aftignore");
        let from = source.join(rel);
        if !wanted || !from.is_file() || from.is_symlink() {
            continue;
        }
        let to = destination.join(rel);
        fs::create_dir_all(to.parent().expect("parent")).expect("create parent");
        fs::copy(&from, &to).expect("copy source file");
    }
}

fn report(label: &str, counts: &WorkCounts) {
    eprintln!(
        "{label}: parses={} max_parses_of_one_file={} package_json_reads={} tsconfig_reads={} cargo_toml_reads={} config_reads={} statements_compiled={} wall_ms={}",
        counts.parses,
        counts.max_parses_of_one_file,
        counts.package_json_reads,
        counts.tsconfig_reads,
        counts.cargo_toml_reads,
        counts.config_reads,
        counts.statements_compiled,
        counts.wall.as_millis()
    );
}

/// Cold build and one incremental refresh of a copy of a real repository.
///
/// `AFT_CALLGRAPH_MEASURE_ROOT` names the repository, `AFT_CALLGRAPH_MEASURE_EDIT`
/// a repository-relative file that gets one exported function appended before
/// the refresh, and the optional `AFT_CALLGRAPH_MEASURE_DUMP` a directory for
/// the graph rows and query outputs after each step.
#[test]
#[ignore = "measurement: needs AFT_CALLGRAPH_MEASURE_ROOT and AFT_CALLGRAPH_MEASURE_EDIT"]
fn measure_cold_build_and_refresh_on_real_repo() {
    let source = PathBuf::from(std::env::var("AFT_CALLGRAPH_MEASURE_ROOT").expect("root"));
    let edit = PathBuf::from(std::env::var("AFT_CALLGRAPH_MEASURE_EDIT").expect("edit"));
    let dump = std::env::var_os("AFT_CALLGRAPH_MEASURE_DUMP").map(PathBuf::from);
    let samples = std::env::var("AFT_CALLGRAPH_MEASURE_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200usize);

    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("project");
    fs::create_dir_all(&root).expect("create root");
    let root = fs::canonicalize(root).expect("canonical root");
    copy_tracked_sources(&source, &root);
    let files: Vec<PathBuf> = callgraph::walk_project_files(&root).collect();
    eprintln!("files={}", files.len());

    let store = CallGraphStore::open(dir.path().join("store"), root.clone()).expect("open store");
    let compiles = install_compile_counter(&store);
    callgraph::clear_workspace_package_cache();
    let (stats, cold) = measure(&root, compiles, || store.cold_build(&files));
    let stats = stats.expect("cold build");
    report("cold_build", &cold);
    eprintln!("cold_build stats: {stats:?}");
    if let Some(dump) = &dump {
        fs::create_dir_all(dump).expect("create dump dir");
        fs::write(dump.join("cold_rows.txt"), dump_graph_rows(&store)).expect("write rows");
        fs::write(
            dump.join("cold_queries.txt"),
            dump_query_outputs(&store, samples),
        )
        .expect("write queries");
    }

    let edit_path = root.join(&edit);
    let addition = match edit_path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => "\npub fn aft_measure_added_function() {}\n",
        Some("py") => "\n\ndef aft_measure_added_function():\n    pass\n",
        _ => "\nexport function aftMeasureAddedFunction() {}\n",
    };
    let mut text = fs::read_to_string(&edit_path).expect("read edit file");
    text.push_str(addition);
    fs::write(&edit_path, text).expect("write edit file");

    let (refresh_stats, refresh) = measure(&root, compiles, || {
        store.refresh_files(std::slice::from_ref(&edit_path))
    });
    let refresh_stats = refresh_stats.expect("refresh");
    report("refresh", &refresh);
    eprintln!(
        "refresh stats: dependency_selected_refs={} refreshed_own_files={} surface_changed={:?}",
        refresh_stats.dependency_selected_refs,
        refresh_stats.refreshed_own_files,
        refresh_stats.surface_changed
    );
    if let Some(dump) = &dump {
        fs::write(dump.join("refresh_rows.txt"), dump_graph_rows(&store)).expect("write rows");
        fs::write(
            dump.join("refresh_queries.txt"),
            dump_query_outputs(&store, samples),
        )
        .expect("write queries");
    }
}

fn fixture_root(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    let root = dir.path().join(name);
    fs::create_dir_all(&root).expect("create fixture root");
    fs::canonicalize(root).expect("canonical fixture root")
}

fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(&path, text).expect("write fixture file");
    path
}

/// A workspace whose app files import two workspace packages by name, so
/// each import walks `package.json` files to find the package. Returns the
/// source files and the number of `package.json` files.
fn ts_workspace_fixture(root: &Path, app_files: usize) -> (Vec<PathBuf>, usize) {
    write(
        root,
        "package.json",
        r#"{"name":"fx-root","private":true,"workspaces":["packages/*"]}"#,
    );
    write(
        root,
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":".","paths":{}}}"#,
    );
    let mut files = Vec::new();
    for lib in ["alpha", "beta"] {
        write(
            root,
            &format!("packages/{lib}/package.json"),
            &format!(r#"{{"name":"@fx/{lib}","exports":{{".":{{"source":"./src/index.ts"}}}}}}"#),
        );
        files.push(write(
            root,
            &format!("packages/{lib}/src/index.ts"),
            &format!("export function {lib}(value: number) {{ return value + 1; }}\n"),
        ));
    }
    write(root, "packages/app/package.json", r#"{"name":"@fx/app"}"#);
    for index in 0..app_files {
        files.push(write(
            root,
            &format!("packages/app/src/deep/nested/caller_{index:03}.ts"),
            &format!(
                "import {{ alpha }} from \"@fx/alpha\";\nimport {{ beta }} from \"@fx/beta\";\n\
                 export function caller{index}() {{ return alpha({index}) + beta({index}); }}\n"
            ),
        ));
    }
    (files, 4)
}

/// A Rust crate whose `caller.rs` makes `calls` path-qualified calls into a
/// sibling module, plus a call into an inline module.
fn rust_qualified_call_fixture(root: &Path, calls: usize) -> Vec<PathBuf> {
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    let mut caller = String::new();
    for index in 0..calls {
        caller.push_str(&format!(
            "pub fn caller_{index}() {{ crate::util::helper(); crate::util::inner::deep(); }}\n"
        ));
    }
    vec![
        write(root, "src/lib.rs", "pub mod caller;\npub mod util;\n"),
        write(
            root,
            "src/util.rs",
            "pub fn helper() {}\npub mod inner {\n    pub fn deep() {}\n}\n",
        ),
        write(root, "src/caller.rs", &caller),
    ]
}

fn cold_build_counts(
    root: &Path,
    store_dir: &Path,
    files: &[PathBuf],
) -> (CallGraphStore, WorkCounts) {
    let store =
        CallGraphStore::open(store_dir.to_path_buf(), root.to_path_buf()).expect("open store");
    let compiles = install_compile_counter(&store);
    callgraph::clear_workspace_package_cache();
    let (stats, counts) = measure(root, compiles, || store.cold_build(files));
    stats.expect("cold build");
    (store, counts)
}

#[test]
fn cold_build_parses_each_file_once_and_matches_a_reparsing_build() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = fixture_root(&dir, "project");
    let (mut files, _) = ts_workspace_fixture(&root, 12);
    let rust_root = root.join("crates/fx");
    files.extend(rust_qualified_call_fixture(&rust_root, 6));

    let (retaining, retained) = cold_build_counts(&root, &dir.path().join("store-a"), &files);
    assert_eq!(
        retained.parses,
        files.len(),
        "every source file is parsed by extraction and by nothing else"
    );
    assert_eq!(retained.max_parses_of_one_file, 1);

    // With no retention budget the resolution pass parses every caller again,
    // which is the reference the retained resolver inputs must match.
    set_retained_caller_budget_for_test(Some(0));
    let (reparsing, reparsed) = cold_build_counts(&root, &dir.path().join("store-b"), &files);
    set_retained_caller_budget_for_test(None);
    eprintln!(
        "parses for {} files: retained={} reparsing={}",
        files.len(),
        retained.parses,
        reparsed.parses
    );
    assert!(
        reparsed.parses > files.len(),
        "control: without retention callers are parsed twice ({} parses for {} files)",
        reparsed.parses,
        files.len()
    );
    assert_eq!(
        dump_graph_rows(&retaining),
        dump_graph_rows(&reparsing),
        "retained resolver inputs must resolve exactly like a fresh parse"
    );
}

#[test]
fn cold_build_reads_each_package_json_a_bounded_number_of_times() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = fixture_root(&dir, "project");
    let app_files = 96;
    let (files, package_jsons) = ts_workspace_fixture(&root, app_files);
    // Readers sharing one memo can race to the first read of a file, at most
    // once per parse thread; resolution adds at most one more read.
    let bound = package_jsons * (build_pool_size() + 1);
    assert!(
        app_files > bound,
        "fixture must import more often than the bound allows a per-file memo"
    );

    let (_store, counts) = cold_build_counts(&root, &dir.path().join("store"), &files);
    eprintln!(
        "package.json reads: {} (bound {bound})",
        counts.package_json_reads
    );
    assert!(
        counts.package_json_reads <= bound,
        "package.json reads must not scale with importers: {} reads, bound {bound}",
        counts.package_json_reads
    );
}

#[test]
fn rust_qualified_call_resolution_compiles_statements_independent_of_call_count() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut compiled = Vec::new();
    for calls in [20usize, 80] {
        let root = fixture_root(&dir, &format!("project-{calls}"));
        let files = rust_qualified_call_fixture(&root, calls);
        let (store, counts) =
            cold_build_counts(&root, &dir.path().join(format!("store-{calls}")), &files);
        let edges = dump_graph_rows(&store)
            .lines()
            .filter(|line| line.starts_with("edges\t") && line.contains("src/util.rs"))
            .count();
        assert_eq!(edges, calls * 2, "every qualified call must resolve");
        compiled.push(counts.statements_compiled);
    }
    let growth = compiled[1].saturating_sub(compiled[0]);
    eprintln!("statements compiled for 20 and 80 calls: {compiled:?}");
    assert!(
        growth < 20,
        "60 more qualified calls must not compile statements per call: {compiled:?}"
    );
}

/// Refresh `edit` in a store built from `files`, check the graph equals a
/// cold build of the edited tree, and return the refresh's work counts.
fn refresh_matches_cold_build(files: &[(&str, &str)], edit: (&str, &str)) -> WorkCounts {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = fixture_root(&dir, "project");
    let paths = files
        .iter()
        .map(|(rel, text)| write(&root, rel, text))
        .collect::<Vec<_>>();
    let (store, _) = cold_build_counts(&root, &dir.path().join("store"), &paths);
    let compiles = install_compile_counter(&store);
    let edited = write(&root, edit.0, edit.1);
    let (stats, counts) = measure(&root, compiles, || {
        store.refresh_files(std::slice::from_ref(&edited))
    });
    stats.expect("refresh");

    let (rebuilt, _) = cold_build_counts(&root, &dir.path().join("store-rebuilt"), &paths);
    assert_eq!(
        dump_graph_rows(&store),
        dump_graph_rows(&rebuilt),
        "refreshing {} must give the rows of a cold build",
        edit.0
    );
    counts
}

fn ts_importer_files(plain: usize) -> Vec<(String, String)> {
    let mut files = vec![(
        "src/lib.ts".to_string(),
        "export function alpha() { return 1; }\nexport function beta() { return 2; }\n\
         export default function main() { return alpha(); }\n"
            .to_string(),
    )];
    for index in 0..plain {
        files.push((
            format!("src/plain_{index:02}.ts"),
            format!(
                "import {{ alpha, beta }} from \"./lib\";\n\
                 export function plain{index}() {{ return alpha() + beta(); }}\n"
            ),
        ));
    }
    files.push((
        "src/named.ts".to_string(),
        "import { zed } from \"./lib\";\nexport function named() { return zed(); }\n".to_string(),
    ));
    files.push((
        "src/aliased.ts".to_string(),
        "import { zed as z } from \"./lib\";\nexport function aliased() { return z(); }\n"
            .to_string(),
    ));
    files.push((
        "src/through_default.ts".to_string(),
        "import main from \"./lib\";\nexport function viaDefault() { return main(); }\n"
            .to_string(),
    ));
    files
}

fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(rel, text)| (rel.as_str(), text.as_str()))
        .collect()
}

#[test]
fn refresh_parses_only_importers_that_name_a_changed_symbol() {
    let files = ts_importer_files(16);
    let added = "export function alpha() { return 1; }\nexport function beta() { return 2; }\n\
                 export default function main() { return alpha(); }\nexport function zed() { return 3; }\n";
    let counts = refresh_matches_cold_build(&as_refs(&files), ("src/lib.ts", added));
    eprintln!("added export: refresh parses={}", counts.parses);
    // The changed file, the importer calling `zed` and the one calling it as
    // `z`; the 16 importers of other names and the default importer are
    // left alone.
    assert_eq!(counts.parses, 3, "refresh parses: {counts:?}");
}

#[test]
fn refresh_reresolves_every_caller_bound_to_a_removed_or_moved_symbol() {
    let files = ts_importer_files(4);
    // `beta` disappears and `alpha` moves (its node id changes).
    let edited = "\nexport function alpha() { return 1; }\n\
                  export default function main() { return alpha(); }\n";
    refresh_matches_cold_build(&as_refs(&files), ("src/lib.ts", edited));
}

#[test]
fn refresh_of_a_changed_default_export_falls_back_to_every_importer() {
    let files = ts_importer_files(4);
    let edited = "export function alpha() { return 1; }\nexport function beta() { return 2; }\n\
                  export default function other() { return beta(); }\n";
    let counts = refresh_matches_cold_build(&as_refs(&files), ("src/lib.ts", edited));
    assert_eq!(
        counts.parses,
        files.len(),
        "a default-export change reaches importers by their local name, so all are re-resolved"
    );
}

#[test]
fn rust_refresh_parses_only_callers_naming_a_changed_function() {
    let mut files = vec![
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_string(),
        ),
        (
            "src/lib.rs".to_string(),
            "pub mod util;\npub mod late;\n".to_string()
                + &(0..8)
                    .map(|index| format!("pub mod caller_{index};\n"))
                    .collect::<String>(),
        ),
        (
            "src/util.rs".to_string(),
            "pub fn helper() {}\n".to_string(),
        ),
        (
            "src/late.rs".to_string(),
            "pub fn late() { crate::util::added(); }\n".to_string(),
        ),
    ];
    for index in 0..8 {
        files.push((
            format!("src/caller_{index}.rs"),
            format!("use crate::util::helper;\npub fn run_{index}() {{ helper(); crate::util::helper(); }}\n"),
        ));
    }
    let counts = refresh_matches_cold_build(
        &as_refs(&files),
        ("src/util.rs", "pub fn helper() {}\npub fn added() {}\n"),
    );
    eprintln!("rust added fn: refresh parses={}", counts.parses);
    assert_eq!(counts.parses, 2, "refresh parses: {counts:?}");
}

#[test]
fn refresh_releases_the_store_connection_while_it_parses() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = fixture_root(&dir, "project");
    let (files, _) = ts_workspace_fixture(&root, 4);
    let (store, _) = cold_build_counts(&root, &dir.path().join("store"), &files);
    let edited = write(
        &root,
        "packages/alpha/src/index.ts",
        "export function alpha(value: number) { return value + 2; }\n",
    );

    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let started_tx = Mutex::new(started_tx);
    set_refresh_parse_hook_for_test(
        &root,
        Some(Arc::new(move || {
            let _ = started_tx.lock().expect("hook").send(());
            // The parse waits here until the reader below has finished.
            let _ = release_rx
                .lock()
                .expect("hook")
                .recv_timeout(Duration::from_secs(30));
        })),
    );
    std::thread::scope(|scope| {
        let refresh = scope.spawn(|| store.refresh_files(std::slice::from_ref(&edited)));
        started_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("refresh reached its parse");
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let store = &store;
        scope.spawn(move || {
            let _ = read_tx.send(store.indexed_file_count());
        });
        let read = read_rx.recv_timeout(Duration::from_secs(10));
        release_tx.send(()).expect("release parse");
        let read = read.expect("a reader must get the connection while the refresh parses");
        assert_eq!(read.expect("indexed file count"), files.len());
        refresh.join().expect("refresh thread").expect("refresh");
    });
    set_refresh_parse_hook_for_test(&root, None);
}

/// Statements compiled by a cold build of `files` TS modules and by a
/// refresh that rewrites every one of them.
fn build_and_refresh_compiles(dir: &tempfile::TempDir, files: usize) -> (usize, usize) {
    let root = fixture_root(dir, &format!("project-{files}"));
    let module = |index: usize, extra: &str| {
        format!(
            "import {{ helper }} from \"./lib\";\n\
             export class Worker{index} {{ run() {{ return helper(); }} }}\n\
             export function use{index}(worker: any) {{ return worker.run() + helper(); }}\n{extra}"
        )
    };
    let mut paths = vec![write(
        &root,
        "src/lib.ts",
        "export function helper() { return 1; }\n",
    )];
    for index in 0..files {
        paths.push(write(
            &root,
            &format!("src/m{index:03}.ts"),
            &module(index, ""),
        ));
    }
    let (store, cold) =
        cold_build_counts(&root, &dir.path().join(format!("store-{files}")), &paths);
    let compiles = install_compile_counter(&store);
    let mut changed = Vec::new();
    for index in 0..files {
        changed.push(write(
            &root,
            &format!("src/m{index:03}.ts"),
            &module(
                index,
                &format!("export function added{index}() {{ return 2; }}\n"),
            ),
        ));
    }
    let (stats, refresh) = measure(&root, compiles, || store.refresh_files(&changed));
    stats.expect("refresh");
    (cold.statements_compiled, refresh.statements_compiled)
}

#[test]
fn cold_build_and_refresh_compile_statements_independent_of_file_count() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (small_cold, small_refresh) = build_and_refresh_compiles(&dir, 10);
    let (large_cold, large_refresh) = build_and_refresh_compiles(&dir, 40);
    eprintln!(
        "statements compiled: cold 10 files={small_cold} 40 files={large_cold}; \
         refresh 10 files={small_refresh} 40 files={large_refresh}"
    );
    assert!(
        large_cold.saturating_sub(small_cold) < 30,
        "30 more files must not compile statements per file or row in a cold build: \
         {small_cold} vs {large_cold}"
    );
    assert!(
        large_refresh.saturating_sub(small_refresh) < 30,
        "30 more refreshed files must not compile statements per file or row: \
         {small_refresh} vs {large_refresh}"
    );
}
