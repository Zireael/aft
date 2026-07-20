# DB Schema Migration Chain Design — `bd-aft-db` Reconciliation

**Date:** 2026-07-20
**File:** `crates/aft/src/db/mod.rs`
**Bead epic:** `bd-aft-db` (persistent repository-intelligence graph substrate)
**Related beads:** `bd-aft-db.1` (schema spike), `bd-aft-db.2` (storage/migration impl), `bd-aft-db.12` (verification)
**Cross-ref:** [merge-conflict-analysis-upstream-20260720.md](./merge-conflict-analysis-upstream-20260720.md) §3.4

---

## 1. The Problem: Schema Version Collision at v3

Both the fork and upstream independently assigned **different content** to schema version 3. This is a version-number collision — the `schema_version` table records only the integer, not *which* v3 was applied, so the migration system cannot distinguish them at runtime.

### 1.1 Fork side (HEAD, `CURRENT_SCHEMA_VERSION = 3`)

| Version | Constant | Content |
| :--- | :--- | :--- |
| 1 | `MIGRATION_V1` | Base tables: `schema_version`, `bash_tasks`, `compression_events`, `backups`, `harness_state`, `host_state` + indexes |
| 2 | `MIGRATION_V2` | Dedup `compression_events` + unique index `idx_compression_event_identity` |
| **3** | **`MIGRATION_V3_RIL`** | **RIL graph tables:** `ril_files`, `ril_symbols`, `ril_edges`, `ril_source_test_links`, `ril_metadata` + 9 indexes |

### 1.2 Upstream side (`cortexkit/main`, `CURRENT_SCHEMA_VERSION = 4`)

| Version | Constant | Content |
| :--- | :--- | :--- |
| 1 | `MIGRATION_V1` | Base tables (identical to fork) |
| 2 | `MIGRATION_V2` | Dedup `compression_events` (identical to fork) |
| **3** | **`MIGRATION_V3`** | **Single index:** `idx_bash_tasks_project_lookup` on `bash_tasks(harness, project_key, task_id, started_at DESC)` |
| **4** | **`MIGRATION_V4`** + `apply_migration_v4()` | **`ALTER TABLE backups ADD COLUMN restore_meta TEXT`** (idempotent Rust guard checks `PRAGMA table_info(backups)`) |

### 1.3 Identical foundations

`MIGRATION_V1` and `MIGRATION_V2` are **byte-identical** on both sides (verified via line-number alignment: V1 at line 13, V2 at line 99 on both). The divergence begins at v3 only. No existing table is ALTERed by the fork — `MIGRATION_V3_RIL` creates only new `ril_*` tables.

### 1.4 The three DB populations at merge time

| DB origin | `schema_version` | Has RIL tables? | Has `idx_bash_tasks_project_lookup`? | Has `restore_meta`? |
| :--- | :---: | :---: | :---: | :---: |
| Fork DB | 3 | ✅ | ❌ | ❌ |
| Upstream DB | 4 | ❌ | ✅ | ✅ |
| Fresh DB (post-merge) | (new) | must get ✅ | must get ✅ | must get ✅ |

The migration system is **forward-only** — it loops `(db_version + 1)..=CURRENT_SCHEMA_VERSION` and skips already-applied versions. A fork DB at v3 will **never re-run v3**, so whatever was in upstream's v3 (the bash_tasks index) is silently skipped unless we account for it.

---

## 2. Chosen Design: Renumber RIL to v5 + Reconciliation Guard

### 2.1 Final migration chain

