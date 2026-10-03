# Subc safety parity timeout investigation

## Scope and diagnostic capture

Investigated the Linux `opencode-subc` failure in run 36510924104 from main
`2392b1b0534c5c3d28427f906f7f1b7cd6f33ccd`. No cause has been established.
The local runs below are macOS arm64, not a reproduction of Linux CI.

The end-to-end test rig launches a subc daemon and its AFT module. It now prints
its diagnostic directory and keeps logs outside its temporary project fixture. Set `AFT_SUBC_E2E_LOG_DIR` to choose the parent directory;
the default is `aft-subc-e2e-logs` under the OS temporary directory. Each rig
has a unique `rig-*` directory containing:

- `aft.stderr.log`: on Unix, the module's stderr is redirected directly to an
  file opened in append mode by a shell wrapper. `exec` replaces the shell with
  AFT while preserving its process ID and protocol file descriptors. This captures Rust panic output as well as structured logs,
  without depending on the daemon forwarding or draining the module's stderr.
- `subc-core.stderr.log`: the daemon's stderr, appended across startup retries
  and explicit restarts rather than asynchronously rewriting earlier chunks.

Logs survive both ordinary cleanup and startup failure, and are available even
if Bun is killed before teardown. Windows still launches the module directly;
the direct module-stderr capture is Unix-only. The Linux plugin matrix uploads
these directories on failure as `subc-e2e-logs-<suite>-linux` (14-day retention).
Only the two stderr logs are uploaded, not fixture contents, configuration files
or databases. Local retained log directories can be removed manually.

## Slow-open reproduction

The existing database-stage gate is Rust-test-only. The new debug-build-only
`AFT_TEST_DATABASE_OPEN_DELAY_MS` hook sleeps immediately before `App::open_db`,
after releasing the backup lock. It caps the delay at 60 seconds and logs the
actual delay and database path. The rig explicitly passes the variable to the
module. Release builds do not implement the hook.

Build and run from the repository root:

```sh
CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build -p agent-file-tools --bin aft
bun install --frozen-lockfile
bun run --cwd packages/aft-bridge build
AFT_BINARY_PATH="$PWD/target/debug/aft" \
  AFT_SUBC_E2E_LOG_DIR=/tmp/aft-subc-investigation \
  AFT_TEST_DATABASE_OPEN_DELAY_MS=2000 \
  CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= \
  bun test --cwd packages/opencode-plugin --timeout 30000 \
  src/__tests__/e2e/ --test-name-pattern 'subc transport parity sweep'
```

Local observations:

- No delay: 69/69 passed (40.72 seconds).
- 250 ms per database stage: 69/69 passed.
- 2,000 ms per database stage: 69/69 passed (132.38 seconds).
- 2,000 ms per database stage plus four confirmed-live CPU burners: 69/69
  passed (135.21 seconds), including all safety/undo tests. No module panic was
  found in the captured module stderr.
- An initial attempt to add CPU contention used Python multiprocessing from
  stdin; its macOS spawn workers failed before doing work. Those two delayed
  runs are **not** counted as load tests. The subsequent load run uses four
  independent `python3 -c` arithmetic loops, asserts that all four are alive
  before and after the suite, and terminates/joins them in a `finally` block.

The logs for these experiments are retained locally at
`/tmp/aft-subc-pool505/{baseline,delay-250,delay-2000,load-delay-2000}`.
The delay lines in the module logs confirm the hook executed; this is not just
an environment setting presumed to have propagated.

## Readiness, lock and panic inspection

The key question is whether a request waiting for database initialization can
prevent that initialization from completing. The readiness signal lets requests
wait without occupying the worker threads that execute tools and maintenance.

- `context.rs::wait_for_database_runtime` creates and enables a pinned
  `Notify::notified()` future **before** checking the acquire-loaded pending
  state. `finish_database_runtime` release-stores the result before
  `notify_waiters()`. The suspected check-before-registration lost-wakeup
  ordering is not present in this implementation.
- `subc/persistence.rs::DatabaseWaits::defer` creates a Tokio task to await that
  notification. Its wait holds no backup/DB mutex and has a ten-second deadline.
  `subc/mod.rs` selects completed waits alongside incoming frames, before
  executor admission, and marks resumed frames so they are not deferred again.
  Thus this wait itself does not consume an executor worker needed by opening.
- `configure_database_runtime` takes the backup lock to set the project key,
  releases it before `App::open_db`, then installs the returned handle in the
  backup and bash registries before publishing readiness. `App::open_db` holds
  the process DB-slot mutex across SQLite opening, but releases it when returning
  the shared handle. This inspection did not establish a reverse lock cycle.
- `commands/edit_history.rs::handle_edit_history` does hold the backup lock
  through `BackupStore::history`. History takes a stack disk lock and may then
  acquire the database connection mutex on its DB fallback path. These remain
  possible places to investigate with a hung-process stack, not demonstrated
  causes of the CI failure.
- `executor/mod.rs::worker_loop` catches panics from `run_lane_job` and builds a
  panic response; its completion-guard `expect` calls are outside that catch.
  `DatabaseWaits::next` ignores failed joined tasks. Neither observation proves
  a panic occurred. Direct stderr capture and `RUST_BACKTRACE=1` preserve evidence
  of a future panic even if it bypasses structured logging.

The next failing CI run should be inspected using the uploaded logs before a
retry; passing local runs do not rule out Linux-specific locking, scheduling or
process-lifecycle failures. No speculative synchronization fix is included.
