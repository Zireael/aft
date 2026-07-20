# Merge Conflict Analysis & Resolution Plan

**Date:** 2026-07-20
**Fork branch:** `semantic-search-enhancement` (remote: `zireael/semantic-search-enhancement`)
**Upstream target:** `cortexkit/main`
**PR:** https://github.com/cortexkit/aft/pull/87 (open, "merge candidate build")
**Method:** Non-destructive virtual merge via `git merge-tree --write-tree` + real merge in isolated detached worktree at `D:/tmp/aft-merge-conflict-probe`. **No changes were made to the working tree or uncommitted edits.**

---

## 1. Divergence Summary

| Metric | Value |
| :--- | :--- |
| Merge base | `488af7a7` ("release-prep: v0.39.2 notes") |
| Fork commits ahead | **239** |
| Upstream commits ahead | **718** |
| Files changed on fork side | 217 |
| Files changed on upstream side | 1101 |
| Files changed on **both** sides (overlap) | 43 |
| **Conflicting files** | **25** |
| Auto-merged cleanly (in overlap) | 18 |

This is a **large, long-lived divergence**. Upstream has moved ~8 minor releases forward (v0.39.2 → v0.47.2) with substantial architecture changes, while the fork has built an entire semantic retrieval layer (PR #87) plus Retrieval Intelligence v3.1 work tracked under the `aft-ri-v31` and related beads epics.

**Active beads epics on the fork** (features that touch conflicting files):
- `aft-fts5e2e` — opt-in FTS5 end-to-side features (14 tasks)
- `aft-t6p` — semantic search upgrade + `aft-t6p.snippet` enrichment (in progress: `aft-fts5e2e.1`)
- `bd-aft-db` — persistent repository intelligence DB (schema/migrations/graph storage)
- `bd-aft-ri` — Qartez-style intelligence ported into AFT
- `aft-ri-v31` — Retrieval Intelligence v1

---

## 2. Conflict Severity Ranking

Files ranked by conflict hunk count (`<<<<<<<` markers). Higher = harder to resolve.

| Hunks | File | Category |
| ----: | :--- | :--- |
| 47 | `crates/aft/src/semantic_index.rs` | 🔴 Core semantic — hard |
| 27 | `crates/aft/src/commands/semantic_search.rs` | 🔴 Core semantic — hard |
| 9 | `ARCHITECTURE.md` | 🟢 Docs — mechanical |
| 8 | `crates/aft/src/commands/configure.rs` | 🟡 Module wiring |
| 8 | `STRUCTURE.md` | 🟢 Docs — mechanical |
| 4 | `crates/aft/src/lib.rs` | 🟡 Module wiring |
| 4 | `crates/aft/src/config.rs` | 🟡 Config schema |
| 3 | `crates/aft/src/db/mod.rs` | 🔴 Schema version — needs care |
| 3 | `crates/aft/src/context.rs` | 🔴 AppContext refactor — hard |
| 2 | `crates/aft/src/commands/grep.rs` | 🟡 Response shape |
| 1 × 12 | (12 files, 1 hunk each) | 🟢/🟡 — see §3 |

---

## 3. Per-Conflict Resolution Plan

Legend: **HEAD** = fork (`semantic-search-enhancement`); **UP** = upstream (`cortexkit/main`).

### Tier 1 — 🔴 Hard conflicts (require careful manual resolution + compile check)

#### 3.1 `crates/aft/src/semantic_index.rs` (47 hunks) — CRITICAL
**Nature:** Structural divergence. UP modernized concurrency (introduces `AtomicUsize`, `OnceLock`, `Weak`, `Instant`), switched parser entry point to `parse_source_with_cached_parser`, bumped `SEMANTIC_INDEX_VERSION` (HEAD: `V7`, UP: `V8`), and added default timeout constants. HEAD carries the fork's provider-aware vector storage, fingerprint invalidation, and model2vec plumbing from PR #87.
**Risk:** High. Both sides rewrote overlapping regions. 47 hunks means near-total file overlap.
**Beads link:** `aft-fts5e2e`, `aft-t6p`, `bd-aft-db` all depend on this file.
**Resolution strategy:**
1. **Prefer UP's concurrency/parser/skeleton** as the base (it's the newer architecture the fork must adopt).
2. **Re-apply fork's PR #87 additions on top**: provider-aware vector kinds (`VectorKind`), fingerprint-based invalidation, `model2vec` backend hooks, FTS5 adapter integration.
3. Bump to `SEMANTIC_INDEX_VERSION_V8` (UP) and extend the fork's fingerprint logic to account for the new version.
4. After manual merge, run `cargo check -p aft --features semantic-model2vec,semantic-fts5` to validate.
5. **Validate** the index-invalidation path with `aft-fts5e2e.1` acceptance criteria (feature gating + runtime config).

