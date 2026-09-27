# Views: incremental `derived.sqlite` materialization

Status: design for review, 2026-09-27. Nothing here has been implemented. Companion finding:
`docs/investigations/views-soak-2026-09/parity-divergence-2026-09-27.md`.

## 0. Premise correction: the incremental materializer already exists

The brief uses the Run 3 cost attribution in `branch-drill.md`: 35.8–43.8 s of full
`derived.sqlite` materialization per switch and a ~257 MiB temporary generation. Those numbers were
committed at 2026-09-12 18:34 (`4beba9efe`). The incremental path landed **the same evening** and
was tuned over the following week:

| commit | date | change |
|---|---|---|
| `6691e112c` | 09-12 18:56 | bound derived graph writes to manifest changes and incoming relinks |
| `e0c66a63e` | 09-12 20:12 | persist binding dependencies, resolve only affected callers |
| `26977aa22` | 09-12 20:29 | apply the manifest diff to a cloned generation (clone-then-patch) |
| `a30a5e35e`, `7412ef03d` | 09-13 | persisted resolver surfaces; invalidation by consulted resolution facts |
| `7ac8dd82c` … `cd68886df` | 09-15 – 09-18 | bounded deletion, caller-batched lookups, UPSERT reconciliation, parallel decode |

The paragraph in `branch-drill.md` that names this as a future follow-up was never updated.
Measured state on the real opencode 300-Git-path pair (285 changed manifest entries):

| scope | derived materialization | source |
|---|---|---|
| Run 3, full rewrite (09-12) | 35.8–43.8 s | `branch-drill.md` |
| release offline, cold | 23.9–31.6 s | `materialization/MEASUREMENTS.md` §Same-input release |
| release offline, incremental | 5.35–5.48 s wall, 4.4 s CPU, 109 MB physical, 107 MB WAL | `materialization-emission-2026-09-18.md` |
| in daemon, fresh storage, load 17–32 | 6.7–12.6 s | same, §Fresh-storage in-daemon drill |

So the job is not to introduce incremental materialization. It is to (a) write down its design so
that it can be reviewed against the rules in the brief, and (b) close the gaps found while doing
so. Sections 1–4 describe the existing design and cite its code. Section 5 lists the gaps.
Section 6 is the proposal.

## 1. Tables, row ownership, and what an edit invalidates

### 1.1 Tables

The view uses the callgraph schema from `callgraph_store::initialize_schema`
(`callgraph_store/mod.rs:8321-8460`) plus two cache tables it creates itself
(`views/materialization.rs:209-218`).

| table | key | owned by | contents |
|---|---|---|---|
| `files` | `path` | the path | content key and language |
| `nodes` | `id = view:{path}:{scoped_name}:{ordinal}` | `file_path` | symbols |
| `refs` | `ref_id = view:{caller}:{ref_ordinal}` | `caller_file` | every reference, with its resolution (`status`, `target_node`, `target_file`, `target_symbol`) |
| `edges` | `edge:{ref_id}` | the owning ref (`ref_id`) | resolved call edges only (`kind='call'`) |
| `file_dependencies` | `(file_path, dep_file)` | `file_path` (the dependent) | every path or pseudo-domain the dependent's resolution consulted, **including misses** |
| `view_bindings` | `file_path` | the caller | JSON: binding dependencies, consulted configuration facts, surface-query answers |
| `view_file_surfaces` | `file_path` | the file | JSON: compact resolver surface (exports, aliases, modules, re-exports) |
| `meta` | `k` | the generation | `view_manifest_fingerprint`, `view_materialization_version` (now `6`), readiness |

The view path does not populate the legacy-only tables (`dispatch_hints`, `type_ref_names`,
`resolution_config_fields`, `backend_file_state`, `staging_*`). They exist but stay empty.

### 1.2 Rows that depend on files other than their owner

A changed path always rewrites its own `files`, `nodes`, `refs` and `edges` rows
(`materialization.rs:303-361` deletes them; `:438-479` and `:725-958` write them back). Rows owned by
**unchanged** files depend on changed files through four channels:

1. **Resolution targets.** A ref's `target_node` embeds the target's scoped name and AST
   ordinal. Any edit that renumbers the target file's symbols, or changes which file or symbol a
   name resolves to, changes the tuple on refs and edges owned by unchanged callers. These rows
   are *relinked*, not rebuilt. The caller's existing refs are loaded once per caller and only
   differing tuples are rewritten (`:776-830`). On the real pair, 55,387 identical dependent refs
   were skipped and 3,325 were rewritten.
2. **Re-exports and barrels.** Resolving through `export … from` chains records every
   intermediate file in `file_dependencies` and every surface query answer in `view_bindings`.
   Changing a barrel therefore reaches the barrel's importers transitively
   (`new_reexport_target_invalidates_transitive_unchanged_importer`).
3. **Membership.** Resolution probes that missed, such as a candidate module file that did not
   exist, are stored as dependencies (MEASUREMENTS §Selection). When such a file appears, the
   callers that probed for it are reselected
   (`added_and_removed_targets_relink_previously_unresolved_callers`,
   `added_rust_module_invalidates_missing_candidate`). Rust crate-wide inline-module and parent
   lookups use a pseudo-path, `\0view:rust-module-index` (`callgraph_store/join.rs:2207`), which
   is seeded on every non-empty diff (`materialization.rs:1241-1246`).
4. **Configuration.** `package.json`, `tsconfig.json`, `pnpm-workspace.yaml` and `Cargo.toml` are
   read as manifest facts. Callers are invalidated by the **fields** they consulted
   (`resolution_facts::diff_inputs`, `materialization.rs:236-248`). If a configuration file is
   added or removed, the pseudo-path `\0view:config-membership` (`join.rs:2206`) is seeded.

**Method dispatch is not a channel today.** The view extract sets `dispatch_hints: Vec::new()`
(`join.rs:1117`), and emission writes only `kind='call'` edges (`materialization.rs:931-935`). The
legacy cold build inserts method-dispatch edges (`insert_method_dispatch_edges_chunked`,
`callgraph_store/mod.rs:13397`). This design adds no dispatch cost. If dispatch edges are added
to views, they need their own pseudo-domain per method name, for example
`\0view:dispatch:<method>`. Its seeds would be files whose method set or receiver types changed,
mirroring the legacy refresh (`eeda14a10`, `c3c2716fe`). Whether views-on `callers` misses
dispatch-only callers that legacy returns is **a separate parity question this note does not
answer**. It should be checked before views become the default.

### 1.3 How the re-resolved set is bounded

Selection (`materialization.rs:234-280`, `dependent_closure` at `:1224-1262`) works in three steps:

1. **Seeds**: changed paths (excluding configuration files whose presence did not change), callers
   invalidated by configuration facts, and the membership and Rust-module pseudo-paths.
2. **Candidates**: the transitive reverse closure over `file_dependencies`, computed as a
   recursive CTE through `idx_file_dependencies_dep_file`. On the real pair this is 1,430–1,635
   paths.
3. **Pruning by surface replay**: for each candidate, the resolver-surface queries it consumed
   last time are replayed against the new surfaces. Only callers with a changed answer are decoded
   and re-resolved: 296–323 of them, plus the 265–280 changed callers. Everything else keeps its
   binding without decoding its blob.

The bound is therefore *the changed files, plus the unchanged callers whose recorded resolution
inputs changed answer*. Work is not bounded by the size of the closure, only by the answers that
changed. Surface restore touches 4,632–4,637 compact surfaces. That part scales with the manifest,
but costs tens of milliseconds.

**Full resolution** (no pruning; every caller re-resolved and relinked with tuple comparison) is
forced when any of the following changed: a synthetic, symlink or gitlink entry
(`requires_full_resolution`, `:1264-1282`); a configuration read with unknown facts; or an
unattributed configuration read (`:249-253`). Each case logs
`views materialization: full resolution (reason=…)`.

## 2. Publication: atomicity, crash windows, live-file rules

### 2.1 Current sequence (clone-then-patch under generation pointers)