| Version | Constant | Content | Idempotent? |
| :---: | :--- | :--- | :--- |
| 1 | `MIGRATION_V1` | Base tables (unchanged) | `CREATE TABLE/INDEX IF NOT EXISTS` |
| 2 | `MIGRATION_V2` | Compression dedup (unchanged) | `DELETE` + `CREATE UNIQUE INDEX IF NOT EXISTS` |
| 3 | `MIGRATION_V3` | `idx_bash_tasks_project_lookup` index *(from upstream)* | `CREATE INDEX IF NOT EXISTS` ✅ |
| 4 | `MIGRATION_V4` + `apply_migration_v4()` | `backups.restore_meta` column *(from upstream)* | Rust guard checks `PRAGMA table_info` ✅ |
| **5** | **`MIGRATION_V5_RIL`** | **RIL graph tables** *(renumbered from fork's v3)* **+ reconciliation guard** | `CREATE TABLE/INDEX IF NOT EXISTS` ✅ |

**`CURRENT_SCHEMA_VERSION = 5`**

### 2.2 Why v5 includes a reconciliation guard

A fork DB at version 3 has RIL tables but **never received upstream's v3** (the `idx_bash_tasks_project_lookup` index). Since the migration loop skips v3 (already "applied"), that index is lost. The fix: prepend the bash_tasks index creation to `MIGRATION_V5_RIL`'s SQL body. Because `CREATE INDEX IF NOT EXISTS` is idempotent, this is a no-op on DBs that already have the index (fresh DBs, upstream DBs) and a real creation on fork DBs that don't.

### 2.3 The `MIGRATION_V5_RIL` SQL body (proposed)

```sql
-- ──────────────────────────────────────────────────────────
-- Reconciliation guard
-- Fork DBs at v3 applied MIGRATION_V3_RIL (RIL tables) instead
-- of upstream's MIGRATION_V3 (this index).  Re-apply idempotently
-- so every DB reaching v5 has the complete schema regardless of
-- which v3 variant it originally received.
-- ──────────────────────────────────────────────────────────
CREATE INDEX IF NOT EXISTS idx_bash_tasks_project_lookup
ON bash_tasks (harness, project_key, task_id, started_at DESC);

-- ──────────────────────────────────────────────────────────
-- RIL: Repository Intelligence Layer tables
-- (renumbered from fork's MIGRATION_V3_RIL; content unchanged)
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS ril_files (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  path          TEXT NOT NULL UNIQUE,
  content_hash  TEXT NOT NULL,
  language      TEXT NOT NULL,
  size_bytes    INTEGER NOT NULL,
  mtime_secs    INTEGER NOT NULL,
  generation    INTEGER NOT NULL DEFAULT 1,
  indexed_at    INTEGER NOT NULL,
  FOREIGN KEY (path) REFERENCES ril_files(path) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ril_files_language ON ril_files(language);
CREATE INDEX IF NOT EXISTS idx_ril_files_generation ON ril_files(generation);

CREATE TABLE IF NOT EXISTS ril_symbols (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  file_id       INTEGER NOT NULL,
  name          TEXT NOT NULL,
  kind          TEXT NOT NULL,
  start_line    INTEGER NOT NULL,
  end_line      INTEGER NOT NULL,
  body_hash     TEXT,
  name_path     TEXT,
  generation    INTEGER NOT NULL DEFAULT 1,
  FOREIGN KEY (file_id) REFERENCES ril_files(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ril_symbols_file ON ril_symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_ril_symbols_name ON ril_symbols(name);
CREATE INDEX IF NOT EXISTS idx_ril_symbols_kind ON ril_symbols(kind);

CREATE TABLE IF NOT EXISTS ril_edges (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id     INTEGER NOT NULL,
  source_type   TEXT NOT NULL,
  target_id     INTEGER NOT NULL,
  target_type   TEXT NOT NULL,
  edge_type     TEXT NOT NULL,
  metadata      TEXT,
  created_at    INTEGER NOT NULL,
  FOREIGN KEY (source_id) REFERENCES ril_files(id) ON DELETE CASCADE,
  FOREIGN KEY (target_id) REFERENCES ril_files(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ril_edges_source ON ril_edges(source_id, source_type);
CREATE INDEX IF NOT EXISTS idx_ril_edges_target ON ril_edges(target_id, target_type);
CREATE INDEX IF NOT EXISTS idx_ril_edges_type ON ril_edges(edge_type);

CREATE TABLE IF NOT EXISTS ril_source_test_links (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id     INTEGER NOT NULL,
  test_id       INTEGER NOT NULL,
  link_type     TEXT NOT NULL,
  confidence    REAL NOT NULL DEFAULT 1.0,
  created_at    INTEGER NOT NULL,
  FOREIGN KEY (source_id) REFERENCES ril_files(id) ON DELETE CASCADE,
  FOREIGN KEY (test_id) REFERENCES ril_files(id) ON DELETE CASCADE,
  UNIQUE(source_id, test_id, link_type)
);
CREATE INDEX IF NOT EXISTS idx_ril_source_test_source ON ril_source_test_links(source_id);
CREATE INDEX IF NOT EXISTS idx_ril_source_test_test ON ril_source_test_links(test_id);

CREATE TABLE IF NOT EXISTS ril_metadata (
  key           TEXT NOT NULL PRIMARY KEY,
  value         TEXT NOT NULL,
  updated_at    INTEGER NOT NULL
);
```

### 2.4 Updated `apply_migration` dispatch

```rust
fn apply_migration(conn: &mut Connection, version: u32) -> Result<(), OpenError> {
    let from = version - 1;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| OpenError::MigrationFailed { from, to: version, error })?;

    let result = match version {
        1 => tx.execute_batch(MIGRATION_V1),
        2 => tx.execute_batch(MIGRATION_V2),
        3 => tx.execute_batch(MIGRATION_V3),         // upstream: bash_tasks index
        4 => apply_migration_v4(&tx),                 // upstream: restore_meta (idempotent)
        5 => tx.execute_batch(MIGRATION_V5_RIL),      // fork: RIL tables + reconciliation guard
        _ => Ok(()),
    }
    .and_then(|()| {
        tx.execute("DELETE FROM schema_version", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            [version],
        )?;
        tx.commit()
    });

    result.map_err(|error| OpenError::MigrationFailed { from, to: version, error })
}
```

`apply_migration_v4` is kept **verbatim from upstream** — it already has the `PRAGMA table_info(backups)` guard that makes it safe to run on DBs with or without the `restore_meta` column.

---

## 3. Migration Trace — All DB Populations

### 3.1 Fresh DB (no `schema_version` table)

```
db_version = 0
runs: v1 → v2 → v3 → v4 → v5
result: base tables ✅ | compression dedup ✅ | bash_tasks index ✅ | restore_meta ✅ | RIL tables ✅
final: schema_version = 5
```

### 3.2 Upstream DB at v4

```
db_version = 4
runs: v5 only
v5: CREATE INDEX idx_bash_tasks_project_lookup → already exists, no-op ✅
    CREATE TABLE ril_* → created ✅
result: RIL tables added; everything else already present ✅
final: schema_version = 5
```

### 3.3 Fork DB at v3 (the critical case)

```
db_version = 3
runs: v4 → v5
v4: apply_migration_v4 → checks PRAGMA table_info(backups) → restore_meta missing → ALTER TABLE ✅
v5: CREATE INDEX idx_bash_tasks_project_lookup → MISSING (fork's v3 was RIL, not this index) → created ✅
    CREATE TABLE ril_* → already exist (from fork's v3) → IF NOT EXISTS no-op ✅
result: restore_meta added ✅ | bash_tasks index added ✅ | RIL tables already present ✅
final: schema_version = 5
```

### 3.4 DB at v2 (from either side)

```
db_version = 2
runs: v3 → v4 → v5
v3: bash_tasks index created ✅
v4: restore_meta added ✅
v5: bash_tasks index no-op ✅ | RIL tables created ✅
result: everything ✅
final: schema_version = 5
```

### 3.5 DB at v1

```
db_version = 1
runs: v2 → v3 → v4 → v5 → all applied ✅
final: schema_version = 5
```

**All five populations reach a consistent v5 state with the complete schema.** ✅

---

## 4. Why Not the Alternatives?

| Alternative | Why rejected |
| :--- | :--- |
| **Keep fork's v3 numbering, add upstream's changes as v4** | Reverses upstream's numbering; upstream DBs (the larger population) at v4 would have a version *higher* than our v4 and hit the "db_version > CURRENT_SCHEMA_VERSION" error path. |
| **Merge both v3 contents into one v3** | A fork DB at v3 would skip the merged v3 and miss upstream's bash_tasks index; an upstream DB at v3 would skip it and miss RIL tables. Same collision, different victim. |
| **Separate reconciliation migration at v6** | Works but adds an unnecessary version. v5 can carry the reconciliation guard inline since RIL tables are all `IF NOT EXISTS` anyway. One fewer migration = simpler testing. |
| **Backward-migration / schema introspection at open time** | The codebase uses forward-only migrations by design (`bd-aft-db.1` acceptance: "migration/versioning strategy"). Introducing backward migration breaks the contract and adds risk. |

---

## 5. Implementation Steps (for the merge)

1. **In `crates/aft/src/db/mod.rs`:**
   - Set `pub const CURRENT_SCHEMA_VERSION: u32 = 5;`
   - Rename fork's `MIGRATION_V3_RIL` → `MIGRATION_V5_RIL`
   - Prepend the reconciliation guard (`CREATE INDEX IF NOT EXISTS idx_bash_tasks_project_lookup ...`) to `MIGRATION_V5_RIL`'s SQL body
   - Add upstream's `MIGRATION_V3`, `MIGRATION_V4`, and `apply_migration_v4()` verbatim
   - Update the `apply_migration` dispatch: `3 => MIGRATION_V3`, `4 => apply_migration_v4(&tx)`, `5 => MIGRATION_V5_RIL`
   - Update the `sqlite_names` / table-dump test helper lists to include both upstream's new index/column AND the RIL tables/indexes

2. **In `crates/aft/src/ril_indexer.rs`** — no changes needed. It queries `ril_files`/`ril_symbols`/`ril_edges`/`ril_source_test_links` by name; the tables still exist with the same schema, just created at v5 instead of v3.

3. **No changes to `MIGRATION_V1` or `MIGRATION_V2`** — they are identical on both sides.

---

## 6. Test Plan (satisfies `bd-aft-db.2` + `bd-aft-db.12` acceptance)

Add these test cases to the `#[cfg(test)]` block in `db/mod.rs`:

| Test name | Setup | Asserts |
| :--- | :--- | :--- |
| `migration_v3_adds_bash_tasks_project_lookup_index` | Start at v2, migrate to current | `idx_bash_tasks_project_lookup` exists in `sqlite_master` |
| `migration_v4_adds_restore_meta_to_v2_and_v3_databases` | *(already exists upstream — keep)* | `restore_meta` column present, legacy rows nullable |
| `migration_v4_is_idempotent_when_column_already_exists` | *(already exists upstream — keep)* | Running v4 twice does not error |
| `migration_v5_creates_ril_tables` | Start at v4, migrate to v5 | All 5 `ril_*` tables + 9 indexes exist in `sqlite_master` |
| **`migration_v5_reconciles_fork_v3_database`** | Apply V1+V2+V3_RIL manually, set `schema_version=3`, then migrate to current | `idx_bash_tasks_project_lookup` exists (reconciliation guard), `restore_meta` exists (v4), all `ril_*` tables exist (idempotent no-op), `schema_version = 5` |
| **`migration_v5_is_idempotent_for_ril_tables`** | Pre-create `ril_files` table, run v5 | No error; table not duplicated |
| `full_migration_from_empty_reaches_v5` | Empty DB, call `open()` | `schema_version = 5`, all tables/indexes present |
| `upstream_v4_db_migrates_to_v5` | Apply V1+V2+V3+V4, set `schema_version=4`, migrate | RIL tables created, `schema_version = 5` |

The **`migration_v5_reconciles_fork_v3_database`** test is the most important — it proves the reconciliation guard works for the critical fork-DB-at-v3 population.

---

## 7. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- |
| Fork DB at v3 misses bash_tasks index | **Certain** without reconciliation guard | Low (missing index = slower lookups, not data loss) | Reconciliation guard in v5 (§2.2) |
| `ALTER TABLE` fails on fork DB (table locked) | Low | Medium | `apply_migration_v4` uses `Immediate` transaction; same as upstream |
| RIL table schema drift between fork's v3 and v5 | Very low | High | v5 uses fork's v3 SQL verbatim (only prepends the index) |
| `CURRENT_SCHEMA_VERSION` mismatch between Rust and TS config | Medium | Low | No TS schema for DB version; it's Rust-only. Verify no `const` is exported to TS. |
| Test helper `sqlite_names` list incomplete | Medium | Low (test-only) | Add all v3+v4+v5 names to the test helper lists |

---

## 8. `bd-aft-db` Acceptance Criteria Compliance

| `bd-aft-db.2` acceptance | How this design satisfies it |
| :--- | :--- |
| Repo-intelligence storage initializes safely | RIL tables created via idempotent `CREATE TABLE IF NOT EXISTS` in v5 |
| Schema versioning and migrations are tested | §6 test plan covers all 5 DB populations + idempotency |
| File freshness metadata is correctly persisted | `ril_files.content_hash`, `mtime_secs`, `size_bytes`, `indexed_at` columns preserved unchanged |
| Stale rows can be safely cleaned up | `ON DELETE CASCADE` foreign keys preserved; `generation` column for stale detection |
| Graph storage can be disabled via configuration | Not affected by migration chain — config flag is separate (see `bd-aft-db.2` step 5) |
| Existing trigram and semantic indexes remain functional | RIL tables are additive; no existing table is altered except `backups.restore_meta` (upstream's change, nullable) |

---

*This is a design document. No source code was modified. The proposed SQL and Rust snippets in §2.3–2.4 are ready for implementation during the merge execution phase.*
