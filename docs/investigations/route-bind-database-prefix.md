# Route-bind configure prefix and database readiness

## Scope of the delivered change

Database initialization is now a named `database_runtime` configure-maintenance stage, after the bind acknowledgement and before session replay. It no longer contributes to the misleading `index_loading_state` acknowledgement phase. The remaining Git/artifact-identity pipeline is **not** moved in this change: doing so changes bind rejection into post-bind degraded-index behavior and requires separate design approval.

A configured root publishes a nonblocking persistence-readiness state. Only persistence-dependent tools wait for it. Subc parks those calls on an async notification outside the executor for at most 10 seconds from frame receipt (or a supplied absolute `deadline_ms`, if sooner). They proceed normally if opening completes; only expiration returns `database_initializing` with `retryable: true`. If opening fails they receive `database_unavailable`, including the database path and underlying error and explicitly stating that reads still work. A subsequent configure retries the open even for identical configuration. Successful initialization installs the shared handle in both the backup store and bash registry before clearing the gate. An unconfigured library context retains its previous in-memory behavior.

The subc wait runs **before executor submission**, permission asks, and bash spawning, without blocking the frame loop. Completed waits re-enter normal route validation and dispatch; Cancel aborts an outstanding wait. The queue is bounded to 256 calls; excess calls receive the ordinary retryable readiness refusal immediately. A second guard at tool preparation and standalone dispatch covers requests admitted before a configuration change. The standalone synchronous dispatch guard refuses pending persistence operations rather than parking an executor worker. Read-only tools run normally during opening and after an open failure. `persistence_gate.rs` is the single classification table for public names, `aft_` aliases, and native command names. A test iterates the actual advertised mutating tools and requires an explicit classification for each.

## What the recorded phases actually measure

The following line references are against baseline `5ab60863b7efb3c839a90abd01e58a5fb45cb672`, so they remain unambiguous as the implementation moves.

### `index_loading_state`

`crates/aft/src/commands/configure.rs:3516–3686` is a large aggregate, not an index-file load timer:

- It clears root workspace-package caches, takes index/receiver locks, adopts or retires generations, clears pending update queues, and drops superseded resident indices. Dropping a large index can itself consume CPU.
- `adopt_resident_semantic_index_if_available` at `3800` looks for an already resident shared semantic index. `schedule_missing_artifact_loads` at `3924` takes the root artifact-reload guard, checks missing planes, and installs gated worker receivers.
- `schedule_artifact_loads` at `4157` prepares keys, channels and workers. Actual search deserialization is **after** `start_rx.recv()` at `4234`; the start sender is released from post-ack maintenance. The owner cold-build permit is acquired inside that worker, not by the configure prefix. Semantic loading likewise runs behind its start gate. Borrow-only search reads bypass the cold-build limiter. Existing configure tests cover both saturated-limiter paths.
- `refresh_changed_workspace_manifests` and the runtime preparation at the end of the phase also belong to this timing interval.
- Critically, `configure_database_runtime` is called at **3686**, before the next `maintenance_enqueue` phase starts. Its implementation at **5499–5526** calls `App::open_db` (`context.rs:2329–2348`). `open_db` takes the **process-shared database-slot mutex** and retains it through `db::open`. A root can therefore wait for another root's open, not just its own I/O. A resident matching handle short-circuits; a different or cold path opens synchronously under the same mutex.
- `db/mod.rs:396–406` creates directories, opens SQLite, applies PRAGMAs and runs migrations. `428–429` installs a **5,000 ms SQLite busy timeout** before switching to WAL; migrations perform SQL and disk writes. CPU starvation, SQLite locking, slow disk and another root holding the slot can all lengthen this phase. There is no aggregate five-second bound on the whole operation.

The live 21.8 s sample alone cannot prove how much was database open versus another operation inside this aggregate: no stack sample or per-subphase timing accompanied it. The patch removes a concrete synchronous disk/lock dependency and tests that dependency deterministically; it does not claim to reproduce or uniquely attribute the live stall.

