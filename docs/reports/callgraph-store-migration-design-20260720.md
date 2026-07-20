# `callgraph_store` Type Migration Design — Writer/Reader Split + Background Refresh Worker

**Date:** 2026-07-20
**Files:** `crates/aft/src/callgraph_store/mod.rs`, `crates/aft/src/context.rs`, `crates/aft/src/main.rs`, `crates/aft/src/commands/configure.rs`, `crates/aft/src/commands/move_symbol.rs`
**Cross-ref:** [context-refactor-field-mapping-20260720.md](./context-refactor-field-mapping-20260720.md) §3.5

---

## 1. The Architectural Shift

Upstream fundamentally restructured how callgraph data is accessed and mutated. The fork uses a **single mutable struct** with direct in-place operations; upstream uses a **two-struct split** with a **background refresh worker** for writes.

| Aspect | Fork (HEAD) | Upstream (cortexkit/main) |
| :--- | :--- | :--- |
| Store struct | `CallGraphStore` (single struct, mutable) | `CallGraphStore` (writer) + `ReadonlyCallGraphStore` (reader) |
| Reader trait | None — same struct for reads and writes | `pub trait CallGraphRead` with 20+ read methods |
| Context field type | `RefCell<Option<CallGraphStore>>` | `RwLock<Option<Arc<ReadonlyCallGraphStore>>>` |
| Write mechanism | Direct: `store.refresh_files()`, `store.mark_files_stale()` | Background worker: `enqueue_callgraph_store_refresh()` |
| Build event type | `CallGraphStore` sent over channel | `CallGraphStoreBuildEvent` enum (`Ready`/`Settled`) |
| In-memory `CallGraph` | `RefCell<Option<CallGraph>>` in `AppContext` | **Removed** — replaced by `callgraph_writer: AtomicBool` |
| `CallgraphStoreAccess` | `Ready(RefMut<CallGraphStore>)` | `Ready(Arc<ReadonlyCallGraphStore>)` |
| Store module size | ~1,400 lines | ~12,490 lines (massively expanded) |

---

## 2. Upstream's Architecture (the target model)

### 2.1 Two-struct split

```rust
// WRITER — owns cold_build, refresh_files, mark_files_stale
pub struct CallGraphStore {           // line 868
    project_root: PathBuf,
    project_key: String,
    sqlite_path: PathBuf,
    generation: Option<String>,
    conn: Mutex<Connection>,
}

// READER — implements CallGraphRead trait, no write methods
pub struct ReadonlyCallGraphStore {   // line 897
    // (internal fields — wraps a read-only DB connection)
}
```

### 2.2 `CallGraphRead` trait (line 901)

The reader trait defines **20 read-only methods**:
```rust
pub trait CallGraphRead {
    fn project_root(&self) -> &Path;
    fn project_key(&self) -> &str;
    fn sqlite_path(&self) -> &Path;
    fn is_current(&self) -> bool;
    fn edge_snapshot(&self) -> Result<BTreeSet<StoredEdge>>;
    fn indexed_file_count(&self) -> Result<usize>;
    fn node_for(&self, file_rel: &Path, symbol: &str) -> Result<StoreNode>;
    fn nodes_for(&self, file_rel: &Path, symbol: &str) -> Result<Vec<StoreNode>>;
    fn nodes_matching(&self, symbol: &str) -> Result<Vec<StoreNode>>;
    fn direct_callers_of(&self, file_rel: &Path, symbol: &str) -> Result<Vec<StoreCallSite>>;
    fn direct_caller_counts_of(&self, targets: &[(String, String)]) -> Result<HashMap<(String, String), usize>>;
    fn callers_of(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<StoreCallersResult>;
    fn impact_of(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<StoreImpactResult>;
    fn outgoing_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreCallSite>>;
    fn resolved_self_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreCallSite>>;
    fn unresolved_calls_of(&self, node: &StoreNode) -> Result<Vec<StoreUnresolvedCall>>;
    fn call_tree(&self, file_rel: &Path, symbol: &str, depth: usize) -> Result<CallTreeNode>;
    fn trace_to(&self, file_rel: &Path, symbol: &str, max_depth: usize) -> Result<TraceToResult>;
    fn trace_to_symbol_candidates(&self, to_symbol: &str) -> Result<Vec<TraceToSymbolCandidate>>;
    fn trace_to_symbol(&self, ...) -> Result<TraceToSymbolResult>;
}
```

Both `CallGraphStore` and `ReadonlyCallGraphStore` implement this trait (verified: `impl CallGraphRead for ReadonlyCallGraphStore` at line 3305).

### 2.3 Write routing — the background refresh worker

