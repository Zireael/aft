# Semantic search and callgraph in borrowed checkouts (design note, 2026-09)

Status: design only. No product code changed. Builds on
`docs/investigations/borrowed-index-phantom-paths-2026-09.md` (the search
index half of the same problem) and `docs/design/content-addressed-index-views.md`.

## Summary

- **Today (views off, the default), a borrowed checkout's semantic lane and
  callgraph describe the live checkout.** Measured in a linked worktree and a
  shared clone at an older commit: the callgraph was wrong on all 5 probes
  (false callers, missing callers, a phantom file with a full call tree, and
  `symbol_not_found` for a function that is in this checkout). The semantic
  lane ranks functions by the live checkout's bodies. `aft_search` returned a
  function for a phrase that exists only in the live copy. Every answer said
  `status: ready`, `complete: true`. `worktree.ram_overlay` fixes the lexical
  lanes but changes nothing for semantic or callgraph.
- **With `views.enabled`, the borrower's callgraph was already correct on all
  5 probes,** because the borrower publishes its own per-checkout view. The
  semantic lane silently drops files whose content differs from the owner's,
  because nobody embeds them for the borrower. The borrower's callgraph blobs
  arrive through one of three publication paths, the only one that allows a
  borrow-only root to write blobs (`runtime_drain.rs:1212`).
- **Recommendation:** (1) honesty first: an existence-and-freshness filter plus
  disclosure on the views-off path; (2) make views correct for borrowers:
  one blob-write policy, a semantic fill for pending paths, and pending paths
  shown in responses; (3) seed a new checkout's first view from the owner's
  derived database, so bind cost scales with the changed files; (4) then turn
  views on for borrowers. A private RAM callgraph overlay is not recommended.

## 1. What happens today (measured)

### Fixture and method

A temporary `#[ignore]` probe in the `configure.rs` test module was run with
isolated tempdir storage, never the live daemon, and was not committed.
Reproduction is described in the appendix. It reuses the phantom-path harness:
the live repository is configured first and stays alive as artifact owner, then
the older checkout is configured and probed. A local mock OpenAI-compatible
embedder returns a 256-dimensional hashed bag of words, so vectors depend on
content and the run is deterministic. Search, semantic and callgraph are all
enabled.

