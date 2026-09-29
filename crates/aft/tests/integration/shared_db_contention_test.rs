//! Several AFT processes (standalone OpenCode windows, Pi, the CLI) share one
//! `aft.db`. In each test here the test process holds a write transaction open
//! on that database through its own single connection, while a standalone
//! `aft` child runs its storage maintenance and serves requests (#375).
//!
//! The hold outlasts the steady five-second SQLite busy wait, which is how the
//! reported `database is locked` fold failures, per-minute retention warnings
//! and five-second `status` timeouts arose.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aft::db::TrackedConnection as Connection;
use serde_json::json;

use super::helpers::AftProcess;

/// Longer than the steady busy wait, so any maintenance still using it fails.
const HOLD: Duration = Duration::from_secs(7);
const STATUS_BUDGET: Duration = Duration::from_secs(1);
/// How long the child's maintenance keeps retrying a write that meets the
/// held write lock. Set far above [`READ_BUDGET`], so a maintenance path that
/// waits while holding the connection mutex delays reads by about this much,
/// on fast and slow machines alike.
const MAINTENANCE_BUSY_BUDGET_MS: &str = "2000";
/// A read must never wait out a maintenance busy budget. Half a second leaves
/// room for a loaded machine's scheduling and stays far below the budget above.
const READ_BUDGET: Duration = Duration::from_millis(500);

struct Contended {
    _project: tempfile::TempDir,
    storage: tempfile::TempDir,
    aft: AftProcess,
    /// The only connection this test process opens on the shared database.
    conn: Connection,
}

impl Contended {
    fn start() -> Self {
        let project = tempfile::tempdir().expect("project dir");
        let storage = tempfile::tempdir().expect("storage dir");
        let mut aft = AftProcess::spawn_with_env(&[
            ("AFT_STORAGE_DIR", storage.path().as_os_str()),
            // Run maintenance every few hundred milliseconds instead of
            // every minute, so several runs fall inside the hold.
            (
                "AFT_TEST_MAINTENANCE_INTERVAL_MS",
                std::ffi::OsStr::new("300"),
            ),
            (
                "AFT_TEST_MAINTENANCE_BUSY_BUDGET_MS",
                std::ffi::OsStr::new(MAINTENANCE_BUSY_BUDGET_MS),
            ),
            (
                "RUST_LOG",
                std::ffi::OsStr::new(
                    "info,aft::db::write_ledger=debug,aft::db::compression_events=debug",
                ),
            ),
        ]);
        let response = aft.send(
            &json!({
                "id": "cfg",
                "command": "configure",
                "harness": "opencode",
                "project_root": project.path(),
                "storage_dir": storage.path(),
            })
            .to_string(),
        );
        assert_eq!(response["success"], true, "configure failed: {response:?}");
        let conn = aft::db::open(&storage.path().join("aft.db")).expect("open shared aft.db");
        let contended = Self {
            _project: project,
            storage,
            aft,
            conn,
        };
        // Start contending only once the child has committed a fold, so the
        // folds under test are the ones that meet the held write lock.
        contended.wait_for(Duration::from_secs(20), "first write-ledger fold", |c| {
            c.conn
                .query_row("SELECT COUNT(*) FROM write_ledger_meta", [], |row| {
                    row.get::<_, i64>(0)
                })
                .is_ok_and(|rows| rows > 0)
        });
        contended
    }

    fn log_path(&self) -> PathBuf {
        self.storage
            .path()
            .join("logs")
            .join(format!("aft-{}.log", self.aft.pid()))
    }

    fn log(&self) -> String {
        self.log_since(0)
    }

    /// The child's durable log from byte `start` on. The file only grows.
    fn log_since(&self, start: usize) -> String {
        let bytes = std::fs::read(self.log_path()).unwrap_or_default();
        String::from_utf8_lossy(bytes.get(start..).unwrap_or_default()).into_owned()
    }

    fn log_len(&self) -> usize {
        std::fs::metadata(self.log_path()).map_or(0, |meta| meta.len() as usize)
    }

    fn wait_for(&self, timeout: Duration, what: &str, ready: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + timeout;
        while !ready(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; child log:\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Hold the database write lock for [`HOLD`], calling `during` repeatedly
    /// meanwhile. Returns the log's length when the hold began.
    fn hold_write_lock(&mut self, mut during: impl FnMut(&mut AftProcess)) -> usize {
        let log_start = self.log_len();
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .expect("take the write lock");
        let held_since = Instant::now();
        while held_since.elapsed() < HOLD {
            during(&mut self.aft);
            std::thread::sleep(Duration::from_millis(100));
        }
        self.conn
            .execute_batch("COMMIT")
            .expect("release the write lock");
        log_start
    }

    /// Stop the child and return its complete durable log.
    fn finish(self) -> String {
        let log_path = self.log_path();
        let status = self.aft.shutdown();
        assert!(status.success(), "aft exited with {status}");
        std::fs::read_to_string(&log_path).unwrap_or_default()
    }
}

fn warn_lines<'a>(log: &'a str, needle: &str) -> Vec<&'a str> {
    log.lines()
        .filter(|line| line.contains(" WARN ") && line.contains(needle))
        .collect()
}

