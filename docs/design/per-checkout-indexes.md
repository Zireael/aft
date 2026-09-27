# Per-checkout indexes through views, trigram included (end-state design, 2026-09-27)

Status: design only, **round 2**. No product code changed. Round 1 (commit `bea20611f`) went to an
Athena panel (consult `ct_00000000-0000-4001-98d9-323b501af348`). The panel kept the architecture
and found ten blocking specification gaps. §14 closes each one, and the body sections now point
there wherever their text changed. §15 lists the three decisions the operator still has to make.

Base commit `573f06fcd`. Every claim about today's code cites `file:line` at that commit. Round 1
cited `015fd232c`. Since then, only `search_index.rs` has moved in the regions cited here: its
lines after 992 are 4–9 higher, and the citations below are updated. The other cited files are
unchanged in the cited regions. Every number says whether it was **measured** (isolated storage,
never the live daemon) or **modelled** (derived from cited measurements plus stated assumptions).

Builds on: `content-addressed-index-views.md` (rulings R1–R25, cited as "R*n*"),
`views-incremental-materialization.md`, `borrowed-semantic-callgraph.md`,
`investigations/borrowed-index-phantom-paths-2026-09.md`,
`investigations/views-soak-2026-09/branch-drill.md`, `umbrella-sessions.md`. The round-2
resolutions are cited by section number, §14.1–§14.10, so they do not collide with the rulings.

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
   base/delta/superseded structure `search_index.rs` already runs (`search_index.rs:347-394`,
   `2823-2966`). Views do not assemble postings of their own on disk. Measured on this repository and on
   opencode, a view built this way answers exactly like a cold build of that checkout (0 mismatches in
   916 grep probes). Query time was within 1–2% of the cold build. Per-view postings would cost a full
   29–44 MB `cache.bin` per view.
2. **Freshness.** Every checkout has one live delta over its published generation. Readers take one
   immutable snapshot `(generation, delta version, dirty-intent version)` per query. AFT's own writes
   are visible to the next query, because the write tools record dirty intent before they acknowledge
   the write, and queries scan intent paths directly (§14.3). Watcher events are applied to trigram
   as they are drained. When the watcher cannot vouch for disk, the index is not used for pruning and
   answers come from a direct scan (§14.3, §14.4). Semantic is filled after the 15 s quiet window.
   Callgraph is folded into a generation after the 1 s quiet window plus an incremental
   materialization. Pending and failed work is tracked per plane, independently of whether disk
   differs from the generation (§14.2). A generation switch derives the successor's delta from the old
   delta and the generation diff. It then replays the mutations made since, and swaps a fully built
   snapshot (§14.1). So a query never sees a gap and never counts a path twice.
3. **What goes away:** `artifact_owner.rs`, `owner.json` and its heartbeat, borrow-only `ArtifactAccess`,
   the own-root read-only openers, `ram_overlay` and its seven gates, borrow reconcile, the per-root
   `cache.bin` publish epoch and lease. Blob puts need no lease. A put is one IMMEDIATE SQLite
   transaction and is idempotent (`blob_store/mod.rs:533-575`). Publication is a per-view CAS. The
   root actor's content-generation and lifecycle checks stay as they are
   (`executor/view_publication.rs:131`, `165-170`, `186-204`). GC must protect the references of
   **every** view in the family, concurrently with publishers. Today it marks only one view
   (`gc/mod.rs:92-112`, `commands/configure.rs:5940-6009`). §14.5 specifies the family GC epoch
   protocol that replaces it.
4. **Semantic and callgraph per view.** A shared in-process vector arena replaces today's view query,
   which scans SQLite per query. A semantic fill embeds a checkout's pending work on that checkout's
   budget. A new view may seed its derived callgraph from another view's generation. The code supports
   root independence of derived rows (§6.3) but does not prove it, so seeding stays off until the C2
   proof passes on the final materializer (§14.7). Views must gain method-dispatch edges before they
   become the default (`callgraph_store/join.rs:1117`).
5. **First load** of a mason worktree adopts the closest family generation and pays for its diff D:
   modelled at 2–3 s plus embeds for D≈16, and 7–10 s for D≈300. A cold view costs 24–44 s.
6. **Umbrella sessions** read each child repository's current generation through a read-only view
   reader. There is no live delta for child repos. Staleness is found by content and disclosed.
7. **Honest reporting** uses one freshness block per index-backed answer, with non-exact states for
   every plane. `complete: true` only when the planes used are exact as of the snapshot, or were read
   directly from disk (§14.6).
8. **Cutover** is clean: one runtime model, and no `views.enabled=false` escape hatch. Rollback is
   offline: install the previous binary, which still finds its retained legacy caches. Import runs
   through a durable, idempotent state machine and has its own crash and concurrency gate before the
   default flips (§14.8, §14.9).
9. **Resources** have soft targets and hard limits for payload, physical disk, resident memory and
   build peaks. Admission control applies at the hard limit, and nothing referenced or pinned is
   deleted to meet a limit (§14.10).

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
- **`worktree.ram_overlay`.** The key is declared at `config.rs:445-461`. Its default is `true` at
  `:466` since commit `3c3fc6e90`, which is in this base. That flip is the "temporary fix" the
  operator refers to. The key is resolved at `config_resolve.rs:512-520`, `1447`, `1952-1958`. The predicate is
  `ram_overlay_active()` (`context.rs:5046-5053`). Its gates are: bind reconcile
  (`commands/configure.rs:4203-4204`); the watcher RAM apply (`runtime_drain.rs:2598`, `2645-2647`);
  the semantic refresh worker (`commands/configure.rs:1215-1226`, `4032`, `4050`); idle reload
  (`:3940-3948`); and rescan retirement (`runtime_drain.rs:2344-2346`).
- **Borrow reconcile** is `SearchIndex::reconcile_borrowed_snapshot_with_disk`
  (`search_index.rs:2068-2173`). It does a walk, a verify with `VerifyStrategy::StatFirst`
  (`:2105-2108`), then applies removed, stale and added paths to the RAM delta, and never writes
  `cache.bin`. A pass that is abandoned must be discarded, because it is partial (`:2065-2067`).
  Measured cost at bind on this repository: 1.01–1.12 s to `Ready` against 0.36–0.41 s without it,
  most of it re-indexing 319 changed files; hashing 3,473 files took 30–33 ms
  (`investigations/borrowed-index-phantom-paths-2026-09.md:305-315`).

### 1.2 The trigram index (`search_index.rs`)

- **Structure.** `SearchIndex` = optional `base: Arc<BasePostings>` + `delta: Arc<DeltaState>` +
  per-view tables: `files`, `path_to_id` keyed by absolute path, `file_trigram_count`,
  `unindexed_files`, and the writer-only `delta_file_trigrams` reverse map (`:347-371`). `DeltaState`
  holds the delta postings and a `superseded` set of base file ids, versioned together so that a query
  never mixes them (`:373-379`). The base keeps only its lookup table resident. Postings are read with
  `pread` from `cache.bin` (`:381-394`, `2378-2396`).
- **Query.** `candidates` (`:2823-2890`) intersects per-trigram matches. Base postings of superseded or
  inactive ids are skipped, delta postings are added, and the result is sorted and deduplicated
  **by file id** (`is_active_file` `:2898-2906`, `postings_for_trigram` `:2908-2966`, dedup
  `:2961-2964`). Two file ids for one path would both survive that dedup. An edit supersedes a base id
  at `:1450`.
- **Disk format.** `cache.bin` v4 stores, per file, a *relative* path, size, mtime and blake3
  (`:3824-3891`). Postings are 6 bytes each (`:55`), followed by a sorted lookup section
  (`:3893-3913`). The write is temp + fsync + rename (`:3670-3759`). The loader re-roots the relative
  paths onto the loading root (`:1732-1997`). So a `cache.bin` is already root-independent and
  content-described. It is a segment in all but name.
- **Compaction thresholds** exist for the delta: soft 1,000 files or 32 MiB, hard 5,000 files or
  128 MiB (`:47-50`).
- **Membership.** `update_file` does not consult ignore rules (`:1485-1530`). The watcher filter does
  that upstream, with the current matcher (`runtime_drain.rs:2703`). See §2 for why this matters.
- **Watcher apply.** The drain's SearchIndex phase applies each path itself. It calls `update_file` or
  `remove_file` inline on the drain thread under the search index write lock
  (`runtime_drain.rs:2688-2713`). It is gated by `heavy_root_work_allowed && apply_ram_search_updates`
  (`:2596-2598`, `:2695`) and sliced by `WATCHER_DRAIN_SLICE_BUDGET` (`:2602-2631`). Round 1 described
  these slices as path collectors, which was wrong.
- **AFT's own writes** do not notify the index. `write_format_validate` writes, formats and validates,
  then returns (`edit.rs:590-683`). An agent's edit reaches the index only through the watcher.

### 1.3 Views as they exist (`views.enabled`, default off)

- **Manifest planes** are semantic and callgraph only (`views/mod.rs:303-322`, `328-344`).
- **Membership is the HEAD tree.** Assembly lists `git ls-tree -r HEAD` (`views/assembly.rs:178`,
  `alias/mod.rs:777-805`) and reads working-tree bytes for changed tracked paths (`assembly.rs:257-261`).
  **Untracked files are not in any view.** Grep sees them today through the walker. The read uses
  `?` (`assembly.rs:261`), so a tracked file deleted in the working tree fails the assembly.
- **Manifest entries carry no stat and no content hash.** A `Regular` entry holds mode, the plane keys
  and `resolution_input` (`assembly.rs:297-304`). The semantic key is taken by path from the request
  (`:300`), separately from the bytes assembly reads (`:261`). Resolution inputs are extracted with
  language `config` (`:262-268`).
- **Path status** is a per-view table of `pending`/`failed` annotations keyed by rel path only, with
  no plane column (`path_status/mod.rs:17-23`). Assembly writes the reasons "shared callgraph blob
  unavailable" and "shared semantic blob unavailable" there (`assembly.rs:403-417`).
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
- **GC marks one view, and fails open.** `gc::sweep` marks the calling view's manifest keys, its
  `pins/` and its `readers/` (`gc/mod.rs:92-112`, `119`, `163`), and sweeps only the Semantic and
  Callgraph planes (`:98`). The only caller passes one view's manifest and that view's generation keys
  (`commands/configure.rs:5949-5981`, `views/generation.rs:60-91`) with a 2 GiB family budget
  (`commands/configure.rs:5940-6009`). A second view's blobs in the same family are protected only by
  the 15-minute age floor (`gc/mod.rs:18`). That floor reads `created_at_ms` (`:199-200`, `225`), and a
  `put` of an existing key changes nothing (`ON CONFLICT DO NOTHING`, `blob_store/mod.rs:553-574`), so
  a reused old blob is never young. A malformed pin is skipped (`gc/mod.rs:132-135`), and an error
  reading `readers/` returns with nothing marked (`:163-170`). Both fail open. Generation sweeping, by
  contrast, retains files when pin metadata is unreadable (`views/generation.rs:141-149`). The delete
  is an autocommit statement with no revalidation (`gc/mod.rs:228-231`). The budget counts payload
  bytes, not file bytes (`:209-211`). No SQLite store in the crate enables `auto_vacuum`, so deleted
  pages never shrink a file.
- **Pins.** An assembly pin lists keys fixed at creation (`pins/mod.rs:90-128`). Assembly checks
  whether a callgraph payload is already stored (`assembly.rs:282-289`) before it creates the pin
  (`:345-352`). The pin lists callgraph keys only (`:339-342`). A pin is renewed only on `put` or
  `renew_if_due` (`pins/mod.rs:138-152`). A sweep reclaims it when it is expired **or** its owner is
  gone (`gc/mod.rs:143-149`), so a live owner that stalls past the 30-minute TTL (`pins/mod.rs:19`)
  loses its pin. Owner liveness is a local pid plus start-time check (`pins/mod.rs:209-213`).
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
  because `update_file` does not (`search_index.rs:1485-1530`). This is R25 in practice.
