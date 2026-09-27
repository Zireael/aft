//! Measurement only: how many `callers` answers change when a root switches
//! from the legacy callgraph store to a views-derived database, for methods
//! the legacy store reaches through receiver-type or name-match dispatch.
//!
//! The legacy cold build adds `call` edges with provenance `type_match` or
//! `name_match` for method calls whose receiver the resolver cannot bind
//! (`insert_method_dispatch_edges`). The view extract sets no dispatch hints,
//! so a views database has no such edges.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::callgraph_store::{CallGraphStore, ReadonlyCallGraphStore};
use crate::commands::callgraph_store_adapter::callers_result;

fn provenance_counts(path: &Path) -> Vec<(String, String, i64)> {
    Connection::open(path)
        .unwrap()
        .prepare(
            "SELECT COALESCE(provenance, ''), kind, COUNT(*) FROM edges
             GROUP BY 1, 2 ORDER BY 3 DESC",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

/// `(file, caller symbol, line)` for every caller group entry.
fn caller_sites(
    result: &crate::commands::callgraph_store_adapter::StoreCallersResult,
) -> BTreeSet<(String, String, u32)> {
    result
        .callers
        .iter()
        .flat_map(|group| {
            group
                .callers
                .iter()
                .map(|entry| (group.file.clone(), entry.symbol.clone(), entry.line))
        })
        .collect()
}

/// `(total_callers, listed caller entries)` for the first of `names` that
/// resolves in `store`.
fn callers_total(
    store: &impl crate::callgraph_store::CallGraphRead,
    file: &Path,
    names: &[&str],
) -> Option<(usize, usize)> {
    names.iter().find_map(|name| {
        callers_result(store, file, name, 1, true)
            .ok()
            .map(|result| (result.total_callers, caller_sites(&result).len()))
    })
}

#[test]
#[ignore = "measurement against a real checkout; clones AFT_DISPATCH_PROBE_SOURCE into isolated storage"]
fn probe_method_dispatch_callers_views_vs_legacy() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"));
    let source = var("AFT_DISPATCH_PROBE_SOURCE");
    let revision = var("AFT_DISPATCH_PROBE_REV");
    let samples = std::env::var("AFT_DISPATCH_PROBE_SAMPLES")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(15);
    let out = tempfile::tempdir_in(
        std::env::var_os("AFT_DISPATCH_PROBE_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir),
    )
    .unwrap();
    let checkout = out.path().join("checkout");
    for args in [
        vec![
            "clone",
            "--shared",
            "--quiet",
            "--no-checkout",
            source.as_str(),
            checkout.to_str().unwrap(),
        ],
        vec![
            "-C",
            checkout.to_str().unwrap(),
            "checkout",
            "--quiet",
            revision.as_str(),
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(&args)
            .status()
            .unwrap()
            .success());
    }
    let root = std::fs::canonicalize(&checkout).unwrap();
    let storage = out.path().join("storage");

    let started = std::time::Instant::now();
    let family = crate::search_index::artifact_cache_key(&root);
    let scope = crate::path_identity::project_scope_key(&root);
    let files = crate::callgraph::walk_project_files(&root).collect::<Vec<_>>();
    // Register the checkout as an owning root, as configure does, so the cold
    // build may take the writer lease.
    crate::root_cache::configure_artifact_access(&root, &family, false);
    // The same per-root directory AppContext::callgraph_store_dir uses.
    let (legacy, _) = CallGraphStore::cold_build_with_lease(
        storage.join("callgraph").join(&family),
        root.clone(),
        &files,
    )
    .unwrap();
    println!(
        "legacy cold build: files={} s={:.1}",
        files.len(),
        started.elapsed().as_secs_f64()
    );

    let started = std::time::Instant::now();
    let report =
        crate::views::assembly::publish_checkout(&crate::views::assembly::AssemblyRequest {
            storage: storage.clone(),
            project_root: root.clone(),
            family: family.clone(),
            scope: scope.clone(),
            desired_head: revision.clone(),
            changed_paths: BTreeSet::new(),
            semantic_keys: Default::default(),
            require_semantic: false,
            allow_blob_put: true,
        })
        .unwrap();
    let view_dir = crate::views::ViewStore::open(&storage, &scope)
        .unwrap()
        .view_dir()
        .to_path_buf();
    let views = ReadonlyCallGraphStore::open_manifest_view(
        root.clone(),
        family,
        view_dir,
        report.generation.as_deref().unwrap(),
        None,
    )
    .unwrap();
    println!(
        "views publication: s={:.1}",
        started.elapsed().as_secs_f64()
    );

    for (label, path) in [
        ("legacy", legacy.sqlite_path()),
        ("views", views.sqlite_path()),
    ] {
        println!("{label} edges by provenance: {:?}", provenance_counts(path));
    }

    // Every legacy dispatch target, ranked by how many dispatch edges reach it.
    let legacy_connection = Connection::open(legacy.sqlite_path()).unwrap();
    let all_targets = legacy_connection
        .prepare(
            "SELECT e.target_file, e.target_symbol, e.provenance, COUNT(*) AS edges,
                    COALESCE(n.kind, '')
             FROM edges e LEFT JOIN nodes n ON n.id = e.target_node
             WHERE e.provenance IN ('type_match', 'name_match')
             GROUP BY 1, 2, 3 ORDER BY 4 DESC",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    // A class that implements an interface, or a Rust `impl Trait for Type`,
    // in the target's file: the candidates for interface or trait dispatch.
    let implements = |file: &str| {
        let source = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        source.contains(" implements ")
            || source
                .lines()
                .any(|line| line.trim_start().starts_with("impl") && line.contains(" for "))
    };
    let by_count = all_targets
        .iter()
        .take(samples)
        .cloned()
        .collect::<Vec<_>>();
    let interface_targets = all_targets
        .iter()
        .filter(|(file, _, _, _, kind)| kind == "method" && implements(file))
        .take(samples)
        .cloned()
        .collect::<Vec<_>>();
    println!(
        "legacy dispatch targets={} with an implements/impl-for in the target file={}",
        all_targets.len(),
        all_targets
            .iter()
            .filter(|(file, _, _, _, kind)| kind == "method" && implements(file))
            .count()
    );
    for (set, targets) in [
        ("top-by-dispatch-edges", by_count),
        ("interface-or-trait", interface_targets),
    ] {
        for (file, symbol, provenance, edges, kind) in targets {
            let short = symbol
                .rsplit(['.', ':'])
                .next()
                .unwrap_or(&symbol)
                .to_string();
            let names = [symbol.as_str(), short.as_str()];
            let legacy_total = callers_total(&legacy, &root.join(&file), &names);
            let views_total = callers_total(&views, &root.join(&file), &names);
            // Three dispatch call sites with their source line, to show the
            // receiver the name match paired with this target.
            let examples = legacy_connection
                .prepare(
                    "SELECT r.caller_file, r.line FROM edges e JOIN refs r ON r.ref_id = e.ref_id
                     WHERE e.target_file = ?1 AND e.target_symbol = ?2 AND e.provenance = ?3
                     ORDER BY r.caller_file, r.line LIMIT 3",
                )
                .unwrap()
                .query_map(rusqlite::params![file, symbol, provenance], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .map(|(caller, line)| {
                    let text = std::fs::read_to_string(root.join(&caller))
                        .ok()
                        .and_then(|source| {
                            source
                                .lines()
                                .nth(line.saturating_sub(1) as usize)
                                .map(|text| text.trim().chars().take(100).collect::<String>())
                        })
                        .unwrap_or_default();
                    format!("{caller}:{line}: {text}")
                })
                .collect::<Vec<_>>();
            println!(
                "[{set}] {file}::{symbol} kind={kind} provenance={provenance} dispatch_edges={edges} legacy_total_callers={:?} views_total_callers={:?} dispatch_examples={examples:?}",
                legacy_total,
                views_total,
            );
        }
    }
}