Writes **never** happen via direct mutation of the resident store. Instead:

1. **`enqueue_callgraph_store_refresh()`** (line 526) — the primary public entry point for routing writes to a background worker
2. **`CallGraphStoreBuildEvent`** enum — sent over a `crossbeam_channel` when a build/refresh completes:
   ```rust
   pub enum CallGraphStoreBuildEvent {
       Ready { store: CallGraphStore, fulfilled_force_token: Option<u64>, publication_epoch: ... },
       Settled,
   }
   ```
3. **`drain_callgraph_store_events()`** — the main loop calls this to install completed builds into the resident `RwLock<Option<Arc<ReadonlyCallGraphStore>>>` slot
4. **`flush_callgraph_store_refreshes_on_graceful_shutdown()`** (line 579) — drains pending refreshes on exit
5. **`flush_callgraph_store_refreshes_with_budget(budget: Duration)`** (line 584) — bounded flush from the watcher path

### 2.4 `CallgraphStoreAccess` enum (upstream)

```rust
pub enum CallgraphStoreAccess {
    Ready(Arc<ReadonlyCallGraphStore>),  // ← changed from RefMut<CallGraphStore>
    Building,
    Unavailable,
    Error(CallGraphStoreError),
}
```

The `Ready` variant now returns an `Arc<ReadonlyCallGraphStore>` — a cheaply-clonable, read-only handle. This is fundamentally different from the fork's `Ready(RefMut<CallGraphStore>)` which returns an exclusive mutable borrow.

### 2.5 Context fields (upstream)

```rust
// AppContext (upstream)
callgraph_store: RwLock<Option<Arc<ReadonlyCallGraphStore>>>,     // reader — shared, immutable
callgraph_writer: AtomicBool,                                      // true = this context may write
callgraph_store_rx: parking_lot::Mutex<Option<Receiver<CallGraphStoreBuildEvent>>>,
callgraph_store_force_requested: AtomicU64,
callgraph_store_force_fulfilled: AtomicU64,
callgraph_store_rx_generation: AtomicU64,
callgraph_store_rx_epoch: AtomicU64,
callgraph_persist_epoch: ArtifactPublishEpoch,
callgraph_legacy_migration_summary_logged: Arc<AtomicBool>,
pending_callgraph_store_paths: PendingCallGraphStorePaths,         // newtype wrapper
```

---

## 3. Fork's Mutable Operations — Complete Inventory

### 3.1 Direct `store.refresh_files()` / `store.mark_files_stale()` call sites

| File | Line | Operation | Context |
| :--- | :--- | :--- | :--- |
| `context.rs` | 1520 | `store.refresh_files(&pending)` | Post-cold-build inline refresh |
| `context.rs` | 1525 | `store.mark_files_stale(&pending)` | Fallback when refresh fails |
| `main.rs` | 1227 | `store.mark_files_stale(&current_files)` | Watcher-driven file changes |
| `main.rs` | 1321 | `store.refresh_files(&source_paths)` | Post-edit refresh |
| `main.rs` | 1323 | `store.mark_files_stale(&source_paths)` | Fallback |
| `main.rs` | 1656 | `store.refresh_files(&pending)` | Drain install path |
| `main.rs` | 1661 | `store.mark_files_stale(&pending)` | Fallback |
| `cli/warmup.rs` | 567 | `store.refresh_files(&pending)` | Warmup refresh |
| `cli/warmup.rs` | 569 | `store.mark_files_stale(&pending)` | Fallback |

### 3.2 In-memory `ctx.callgraph()` call sites (the removed field)

| File | Line | Operation | What it does |
| :--- | :--- | :--- | :--- |
| `main.rs` | 1202 | `ctx.callgraph().borrow_mut().as_mut()` | Checks if in-memory graph exists, iterates edges |
| `main.rs` | 1285-1286 | `ctx.callgraph().borrow()` / `.borrow_mut()` | Initializes in-memory `CallGraph::new(root)` |
| `main.rs` | 1482 | `ctx.callgraph().borrow_mut()` | Edits the in-memory graph during configure |
| `main.rs` | 2776 | `ctx.callgraph().borrow_mut()` | Re-initializes in-memory graph |
| `main.rs` | 2797 | `ctx.callgraph().borrow()` | Checks graph existence |
| `configure.rs` | 3055 | `ctx.callgraph().borrow_mut() = Some(graph)` | Sets graph from configure |
| `move_symbol.rs` | 153 | `ctx.callgraph().borrow_mut()` | Mutates graph for symbol moves |