- **M4 (measured).** A full per-view postings file costs 29.4 MB (aft) or 44.3 MB (opencode), about
  6.3–7.5 KB per indexed file. At 40 views that is 1.2–1.8 GB. At the 273 MB umbrella-scale index
  (`umbrella-sessions.md:7`) it would be 10.9 GB.
- **M5 (measured).** A trigram cold build is cheap at these sizes (about 1–2 s). Sharing matters most
  for **disk** (M4) and for the **semantic and callgraph** planes, not for trigram build time. The
  design still gives trigram the same model, so there is one freshness contract.

**Scope of M1–M5.** The harness is single-threaded and compares grep *file sets* on about 300
identifiers per pair. It supports the choice of representation. It does not test switches, overflow,
concurrency, GC, other query forms, or high-divergence overlays near the hard threshold. Those have
their own verification in §14. T3 compares matched lines, not only file sets.

### 2.1 Round-2 measurements (isolated storage)

These use a second scratch crate outside the repository (`blake3` and `ignore` only, release build,
its own target directory) and the same `git archive` exports under `/tmp/aft-measure/data`. They read
the exports and write only under `/tmp/aft-measure-r2`. The host load average was 14–16, so the
times are upper bounds. The page cache was warm after the first run, and macOS offers no way to drop
it without root, so the first single-thread run is the closest to a cold read.

- **M6 (measured): a strict reconcile is cheap.** Walk (gitignore and `.aftignore` rules), then read
  and blake3-hash every walked file:

  | export | walked files | bytes | walk | stat all | hash, 1 thread (first run / warm) | hash, 8 threads |
  |---|---:|---:|---:|---:|---:|---:|
  | aft (`aft-head`) | 3,913 | 64.2 MB | 78 ms | 81 ms | 401 ms / 82–95 ms | 32–33 ms |
  | opencode (`oc-head`) | 6,997 | 128.4 MB | 85 ms | 66 ms | 756 ms / 164–192 ms | 58–59 ms |

  Eight threads matches the strict verify pool (half the cores, capped at 8,
  `cache_freshness.rs:292-302`). The phantom-path investigation measured the same order in the
  daemon: hashing 3,473 files took 30–33 ms (`investigations/borrowed-index-phantom-paths-2026-09.md:308`).
  So hashing every member costs less than the walk. The cost of a bind or overflow reconcile is
  re-indexing the files that differ, not verifying the ones that do not (§14.4).
- **M7 (measured): SQLite stores more than the payload.** Trigram blob payloads, modelled as one flag
  byte plus 6 bytes per distinct lower-cased byte trigram for text files up to 1 MiB, were written with
  the family blob-store schema (`blob_store/mod.rs:87-101`), WAL, `synchronous=FULL`, and one
  transaction per put:

  | export | rows | logical payload | file after checkpoint | peak file + WAL | put time |
  |---|---:|---:|---:|---:|---:|
  | aft | 3,526 | 27.66 MB | 33.39 MB (1.21×) | 35.35 MB (1.28×) | 0.9 s (≈0.25 ms/put) |
  | opencode | 6,542 | 41.14 MB | 54.00 MB (1.31×) | 55.30 MB (1.34×) | 1.5 s |

  The logical totals reproduce the per-file blob model of §2 (27.8 MB and 41.6 MB) within 1%. A
  budget on logical bytes, which is what the sweep counts today (`gc/mod.rs:209-211`),
  under-states disk by 21–34%. Deleted pages are never returned to the filesystem without
  `auto_vacuum`, so a store's file stays at its high-water mark (§14.10).

Not measured here: the derived callgraph seed across views (modelled in §7 from the incremental-note
measurements) and the semantic arena (modelled in §6.1).

---

## 3. The model

### 3.1 Vocabulary

| term | what it is | keyed by | stored at |
|---|---|---|---|
| **family** | all checkouts sharing root commits (R10) | artifact key | `blobs/v2/<family>/` (versioned, §14.8) |
| **blob** | immutable per-file artifact of one plane | plane key tuple (R1, R2, R16; trigram below) | `blobs/v2/<family>/<plane>.sqlite` |
| **segment** | immutable trigram postings for a set of `(rel_path, content)` | blake3 of its file table + trigram policy fingerprint | `blobs/v2/<family>/trigram-seg-<id>.bin`, with a row in the segment table |
| **view** | one checkout's published state | `project_scope_key` (R3) | `views/v2/<scope>/` |
| **generation** | one immutable manifest + derived callgraph db + trigram overlay record | pointer row CAS (R18); named by the manifest content fingerprint | `views/v2/<scope>/manifest-<gen>.json`, `derived-<gen>.sqlite` |
| **live delta** | in-RAM difference between the checkout's disk and its generation, plus a mutation journal | rel path | RAM only, per bound root |
| **dirty intent** | paths AFT has written but the live delta has not applied yet | rel path | RAM only, per bound root |
| **family registry** | the views of a family, the GC epoch and the import state | `(family, scope)` | `blobs/v2/<family>/members.sqlite` (new, through `IdentityConnection`) |

### 3.2 Membership (change from today)

A manifest lists the **walker's membership**: tracked and untracked files that are not ignored, using
the same walker and ignore rules as the trigram build today (`walk_project_files`, used at
`search_index.rs:2076`). This replaces the HEAD tree (`assembly.rs:178`). Otherwise, moving grep onto
views would drop untracked files, which grep sees today. Trigram and semantic use walker membership.
Whether callgraph does too is decision D1 (§15.1). The recommendation is yes.

- Each `Regular` entry gains `content` (blake3 of the bytes), `size`, a `trigram` plane key, a
  `git_oid: Option` field, and a per-plane state `Ready(key) | Pending(reason) | Failed(reason,
  producer)` (§14.2). Every plane key of one entry is derived from the one byte buffer whose hash is
  `content` (§14.1). `git_oid` is set only when the bytes equal the HEAD blob, proven per R17.
- **Generation identity** becomes a fingerprint of the manifest content. With untracked and dirty
  files in the manifest, two generations with the same HEAD can differ, so the HEAD tree fingerprint
  in today's names (`assembly.rs:804-819`) no longer identifies a generation. It is kept as a field
  for seed selection.
- Ignore files and their fingerprint are part of the manifest closure (R25). An ignore-file change on
  disk triggers a membership reconcile of the live delta under the **current** rules, not G's (§5.1).
  The HEAD tree listing is kept for the alias fast path only.
- Symlinks and gitlinks stay manifest entry kinds (`views/mod.rs:334-339`).

### 3.3 Invariants (additions to R24's list)

- **I6 — One active source per path.** For every rel path a query sees exactly one of: a live-delta
  entry, a generation-overlay entry, or a segment entry. The final sort and dedup in
  `postings_for_trigram` works on file ids, not paths (`search_index.rs:2961-2964`), so it is **not** a
  guard. I6 holds by construction: the snapshot's `DeltaState` has one file id per rel path, and a live
  entry supersedes the path's overlay id and segment id before the snapshot is published (§14.1). T3
  asserts I6 per rel path.
- **I7 — Snapshot atomicity.** A query reads one `(generation, delta version, intent version)`
  snapshot, taken before its first index read, and never re-reads the pointer mid-query. Every plane of
  one answer comes from that snapshot's generation: the callgraph opens the snapshot's generation, not
  whatever the on-disk pointer names at that moment.
- **I8 — Honesty.** An answer that is not exact for the planes it used, as of its snapshot, says so,
  with a state, a count and a reason (§14.6).
- **I9 — Family GC safety.** A blob, segment or derived file is deleted only if no registered view's
  current or protected generation, pin or live pin references it, and no reference was created
  concurrently with the sweep (§14.5).
- **I10 — Rebase correctness.** After a switch from G to G+1, for every path, the successor's delta
  has an entry exactly when the last observed disk state differs from G+1's entry (§14.1).
- **I11 — Readiness independent of divergence.** Pending and failed plane work survives any fold that
  makes disk equal the generation (§14.2).

---

## 4. Trigram plane in views (question 1)

### 4.1 Per-file artifact

- **Trigram blob key:** `(blake3(bytes), trigram_policy_fingerprint)`. There is no path component,
  because trigram postings do not embed the path, unlike semantic under R1. The policy fingerprint
  hashes everything that decides the payload: `INDEX_VERSION` (`search_index.rs:42`), mask
  semantics, the binary rule, and `max_file_size`, which decides `unindexed_oversize`. The segment
  header already records `max_file_size` (`:3843`). Hashing it into the key means two checkouts with
  different size limits can never store different payloads under one key. A policy change creates
  new keys.
- **Payload:** a flag byte (`indexed | unindexed_binary | unindexed_oversize`) plus the sorted distinct
  `(trigram u32, next_mask u8, loc_mask u8)` records. This is the 6-byte posting shape (`:55`), with
  the file id implicit.
- **Size** (modelled from measured counts, §2): 0.94–0.95× a segment covering the same files, in
  logical bytes (27.8 MB vs 29.4 MB aft; 41.6 MB vs 44.3 MB opencode). Round 1 inverted this ratio.
  On disk, SQLite stores 1.21–1.31× the logical bytes (M7, measured).

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

- Segments are written like `cache.bin` today (temp + fsync + rename, `search_index.rs:3670-3759`),
  under a content-derived name, from blobs rather than from files. A segment row is committed, and
  the segment is listed in the builder's live pin, before the build starts (§14.5).
- A view **rebases** onto a new segment when its overlay crosses the existing delta thresholds (soft
  1,000 files or 32 MiB, `:47-50`). It either adopts the family's newest segment, if that gives a
  smaller overlay (a merge-walk comparison), or builds one from its own manifest. It publishes the
  rebase as its next generation.
- Segment count has a target and a hard limit, with no exception (§14.10): a target of 4 and a hard
  limit of 6 per family (starting values, modelled). At the hard limit no segment is built. A view
  whose overlay then crosses the hard threshold serves trigram through the direct scan until a segment
  retires.

### 4.3 Query cost versus today

The algorithm is identical: one binary search per trigram in the lookup table, a `pread` per posting
list, and a superseded check per base posting. Measured per-query view/cold: 5,979/5,902 µs
(opencode) and 7,468/7,500 µs (aft, D=90). The superseded check is a `HashSet<u32>` lookup today
(`search_index.rs:2899`, `2925`). A bitset over segment ids is a candidate improvement, to benchmark.

### 4.4 Disk and memory per view, and the bound for many views

These are payload models for typical divergence. The limits that actually bound many views are the
resource contract of §12.2.

- **Disk per view** (modelled): the manifest JSON (about 200 B per entry: 1.4 MB for 7k files before
  the v2 fields; the content hash, size and per-plane states add roughly 100 B per entry) plus the
  trigram overlay record (segment id only). No postings. Family-wide: the live segments (at most the
  hard limit of 6 × 44 MB for opencode, measured size) plus the trigram blob store. The blob store holds
  the **union** of contents referenced by every view and retained generation, not one checkout's
  worth. For one set of contents it is 0.94–0.95× a segment logically and 1.21–1.31× that on disk (M7).
- **Memory per resident view** (measured components):
  - A segment lookup table: 16 B × trigrams, so 1.5 MB (aft) or 2.6 MB (opencode), **shared** per
    segment. `LOOKUP_ENTRY_BYTES` is 16 (`search_index.rs:54`).
  - Per-view tables: 1.2–1.9 MB per view today. `FileEntry` holds absolute paths
    (`search_index.rs:960-966`), and `path_to_id`, `file_trigram_count`, `unindexed_files` and the
    writer-only `delta_file_trigrams` are per index (`:347-371`). Sharing the file table per segment
    needs a refactor that binds rel paths to the root at result time. T3 owns that refactor. Until it
    lands, count 1.2–1.9 MB per view.
  - The overlay and live delta: about 27–43 KB per changed file (measured: 7.97 MB / 298 opencode;
    3.25 MB / 90 aft; 0.69 MB / 16 aft), before hash-table overhead. Measured in the daemon with a
    real borrower, the trigram estimate rose from 2.6 MB to 14.6 MB (432 superseded files) while RSS
    rose 23–27 MB (`investigations/borrowed-index-phantom-paths-2026-09.md:309-310`). RSS grew about
    1.9–2.3× the estimate, which also includes reconcile working memory.
