# Per-checkout indexes through views, trigram included (end-state design, 2026-09-27)

Status: design only, for an Athena panel before slicing. No product code changed. Base commit
`015fd232c`. Every claim about today's code cites `file:line` at that commit. Every number says
whether it was **measured** (isolated storage, never the live daemon) or **modelled** (derived from
cited measurements plus stated assumptions).

Builds on: `content-addressed-index-views.md` (rulings R1–R25, cited as "R*n*"),
`views-incremental-materialization.md`, `borrowed-semantic-callgraph.md`,
`investigations/borrowed-index-phantom-paths-2026-09.md`,
`investigations/views-soak-2026-09/branch-drill.md`, `umbrella-sessions.md`.

## 0. Decision and summary

The operator's decision is fixed: **one model for every checkout**. Each checkout has content-addressed
per-file data shared through its repository family, its own manifest (its view), and a live in-RAM
delta for edits that are not published yet. All three planes (trigram, semantic, callgraph) use this
model. `worktree.ram_overlay` and the read-only borrowing path are deleted when it lands. There is no
owner and no borrower.

This note settles what that means:

1. **Trigram.** Per-file trigram blobs are keyed by content only. Immutable **segments** hold postings
   and are shared by the family. A segment is today's `cache.bin` format, which already stores relative
   paths and a blake3 per file. A view's queryable index is *segment + overlay + live delta*. The
   overlay holds the manifest paths whose content differs from the segment. This is the
   base/delta/superseded structure `search_index.rs` already runs (`search_index.rs:346-387`,
   `2814-2957`). Views do not assemble postings of their own on disk. Measured on this repository and on
   opencode, a view built this way answers exactly like a cold build of that checkout (0 mismatches in
   916 grep probes). Query time was within 1–2% of the cold build. Per-view postings would cost a full
   29–44 MB `cache.bin` per view.
2. **Freshness.** Every checkout has one live delta over its published generation. Readers take one
   immutable snapshot `(generation, delta version)` per query. The live delta is applied to trigram
   immediately. Semantic is filled after the 15 s quiet window, with pending paths disclosed.
   Callgraph is folded into a generation after the 1 s quiet window plus an incremental
   materialization; until then, affected answers disclose it. A generation switch is one pointer swap
   of a fully built snapshot, and each path has exactly one active source. So a query never sees a gap
   and never counts a file twice.
3. **What goes away:** `artifact_owner.rs`, `owner.json` and its heartbeat, borrow-only `ArtifactAccess`,
   the own-root read-only openers, `ram_overlay` and its seven gates, borrow reconcile, the per-root
   `cache.bin` publish epoch and lease. Blob puts need no lease: they are idempotent, hash-verified and
   done in one SQLite transaction. Publication is a per-view CAS. GC must mark the references of
   **every** view in the family. Today it marks only one view (`gc/mod.rs:92-112`,
   `commands/configure.rs:5940-6009`), and that is a real bug once many views exist.
4. **Semantic and callgraph per view.** A shared in-process vector arena replaces today's view query,
   which scans SQLite per query. A semantic fill embeds a checkout's pending paths on that checkout's
   budget. A new view seeds its derived callgraph from another view's generation, which is sound because
   derived rows do not depend on the root by construction (§6.3). Views must gain method-dispatch edges
   before they become the default (`callgraph_store/join.rs:1117`).
5. **First load** of a mason worktree adopts the closest family generation and pays for its diff D:
   modelled at 2–3 s plus embeds for D≈16, and 7–10 s for D≈300. A cold view costs 24–44 s.
6. **Umbrella sessions** read each child repository's current generation through a read-only view
   reader. There is no live delta for child repos. Staleness is found by content and disclosed.
7. **Honest reporting** uses one freshness block for every index-backed answer. `complete: true` only
   when the planes used equal a cold build of the current disk.
8. **Cutover** imports legacy caches: the trigram `cache.bin` becomes the first segment with no rebuild,
   semantic imports on the same root, and callgraph is re-extracted once. After that the tree is
   views-only.

---

## 1. Today's code, as it bears on this design

### 1.1 Owner and borrower

- **Family key.** Every checkout whose HEAD history shares a root commit gets the same artifact key
  (phantom note §2.1). The first checkout to claim writes `artifact-owners/<key>/owner.json`
  (`artifact_owner.rs:96-190`, `639-651`). Linked worktrees always borrow (`:104-110`). Clones borrow
  whenever the recorded owner pid is alive (`:148-161`), and in the daemon that pid is its own
  (phantom note §2.2). Owners keep their lease alive from a heartbeat thread (`:239-253`, `305-343`,
  `404-423`).
- **Borrowers.** A borrower has no callgraph writer, so `shared_artifacts_read_only()` is true
  (`context.rs:5030-5032`). It opens the owner's `cache.bin` and `semantic.bin` read-only
  (`readonly_artifacts.rs:148-188`, `257-307`) and the owner's callgraph generation read-only
  (`callgraph_store/mod.rs:3486-3542`). Writes are fenced by `ArtifactAccess.borrow_only_shared`
  (`root_cache.rs:134-203`, registered at `:322-338`).
- **`worktree.ram_overlay`.** The key is declared at `config.rs:445-461` (default `false` at `:459`)
  and resolved at `config_resolve.rs:512-520`, `1447`, `1952-1958`. The predicate is
  `ram_overlay_active()` (`context.rs:5046-5053`). Its gates are: bind reconcile
  (`commands/configure.rs:4203-4204`); the watcher RAM apply (`runtime_drain.rs:2598`, `2645-2647`);
  the semantic refresh worker (`commands/configure.rs:1215-1226`, `4032`, `4050`); idle reload
  (`:3940-3948`); and rescan retirement (`runtime_drain.rs:2344-2346`). Commit `0f138f0ca`, on a task
  branch and not in this base, flips the default to `true`. That flip is the "temporary fix" the
  operator refers to.
- **Borrow reconcile** is `SearchIndex::reconcile_borrowed_snapshot_with_disk`
  (`search_index.rs:2064-2169`). It does a walk, a stat-first verify, then applies removed, stale and
  added paths to the RAM delta, and never writes `cache.bin`.

### 1.2 The trigram index (`search_index.rs`)

- **Structure.** `SearchIndex` = optional `base: Arc<BasePostings>` + `delta: Arc<DeltaState>` +
  file tables (`:346-371`). `DeltaState` holds the delta postings and a `superseded` set of base file
  ids, versioned together so that a query never mixes them (`:373-379`). The base keeps only its lookup
  table resident. Postings are read with `pread` from `cache.bin` (`:381-394`, `2374-2392`).
- **Query.** `candidates` (`:2814-2881`) intersects per-trigram matches. Base postings of superseded or
  inactive ids are skipped, delta postings are added, and the result is deduplicated
  (`is_active_file` `:2889-2897`, `postings_for_trigram` `:2899-2957`). An edit supersedes a base id at `:1446`.