`views/assembly.rs::prepare_checkout` (`:153-594`) and `PreparedAssembly::commit` (`:89-135`) run
these steps:

1. Pin the current generation (`QueryPin`) and create an assembly pin for the next generation
   (`:167-171`, `:345-352`).
2. If the new manifest is callgraph-equivalent to the current one (a semantic-only fill), write a
   durable `derived-<next>.ref` that points at the current owner. No derived write happens
   (`:468-479`, `generation.rs:36-53`).
3. Otherwise **clone** the current owner's file to `derived-<next>.sqlite` (`:483-491`,
   `generation.rs::clone_derived` `:386-429`).
4. Open a keeper connection that is attached to the WAL, so closing the materializer cannot
   checkpoint before publication (`:495-507`).
5. `apply_manifest_diff(clone, owner_manifest, next_manifest)` (`:510-525`). This is **one
   `BEGIN IMMEDIATE` transaction** with `synchronous=FULL` and `wal_autocheckpoint=0`
   (`materialization.rs:1090-1110`, `:168-169`, `:990`). The base fingerprint is verified first
   (`:170-208`).
6. Closure and durability steps (`PublicationStep`, `views/mod.rs:561-578`), then the pointer CAS:
   `UPDATE pointer SET generation=?new WHERE generation=?base` in one FULL-synchronous transaction
   (`views/mod.rs:602-629`).
7. A deferred `wal_checkpoint(TRUNCATE)` runs on the keeper after publication (`generation.rs:293-381`).
8. The sweep removes non-current generations that have no live pin and no read marker, under the
   pointer lock (`generation.rs:95-171`).

**Readers never see a half-built generation.** Readers resolve `current` from the pointer and pin
it. The next generation's file is private until the pointer commit. The materializer's changes are
inside one SQLite transaction on that private file. The published file is never written again
except by checkpointing, which does not change its logical content.

### 2.2 Crash windows

| crash point | on-disk state | recovery |
|---|---|---|
| during the clone | partial `derived-<next>.sqlite`; no manifest; pointer = base | `PreparedAssembly::drop` or the sweep removes it once the assembly pin's owner is dead; a retry recreates it after checking that no connection is live on it (`generation.rs:394-416`) |
| during the diff transaction | the clone's WAL has an uncommitted transaction | SQLite rolls back to base content; the unpublished file is reclaimed as above |
| after the diff commits, before the CAS | complete but unpublished generation | reclaimed; the pointer still names the base |
| during the pointer CAS | pointer WAL | old or new generation, never both (R18) |
| after the CAS, before the deferred checkpoint | committed, fsynced frames in the derived WAL | WAL replay on the next open; the next clone copies through the backup API, which includes WAL content |

### 2.3 Live SQLite file-set rules (as enforced today)

- **Never open a second descriptor on a live database set.** A POSIX advisory lock does not
  conflict between two descriptors of the same process, and closing *any* descriptor on the inode
  releases the process's locks. A second connection attached to a *different* file at the same
  path truncates a `-shm` that another connection has mapped (SIGBUS). See
  `db/file_identity.rs:1-25`. All access goes through `IdentityConnection`. File-set mutations
  hold `filesystem_guard()` (`file_identity.rs:227`) and refuse when `open_connections(path) != 0`
  (`:394`). Removal goes through `guard_replacement` (`:418`).
- **Clone through a connection.** Since `1c8e5df50` (2026-09-20), `clone_derived` uses the SQLite
  backup API. It replaced `clonefile`/reflink, which needed a pre-checkpoint that opened the
  source behind live connections, and an `fs::copy` fallback.
- **Durability inside SQLite.** fsync is done by FULL-synchronous commits and checkpoints, never
  by reopening the main file to call `fsync` (same commit).
- **Never rename a live set.** Generations get new file names. Indirection uses `derived-<gen>.ref`
  files, not renames.

## 3. Equivalence: incremental equals cold

### 3.1 Definition

For a manifest `M`, let `cold(M)` be `materialize_manifest_view_database` into an empty file, and
let `inc(B, M_b → M)` be `apply_manifest_diff` on a copy of a database `B` whose logical content
equals `cold(M_b)`. The requirement is `L(inc(B, M_b → M)) == L(cold(M))`, where `L(db)` is:

1. **Schema**: the set of `(type, name, tbl_name, sql)` from `sqlite_schema`, excluding
   `sqlite_%` internals. This covers tables and indexes, including the two cache tables created
   with `CREATE TABLE IF NOT EXISTS`.
2. **Rows**: for every table in the union of both schemas, the *multiset* of rows. Each row is the
   tuple of all declared columns as `(storage class, bytes)`: `INTEGER 1` differs from `REAL 1.0`
   and from `TEXT '1'`, and JSON payloads compare as exact text. Implicit `rowid` is excluded:
   insertion order legitimately differs, and no view reader may depend on it. Readers use text
   keys (`ref_id`, `id`, `path`).
3. **meta**: compared in full. The fingerprint and version must match the target manifest.

This is R24 made precise: equal logical row sets, not equal SQLite files. Page layout,
freelists and WAL contents are explicitly outside `L`.

### 3.2 What the current test checks, and what it misses

`materialization/tests.rs::snapshot` (`:87-110`) plus `assert_snapshot_parity` (`:770-793`)
compare sorted, typed rows of every table named in the *expected* database. It has three gaps:

- A table that exists only in the actual database is not detected, because the comparison
  iterates the expected tables only.
- Index and schema SQL are not compared, so a missing or different secondary index passes.
- It depends on `Debug` formatting of `rusqlite::types::Value`. This is adequate but implicit.
  Blobs, for example, compare as their `Debug` rendering.

### 3.3 Differential test plan

1. **Tighten the comparator** as in §3.1: compare schemas, take the union of tables, and compare
   typed values. Guard it with a `NON-VACUITY BREAK` that makes the incremental path create an
   extra index. The strengthened comparator must fail and the old one must pass.
2. **Keep the existing deterministic fixtures** as named regressions: cross-file relink, bounded
   deletes, mismatched-base refusal, missing-blob rollback, re-export transitivity, Rust module
   addition, tsconfig change, surface pruning, and ordinal collisions.
3. **Generated sequences.** Use a seeded generator over a small mixed TypeScript and Rust corpus
   (barrels, namespace imports, default exports, a workspace `package.json`, `tsconfig` paths,
   Rust `mod`/`pub use`, a symlink). Operations: edit a body, insert a symbol before others
   (renumbers ordinals), add, remove, rename, move a re-export, change or add or remove a
   configuration file, and toggle a symlink. For each generated manifest sequence
   `M0 … Mn`, assert each of the following:
   - adjacent steps: `inc(cold(Mi), Mi → Mi+1) == cold(Mi+1)`;
   - **chained**: apply every step incrementally from `cold(M0)` and compare with `cold(Mn)`.
     This catches drift, where an error in step *k* seeds a wrong base for step *k+1*;
   - **non-adjacent**: `inc(cold(Mi), Mi → Mj)` for `j > i + 1`, including switch-back
     `Mi → Mj → Mi`. The recycling proposal in §6.2 depends on this;
   - work bounds: `full_resolution == false` unless the step touched a trigger from §1.3.

   Seeds are logged. A failing seed is shrunk to a fixture and committed.
4. **Real corpus**: `bench_real_manifest_diff` on the retained opencode pairs, HEAD→A, A→HEAD,
   HEAD→B and B→HEAD. Add the two-step composites A→B and B→A. The pairs are reproducible from
   their manifest fingerprints (emission report, §Scope).
5. **Mutation controls**, each expected to turn exactly one named test red: skip incoming
   relinks; drop the transitive closure (use seeds only); ignore consulted-fact invalidation;
   skip the `files` delete; skip the Rust-module seed.
6. **Crash**: extend the existing SIGKILL failpoint test, which covers five publication
   boundaries, to the recycle transitions in §6.2. Assert that after restart the pointer names a
   generation whose `L` equals the cold build of its manifest.

## 4. Cost for a 300-file switch on opencode

### 4.1 Current code, per graph-changing publication