> **Update (2026-07-20):** All 7 of these call sites are **resolved by upstream's rewrite** — none require manual callgraph migration. See §4.3a (move_symbol.rs), §4.3b (main.rs), and §4.3c (configure.rs) for the investigation results.

### 3.3 Store lifecycle call sites (`cold_build`, `ensure_built`, `open`)

| File | Line | Operation |
| :--- | :--- | :--- |
| `context.rs` | 1371 | `CallGraphStore::open_readonly()` |
| `context.rs` | 1375 | `CallGraphStore::cold_build_with_lease()` |
| `context.rs` | 1380 | `CallGraphStore::ensure_built_with_lease()` |
| `context.rs` | 1383 | `CallGraphStore::open()` |
| `context.rs` | 1461 | `CallGraphStore::open_readonly()` |
| `context.rs` | 1479 | `CallGraphStore::open()` |
| `context.rs` | 1569-1572 | `cold_build_with_lease` / `ensure_built_with_lease` (spawn thread) |
| `inspect/manager.rs` | 1344 | `CallGraphStore::open_ready_repairing()` |

---

## 4. Migration Mapping — Each Fork Operation → Upstream Equivalent

### 4.1 Read operations (queries) — ✅ EASY

All fork read call sites (callers, impact, call_tree, trace_to, etc.) go through `ctx.callgraph_store_for_ops()` which returns `CallgraphStoreAccess`. The upstream version returns `Ready(Arc<ReadonlyCallGraphStore>)` instead of `Ready(RefMut<CallGraphStore>)`.

| Fork pattern | Upstream pattern | Change needed |
| :--- | :--- | :--- |
| `match ctx.callgraph_store_for_ops() { CallgraphStoreAccess::Ready(store) => { store.callers_of(...)? } }` | Same match, but `store` is `Arc<ReadonlyCallGraphStore>` | **None** — `CallGraphRead` trait methods are identical signatures. The fork's `CallGraphStore` already has these methods; upstream's `ReadonlyCallGraphStore` implements the same trait. Deref works. |

**Files with read-only store access (no change needed beyond the type):**
- `commands/callers.rs:70` — `ctx.callgraph_store_for_ops()`
- `commands/call_tree.rs:68` — same
- `commands/impact.rs:68` — same
- `commands/trace_to.rs:68` — same
- `commands/trace_to_symbol.rs:79` — same
- `commands/trace_data.rs:98` — same
- `commands/aft_orient.rs:194` — same
- `commands/aft_impact_delta.rs:49` — same

### 4.2 Direct `refresh_files()` / `mark_files_stale()` — ✅ MOSTLY AUTO-RESOLVED (7 of 9 call sites)

The fork calls these directly on a mutable `CallGraphStore` reference. Upstream routes them through the background worker. Investigation (2026-07-20) confirmed that **7 of 9 call sites are auto-resolved** by upstream's rewrite via clean 3-way merge — only the 2 `context.rs` call sites require manual attention (context.rs is a known architectural conflict).

| Fork call site | Upstream equivalent | Migration action |
| :--- | :--- | :--- |
| `context.rs:1520` `store.refresh_files(&pending)` | **Remove** — the inline post-cold-build refresh is handled by the build event install path in upstream | 🔴 Manual — context.rs is an architectural conflict |
| `context.rs:1525` `store.mark_files_stale(&pending)` | **Remove** — same as above | 🔴 Manual — same |
| `main.rs:1227` `store.mark_files_stale(&current_files)` | `enqueue_callgraph_store_refresh(paths)` | ✅ **Auto-resolved** — 0 in merged tree (§4.3b) |
| `main.rs:1321` `store.refresh_files(&source_paths)` | `enqueue_callgraph_store_refresh(paths)` | ✅ **Auto-resolved** — 0 in merged tree (§4.3b) |
| `main.rs:1323` `store.mark_files_stale(&source_paths)` | `enqueue_callgraph_store_refresh(paths)` | ✅ **Auto-resolved** — 0 in merged tree (§4.3b) |
| `main.rs:1656` `store.refresh_files(&pending)` | **Remove** — handled by `drain_callgraph_store_events` | ✅ **Auto-resolved** — 0 in merged tree (§4.3b) |
| `main.rs:1661` `store.mark_files_stale(&pending)` | **Remove** — same | ✅ **Auto-resolved** — 0 in merged tree (§4.3b) |
| `cli/warmup.rs:567` `store.refresh_files(&pending)` | `enqueue_callgraph_store_refresh(paths)` | ✅ **Auto-resolved** — auto-merged cleanly; merged tree has 0 direct calls, 1 `enqueue_callgraph_store_refresh` |
| `cli/warmup.rs:569` `store.mark_files_stale(&pending)` | `enqueue_callgraph_store_refresh(paths)` | ✅ **Auto-resolved** — same |