#### 3.2 `crates/aft/src/commands/semantic_search.rs` (27 hunks) — CRITICAL
**Nature:** Import-level divergence. HEAD pulls in `CandidateProvenance`, `CandidateSet`, `ContextBudget`, `Fts5Adapter`, `RRFFusionEngine`, `GraphExpansionAdapter` (the RI v3.1 retrieval stack). UP replaced the dispatch surface with `readonly_artifacts` (`GitRootResolutionError`, `ReadOnlyArtifact`), `symbol_render`, and `tool_call` routing.
**Risk:** High. The fork's entire retrieval-intelligence command layer must be reconciled with UP's new `tool_call` dispatch.
**Beads link:** `bd-aft-ri`, `aft-ri-v31` (the `aft_orient`, `aft_impact_delta`, `aft_context_pack` commands live here), `aft-t6p.snippet`.
**Resolution strategy:**
1. Adopt UP's `tool_call` routing and `readonly_artifacts` resolution as the new entry path.
2. Re-wire the fork's retrieval pipeline (`CandidateSet` → fusion → rerank) as the body behind the new dispatch.
3. Keep the fork's `aft_orient` / `aft_impact_delta` / `aft_context_pack` NDJSON commands (from `aft-ri-v31-t6c`); register them through UP's command registration mechanism.
4. Re-integrate the snippet-enrichment config from `aft-t6p.snippet` once that bead sequence lands.
5. **Note:** HEAD has uncommitted edits to this file (the `.aft`/`.bench-cache`/`.beads`/`.pi` grep-exclusion filter). Stash/commit those before merging so they aren't lost.

#### 3.3 `crates/aft/src/context.rs` (3 hunks) — HARD
**Nature:** UP performed a major refactor: `AppContext` moved from `RefCell`-dominated single-threaded state to `parking_lot`/`RwLock` concurrency, and extracted a new `App` struct for process-wide services (DB handle, atomic counters). HEAD added `semantic_*` fields and `inspect_manager` to the old `RefCell` shape.
**Risk:** High. This is a foundational refactor — every subsystem that reads `AppContext` is affected.
**Resolution strategy:**
1. **Take UP's `App`/`AppContext` split as the base.** Do not try to preserve the `RefCell` shape.
2. Re-add the fork's semantic fields (`semantic_index`, `semantic_diagnostics`, semantic refresh epoch/circuit, `inspect_manager`) into UP's new struct shape using `parking_lot`/`Arc<Mutex>` wrappers consistent with UP's pattern.
3. Update all call sites that read these fields (use `code_searcher` to find every `ctx.semantic_*` / `ctx.inspect_manager` access).
4. Compile-gate: `cargo check -p aft` must pass before proceeding.