| phase | cost | basis |
|---|---|---|
| manifest assembly, blob lookup | 1.7–3.1 s; 0.2–1.5 s | Run 3 and in-situ 09-16 |
| **clone (backup API)** | **≈1.8 s and a full-size write (257–428 MiB)** | isolated measurement below; not measured in the daemon since `1c8e5df50` |
| diff materialization | 5.4 s offline; 6.7–12.6 s in the daemon under load | emission report |
| closure | 0.04–0.16 s | in-situ 09-17 |
| pointer CAS | milliseconds; Run 3's 4.4–7.3 s pointer phase most likely included the derived checkpoint that `f097c83d2` (09-13) moved off the publication path | in-situ 09-16 |
| deferred checkpoint | off the publication path | `generation.rs:293` |

**Clone measurement** (2026-09-27, `rusqlite 0.32.1` bundled, release build, isolated `/tmp`
database of 64,158 pages = 262,791,168 bytes, host load ≈ 9). The production parameters
`run_to_completion(256, 5 ms)` took **1.82 s**. One step (`i32::MAX` pages, no pause) took
**0.31 s**. Both wrote the whole file. The gap is the sleep floor: rusqlite sleeps after every
non-final step (`backup.rs:303`), and 251 steps × 5 ms = 1.25 s. The derived database of that
period was 269,889,536 bytes (Run 3). With persisted surfaces it was later measured at 424–428 MB
(MEASUREMENTS §Final run), which scales the clone to roughly 3 s. The size under the current
schema has not been re-measured.

The 09-18 drill (6.7–12.6 s derived) predates the backup-API clone. At that time the clone used
`clonefile` and cost 4–17 ms with 0 SQLite page writes (offline, emission report). **Expected today: roughly 9–15 s derived per forward switch
under load, of which ~2–3 s is an avoidable full copy.** This is an estimate and needs one
confirming drill.

### 4.2 Remaining materialization buckets (release offline, 09-18)

Selected join ≈ 1.8 s; delete rows ≈ 1.0 s; ref/edge emission ≈ 0.45 s; checkpoint ≈ 0.3 s; the
rest < 0.1 s each. Owned rows dominate: 53,914 deleted and 51,632 inserted for changed owners,
versus 3,325/3,327 for dependents. Owned rows are expensive because `ref_id` and node IDs contain
AST ordinals, so any structural edit early in a file renumbers every later row. Stable,
ordinal-free IDs would turn most owned-row churn into no-ops. That would change the ID contract
shared with legacy readers, so it is listed as a lever, not proposed here.

## 5. Gaps found

- **G1 — the clone became a full copy (and a full-size temporary file) again.** Cause:
  `generation.rs:418-419`, since 2026-09-20. It adds ~1.8 s per 257 MiB with the current step
  parameters, and roughly 257–428 MiB of writes per graph-changing switch.
- **G2 — no fallback when the base is unusable.** A fingerprint mismatch returns
  `"…cold materialization required"` (`materialization.rs:189-192`), but no caller acts on it:
  `assembly.rs:525` turns it into `InvalidManifest` and the publication fails. The same happens if
  the base is corrupt: backup or open errors propagate. The only fallbacks are a missing base file
  (cold), missing diff metadata, or a version mismatch (cold inside the clone,
  `materialization.rs:185-194`).
- **G3 — no size cutoff.** Any diff takes the incremental path. At the extreme (a rebase touching
  most files) it pays deletions plus a near-full join, which is more than cold. Debug
  measurements already showed incremental slower than cold on wall time for the 285-entry pair
  before the surface work (MEASUREMENTS §Offline measurement: 291 s vs 263 s).
- **G4 — gaps in the equivalence comparator** (§3.2).
- **G5 — a switch-back pays the reverse diff.** A→HEAD re-applies the ~285-entry diff (5–8 s),
  even though a HEAD-content derived file existed until the sweep removed it.
- **G6 — dispatch edges are absent from views** (§1.2). Parity status unknown.

## 6. Proposal

### 6.1 P1: one-step backup (small, independent)