- **Disk format.** `cache.bin` v4 stores, per file, a *relative* path, size, mtime and blake3
  (`:3809-3876`). Postings are 6 bytes each (`:55`), followed by a sorted lookup section
  (`:3878-3898`). The write is temp + fsync + rename (`:3655-3744`). The loader re-roots the relative
  paths onto the loading root (`:1728-1993`). So a `cache.bin` is already root-independent and
  content-described. It is a segment in all but name.
- **Compaction thresholds** exist for the delta: soft 1,000 files or 32 MiB, hard 5,000 files or
  128 MiB (`:47-50`).
- **Membership.** `update_file` does not consult ignore rules (`:1486-1526`). The watcher filter does
  that upstream. See §2 for why this matters.

### 1.3 Views as they exist (`views.enabled`, default off)

- **Manifest planes** are semantic and callgraph only (`views/mod.rs:303-322`, `328-344`).
- **Membership is the HEAD tree.** Assembly lists `git ls-tree -r HEAD` (`views/assembly.rs:178`,
  `alias/mod.rs:777-805`) and reads working-tree bytes for changed tracked paths (`assembly.rs:257-261`).
  **Untracked files are not in any view.** Grep sees them today through the walker.
- **Trigram is a placeholder.** Assembly writes an empty file (`assembly.rs:542-543`), and the closure
  probe checks only that the file exists (`:918-920`, `views/mod.rs:517-532`).
- **Generations and publication.** A generation name ends with the HEAD tree fingerprint
  (`assembly.rs:804-819`). Callgraph refuses with `Building` when it differs from HEAD
  (`context.rs:5854-5868`, `views/mod.rs:56-58`). Publication is a CAS on the pointer row
  (`views/mod.rs:602-629`, R18).
- **Callgraph blobs are root-free.** `CallgraphBlob::extract(source, language, version)` sees no path
  and no root (`assembly.rs:927-958`). The materializer takes only a database path, the blob database
  and the manifests (`views/materialization.rs:16-22`, `143-148`).
- **Incremental materialization** clones the view's *own* current generation and applies the manifest
  diff (`assembly.rs:483-538`, `materialization.rs:67-94`, `185-207`).
- **Semantic query in a view** decodes every manifest entry's blob from SQLite on every query
  (`commands/semantic_search/mod.rs:2161-2216`). The resident index instead does a brute-force scan
  over entries in RAM (`semantic_index.rs:5942-5975`).
- **Blob writes are allowed only when `allow_blob_put`.** That flag is true on the semantic-ready path
  (`runtime_drain.rs:1212`) and `!shared_artifacts_read_only()` on the other schedulers
  (`runtime_drain.rs:1916`, `2950-2954`; `commands/configure.rs:6330`). A missing callgraph blob blocks
  publication (`assembly.rs:391-426`). Semantic keys come from `store_live_semantic_blobs` only when
  puts are allowed (`context.rs:5287-5311`).
- **GC marks one view.** `gc::sweep` marks the calling view's manifest keys, its `pins/` and its
  `readers/` (`gc/mod.rs:92-112`, `119`, `163`). The only caller passes one view's manifest with a
  2 GiB family budget (`commands/configure.rs:5940-6009`). A second view's blobs in the same family are
  protected only by the 15-minute age floor (`gc/mod.rs:18`).
- **Plane-worker dedup** (`refresh/mod.rs:55-94`, R21) has no production callers.
- **Method dispatch.** The view extract sets `dispatch_hints: Vec::new()` (`join.rs:1117`) and emission
  writes resolved call edges only (`materialization.rs:931-935`). Legacy inserts dispatch edges as
  `kind='call'` with provenance `type_match`/`name_match` (`callgraph_store/mod.rs:13313-13395`,
  `13439-13450`). It infers receiver types by reading source through `project_root`
  (`callgraph_store/mod.rs:13337`), which is a live-filesystem read that R5 forbids at the join.
- **Publication cadence.** The view publication quiet window is 1 s (`runtime_drain.rs:33`,
  `2931-2954`). Superseded jobs for a root are cancelled and merged (`executor/view_publication.rs:137-144`),
  and every publication takes the cold-build limiter (`:172-180`). The semantic refresh quiet window is
  15 s (`commands/configure.rs:365-377`). The refresh worker reuses view semantic blobs by full key
  (`:845-872`).

---

## 2. Measurements (isolated storage)

**Method.** A scratch binary links `crates/aft` as a library (`agent-file-tools`, release, no LTO) and
uses only public `SearchIndex` APIs. Both sides of each pair are `git archive` exports under
`/tmp/aft-measure/data`, so no live repository and no live daemon was touched. The binary:

1. builds the base checkout's `cache.bin`;
2. cold-builds the target checkout's own `cache.bin`;
3. loads the *base* `cache.bin` re-rooted onto the target;
4. applies the diff through `update_file`, which is the view overlay;
5. compares grep file sets for about 300 identifiers taken from the changed files of both sides, plus
   common words, between the view, the cold target build and the re-rooted base alone.

The host load average was 8.6–11 throughout, so treat the times as upper bounds.

| pair (base → target) | files | base `cache.bin` bytes | postings | lookup trigrams | per-file blob model¹ | cold build ms | base load ms | D applied | apply ms | delta RAM est.² | view≠cold | base-only≠cold |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| aft `HEAD` → `HEAD~3` | 3,941 → 3,935 | 29,439,468 | 4,575,777 | 95,110 | 27.8 MB | 815–841 | 62 | 16 | 47 | 0.69 MB | **0**/304 | 0/304 |
| aft `HEAD` → `HEAD~50` | 3,941 → 3,916 | 29,439,468 | 4,575,777 | 95,110 | 27.8 MB | 1,337–2,258 | 68–106 | 90 | 214–283 | 3.25 MB | **0**/306 | 208/306 |
| opencode `5716f8ba` → `a085bf62` | 6,991 → 6,976 | 44,323,664 | 6,831,178 | 161,473 | 41.6 MB | 1,062–1,129 | 73 | 298 | 229 | 7.97 MB | **0**/306 | 6/306 |

¹ Modelled from measured per-file trigram counts: 6 B per distinct trigram plus 88 B of key, digest
and row overhead. ² `SearchIndex::estimated_memory` delta. It excludes hash-table overhead, so real
RSS is higher.

Query cost, mean per grep including disk verification (µs, view / cold / base-only):
aft `HEAD~3` 4,420 / 4,440 / 4,501; aft `HEAD~50` 7,468 / 7,500 / 2,113; opencode 5,979 / 5,902 /
6,319. Base-only is faster on aft `HEAD~50` only because it misses files: 208 of 306 answers were
wrong. Cloning a query snapshot (`SearchIndex::snapshot`, `search_index.rs:581`) cost 0.02–0.04 µs.

**Findings.**

- **M1 (measured).** Segment + overlay is exact. It answered identically to a cold build on all three
  pairs, and its query cost is within noise of the cold index.
- **M2 (measured).** A stale base without an overlay is wrong in the direction the phantom note
  predicted: false negatives on this checkout's own content, 208 of 306 probes on the 50-commit pair.