- **Typical 10–40 mason views** (modelled): shared segment tables (≤ 6 × 4.5 MB) + Σ views
  (1.2–1.9 MB + D × 27–43 KB). For 40 views with D = 100: 156–248 MB before hash-table overhead;
  with D = 16: 65–104 MB. Round 1 gave 140 MB and 28 MB, which omitted the per-view tables and used
  the low end of the per-file range.
- **Worst case under the thresholds** (modelled): a view's overlay may grow to the soft threshold
  (32 MiB) before it rebases, and to the hard threshold (128 MiB) when no segment can be built. At 40
  views that is 1.25–5 GiB before overhead. The design does not allow it: the process memory budget
  (§12.2) evicts idle views and sends queries of views over the per-view cap to the direct scan.
  Views that are not bound are not resident, and idle eviction already exists for the trigram index
  (`commands/configure.rs:3940-3948`).

---

## 5. Freshness (question 2)

### 5.1 The live delta

There is one per bound root, owned by the root actor. Every checkout gets the same treatment.

```
LiveDelta {
  base: Arc<OpenGeneration>,             // generation G this delta is relative to
  epoch: u64,                            // bumps on overflow, rescan, bind and membership reconcile
  version: u64,                          // bumps on every mutation
  entries: BTreeMap<RelPath, LiveEntry>, // invariant: entry exists iff disk != G.manifest[path]
  journal: Vec<(version, RelPath, DiskState)>, // every mutation, including ones that remove an entry
  intent: BTreeMap<RelPath, u64>,        // dirty intent from AFT writes, with the intent version
  watcher: Healthy | Overflowed | Stopped | Reconciling,
}
DiskState = Present { content: blake3, size } | Absent
LiveEntry {
  disk: DiskState,
  seq: u64,                               // watcher or intent sequence that produced it
  trigram: Option<Arc<FileTrigrams>>,     // computed on apply (unless unindexed)
  // semantic and callgraph readiness live in the plane state of §5.6, not here
}
```

- **What feeds it.** Watcher events and dirty intent. Today the drain's SearchIndex phase applies
  `update_file`/`remove_file` itself, inline and under the index write lock, and the apply is gated
  off when heavy root work is not allowed (`runtime_drain.rs:2688-2713`, `2695`). In the new model the
  drain hands paths to the root's plane worker, which hashes and applies them under the same lock.
  A path that cannot be applied now is kept as intent instead of being dropped (§14.3).
- **Membership.** Every apply uses the membership rules on disk now: the current ignore matcher, as
  the watcher does today (`runtime_drain.rs:2703`), not G's closure. An event on an ignore file
  (`.gitignore`, `.aftignore`, or any file in the closure) starts a membership reconcile. That
  reconcile re-walks, adds newly included paths, and records newly excluded paths as `Absent`.
- **Reconcile.** Every trigger after which the watcher cannot vouch for disk runs the **strict**
  reconcile: overflow or rescan, any bind (first bind, after shutdown, after idle eviction), watcher
  restart, resume after suspension. It walks with the current rules and hashes every member against
  G's manifest `content` (`verify_files_strict_bounded`, `cache_freshness.rs:298-302`). It never uses
  `StatFirst`, which today's borrowed reconcile hard-codes (`search_index.rs:2105-2108`). Hashing is
  cheap (M6: 32–59 ms for 4–7k files). While it runs, `watcher` is `Reconciling`. The index is not
  used for pruning, and answers come from the direct scan (§5.2). An abandoned pass leaves nothing
  behind (`search_index.rs:2065-2067`). The epoch bumps, and a successor built against an older epoch
  is discarded (§5.3).
- **Stat table.** An in-RAM `(rel_path → size, mtime, content)` table lets steady-state events skip
  re-hashing unchanged bytes. It is trusted only while the watcher has been healthy since the table
  entry was recorded. It is never persisted and never shared.
- **What it covers.**

  | plane | treatment | lag |
  |---|---|---|
  | trigram | applied per event; AFT writes are covered at once by intent scanning | watcher delivery + apply for external writes (0.8 ms per file measured, including read); none for AFT writes |
  | semantic | work items queued per §5.6 and embedded on the checkout's budget after the quiet window | 15 s quiet window + embed time |
  | callgraph | a callgraph-relevant change marks answers stale (§10) | until the next generation |
- **Removals and adds.** An edit that restores G's content removes the entry (the invariant is by
  content, not by event). The removal is still a journal record, which the switch needs (§5.3).

### 5.2 Query view during steady state

A query snapshot is `Arc<(OpenGeneration, LiveDelta@version, intent@version)>`. The generation is
held by a **residency pin**: one read marker per resident open generation, created when the
generation is opened and released when it is evicted. A query costs an `Arc` clone (0.02–0.04 µs
measured, §2), not a marker file.

- **Trigram:** the resolution order per path is live delta → overlay → segment (I6). Before candidate
  pruning, each intent path not yet applied at the snapshot's version is removed from the candidate
  sources and matched on its current bytes directly, so a trigram an agent just wrote cannot be pruned
  away. If `watcher` is not `Healthy`, the query does not prune with the index at all. It answers
  through the fallback walk, as a borrowed index does today while it reconciles
  (`commands/configure.rs:4270-4276`), and reports `scanned`.
- **Semantic:**
  1. Score the arena vectors of G's `Ready` keys, minus the live-delta paths.
  2. Add the vectors installed for live entries (§5.6).
  3. Disclose pending and failed work from both the generation and the live delta (§10).
- **Callgraph:** answer from G. If any callgraph-relevant change exists, disclose it (§10).

### 5.3 Fold into a new generation

- **Triggers:**
  - the view publication quiet window elapses (1 s, `runtime_drain.rs:33`) with callgraph-relevant
    entries present;
  - HEAD moves (commit, checkout, rebase);
  - a semantic fill completes (as today, `runtime_drain.rs:1194-1215`);
  - the overlay crosses a segment threshold (§4.2), which becomes a rebase generation;
  - idle eviction or shutdown are **not** triggers, because the delta rebuilds from disk at bind.

  Superseded jobs are cancelled and merged (`executor/view_publication.rs:137-144`). The cancelled
  worker is asked to stop, not awaited (`:137-162`), so two workers may briefly overlap. The pointer
  CAS decides which one publishes. A job that has finished materializing is allowed to reach its CAS
  instead of being cancelled. The switch rule below makes a slightly stale generation safe to
  install, and cancelling late jobs could stop an agent that edits faster than one publication from
  ever publishing.
- **Content.** Generation G+1's manifest is G's manifest patched with the paths the fold reads. Each
  patched entry's `content`, size and every plane key come from one read of the bytes (§3.2). A
  semantic key is admitted only if it was computed for that `content`. Blobs are put before publication
  (R8, R20), under the protocol of §8.
- **Switch.** CAS first, then build the successor from the now-known G+1, then replay and swap:
  1. **Derive** the successor's delta from `delta@v`, the version the builder reads. Let
     `K = keys(delta@v) ∪ paths(diff(G.manifest, G+1.manifest))`. For each `p` in K the last observed
     disk state is `delta@v[p].disk` if the entry exists, else `G.manifest[p]` (Absent if G has none).
     The else branch is exact because of the delta invariant: no entry at v means disk equalled G.
     The successor has an entry for `p` exactly when that state differs from `G+1.manifest[p]`. A path
     outside K is equal in G and G+1 and had no entry, so disk equals G+1 there.
  2. **Carry or fetch data.** Entries kept from the old delta carry their trigram data by `Arc`. An
     entry created because disk went back to G's bytes takes G's plane keys. G is pinned for the whole
     switch, so its blobs are protected, and neither an extraction nor an embed is needed.
  3. **Build** the successor's `DeltaState` with one file id per rel path. The inverted postings map
     over overlay ∪ live entries is rebuilt, because delta postings carry per-index file ids
     (`search_index.rs:352`, `373-379`). Cost O(overlay + live postings), measured at 47–283 ms for
     D = 16–298 in §2. Round 1 said nothing was recomputed, which was wrong.
  4. **Replay and swap.** Under the root's index write lock, take every path the journal names after
     `v` and re-run step 1's rule for it against the old delta as it is now: its current entry, or G's
     entry if it has none. Data comes from that entry or from G's keys, as in step 2. Then swap the
     `Arc`. Watcher applies, intent applies and reconcile applies take the same lock, so
     nothing lands between the replay and the swap. The journal is trimmed to the oldest version an
     in-flight successor still needs.
  5. **Epoch check.** If the delta's epoch changed since `v` (overflow, rescan, membership
     reconcile), the successor is discarded and rebuilt from the post-reconcile delta.

  The rule never reads the fold's cut, so it holds for any G+1: this daemon's fold, a fold that read
  later or earlier bytes than the live entry recorded (assembly reads its own bytes,
  `assembly.rs:261`), and another daemon's CAS winner. It covers a revert to G's bytes and an
  add-then-delete anywhere between the fold's read and the swap. Round 1 filtered old entries only and
  lost both (§14.1). Today's pending-path replay (`runtime_drain.rs:2637-2644`) only runs while a
  search index load is in flight, so it is not the switch mechanism.

### 5.4 What a query sees during a switch

- A query that started before the swap keeps the old `(G, delta)` snapshot, held by `Arc` and by G's
  residency pin. The pin is released when the last snapshot of G is dropped. The old segment file
  descriptor stays valid even if the segment is swept: on POSIX an open file survives unlinking, and
  on Windows the sweep treats an open segment as protected and retries (§12).
- A query after the swap sees `(G+1, derived delta)`.
- **No gap:** the successor is complete, including the replay, before the swap.
- **No double count:** I6 holds in both snapshots by construction, one file id per rel path. The
  file-id dedup at `search_index.rs:2961-2964` is not a guard for it.
- The equivalence harness (§2) is the conformance test for "delta over G equals cold(disk)". T3
  extends it to switch sequences (§14.1).

### 5.5 Lag each plane tolerates, stated

| plane | freshness contract | typical lag (modelled) | while lagging |
|---|---|---|---|
| trigram (grep, glob, lexical lanes, path lookup) | exact as of the snapshot for AFT writes and applied watcher events; direct scan when the watcher cannot vouch | watcher delivery + one apply for external writes; batches larger than one drain slice take several slices (`runtime_drain.rs:2602-2631`) | `applying(n)` while intents wait with no direct scan; `reconciling` or `scanned` otherwise (§10) |
| semantic | changed paths excluded from stale vectors immediately; new vectors after fill | 15 s + D × chunks / 199 chunks/s local (`semantic-embed-rss-2026-09.md:128`) | `pending(n)` from generation and live delta; lexical lanes cover them |
| callgraph | generation-exact | 1 s + publication: ≈1.2 s fixed + 15–20 ms per changed entry (views-incremental §6.4, from a measured 5.4 s / 285 entries) | `stale` on every callgraph answer while a relevant change exists (§10, decision D3) |
| dead code / Tier-2 | generation-exact | as callgraph | `projection=none reason=view_pending` as today (perf hunt, line 85) |

### 5.6 Per-plane readiness

Readiness is kept per generation and per plane. It does not depend on whether disk differs from the
generation.

- **Generation plane state.** Each manifest v2 `Regular` entry holds, per plane, `Ready(key)`,
  `Pending(reason)` or `Failed(reason, producer)` (§3.2). Pending and failed entries are part of the
  published generation, so they survive folds, eviction and restart. `path_status` gains `plane` and
  `content` columns (today it has neither, `path_status/mod.rs:17-23`) and indexes these entries for
  status responses.