**Verification (merged tree `a1a788be`):**
| File | Fork HEAD `refresh_files` | Merged `refresh_files` | Fork HEAD `mark_files_stale` | Merged `mark_files_stale` |
| :--- | :---: | :---: | :---: | :---: |
| `main.rs` | 2 | **0** | 3 | **0** |
| `cli/warmup.rs` | 1 | **0** | 1 | **0** |

**Key insight:** The fork's `refresh_files` / `mark_files_stale` pattern is:

**Key insight:** The fork's `refresh_files` / `mark_files_stale` pattern (now only relevant for the 2 `context.rs` call sites — the other 7 are auto-resolved):
```rust
// FORK PATTERN (to be removed in context.rs only)
if let Err(error) = store.refresh_files(&pending) {
    let _ = store.mark_files_stale(&pending);
}
```
Upstream replaces this with:
```rust
// UPSTREAM PATTERN
enqueue_callgraph_store_refresh(pending);  // routes to background worker
// The worker does refresh_files internally and sends CallGraphStoreBuildEvent::Ready when done
// drain_callgraph_store_events() in the main loop installs the result
```

### 4.3 In-memory `ctx.callgraph()` — ✅ RESOLVED BY UPSTREAM'S REWRITE (all 7 call sites)

The in-memory `CallGraph` field is **gone** in upstream. However, investigation (2026-07-20) revealed that **all 7 fork call sites are resolved by upstream's rewrite** — none require manual callgraph migration. The fork never touched callgraph logic in these files; the conflicts (where they exist) are purely text-level overlaps with the fork's semantic-search additions.

| Fork call site | What it does | Upstream equivalent | Migration action |
| :--- | :--- | :--- | :--- |
| `main.rs:1202` | Iterates in-memory graph edges | `callgraph_store_for_ops()` → `store.edge_snapshot()` | ✅ **Auto-resolved** — see §4.3b |
| `main.rs:1285-1286` | Initializes `CallGraph::new(root)` | Store cold-build path | ✅ **Auto-resolved** — see §4.3b |
| `main.rs:1482` | Edits in-memory graph during configure | `enqueue_callgraph_store_refresh()` | ✅ **Auto-resolved** — see §4.3b |
| `main.rs:2776` | Re-initializes graph | `mark_callgraph_store_force_rebuild()` | ✅ **Auto-resolved** — see §4.3b |
| `main.rs:2797` | Checks graph existence | `callgraph_store_for_ops()` != `Unavailable` | ✅ **Auto-resolved** — see §4.3b |
| `configure.rs:3055` | `ctx.callgraph().borrow_mut() = Some(graph)` | `mark_callgraph_store_force_rebuild()` / store cold-build | ✅ **Auto-resolved** — see §4.3c |
| `move_symbol.rs:153` | Mutates graph for symbol moves | `callgraph_store_for_ops()` → best-effort store query | ✅ **Auto-resolved** — see §4.3a |

### 4.3a `move_symbol.rs` — ✅ NOT A CONFLICT (investigation 2026-07-20)

**Investigation result:** `move_symbol.rs` was changed **only on the upstream side** since the merge base (`488af7a7`). The fork's version is identical to the merge base. Therefore:
- It does **not** appear in the `git merge-tree` conflict list
- Upstream's version will be taken **automatically** by the merge with zero manual resolution

**Upstream's rewrite:** Upstream replaced the fork's in-memory `ctx.callgraph().borrow_mut()` pattern with a store-based approach:
```rust
// UPSTREAM (line 345) — store-based, best-effort
let (project_root, consumers) = match ctx.callgraph_store_for_ops() {
    CallgraphStoreAccess::Ready(store) => {
        let sites = store
            .node_for(rel_source, symbol_name)
            .and_then(|node| store.direct_callers_of(Path::new(&node.file), &node.symbol))
            .unwrap_or_default();
        // ... map sites to consumer files
    }
    _ => {
        // Graceful degradation — Building/Unavailable/Error are NOT fatal
        // Falls back to TS/JS brute-walk for consumer discovery
        Vec::new()
    }
};
```

**Fork's original approach (at merge base, unchanged by fork):**
```rust
// FORK (line 153) — in-memory graph, synchronous
let mut cg_ref = ctx.callgraph().borrow_mut();
let graph = cg_ref.as_mut().ok_or(/* not_configured */)?;
// ...
graph.build_file(source_path);
let consumers = graph.callers_of(source_path, symbol_name, 1, max_files)?;
```