- **M3 (measured).** Membership must be part of the delta. On the first run, the harness fed every
  changed path to `update_file`. Three files listed in `benchmarks/aft-search/.aftignore` were modified
  in the diff, got indexed, and produced three false positives (`function`, `import`, `useState`).
  Filtering to paths that are members of either checkout removed all three. The live delta must apply
  exactly the membership rules of the segment build, including `.aftignore` and ignore-file changes,
  because `update_file` does not (`search_index.rs:1486-1526`). This is R25 in practice.
- **M4 (measured).** A full per-view postings file costs 29.4 MB (aft) or 44.3 MB (opencode), about
  6.3–7.5 KB per indexed file. At 40 views that is 1.2–1.8 GB. At the 273 MB umbrella-scale index
  (`umbrella-sessions.md:7`) it would be 10.9 GB.
- **M5 (measured).** A trigram cold build is cheap at these sizes (about 1–2 s). Sharing matters most
  for **disk** (M4) and for the **semantic and callgraph** planes, not for trigram build time. The
  design still gives trigram the same model, so there is one freshness contract.

Not measured here: the derived callgraph seed across views (modelled in §7 from the incremental-note
measurements) and the semantic arena (modelled in §6.1).

---

## 3. The model

### 3.1 Vocabulary

| term | what it is | keyed by | stored at |
|---|---|---|---|
| **family** | all checkouts sharing root commits (R10) | artifact key | `blobs/<family>/` |
| **blob** | immutable per-file artifact of one plane | plane key tuple (R1, R2, R16; trigram below) | `blobs/<family>/<plane>.sqlite` |
| **segment** | immutable trigram postings for a set of `(rel_path, content)` | blake3 of its file table + tokenizer version | `blobs/<family>/trigram-seg-<id>.bin` |
| **view** | one checkout's published state | `project_scope_key` (R3) | `views/<scope>/` |
| **generation** | one immutable manifest + derived callgraph db + trigram overlay record | pointer row CAS (R18) | `views/<scope>/manifest-<gen>.json`, `derived-<gen>.sqlite` |
| **live delta** | in-RAM difference between the checkout's disk and its generation | rel path | RAM only, per bound root |
| **family registry** | the views of a family, for GC and seeding | `(family, scope)` | `blobs/<family>/members.sqlite` (new) |

### 3.2 Membership (change from today)

A manifest lists the **walker's membership**: tracked and untracked files that are not ignored, using
the same walker and ignore rules as the trigram build today (`walk_project_files`, used at
`search_index.rs:2072`). This replaces the HEAD tree (`assembly.rs:178`). Otherwise, moving grep onto
views would drop untracked files, which grep sees today.

- Each `Regular` entry gains a `trigram` plane key and a `git_oid: Option` field. `git_oid` is set only
  when the bytes equal the HEAD blob, proven per R17.
- Ignore files and their fingerprint are part of the manifest closure (R25). The HEAD tree listing is
  kept for the alias fast path only.
- Symlinks and gitlinks stay manifest entry kinds (`views/mod.rs:334-339`).

### 3.3 Invariants (additions to R24's list)

- **I6 — One active source per path.** For every rel path a query sees exactly one of: a live-delta
  entry, a generation-overlay entry, or a segment entry (`is_active_file`/`superseded` today,
  `search_index.rs:2889-2957`).
- **I7 — Snapshot atomicity.** A query reads one `(generation, delta version)` pair, taken before its
  first index read, and never re-reads the pointer mid-query.
- **I8 — Honesty.** An answer that is not equal to a cold build of the current disk for the planes it
  used says so, with a count and a reason (§10).
- **I9 — Family GC completeness.** A blob, segment or derived file is deleted only after the
  references of *every* registered view of the family, plus its live pins, have been marked.

---

## 4. Trigram plane in views (question 1)

### 4.1 Per-file artifact

- **Trigram blob key:** `(blake3(bytes), trigram_extractor_version)`. There is no path component,
  because trigram postings do not embed the path, unlike semantic under R1. `trigram_extractor_version`
  covers `INDEX_VERSION` (`search_index.rs:42`), mask semantics and the binary and size rules.
  `max_file_size` is a family setting recorded in the segment header (it is already in the header,
  `:3822-3828`). A change of either creates new keys.
- **Payload:** a flag byte (`indexed | unindexed_binary | unindexed_oversize`) plus the sorted distinct
  `(trigram u32, next_mask u8, loc_mask u8)` records. This is the 6-byte posting shape (`:55`), with
  the file id implicit.
- **Size** (measured model): about 1.0–1.06× a segment covering the same files (27.8 MB vs 29.4 MB
  aft; 41.6 MB vs 44.3 MB opencode).

### 4.2 Queryable postings: shared segment + overlay + live delta (chosen)

A view's index is built exactly like today's `SearchIndex`:

- `base` = one family segment, shared by every view in the process that references it. The lookup
  table is resident once per segment and postings are read with `pread`.
- `superseded` = segment files whose rel path is absent from the manifest or whose content key differs.
- `delta` = postings of the manifest's non-segment files (the overlay) plus the live delta.

The overlay set is computed at generation open by a merge-walk of two sorted lists: the manifest and
the segment file table, both sorted by rel path. That is O(N) with no I/O beyond the manifest. The
overlay's postings come from trigram blobs (SQLite point reads) or, for the live delta, from the
watcher's own read.

**Rejected: per-view assembled postings patched from the previous generation.** Each view would write
a full `cache.bin` (M4: 29–44 MB per view) on every generation, which is write amplification per fold.
It would not reduce query cost, because M1 shows segment + overlay equals cold.

**Segment lifecycle.**

- Segments are written like `cache.bin` today (temp + fsync + rename, `search_index.rs:3655-3744`),
  under a content-derived name, from blobs rather than from files.
- A view **rebases** onto a new segment when its overlay crosses the existing delta thresholds (soft
  1,000 files or 32 MiB, `:47-50`). It either adopts the family's newest segment, if that gives a
  smaller overlay (a merge-walk comparison), or builds one from its own manifest. It publishes the
  rebase as its next generation.
- A family keeps at most **S = 4** segments. The fifth rebase in a family must adopt an existing
  segment unless its overlay would exceed the hard threshold (5,000 files or 128 MiB). S is a starting
  value to calibrate (§12).

### 4.3 Query cost versus today

The algorithm is identical: one binary search per trigram in the lookup table, a `pread` per posting
list, and a superseded check per base posting. Measured per-query view/cold: 5,979/5,902 µs
(opencode) and 7,468/7,500 µs (aft, D=90). The superseded check is a `HashSet<u32>` lookup today
(`search_index.rs:2890`, `2916`). A bitset over segment ids is a free improvement.

### 4.4 Disk and memory per view, and the bound for many views