- **Plane readiness** of a generation: `absent` (no generation), `building` (the generation exists but
  the plane is not materialized, such as derived rows missing after migration), or
  `ready { pending: n, failed: m }`.
- **Work queue.** A per-view queue of `WorkItem { plane, rel_path, content, producer }`, where
  producer is the model or extractor fingerprint. It is filled from two sources: live entries whose
  plane is not ready, and the current generation's `Pending` entries for paths without a live entry.
  Items are deduplicated by full key across views in the process (ruling R21,
  `refresh::deduplicate_full_keys`, `refresh/mod.rs:55-94`).
- **Completion admission.** A completion carries its work item. The actor installs it only if the
  producer equals the current plane configuration, and either the live entry for the path has that
  `content`, or there is no live entry and the generation's entry has that `content`. Otherwise the
  completion is dropped. Its blob stays in the live pin until the pin is next trimmed, then becomes
  ordinary garbage (§8). An installed completion goes into a per-view fill map
  `(rel_path, content) → key`, which queries use at once and the next fold writes into the manifest,
  turning `Pending` into `Ready`.
- **Failures.** A deterministic extraction failure is quarantined (ruling R21) and recorded as
  `Failed(reason, extractor version)`. It persists across folds while content and extractor are
  unchanged, and never blocks the other paths.

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
- **Fill.** The checkout's semantic refresh worker drains the semantic work items of §5.6: live entries
  that are not ready and the generation's `Pending` entries. It first looks up existing blobs by full
  key, as it already does (`commands/configure.rs:845-872`), and embeds only misses on this checkout's
  model budget. Admission is per `(family, semantic)` breaker (ruling R21), with in-process dedup by
  full key, so two views that embed the same `(content, path)` pay once. This wires
  `refresh::deduplicate_full_keys` (`refresh/mod.rs:55-94`), which is unused today. Each key goes into
  the live pin before its put (§8). The completion is admitted per §5.6, and the next fold carries the
  key.
- **Pending does not block publication.** The entry is published as `Pending` and stays in the work
  queue after the fold. Today assembly records only a per-path pending reason
  (`assembly.rs:321-337`, `403-417`), and the pending state disappears when the path's live entry does.
- **Keys.** `store_live_semantic_blobs`, which serializes the whole resident index
  (`migration/mod.rs:273-340`, perf hunt line 117), is no longer on the publication path. Keys come
  from the fill.

### 6.2 Callgraph per view

- **Blob writes.** Every first-party bind may put blobs (R12). `allow_blob_put` disappears
  (`assembly.rs:35`, `375-398`), so "shared callgraph blob unavailable" (`assembly.rs:410`) becomes
  unreachable except for deterministic extraction failure. Such a key is quarantined (R21) and the path
  is reported failed. It never blocks the other paths' publication.
- **Derived rows stay per view** (`derived-<gen>.sqlite`), incrementally materialized as today, with
  the incremental note's P1 (one-step backup) and P3 (fallbacks) as prerequisites.

### 6.3 Seeding a new view's derived callgraph from another view's generation (slice C2)

**Root independence: what the code supports.** This is an argument, not a proof. Seeding stays off
until C2's proof passes on the final materializer (§14.7).

1. **Blob payloads.** `CallgraphBlob::extract` receives only bytes, language and producer version
   (`assembly.rs:944-957`). The key has no path (R2, `blob_store/mod.rs:232-236`).
2. **The materializer.** `materialize(database_path, callgraph_blob_database, manifest, base)` has no
   root parameter (`materialization.rs:143-148`). Node and ref IDs are `view:{rel_path}:…`
   (views-incremental §1.1). Configuration is read as manifest facts (§1.2 item 4).
3. **The join binds a synthetic root.** `JoinResult::from_manifest` binds `/`, not the checkout root
   (`callgraph_store/join.rs:1127-1137`), and the view extract zeroes freshness (`:1106-1118`). So
   bound paths in derived rows are `/<rel_path>` whatever the checkout. The join also sets
   `dispatch_hints` to an empty vector (`:1117`), so the materializer that C2 must prove is the one
   after C1, not today's.
4. **Metadata.** `meta` holds the schema version, schema fingerprint, manifest fingerprint and
   materialization version (`callgraph_store/mod.rs:8467-8480`, `materialization.rs:983-990`), and no
   root.
5. **Binding happens at read.** `open_published_callgraph(project_root, …)` binds the root when it
   opens (`context.rs:5869-5875`).

**Proof obligation (slice C2, after C1, T2 and G1):** publish the same manifest through the
production publication path from two real checkouts at different roots, with different non-member
files and parent-directory configuration. Compare every logical table by the comparator of
views-incremental §3.1, including the dispatch rows C1 adds, and a fixed set of bound query results
(callers, impact, call tree, dead code). Assert that no stored value contains either absolute root and
that stored paths are consistently rel-path or `/`-prefixed. Then compare
`inc(clone(derived_A), M_A → M_B)` with `cold(M_B)` on the generated sequences of views-incremental
§3.3, including dispatch-changing ones. `NON-VACUITY BREAK`: write the real root into a row table
(`view_bindings` or a node path) and the equality must go red. Writing it into `meta` only proves that
the comparator reads `meta`.

**Seeding protocol:**

