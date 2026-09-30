//! Rollback safety: current code meeting artifacts in a newer on-disk format.
//!
//! Each test plants one artifact whose version header is above what this build
//! reads (the "future" build's output), configures current code over it, and
//! checks three things: the refusal names the artifact, the artifact's bytes
//! are unchanged afterwards (no rebuild, rewrite or removal over it), and the
//! owning component reports itself unavailable with that refusal as the reason.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aft::config::Config;
use aft::context::AppContext;
use aft::feature_status::{observed_index_status, IndexEffective, IndexPlane};
use aft::parser::TreeSitterProvider;
use aft::persisted_format::{PersistedStore, CODE};
use aft::protocol::RawRequest;
use serde_json::{json, Value};

const FUTURE: u32 = 99;
const SETTLE: Duration = Duration::from_secs(15);

struct Fixture {
    _project_dir: tempfile::TempDir,
    _storage_dir: tempfile::TempDir,
    project: PathBuf,
    storage: PathBuf,
    key: String,
}

impl Fixture {
    fn new() -> Self {
        let project_dir = tempfile::tempdir().expect("project dir");
        let storage_dir = tempfile::tempdir().expect("storage dir");
        fs::write(
            project_dir.path().join("lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\npub fn main() { greet(); }\n",
        )
        .expect("write source");
        let project = fs::canonicalize(project_dir.path()).expect("canonical project");
        let storage = storage_dir.path().to_path_buf();
        let key = aft::search_index::artifact_cache_key(&project);
        Self {
            _project_dir: project_dir,
            _storage_dir: storage_dir,
            project,
            storage,
            key,
        }
    }

    fn artifact(&self, dir: &str, file: &str) -> PathBuf {
        let path = self.storage.join(dir).join(&self.key).join(file);
        fs::create_dir_all(path.parent().unwrap()).expect("artifact dir");
        path
    }

    fn configure(&self, indexes: Value) -> Arc<AppContext> {
        let ctx = Arc::new(AppContext::new(
            Box::new(TreeSitterProvider::new()),
            Config::default(),
        ));
        let mut doc = json!({ "indexes": indexes, "views": { "enabled": false } });
        if indexes["semantic"] == true {
            // An unreachable endpoint: the semantic lane must never need an
            // embedding to reach its refusal.
            doc["semantic"] = json!({
                "backend": "openai_compatible",
                "model": "rollback-probe",
                "base_url": "http://127.0.0.1:9",
                "timeout_ms": 200,
            });
        }
        let request: RawRequest = serde_json::from_value(json!({
            "id": "configure-rollback-safety",
            "command": "configure",
            "harness": "opencode",
            "session_id": "rollback-safety",
            "project_root": self.project,
            "storage_dir": self.storage,
            "config": crate::helpers::user_config(doc),
        }))
        .expect("configure request");
        let response = aft::commands::configure::handle_configure(&request, &ctx);
        assert!(response.success, "configure failed: {response:?}");
        aft::runtime_drain::drain_deferred_configure_maintenance(&ctx);
        if indexes
            .as_object()
            .is_some_and(|planes| planes.values().any(|on| on == true))
        {
            assert_eq!(
                ctx.cached_artifact_cache_key(&self.project).as_deref(),
                Some(self.key.as_str()),
                "the planted artifact must sit under the key configure actually uses"
            );
        }
        ctx
    }
}

fn drain(ctx: &AppContext) {
    aft::runtime_drain::drain_search_index_events(ctx);
    aft::runtime_drain::drain_semantic_index_events(ctx);
    aft::runtime_drain::drain_callgraph_store_events(ctx);
}

/// Wait until `done` holds (the component settled after refusing), draining
/// background events meanwhile.
fn settle(ctx: &AppContext, what: &str, mut done: impl FnMut(&AppContext) -> bool) {
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        drain(ctx);
        if done(ctx) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "{what} did not settle within {SETTLE:?}; semantic status {:?}",
        ctx.semantic_index_status()
            .read()
            .map(|status| format!("{status:?}"))
    );
}