- **Disk per view** (modelled): the manifest JSON (about 200 B per entry: 1.4 MB for 7k files) plus
  the trigram overlay record (segment id only). No postings. Family-wide: at most S segments
  (≤ 4 × 44 MB for opencode, measured size) plus the trigram blob store (≈ 1 segment, measured model).
- **Memory per resident view** (measured components):
  - A segment lookup table: 16 B × trigrams, so 1.5 MB (aft) or 2.6 MB (opencode), **shared** per
    segment.
  - File tables: 1.2–1.9 MB per view today, because `FileEntry` holds absolute paths
    (`search_index.rs:960-966`). With rel paths bound to the root at result time, the file table
    becomes per segment and shared too. Per view it then keeps an N-bit superseded bitset plus overlay
    entries.
  - The overlay and live delta: about 27–43 KB per changed file (measured: 7.97 MB / 298 opencode;
    3.25 MB / 90 aft; 0.69 MB / 16 aft), before hash-table overhead.
- **Bound for 10–40 mason views** (modelled): shared segment tables (≤ S × 4.5 MB) + Σ views
  (N/8 B + D × ~35–43 KB). For 40 views with D = 100: about 140 MB plus shared segments, and for
  D = 16: about 28 MB. Views that are not bound are not resident, and idle eviction already exists for the
  trigram index (`commands/configure.rs:3940-3948`).

---

## 5. Freshness (question 2)

### 5.1 The live delta

There is one per bound root, owned by the root actor. Every checkout gets the same treatment.

```
LiveDelta {
  base: Arc<OpenGeneration>,             // generation G this delta is relative to
  version: u64,                          // bumps on every mutation
  entries: BTreeMap<RelPath, LiveEntry>, // invariant: entry exists iff disk != G.manifest[path]
}
LiveEntry {
  disk: Present { content: blake3, size, mtime } | Absent,
  seq: u64,                               // watcher sequence that produced it
  trigram: Option<Arc<FileTrigrams>>,     // always computed on apply (unless unindexed)
  semantic: Pending | Ready(Arc<Vectors>) | Failed(reason),
  // callgraph holds no data: presence in `entries` marks the path stale for callgraph
}
```

- **What feeds it.** Watcher events go through the existing slices, which only collect paths
  (R13, `runtime_drain.rs:2597-2650`). A plane worker hashes and applies them. Rescan, overflow and
  bind run the reconcile pass, which is today's `reconcile_borrowed_snapshot_with_disk` logic
  (`search_index.rs:2064-2169`) retargeted to compare disk with **G's manifest**. Every event applies
  the membership rules of G's closure (M3).
- **What it covers.**

  | plane | treatment | lag |
  |---|---|---|
  | trigram | exact immediately | the watcher apply only (0.8 ms per file measured, including read) |
  | semantic | embedded on the checkout's budget after the quiet window, vectors put to the family store as they are produced | 15 s quiet window + embed time |
  | callgraph | stale marker only | until the next generation |
- **Removals and adds.** An edit that restores G's content removes the entry (the invariant is by
  content, not by event).

### 5.2 Query view during steady state

A query snapshot is `Arc<(OpenGeneration, LiveDelta@version)>`.

- **Trigram:** the resolution order per path is live delta → overlay → segment (I6).
- **Semantic:**
  1. Score the arena vectors of G's manifest keys minus the live-delta paths.
  2. Add the `Ready` live vectors.
  3. Disclose the `Pending` and `Failed` paths (§10).
- **Callgraph:** answer from G. If the live delta contains callgraph-language paths that the answer
  touches, add a stale-files disclosure (§10).

### 5.3 Fold into a new generation

- **Triggers:**
  - the view publication quiet window elapses (1 s, `runtime_drain.rs:33`) with callgraph-relevant
    entries present;
  - HEAD moves (commit, checkout, rebase);
  - a semantic fill completes (as today, `runtime_drain.rs:1194-1215`);
  - the overlay crosses a segment threshold (§4.2), which becomes a rebase generation;
  - idle eviction or shutdown are **not** triggers, because the delta rebuilds from disk at bind.

  Only one publication runs per view at a time, and superseded jobs are merged
  (`executor/view_publication.rs:137-144`).
- **Content.** Generation G+1's manifest = G's manifest patched with the live entries observed at fold
  start. The fold records the watcher `seq` cut. Blobs are put before publication (R8, R20).
- **Rebase at switch.** The actor builds the successor snapshot off-lock:
  - open G+1: its segment (usually already resident) and overlay;
  - the new live delta = every entry of the old live delta whose content differs from G+1's manifest
    entry. That includes edits with `seq` after the cut and edits the fold missed. Entries equal to
    G+1 drop out;
  - carry trigram postings and `Ready` semantic vectors over by `Arc` clone. Nothing is recomputed.

  Then, under the root's index write lock, it swaps the `Arc`. Watcher applies are serialized on the
  same actor. An event after the swap applies to the new delta. An event during the off-lock build is
  replayed onto the successor before the swap (today's pending-paths replay,
  `runtime_drain.rs:2637-2644`).

### 5.4 What a query sees during a switch

- A query that started before the swap keeps the old `(G, delta)` snapshot, held by `Arc` and a
  `QueryPin` on G (`pins/mod.rs:175-180`). The old segment file descriptor stays valid even if the
  segment is swept (on POSIX an open file survives unlinking; on Windows the sweep retries, §12).
- A query after the swap sees `(G+1, rebased delta)`.
- **No gap:** the successor is complete before the swap.
- **No double count:** I6 holds in both snapshots. Each path is resolved from exactly one source, and
  the final `dedup` exists as a second guard (`search_index.rs:2952-2955`).
- The equivalence harness (§2) is the conformance test for "delta over G equals cold(disk)". Slice T3
  extends it to a switch sequence.

### 5.5 Lag each plane tolerates, stated

| plane | freshness contract | typical lag (modelled) | while lagging |
|---|---|---|---|
| trigram (grep, glob, lexical lanes, path lookup) | exact after each watcher apply | ≤ 1 watcher slice | never stale |
| semantic | changed paths excluded from stale vectors immediately; new vectors after fill | 15 s + D × chunks / 199 chunks/s local (`semantic-embed-rss-2026-09.md:128`) | `pending_paths` disclosed; lexical lanes cover them |
| callgraph | generation-exact | 1 s + publication: ≈1.2 s fixed + 15–20 ms per changed entry (views-incremental §6.4, from a measured 5.4 s / 285 entries) | stale-file disclosure on answers that touch or may miss changed files |
| dead code / Tier-2 | generation-exact | as callgraph | `projection=none reason=view_pending` as today (perf hunt, line 85) |

---

## 6. Semantic fill and callgraph per view (question 4)

### 6.1 Semantic