**Key design difference:** Upstream treats the callgraph store as **best-effort enrichment** — if the store is Building/Unavailable/Error, it falls back to a TS/JS file brute-walk and still completes the move. The fork's version treated the in-memory graph as **required** — if it wasn't configured, the move failed with `not_configured`. Upstream's approach is strictly more robust.

**Migration action:** **None required.** Take upstream's version automatically. The merge resolves this file without conflict.

### 4.3b `main.rs` — ✅ CLEAN AUTO-MERGE (investigation 2026-07-20)

**Investigation result:** `main.rs` was changed on **both** sides since the merge base, but the fork's changes are entirely **semantic-search command registration** — the fork did **not** touch any callgraph logic. Git's 3-way merge cleanly auto-merges the fork's semantic additions with upstream's callgraph rewrite.

**Evidence:**
- `git merge-tree --write-tree HEAD cortexkit/main` → `main.rs` has **0 conflict markers** in the merged tree (`a1a788be`)
- Fork's diff (`git diff $MB..HEAD -- main.rs`) contains **zero** callgraph-related lines — all 30 insertions are semantic/FTS5 command routing:
  - `explain_search`, `semantic_doctor`, `semantic_eval` command handlers
  - `fts5_index`, `fts5_search`, `fts5_find_symbol`, `fts5_read_symbol`, `fts5_doctor` (feature-gated)
  - `aft_orient`, `aft_impact_delta`, `aft_context_pack` command handlers
  - `why_missed`, `verify` command handlers
  - Telemetry CLI entry point
- None of the fork's 6 `ctx.callgraph()` call sites (lines 1202, 1285-1286, 1482, 2776, 2797) appear in the fork's diff — they are pre-existing, unchanged from the merge base

**Merged result (verified via `git show $TREE:crates/aft/src/main.rs`):**
| Metric | Fork HEAD | Merged tree |
| :--- | :---: | :---: |
| `ctx.callgraph()` | 6 | **0** |
| `ctx.callgraph_store()` | 0 | 2 |
| `callgraph_store_for_ops` | 0 | 1 |
| `drain_callgraph` | 0 | 1 |
| Conflict markers | N/A | **0** |

**Conclusion:** Upstream's store-based infrastructure (`drain_callgraph_store_events`, `callgraph_refresh_worker`, `flush_callgraph_store_refreshes`, `ensure_callgraph_store`) fully replaces all 6 of the fork's in-memory `ctx.callgraph()` calls via clean 3-way auto-merge. **No manual callgraph migration needed in `main.rs`.**

### 4.3c `configure.rs` — 🟡 TEXT-LEVEL CONFLICTS, NOT ARCHITECTURAL (investigation 2026-07-20)

**Investigation result:** `configure.rs` was changed on **both** sides and produces **8 conflict markers** in the merged tree. However, the fork's changes are entirely **semantic config parsing** — the fork did **not** touch any callgraph logic. The conflicts are text-level overlaps between the fork's `SemanticFilePolicy` / `parse_fts5_config` additions and upstream's callgraph rewrite, not architectural disagreements.

**Evidence:**
- Fork's callgraph-related diff (`git diff $MB..HEAD -- configure.rs | grep callgraph`) → **empty** — zero callgraph lines changed by the fork
- Fork's 580 insertions are all semantic/FTS5 config: `SemanticFilePolicy`, `parse_fts5_config()`, `SemanticBackend`, dimensions, output encoding, input mode
- The fork's single `ctx.callgraph()` call site (line 3055) is **pre-existing**, unchanged from the merge base