### `worktree_probe`

The phase begins at `configure.rs:3027`, calling `detect_worktree_bridge` (`1338–1394`). A valid topology memo avoids the subprocess but still validates the `.git` marker. On a miss it invokes `git -C <root> rev-parse --path-format=absolute --git-dir --git-common-dir`, waits synchronously for process output, canonicalizes both paths, and caches the result. It does not use the cold-build limiter. Process spawn/scheduling, Git startup, and filesystem metadata work explain why this can take roughly a second under load even when the repository is small. If probing fails for a root whose `.git` entry is a file, it may reuse existing artifacts but may not build or publish shared artifacts.

### `cache_key_resolve`

`configure.rs:3293–3323` calls `AppContext::memoized_artifact_cache_key_for_configure` (`context.rs:4446–4476`). The per-context key map short-circuits a hit. On a miss, `search_index.rs:5489–5552`:

1. Stats Git markers and resolves the repository root-commit identity.
2. `repo_root_commit_with_retry` (`6147–6175`) can invoke `git rev-list --max-parents=0 HEAD` up to three times, with 50/100 ms backoffs for ambiguous failures (`git_root_commit_once_real`, `6235` onward). It waits for each subprocess; the sleeps are not a subprocess deadline.
3. Derives the checkout/subfolder artifact-family key.
4. `record_artifact_cache_key_memo` (`5750–5777`) holds a **process-global memo mutex** across loading the memo, pruning entries (including path existence checks), serialization and writing the memo file. Other roots can contend here too. If Git probing fails, a persisted memo can supply the identity; without one a Git-like root receives `cache_key_probe_failed`.

None of these steps waits for the cold-build limiter. They are real artifact-identity operations, not index deserialization.

## Queueing and index honesty

The executor already permits unchanged binds beside a running configure tail. Configure's first fast path uses the canonical-root/config identity and live runtime; its secondary equivalent-warm-config path can register a new session while the initial runtime is still pending. The regression exercises that latter case by holding `database_runtime` **before watcher startup**, then binding another session with the same inputs. The configure generation stays unchanged.

The existing search receiver is installed before acknowledgement and the index starts as absent/loading, not a false ready result. Moving database open does not change that index contract. A read-only tool arriving during database initialization follows existing index-building/loading behavior. A persistence-dependent tool waits off-executor and receives a named readiness refusal only if the bound expires or opening fails.

## Database-consumer behavior in the new window

| Consumer | Before DB initialization succeeds | After initialization |
| --- | --- | --- |
| Bash / PowerShell foreground, background and PTY, task rows, task control and completion plumbing | Subc waits on readiness before permission preflight/spawn/queue admission, refusing only on expiry/failure; standalone dispatch refuses before command execution. No new process or task row is created. | Registry receives the shared pool before readiness is published; session replay runs afterward. |
| Undo, backup history, writes/edits/deletes/moves and nested mutation operations | Subc waits before tool preparation or dispatch, then refuses on expiry/failure; a standalone refusal also bypasses post-mutation backup finalization. No new JSON-only backup is substituted for DB-backed operation in this window. | Existing BackupStore and its shared DB mirror are used. |
| Checkpoints, list/restore | Waits/refuses at the same boundary, so the configure tail selects storage before any operation. Checkpoints themselves are disk-backed, not rows in aft.db (`checkpoint.rs::persist_checkpoint_locked`). | Existing file-backed checkpoint durability and backup integration. |
| Compression events / aggregates | No new bash operation can start before readiness, so it cannot silently complete without a pool. Status remains readable and retains its existing unavailable-aggregate behavior. | Existing registry pool records compression events; status reads the shared handle. Already-running tasks on a reconfigured root retain their existing installed pool until replacement; no new lazy open is added. |
| Host/harness state (including caller-managed alert/state keys) | `db_get_*` / `db_set_*` requests wait/refuse, preventing their existing legacy-file fallback from masking pending DB initialization. | Existing `App::db` path and established legacy dual-write behavior. |
| Diagnostic alert deltas | `AlertDeltaState` is in-memory (`alert_state.rs`), not a SQLite consumer. Refused tools do not reach response finalization. Background diagnostic state is not turned into a new DB open. | Existing in-memory alert lifecycle. |
| Watcher observations | New normal watcher setup follows `database_runtime`; its observation handle comes from `App::db`. Existing watchers on a reconfigured root keep their established observations path. | Existing shared-handle persistence. |
| Process maintenance: write-ledger folding, retention sweeps, channel-zero health | These use an optional **existing** process-shared handle; absence means no maintenance target, not a successful tool write. They neither accept a user mutation nor create a connection because of this change. | Existing process-level maintenance/health behavior. |