- **Vector arena (new).** One per family per process:
  `HashMap<SemanticFullKey, Arc<ChunkVectors>>`, loaded lazily from `semantic.sqlite` and shared by
  every resident view of the family. A view search scans the arena entries named by its manifest,
  minus live-delta paths, plus live vectors. It is the same brute-force scan the resident index does
  today (`semantic_index.rs:5942-5975`), without SQLite decoding on the query path
  (`semantic_search/mod.rs:2161-2216` today).
  - Memory (modelled): about 2.6 KB per chunk, **once per family** instead of once per checkout
    (`borrowed-semantic-callgraph.md:146-148`).
  - It replaces `SemanticIndex.shared_base` and its `materialize_shared_base` trap, which copies the
    whole base on the first invalidation (`semantic_index.rs:4449-4460`, `6160-6164`).
- **Fill (slice 2 of the borrowed design).** The checkout's semantic refresh worker processes the live
  delta's `Pending` entries. It first looks up existing blobs by full key, as it already does
  (`commands/configure.rs:845-872`), and embeds only misses on this checkout's model budget. Admission
  is per `(family, semantic)` breaker (R21), with in-process dedup by full key, so two views that
  embed the same `(content, path)` pay once. This wires `refresh::deduplicate_full_keys`
  (`refresh/mod.rs:55-94`), which is unused today. Vectors are put to the family store and set
  `Ready` in the live entry. The next generation carries the key.
- **Pending does not block publication.** The key stays `None` and the path is listed as pending
  (`assembly.rs:321-337` today).
- **Keys.** `store_live_semantic_blobs`, which serializes the whole resident index
  (`migration/mod.rs:273`, perf hunt line 117), is no longer on the publication path. Keys come from
  the fill.

### 6.2 Callgraph per view

- **Blob writes.** Every first-party bind may put blobs (R12). `allow_blob_put` disappears
  (`assembly.rs:35`, `375-398`), so "shared callgraph blob unavailable" (`assembly.rs:410`) becomes
  unreachable except for deterministic extraction failure. Such a key is quarantined (R21) and the path
  is reported failed. It never blocks the other paths' publication.
- **Derived rows stay per view** (`derived-<gen>.sqlite`), incrementally materialized as today, with
  the incremental note's P1 (one-step backup) and P3 (fallbacks) as prerequisites.

### 6.3 Seeding a new view's derived callgraph from another view's generation (slice 3)

**Root independence, argued from the code:**

1. **Blob payloads.** `CallgraphBlob::extract` receives only bytes, language and producer version
   (`assembly.rs:944-957`). The key has no path (R2, `blob_store/mod.rs:232-236`).
2. **The materializer.** `materialize(database_path, callgraph_blob_database, manifest, base)` has no
   root parameter (`materialization.rs:143-148`). Node and ref IDs are `view:{rel_path}:…`
   (views-incremental §1.1). Configuration is read as manifest facts (§1.2 item 4).
3. **Metadata.** `meta` holds the schema version, schema fingerprint, manifest fingerprint and
   materialization version (`callgraph_store/mod.rs:8467-8480`, `materialization.rs:983-990`), and no
   root.
4. **Binding happens at read.** `open_published_callgraph(project_root, …)` binds the root when it
   opens (`context.rs:5869-5875`).

**Proof obligation (test, slice C2):** materialize the same manifest in two scopes whose roots differ,
compare `L(db)` by the comparator in views-incremental §3.1, then compare
`inc(clone(derived_A), M_A → M_B)` with `cold(M_B)`. It must be equal on the generated sequences of
views-incremental §3.3. A `NON-VACUITY BREAK` that writes the root into one `meta` row must turn it red.

**Seeding protocol:**

1. Pick a seed from the family registry: the view whose manifest has the smallest symmetric
   difference from the new checkout (§7).
2. `QueryPin::acquire(seed_view_dir, seed_gen)`, then **re-read the seed's pointer and generation
   files after pinning**. This closes the pin-after-read window that `prepare_checkout` also has
   (`assembly.rs:166-171`).
3. Clone the seed's `derived-<gen>.sqlite` through the backup API into
   `views/<new>/derived-<1>.sqlite` (`generation.rs:386-429`, one step per P1).
4. `apply_manifest_diff(clone, seed_manifest, new_manifest)` (`materialization.rs:67-80`). The
   fingerprint check (`:189-192`) verifies that the clone holds `seed_manifest`. On any P3 fallback
   condition, materialize cold.
5. Release the seed pin after the new generation publishes.

### 6.4 Method-dispatch edges (required before views become the default)

- **Extraction.** The callgraph blob keeps the dispatch inputs that legacy stores: method-call refs
  with receiver shape, `dispatch_hints` and type-ref names (`callgraph_store/mod.rs:2814`). Today
  `join.rs:1117` drops them. This is an extractor-version bump, so blobs re-extract once.
- **Join.** After call resolution, run the legacy selection (`insert_method_dispatch_edges`,
  `callgraph_store/mod.rs:13313-13395`) over view refs and nodes. Receiver-type inference reads
  declaration files through the **manifest blob reader**, never `project_root`, which replaces the
  live read at `:13337`. Edges are emitted with legacy semantics (`kind='call'`, provenance
  `type_match`/`name_match`, `:13439-13450`).
- **Incremental invalidation** uses the pseudo-domain `\0view:dispatch:<method>` (views-incremental
  §1.2). Its seeds are files whose method set or receiver types changed.
- **Parity gate.** On the soak corpus (opencode) and this repository, views-on `callers`, `impact` and
  the dead-code projection must equal legacy for dispatch-only callers. Without this gate, methods
  reached only by dispatch show as dead in views, a false positive that legacy does not produce.

---

## 7. First load of a new checkout (question 5)

A mason worktree created from main binds with no generation of its own.

1. **Family and scope** come from the memoized artifact key and `project_scope_key`.
2. **Seed selection.** Read `members.sqlite`. For each candidate view, compare its recorded HEAD tree
   fingerprint and commit with this checkout's (`git ls-tree`: 48–134 ms measured, perf hunt line 60).
   - An exact match with no dirty entries on either side gives D = 0 for tracked files.
   - Otherwise: `git diff --name-status <seed_commit> HEAD`, plus `git status --porcelain`, plus the
     seed's entries whose `git_oid` is absent (the seed's dirty set, §3.2).
   - Pick the smallest D.
3. **Serve immediately.** The new checkout's snapshot is `(seed generation, live delta of D)`, and
   answers disclose "serving from a family generation, D paths differ". Trigram is exact as soon as D
   is applied. Semantic excludes D and fills it. Callgraph discloses D.
4. **Build its own generation 1:**
   - manifest = seed manifest patched with D. Clean tracked files take their keys from the alias table
     with zero reads (R17);
   - blobs for D: trigram, callgraph extraction, semantic fill;
   - derived seeded per §6.3;
   - publish (CAS from empty).

**Cost model.**