| file | older commit (the borrower's checkout) | live HEAD (the owner) |
|---|---|---|
| `helpers.ts` | `helperOld()`, `helperNew()` | same |
| `router.ts` `routePacket` (**differs**) | doc "carrier pigeon courier", calls `helperOld()` | doc "quantum teleport beam", calls `helperNew()` |
| `here_only.ts` `calibrateFlux` (**only here**) | doc "calibrate flux walrus", calls `helperOld()` | deleted |
| `live_only.ts` `reticulateSplines` (**only live**) | absent | doc "reticulate splines zebra", calls `helperNew()` |

Variants: linked worktree with `ram_overlay` off and on, shared clone with
`ram_overlay` off, linked worktree with views on (`ram_overlay` off and on).
In every variant the borrower was `ArtifactOwnerMode::ReadOnly`,
`shared_artifacts_read_only()=true` and semantic `Ready`. The three views-off
variants gave identical semantic and callgraph rows. `ram_overlay` changed only
the lexical lanes of `aft_search`.

### Semantic rows

"Lane" is the borrower's resident semantic index queried directly (top hit).
"aft_search" is the user-facing hybrid handler. Every `aft_search` response
was `status=ready complete=true semantic_status=ready`.

| probe (query) | expected here | views off | views on |
|---|---|---|---|
| index contents | 8 entries, including `here_only.ts::calibrateFlux` | 8 entries, **including `live_only.ts::reticulateSplines`**, none from `here_only.ts` | view manifest: `router.ts` and `here_only.ts` pending ("shared semantic blob unavailable") |
| only live ("reticulate splines zebra") | no hit | lane top: `live_only.ts::reticulateSplines` 0.514 (file absent). aft_search drops it with the borrower `is_file()` filter | not in lane |
| only here ("calibrate flux walrus") | `here_only.ts::calibrateFlux` | not in lane (all scores 0.000). aft_search: **not found** with overlay off; with overlay on, found only by the exact lexical lane | not in lane; exact lane only (overlay on) |
| differs, this body ("carrier pigeon courier") | `routePacket` first | lane top-3 is `helpers.ts` (0.289/0.100/0.100); `routePacket` appears only as a low semantic tail in aft_search, with a snippet read from disk | not in lane; exact lane only (overlay on) |
| differs, live body ("quantum teleport beam") | no `routePacket` | lane top: **`routePacket` 0.505** (live vector). aft_search overlay off: `routePacket` rank 1 via lexical, **snippet quotes "quantum teleport beam", text not on disk**. Overlay on: `routePacket` rank 2 via semantic, disk snippet says "carrier pigeon" | not in lane |

Freshness detection is already available on the borrowed index.
`is_file_stale` returned `router.ts=true`, `here_only.ts=true` (no record),
`live_only.ts=true` (deleted) and `helpers.ts=false`.

### Callgraph rows

All responses were `success=true` unless noted, with no staleness marker.

| probe | expected here | views off (worktree ±overlay, clone) | views on |
|---|---|---|---|
| `callers helpers.ts::helperOld` | `routePacket`, `calibrateFlux` | **0 callers** | correct |
| `callers helpers.ts::helperNew` | 0 | **`reticulateSplines` (live_only.ts, no such file), `routePacket`** | correct (0) |
| `call_tree router.ts::routePacket` | → `helperOld` | **→ `helperNew`** | correct |
| `call_tree here_only.ts::calibrateFlux` | → `helperOld` | **`symbol_not_found`** (file exists) | correct |
| `call_tree live_only.ts::reticulateSplines` | file does not exist | **success, full tree → `helperNew`** | `symbol_not_found` (correct refusal, misleading wording) |

### Costs observed

- Borrower model calls: 4 in every variant, the 4 query embeds. There were zero
  corpus embeds, even in the views and overlay variants.
- Views on: the borrower wrote 2 callgraph blobs (blob-store rows 4 → 6, the
  older `router.ts` and `here_only.ts`) and 0 semantic blobs (4 → 4). It
  published its own view (`storage/views/<its scope>/`, generation 2) with
  `pending=[here_only.ts, router.ts]`.
- Bind to ready: 135-142 ms with views off, 391-432 ms with views on. This is a
  4-file fixture; it shows the ordering, not the cost at scale.

## 2. Why

**Semantic, views off.** The borrower opens the owner's `semantic.bin`
read-only (`readonly_artifacts.rs:284-337`). Bind-time reconciliation covers
only the search index (`configure.rs:4203-4204`, `4277-4291`). The semantic
refresh worker starts on a borrower only with `ram_overlay`, and even then only
for files changed after bind (`configure.rs:1215-1226`, `docs/config.md:166-172`).
The only borrower guard on results is `is_file()` (`commands/semantic_search/mod.rs:3548-3550`,
`3571-3574`). It hides phantom files but not stale vectors. Snippets for the top
results are re-read from disk (`mod.rs:3611-3615`). That is why a stale ranking
can arrive with a current snippet that contradicts it. The `WorktreeConfig` doc
says semantic "stays frozen" (`config.rs:449-453`), but `docs/config.md:167-171`
claims private semantic deltas. The two disagree.

**A trap for any delta design.** `SemanticIndex::invalidate_files` first calls
`materialize_shared_base` (`semantic_index.rs:6160-6165`, `4449-4500`). That
clones every borrowed vector into private memory, so one invalidation on a
borrower costs a full copy of the index.

**Callgraph, views off.** The borrower opens the owner's published generation,
keyed by artifact key, read-only (`callgraph_store/mod.rs:3486-3511`,
`context.rs:5955-5965`). It never builds or repairs it (test
`linked_worktree_configure_defers_read_only_artifact_loads_without_cold_builds`,
`configure.rs:10354-10401`). Queries answer from SQLite with no check of file
content. The store records a per-file `content_hash` (`callgraph_store/mod.rs:8321-8331`),
so such a check is possible.

**Views on.** Callgraph queries read this checkout's own view, keyed by
`project_scope_key` (design R3, `content-addressed-index-views.md:59-62`;
`context.rs:5842-5866`). A view whose generation lags HEAD answers `Building`
(`context.rs:5849-5856`). A borrower gets callgraph blobs only because the
semantic-ready publication path passes `allow_blob_put=true` unconditionally
(`runtime_drain.rs:1194-1215`). The other three schedulers pass
`!shared_artifacts_read_only()` (`configure.rs:6330-6334`,
`runtime_drain.rs:1916`, `2950-2954`). With writes refused, a missing
callgraph blob blocks publication (`views/assembly.rs:391-426`). Design R12
allows borrower writes (`content-addressed-index-views.md:114-117`), but the
code allows them on one path only. Semantic: `view_semantic_search` scores only
the manifest's semantic keys (`commands/semantic_search/mod.rs:2161-2216`,
`3499-3513`). A borrower's keys come from `store_live_semantic_blobs`, which
re-keys a borrowed vector only when the disk bytes' blake3 matches the recorded
hash (`migration/mod.rs:273-340`). So stale vectors never reach the view, but
changed files get no key. Nothing embeds them (`configure.rs:1215-1226`), and
they stay pending (`views/assembly.rs:427-435`) without disclosure.