fn assert_named(reason: &str, store: PersistedStore, path: &Path) {
    assert!(
        reason.contains(CODE),
        "reason must carry the code: {reason}"
    );
    assert!(
        reason.contains(store.name()),
        "reason must name {store}: {reason}"
    );
    assert!(
        reason.contains(&path.display().to_string()),
        "reason must name {}: {reason}",
        path.display()
    );
    assert!(
        reason.contains(&FUTURE.to_string()),
        "reason must carry the version: {reason}"
    );
}

fn assert_plane_refused(ctx: &AppContext, plane: IndexPlane, store: PersistedStore, path: &Path) {
    let observation = observed_index_status(ctx, plane);
    assert_eq!(
        observation.effective,
        IndexEffective::Unavailable,
        "{observation:?}"
    );
    assert_named(
        observation
            .unavailable_reason
            .as_deref()
            .unwrap_or_default(),
        store,
        path,
    );
}

fn assert_status_lists(ctx: &AppContext, store: PersistedStore) -> Value {
    let status = ctx.build_status_snapshot();
    let reason = format!("{CODE}:{}", store.name());
    assert!(
        status["degraded_reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|value| value == &json!(reason))),
        "degraded_reasons must name {reason}: {}",
        status["degraded_reasons"]
    );
    assert!(
        status["storage_refusals"]
            .as_array()
            .is_some_and(|refusals| refusals
                .iter()
                .any(|refusal| refusal["store"] == store.name())),
        "storage_refusals must list {store}: {}",
        status["storage_refusals"]
    );
    status
}

#[test]
fn future_search_index_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let cache = fixture.artifact("index", "cache.bin");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0x3144_4958_u32.to_le_bytes()); // trigram cache magic
    bytes.extend_from_slice(&FUTURE.to_le_bytes());
    bytes.extend_from_slice(&[0xA5; 64]);
    fs::write(&cache, &bytes).unwrap();

    let ctx = fixture.configure(json!({ "trigram": true, "semantic": false, "callgraph": false }));
    // The configure worker settles on a build-denied (empty) index instead of
    // building and persisting over the newer cache.
    settle(&ctx, "search index", |ctx| {
        ctx.search_index()
            .read()
            .ok()
            .is_some_and(|index| index.as_ref().is_some_and(|index| index.build_denied))
    });

    assert_eq!(
        fs::read(&cache).unwrap(),
        bytes,
        "newer search cache was changed"
    );
    assert_plane_refused(
        &ctx,
        IndexPlane::Trigram,
        PersistedStore::SearchIndex,
        &cache,
    );
    let status = assert_status_lists(&ctx, PersistedStore::SearchIndex);
    assert_eq!(status["search_index"]["status"], "unavailable");
    assert_named(
        status["search_index"]["reason"]
            .as_str()
            .unwrap_or_default(),
        PersistedStore::SearchIndex,
        &cache,
    );
}

#[test]
fn future_semantic_index_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let snapshot = fixture.artifact("semantic", "semantic.bin");
    let mut bytes = vec![FUTURE as u8];
    bytes.extend_from_slice(&[0x5A; 256]);
    fs::write(&snapshot, &bytes).unwrap();

    let ctx = fixture.configure(json!({ "trigram": false, "semantic": true, "callgraph": false }));
    // The semantic lane ends with a failure (the refusal, or the unreachable
    // test backend), never with a rebuilt snapshot.
    settle(&ctx, "semantic index", |ctx| {
        matches!(
            *ctx.semantic_index_status().read().unwrap(),
            aft::context::SemanticIndexStatus::Failed(_)
        )
    });

    assert_eq!(
        fs::read(&snapshot).unwrap(),
        bytes,
        "newer semantic snapshot was changed"
    );
    assert_plane_refused(
        &ctx,
        IndexPlane::Semantic,
        PersistedStore::SemanticIndex,
        &snapshot,
    );
    let status = assert_status_lists(&ctx, PersistedStore::SemanticIndex);
    assert_eq!(status["semantic_index"]["status"], "unavailable");
    assert_named(
        status["semantic_index"]["reason"]
            .as_str()
            .unwrap_or_default(),
        PersistedStore::SemanticIndex,
        &snapshot,
    );
}