| step | cost | basis |
|---|---|---|
| seed selection | 0.05–0.15 s + git diff O(D) | measured ls-tree (perf hunt l.60) |
| trigram: attach segment | 0 if resident, else 62–106 ms | measured (§2) |
| trigram: overlay D | 47 ms (D=16), 214–283 ms (D=90), 229 ms (D=298) | measured (§2) |
| callgraph blobs for D | D × extraction; zero for content another view already extracted | not measured here |
| semantic for D | blob hits free; misses D × 8–20 chunks / 199 chunks/s local | modelled (`semantic-embed-rss-2026-09.md:71`, `128`) |
| derived: clone | 0.31 s one-step, 1.82 s production params, per 257 MB | measured (views-incremental §4.1) |
| derived: diff | ≈1.2 s + 15–20 ms per changed entry: ≈1.5 s (D=16), ≈6–7 s (D≈300) | modelled from measured 5.4 s / 285 (views-incremental §6.4) |
| closure + CAS | tens of ms (INDEXED BY membership 2–3 ms) | measured (perf hunt l.67) |
| **total, excluding embeds** | **≈2–3 s (D≈16), ≈8–10 s (D≈300)** | modelled |
| today, cold view | 24–31 s release offline (opencode); 36–44 s (Run 3) | measured (views-incremental §0) |

**Where the proportionality comes from:** every step after seed selection iterates D, except three.
The segment attach is O(1) because the segment is shared. The overlay merge-walk is O(N) in memory
with no I/O. The clone is O(size of the derived db) and is the one remaining non-D cost; the
incremental note's P2 recycling does not apply to a brand-new view.

---

## 8. What goes away, and what replaces each duty (question 3)

| removed | code | duty it served | replaced by |
|---|---|---|---|
| owner claim and `owner.json` | `artifact_owner.rs:16-62`, `96-190`, `639-651`; call at `commands/configure.rs:3320` | one writer per family artifact | nothing: shared state is blobs (idempotent puts, R7/R15) and segments (content-named, immutable); per-view publication is CAS (R18) |
| owner heartbeat thread | `artifact_owner.rs:239-253`, `305-343`, `404-423`; `context.rs:5013` | owner liveness for takeover | pin owner liveness (R19, `pins/mod.rs`) for in-flight work only |
| linked-worktree borrow and borrow note | `artifact_owner.rs:104-110`, `192-237` | stop worktrees writing shared state | R12 trust by bind class: every first-party bind may put; `mcp:*`, `fed:*` and `Unverified` binds read their own family and write nothing shared |
| borrow-only capability | `root_cache.rs:134-203`, `322-338`; `context.rs:5030-5032` | write fence for borrowers | a bind-class capability, checked in the blob store's put path |
| own-root read-only openers | `readonly_artifacts.rs:148-188`, `257-307`; own-root uses in configure (`commands/configure.rs:4195-4198`) | give a borrower an index | the checkout's own view |
| cross-root openers | `context.rs:4482-4560` via `readonly_artifacts` `_with_key` | `aft_search path:`, umbrella | **kept in role, reimplemented**: a read-only *view reader* of another scope's current generation (§9) |
| `worktree.ram_overlay` and gates | `config.rs:445-461`; `config_resolve.rs:512-520`, `1447`, `1952-1958`; `context.rs:5046-5053`; `commands/configure.rs:1215-1226`, `3940-3948`, `4032`, `4050`, `4203-4204`; `runtime_drain.rs:2124`, `2344-2346`, `2598`, `2645-2647` | private deltas for borrowers | the live delta, which every checkout has |
| borrow reconcile | `search_index.rs:2064-2169`, borrow-tolerant load `:1642` | align a borrowed snapshot with disk | bind reconcile of disk against the checkout's own manifest (same algorithm, new reference) |
| per-root `cache.bin` publish | `search_index.rs:1572` (`write_to_disk`), `runtime_drain.rs:2123-2128`, `root_cache.rs:66-97` (`ArtifactPublishEpoch`), `RootCacheDomain::Index` lease (`root_cache.rs:47-54`, `subc/standing.rs:454-485`) | publish the one family snapshot | segment writer (content-named, temp + rename) + generation CAS |
| legacy callgraph store for bound roots | `callgraph_store/mod.rs:3486-3542` (read-only open), legacy refresh and cold build | per-root callgraph | view derived db; legacy code kept only until cutover (§11) |
| `SemanticIndex.shared_base` | `semantic_index.rs:4449-4460` | borrowed vectors | family vector arena (§6.1) |
| `allow_blob_put` | `assembly.rs:35`, `375-398`; `runtime_drain.rs:1212`, `1916`, `2950-2954`; `commands/configure.rs:6330`; `context.rs:5259-5322` | borrower write fence at scheduler sites | bind-class capability (above) |

**New duties.**

- **Writer leases for shared blobs: none.** Concurrent puts of one key are one SQLite transaction
  each: `INSERT OR IGNORE` with payload digest and completeness marker (R7, R15), serialized by WAL
  and `busy_timeout` (`blob_store/mod.rs:680-704`). A payload that loses the race is identical by
  construction. Segment files use content names and `create_new` temp + rename. Two writers of the
  same segment write equal bytes, and whichever rename comes second replaces an equal file.
- **Publish epochs:** per view, the pointer CAS (`views/mod.rs:602-629`). Losers re-derive from the
  new base (R18). There is no family-level epoch.
- **Family registry** `blobs/<family>/members.sqlite(scope, view_dir, first_seen, last_publish,
  last_bind)`. A view inserts or updates its row at bind and at publish. GC and seeding read it.
- **GC with many views (closes I9):**
  - Take the family sweep lock (`fs_lock` under `blobs/<family>/sweep.lock`).
  - For each registered view: mark its current and previous manifests (every plane key and its
    segment id), its live assembly pins and its protected read markers. This is today's per-view logic
    at `gc/mod.rs:114-185`, run for every member.
  - Views whose directory is gone for two sweeps leave the registry (R9).
  - Only then sweep blobs over the budget, keeping the 15-minute age floor (`gc/mod.rs:18`).
  - Segments not referenced by any marked generation and older than the floor are deleted.
  - Derived files stay under each view's own `sweep_generations` (`views/generation.rs:95-171`).
    Cross-view seeding pins the seed generation (§6.3).

---

## 9. Umbrella sessions (question 6)

The umbrella design stays as written (`umbrella-sessions.md:9-15`): the parent builds nothing, and
child repositories are served from their own indexes. Under views:

- **Child index = child's current generation**, opened by the read-only **view reader**. The reader:
  - resolves the child's scope and family;
  - pins its current generation with a read marker (`root_cache.rs:1-17`), which protects it from the
    child's sweep;
  - never puts blobs, never writes the child's view, and never opens a live delta.

  "Opening the parent never builds or repairs a child's index" holds.
- **Fresh / Stale / Absent** (`umbrella-sessions.md:28-33`):
  - *Absent*: no view directory for the child's scope.
  - *Fresh*: every manifest entry's `(size, mtime)` still matches disk, checked by a bounded stat walk
    that stops at the first mismatch.
  - *Stale*: otherwise. The staleness age is `now - generation publish time`, which is in the
    generation name (`assembly.rs:815-817`).

  A child that is bound in its own session stays fresh because that session folds its live delta.
  The umbrella reader simply follows the child's pointer.