## 3. Options

Scale anchors: about 8-20 chunks per file and about 64 chunks per batch
(`semantic-embed-rss-2026-09.md:71`, `136-137`). Resident cost is about
2.6 KB per chunk (`:342`). Local ONNX embeds about 199 chunks/s (`:128`).
Views soak on the aft repository: cold `derived.sqlite` materialization takes
35.8-43.8 s and about 257 MiB (plus a temporary generation of the same size),
manifest assembly 1.7-2.5 s, blob lookup 0.2-0.3 s
(`views-soak-2026-09/branch-drill.md:34`). Below, N is the files in the
checkout and D is the files that differ from the borrowed snapshot (D ≪ N).

### (a) Private in-RAM delta

- **Semantic.** At bind, find D with `retain_files_with_changed_content`
  (`semantic_index.rs:6117`) or with a git diff (phantom note, option B).
  Tombstone D's base entries and embed D privately. This needs a new overlay
  structure (a tombstone set plus private entries over the `Arc` base), because
  today's invalidation copies the whole base.
  - Memory: D × chunks/file × 2.6 KB. For D=200, about 4,000 chunks, about
    10 MB. The existing code path instead costs the full index, about 0.9 GB at
    353k chunks.
  - Model calls: D × chunks/64 batches, about 63 batches (about 20 s local) for
    D=200. This is paid again by every borrower and after every restart, with
    no sharing.
  - Bind: a freshness walk over N (clones hash all N because their mtimes
    differ), or O(D) with a git diff.
  - Correctness: exact after catch-up.
- **Callgraph.** Re-extracting D over the borrowed graph means an overlay that
  masks and unions inside every query (callers, call_tree, impact, trace_to,
  trace_data, dead code). It must also re-resolve reverse dependents of D,
  because imports resolve across files.
  - Memory: small.
  - Model calls: 0.
  - Bind: extraction of D plus dependents.
  - Correctness risk: high, and it is a large refactor.
  - Variant: clone the owner's generation file (APFS clonefile, near free;
    otherwise a full copy) and run the existing writer refresh on D privately.
    That moves the cost to disk per borrower and adds garbage collection for
    private stores.

### (b) Content-addressed views

The measurement shows views already give a correct borrower callgraph. What is
missing:

1. **One blob-write policy for borrowers.** Today writes happen only via
   `runtime_drain.rs:1212`. Either all four schedulers follow R12, or none do
   and the borrower view stays pending forever.
2. **Semantic fill.** A borrower-side worker should embed the semantic-pending
   paths on the borrower's budget, write `SemanticKey` blobs (content plus
   path, `blob_store/mod.rs:163-221`) and republish. The model-call cost equals
   (a), but it is paid once per (content, path) across every checkout and
   restart.
3. **Cheap first publication.** Incremental materialization clones a base only
   from this view's own current generation (`views/assembly.rs:468-539`). A new
   mason worktree has no generation, so its first publication is cold:
   36-44 s and about 257 MiB at aft scale. **The in-flight incremental work
   does not change first-bind cost** unless the first publication can seed
   from the owner's `derived-<gen>.sqlite` and apply the owner-to-borrower
   manifest diff (`apply_manifest_diff`, `views/materialization.rs:63-94`).
   That needs derived rows independent of the root (to verify). With seeding,
   the cost becomes a clone plus D-proportional materialization.
4. **Disclosure** of `pending_paths` in `aft_search` and callgraph responses.
5. **Default:** views are opt-in (`docs/config.md:177-182`).

Costs:

- Memory: on disk. A derived database per checkout (clonefile shares extents
  on APFS), plus a transient RSS peak during materialization.
- Model calls: D, shared across checkouts.
- Bind: HEAD reuse plus assembly (seconds). Materialization is cold today and
  D-proportional with seeding (unmeasured).
- CPU per publication: `store_live_semantic_blobs` serializes the whole index
  (`views-perf-hunt-2026-09-16.md:117`).
- Correctness: exact per checkout.

### (c) Filter to present-and-unchanged paths, and say so

- **Semantic.** Drop results whose file fails `is_file_stale`
  (`semantic_index.rs:6041-6076`; a stat, plus a hash only when the mtime
  changed).