Call `run_to_completion` with all pages in one step and no pause. The source is an immutable
published generation: only checkpoints touch it, and they do not change its logical content. The
pause exists to let concurrent writers make progress, and there are none here. Holding the
source's read lock for ~0.3 s blocks nobody. Expected saving: ~1.5 s per switch. The full-size
write is unchanged. Guard: a unit test asserting a single `step` call, or a timing bound on a
fixture.

### 6.2 P2: recycle the retired generation's file instead of cloning

**Idea.** Keep one *retired slot* per view: the derived file of the most recent generation that
lost the pointer. On the next graph-changing publication, choose the base with the smaller diff
to the target:

- **clone base**: copy the current generation (today's path, with P1): cost ≈ backup + `|Δ(current → next)|`;
- **recycle base**: patch the retired slot in place: cost ≈ `|Δ(slot → next)|`, with no copy.

This is safe because `apply_manifest_diff` is defined for any `(base, next)` pair of manifests.
`changed` is the symmetric entry difference (`materialization.rs:219-225`), and the base is
identified by its recorded fingerprint, not by adjacency. That safety has to be proven by the
non-adjacent differential tests in §3.3.

Effect on the drill sequence HEAD→A→HEAD→B→HEAD:

| switch | retired slot holds | best base | derived work |
|---|---|---|---|
| HEAD→A | nothing or older | clone of HEAD | Δ ≈ 285, no change except P1 |
| A→HEAD | HEAD | **recycle** | Δ(HEAD → HEAD) = 0 → fingerprint-only commit (tens of ms) |
| HEAD→B | A | clone of HEAD (Δ(A→B) ≈ 570 > Δ(HEAD→B) ≈ 283) | as today with P1; slot A is released to the sweep |
| B→HEAD | HEAD | **recycle** | ≈ 0 |

On HEAD→B the slot (A) is the worse base, so the clone path is used. The comparison of `|Δ|` is a manifest walk that takes milliseconds. Weight it with a
per-entry cost (≈ 15–20 ms per changed entry on this corpus, from 5.4 s / 285) against the clone
cost to decide.

**Protocol, stated as a variant of the sweep.** Recycling is "sweep generation G, but hand its
file to the assembly instead of deleting it". It runs under the same pointer-database
`BEGIN IMMEDIATE` lock that `sweep_generations` and `reuse_derived` use (`generation.rs:40-42`,
`:98-100`):

1. The candidate file must satisfy every condition:
   - no generation that references it is `current`;
   - no live assembly or query pin on any of those generations;
   - no read markers (`root_cache::sweep_read_markers`);
   - `open_connections(path) == 0` while holding `filesystem_guard()`;
   - after the pin checks, re-read `current` and confirm it still does not name a referencing
     generation (the sweep performs the same recheck, `:162-166`).
2. Retire those generations *logically*: delete their `.ref` files and record in the pointer
   database a row `slot(path, holder = <next generation>, base_fingerprint)`. Do this before
   releasing the lock. Their manifest JSON files are kept until step 4, because the diff needs
   the base manifest.
3. Open the slot through an `IdentityConnection`, keep the keeper connection as today, and run
   `apply_manifest_diff(slot, base_manifest, next_manifest)`. That is one transaction, as today.
   Then write `derived-<next>.ref` pointing at the slot's owner name, so the physical file is
   **never renamed**.
4. Continue the normal closure and pointer CAS. On success, delete the retired generations'
   manifests and the `slot` row. On conflict or error, the file is unpublished. Its fingerprint
   says which manifest it holds, and the sweep reclaims it.

**Crash windows (recycle).**

| crash point | state | recovery rule |
|---|---|---|
| after step 2, before the diff commits | slot content = base (SQLite rollback), `slot` row names a dead holder | the sweep sees a `slot` row whose holder's pin owner is dead → the file becomes the retired slot again (fingerprint = base, base manifest still present) or is deleted |
| after the diff commits, before the CAS | slot content = next, unpublished | a retry of the same target reads the fingerprint = next → zero-work adoption; otherwise the sweep deletes it |
| during or after the CAS | as today | as today |

Invariant: **a physical derived file is referenced by at most one of {published generations,
one in-flight assembly}**, and it changes hands only under the pointer lock. Readers can never
hold it while it is being patched, because every reader path pins a generation that is current,
and this file's generations stopped being current before step 1.

**Retention cost.** The sweep must spare one retired slot per view. Steady-state disk becomes two
derived files per view-enabled root (+257–428 MiB each), instead of one plus a transient third
during publication. A global byte budget (#210) may evict retired slots first. They hold no
unique information.

### 6.3 Alternatives considered and rejected

- **Restore `clonefile`/reflink.** Near-zero cost and copy-on-write sharing, but it copies the
  main file behind SQLite. It requires the main file to be quiescent, meaning fully checkpointed
  with no concurrent checkpoint. No in-process lock can enforce that against another process.
  Linux `FICLONE` needs a descriptor on the live source, which breaks the second-descriptor rule.
  It is rejected under the brief's "clone through a connection" rule.
- **`VACUUM INTO`.** Goes through a connection, but rewrites the whole file, so it costs more
  than a one-step backup.
- **Patch the single live file in place, with the generation pointer stored inside
  `derived.sqlite`.** SQLite's atomic commit would give readers a consistent snapshot *if*
  every reader took its generation and all its derived reads from one read transaction. Today
  readers pin a generation and issue many statements across a tool call. Tier-2 and navigation
  hold generations longer. All of them would have to change. The pointer would also move out of
  the pointer database, breaking the R18/R20 composition with manifest, semantic and trigram
  artifacts. Readers holding long snapshots block checkpoints. The blast radius is too large for
  the gain over P2.
- **`sqlite3_snapshot_open` to pin readers to an older snapshot.** Needs `SQLITE_ENABLE_SNAPSHOT`,
  snapshots do not survive checkpoints or restarts, and it has the same reader-contract change as
  above.

### 6.4 P3: fallback policy

Decide in `prepare_checkout` before materializing, and again on error:

| condition | action |
|---|---|
| no current generation, or its derived file is missing | cold (as today) |
| base lacks diff metadata, or `view_materialization_version` differs | cold inside the clone (as today) |
| **fingerprint mismatch** | discard the clone and cold-materialize into a fresh file; log `reason=base_fingerprint_mismatch` (closes G2) |
| **base unreadable**: backup or open fails with `SQLITE_CORRUPT`/`SQLITE_NOTADB`, or `quick_check` fails | cold into a fresh file; quarantine the base (keep it for inspection, never reuse it); log the reason. Do not run `integrity_check` on the hot path: it scans the whole file |
| missing blob during the diff | **not** a fallback: the transaction rolls back and the path is reported pending (cold would fail the same way) |
| **diff too large**: changed callgraph entries > 25 % of manifest entries, or `full_resolution` would be forced **and** changed entries > 10 % | cold (closes G3). Calibrate both constants with a sweep of the real-pair benchmark at 1 %, 5 %, 10 %, 25 % and 50 % synthetic change, and set them at the measured crossover. The 25 % starting point comes from a linear model: ≈1.2 s fixed + ≈15 ms per entry, against a ≈24 s cold build |
| recycle is ineligible | clone path |

Every fallback is observable: extend the existing `view manifest diff` log line with
`path=incremental|recycle|cold reason=…`.

### 6.5 Order of work (after review)

1. P1 and the §3.2 comparator fix: small and independent. Include a drill to confirm §4.1.
2. P3 fallbacks, with their fixtures: fingerprint mismatch, a corrupt base (a truncated file),
   and an oversized diff.
3. The §3.3 generated differential suite, including non-adjacent and chained cases. This must be
   green **before** P2.
4. P2 recycling, with crash failpoints at each transition in §6.2 and one isolated-storage drill.

## 7. Decisions requested

1. Approve the retention change that P2 needs: one retired derived file per view, about
   +257–428 MiB of steady-state disk per view-enabled root. Alternatively, limit it to roots that
   switched within a time window.
2. Confirm that the "diff too large" constants may be set from measurement (§6.4) rather than
   fixed in the design.
3. G6 (dispatch edges in views) is outside this note. Decide whether it gets its own parity
   check before views ship as the default.