#### 3.4 `crates/aft/src/db/mod.rs` (3 hunks) — CARE
**Nature:** Schema version conflict. HEAD: `CURRENT_SCHEMA_VERSION = 3` with `MIGRATION_V3_RIL`. UP: bumps to `4` with `MIGRATION_V3` + `apply_migration_v4`.
**Beads link:** `bd-aft-db` (schema/migrations/graph storage).
**Resolution strategy:**
1. Take UP's version `4` as the floor.
2. Re-introduce the fork's RIL-specific migration as `MIGRATION_V4_RIL` (or whichever next number is free) so it runs *after* UP's v4.
3. Bump `CURRENT_SCHEMA_VERSION` to `5` (fork) if RIL adds tables/columns, else keep `4`.
4. **Critical:** verify migration ordering with `bd-aft-db` acceptance — existing DBs must migrate cleanly through v4 → v5.

---

### Tier 2 — 🟡 Module wiring / config / response shape (medium effort)

#### 3.5 `crates/aft/src/lib.rs` (4 hunks)
**Nature:** Module registration. Each hunk is HEAD adding a module vs UP adding a *different* module in the same spot:
- `export_detection` (H) ↔ `executor` (UP)
- `intelligence_config` (H) ↔ `jsonc` (UP)
- `refactor_plan`, `retrieval`, `ril_indexer` (H) ↔ `readonly_artifacts`, `response_finalize`, `root_cache`, `run_tool_call`, `runtime_drain`, `runtime_registry`, `sandbox_profile`, `sandbox_spawn` (UP)
- `semantic_rerank` (H) ↔ `subc`, `subc_config`, `subc_format`, `subc_translate` (UP)
**Resolution:** **Keep both.** These are additive module declarations — concatenate HEAD's and UP's `pub mod` lines, sorted alphabetically. Verify each `mod` has a corresponding file/dir.
**Beads link:** `bd-aft-ri` (`refactor_plan`, `retrieval`, `ril_indexer`), PR #87 (`semantic_rerank`).

#### 3.6 `crates/aft/src/commands/mod.rs` (1 hunk)
**Nature:** HEAD: `aft_context_pack`, `aft_impact_delta`, `aft_orient`. UP: `apply_patch`.
**Resolution:** **Keep all four.** Additive. Ensure `apply_patch.rs` exists (UP side) and the three `aft_*` files exist (fork side).
**Beads link:** `aft-ri-v31-t6c` (the three `aft_*` commands).

#### 3.7 `crates/aft/src/cli/mod.rs` (1 hunk)
**Nature:** `telemetry` (H) ↔ `sandbox_launch` (UP).
**Resolution:** **Keep both** (additive module declarations).

#### 3.8 `crates/aft/src/commands/configure.rs` (8 hunks)
**Nature:** Import divergence. HEAD imports `CallGraph` + config traits; UP imports `cache_freshness` + broader `Config`.
**Resolution:** Merge both import sets. Reconcile the `Config` struct usage — UP's `BackupConfig` (see 3.9) replaces HEAD's `Fts5Config` *position*, but **both fields belong in the final struct**.
**Beads link:** `bd-aft-ri` (`CallGraph`), `aft-fts5e2e` (`Fts5Config`).

#### 3.9 `crates/aft/src/config.rs` (4 hunks)
**Nature:** HEAD adds `fts5: Fts5Config` field + default; UP adds `backup: BackupConfig` field + default. These occupy the same struct slot.
**Resolution:** **Keep both fields.** Add `fts5: Fts5Config` *and* `backup: BackupConfig` to the config struct, with both defaults in the `Default` impl. Ensure the TypeScript schemas (3.14/3.15) match.
**Beads link:** `aft-fts5e2e` (`Fts5Config`).

#### 3.10 `crates/aft/src/compress/mod.rs` (1 hunk)
**Nature:** `failure_classifier` (H) ↔ `find` (UP) module declaration.
**Resolution:** **Keep both** (additive).