#[test]
fn future_callgraph_generation_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let generation = format!("{}.g1.1.sqlite", fixture.key);
    let generation_path = fixture.artifact("callgraph", &generation);
    {
        let conn = rusqlite::Connection::open(&generation_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             INSERT INTO meta(k, v) VALUES ('schema_version', '{FUTURE}');
             INSERT INTO meta(k, v) VALUES ('fingerprint', 'written-by-a-newer-build');
             INSERT INTO meta(k, v) VALUES ('ready', '1');"
        ))
        .unwrap();
    }
    let pointer = fixture.artifact("callgraph", &format!("{}.current", fixture.key));
    fs::write(&pointer, &generation).unwrap();
    let generation_bytes = fs::read(&generation_path).unwrap();
    let callgraph_dir = generation_path.parent().unwrap().to_path_buf();
    let listing = |dir: &Path| {
        let mut names = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".sqlite") || name.ends_with(".current"))
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let files_before = listing(&callgraph_dir);

    let ctx = fixture.configure(json!({ "trigram": false, "semantic": false, "callgraph": true }));
    // Asking for the store is what would cold-build and republish the
    // pointer; it must answer with the refusal instead.
    let access = ctx.callgraph_store_for_ops();
    let error = match access {
        aft::context::CallgraphStoreAccess::Error(error) => error.to_string(),
        aft::context::CallgraphStoreAccess::Ready(_) => panic!("the newer store was opened"),
        aft::context::CallgraphStoreAccess::Building => panic!("a build was started over it"),
        _ => panic!("expected the named refusal"),
    };
    assert_named(&error, PersistedStore::CallgraphStore, &generation_path);
    settle(&ctx, "callgraph", |ctx| {
        ctx.callgraph_store_rx()
            .try_lock()
            .is_some_and(|rx| rx.is_none())
    });

    assert_eq!(fs::read(&generation_path).unwrap(), generation_bytes);
    assert_eq!(
        fs::read_to_string(&pointer).unwrap(),
        generation,
        "pointer was republished"
    );
    assert_eq!(
        listing(&callgraph_dir),
        files_before,
        "a new generation was published"
    );
    assert_plane_refused(
        &ctx,
        IndexPlane::Callgraph,
        PersistedStore::CallgraphStore,
        &generation_path,
    );
    assert_status_lists(&ctx, PersistedStore::CallgraphStore);
}

#[test]
fn future_symbol_cache_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let cache = fixture.artifact("symbols", "symbols.bin");
    let mut bytes = b"AFTSYM1\0".to_vec();
    bytes.extend_from_slice(&FUTURE.to_le_bytes());
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&[0x3C; 64]);
    fs::write(&cache, &bytes).unwrap();

    // The search index load prewarms (and persists) the symbol cache.
    let ctx = fixture.configure(json!({ "trigram": true, "semantic": false, "callgraph": false }));
    settle(&ctx, "search index", |ctx| {
        ctx.search_index()
            .read()
            .ok()
            .is_some_and(|index| index.as_ref().is_some_and(|index| index.ready))
    });
    // Give the prewarm, which runs after the index is published, time to
    // reach its persist step.
    thread::sleep(Duration::from_millis(500));

    assert_eq!(
        fs::read(&cache).unwrap(),
        bytes,
        "newer symbol cache was changed"
    );
    let status = assert_status_lists(&ctx, PersistedStore::SymbolCache);
    let refusal = status["storage_refusals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|refusal| refusal["store"] == "symbols")
        .unwrap();
    assert_named(
        refusal["message"].as_str().unwrap(),
        PersistedStore::SymbolCache,
        &cache,
    );
}

