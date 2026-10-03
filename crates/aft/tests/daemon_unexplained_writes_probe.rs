//! Offline write measurements. AFT_WRITE_PROBE_DIR must contain SQLite-backup
//! snapshots named aft-snapshot.sqlite and callgraph.sqlite, never live stores.
//! Run this ignored test alone; process counters include concurrent threads.
//! The production-refresh probe edits its disposable prefrontal-root clone;
//! AFT_WRITE_PROBE_DELETE=1 deletes the two selected source files in that clone.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::Instant;

fn measure<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let before = aft::process_io::Bytes::capture().unwrap();
    let started = Instant::now();
    let result = f();
    let delta = aft::process_io::Bytes::capture()
        .unwrap()
        .delta(before)
        .unwrap();
    println!(
        "{label}: seconds={:.3} physical={} logical={}",
        started.elapsed().as_secs_f64(),
        delta.written,
        delta.logical
    );
    result
}

#[test]
#[ignore = "requires offline database snapshots; measures physical writes"]
fn offline_database_read_writes() {
    let dir = PathBuf::from(std::env::var_os("AFT_WRITE_PROBE_DIR").unwrap());
    let db = rusqlite::Connection::open_with_flags(
        dir.join("aft-snapshot.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let session: (String, String) = db.query_row("SELECT harness,session_id FROM bash_tasks GROUP BY harness,session_id ORDER BY count(*) DESC LIMIT 1", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    for _ in 0..2 {
        measure("bash-session", || {
            aft::db::bash_tasks::list_bash_tasks_for_session(&db, &session.0, &session.1).unwrap()
        });
        measure("callgraph-projection", || {
            aft::callgraph_store::project_dead_code_snapshot(&dir.join("callgraph.sqlite")).unwrap()
        });
    }
}

#[test]
#[ignore = "requires offline database snapshots; measures physical writes"]
fn offline_sparse_callgraph_writes() {
    let dir = PathBuf::from(std::env::var_os("AFT_WRITE_PROBE_DIR").unwrap());
    let temp = tempfile::tempdir_in(&dir).unwrap();
    let path = temp.path().join("sparse.sqlite");
    let source = rusqlite::Connection::open_with_flags(
        dir.join("callgraph.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut destination = rusqlite::Connection::open(&path).unwrap();
    rusqlite::backup::Backup::new(&source, &mut destination)
        .unwrap()
        .run_to_completion(512, std::time::Duration::from_millis(1), None)
        .unwrap();
    drop(destination);
    drop(source);
    let writer =
        aft::db::TrackedConnection::open(&path, aft::db::SqliteStore::CallgraphGeneration).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8192;",
        )
        .unwrap();
    writer.set_wal_autocheckpoint(4000).unwrap();
    let file: String = writer
        .query_row(
            "SELECT caller_file FROM refs GROUP BY caller_file ORDER BY count(*) DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    writer
        .execute_batch("CREATE TEMP TABLE saved AS SELECT * FROM refs WHERE 0")
        .unwrap();
    writer
        .execute(
            "INSERT INTO saved SELECT * FROM refs WHERE caller_file=?1",
            [&file],
        )
        .unwrap();
    let ledger = aft::db::open(&temp.path().join("ledger.sqlite")).unwrap();
    writer.sample_write_pages();
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let root = path.display().to_string();
    let before = aft::write_ledger::census(&ledger, 0, Some(&root), now()).unwrap();
    measure("sparse-ref-rewrite-10", || {
        for _ in 0..10 {
            let tx = writer.unchecked_transaction().unwrap();
            tx.execute("DELETE FROM refs WHERE caller_file=?1", [&file])
                .unwrap();
            tx.execute_batch("INSERT INTO refs SELECT * FROM saved")
                .unwrap();
            tx.commit().unwrap();
        }
        writer.sample_write_pages();
        writer
            .checkpoint_wal_as(
                aft::db::lifecycle::WalCheckpointMode::Passive,
                aft::write_ledger::Domain::CallgraphCheckpoint,
            )
            .unwrap();
    });
    let after = aft::write_ledger::census(&ledger, 0, Some(&root), now()).unwrap();
    println!(
        "sparse-ledger-before: {}",
        serde_json::to_string(&before).unwrap()
    );
    println!(
        "sparse-ledger-after: {}",
        serde_json::to_string(&after).unwrap()
    );
}

#[test]
#[ignore = "requires offline snapshots and an isolated prefrontal-root source clone"]
fn offline_production_callgraph_refresh() {
    let dir = PathBuf::from(std::env::var_os("AFT_WRITE_PROBE_DIR").unwrap());
    let root = dir.join("prefrontal-root").canonicalize().unwrap();
    assert!(
        root.starts_with(dir.canonicalize().unwrap()),
        "source clone must be inside the offline input directory"
    );
    assert!(
        root.join(".git").is_dir(),
        "expected a disposable source clone"
    );
    let temp = tempfile::tempdir_in(&dir).unwrap();
    let key = aft::search_index::artifact_cache_key(&root);
    let path = temp.path().join(format!("{key}.sqlite"));
    let source = rusqlite::Connection::open_with_flags(
        dir.join("callgraph.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut destination = rusqlite::Connection::open(&path).unwrap();
    rusqlite::backup::Backup::new(&source, &mut destination)
        .unwrap()
        .run_to_completion(512, std::time::Duration::from_millis(1), None)
        .unwrap();
    // The copy belongs to the isolated clone, not the still-live source root.
    destination
        .execute(
            "UPDATE OR REPLACE backend_file_state SET workspace_root=?1",
            [root.display().to_string()],
        )
        .unwrap();
    drop(destination);
    drop(source);
    let store = aft::callgraph_store::CallGraphStore::open(temp.path().to_path_buf(), root.clone())
        .unwrap();
    let ledger = aft::db::open(&temp.path().join("ledger.sqlite")).unwrap();
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let before = aft::write_ledger::census(&ledger, 0, None, now()).unwrap();
    let files = vec![
        root.join("crates/prefrontal-core-module/src/manager_runtime.rs"),
        root.join("crates/prefrontal-core-module/src/dispatch.rs"),
    ];
    for round in 0..2 {
        if round == 0 {
            for file in &files {
                if std::env::var_os("AFT_WRITE_PROBE_DELETE").is_some() {
                    std::fs::remove_file(file).unwrap();
                    continue;
                }
                use std::io::Write;
                writeln!(
                    std::fs::OpenOptions::new().append(true).open(file).unwrap(),
                    "\n// Offline write probe changes the content hash."
                )
                .unwrap();
            }
        }
        let stats = measure(&format!("production-refresh-{round}"), || {
            store.refresh_files_profiled(&files).unwrap()
        });
        println!("refresh stats: {stats:?}");
    }
    drop(store);
    let after = aft::write_ledger::census(&ledger, 0, None, now()).unwrap();
    println!(
        "production-ledger-before: {}",
        serde_json::to_string(&before).unwrap()
    );
    println!(
        "production-ledger-after: {}",
        serde_json::to_string(&after).unwrap()
    );
    std::thread::sleep(std::time::Duration::from_secs(3));
}
