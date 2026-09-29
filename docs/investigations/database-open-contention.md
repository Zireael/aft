# Deferred database open under cross-process contention

## What changed

`b1e87c9d9` moved `configure_database_runtime` from configure's synchronous prefix to the post-ack `DatabaseRuntime` maintenance stage. It also changed failed initialization from a warning and cleared pools (the old "running with JSON-only persistence" path) into a failed readiness state that refuses tools. `3c9f37c46` narrowed that gate to persistence-dependent tools and added a ten-second off-executor wait. Neither change added a SQLite-open retry.

Before these changes, `App::open_db` held a **process-local** database-slot mutex, then `db::open` created a connection, applied PRAGMAs, and ran migrations. The existing busy timeout was five seconds, installed before `journal_mode=WAL`. Migration steps used `BEGIN IMMEDIATE` and re-read the schema version after taking the write lock. There was no cross-process file lock or application retry loop. Moving the open did not remove such protection; making every open error permanent exposed transient contention as a latched readiness failure. The old path was not proof that every concurrent open succeeded.

## SQLite failure boundary

Connection creation precedes `PRAGMA journal_mode=WAL`. Switching a rollback-journal database to WAL requires locks on the database; another process can prevent that transition. SQLite can return BUSY from journal-mode transitions without invoking the busy handler (notably lock-upgrade/deadlock avoidance). A busy timeout is consequently not an application retry policy. Even when the handler is invoked, a lock held beyond five seconds exhausts the old timeout.

With the retry loop disabled and the old five-second handler restored, the original two-process concurrent test failed on repetition 7 of a 30-run loop. The added step attribution reported **`sqlite PRAGMA journal_mode=WAL: database is locked`**. The whole failing repetition finished in 2.28 seconds, including process startup/shutdown, so the five-second handler did not cover this failure. This pins the reproduced concurrent-open failure to WAL setup, not connection creation or `BEGIN IMMEDIATE`.

The original unqualified `sqlite error: database is locked` response did not identify the statement: it excluded the separately formatted `MigrationFailed` path but could not distinguish PRAGMAs from initial schema setup. PRAGMA failures now preserve their exact step in `OpenError`. Deterministic regressions hold an exclusive rollback-journal lock through SQLite, exercising the WAL transition rather than assuming that connection creation takes the lock.

## Recovery policy

Initialization retries only SQLite BUSY and LOCKED primary codes (including extended codes and migration wrappers). It reuses the owning connection, with exponential sleep from 10 to 200 ms and a ten-second retry budget. Each initialization busy-handler wait is capped at 100 ms and remaining budget; a successful connection regains its ordinary five-second busy timeout. Completed migration steps remain committed; failed transactions roll back before another attempt. Non-transient errors are returned immediately.

Readiness distinguishes permanent failure from busy exhaustion. Exhaustion is reported once as `database_unavailable` with `retryable: true`, without performing the requested operation. A subsequent persistence call claims one new attempt. Standalone dispatch retries synchronously; subc schedules a `MaintenanceCommit` job so read-only calls remain available and config-changing binds cannot race pool installation. Its generation check prevents an old queued retry from overwriting a newer bind. Subc callers use the existing deadline-bounded wait outside executor admission. No raw database, WAL, or SHM descriptors are opened for locking or probing.

## Verification

The integration regressions release an exclusive lock after six seconds (past the old five-second handler, within the new budget), and hold it through exhaustion before releasing it and retrying without rebind. The wire-admission regression also exercises exhaustion and the next call's maintenance retry. Existing non-transient failure/rebind and deferred readiness tests retain their contracts.

The macOS repetition gate passed **100/100 runs** on the delivered implementation. It uses the existing two-process test, without test-level retries or accepting refusal responses:

```sh
for i in $(seq 1 100); do
  echo "concurrent run $i/100"
  CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo test -q \
    -p agent-file-tools --test integration \
    state_commands_test::db_set_host_state_concurrent_insert -- --exact || exit $?
done
```

Negative controls restore the old single-attempt initialization with a five-second busy handler, and separately force busy exhaustion into the permanent-failure readiness state. The first makes `deferred_database_open_retries_journal_lock_until_release` fail with the WAL lock error; the second makes `deferred_database_open_busy_exhaustion_retries_without_rebind` fail on `retryable: false`. Both controls are restored before verification of the delivered implementation.