1. Pick a seed from the family registry: the view whose manifest has the smallest symmetric
   difference from the new checkout (§7). Skip a seed whose generation has no derived rows (the
   cutover's generation 1), or whose schema, materialization or extractor version differs.
2. The new view is registered, and its assembly pin exists (§8).
   `QueryPin::acquire(seed_view_dir, seed_gen)`, then **re-read the seed's pointer and require it to
   still equal `seed_gen`**. A file-existence check is not enough, because `sweep_generations` skips
   only the current generation and protected ones (`generation.rs:95-171`). The same pin-then-verify
   fixes `prepare_checkout` (`assembly.rs:166-171`).
3. Resolve the seed's derived file through `derived_path` (`generation.rs:19-34`), which follows a
   `derived-<gen>.ref`. Clone it through the backup API into the new view's first derived file
   (`generation.rs:386-429`, one step per P1).
4. `apply_manifest_diff(clone, seed_manifest, new_manifest)` (`materialization.rs:67-80`). The
   fingerprint check (`:189-192`) verifies that the clone holds `seed_manifest`. On any P3 fallback
   condition, materialize cold.
5. Release the seed pin after the new generation publishes.

### 6.4 Method-dispatch edges (required before views become the default)

- **Extraction.** The callgraph blob keeps every input the legacy dispatch inference reads from source:
  method-call refs with receiver shape, `dispatch_hints`, type-ref names, and the method set per type
  (`callgraph_store/mod.rs:2814`). Today `join.rs:1117` drops them. This is an extractor-version bump,
  so blobs re-extract once.
- **Join, hints only.** After call resolution, run the legacy selection (`insert_method_dispatch_edges`,
  `callgraph_store/mod.rs:13313-13395`) over view refs and nodes, using only what the blobs carry. No
  generation-addressable source text exists for dirty or untracked files. So the join reads no file
  at all, through `project_root` or through any other reader. That replaces the live read at `:13337`.
  Edges are emitted with legacy semantics (`kind='call'`, provenance `type_match`/`name_match`,
  `:13439-13450`).
- **Incremental invalidation** uses the pseudo-domain `\0view:dispatch:<method>` (views-incremental
  §1.2). Its seeds are files whose method set or receiver types changed.
- **Parity gate.** On the soak corpus (opencode) and this repository, views-on `callers`, `impact` and
  the dead-code projection must equal legacy for dispatch-only callers, with the checkout root
  unreadable while the join runs. Without this gate, methods reached only by dispatch show as dead in
  views, a false positive that legacy does not produce. Cases: unchanged callers affected by a receiver
  declaration change, method additions and removals, configuration changes, and previously missing
  targets. `NON-VACUITY BREAK`: drop dispatch edges, and dead-code parity must go red. That proves the
  corpus has dispatch-only callers.

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
3. **Serve immediately.** The new checkout registers (§8), then its snapshot is
   `(seed generation, live delta of D)`. The live delta is built by the strict reconcile of §5.1 against
   the seed's manifest, not from git's view of D alone, because untracked and dirty files count too.
   Until that pass ends, `snapshot.watcher` is `reconciling` and grep answers through the direct scan
   (`scanned`). After it,
   trigram is exact. Semantic excludes D and queues it. Callgraph reports `stale` for D, or `building`
   if the seed has no derived rows. Every answer discloses `family_seed { d }` until the checkout's own
   generation 1 is installed (§10).
4. **Build its own generation 1:**
   - manifest = seed manifest patched with D. Clean tracked files take their keys from the alias table
     with zero reads (R17);
   - blobs for D: trigram, callgraph extraction, semantic fill;
   - derived seeded per §6.3 once C2 has passed, cold otherwise;
   - publish (CAS from empty).

**Cost model.**

| step | cost | basis |
|---|---|---|
| seed selection | 0.05–0.15 s + git diff O(D) | measured ls-tree (perf hunt l.60) |
| strict reconcile against the seed manifest | walk 78–85 ms + hash 32–59 ms (8 threads, warm) | measured (M6) |
| trigram: attach segment | 0 if resident, else 62–106 ms | measured (§2) |
| trigram: overlay D | 47 ms (D=16), 214–283 ms (D=90), 229 ms (D=298) | measured (§2) |
| callgraph blobs for D | D × extraction; zero for content another view already extracted | not measured here |
| semantic for D | blob hits free; misses D × 8–20 chunks / 199 chunks/s local | modelled (`semantic-embed-rss-2026-09.md:71`, `128`) |
| derived: clone | 0.31 s one-step, 1.82 s production params, per 257 MB | measured (views-incremental §4.1) |
| derived: diff | ≈1.2 s + 15–20 ms per changed entry: ≈1.5 s (D=16), ≈6–7 s (D≈300) | modelled from measured 5.4 s / 285 (views-incremental §6.4) |
| closure + CAS | tens of ms (INDEXED BY membership 2–3 ms) | measured (perf hunt l.67) |
| **total, excluding embeds and callgraph extraction** | **≈2–3 s (D≈16), ≈8–10 s (D≈300)** | modelled |
| today, cold view | 24–31 s release offline (opencode); 36–44 s (Run 3) | measured (views-incremental §0) |

**Where the proportionality comes from:** every step after seed selection iterates D, except four.
The segment attach is O(1) because the segment is shared. The overlay merge-walk is O(N) in memory
with no I/O. The strict reconcile is O(N) hashing, measured at under 0.15 s including the walk. The
clone is O(size of the derived db) and is the one remaining large non-D cost. The incremental note's
P2 recycling does not apply to a brand-new view.

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
| borrow reconcile | `search_index.rs:2068-2173`, borrow-tolerant load `:1646` | align a borrowed snapshot with disk | strict reconcile of disk against the checkout's own manifest (§5.1): same walk, `Strict` instead of `StatFirst`, new reference |
| per-root `cache.bin` publish | `search_index.rs:1576` (`write_to_disk`), `runtime_drain.rs:2123-2128`, `root_cache.rs:66-97` (`ArtifactPublishEpoch`), `RootCacheDomain::Index` lease (`root_cache.rs:47-54`, `subc/standing.rs:454-485`) | publish the one family snapshot | segment writer (content-named, temp + rename) + generation CAS |
| legacy callgraph store for bound roots | `callgraph_store/mod.rs:3486-3542` (read-only open), legacy refresh and cold build | per-root callgraph | view derived db; removed in X1 (§11) |
| `SemanticIndex.shared_base` | `semantic_index.rs:4449-4460` | borrowed vectors | family vector arena (§6.1) |
| `allow_blob_put` | `assembly.rs:35`, `375-398`; `runtime_drain.rs:1212`, `1916`, `2950-2954`; `commands/configure.rs:6330`; `context.rs:5259-5322` | borrower write fence at scheduler sites | bind-class capability (above) |

**New duties.**

- **Writer leases for shared blobs: none.** A put is one IMMEDIATE transaction that inserts with
  `ON CONFLICT(full_key) DO NOTHING` and stores a payload digest (`blob_store/mod.rs:533-575`). A
  payload that loses the race is identical by construction, because the key covers every input
  (§4.1). Segment files use content names and `create_new` temp + rename. Two writers of the same
  segment write equal bytes, and whichever rename comes second replaces an equal file. In v2 the put
  becomes put-or-touch (below).
- **Publish epochs:** per view, the pointer CAS (`views/mod.rs:602-629`). Losers re-derive from the
  new base (R18), keeping all outstanding edits in the live delta (the switch of §5.3 handles a
  foreign G+1). The root actor's own checks stay: the content generation captured at scheduling and
  re-checked before running, and the actor epoch taken around the commit
  (`executor/view_publication.rs:131`, `165-170`, `186-204`). Completions are admitted by §5.6.
  Unpublished generation names are unique per daemon. They include the pid and start time, so two
  daemons on one checkout never write the same private file.
- **Family registry** `blobs/v2/<family>/members.sqlite`, opened through `IdentityConnection`:
  `members(scope, view_dir, registered_at, last_bind, last_publish, state)`, `gc_epoch`, and the
  import table of §11. A view inserts its row in an IMMEDIATE transaction **before** it creates any pin,
  put, segment, seed pin or derived clone.

### 8.1 Family GC protocol (closes I9)

Today's sweep marks one view, fails open, and has no ordering against reference creation (§1.3). The
replacement is a family GC epoch with revalidation at deletion time.

1. **Reference epoch in every store.** Each plane database and the segment table gain a `gc_epoch`
   row and a per-row `ref_epoch`. `put` becomes put-or-touch in its IMMEDIATE transaction: an insert
   sets `ref_epoch` to the current epoch, and an existing key has its `ref_epoch` set to the current
   epoch and reports `Reused`. Adopting a segment touches its row the same way.
2. **Protect, then touch.** Every reference-creating action makes its protection durable first, then
   touches every key it relies on, including keys that are already stored. Assembly creates its pin
   before it checks for an existing payload, which moves `assembly.rs:345-352` ahead of `:282-289`.
   The pin lists every key the next manifest adds relative to its base manifest, in every plane.
   Today it lists callgraph keys only (`:339-342`). A touch that finds its key missing makes the
   caller put it again, since it still holds the bytes, or abort.
3. **Live pin.** Each view keeps `pins/live.keys`, in the assembly-pin format. It lists the keys of
   installed semantic completions not yet folded, trigram blobs of live entries, and segments under
   construction. A key is written there before its put, and the pin is trimmed after each successful
   fold.
4. **Sweep.** Take the family sweep lock (one sweeper per family). Bump `gc_epoch` to S in every
   store. Then mark, for every registered view: its current manifest, every generation protected by a
   pin or read marker in its `pins/` and `readers/` (with that view's own generation keys), its live
   pin, and each marked generation's segment and derived owner (`.ref`, `generation.rs:19-34`). Then
   delete each unmarked candidate over budget in its own IMMEDIATE transaction, as
   `DELETE … WHERE full_key = ? AND ref_epoch < S`. Delete a segment file only after its row has been
   deleted that way. After deleting, run `PRAGMA incremental_vacuum` (v2 stores are created with
   `auto_vacuum=INCREMENTAL`) so that disk actually shrinks (M7).
5. **Why it is safe.** Take any key K that some work W relies on. W made its protection durable, then
   touched K. If the touch committed after the epoch bump, K's `ref_epoch ≥ S`, so the conditional
   delete does nothing. If the delete committed first, the touch finds K missing and W puts it again.
   If the touch committed before the bump, W's protection was durable before marking began, so
   marking saw it. Registration comes before any protection, so a view registered after marking began
   can only create references that touch at epoch ≥ S. The store's SQLite write lock orders these
   events, and no wall clock is involved.
6. **Fail closed.** Any error reading a member's manifest, `pins/`, `readers/` or a pin's keys file,
   and any malformed pin, aborts the sweep with nothing deleted. Today these are skipped
   (`gc/mod.rs:132-135`, `163-170`). A member whose root no longer exists is not an error. It leaves
   the registry after two sweeps with no live pins in its view directory (ruling R9). Its view
   directory is removed then, and only then do its references stop being marked.
7. **Renewal and suspension.** Each daemon renews all its pins from a timer every
   `PIN_RENEW_INTERVAL_MS` (`pins/mod.rs:21`), not only on put, so long materializations stay pinned.
   A pin is reclaimed only when its owner is gone (`owner_is_live`, `pins/mod.rs:209-213`). Expiry
   alone no longer reclaims a pin whose owner is alive, as it does today (`gc/mod.rs:143-149`). A
   stopped owner therefore keeps its protection, and those bytes count as pinned in §12.2. Before
   its CAS a publisher checks that its pin files still exist with its own metadata and re-touches its
   pin keys. If either check fails, it aborts and retries.
8. **Derived files** stay under each view's own `sweep_generations`, which already serializes with
   publishers through the pointer transaction (`views/generation.rs:95-171`, `36-53`). Cross-view
   seeding holds a query pin on the seed generation (§6.3).
9. **One host.** `owner_is_live` is a local pid and start-time check. Storage shared by several hosts
   over a network filesystem is unsupported, and the design states that instead of guessing liveness.
10. **Windows.** A segment that is still open cannot be unlinked. The sweep treats it as protected and
    retries on the next sweep.

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
  - *Fresh by stat*: every manifest entry's `size` still matches disk and no member's mtime is later
    than the generation's publish time. A bounded stat walk checks this and stops at the first
    mismatch. The manifest carries no mtime (§3.2), so this check cannot see an edit that preserves
    size and mtime. A foreign answer is therefore never reported `exact` (§10).
  - *Stale*: otherwise. The staleness age is `now - generation publish time`. The publish time is
    recorded with the generation, because v2 names generations by content (§3.2).

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
  snapshot: { generation, delta_version, watcher: healthy | overflowed | stopped | reconciling,
              exact_as_of_seq },
  served_from: own | family_seed { d } | foreign_view { age, checked: stat },
  divergence: { live_delta_paths: n },   // disk vs published generation, not answer exactness
  trigram:   exact | applying(n) | scanned | reconciling | stale_foreign { age, corrected },
  semantic:  exact | pending(n) | failed(n) | building | absent | stale_foreign { age },
  callgraph: exact | stale { reasons, n, files[..k] } | failed(n) | building | absent
             | stale_foreign { age },
  complete: bool, reasons: ["…"]
}
```

**States.**

- **trigram.** `exact`: the index answered, the watcher is healthy, and every intent path was applied
  or scanned directly. `applying(n)`: n intent or watcher paths wait with no direct scan, for example
  when the query hit its scan cap. `scanned`: the answer came from a direct read of disk (fallback
  walk), because the watcher could not vouch for the index; `snapshot.watcher` says why.
  `reconciling`: a strict reconcile is running and the direct scan did not cover every member, for
  example because the fallback walk hit its cap. The answer is partial.
  `stale_foreign`: another scope's generation, with the stat check of §9 and the mtime correction
  (`corrected: true` when it ran).
- **semantic.** `pending(n)` and `failed(n)` count **both** the generation's deficiencies not shadowed
  by a live entry **and** live entries whose vectors are not installed (§5.6). `building`: a generation
  exists but semantic is not materialized for it. `absent`: no generation.
- **callgraph.** A change is **callgraph-relevant** if it touches a callgraph-language file, a
  resolution input (assembly extracts these with language `config`, `assembly.rs:262-268`), membership
  (an add, a remove, an ignore-file change), or a path whose callgraph plane is not ready. The change
  can be in the live delta or in the generation's deficiencies. Until an answer-specific dependency
  proof exists, **every** callgraph answer reports `stale` while any relevant change exists, whatever
  the answer touches. Round 1's "a node on the call tree is in the delta" rule was not sound: a new
  target, a configuration edit or a membership change can alter an unchanged caller's resolution
  (`views-incremental-materialization.md:60-84`). The precision of this disclosure is decision D3
  (§15.3). `failed(n)`: extraction failures in the generation or the live delta. `building`: the
  generation exists without derived rows, as after migration. `absent`: no generation.

**Rules.**

- `complete` is true only when every plane the answer used is `exact` (trigram `scanned` also counts,
  since it is a direct read of disk), `served_from` is not `foreign_view`, and `snapshot.watcher` is
  `healthy` or the trigram answer was `scanned`. Every other combination is false with a reason.
  Migration, extraction failure and reconciliation therefore cannot produce `complete: true`. A
  `family_seed` snapshot is exact for trigram once its strict reconcile has run. Its semantic and
  callgraph planes report the D paths through their own states.
- **Exact as of what.** An answer is exact as of the instant its snapshot was taken. It covers every
  write AFT acknowledged before that instant, and every external write whose watcher event was
  applied before it. `exact_as_of_seq` names the last applied watcher sequence. An external write
  whose event had not been delivered is outside the contract. A file that changes while the query
  reads it is matched on the bytes read.
- **Divergence is separate from exactness.** A non-empty live delta does not make trigram inexact,
  because the delta is applied. It does make callgraph `stale` when the change is relevant.
- **Stale callgraph answers** can hold false positives (a deleted caller still in G) as well as false
  negatives. The block says so. Callgraph probe tests assert "disclosed", not "no false positive".
- **Seed and foreign views** always disclose `served_from`. A foreign view is never `complete`.
- The current refusal on a HEAD mismatch (`context.rs:5861-5867`) becomes a served answer with
  `stale`, because the live delta now names what differs.
- **Existing plumbing** carries the counts: `ViewRuntimeSnapshot.pending_paths` (`context.rs:5347`)
  and `path_status` (`path_status/mod.rs:17-23`, gaining `plane` and `content` per §5.6).
- **Tests:** the rows of the borrowed-semantic probe table (`borrowed-semantic-callgraph.md` §1)
  become tests: "no false positive, disclosed false negative" for trigram and semantic, and
  "disclosed" for callgraph. The phantom-path tests (phantom note §1) run unchanged against the new
  model. Every non-exact state has a negative test asserting `complete: false` (§14.6).

---

## 11. Migration and cutover (question 8)

The cutover is the operator's clean cutover: one runtime model, and no second runtime path to fall
back to. X1 makes views the only runtime and removes the legacy runtime in the same release.

| legacy artifact | location | becomes | one-time cost |
|---|---|---|---|
| trigram `cache.bin` | `index/<key>/cache.bin` | **the family's first segment**, if it passes the compatibility checks below: **copied** (never hard-linked) into `blobs/v2/<family>/trigram-seg-<id>.bin`. The format already has relative paths and a blake3 per file (`search_index.rs:3824-3891`). Trigram blobs are derived lazily when a file is first needed in an overlay | one 29–44 MB copy plus one pass over the file table for the segment id (measured load 62–106 ms) |
| semantic `semantic.bin` | `semantic/<key>/semantic.bin` | imported on the same root into `semantic.sqlite` by `import_legacy_semantic` (`migration/mod.rs:342-529`) after its fingerprint check (`:895-907`); lossless per ruling R1 | one serialization pass; zero embeds |
| legacy callgraph | `callgraph/<key>/…` | not imported: every file re-extracts once to blobs, then one cold view materialization per family, using the durable `RebuildState` pattern of `rebuild_legacy_callgraph_once` (`migration/mod.rs:531-608`) | 24–44 s materialization (measured) plus extraction, **once per family** (single flight, below); later views seed once C2 has passed (§6.3) |
| `owner.json`, `artifact-owners/` | `artifact-owners/<key>/` | left untouched by the new version until the cleanup of §11.3; the new version reads only the heartbeat, to detect live old-version daemons | none |
| views generations (opt-in users) | `views/<scope>/` | not migrated: the v2 manifest re-assembles into `views/v2/<scope>/` | one assembly per view |
| `worktree.ram_overlay` and `views.enabled` keys | user and project config | accepted and ignored with a one-time deprecation warning for one release, then an unknown-key error per the feature-config migration policy | none |

### 11.1 Rollback is offline

To roll back, stop the daemons and install the previous binary. The previous binary finds its own
legacy directories (`index/`, `semantic/`, `callgraph/`, `artifact-owners/`). The new version reads
them for import but never writes, moves or deletes them during a retention window: 14 days after the
family's import completed (a starting value). The previous binary validates them as it does today:
`cache.bin` version and per-file verification, and the semantic fingerprint. It then catches up with
edits made since, through its own freshness checks. Nothing the new version wrote is read by the old
one. After the window, rollback costs the old binary a cold build of each family. There is no
"delete `views/` and `blobs/`" rollback: deleting stores that open database sets, pins and other
views depend on is not a safe live operation.

### 11.2 Old-version daemons

Masons upgrade one at a time, so old and new daemons run side by side.

- **Versioned stores.** New stores live under `blobs/v2/<family>/` and `views/v2/<scope>/`. An
  old-version daemon with `views.enabled=true` runs a single-view sweep over `blobs/<family>/`
  (`commands/configure.rs:5940-6009`). It never sees the v2 paths, so it cannot delete new-version
  blobs.
- **No shared writable state.** Old daemons keep serving from their legacy artifacts until they
  restart. The new version never deletes `owner.json` or a legacy directory while a legacy owner
  heartbeat for the family is fresh (the heartbeat thread, `artifact_owner.rs:239-253`).

### 11.3 Import state machine

The import is per family, in the `imports` table of `members.sqlite`, one row per legacy artifact:

```
absent → claimed { owner, attempt } → staged → validated → registered → done
                                  ↘ rejected { reason }
```

- **Durable and idempotent.** Every transition is one IMMEDIATE transaction. `staged` means the temp
  copy is written and fsynced. `registered` means the segment row is committed and the file has been
  renamed to its content-derived name. A retry after a crash in any state repeats at most one
  idempotent step.
- **Concurrent first bind.** The first binder claims. Other binders of the family see `claimed` with a
  live owner and do not import. They serve trigram through the direct scan (`scanned`) and callgraph as
  `building`. A claim whose owner is dead is taken over with `attempt + 1`. The same claim is the
  **single flight** for the family's first callgraph extraction and cold materialization: other views
  wait with `building`, then seed.
- **Compatibility checks before a legacy `cache.bin` becomes a segment.** The header version must
  equal the current `INDEX_VERSION` (`search_index.rs:42`). `max_file_size`, the binary rule and the
  tokenizer version must equal the family's trigram policy, which is the fingerprint in the blob key
  (§4.1). The file table must parse, with relative paths and a blake3 per file. The copied bytes must
  hash equal to the source read. Any mismatch makes the row `rejected`, and the segment is built from
  the checkout instead.
- **Strict reconciliation before exact service.** An imported segment plus the first manifest serve
  trigram through the direct scan (`scanned`, with `snapshot.watcher: reconciling`) until the strict
  pass of §5.1 completes for that
  checkout.
- **Cleanup follows verified dependencies**, not the first complete generation. A family's legacy
  directories are removed only when every import row is `done` or `rejected`, no legacy owner
  heartbeat is fresh, and the retention window has passed. Legacy SQLite sets are removed only under
  the same no-live-owner condition, as whole sets, never file by file.

### 11.4 Cutover sequence (first new-version bind of a family)

1. Register the view (§8), then claim the family's imports.
2. Import the trigram segment and semantic vectors per §11.3. Serve through the direct scan meanwhile.
3. Start callgraph extraction (single flight).
4. Publish generation 1 without derived rows: callgraph `building` (§5.6, §10).
5. Publish derived rows when ready. Other views of the family then seed or materialize.

---

## 12. Risks and open questions (question 9)

### 12.1 Concurrency and crash windows

1. **GC with many views (existing bug, found by reading).** Today a sweep from view A can delete
   blobs referenced only by view B once they are older than 15 minutes and the family is over 2 GiB
   (`gc/mod.rs:92-112`, `commands/configure.rs:5996-6006`). A reused old blob is never young
   (§1.3), so the age floor does not protect reuse. Views are opt-in and rarely numerous now, but the
   new default makes this a correctness bug. §8.1 replaces the sweep; slice G1 lands it before any
   slice that writes family blobs from a second view.
2. **Seed pin race.** Reading the seed pointer, then pinning, leaves a window in which the seed
   publishes and sweeps. Pin, then require the pointer to still equal the pinned generation (§6.3).
   The same fix applies at `assembly.rs:166-171`.
3. **Generation switch versus writes.** §5.3 derives the successor's delta from the old delta and
   the generation diff, and replays the journal under the swap lock. Tests are in §14.1.
4. **Crash with an unpublished live delta.** Nothing durable is lost: the delta is rebuilt at bind by
   the strict reconcile. Blobs in the live pin stay protected until the pin's owner is gone, then
   become ordinary garbage. Crash windows during publication are as in views-incremental §2.2.
   Seeding adds one: a crash after the clone but before the diff commits leaves an unpublished file,
   which the new view's sweep reclaims.
5. **Segment write crash.** A temp file is left behind and swept (the analogue of
   `sweep_stale_search_build_dirs`, `search_index.rs:3677`). The segment's row and live-pin entry
   exist before the build (§8.1). The rename is atomic, and a manifest references a segment only
   after it is durable (ruling R20 order: segment fsync → manifest → pointer).
6. **Windows open-file deletion.** §8.1 item 10.
7. **Many publications at once.** Every publication takes the cold-build limiter
   (`executor/view_publication.rs:176`). Forty masons editing at once queue for it. Trigram stays
   exact meanwhile, but callgraph lag grows to about 40 × (1.5–7 s) in the worst case (modelled).
   Recommendation (not blocking): a dedicated view-publication limiter with per-family fairness,
   sized in the resource contract, and no cancellation of a job past materialization (§5.3).
8. **Dirty-file race at fold.** A file edited during the fold read gets one `content` in G+1, read
   once (§5.3). The switch then keeps a live entry for it, because the last observed disk state
   differs from G+1's entry.
9. **Two daemons, one checkout.** Both hold live deltas and both may publish. CAS serializes them,
   and the loser keeps its edits in its live delta and derives against the winner's generation (§5.3).
   Private generation names include the pid and start time (§8). No lease is needed.

### 12.2 Resource contract

A **soft target** starts background work. A **hard limit** starts admission control. Nothing that is
referenced or pinned is ever deleted to meet a limit. Pinned bytes count toward the budget, and status
reports them. The values are starting values (modelled), to calibrate with a family of 10–40 mason
views.

| budget | counted | soft target | hard limit | at soft | at hard |
|---|---|---|---|---|---|
| family blob payload | `SUM(length(payload))` per plane, what the sweep counts today (`gc/mod.rs:209-211`) | 2 GiB, today's budget (`commands/configure.rs:5996-6006`) | governed by physical disk | sweep unmarked rows (§8.1) | — |
| family physical disk (blobs, segments, registry) | file bytes: plane SQLite files and WAL (`BlobStore::usage`, `blob_store/mod.rs:512-529`), segment files, `members.sqlite`. Measured 1.21–1.34× payload (M7) | 3 GiB | 4 GiB | sweep, then `incremental_vacuum`; retire the least-referenced segment | admission |
| family derived databases | every `derived-*.sqlite` of every view, including retained generations and clones in flight | decision D2 (§15.2) | decision D2 | evict per D2 | admission |
| segments per family | count | 4 | 6 | rebase views onto the newest segment when that shrinks their overlay | build no segment; no exception |
| per-view overlay + live delta | `estimated_memory` × the measured RSS factor (1.9–2.3×, §4.4) | 1,000 files or 32 MiB (existing, `search_index.rs:47-50`) | 5,000 files or 128 MiB | rebase | that view's trigram answers use the direct scan (`scanned`) until it rebases |
| process resident index memory | segment tables, per-view tables, overlays, live deltas, retained snapshots, semantic arena | 512 MiB | 1 GiB | evict idle views, least recently queried first; they rebuild at the next bind | new binds serve the direct scan and callgraph `building`; no new resident view |
| retained snapshots per view | versions alive | 2 (current + one draining) | 2 | — | a switch waits until the draining snapshot is released |
| build peaks | concurrent segment builds, derived clones, materializations | — | 2 per process; each reserves its peak (a clone reserves one derived size) against disk before it starts | — | queue |

**Admission at a hard limit.** New protected work that would add bytes is queued: a new view's
generation 1, a segment build, a cross-view seed clone, a semantic fill for a view that is not bound.
Work that already holds its pins completes. Answers disclose the plane as `building` with the reason
`queued: <budget>`. If pinned bytes alone exceed a hard limit, for example because an owner is stopped,
status reports it and nothing is deleted.

**Why the old bounds were not bounds.** Round 1's "S = 4" let a fifth segment through whenever every
existing choice left too large an overlay. Now the hard limit has no exception. The blob store holds
the union of contents across views and retained generations, not one checkout. The RAM formula
omitted retained snapshots, switch overlap, hash-table overhead and the per-view tables. All are now
counted in the budgets above.

- **Clone cost remains O(derived size)** on the first load, measured at 0.31–1.82 s per 257 MB. It is
  the one large non-D term in §7.
- **Semantic arena memory** is shared per family, but a family with several embedding models (after a
  model change) holds both until the old keys age out. Arena entries are loaded lazily and only for keys
  named by resident views, and the arena counts toward the process budget.

### 12.3 Open questions (not blocking)

The three decisions that block slicing are in §15.

16. **Seed selection with no git** (`git` unusable, `alias/mod.rs:781-786`). Fall back to a full
    content walk. The hashing costs 32–59 ms at 4–7k files (M6).
17. **Publication limiter** (§12.1 item 7): the dedicated limiter's size and fairness.
18. **Write contention on a family store.** One writer measured ≈0.25 ms per put transaction (M7).
    10–40 concurrent writers on one family SQLite file, and the `busy_timeout` policy, are not
    measured. T1 measures them.

**Estimate provenance recap.** Measured: every §2 number, M6 and M7; clone and diff times
(views-incremental); ls-tree and closure (perf hunt); cold materialization (branch drill,
views-incremental); borrowed reconcile cost and RSS (phantom-path investigation). Modelled: per-view
memory for many views, first-load totals, derived disk at 40 views, fold lag, every budget value in
§12.2.

---

## 13. Slice plan (question 10)

Contracts come first. K1, K2, K3 and G1 define the snapshot, readiness, freshness and GC protocols,
with their tests, before any slice consumes them. Every slice depends on the contracts it uses.
Migration has its own verification gate, M2, before X1. `NB` marks a check that needs a
`NON-VACUITY BREAK` mutation proof, because its failure would otherwise be silent.

| # | slice | depends on | verification |
|---|---|---|---|
| **K1** | snapshot contract: `LiveDelta` with epoch, journal and intent; the switch derivation of §5.3; one file id per rel path; snapshot `(G, delta@v, intent@w)` with a residency pin; callgraph opens the snapshot's generation (I7) | — | §14.1 sequences on in-memory fixtures; I6 per rel path. **NB:** derive over `keys(delta@v)` only → the revert test red |
| **K2** | readiness contract: per-plane entry states, `path_status` plane and content columns, work queue, completion admission, fill map (§5.6) | — | §14.2 tests. **NB:** count pending from live entries only → the fold-first test red |
| **K3** | freshness contract: block schema of §10, watcher state, dirty intent recorded by every AFT write tool before it acknowledges | K1 | §14.3 tests. **NB:** skip the intent scan → the immediate-grep test red |
| **G1** | family registry and GC epoch protocol for the semantic and callgraph planes (§8.1): register first, protect then touch, conditional delete, fail closed, timer renewal, live pin; pin-then-verify at `assembly.rs:166-171` | — | §14.5 interleavings, multi-process. **NB1:** mark only the caller's view → the cross-view test red. **NB2:** drop `ref_epoch < S` → the reuse-after-mark test red |
| **T1** | trigram blobs keyed with the policy fingerprint; segment writer from blobs; segment table rows; write-contention measurement | G1 | segment from blobs byte-equal to `cache.bin` from files, file table included; a policy change gives new keys; identical concurrent writers; crash at each durability boundary; mismatched payload for one key rejected |
| **G2** | trigram plane, segments and `incremental_vacuum` under G1's protocol | G1, T1 | a segment referenced only by another view's pinned non-current generation survives; a segment under construction survives; file bytes shrink after a sweep |
| **T2** | manifest v2: `content`, `size`, trigram key, per-plane states, walker membership per D1, ignore closure, content-fingerprint generation identity | T1, K2, D1 decided | membership tests of §14.7 in every plane membership applies to; round trip; old manifests re-assemble into v2 |
| **T3** | view trigram index = segment + overlay + live delta, implementing K1 and K3; strict reconcile (§5.1); membership reconcile on ignore-file change; per-segment rel-path file table | T2, K1, K3, G2 | §14.1, §14.3, §14.4 tests; the §2 oracle on matched lines at every step; RSS at 10 and 40 views. **NB:** skip superseding on overlay → equivalence red; skip the membership filter → the M3 test red; `StatFirst` on overflow → the preserved-mtime test red |
| **S1** | semantic vector arena; view semantic search on the arena | — | arena search == today's `view_semantic_search` on fixtures; per-query SQLite reads = 0 (counter); arena memory counted |
| **S2** | semantic fill through K2's work queue on the checkout budget; `refresh::deduplicate_full_keys`; one bind-class put policy (drop `allow_blob_put`) | S1, K2, T3, G1 | borrowed-semantic probe rows; embed count = misses only; two views, one key → one model call; a sweep during a fill keeps the live-pinned vectors |
| **C1** | method-dispatch edges, hints-only join, `\0view:dispatch:` invalidation (§6.4) | G1, T2 | parity vs legacy on opencode and aft with the root unreadable; incremental == cold for dispatch-changing sequences. **NB:** drop dispatch edges → dead-code parity red |
| **C2** | root-independence proof of the final materializer; cross-view seeding (§6.3) | C1, T2, G1, the incremental note's P1 and P3 | two real roots through the production path, every table and bound query results; `inc(seed) == cold` including dispatch. **NB:** root in a row table → equality red |
| **H1** | freshness block in every index-backed response | K2, K3, T3, C1 | §14.6 negative tests for every non-exact state. **NB:** hard-code trigram `exact` → the reconciling test red |
| **F1** | first load: seed selection, strict reconcile against the seed, serve from seed + live delta, own generation 1 | T3, S2, C1, C2, H1, G1 | seed + live delta == cold build of the new checkout at load; the seed publishes or is swept while loading; edits during load; `family_seed` disclosed until installed; timing within the §7 model (±50%) as a secondary check |
| **U1** | read-only view reader; `aft_search path:` and umbrella use it | T3, H1, G1 | cross-root search tests; Fresh-by-stat / Stale / Absent fixtures; `stale_foreign` reported; zero writes outside read markers (filesystem audit in test) |
| **L1** | resource contract of §12.2: budgets, admission, eviction, derived-db policy per D2 | G2, T3, C2, D2 decided | over-budget tests: pinned bytes never deleted; admission queues new protected work; bind after eviction answers the same as with the resident db |
| **M1** | migration: versioned stores, the import state machine, single flight, cleanup rules (§11) | T1, G2, T3, S1, C2 | a unit test per transition |
| **M2** | **migration gate**, required before X1 | M1 | kill -9 at every transition, then restart and complete; repeated import is a no-op; 10 concurrent first binds of one family make one import; an old-version daemon with views on runs during and after the import, and its sweep never touches v2; offline rollback drill (the previous binary serves after downgrade); an incompatible `cache.bin` is rejected and the segment is built instead |
| **X1** | **cutover**: views are the only runtime; delete the `ram_overlay` key handling and gates, owner/borrower, borrow reconcile, per-root `cache.bin` publish, the legacy callgraph store for bound roots, `SemanticIndex.shared_base`, and the `views.enabled` key | every slice above, M2, L1, D1–D3 decided | soak drill (branch-drill harness) with the cold-build oracle sampled during the soak: correctness defects 0; phantom and borrowed probes green; deprecation-warning test for the two removed keys |
| **X2** | remove the legacy importers and legacy-format readers after the support window | X1 + the window | build + full test suite; `aft_inspect` dead-code shows no orphaned legacy symbols |

There is no release with a runtime escape hatch. X1 removes the overlay and the borrow path in the
same release that makes the live delta every checkout's only path. Rollback is the offline procedure
of §11.1.

---

## 14. Round-2 resolutions of the blocking findings

The Athena panel (consult `ct_00000000-0000-4001-98d9-323b501af348`, snapshot `bea20611f`) kept the
architecture and named ten blocking gaps. Each is closed here in short form. The full specification
lives in the body section named in each subsection. Citations are at `573f06fcd`.

### 14.1 Switch rebase: reverts and add-then-delete between cut and swap

**Finding.** Round 1 built the successor's delta from old-delta entries only. The delta is keyed by
content against G, so a revert to G's bytes or an add-then-delete left no entry, and G+1 then served
content that was no longer on disk, reported as exact.

**Resolution** (§5.3). The successor's delta is derived over
`K = keys(delta@v) ∪ paths(diff(G, G+1))`. For each path the last observed disk state is the old
entry's state if there is one, else G's entry, which is exact by the delta invariant. The successor has
an entry exactly when that state differs from G+1. Every mutation, including one that removes an
entry, goes into a journal, and the journal after `v` is replayed under the swap lock. The rule never
reads the fold's cut, so it covers any point between cut and swap, and a foreign CAS winner. I6 holds
by construction (one file id per path), not by the final dedup.

**Evidence.** The final dedup works on file ids (`search_index.rs:2961-2964`). The only existing
replay runs while a load is in flight (`runtime_drain.rs:2637-2644`), and overflow clears it
(`:2307`). Delta postings carry per-index file ids (`search_index.rs:352`, `373-379`). Assembly reads
its own bytes (`assembly.rs:261`) and takes the semantic key by path (`:300`).

**Verification** (K1, T3). Deterministic sequences inject a revert to G's bytes, add-then-delete,
delete-then-recreate, and A→B→A at each point: before the fold read, after it, after the CAS, during
the successor build, and between replay and swap. A foreign-winner case is included. After each
step, I6 per rel path and matched lines equal a cold build. NB: derive over `keys(delta@v)` only, and
the revert test goes red.

### 14.2 Per-plane readiness state machine

**Finding.** A fold can publish content before its semantic fill ends. The live entry then
disappears and takes the pending work and its disclosure with it. Generations with missing keys, and
callgraph extraction failures, had the same gap.

**Resolution** (§5.6). Plane state is part of each manifest entry (`Ready`, `Pending`, `Failed`) and
survives folds, eviction and restart. A content-keyed work queue is filled from live entries and from
the generation's pending entries, so its membership does not depend on divergence. A completion is
installed only for the content, path and producer it computed, and only if the live entry or the
generation still holds that content. Extraction failures persist as `Failed` with the extractor
version.

**Evidence.** The semantic key is optional and taken by path (`assembly.rs:300`). Pending is recorded
only as a per-path reason (`assembly.rs:321-337`, `403-417`). `path_status` has no plane column
(`path_status/mod.rs:17-23`).

**Verification** (K2, S2). Pending survives a fold that publishes first (1 s fold against 15 s fill).
A generation that starts with missing semantic keys fills them with no live entry. A completion that
arrives after a newer edit is dropped, and so is one from before a model change. Pending counts
survive eviction and rebind. NB: count pending from live entries only, and the fold-first test goes
red.

### 14.3 Freshness boundary for grep

**Finding.** "Exact after watcher apply" misses the interval between a write and its apply, when a
new trigram cannot be found at all. Round 1 also misdescribed the watcher path.

**Resolution** (§5.1, §5.2, §10). Every AFT write tool records dirty intent before it acknowledges
the write. A query applies intent before candidate pruning: each intent path not yet applied is
matched on its current bytes directly. An event that cannot be applied now is kept as intent, not
dropped. When the watcher is not healthy, the index is not used for pruning, and the answer comes from
the direct scan (`scanned`). The as-of point is defined: exact as of the snapshot instant, for every
acknowledged AFT write and every applied watcher event, with the last applied watcher sequence
reported.

**Evidence and correction.** The drain's SearchIndex phase applies `update_file`/`remove_file`
itself, inline under the index write lock, with the current ignore matcher, gated by
`heavy_root_work_allowed && apply_ram_search_updates` and sliced by budget
(`runtime_drain.rs:2688-2713`, `2703`, `2695`, `2596-2598`, `2602-2631`). It is not a path collector.
AFT's write path does not notify the index (`edit.rs:590-683`). Today a reconciling borrowed index
already serves through the fallback walk with status Building (`commands/configure.rs:4270-4276`).

**Verification** (K3, T3, H1). A grep issued right after an acknowledged write, with the drain
paused, finds a trigram that did not exist before. With the drain gate forced off, the answer still
finds it. NB: skip the intent scan, and the immediate-grep test goes red.

### 14.4 Strict content verification after any unwatched interval

**Finding.** The reconcile round 1 reused verifies with `StatFirst`, which trusts matching size and
mtime. Lost events are exactly where same-size, preserved-mtime edits hide.

**Resolution** (§5.1). Overflow, rescan, every bind (including after shutdown or eviction), watcher
restart and resume after suspension all run a **strict** reconcile. It hashes every member against
the manifest's `content`. Until it ends, `snapshot.watcher` is `reconciling` and trigram answers come
from the direct scan. There is no
persisted stat baseline to trust. An in-RAM stat table is used only while the watcher stays healthy.
Measured cost: hashing 3,913 files (64 MB) takes 32 ms and 6,997 files (128 MB) takes 58 ms with 8
threads, below the 78–85 ms walk (M6).

**Evidence.** `StatFirst` is hard-coded in the borrowed reconcile (`search_index.rs:2105-2108`). The
overflow handler and the verify memo both say lost events need strict hashing
(`runtime_drain.rs:2301-2305`, `cache_freshness.rs:248-262`). The strict pool exists
(`cache_freshness.rs:298-302`). Manifest entries carry no size or mtime (`assembly.rs:297-304`). An
abandoned pass must be discarded (`search_index.rs:2065-2067`).

**Verification** (T3). Overflow injection: drop events, make a same-size edit that preserves mtime,
trigger the rescan. Grep must find the new content, with `reconciling` or `scanned` reported until
the pass ends. NB: pass `StatFirst` to that reconcile, and the test goes red.

### 14.5 Concurrency-safe family GC

**Finding.** Enumerating every view fixes the scope bug but not the races between marking and
reference creation. The sweep also fails open, and a live but stalled owner loses its pin.

**Resolution** (§8, §8.1). Views register before any protected work. Every store keeps a GC epoch
and a per-row `ref_epoch`, and put becomes put-or-touch. Reference creators make their protection
durable, then touch every key they rely on. The sweep bumps the epoch, marks every member's current
and protected generations, pins, live pin, segments and derived owners, then deletes with
`WHERE ref_epoch < S`. The delete is revalidated in the same transaction that any touch would
serialize with. Unpublished semantic keys, trigram blobs of live entries and segments under
construction are in the live pin. Any unreadable or malformed protection aborts the sweep. Pins are
renewed by timer and reclaimed only when their owner is gone, and a publisher re-checks its pin before
its CAS.

**Evidence.** One view marked: `gc/mod.rs:92-112`, `119`, `163`, `177-183`;
`commands/configure.rs:5949-6005`; `views/generation.rs:60-91`. Only two planes: `gc/mod.rs:98`,
`blob_store/mod.rs:103-109`. Fail-open: `gc/mod.rs:132-135`, `163-170`, against
`views/generation.rs:141-149`. Reuse is never young: `blob_store/mod.rs:553-574` with
`gc/mod.rs:199-200`, `225`. Existence check before pin, callgraph keys only: `assembly.rs:282-289`,
`339-342`, `345-352`. Renewal and expiry: `pins/mod.rs:138-152`, `gc/mod.rs:143-149`. Unconditional
delete: `gc/mod.rs:228-231`.

**Verification** (G1, G2). Multi-process deterministic interleavings: registration during a sweep;
pin creation after marking; reuse of an old unreferenced key after marking; publication and pin
release during marking; a third view's reader pinned on a non-current generation; a malformed pin and
an unreadable `readers/` (the sweep aborts); a publisher stopped past the TTL (its pin survives and it
publishes); process death (its pin is reclaimed). NB1: mark only the calling view, and the cross-view
test goes red. NB2: drop the `ref_epoch < S` condition, and the reuse-after-mark test goes red.

### 14.6 Honesty states

**Finding.** "Trigram: exact" could not express the apply window, reconciliation or a foreign view.
Semantic counts missed published deficiencies. The callgraph rule missed configuration and
membership dependencies, and `building` meant two things.

**Resolution** (§10). Trigram gains `applying(n)`, `scanned`, `reconciling` and `stale_foreign`.
Semantic and callgraph gain `failed`, `building` and `absent`, and their counts include published
deficiencies. Readiness is per plane, so migration (`building`), extraction failure (`failed`) and
reconciliation cannot produce `complete: true`. Any callgraph-relevant change, whether a language
file, a resolution input, a membership change or a deficiency, marks every callgraph answer `stale`
until an answer-specific dependency proof exists (D3). Divergence from the generation is reported
separately from answer exactness.

**Evidence.** Resolution inputs are extracted as `config` (`assembly.rs:262-268`). The incremental
note names configuration, membership and transitive dependencies
(`views-incremental-materialization.md:60-84`).

**Verification** (H1). For each response family and each non-exact state, a negative test asserts
`complete: false`: reconciling, applying, pending from a published deficiency, failed callgraph,
building after migration, a relevant configuration change, and a foreign view. NB: hard-code trigram
`exact`, and the reconciling test goes red.

### 14.7 Callgraph gates

**Finding.** Seeding rested on a root-independence argument that the code supports but does not
prove. Views drop dispatch inputs, and membership was still an open question.

**Resolution** (§6.3, §6.4, §3.2, §15.1). C1, with dispatch through a hints-only join and no file
reads, comes before any seeding. C2 depends on C1, T2 and G1, and proves root independence on the
final materializer: two real roots through the production path, every table and bound query results,
no absolute root anywhere, and a row-table mutation control. Seeds without derived rows, or with other
schema, materialization or extractor versions, are refused. Seeds are resolved through `.ref` and
pinned-then-verified by pointer equality. Walker membership replaces the HEAD tree, subject to D1.
Tests cover untracked add and remove and ignore-file changes in every plane membership applies to.

**Evidence.** `dispatch_hints: Vec::new()` (`callgraph_store/join.rs:1117`, in `:1106-1118`); synthetic
`/` root (`:1127-1137`); HEAD-tree membership (`assembly.rs:178`, `213`); `.ref` indirection
(`views/generation.rs:19-34`); the tracked-deleted read uses `?` (`assembly.rs:261`).

**Verification** (C1, C2, T2). See the slice rows in §13, including the dispatch-edge NB and the
row-table root NB.

### 14.8 Cutover

**Finding.** X1 deleted the dependencies of the `views.enabled=false` escape hatch it kept, and
migration had no durable state machine, no concurrency handling and no rule for old daemons.

**Resolution** (§11). There is one runtime model and no escape hatch. X1 removes the legacy runtime.
Rollback is offline: the previous binary plus legacy caches that the new version never writes and
retains for a window. Stores are versioned (`blobs/v2`, `views/v2`), so an old daemon's single-view
sweep cannot reach them. Legacy directories are never cleaned while a legacy owner heartbeat is
fresh. A durable, idempotent import state machine handles concurrent first binds with a claim, and
the same claim single-flights the family's first callgraph build. Compatibility checks run before a
legacy segment is attached. Trigram is not exact until the strict reconcile has run. Cleanup follows
verified import rows.

**Evidence.** Old daemons sweep `blobs/<family>/` with one view's references
(`commands/configure.rs:5940-6009`). Legacy owners keep a heartbeat (`artifact_owner.rs:239-253`).
The existing importers and their fingerprint check and durable rebuild state are
`migration/mod.rs:342-529`, `895-907` and `531-608`.

**Verification** (M2). See the M2 row in §13: kill -9 at every transition, repeated import,
concurrent first binds, an old daemon running alongside, the offline rollback drill, and an
incompatible `cache.bin`.

### 14.9 Slice dependencies

**Finding.** The dependency graph did not encode the contracts the slices consume, and migration had
no gate of its own.

**Resolution** (§13). K1, K2 and K3 (snapshot, readiness, freshness) and G1 (GC) come first. S2
consumes K2 and T3. C2 follows C1, T2 and G1. F1 depends on H1 and C1. T1 depends on G1, so no family
blob is written before its collector is safe. H1 depends on C1. L1 carries the resource contract. M2
is a distinct migration gate before X1, and X1 also requires the three operator decisions.

### 14.10 Resource contract

**Finding.** "S = 4" had an exception, the blob store was sized for one checkout, and the RAM formula
omitted retained snapshots, overhead and per-view tables. There were no admission rules.

**Resolution** (§12.2, §4.4). Soft targets and hard limits cover payload, physical disk (measured
1.21–1.34× payload, M7), derived databases (D2), segment count (4 and 6, no exception), per-view
overlay, process resident memory, retained snapshots and build peaks. At a hard limit, new protected
work is queued and disclosed. Nothing referenced or pinned is deleted, and pinned bytes are counted
and reported.

**Evidence.** Per-view tables in `SearchIndex` (`search_index.rs:347-371`); the sweep counts logical
bytes (`gc/mod.rs:209-211`); file bytes are available (`blob_store/mod.rs:512-529`); no store enables
`auto_vacuum`.

**Verification** (L1, T3, G2). See the L1 row in §13, plus RSS at 10 and 40 views in T3 and file
shrink after a sweep in G2.

### 14.11 Also corrected (not blocking)

- The blob-to-segment ratio was inverted: it is 0.94–0.95× (§4.1).
- The 40-view memory figures are 156–248 MB (D = 100) and 65–104 MB (D = 16) including per-view tables
  (§4.4).
- A query pin is one read marker per resident generation, not one per query (§5.2).
- One content per manifest entry, and every plane key from one read (§3.2, §5.3).
- A publication past materialization is allowed to reach its CAS (§5.3).
- The trigram key includes `max_file_size` and the other payload-deciding rules (§4.1).
- An ignore-file change triggers a membership reconcile under the current rules (§5.1).
- New SQLite files open through `IdentityConnection` and follow the existing live-file-set rules: no
  raw second descriptor, clone only through backup, and no rename of a live set (§8).

---

## 15. Operator decisions needed before slicing

Each decision has options, a recommendation, and the default this note assumes if no answer comes.

### 15.1 D1: untracked files in the callgraph

- **(A) Walker membership for all planes (recommended).** Callgraph sees the same files as grep and
  semantic: tracked and untracked files that are not ignored.
  - For: it matches legacy callgraph, which builds from the walker, so the C1 and X1 parity gates
    compare like with like. A mason's new files are untracked until it commits, often until the end of
    its task. Under (A), `callers` and `impact` see the code the agent is writing.
  - Against: extraction runs on untracked files, so generated or scratch files that are not ignored
    enter the callgraph. Ignore rules are the control, as they are for grep today. Generations are
    identified by manifest content, not HEAD (§3.2). Folds happen more often, because untracked
    edits count.
- **(B) Tracked-only callgraph.** Walker membership for trigram and semantic, and the HEAD tree for
  callgraph.
  - For: smaller extraction scope, fewer folds, and generation identity close to today's.
  - Against: callers of a function defined in a new, uncommitted file are missing. That is a false
    negative against legacy, and a common one, because masons commit late. Membership differs by
    plane, which the manifest and the honesty block must both represent. Every callgraph answer while
    untracked files exist would need a disclosure.

Default if unanswered: (A).

### 15.2 D2: derived-database disk per view

Measured: 257–428 MB per view at opencode scale (Run 3 269,889,536 bytes; MEASUREMENTS §Final run
424–428 MB). Modelled: 10–17 GB at 40 views, one database each. That is not a peak: a publication holds
the base and the next generation together, and seed clones add one database each while they run.

- **(a) Family budget with eviction of unbound views (recommended).** Soft target 4 GiB, hard limit
  6 GiB per family (starting values, modelled: 10–15 databases at 257–428 MB, plus room for two
  concurrent clones at the hard limit). At the soft target, evict the derived databases of views that
  are not bound, oldest `last_bind` first. Never evict a bound view's current generation or any
  pinned generation. An evicted view re-seeds on its next bind (§6.3, modelled at 2–3 s for
  D≈16). When bound views alone reach the hard limit, a new bound view's callgraph is
  `building (queued: disk)` rather than evicting another bound view.
- **(b) Share one derived file across views with identical callgraph-plane manifests,** common for
  fresh masons before their first source edit. It saves the most disk, but it needs cross-view
  ownership references and matching GC rules. It is a follow-up to (a).
- **(c) No materialized database for unbound views** (lazy navigation reads,
  `views-lazy-read-benchmark-2026-09.md`, provisional). Research.

Default if unanswered: (a) with the values above.

### 15.3 D3: precision of stale-callgraph disclosure

- **(A) Conservative and global (recommended for X1).** Any callgraph-relevant change (§10) marks
  every callgraph answer `stale` until the next generation installs. It is sound. The cost is that
  answers are incomplete while an agent edits. The window is the 1 s quiet period plus a publication
  (≈1.5–7 s modelled, §5.5), so most answers outside active bursts stay complete.
- **(B) Answer-specific dependency check.** Mark an answer stale only when a change reaches it through
  `file_dependencies`, negative (missing-target) dependencies, configuration inputs or the dispatch
  pseudo-domain. It is precise. It needs its own proof test against generated sequences before it may
  report `exact`, and it costs about one indexed query per answer. It is a follow-up slice after C1.
- **(C) Round 1's per-file rule** ("a node on the returned path is in the delta"). It is unsound,
  because new targets and configuration changes alter unchanged callers (§10), and it is listed only
  to reject it.

Default if unanswered: (A), with (B) as a follow-up.

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
5. **M6 and M7** (round 2). A second scratch crate at `/tmp/aft-measure-r2` depends only on
   `blake3 = "1.8"` and `ignore = "0.4"` (release, own target directory). It walks each export with
   `ignore::WalkBuilder` (`hidden(false)`, `require_git(false)`, custom ignore file `.aftignore`,
   `.git` skipped). It stats every file, then reads and blake3-hashes every file three times with 1
   thread and three times with 8 threads that pull from a shared counter. M7 is a Python script that
   walks the same export (skipping `.git`, `node_modules` and `target`), keeps files up to 1 MiB with no
   NUL in their first 8 KiB, and counts distinct lower-cased 3-byte windows per file. It inserts one row
   per distinct file into a fresh SQLite database with the `blob_payloads` schema and index of
   `blob_store/mod.rs:87-101`, `journal_mode=WAL`, `synchronous=FULL`, one transaction per insert, and
   a random payload of `1 + 6 × distinct` bytes. It records file and WAL sizes before and after
   `wal_checkpoint(TRUNCATE)`.