#### 3.11 `crates/aft/src/commands/grep.rs` (2 hunks)
**Nature:** Response shape. HEAD removes profile generation and adds `no_files_matched_scope`; UP adds `result.walk_truncated` check + restructures `index_status`/`truncated`.
**Resolution:** Manual merge — combine HEAD's `no_files_matched_scope` field with UP's `walk_truncated` handling in the response builder. Both are additive response fields.
**Note:** HEAD's uncommitted `.aft`/`.bench-cache` grep-exclusion filter lives in `semantic_search.rs` (the degraded-grep path), not here — but verify the grep command's exclusion list also matches.

#### 3.12 `crates/aft/src/commands/glob.rs` (1 hunk)
**Nature:** JSON response building + classification block.
**Resolution:** Take UP's response structure; re-add any fork classification additions. Low risk.

#### 3.13 `crates/aft/src/commands/status.rs` (1 hunk)
**Nature:** `SemanticIndexStatus` response shape (`Disabled`/`Building` variants).
**Resolution:** Reconcile enum variants — keep fork's richer status (it has diagnostics from PR #87) but adopt UP's variant naming if changed.

---

### Tier 2 — 🟡 TypeScript config schemas

#### 3.14 `packages/opencode-plugin/src/config.ts` (1 hunk)
#### 3.15 `packages/pi-plugin/src/config.ts` (1 hunk)
**Nature:** HEAD adds `fts5` config to `AftConfigSchema`; UP replaces that slot with `callgraph_store` + `callgraph_chunk_size`.
**Resolution:** **Keep both.** Add `fts5` (fork) *and* `callgraph_store`/`callgraph_chunk_size` (UP) to the schema. Ensure Rust ↔ TS schema parity (PR #87 review guide §1 requires this).
**Beads link:** `aft-fts5e2e`, `bd-aft-db` (`callgraph_store`).

---

### Tier 3 — 🟢 Docs / generated / mechanical (low effort)

#### 3.16 `ARCHITECTURE.md` (9 hunks)
**Nature:** UP rewrote sections for `tool_call` routing, `memory.rs`, `patch/`, new `AppContext`/`App` split. Fork's version describes the semantic/RI layer.
**Resolution:** Take UP's text as base; append fork-specific sections (semantic retrieval, RI v3.1, FTS5). Mechanical — no compilation impact.

#### 3.17 `STRUCTURE.md` (8 hunks)
**Nature:** UP expanded `inspect/` (cycles, metrics, scanners) and streamlined plugin tool adapters (removed `semantic-doctor`, `semantic-eval`, `verify`). Fork lists those.
**Resolution:** Take UP's structure list; re-add fork's `semantic-doctor`, `semantic-eval`, `verify`, `aft_context_pack`, `aft_impact_delta`, `aft_orient` entries. Mechanical.

#### 3.18 `.gitattributes` (1 hunk, add/add)
**Nature:** HEAD enforces `eol=lf` for PTY fixtures; UP adds `*.raw` binary rule.
**Resolution:** **Union both** — keep HEAD's eol rules and add UP's `*.raw` line.

#### 3.19 `.gitignore` (1 hunk)
**Nature:** UP added many entries (`/target`, `.sisyphus/`, `local-ignore/`, nested lockfiles, Docker/E2E artifacts).
**Resolution:** Take UP's version; re-add any fork-specific entries (`.aft-bench/`, `.beads/`, `.pi/` per the uncommitted semantic_search filter).

#### 3.20 `.github/workflows/release.yml` (1 hunk)
**Nature:** UP uses `cross` instead of `cargo` for `x86_64-unknown-linux-gnu` builds.
**Resolution:** Take UP's `cross` build command (it's the newer release tooling).

#### 3.21 `Cargo.lock` (1 hunk)
**Nature:** `tokenizers 0.22.2` (H) ↔ `tokenizers` + `tokio` (UP) — dependency set divergence.
**Resolution:** **Do not hand-merge.** After all source conflicts are resolved, run `cargo update -p aft && cargo build` to regenerate `Cargo.lock` from the merged `Cargo.toml`. Commit the regenerated lock.