- **Callgraph.** Compare the stored `content_hash` with disk for every file a
  result touches, as caller or target. Drop or flag edges from changed files
  and refuse phantom files.
- **Both.** Report "D files differ from the borrowed snapshot; semantic and
  callgraph answers exclude them" with `complete=false`. D comes from the
  search reconcile summary that already runs at bind once `ram_overlay` is the
  default.
- Costs: memory about 0, model calls 0, bind about 0 beyond the search
  reconcile, and one stat per result.
- Correctness: no false positives, and every false negative is disclosed. On
  the probes, C1 would say "0 callers, 2 changed files excluded" and C4 would
  say "here_only.ts is not in the borrowed snapshot". Views-on semantic already
  behaves this way, minus the disclosure.

### (d) Better combinations

- **(b) with seeding plus (c) as a permanent backstop** dominates (a). It
  costs the same model calls, and the embeddings are shared, persistent and
  already built.
- **Exact-match fast path.** A borrower at the owner's HEAD with no dirty
  files on either side is already exact, so it can skip all work. This needs
  the owner's dirty set in the snapshot (phantom note, option B). Mason
  worktrees often start at the live HEAD.

| option | memory | model calls | bind | correctness |
|---|---|---|---|---|
| (a) semantic delta | D×chunks×2.6 KB (needs a new overlay; today's path copies the full index) | D per borrower, per restart | walk N, or git diff D | exact after catch-up |
| (a) callgraph overlay | small | 0 | extract D plus dependents | high risk; every query path changes |
| (b) views, as shipped | disk | 0 for borrowers | cold materialization per new checkout | callgraph exact; semantic silently incomplete |
| (b) + fill + seeding | disk | D, shared | about D | exact |
| (c) filter + disclose | about 0 | 0 | about 0 | honest but incomplete |

## 4. Recommendation and slices

**Invariant for every slice:** a semantic or callgraph answer served from a
snapshot that differs from this checkout says so. It carries
`complete=false`, a count of differing paths, and a named reason. It never
reports `ready`/`complete: true` as it does today.

1. **Honesty on the views-off path (small).** Filter and disclose as in (c):
   semantic `is_file_stale` filtering in both borrower retain sites
   (`semantic_search/mod.rs:3548`, `3571`); callgraph `content_hash` check
   plus a file-existence refusal in the callgraph adapter; the differing-path
   count taken from the search reconcile summary. Fix the
   `config.rs:449-453` / `docs/config.md:167-171` contradiction. Turn the probe
   rows above into committed tests; they should go green for "no false
   positive, disclosed false negative".
2. **Views correct for borrowers.** Pick one borrower blob-write policy
   (recommended: R12, applied at all four scheduler sites). Add a
   semantic-fill worker for pending paths on the borrower's own budget, with
   breaker and quota per family. Show `pending_paths` in `aft_search` and
   callgraph responses.
3. **Seed the first view from the owner.** Clone the owner's derived
   generation and apply the manifest diff. Prove first that derived rows are
   independent of the root. Measure on the aft repository with a mason
   worktree 1 and 50 commits behind; target bind proportional to D, not
   35-44 s.
4. **Views on for borrow-only roots** after a soak. Keep slice 1's filter as
   the backstop for any path that skips publication.

Not recommended: the private RAM callgraph overlay. The semantic RAM delta is
worth building only if views cannot become the default for borrowers, and then
only with a tombstone overlay, never `materialize_shared_base`.

## Appendix: reproducing the probe

In the `configure.rs` test module, reuse `init_git_fixture`, `git_commit_all`,
`test_context`, `handle_configure_for_test`, `wait_for_search_index_ready` and
`OlderCheckoutKind`. Configure with `search_index`, `semantic_search`,
`callgraph_store`, `views.enabled` and `worktree.ram_overlay` as per variant,
and `semantic.backend=openai_compatible` pointing at an in-test HTTP server
that returns an L2-normalised 256-bucket blake3 bag-of-words vector per input.
Build the fixture from the table in section 1, configure the live root
(owner), then `git worktree add --detach <older>` or
`git clone --shared` + `checkout --detach <older>`, and configure the borrower.
Probe with `SemanticIndex::search` on `ctx.semantic_index()`,
`handle_semantic_search`, `handle_callers` and `handle_call_tree`. Count
embedding-server inputs before and after the borrower binds, and count
`blob_payloads` rows in `storage/blobs/<key>/*.sqlite`. Run with
`AFT_TEST_DISABLE_FILE_WATCHER=1`, `AFT_CALLGRAPH_BUILD_WAIT_MS=5000`,
`--ignored --nocapture --test-threads=1`.