- **Trigram correction for stale children** (`umbrella-sessions.md:44`). Files with mtime after the
  publish time are searched directly and merged, with the cap and partial-coverage flag as designed.
  This is a bounded, read-only live delta computed per query.
- **Semantic from a stale child:** served with its age, as designed.
- **Callgraph:** the per-query mtime check on the queried file (`umbrella-sessions.md:54`), extended
  to the answer's files (§10).
- **Merge rules** stay as in v3. The view reader returns the family's model fingerprint, so
  "same embedding model" becomes a key comparison.

---

## 10. Honest reporting (question 7)

Every index-backed response (grep, glob, `aft_search` lanes, callgraph tools, inspect) carries one
block:

```
freshness: {
  generation, served_from: own | family_seed | foreign_view,
  live_delta_paths: n,
  trigram: exact,                              // by construction for own views
  semantic: exact | pending(n) | failed(n) | stale_child(age),
  callgraph: exact | stale(n, files[..k]) | building,
  complete: bool, reason: "…"
}
```

**Rules.**

- `complete` is true only if every plane the answer used is `exact`.
- **Semantic answers:** `pending(n)` with `n = |live-delta paths with semantic Pending|` whenever n > 0.
- **Callgraph answers:** `stale(n)` whenever the live delta holds callgraph-language paths that the
  answer touches. A callers or impact query touches them if any live path could hold callers, so the
  test is any callgraph-language path in the delta. A call-tree query touches them if any node on the
  path is in the delta. `files[..k]` names up to k of them.
- **Seed and foreign views** always disclose `served_from` and D.
- **`building`** is reserved for "no generation at all". The current refusal on a HEAD mismatch
  (`context.rs:5861-5867`) becomes a served answer with `stale`, because the live delta now names what
  differs.
- **Existing plumbing** carries the counts: `ViewRuntimeSnapshot.pending_paths` (`context.rs:5347`)
  and `PathStatusStore` reasons (`assembly.rs:403-417`).
- **Tests:** the rows of the borrowed-semantic probe table (`borrowed-semantic-callgraph.md` §1)
  become tests asserting "no false positive, disclosed false negative". The phantom-path tests (phantom
  note §1) run unchanged against the new model.

---

## 11. Migration and cutover (question 8)

| legacy artifact | location | becomes | one-time cost |
|---|---|---|---|
| trigram `cache.bin` | `index/<key>/cache.bin` | **the family's first segment**: hard-link or copy into `blobs/<family>/trigram-seg-<id>.bin` (the format already has relative paths and blake3, `search_index.rs:3809-3876`); trigram blobs derived lazily when a file is first needed in an overlay | O(1) link; the segment id needs one pass over the file table (measured load 62–106 ms) |
| semantic `semantic.bin` | `semantic/<key>/semantic.bin` | imported on the same root into `semantic.sqlite` (R14, `migration/mod.rs:348`, lossless per R1) | one serialization pass; zero embeds |
| legacy callgraph | `callgraph/<key>/…` | not imported: every file re-extracts once to blobs, then a cold view materialization | 24–44 s materialization per first view (measured) plus extraction, **once per family**; later views seed (§6.3) |
| `owner.json` | `artifact-owners/<key>/` | deleted at first bind of the new version | O(1) |
| views generations (opt-in users) | `views/<scope>/` | kept; manifests gain the trigram key and membership in a `path_identity_version`-style manifest version bump, so old manifests re-assemble once | one assembly per view |
| `worktree.ram_overlay` key | user and project config | accepted and ignored with a one-time deprecation warning for one release, then an unknown-key error per the feature-config migration policy | none |

**Cutover sequence.**

1. New version binds a family's first checkout.
2. Import the trigram segment and semantic vectors.
3. Start callgraph extraction.
4. Publish generation 1 without derived rows, served with `callgraph: building`, as the legacy cold
   build does today.
5. Publish derived rows when ready.
6. Legacy directories are retained until the family's first complete generation publishes (R14), then
   removed by the existing storage sweeps. Rollback before that point = delete `views/` and `blobs/`.

**There is no coexistence mode.** One release carries a kill switch, `views.enabled=false`, which
restores the legacy code path. The legacy code is deleted in the release after, which is slice X2.

---

## 12. Risks and open questions (question 9)

### 12.1 Concurrency and crash windows

1. **GC with many views (existing bug, measured by reading).** Today a sweep from view A can delete
   blobs referenced only by view B once they are older than 15 minutes and the family is over 2 GiB
   (`gc/mod.rs:92-112`, `commands/configure.rs:5996-6006`). Views are opt-in and rarely numerous now;
   the new default makes this a correctness bug. The fix is slice G1, and it must land before
   borrowers' views go default.
2. **Seed pin race.** Reading the seed pointer then pinning leaves a window in which the seed publishes
   and sweeps. Mitigation: pin, then re-verify (§6.3). The same pattern exists at
   `assembly.rs:166-171` and should be fixed at the same time.
3. **Generation switch versus watcher.** Covered by actor serialization and the pending-path replay
   (§5.3). **Test:** inject events between the off-lock build and the swap, and assert I6 and
   equivalence with a cold build after every step.
4. **Crash with an unpublished live delta.** Nothing durable is lost: the delta is rebuilt at bind by
   reconcile. Blobs already put become orphans until GC. Crash windows during publication are as in
   views-incremental §2.2, and seeding adds one: crash after the clone but before the diff commits
   leaves an unpublished file, reclaimed by the new view's sweep.
5. **Segment write crash.** A temp file is left behind and swept (the analogue of
   `sweep_stale_search_build_dirs`, `search_index.rs:3662`). The rename is atomic, and a segment is
   referenced only after it is durable (R20 order: segment fsync → manifest → pointer).
6. **Windows open-file deletion.** A swept segment that is still open by a query cannot be unlinked.
   The sweep must treat that as protected and retry.
7. **Many publications at once.** Every publication takes the cold-build limiter
   (`executor/view_publication.rs:176`). Forty masons editing at once queue for it. Trigram stays
   exact meanwhile, but callgraph lag grows to about 40 × (1.5–7 s) in the worst case (modelled).
   **Open question:** a per-family publication budget, or a dedicated view-publication limiter.
8. **Dirty-file race at fold.** Hash-then-stat (spec item from the views design): a file edited during
   the fold read is caught because the live entry's content still differs from G+1's entry and stays
   in the delta (§5.3).
9. **Two daemons, one checkout.** Both hold live deltas and both may publish. CAS serializes, and the
   loser re-derives from the winner's generation (R18). No lease is needed.

### 12.2 Resource risks

10. **Derived database disk per view (the largest one).** 257–428 MB per view at opencode scale
    (measured: Run 3 269,889,536 bytes; MEASUREMENTS §Final run 424–428 MB). At 40 views that is
    10–17 GB (modelled). Options, **decision requested**:
    - (a) evict the derived db of views not bound for more than T hours, since it can be re-derived
      from manifest + blobs (R24) and re-seeded on the next bind (§6.3);
    - (b) share one derived file across views whose callgraph-plane manifests are identical, which is
      common for fresh masons before their first source edit;
    - (c) lazy navigation reads without a materialized db, benchmarked provisionally in
      `views-lazy-read-benchmark-2026-09.md`.

    Recommendation: (a) now, (b) as a follow-up, (c) as research.