Failed opening stops that root's configure-maintenance job before session replay, and queued session-only replays are refused/forgotten rather than replayed without persistence. The next equivalent configure takes the full path to retry. Existing independent CLI/standing-root/GitHub cache database opens remain separate concerns. Local filesystem reads do not require the new gate. External-root standing metadata and GitHub URL reads have the pre-existing SQLite paths detailed below; these remain available and are not covered by the root readiness wait. This change does not redesign all database connection ownership across the application.

## Optional read-side SQLite connections (unchanged)

Two existing read-side adapters use aft.db without the root's shared handle. Both open **SQLite connections**, not raw file descriptors to aft.db, its WAL, or its SHM file:

- `commands/semantic_search/mod.rs:610–643`, `standing_snapshot_metadata`, first stats aft.db (`is_file`), then calls `crate::db::open`. When the database does not exist it immediately returns default metadata. Otherwise it synchronously opens and queries standing-root freshness metadata. It does not consult the root readiness flag: while configure's open is pending it can wait on SQLite/filesystem locking, then either succeed or return default metadata if open/query fails. The fallback is no standing-snapshot metadata, not a tool-wide persistence refusal.
- `github_read/cache.rs:74–95`, `SqliteGithubReadCacheStore::connection`, also calls `crate::db::open` for lookup and upsert. Lookup failure is logged and live fetch continues without cached fallback (`403–422`). Cache write failure is logged without converting a successful live document into a failed read (`531–544`). These calls can block during SQLite opening/queries before the fallback takes effect; a live-fetch completion can likewise wait for its cache upsert. They do not wait on the new root readiness notification.

The traced production call chain for both is `db::open` (`db/mod.rs:396–406`) → `TrackedConnection::open/open_attributed` (`db/lifecycle.rs:480–490`) → **`rusqlite::Connection::open(path)`**. Directory creation and metadata checks do not open a raw DB descriptor. The connection wrapper registers identity using SQLite's connection; no raw `File::open`/`fs::read` of the database/WAL/SHM appears in these production paths. Thus these are additional SQLite-managed connections, not the raw-descriptor close hazard that can discard POSIX locks held by the process. The existing test-only `db/lifecycle.rs::tests::shm_checkpoint_state` reads SHM bytes, but is not on either production path.

Both adapters inherit `db::open`'s synchronous PRAGMAs/migrations and 5-second SQLite busy timeout. That timeout is not an aggregate bound on the whole adapter, and neither shares the `App::open_db` slot mutex. Redesigning these optional adapters to use existing/read-only handles or skip cache access under pressure remains separate follow-up work, explicitly left unchanged in this revision.

## Regression and timing method

`subc::tests::route_bind_acks_before_database_open_and_rebinds_beside_blocked_open` uses the real bind executor and a root-scoped gate **inside `configure_database_runtime`**, not merely a gate in its new caller. Consequently moving the open back to the prefix makes the first bind fail the two-second bound. It verifies:

