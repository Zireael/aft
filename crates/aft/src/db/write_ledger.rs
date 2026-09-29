//! Durable per-minute storage for the process write ledger.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::db::TrackedConnection;

/// How long a fold waits for another process's write lock on `aft.db`.
///
/// The fold holds the process's shared connection mutex while it waits, so
/// every request needing the database waits too. A quarter second matches the
/// request-path retry wait; past it the fold is deferred to the next run.
const FOLD_BUSY_WAIT: Duration = Duration::from_millis(250);

/// Folds that did not run because the database was busy, either in this
/// process (connection mutex held) or in another (SQLite write lock held).
/// Deferred counters stay pending in memory and are written by the next fold.
static FOLDS_DEFERRED: AtomicU64 = AtomicU64::new(0);

/// Number of minute folds deferred by database contention since process start.
pub fn folds_deferred_total() -> u64 {
    FOLDS_DEFERRED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Fold at most once per minute without delaying a request loop on the shared DB.
///
/// A fold that finds the database busy is deferred, not failed: the in-memory
/// counters only advance their folded marks after a commit, so the next run
/// writes everything this one could not.
pub fn maybe_spawn_fold(db: Option<Arc<Mutex<TrackedConnection>>>) {
    static IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static LAST: std::sync::OnceLock<Mutex<Option<Instant>>> = std::sync::OnceLock::new();
    use std::sync::atomic::Ordering;

    let Some(db) = db else {
        return;
    };
    let mut last = LAST
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|value| value.elapsed() < crate::db::maintenance_interval())
        || IN_FLIGHT.swap(true, Ordering::AcqRel)
    {
        return;
    }
    *last = Some(Instant::now());
    drop(last);

    let spawn = std::thread::Builder::new()
        .name("aft-write-ledger-fold".to_owned())
        .spawn(move || {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or(0);
            match db.try_lock() {
                Ok(mut conn) => {
                    match crate::db::with_busy_wait(&mut *conn, FOLD_BUSY_WAIT, |conn| {
                        crate::write_ledger::fold_minute(conn, now_ms)
                    }) {
                        Ok(()) => crate::slog_debug!("write ledger minute fold committed"),
                        Err(error) if crate::db::is_busy_error(&error) => {
                            FOLDS_DEFERRED.fetch_add(1, Ordering::Relaxed);
                            crate::slog_debug!(
                                "write ledger minute fold deferred: {error}; pending counters stay in memory for the next fold"
                            );
                        }
                        Err(error) => {
                            crate::slog_warn!("write ledger minute fold failed: {error}")
                        }
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    FOLDS_DEFERRED.fetch_add(1, Ordering::Relaxed);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    log::warn!("write ledger minute fold skipped: database mutex poisoned");
                }
            }
            IN_FLIGHT.store(false, Ordering::Release);
        });
    if let Err(error) = spawn {
        IN_FLIGHT.store(false, Ordering::Release);
        log::warn!("write ledger minute fold could not start: {error}");
    }
}
