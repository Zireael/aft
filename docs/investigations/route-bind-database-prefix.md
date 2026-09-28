# Route-bind configure prefix and database readiness

## Scope of the delivered change

Database initialization is now a named `database_runtime` configure-maintenance stage, after the bind acknowledgement and before session replay. It no longer contributes to the misleading `index_loading_state` acknowledgement phase. The remaining Git/artifact-identity pipeline is **not** moved in this change: doing so changes bind rejection into post-bind degraded-index behavior and requires separate design approval.

A configured root publishes a nonblocking persistence-readiness state. While opening, tool requests fail with `database_initializing` and `retryable: true`, without executing their operation. If opening fails they fail with `database_unavailable`; a subsequent configure retries the open even for identical configuration. Successful initialization installs the shared handle in both the backup store and bash registry before clearing the gate. An unconfigured library context retains its previous in-memory behavior.

The subc gate runs **before executor submission**, permission asks, and bash spawning. A second guard at tool preparation and standalone dispatch covers requests admitted before a configuration change. This is a fail-fast policy, not an unbounded wait behind the database maintenance worker. Configure, ping and version remain available. Conservatively, the readiness gate covers all tool calls, including read-only tools, rather than maintaining a fragile list of tools whose finalizers or nested calls might persist state.

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

The existing search receiver is installed before acknowledgement and the index starts as absent/loading, not a false ready result. Moving database open does not change that index contract. A tool arriving during database initialization receives the named persistence-readiness refusal instead of running with missing persistence. Once initialization succeeds it follows the existing index-building/loading behavior.

## Database-consumer behavior in the new window

| Consumer | Before DB initialization succeeds | After initialization |
| --- | --- | --- |
| Bash / PowerShell foreground, background and PTY, task rows, task control and completion plumbing | Subc refuses before permission preflight/spawn/queue admission; standalone dispatch refuses before command execution. No new process or task row is created. | Registry receives the shared pool before readiness is published; session replay runs afterward. |
| Undo, backup history, writes/edits/deletes/moves and nested mutation operations | Refused before tool preparation or dispatch; the standalone refusal also bypasses post-mutation backup finalization. No new JSON-only backup is substituted for DB-backed operation in this window. | Existing BackupStore and its shared DB mirror are used. |
| Checkpoints, list/restore | Refused at the same boundary, so the configure tail selects storage before any operation. Checkpoints themselves are disk-backed, not rows in aft.db (`checkpoint.rs::persist_checkpoint_locked`). | Existing file-backed checkpoint durability and backup integration. |
| Compression events / aggregates | No new bash operation can start, so it cannot silently complete without a pool. Status/aggregate requests are also refused. | Existing registry pool records compression events; status reads the shared handle. Already-running tasks on a reconfigured root retain their existing installed pool until replacement; no new lazy open is added. |
| Host/harness state (including caller-managed alert/state keys) | `db_get_*` / `db_set_*` requests are refused, preventing their existing legacy-file fallback from masking pending DB initialization. | Existing `App::db` path and established legacy dual-write behavior. |
| Diagnostic alert deltas | `AlertDeltaState` is in-memory (`alert_state.rs`), not a SQLite consumer. Refused tools do not reach response finalization. Background diagnostic state is not turned into a new DB open. | Existing in-memory alert lifecycle. |
| Watcher observations | New normal watcher setup follows `database_runtime`; its observation handle comes from `App::db`. Existing watchers on a reconfigured root keep their established observations path. | Existing shared-handle persistence. |
| Process maintenance: write-ledger folding, retention sweeps, channel-zero health | These use an optional **existing** process-shared handle; absence means no maintenance target, not a successful tool write. They neither accept a user mutation nor create a connection because of this change. | Existing process-level maintenance/health behavior. |

Failed opening stops that root's configure-maintenance job before session replay, and queued session-only replays are refused/forgotten rather than replayed without persistence. The next equivalent configure takes the full path to retry. Existing independent CLI/standing-root/GitHub cache database opens remain separate concerns; no newly admitted route tool can reach those consumers during the pending/failed window because the transport gate runs first. This change does not redesign all database connection ownership across the application.

## Regression and timing method

`subc::tests::route_bind_acks_before_database_open_and_rebinds_beside_blocked_open` uses the real bind executor and a root-scoped gate **inside `configure_database_runtime`**, not merely a gate in its new caller. Consequently moving the open back to the prefix makes the first bind fail the two-second bound. It verifies:

- first bind acknowledgement before any open attempt;
- a pending, index-less search receiver rather than false ready state;
- immediate wire-level refusal for bash, safety, writes, state and inspect before maintenance starts;
- bash refusal within 500 ms even while the database stage is held, without an aft.db file appearing;
- a second equivalent bind acknowledged while opening is blocked;
- the same configure generation and a real DB handle/file after release.

`subc::tests::failed_database_open_refuses_tools_and_rebind_retries_persistence` makes aft.db a directory, checks `database_unavailable` on the wire, removes the obstruction and proves an identical rebind retries and installs persistence.

Unmodified runs measured first/equivalent binds at **179.45/15.26 ms** and, after restoring the mutation controls, **100.55/2.54 ms**, while database opening was blocked. Restoring the old prefix call made the regression fail at **2.002 s** waiting for database open. Disabling the persistence gate made the same regression fail at the first wire bash request (`immediate tool refusal: Empty` and the unexpected-dispatch assertion). Both controls were restored from the staged implementation before the final green test. These are isolated test timings, not fleet latency guarantees. Artificial CPU saturation was deliberately not added: this is a shared host, and long compilation alone was taking minutes. No load or request was directed at the live daemon.

## Follow-up proposal: pending artifact identity (not implemented)

Move topology probing, artifact-family key resolution/memo persistence, artifact-owner claiming, storage capability probing, resident adoption and workspace-manifest refresh into a generation-tagged maintenance initialization stage. Bind would commit only validated root/config/session identity and install a conservative pending-index state. Before implementing this, approve the observable contract:

- `cache_key_probe_failed` and `artifact_owner_unavailable`, currently bind failures, would become named **post-ack degraded artifact states**. Topology failures and network-FS writer restrictions would likewise be published after ack rather than folded into prefix decisions.
- Pending must mean **no artifact writer capability and no fabricated path-derived family key**. All query-triggered reloads, cache accessors, view loading and persistence must honor it; otherwise a tool could derive a key itself or publish into the wrong family.
- Grep/search should either use a bounded filesystem fallback labelled `Loading`/`Building` with explicit completeness, or return `artifact_identity_pending` when a fallback is not safe. Semantic search, callgraph navigation and inspect should report their named unavailable/loading planes, never a clean empty result. Root-local read/glob/outline could proceed only through paths that do not trigger artifact setup; otherwise return the same retryable pending error. Bash and mutation/backup/checkpoint tools should proceed only once independent DB/storage/config initialization is ready and no index writer is started as a side effect.
- A failed stage must release pending receivers into a named failed/degraded state, allow bounded retry, preserve per-session replay, and prevent stale generations from publishing ownership/capabilities.
- Same-input binds must attach to the pending generation, not enqueue duplicate identity work. Changed root, harness, storage or config must supersede it with lifecycle checks. Explicitly decide whether config files are re-resolved at each reconnect and whether any identity failure should still reject a bind.
- Add blocked-Git, blocked memo-lock and blocked owner-claim wire regressions, including changed-config supersession and tool calls before readiness. Measure each maintenance subphase separately so another catch-all phase name cannot conceal disk work.