#[test]
fn future_owner_manifest_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let manifest = fixture.artifact("artifact-owners", "owner.json");
    let bytes = serde_json::to_vec_pretty(&json!({
        "schema_version": FUTURE,
        "project_scope_key": "written-by-a-newer-build",
        "checkout_path": "/nonexistent/newer",
        "owner": { "shape": "changed" },
    }))
    .unwrap();
    fs::write(&manifest, &bytes).unwrap();

    let ctx = fixture.configure(json!({ "trigram": true, "semantic": false, "callgraph": false }));
    drain(&ctx);

    assert_eq!(
        fs::read(&manifest).unwrap(),
        bytes,
        "newer owner manifest was changed"
    );
    let status = assert_status_lists(&ctx, PersistedStore::ArtifactOwner);
    assert_eq!(status["artifact_owner"]["mode"], "read_only");
    assert_named(
        status["artifact_owner"]["note"]
            .as_str()
            .unwrap_or_default(),
        PersistedStore::ArtifactOwner,
        &manifest,
    );
}

#[test]
fn future_aft_db_is_refused_by_name_and_left_byte_identical() {
    let fixture = Fixture::new();
    let db_path = fixture.storage.join("aft.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE schema_version (version INTEGER NOT NULL PRIMARY KEY);
             INSERT INTO schema_version(version) VALUES ({FUTURE});
             CREATE TABLE written_by_a_newer_build (x INTEGER);"
        ))
        .unwrap();
    }
    let bytes = fs::read(&db_path).unwrap();

    let ctx = fixture.configure(json!({ "trigram": false, "semantic": false, "callgraph": false }));

    assert_eq!(
        fs::read(&db_path).unwrap(),
        bytes,
        "newer aft.db was changed"
    );
    assert!(
        !db_path.with_extension("db-wal").exists(),
        "no read-write connection may switch the newer database to WAL"
    );
    let refused = ctx
        .database_runtime_refusal("probe", "write")
        .expect("persistence-dependent tools must be refused");
    let refused = serde_json::to_value(refused).unwrap();
    assert_eq!(refused["code"], "database_unavailable", "{refused}");
    assert_named(
        refused["message"].as_str().unwrap_or_default(),
        PersistedStore::AftDb,
        &db_path,
    );
    assert_status_lists(&ctx, PersistedStore::AftDb);
}

#[test]
fn a_reader_floor_above_this_build_refuses_the_store_before_anything_is_written() {
    let fixture = Fixture::new();
    let above = u64::from(PersistedStore::SearchIndex.supported()) + 1;
    let floor = fixture.storage.join(aft::reader_floor::FLOOR_FILE);
    fs::write(
        &floor,
        serde_json::to_vec(&json!({ "floor_schema": 1, "stores": { "search": above } })).unwrap(),
    )
    .unwrap();

    let ctx = fixture.configure(json!({ "trigram": true, "semantic": false, "callgraph": false }));
    settle(&ctx, "search index", |ctx| {
        ctx.search_index()
            .read()
            .ok()
            .is_some_and(|index| index.as_ref().is_some_and(|index| index.build_denied))
    });

    let cache = fixture
        .storage
        .join("index")
        .join(&fixture.key)
        .join("cache.bin");
    assert!(
        !cache.exists(),
        "a build below the floor must not write the store"
    );
    let observation = observed_index_status(&ctx, IndexPlane::Trigram);
    let reason = observation.unavailable_reason.unwrap_or_default();
    assert!(reason.starts_with(CODE), "{reason}");
    assert!(reason.contains(&floor.display().to_string()), "{reason}");
    let written = aft::reader_floor::read(&fixture.storage).unwrap().unwrap();
    assert_eq!(
        written.get(PersistedStore::SearchIndex),
        Some(above),
        "floor lowered"
    );
}
