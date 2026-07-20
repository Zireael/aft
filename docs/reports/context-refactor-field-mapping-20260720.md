# `context.rs` `AppContext` Refactor — Field Mapping & Migration Design

**Date:** 2026-07-20
**File:** `crates/aft/src/context.rs`
**Conflict:** 3 hunks — `AppContext` struct definition + constructor (`AppContext::new`)
**Cross-ref:** [merge-conflict-analysis-upstream-20260720.md](./merge-conflict-analysis-upstream-20260720.md) §3.3

---

## 1. The Refactor at a Glance

Upstream (`cortexkit/main`) performed a **concurrency model overhaul** of `AppContext` while the fork was building its semantic retrieval layer (PR #87). The two changes are **orthogonal in intent** but **collide in the struct definition**.

| Aspect | Fork (HEAD) | Upstream (cortexkit/main) |
| :--- | :--- | :--- |
| Interior mutability | `std::cell::RefCell` (single-threaded) | `parking_lot::Mutex` / `std::sync::RwLock` (multi-threaded) |
| Process-wide services | Flat in `AppContext` | Extracted to new `pub struct App` (line 1024) |
| `db` field | `RefCell<Option<Arc<Mutex<Connection>>>>` in `AppContext` | `parking_lot::Mutex<Option<(PathBuf, Arc<Mutex<Connection>>)>>` in `App` |
| In-memory `callgraph` | `RefCell<Option<CallGraph>>` | **Removed** — replaced by `callgraph_writer: AtomicBool` |
| `callgraph_store` type | `RefCell<Option<CallGraphStore>>` (mutable) | `RwLock<Option<Arc<ReadonlyCallGraphStore>>>` (immutable shared) |
| Semantic epoch/circuit | Not present | ~15 new `AtomicU64`/`AtomicBool`/`Arc<Mutex>` fields |
| Diagnostics layer (PR #87) | `semantic_search_metrics`, `semantic_diagnostics_logger`, `semantic_warning_dedup` | **Not present** |
| `inspect_manager` | `Arc<InspectManager>` | `Arc<InspectManager>` (identical) |

**Guiding principle for the merge:** Take upstream's `App` + `AppContext` split and `parking_lot`/`RwLock` concurrency as the base. Re-add the fork's three diagnostics fields using upstream's concurrency primitives. The `inspect_manager` field needs no change.

---

## 2. The Three Conflict Hunks

### Hunk 1 — `AppContext::new` channel initialization (~line 1797)

| Fork (HEAD) | Upstream |
| :--- | :--- |
| `mpsc::channel()` for `configure_warnings` | `crossbeam_channel::unbounded()` |
| `progress_sender: mpsc::Sender` type | `progress_sender: SharedProgressSender` (Arc-Mutex) |

**Resolution:** Take upstream's `crossbeam_channel::unbounded()` + `SharedProgressSender` type. This is a pure infrastructure upgrade.

### Hunk 2 — `AppContext` struct field block (~line 1855)

This is the large hunk. The fork adds semantic fields; upstream replaces the entire field set with the `parking_lot`/`RwLock` version. **Both sets must be reconciled** — see §3 for the full field map.

### Hunk 3 — `AppContext::new` constructor initialization (~line 1870)

The constructor body. Must match the resolved struct from Hunk 2. Each field's initialization expression must use the correct primitive constructor (`parking_lot::Mutex::new`, `RwLock::new`, `Arc::new`, `AtomicU64::new`, etc.).

---

## 3. Complete Field Mapping

### 3.1 Fields that are IDENTICAL on both sides (no action needed)

| Field | Type (both sides) | Notes |
| :--- | :--- | :--- |
| `inspect_manager` | `Arc<InspectManager>` | Same type, same semantics. Accessor `pub fn inspect_manager(&self) -> Arc<InspectManager>` returns a clone on both sides. |
| `symbol_cache` | `SharedSymbolCache` | `Arc<RwLock<SymbolCache>>` on both sides. |
| `bash_background` | `BgTaskRegistry` | Same. |
| `gitignore` | `SharedGitignore` | `Arc<RwLock<Option<Gitignore>>>` on both sides. |
| `gitignore_generation` | `Arc<AtomicU64>` | Same. |
| `bash_compress_flag` | `Arc<AtomicBool>` | Same. |
| `filter_registry` | `SharedFilterRegistry` | `Arc<RwLock<FilterRegistry>>` on both sides. |
| `filter_registry_loaded` | `AtomicBool` | Same. |
| `last_seen_reuse_completions` | `AtomicU64` | Same. |
| `status_emitter` | `StatusEmitter` | Same. |
| `lsp_child_registry` | `LspChildRegistry` | Fork: in `AppContext`. Upstream: in `App`. See §3.4. |

### 3.2 Fields that CHANGE concurrency primitive (fork → upstream mapping)

| Fork field | Fork type | Upstream type | Migration action |
| :--- | :--- | :--- | :--- |
| `backup` | `RefCell<BackupStore>` | `parking_lot::Mutex<BackupStore>` | Take upstream. Call sites change `.borrow()`→`.lock()`, `.borrow_mut()`→`.lock()`. |
| `checkpoint` | `RefCell<CheckpointStore>` | `parking_lot::Mutex<CheckpointStore>` | Take upstream. Same lock pattern. |
| `config` | `RefCell<Config>` | `RwLock<Arc<Config>>` | **Take upstream.** This is a significant change — config is now `Arc`-shared and read via `RwLock`. All `config()` / `config_mut()` accessors must be rewritten. |
| `harness` | `RefCell<Option<Harness>>` | `parking_lot::Mutex<Option<Harness>>` | Take upstream. |
| `canonical_cache_root` | `RefCell<Option<PathBuf>>` | `parking_lot::Mutex<Option<PathBuf>>` | Take upstream. |
| `is_worktree_bridge` | `RefCell<bool>` | `parking_lot::Mutex<bool>` | Take upstream. |
| `git_common_dir` | `RefCell<Option<PathBuf>>` | `parking_lot::Mutex<Option<PathBuf>>` | Take upstream. |
| `degraded_reasons` | `RefCell<Vec<String>>` | `parking_lot::Mutex<Vec<String>>` | Take upstream. |
| `callgraph_store` | `RefCell<Option<CallGraphStore>>` | `RwLock<Option<Arc<ReadonlyCallGraphStore>>>` | **Take upstream type.** See §3.5 — the callgraph store is now read-only and shared via `Arc`. The fork's mutable `CallGraphStore` operations must adapt. |
| `callgraph_store_rx` | `RefCell<Option<Receiver<CallGraphStore>>>` | `parking_lot::Mutex<Option<Receiver<CallGraphStoreBuildEvent>>>` | Take upstream. Event type changed from `CallGraphStore` to `CallGraphStoreBuildEvent`. |
| `pending_callgraph_store_paths` | `RefCell<BTreeSet<PathBuf>>` | `PendingCallGraphStorePaths` (newtype) | Take upstream newtype. |
| `search_index` | `RefCell<Option<SearchIndex>>` | `RwLock<Option<SearchIndex>>` | Take upstream. Call sites: `.borrow()`→`.read()`, `.borrow_mut()`→`.write()`. |
| `search_index_rx` | `RefCell<Option<Receiver<SearchIndex>>>` | `RwLock<Option<Receiver<SearchIndex>>>` | Take upstream. |
| `pending_search_index_paths` | `RefCell<BTreeSet<PathBuf>>` | `parking_lot::Mutex<BTreeSet<PathBuf>>` | Take upstream. |
| `tier2_refresh_scheduler` | `RefCell<Tier2RefreshScheduler>` | `parking_lot::Mutex<Tier2RefreshScheduler>` | Take upstream. |
| `semantic_index` | `RefCell<Option<SemanticIndex>>` | `RwLock<Option<SemanticIndex>>` | Take upstream. Accessor changes from `&RefCell<...>` to `&RwLock<...>`. |
| `semantic_index_rx` | `RefCell<Option<Receiver<SemanticIndexEvent>>>` | `parking_lot::Mutex<Option<Receiver<SemanticIndexEvent>>>` | Take upstream. |
| `semantic_index_status` | `RefCell<SemanticIndexStatus>` | `RwLock<SemanticIndexStatus>` | Take upstream. All `borrow()`→`read()`, `borrow_mut()`→`write()`. |
| `pending_semantic_index_paths` | `RefCell<BTreeSet<PathBuf>>` | `Arc<parking_lot::Mutex<BTreeSet<PathBuf>>>` | **Take upstream** — now `Arc`-shared so the refresh worker can access it from a background thread. |
| `pending_semantic_corpus_refresh` | `RefCell<bool>` | `parking_lot::Mutex<bool>` | Take upstream. |
| `semantic_refresh_tx` | `RefCell<Option<Sender<SemanticRefreshRequest>>>` | `Arc<parking_lot::Mutex<Option<Sender<...>>>>` | **Take upstream** — `Arc`-wrapped for cross-thread access. |
| `semantic_refresh_event_rx` | `RefCell<Option<Receiver<SemanticRefreshEvent>>>` | `parking_lot::Mutex<Option<Receiver<...>>>` | Take upstream. |
| `semantic_refresh_worker` | `RefCell<Option<SemanticRefreshWorkerSlot>>` | `parking_lot::Mutex<Option<SemanticRefreshWorkerSlot>>` | Take upstream. |
| `semantic_embedding_model` | `RefCell<Option<EmbeddingModel>>` | `parking_lot::Mutex<Option<EmbeddingModel>>` | Take upstream. |
| `watcher` | `RefCell<Option<RecommendedWatcher>>` | `parking_lot::Mutex<Option<RecommendedWatcher>>` | Take upstream. |
| `watcher_rx` | `RefCell<Option<Receiver<WatcherDispatchEvent>>>` | `parking_lot::Mutex<Option<...>>` | Take upstream. |
| `watcher_thread` | `RefCell<Option<WatcherThreadHandle>>` | `parking_lot::Mutex<Option<WatcherThreadHandle>>` | Take upstream. |
| `lsp_manager` | `RefCell<LspManager>` | `parking_lot::Mutex<LspManager>` | Take upstream. |
| `status_bar_tier2` | `RefCell<StatusBarTier2>` | `RwLock<StatusBarTier2>` | Take upstream. |
| `tsconfig_membership` | `RefCell<TsconfigMembershipCache>` | `parking_lot::Mutex<TsconfigMembershipCache>` | Take upstream. |
| `configure_generation` | `AtomicU64` | `Arc<AtomicU64>` | Take upstream — now `Arc`-shared. |
| `configure_warnings_tx` | `mpsc::Sender<...>` | `crossbeam_channel::Sender<...>` | Take upstream. |
| `configure_warnings_rx` | `mpsc::Receiver<...>` | `crossbeam_channel::Receiver<...>` | Take upstream. |
| `progress_sender` | `SharedProgressSender` | `SharedProgressSender` | Same type, but moved to `App` in upstream (see §3.4). |

### 3.3 Fork-only fields (MUST re-add to upstream's struct)

These three fields exist **only on the fork** (PR #87 diagnostics layer). They have no upstream equivalent and must be added to upstream's `AppContext` struct using `parking_lot`/`RwLock` instead of `RefCell`.

| Fork field | Fork type | **Proposed merged type** | Rationale |
| :--- | :--- | :--- | :--- |
| `semantic_search_metrics` | `RefCell<SearchMetricsCollector>` | `parking_lot::Mutex<SearchMetricsCollector>` | Mutex (not RwLock) — metrics are write-heavy (every query updates). `parking_lot` for consistency with upstream. |
| `semantic_diagnostics_logger` | `RefCell<Option<SemanticDiagnosticsLogger>>` | `parking_lot::Mutex<Option<SemanticDiagnosticsLogger>>` | Mutex — logger is write-only from the search path. |
| `semantic_warning_dedup` | `RefCell<WarningDedup>` | `parking_lot::Mutex<WarningDedup>` | Mutex — dedup state is read+write on every search. |

**Constructor initialization for these fields:**
```rust
semantic_search_metrics: parking_lot::Mutex::new(
    crate::semantic_diagnostics::SearchMetricsCollector::new(metrics_window_size),
),
semantic_diagnostics_logger: parking_lot::Mutex::new(None),
semantic_warning_dedup: parking_lot::Mutex::new(
    crate::semantic_diagnostics::WarningDedup::new(Duration::from_secs(60)),
),
```

**Accessor method migration (3 methods to rewrite):**

| Fork accessor | Fork return type | **Merged return type** |
| :--- | :--- | :--- |
| `pub fn semantic_search_metrics(&self)` | `&RefCell<SearchMetricsCollector>` | `&parking_lot::Mutex<SearchMetricsCollector>` |
| `pub fn semantic_diagnostics_logger(&self)` | `&RefCell<Option<SemanticDiagnosticsLogger>>` | `&parking_lot::Mutex<Option<SemanticDiagnosticsLogger>>` |
| `pub fn semantic_warning_dedup(&self)` | `&RefCell<WarningDedup>` | `&parking_lot::Mutex<WarningDedup>` |

### 3.4 The `App` struct — fields moved out of `AppContext`

Upstream introduced `pub struct App` (line 1024) for process-wide services. These fields **moved** from `AppContext` (fork) to `App` (upstream):

| Fork field (in AppContext) | Upstream location (in App) | Upstream type |
| :--- | :--- | :--- |
| `db` | `App.db` | `parking_lot::Mutex<Option<(PathBuf, Arc<Mutex<Connection>>)>>` |
| `lsp_child_registry` | `App.lsp_child_registry` | `LspChildRegistry` (same type) |
| `stdout_writer` | `App.stdout_writer` | `SharedStdoutWriter` (same type) |
| `progress_sender` | `App.progress_sender` *(also referenced via AppContext)* | `SharedProgressSender` |

`App` also adds fields that didn't exist on the fork:
- `active_watchers: AtomicUsize`
- `active_actor_roots: AtomicUsize`
- `open_routes: AtomicUsize`
- `provider_factory: LanguageProviderFactory`
- `memory_contexts: parking_lot::Mutex<BTreeMap<PathBuf, Weak<AppContext>>>`

**Migration action:** Accept upstream's `App` struct as-is. Update all call sites that accessed `ctx.db()`, `ctx.lsp_child_registry()`, `ctx.stdout_writer()` to route through `ctx.app.db` (or the accessor method upstream provides on `AppContext` that delegates to `App`).

### 3.5 The in-memory `callgraph` field — REMOVED by upstream

| Fork | Upstream |
| :--- | :--- |
| `callgraph: RefCell<Option<CallGraph>>` | **Field removed.** Replaced by `callgraph_writer: AtomicBool` + `callgraph_store: RwLock<Option<Arc<ReadonlyCallGraphStore>>>` |

**Impact:** The fork has ~20 call sites using `ctx.callgraph().borrow_mut()` (see `main.rs`, `configure.rs`, `move_symbol.rs`). Upstream migrated all callgraph operations to the persisted `ReadonlyCallGraphStore`.

**Migration action:**
1. Take upstream's model (no in-memory `CallGraph` field).
2. For each fork call site using `ctx.callgraph().borrow_mut()`, convert to `ctx.callgraph_store_for_ops()` (which already exists on both sides, but returns `CallgraphStoreAccess` — see the upstream accessor).
3. The fork's `move_symbol.rs:153` (`ctx.callgraph().borrow_mut()`) needs special attention — it mutates the in-memory graph directly. Check whether upstream's `move_symbol` command uses the store instead.
4. The fork's `configure.rs:3055` (`ctx.callgraph().borrow_mut() = Some(graph)`) initializes the in-memory graph during configure — upstream's configure path must handle this via the store instead.

### 3.6 Upstream-only fields (NEW — the fork must adopt these)

These fields exist only on upstream. The fork must add them to the merged struct. They represent upstream's concurrency/lifecycle hardening that the fork hasn't seen yet.

| Upstream field | Type | Purpose |
| :--- | :--- | :--- |
| `app` | `Arc<App>` | Back-reference to process-wide services |
| `force_restrict_requests` | `parking_lot::Mutex<BTreeMap<String, usize>>` | Request restriction |
| `shared_artifacts_read_only` | `AtomicBool` | Artifact read-only flag |
| `callgraph_writer` | `AtomicBool` | Replaces in-memory `callgraph` |
| `inspect_writer` | `AtomicBool` | Inspect write flag |
| `artifact_owner_status` | `parking_lot::Mutex<Option<ArtifactOwnerStatus>>` | Artifact ownership |
| `artifact_owner_lease` | `parking_lot::Mutex<Option<ArtifactOwnerLeaseRegistration>>` | Lease registration |
| `heavy_root_work_allowed` | `Arc<AtomicBool>` | Configure-time scan gate |
| `callgraph_store_force_requested` | `AtomicU64` | Force-rebuild tracking |
| `callgraph_store_force_fulfilled` | `AtomicU64` | Force-rebuild completion |
| `callgraph_store_rx_generation` | `AtomicU64` | Receiver generation |
| `callgraph_store_rx_epoch` | `AtomicU64` | Receiver epoch |
| `callgraph_persist_epoch` | `ArtifactPublishEpoch` | Publish epoch |
| `callgraph_legacy_migration_summary_logged` | `Arc<AtomicBool>` | Migration logging |
| `search_index_rx_generation` | `AtomicU64` | Search receiver generation |
| `search_index_rx_epoch` | `AtomicU64` | Search receiver epoch |
| `search_index_rx_terminal_epoch` | `Arc<AtomicU64>` | Terminal epoch |
| `search_persist_epoch` | `ArtifactPublishEpoch` | Search publish epoch |
| `pending_tier2_paths` | `parking_lot::Mutex<BTreeSet<PathBuf>>` | Tier-2 pending paths |
| `semantic_index_rx_generation` | `AtomicU64` | Semantic receiver generation |
| `semantic_index_rx_epoch` | `AtomicU64` | Semantic receiver epoch |
| `semantic_index_rx_terminal_epoch` | `Arc<AtomicU64>` | Terminal epoch |
| `semantic_persist_epoch` | `ArtifactPublishEpoch` | Semantic publish epoch |
| `semantic_persist_lock` | `Arc<parking_lot::Mutex<()>>` | Persist serialization |
| `artifact_reload_lock` | `parking_lot::Mutex<()>` | Reload serialization |
| `semantic_cold_seed_active` | `Arc<AtomicBool>` | Cold seed gate |
| `semantic_cold_seed_generation` | `Arc<AtomicU64>` | Cold seed generation |
| `semantic_fingerprint_generation` | `Arc<AtomicU64>` | Fingerprint generation |
| `semantic_callgraph_warm_deferred` | `AtomicBool` | Warm deferral |
| `semantic_refresh_generation` | `AtomicU64` | Refresh generation |
| `semantic_refresh_epoch` | `AtomicU64` | Refresh epoch |
| `semantic_refresh_build_epoch` | `AtomicU64` | Build epoch |
| `semantic_refresh_retry_attempts` | `parking_lot::Mutex<BTreeMap<PathBuf, usize>>` | Retry tracking |
| `semantic_refresh_circuit` | `Arc<SemanticRefreshCircuit>` | Circuit breaker |
| `watcher_runtime_lock` | `parking_lot::Mutex<()>` | Watcher serialization |
| `watcher_drain_slice` | `parking_lot::Mutex<Option<WatcherDrainSliceState>>` | Drain state |
| `configure_content_generation` | `Arc<AtomicU64>` | Content generation |
| `subc_lifecycle` | `SubcLifecycleAdmission` | Subc lifecycle |
| `configure_warm_state` | `parking_lot::Mutex<ConfigureWarmState>` | Warm state |
| `configure_phase_timing` | `parking_lot::Mutex<ConfigurePhaseTiming>` | Phase timing |
| `configured_session_roots` | `parking_lot::Mutex<BTreeSet<(PathBuf, String)>>` | Session roots |
| `configure_maintenance_jobs` | `parking_lot::Mutex<VecDeque<ConfigureMaintenanceJob>>` | Maintenance jobs |
| `artifact_cache_keys` | `parking_lot::Mutex<BTreeMap<PathBuf, String>>` | Cache keys |
| `artifact_cache_key_derivations` | `AtomicU64` | Key derivations |
| `borrowed_index_cache` | `parking_lot::Mutex<BorrowedIndexCache>` | Borrowed cache |
| `worktree_bridge_cache` | `parking_lot::Mutex<BTreeMap<PathBuf, WorktreeBridgeCacheEntry>>` | Bridge cache |
| `status_bar_last_emitted` | `RwLock<Option<StatusBarCounts>>` | Last emitted bar |
| `status_bar_cached` | `RwLock<StatusBarCache>` | Cached bar |
| `compression_aggregates` | `Arc<CompressionAggregateCache>` | Compression aggregates |
| `filter_registry_rebuild_count` | `AtomicU64` | Rebuild count |
| `escalation_grants` *(unix only)* | `parking_lot::Mutex<EscalationGrantStore>` | Escalation grants |

**Migration action:** Accept all upstream-only fields verbatim. Their initialization in the constructor must match upstream's `with_app_and_provider` constructor. Do not modify their types or semantics.

---

## 4. Accessor Call-Site Migration Patterns

The concurrency primitive change (RefCell → parking_lot/RwLock) affects **every accessor call site** across the codebase. The code search found ~173 call sites across these files:

| File | Call sites | Pattern change |
| :--- | :--- | :--- |
| `main.rs` | ~40 | `.borrow()`→`.read()`, `.borrow_mut()`→`.write()` or `.lock()` |
| `commands/semantic_search.rs` | ~15 | Same |
| `commands/configure.rs` | ~10 | Same + `config()` return type change |
| `cli/warmup.rs` | ~12 | Same |
| `tests/integration/aft_search_contract_test.rs` | ~25 | Same |
| `tests/integration/inspect_*_test.rs` | ~10 | Same |
| `commands/semantic_doctor.rs` | 2 | Same |
| `commands/inspect.rs` | 2 | `inspect_manager()` — no change (Arc clone) |

### Mechanical migration rules

| Fork pattern | Merged pattern | Applies to |
| :--- | :--- | :--- |
| `ctx.semantic_index().borrow()` | `ctx.semantic_index().read()` | `RwLock` fields |
| `ctx.semantic_index().borrow_mut()` | `ctx.semantic_index().write()` | `RwLock` fields |
| `ctx.semantic_index_status().borrow()` | `ctx.semantic_index_status().read()` | `RwLock` fields |
| `ctx.semantic_index_status().borrow_mut()` | `ctx.semantic_index_status().write()` | `RwLock` fields |
| `ctx.callgraph_store().borrow()` | `ctx.callgraph_store().read()` | `RwLock` fields |
| `ctx.callgraph_store().borrow_mut()` | `ctx.callgraph_store().write()` | `RwLock` fields |
| `ctx.config()` | `ctx.config()` (returns `RwLockReadGuard<Arc<Config>>` now) | Config — deref to `&Config` works |
| `ctx.config_mut()` | **Needs rewrite** — upstream uses `ctx.config_write()` returning `RwLockWriteGuard<Arc<Config>>` | Config — may need `Arc::make_mut` |
| `ctx.callgraph().borrow_mut()` | **Remove** — use `ctx.callgraph_store_for_ops()` | In-memory callgraph removed |
| `ctx.semantic_search_metrics().borrow_mut()` | `ctx.semantic_search_metrics().lock()` | Re-added fork field (Mutex) |
| `ctx.semantic_diagnostics_logger().borrow_mut()` | `ctx.semantic_diagnostics_logger().lock()` | Re-added fork field (Mutex) |
| `ctx.semantic_warning_dedup().borrow_mut()` | `ctx.semantic_warning_dedup().lock()` | Re-added fork field (Mutex) |
| `ctx.inspect_manager()` | `ctx.inspect_manager()` — **no change** | Arc clone, identical on both sides |

### Special: `config()` return type change

The fork's `config()` returns `Ref<'_, Config>`. Upstream's returns a guard that derefs to `&Arc<Config>`. Most call sites that do `ctx.config().field` will work via deref coercion. Call sites that **rebind** the borrow (e.g., `let cfg = ctx.config(); ... cfg.field`) need the guard to live long enough — this is automatic with `RwLockReadGuard`.

Call sites that do `ctx.config_mut().field = value` need to change to:
```rust
let mut cfg = ctx.config_write();  // or whatever upstream names it
let cfg = Arc::make_mut(&mut *cfg);  // get mutable Config from Arc
cfg.field = value;
```

---

## 5. Implementation Steps (ordered)

1. **Take upstream's `App` struct verbatim** (line 1024–1035). No fork additions needed here.

2. **Take upstream's `AppContext` struct as the base** (line 1357–1481). This gives us all ~50 upstream fields with correct `parking_lot`/`RwLock`/`Arc`/`Atomic` types.

3. **Add the 3 fork-only fields** to `AppContext` (§3.3):
   ```rust
   semantic_search_metrics: parking_lot::Mutex<SearchMetricsCollector>,
   semantic_diagnostics_logger: parking_lot::Mutex<Option<SemanticDiagnosticsLogger>>,
   semantic_warning_dedup: parking_lot::Mutex<WarningDedup>,
   ```

4. **Update the constructor** (`with_app_and_provider` / `AppContext::new`):
   - Use upstream's constructor body as the base.
   - Add initialization for the 3 fork-only fields (§3.3 constructor snippet).
   - Ensure `metrics_window_size` is read from config before the struct is built (fork reads it from `config.semantic.metrics_window_size`).

5. **Add the 3 fork-only accessor methods** (§3.3 accessor table):
   ```rust
   pub fn semantic_search_metrics(&self) -> &parking_lot::Mutex<SearchMetricsCollector> { &self.semantic_search_metrics }
   pub fn semantic_diagnostics_logger(&self) -> &parking_lot::Mutex<Option<SemanticDiagnosticsLogger>> { &self.semantic_diagnostics_logger }
   pub fn semantic_warning_dedup(&self) -> &parking_lot::Mutex<WarningDedup> { &self.semantic_warning_dedup }
   ```

6. **Migrate all call sites** (§4) — this is the bulk of the work:
   - `.borrow()` → `.read()` or `.lock()`
   - `.borrow_mut()` → `.write()` or `.lock()`
   - Remove all `ctx.callgraph().borrow_mut()` usages → `ctx.callgraph_store_for_ops()`
   - Update `config_mut()` call sites to upstream's `config_write()` pattern

7. **Compile-gate:** `cargo check -p aft` must pass before proceeding to other Tier 1 conflicts.

8. **Run tests:** `cargo test -p aft --features semantic-model2vec,semantic-fts5` — the integration tests in `aft_search_contract_test.rs` have ~25 `semantic_index_status().borrow_mut()` call sites that must all be migrated to `.write()`.

---

## 6. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- |
| `config()` return type change breaks ~50 call sites | High | Medium | Deref coercion handles most; grep for `config_mut()` and `let cfg = ctx.config()` patterns |
| In-memory `callgraph` removal breaks `move_symbol.rs` / `configure.rs` | Medium | High | Check if upstream rewrote these commands; if not, adapt to `callgraph_store_for_ops()` |
| `callgraph_store` type change (`CallGraphStore` → `Arc<ReadonlyCallGraphStore>`) breaks store ops | High | High | The fork's `CallGraphStore` mutation methods may not exist on `ReadonlyCallGraphStore`. Verify whether upstream's store supports the fork's write patterns or if writes go through a different path. |
| Fork's diagnostics accessors (`.lock()`) deadlock if held across another `.lock()` | Low | Medium | `parking_lot::Mutex` is not reentrant — audit call sites that chain `semantic_search_metrics().lock()` with `semantic_diagnostics_logger().lock()` |
| `pending_semantic_index_paths` now `Arc<Mutex>` — fork's direct `.borrow_mut()` misses the Arc | Medium | Low | Mechanical: `.borrow_mut()` → `.lock()` on the Arc-deref'd Mutex |
| Test call sites (~35) not migrated → compile errors in test build | High | Low | Mechanical migration; tests are the last step before compile-gate |

---

## 7. Files Requiring Call-Site Updates

Based on the code search (173 matches), these files need `.borrow()` → `.read()/.lock()` migration:

| File | Approx. call sites | Priority |
| :--- | :--- | :--- |
| `crates/aft/src/main.rs` | 40 | 🔴 Critical — dispatch loop |
| `crates/aft/src/commands/semantic_search.rs` | 15 | 🔴 Critical — conflicts with Tier 1 §3.2 |
| `crates/aft/src/commands/configure.rs` | 10 | 🟡 |
| `crates/aft/src/cli/warmup.rs` | 12 | 🟡 |
| `crates/aft/src/commands/semantic_doctor.rs` | 2 | 🟢 |
| `crates/aft/src/commands/inspect.rs` | 2 | 🟢 (no change — Arc clone) |
| `crates/aft/src/commands/move_symbol.rs` | 1 | 🟡 (callgraph removal) |
| `crates/aft/src/commands/callers.rs` | 1 | 🟢 (callgraph_store_for_ops — no change) |
| `crates/aft/src/commands/call_tree.rs` | 1 | 🟢 |
| `crates/aft/src/commands/impact.rs` | 1 | 🟢 |
| `crates/aft/src/commands/aft_orient.rs` | 1 | 🟢 |
| `crates/aft/src/commands/aft_impact_delta.rs` | 1 | 🟢 |
| `crates/aft/tests/integration/aft_search_contract_test.rs` | 25 | 🟡 |
| `crates/aft/tests/integration/inspect_*_test.rs` | 10 | 🟡 |
| `crates/aft/tests/integration/callgraph_store_*_test.rs` | 5 | 🟡 |
| `crates/aft/tests/callgraph_store_test.rs` | 3 | 🟢 |

---

## 8. Summary

- **1 field identical** (`inspect_manager`) — no action
- **~35 fields change concurrency primitive** — take upstream's versions
- **3 fields are fork-only** (`semantic_search_metrics`, `semantic_diagnostics_logger`, `semantic_warning_dedup`) — re-add with `parking_lot::Mutex`
- **4 fields moved to `App`** (`db`, `lsp_child_registry`, `stdout_writer`, `progress_sender`) — take upstream's `App` struct
- **1 field removed** (`callgraph`) — migrate call sites to `callgraph_store_for_ops()`
- **~50 fields are upstream-only** — accept verbatim
- **~173 call sites** need `.borrow()` → `.read()/.lock()` migration across ~16 files

The merge is mechanical but **volume-heavy**. The recommended approach is to resolve `context.rs` first among the Tier 1 conflicts, compile-gate it with `cargo check -p aft`, then proceed to `semantic_index.rs` and `semantic_search.rs` (which depend on the resolved `AppContext` shape).

---

*This is a design document. No source code was modified. The proposed Rust snippets in §3.3 and §5 are ready for implementation during the merge execution phase.*