/// A fold that meets another process's write lock waits briefly, then defers
/// without warning; once the lock is released a later fold commits the data
/// the deferred one kept in memory.
#[test]
fn minute_fold_defers_under_a_foreign_write_lock_and_commits_after_release() {
    let mut contended = Contended::start();
    let log_start = contended.hold_write_lock(|_| {});
    let held_log = contended.log_since(log_start);
    let warnings = warn_lines(&held_log, "write ledger minute fold");
    assert!(
        warnings.is_empty(),
        "fold warned under contention: {warnings:#?}"
    );
    assert!(
        held_log.contains("write ledger minute fold deferred"),
        "no fold met the held write lock; child log:\n{held_log}"
    );
    let released_at = contended.log_len();
    contended.wait_for(
        Duration::from_secs(10),
        "a fold committed after the lock was released",
        |c| {
            c.log_since(released_at)
                .contains("write ledger minute fold committed")
        },
    );

    let log = contended.finish();
    let warnings = warn_lines(&log, "write ledger minute fold");
    assert!(
        warnings.is_empty(),
        "fold warned under contention: {warnings:#?}"
    );
}

/// `status` answers from memory. A write lock held by another process must
/// not delay it, however long this process's maintenance waits for that lock.
#[test]
fn status_answers_within_a_second_while_another_process_holds_the_write_lock() {
    let mut contended = Contended::start();
    let mut latencies = Vec::new();
    contended.hold_write_lock(|aft| {
        let started = Instant::now();
        let response = aft.send_with_timeout(
            &json!({ "id": "status", "command": "status" }).to_string(),
            Duration::from_secs(30),
        );
        latencies.push(started.elapsed());
        assert_eq!(response["success"], true, "status failed: {response:?}");
    });
    contended.finish();

    assert!(
        latencies.len() >= 5,
        "only {} status calls ran",
        latencies.len()
    );
    let slow = latencies
        .iter()
        .filter(|latency| **latency >= STATUS_BUDGET)
        .collect::<Vec<_>>();
    assert!(
        slow.is_empty(),
        "status exceeded {STATUS_BUDGET:?} while aft.db was write-locked: {slow:?} of {latencies:?}"
    );
}

/// Maintenance retries a write that meets another process's write lock, but
/// it must not hold this process's connection mutex while it waits, or every
/// request that reads the database queues behind it, although a read in WAL
/// mode never needs the write lock itself.
#[test]
fn database_reads_are_not_held_behind_maintenance_waiting_on_a_foreign_write_lock() {
    let mut contended = Contended::start();
    let mut latencies = Vec::new();
    contended.hold_write_lock(|aft| {
        let started = Instant::now();
        let response = aft.send_with_timeout(
            &json!({
                "id": "read",
                "command": "db_get_state",
                "params": { "key": "contention-probe" },
            })
            .to_string(),
            Duration::from_secs(30),
        );
        latencies.push(started.elapsed());
        assert_eq!(response["success"], true, "read failed: {response:?}");
    });
    contended.finish();

    assert!(latencies.len() >= 5, "only {} reads ran", latencies.len());
    let slowest = latencies.iter().max().copied().unwrap_or_default();
    assert!(
        slowest < READ_BUDGET,
        "a read waited {slowest:?}, not under {READ_BUDGET:?}, behind maintenance: {latencies:?}"
    );
}

/// Retention runs under the same contention without a warning per attempt:
/// losing the connection mutex or the write lock is routine for a
/// maintenance task and is logged at debug level.
#[test]
fn retention_does_not_warn_per_attempt_while_another_process_holds_the_write_lock() {
    let mut contended = Contended::start();
    let log_start = contended.hold_write_lock(|_| {});
    let held_log = contended.log_since(log_start);
    assert!(
        held_log.contains("bash task retention"),
        "retention never ran during the hold; child log:\n{held_log}"
    );

    let log = contended.finish();
    let warnings = warn_lines(&log, "retention");
    assert!(
        warnings.is_empty(),
        "retention warned under contention: {warnings:#?}"
    );
}