11. **Clone cost remains O(derived size)** on the first load, measured at 0.31–1.82 s per 257 MB. It is
    the one non-D term in §7.
12. **Segment count S = 4** is a starting value (modelled). Calibrate it with a family of 10–40 mason
    views on this repository and measure the overlay distribution.
13. **Semantic arena memory** is shared per family, but a family with several embedding models (after
    a model change) holds both until the old keys age out. Mitigation: arena entries are loaded
    lazily and only for keys named by resident views.

### 12.3 Open questions

14. **Membership change (§3.2).** Moving from the HEAD tree to walker membership puts untracked files
    in the callgraph and semantic planes. The legacy walker-based builds include them, but views-today
    do not. Confirm this is wanted for callgraph. The alternative is walker membership for trigram and
    semantic, and tracked-only callgraph entries.
15. **Callgraph stale disclosure granularity (§10).** "Any callgraph-language path in the live delta"
    is conservative and may mark most answers incomplete while an agent edits. The alternative is a
    reverse-dependency check through `file_dependencies`, which is more precise and costs one indexed
    query per answer.
16. **Seed selection with no git** (`git` unusable, `alias/mod.rs:781-786`). Fall back to a full
    content walk, which costs O(N) hashing.

**Estimate provenance recap.** Measured: every §2 number; clone and diff times (views-incremental);
ls-tree and closure (perf hunt); cold materialization (branch drill, views-incremental). Modelled:
per-view memory bounds, first-load totals, derived disk at 40 views, fold lag, S.

---

## 13. Slice plan (question 10)

Dependency order. Each slice lists its verification. `NB` marks a slice that needs a `NON-VACUITY
BREAK` mutation proof, because its failure would otherwise be silent.

| # | slice | depends on | verification |
|---|---|---|---|
| **G1** | family registry + GC marks every member view (I9); pin-then-verify at `assembly.rs:166-171` | — | test: two views, the blob referenced only by B survives a sweep from A over budget. **NB:** mark only the caller's view → that test red |
| **T1** | trigram blobs + segments: key, payload, segment writer from blobs, segment id; import `cache.bin` as a segment | — | segment built from blobs == `cache.bin` built from files (byte-equal postings and lookup); import test on a v4 `cache.bin` |
| **T2** | manifest v2: trigram plane key, walker membership, ignore closure, `git_oid` | T1 | assembly tests for untracked, ignored, `.aftignore` change and symlink; manifest round-trip; old manifest re-assembles |
| **T3** | view trigram index = segment + overlay + live delta; switch rebase; bind reconcile against manifest | T1, T2 | the §2 equivalence harness as a test on fixtures and generated sequences (edit, add, remove, ignore-rule change, switch mid-sequence); I6 assertion. **NB:** skip superseding on overlay → equivalence red; skip membership filter → the M3 false-positive test red |
| **S1** | semantic vector arena; view semantic search on the arena | — | arena search == today's `view_semantic_search` results on fixtures; per-query SQLite reads = 0 (counter) |
| **S2** | semantic fill of live-delta pending paths on the checkout budget; wire `refresh::deduplicate_full_keys`; one bind-class put policy (drop `allow_blob_put`) | S1, G1 | borrowed-semantic probe rows: "calibrate flux walrus" found by semantic in the worktree; embed count = misses only; two views embedding one key → 1 model call |
| **C1** | method-dispatch edges in views (extract + join + `\0view:dispatch:` invalidation), manifest-blob reads only | — | dispatch parity vs legacy on opencode and aft (callers, impact, dead code); incremental == cold for dispatch-changing sequences. **NB:** drop dispatch seeds → the incremental-dispatch test red |
| **C2** | root independence proof + cross-view derived seed | incremental-note P1, P3 | two-root `L(db)` equality; `inc(seed) == cold` on generated sequences. **NB:** root written into `meta` → equality red |
| **F1** | first load: seed selection, serve from seed + live delta, own generation 1 | T3, S2, C2, G1 | mason worktree 1 and 50 commits behind on this repository: time to own generation within the §7 model (±50%); answers disclose `family_seed` until then |
| **H1** | freshness block in every index-backed response; `complete` rules | T3, S2 | probe-table rows as tests (no false positive, disclosed false negative); phantom-path tests green |
| **U1** | read-only view reader; `aft_search path:` and umbrella use it | T3, H1 | cross-root search tests; umbrella Fresh/Stale/Absent fixtures; the reader performs zero writes outside read markers (filesystem audit in test) |
| **X1** | **cutover**: views default on for every checkout; delete `worktree.ram_overlay` gates and default (the temporary fix is removed here), owner/borrower (`artifact_owner.rs`, borrow-only `ArtifactAccess`, own-root openers, borrow reconcile), per-root `cache.bin` publish; `views.enabled=false` kept one release as kill switch; migration per §11 | all above, and a soak of F1+H1 on the fleet | soak drill (branch-drill harness) views-on only: correctness defects 0, no `legacy_callgraph_refresh`; phantom and borrowed probes green on default config; config deprecation warning test |
| **X2** | delete the legacy code paths (legacy callgraph store for bound roots, `SemanticIndex.shared_base`, `views.enabled` key) | X1 + one release | build + full test suite; `aft_inspect` dead-code shows no orphaned legacy symbols |

`worktree.ram_overlay`'s default and gates are removed in **X1**, in the same slice that makes views
the default, so there is never a release in which a borrower has neither the overlay nor a live delta.

---

## Appendix: reproducing §2

1. Scratch crate outside the repository: `aft = { package = "agent-file-tools", path = "<worktree>/crates/aft" }`,
   release profile `opt-level=3, lto=false, codegen-units=16`, with its own `CARGO_TARGET_DIR`.
2. Exports: `git archive <rev> | tar -x -C <dir>`, for this repository's `HEAD`, `HEAD~3` and `HEAD~50`
   and opencode's `5716f8ba60e7` and `a085bf62a459`. Diffs come from
   `git diff --name-status --no-renames <base> <target>`.
3. Per pair:
   - `configure_artifact_access(root, key, false)` for both roots;
   - `SearchIndex::build_with_limit_to_cache_dir(base, 1 MiB, base_cache)` + `write_to_disk`;
   - the same for the target;
   - `SearchIndex::read_from_disk(base_cache, target_root)` as the view base;
   - `update_file(target_root/rel)` for each diff path that is a member of either build (the
     membership filter of M3);
   - `grep(word, true, [], [], target_root, 1e6)` file sets compared between view, cold target and
     base-only;
   - `estimated_memory()` before and after the delta.
4. Word lists: up to 300 identifiers `[A-Za-z_][A-Za-z0-9_]{5,30}` from both versions of the changed
   files (seeded shuffle), plus a few common words.