**Merged result (verified via `git show $TREE:crates/aft/src/commands/configure.rs`):**
| Metric | Fork HEAD | Merged tree (with conflict markers) |
| :--- | :---: | :---: |
| `ctx.callgraph()` | 1 | **0** (upstream's side wins) |
| `ctx.callgraph_store()` | 1 | 2 |
| `ctx.callgraph_store_rx` | 0 | 7 |
| `mark_callgraph_store_force_rebuild` | 0 | 2 |
| Conflict markers | N/A | **8** |

**Categorization of the 8 conflict hunks** (by first content line):
1. `use crate::callgraph::CallGraph;` — import overlap (fork added semantic imports near callgraph import)
2. `fn parse_fts5_config(` — fork's new function overlaps upstream's region
3. `url_fetch_allow_private` — config parsing overlap
4. `home_match` — home directory detection overlap
5. `build_once` — build strategy overlap
6. `refresh_stale_files` — refresh logic overlap
7. `}` / `event` — build event handling overlap
8. (build_result match) — result handling overlap

**None of the 8 hunks are callgraph-architectural conflicts.** The resolution strategy is mechanical:
- **Take upstream's callgraph code verbatim** (imports, store access, force-rebuild, build event handling)
- **Merge the fork's semantic config additions around it** (`SemanticFilePolicy`, `parse_fts5_config`, FTS5 config fields)
- The fork's pre-existing `ctx.callgraph().borrow_mut() = Some(graph)` at line 3055 is replaced by upstream's `mark_callgraph_store_force_rebuild()` automatically

**Migration action:** Resolve the 8 text-level conflicts by taking upstream's callgraph code + preserving the fork's semantic config additions. This is a **mechanical merge task**, not a callgraph migration task.

### 4.4 Store lifecycle operations — ✅ MOSTLY COMPATIBLE

| Fork call | Upstream status | Migration |
| :--- | :--- | :--- |
| `CallGraphStore::open()` | Still exists (line ~1353) | No change |
| `CallGraphStore::open_readonly()` | Still exists | No change |
| `CallGraphStore::cold_build_with_lease()` | Still exists (line 1525) | No change — but result goes through `CallGraphStoreBuildEvent::Ready` |
| `CallGraphStore::ensure_built_with_lease()` | Still exists (line 1613) | No change |
| `CallGraphStore::needs_cold_build()` | Still exists (line 694 in fork, upstream has equivalent) | No change |
| `CallGraphStore::open_ready_repairing()` | Still exists | No change |

**Key difference:** In the fork, `cold_build_with_lease` returns `(CallGraphStore, stats)` directly. In upstream, the cold build happens in a background thread and the result arrives as `CallGraphStoreBuildEvent::Ready { store, ... }` over the channel. The `spawn_callgraph_store_cold_build` method in `context.rs` (fork, line ~1569) must be updated to send the build event instead of directly installing the store.

---

## 5. The `CallGraphStoreBuildEvent` Channel — How Writes Land

### 5.1 Fork's current flow (direct mutation)

```
Watcher sees file change
  → main.rs: mark_files_stale(&files) directly on store
  → main.rs: refresh_files(&files) directly on store
  → Store is mutated in-place under RefCell borrow
```

### 5.2 Upstream's flow (background worker)

```
Watcher sees file change
  → enqueue_callgraph_store_refresh(paths)
  → Background worker picks up the refresh
  → Worker calls CallGraphStore::refresh_files() internally (writer struct)
  → Worker sends CallGraphStoreBuildEvent::Ready { store, ... } over channel
  → Main loop: drain_callgraph_store_events() installs the new store
    into RwLock<Option<Arc<ReadonlyCallGraphStore>>>
  → Next callgraph_store_for_ops() returns the fresh Arc<ReadonlyCallGraphStore>
```

### 5.3 What this means for the merge

The fork's **synchronous** refresh pattern (call `refresh_files`, get immediate result, fallback to `mark_files_stale` on error) becomes **asynchronous** in upstream (enqueue, worker processes, result lands on next drain). This affects:

1. **Error handling:** The fork's `if let Err(error) = store.refresh_files()` pattern has no direct upstream equivalent — the worker handles errors internally and may send `Settled` (failure) or nothing.
2. **Timing tests:** Tests that rely on synchronous refresh (`AFT_CALLGRAPH_BUILD_WAIT_MS`) need to use the `flush_callgraph_store_refreshes_with_budget()` path instead.
3. **Warmup:** `cli/warmup.rs` does synchronous refresh — must switch to `flush_callgraph_store_refreshes_with_budget()` for blocking behavior or accept async.

---

## 6. Implementation Plan (ordered)

### Step 1: Accept upstream's `callgraph_store/mod.rs` as the base
- Take upstream's 12,490-line module verbatim (it's a superset of the fork's ~1,400 lines).
- Verify the fork's `dead_code_projection.rs` changes are compatible (both sides have this file — check for conflicts).

### Step 2: Accept upstream's `AppContext` callgraph fields
```rust
callgraph_store: RwLock<Option<Arc<ReadonlyCallGraphStore>>>,
callgraph_writer: AtomicBool,
callgraph_store_rx: parking_lot::Mutex<Option<Receiver<CallGraphStoreBuildEvent>>>,
// ... (all upstream-only epoch/generation fields from §2.5)
```

### Step 3: Remove the fork's in-memory `callgraph` field
- Delete `callgraph: RefCell<Option<CallGraph>>` from `AppContext`.
- Delete the `pub fn callgraph(&self) -> &RefCell<Option<CallGraph>>` accessor.

### Step 4: Update `callgraph_store_for_ops()` return type
- Change `CallgraphStoreAccess::Ready(RefMut<CallGraphStore>)` → `Ready(Arc<ReadonlyCallGraphStore>)`.
- Update the method body to use `self.callgraph_store.read()` and return `Arc::clone(&store)`.

### Step 5: Migrate the 2 remaining direct `refresh_files`/`mark_files_stale` call sites in `context.rs` (§4.2)
- ~~Migrate the 9 direct call sites~~ → **7 of 9 are auto-resolved** by upstream's rewrite (main.rs and cli/warmup.rs auto-merge cleanly with 0 direct calls in the merged tree).
- Only the 2 `context.rs` call sites (lines 1520, 1525) require manual migration — context.rs is an architectural conflict.
- Remove the `if let Err(error) = store.refresh_files() { store.mark_files_stale() }` fallback pattern in context.rs.

### Step 6: ~~Migrate the 7 in-memory `ctx.callgraph()` call sites~~ → ✅ ALL AUTO-RESOLVED (§4.3)
- ~~Replace graph existence checks with `callgraph_store_for_ops()` != `Unavailable`.~~
- ~~Replace graph initialization with store cold-build trigger.~~
- **All 7 call sites are auto-resolved by upstream's rewrite:**
  - `move_symbol.rs` (1): not a conflict — only upstream changed it (§4.3a)
  - `main.rs` (5): clean auto-merge — fork's changes were semantic-search commands, not callgraph (§4.3b)
  - `configure.rs` (1): upstream's callgraph code wins in 8 text-level conflicts (§4.3c)
- **No manual `ctx.callgraph()` migration needed.**

### Step 7: Update `spawn_callgraph_store_cold_build()` in `context.rs`
- Change from sending `CallGraphStore` directly to sending `CallGraphStoreBuildEvent::Ready { store, ... }`.
- Update the drain handler to install `Arc<ReadonlyCallGraphStore>` from the event.

### Step 8: Update test call sites
- Tests that do `*ctx.callgraph_store().borrow_mut() = Some(store)` → `*ctx.callgraph_store().write() = Some(Arc::new(store.into_readonly()))`.
- Tests that call `store.refresh_files()` directly → use `enqueue_callgraph_store_refresh()` + `flush_callgraph_store_refreshes_with_budget()`.

### Step 9: Compile-gate
```bash
cargo check -p aft --features semantic-model2vec,semantic-fts5
```

### Step 10: Test-gate
```bash
cargo test -p aft --features semantic-model2vec,semantic-fts5 -- callgraph
```

---

## 7. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- |
| ~~`move_symbol.rs` has no upstream equivalent~~ **RESOLVED**: upstream rewrote it to use the store; file is not a conflict | ~~Medium~~ → N/A | ~~High~~ → N/A | No action needed — upstream's version taken automatically (see §4.3a) |
| ~~`main.rs` in-memory callgraph calls need manual migration~~ **RESOLVED**: upstream's store-based rewrite auto-merges cleanly; fork's changes were semantic-search commands only (§4.3b) | ~~High~~ → N/A | ~~High~~ → N/A | No action needed — 0 conflict markers in merged tree |
| ~~`configure.rs` callgraph migration is architectural~~ **RESOLVED**: 8 conflict hunks are text-level overlaps from semantic config additions; fork did not touch callgraph logic (§4.3c) | ~~Medium~~ → Low | ~~Medium~~ → Low | Mechanical merge: take upstream's callgraph code + preserve fork's `SemanticFilePolicy`/`parse_fts5_config` |
| Async refresh breaks tests that expect synchronous results | High | Medium | Use `flush_callgraph_store_refreshes_with_budget()` in tests; set `AFT_CALLGRAPH_BUILD_WAIT_MS` for fixture-scale builds |
| Fork's `CallGraphStore` methods have signatures that differ from upstream's | Medium | Medium | The `CallGraphRead` trait abstracts this — verify trait method signatures match. The writer methods (`refresh_files`, `cold_build`) may have different parameter types (e.g., `RefreshFilesProfile` in upstream) |
| `enqueue_callgraph_store_refresh` API doesn't exist on the fork's callgraph_store module | Certain | Low | Take upstream's module verbatim (Step 1) — the function comes with it |
| Fork's `dead_code_projection.rs` diverges from upstream's | Medium | Medium | Check for conflicts in this file during the merge; both sides have it |
| `CallGraphStore` → `ReadonlyCallGraphStore` conversion not straightforward | Low | Medium | Upstream likely has `CallGraphStore::into_readonly()` or similar — verify in the 12,490-line module |
| ~~Removing in-memory `CallGraph` breaks edge iteration in `main.rs:1202`~~ **RESOLVED**: main.rs auto-merges cleanly; upstream's `store.edge_snapshot()` replaces it automatically (§4.3b) | ~~Medium~~ → N/A | ~~Medium~~ → N/A | No action needed — 0 conflict markers in merged tree |

---

## 8. Files Affected

| File | Changes needed | Risk |
| :--- | :--- | :--- |
| `crates/aft/src/callgraph_store/mod.rs` | Take upstream verbatim (12,490 lines) | 🟢 |
| `crates/aft/src/context.rs` | Remove `callgraph` field, change `callgraph_store` type, update `callgraph_store_for_ops()`, update `spawn_callgraph_store_cold_build()` | 🔴 |
| `crates/aft/src/main.rs` | ✅ **CLEAN AUTO-MERGE** — fork's changes are semantic-search command registration; upstream's store-based rewrite replaces all 6 `ctx.callgraph()` calls automatically (§4.3b). 0 conflict markers in merged tree. | 🟢 |
| `crates/aft/src/commands/configure.rs` | 🟡 8 text-level conflict hunks (fork's semantic config additions overlap upstream's callgraph rewrite). Fork did NOT touch callgraph logic. Resolution: take upstream's callgraph code + merge fork's `SemanticFilePolicy`/`parse_fts5_config` additions (§4.3c). | 🟡 |
| `crates/aft/src/commands/move_symbol.rs` | **NOT A CONFLICT** — only upstream changed this file; store-based rewrite taken automatically (§4.3a) | 🟢 |
| `crates/aft/src/cli/warmup.rs` | ✅ **AUTO-MERGED** — 2 direct `refresh_files`/`mark_files_stale` calls replaced by `enqueue_callgraph_store_refresh` via clean 3-way merge (§4.2). 0 conflict markers. | 🟢 |
| `crates/aft/src/commands/callers.rs` | No change (uses `callgraph_store_for_ops()` which still works) | 🟢 |
| `crates/aft/src/commands/call_tree.rs` | No change | 🟢 |
| `crates/aft/src/commands/impact.rs` | No change | 🟢 |
| `crates/aft/src/commands/trace_to.rs` | No change | 🟢 |
| `crates/aft/src/commands/trace_to_symbol.rs` | No change | 🟢 |
| `crates/aft/src/commands/trace_data.rs` | No change | 🟢 |
| `crates/aft/src/commands/aft_orient.rs` | No change | 🟢 |
| `crates/aft/src/commands/aft_impact_delta.rs` | No change | 🟢 |
| `crates/aft/src/inspect/manager.rs` | Verify `open_ready_repairing()` still exists | 🟢 |
| `crates/aft/tests/callgraph_store_test.rs` | Update test patterns (synchronous → flush) | 🟡 |
| `crates/aft/tests/integration/callgraph_store_*_test.rs` | Same | 🟡 |
| `crates/aft/tests/integration/inspect_*_test.rs` | Update `borrow_mut()` → `write()` patterns | 🟡 |

---

## 9. Summary

- **Read operations** (8 command files): ✅ No change — `CallGraphRead` trait method signatures are identical
- **Direct `refresh_files`/`mark_files_stale`** (9 call sites): ✅ **7 of 9 auto-resolved** by upstream's rewrite (main.rs: 5, cli/warmup.rs: 2 — all 0 in merged tree). Only 2 `context.rs` call sites require manual migration.
- **In-memory `ctx.callgraph()`** (7 call sites): ✅ **ALL 7 AUTO-RESOLVED BY UPSTREAM'S REWRITE**
  - `move_symbol.rs` (1 call site): ✅ Not a conflict — only upstream changed it (§4.3a)
  - `main.rs` (5 call sites): ✅ Clean auto-merge — fork's changes are semantic-search commands, not callgraph (§4.3b). 0 conflict markers.
  - `configure.rs` (1 call site): ✅ Upstream's callgraph code wins; 8 text-level conflicts are semantic-config overlaps, not architectural (§4.3c)
- **Store lifecycle** (`open`, `cold_build`, `ensure_built`): ✅ Mostly compatible — API exists on both sides
- **`move_symbol.rs`**: ✅ **RESOLVED** — not a conflict; upstream's store-based rewrite taken automatically (§4.3a)
- **Tests**: 🟡 Update from synchronous direct mutation to flush/enqueue patterns

The core principle: **writes go through `enqueue_callgraph_store_refresh()`, reads go through `callgraph_store_for_ops()` → `Arc<ReadonlyCallGraphStore>`**. The fork's direct mutation pattern is replaced by an asynchronous build-event channel.

---

*This is a design document. No source code was modified. The migration steps in §6 are ready for implementation during the merge execution phase.*