- first bind acknowledgement before any open attempt;
- a pending, index-less search receiver rather than false ready state;
- deadline-bounded wire-level refusal for bash, safety, writes and state while post-ack database initialization has not started;
- a real read returning source content while the database stage is held, with all listed read-only tools classified as independent;
- bash waiting for a shortened 150 ms request deadline before refusal, without an aft.db file appearing;
- bash proceeding through normal dispatch when the blocked database open is released within its deadline;
- a second equivalent bind acknowledged while opening is blocked;
- the same configure generation and a real DB handle/file after release.

`subc::tests::failed_database_open_refuses_tools_and_rebind_retries_persistence` makes aft.db a directory, checks `database_unavailable` with path/error detail for bash while a real read still succeeds, removes the obstruction and proves an identical rebind retries and installs persistence.

The database-deferral implementation in commit `cbd9c6371` measured first/equivalent binds at **179.45/15.26 ms** and **100.55/2.54 ms** while database opening was blocked. These measurements precede the selective, notification-based tool wait added in the follow-up revision. Restoring the synchronous `configure_database_runtime` call to the pre-ack configure prefix made the regression fail at **2.002 s** waiting for database open. Disabling the persistence gate made the same regression fail at the first wire bash request (`immediate tool refusal: Empty` and the unexpected-dispatch assertion). Both temporary test changes were reverted to the proposed implementation before the final green test. These are isolated test timings, not fleet latency guarantees. Artificial CPU saturation was deliberately not added: this is a shared host, and long compilation alone was taking minutes. No load or request was directed at the live daemon.

The selective-gate revision also verifies `every_advertised_mutating_tool_has_explicit_persistence_classification` against the actual served catalog. Removing `write` from the classification made that exact test fail with `new mutating tool write needs an explicit persistence classification`. Replacing notification waiting with an immediate return made `database_wait_notification_releases_only_persistence_calls_and_cancel_aborts_wait` fail its pending-wait assertion. Each temporary test change ran as an isolated exact test and was then reverted to the proposed implementation. The revised blocked-open wire test additionally executes a real background bash after notification and verifies its row in `bash_tasks`.

## Follow-up proposal: pending artifact identity (not implemented)

Move topology probing, artifact-family key resolution/memo persistence, artifact-owner claiming, storage capability probing, resident adoption and workspace-manifest refresh into a generation-tagged maintenance initialization stage. Bind would commit only validated root/config/session identity and install a conservative pending-index state. Before implementing this, approve the observable contract:

- `cache_key_probe_failed` and `artifact_owner_unavailable`, currently bind failures, would become named **post-ack degraded artifact states**. Topology failures and network-FS writer restrictions would likewise be published after ack rather than folded into prefix decisions.
- Pending must mean **no artifact writer capability and no fabricated path-derived family key**. All query-triggered reloads, cache accessors, view loading and persistence must honor it; otherwise a tool could derive a key itself or publish into the wrong family.
- Grep/search should either use a bounded filesystem fallback labelled `Loading`/`Building` with explicit completeness, or return `artifact_identity_pending` when a fallback is not safe. Semantic search, callgraph navigation and inspect should report their named unavailable/loading planes, never a clean empty result. Root-local read/glob/outline could proceed only through paths that do not trigger artifact setup; otherwise return the same retryable pending error. Bash and mutation/backup/checkpoint tools should proceed only once independent DB/storage/config initialization is ready and no index writer is started as a side effect.
- A failed stage must release pending receivers into a named failed/degraded state, allow bounded retry, preserve per-session replay, and prevent stale generations from publishing ownership/capabilities.
- Same-input binds must attach to the pending generation, not enqueue duplicate identity work. Changed root, harness, storage or config must supersede it with lifecycle checks. Explicitly decide whether config files are re-resolved at each reconnect and whether any identity failure should still reject a bind.
- Add blocked-Git, blocked memo-lock and blocked owner-claim wire regressions, including changed-config supersession and tool calls before readiness. Measure each maintenance subphase separately so another catch-all phase name cannot conceal disk work.