---

### Tier 3 — 🟢 Tests (low-medium effort)

#### 3.22 `crates/aft/tests/integration/main.rs` (1 hunk)
**Nature:** `mod ri_v31_contract_baseline_test;` (H) ↔ `mod root_keyed_adversarial_test;` (UP).
**Resolution:** **Keep both** `mod` declarations. Ensure both test files exist.

#### 3.23 `crates/aft/tests/integration/hybrid_search_test.rs` (1 hunk)
#### 3.24 `crates/aft/tests/integration/semantic_test.rs` (1 hunk)
**Nature:** Large single hunks spanning the test setup (HEAD's `configure_semantic_openai` + `MockEmbeddingServer` vs UP's version). The `=======`/`>>>>>>>` markers appear far down — these are near-total file rewrites.
**Resolution:** Manual. Keep HEAD's test bodies (they cover PR #87's provider-aware + RI scenarios), but update any shared helpers/imports to match UP's test infrastructure if `main.rs`/`context.rs` changed the harness. Run `cargo test -p aft --features semantic-model2vec,semantic-fts5 --test integration` to validate.

---

### Tier 3 — modify/delete

#### 3.25 `benchmarks/compression-tokens/data/spike-output.json` (modify/delete)
**Nature:** UP deleted this file; HEAD modified it.
**Resolution:** **Accept the deletion (UP side)** — remove the file. The fork still *references* it in `benchmarks/compression-tokens/REPORT.md:5` and `spike.ts:252,31`, so **also update those references** (either regenerate the file via `spike.ts` or remove the references). Check whether `aft-t6p.snippet` or benchmark beads depend on it.

---

## 4. Recommended Merge Execution Order

Resolve in this order to minimize rework (each tier unblocks the next):

1. **Tier 3 mechanical first** (§3.16–3.21, 3.22): docs, `.gitattributes`, `.gitignore`, `release.yml`, test `mod` declarations. Fast wins, reduces noise.
2. **Tier 2 module wiring** (§3.5–3.10): `lib.rs`, `commands/mod.rs`, `cli/mod.rs`, `compress/mod.rs`. Additive — just concatenate.
3. **Tier 2 config schemas** (§3.9, 3.14, 3.15): keep both `fts5` + `backup`/`callgraph_store`. Verify Rust↔TS parity.
4. **Tier 2 command/response shapes** (§3.8, 3.11, 3.12, 3.13): `configure.rs`, `grep.rs`, `glob.rs`, `status.rs`.
5. **🔴 Tier 1 `context.rs`** (§3.3): adopt UP's `App`/`AppContext` split, re-add semantic fields. **Compile-gate: `cargo check -p aft`.**
6. **🔴 Tier 1 `db/mod.rs`** (§3.4): reconcile schema version + migrations. Validate migration chain.
7. **🔴 Tier 1 `semantic_index.rs`** (§3.1): adopt UP skeleton, re-apply PR #87 vector/fingerprint/model2vec. **Compile-gate.**
8. **🔴 Tier 1 `semantic_search.rs`** (§3.2): adopt UP `tool_call` dispatch, re-wire retrieval pipeline + RI commands. **Compile-gate.**
9. **Tier 3 tests** (§3.23, 3.24): update test harness to match merged context/index.
10. **`Cargo.lock`** (§3.21): regenerate, do not hand-merge.
11. **modify/delete** (§3.25): delete `spike-output.json`, fix references.
12. **Full validation** (see §5).

> ⚠️ **Before starting:** commit or stash the uncommitted edits to `semantic_search.rs` and `search_index.rs` (the `.aft`/`.bench-cache` grep-exclusion filter) so they are not lost during the merge.

---

## 5. Validation Checklist (post-resolution)

```bash
# Rust
cargo fmt --check
cargo clippy -p aft --all-targets --features semantic-model2vec,semantic-fts5 -- -D warnings
cargo test  -p aft --features semantic-model2vec,semantic-fts5

# TypeScript
bun install
bun run typecheck
bun test

# Schema parity (PR #87 §1)
#   - verify Rust config struct fields match both opencode-plugin and pi-plugin TS schemas
#   - verify feature-gated settings fail clearly when feature unavailable

# Migration smoke (bd-aft-db)
#   - open an existing v3 DB, run the merge, confirm it migrates to the final schema version cleanly

# Benchmark smoke (optional, aft-t6p)
cargo build --release -p aft --features semantic-model2vec,semantic-fts5
bun run benchmarks/semble/corpus.ts sync --pilot
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft --k 10
```

---

## 6. Beads Epic Impact Map

Conflicts cross-reference to active beads. Resolution owners should check bead acceptance criteria:

| Bead epic | Conflicting files touched | Notes |
| :--- | :--- | :--- |
| `aft-fts5e2e` | `semantic_index.rs`, `config.rs`, `*.config.ts`, `status.rs`, `semantic_test.rs` | Feature gating + runtime config is the *in-progress* bead (`aft-fts5e2e.1`); merge must preserve gating. |
| `aft-t6p` / `aft-t6p.snippet` | `semantic_search.rs`, `pilot.ts` (no conflict) | Snippet enrichment is a ready bead sequence (`.06`–`.09`); ensure `--snippet-limit` lands after merge. |
| `bd-aft-db` | `db/mod.rs`, `config.rs`, `*.config.ts` | Schema migration reconciliation is the critical path. |
| `bd-aft-ri` | `semantic_search.rs`, `lib.rs`, `commands/mod.rs`, `configure.rs` | `aft_orient`/`aft_impact_delta`/`aft_context_pack` commands must survive the `tool_call` dispatch migration. |
| `aft-ri-v31` | `semantic_search.rs`, `main.rs` (test), `hybrid_search_test.rs` | Contract baseline test must pass post-merge. |

---

## 7. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- |
| `context.rs` `App` refactor breaks every `ctx.*` call site | High | High | Resolve first among Tier 1; compile-gate before touching other Tier 1 files. |
| `semantic_index.rs` 47-hunk merge introduces silent index corruption | Medium | High | Re-run `aft-fts5e2e.1` acceptance + fingerprint invalidation tests. |
| Schema migration skips a version → data loss | Medium | Critical | Test migration chain on a real v3 DB before merge commit. |
| `Cargo.lock` hand-merge leaves inconsistent dep graph | High | Medium | Regenerate, never hand-merge. |
| Fork's RI commands (`aft_orient` etc.) lost in `tool_call` migration | Medium | High | Explicit re-registration step in §3.2; covered by `bd-aft-ri`. |
| Uncommitted working-tree edits lost | High | Low | Stash/commit before merge (noted in §4). |

---

## 8. Alternative Strategies (if manual merge is too costly)

Given the 239/718 divergence, consider alternatives before committing to a full manual merge:

1. **Rebase fork onto `cortexkit/main`** — replays fork's 239 commits on top of upstream. Conflicts re-surface per-commit but are smaller each time. Better if fork commits are well-isolated (they appear to be, given the RI v3.1 milestone structure). `git rebase cortexkit/main`.
2. **Cherry-pick strategy** — start from `cortexkit/main`, cherry-pick the fork's semantic-layer commits (PR #87 scope) only. Discard fork commits that upstream already superseded. Cleanest history but requires identifying which fork commits are still relevant.
3. **Layered merge** — merge upstream into a *fresh* branch from the fork, resolve in a dedicated `merge/upstream-v0.47` branch, keep `semantic-search-enhancement` untouched until the merge branch passes full validation. **Recommended.**

---

*This plan was produced by a read-only analysis pass. No files in the working tree were modified. The temporary worktree at `D:/tmp/aft-merge-conflict-probe` should be removed with `git worktree remove` (see cleanup).*
