# Borrowed search index serves another checkout's files (phantom paths), 2026-09

Status: options E and C are built (see section 5). The reproductions below are
regular tests that pass with the default config. B and the ownership hazard in
2.5 are still open.

## Summary

Every checkout whose HEAD history has the same root commit gets the same
artifact key. Clones, `cp -R` copies and linked worktrees at other commits
therefore open the live checkout's shared search snapshot (`cache.bin`) as
read-only borrowers. With `worktree.ram_overlay` off (the default), a borrower
takes that snapshot as it is: it is marked ready, never compared with the
borrower's own files, and never updated by the borrower's watcher. Answers that
come from the index file list (glob, the path and lexical lanes of
`aft_search`) then list files that exist only in the live checkout. Answers
that come from postings (grep and the lexical lanes) cannot find content that
exists only in the borrowing checkout.

The #337/#338 reconciliation (`reconcile_borrowed_snapshot_with_disk`) would fix
this for every kind of checkout, but it only runs when `worktree.ram_overlay` is
on. The operator's replay ran with the default, so it never ran.

## 1. Reproduction

Tests are in `crates/aft/src/commands/configure.rs` (test module, all
`#[ignore]`):

| test | older checkout made with | result today |
|---|---|---|
| `phantom_paths_shared_clone_of_older_commit` | `git clone --shared live scratch` + `checkout --detach <older>` | **FAILS** (reproduces) |
| `phantom_paths_plain_clone_of_older_commit` | `git clone live scratch` + `checkout --detach <older>` | **FAILS** (reproduces) |
| `phantom_paths_linked_worktree_at_older_commit` | `git worktree add --detach scratch <older>` | **FAILS** (reproduces) |
| `phantom_paths_copied_tree_at_older_commit` (unix) | `cp -R live scratch` + `checkout --detach <older>` | **FAILS** (reproduces) |
| `phantom_paths_shared_clone_with_ram_overlay_control` | shared clone, `worktree.ram_overlay: true` | passes (control) |

Run: `cargo test -p agent-file-tools --lib phantom_ -- --ignored --nocapture --test-threads=1`

Fixture (`older_checkout_beside_live_owner`): the live repository's older commit has
`shared.ts` containing `OLDER_COMMIT_TOKEN`. Its HEAD rewrites `shared.ts` to
`LIVE_HEAD_TOKEN` and adds `extra/phantom.ts` (`PHANTOM_FILE_TOKEN`). The live
root is configured first and publishes as artifact owner. The owner context stays
alive, as the live checkout's owner does in the daemon. Then the older checkout is
configured with default config and probed. Each test first asserts that the
checkout is `ReadOnly` / borrow-only and that glob and grep answered from the
index, so a pass can't come from a fallback disk walk.

Observed probe for all four kinds with `ram_overlay` off (the output is the same for each kind):

| probe | expected | observed |
|---|---|---|
| `glob **/*.ts` | `scratch/shared.ts` only | `scratch/shared.ts`, **`scratch/extra/phantom.ts`** (source: index) |
| `grep PHANTOM_FILE_TOKEN` | 0 | 0 |
| `grep LIVE_HEAD_TOKEN` (live content of a file both checkouts have) | 0 | 0 |
| `grep OLDER_COMMIT_TOKEN` (this checkout's own content) | 1 | **0** (false negative, status `Ready`) |
| `aft_search "phantom.ts"` | no phantom result | **returns `scratch/extra/phantom.ts`** (lexical `path_lookup` lane, `file_summary`) |
| `aft_search "OLDER_COMMIT_TOKEN"` | finds `shared.ts` | **does not find `shared.ts`** |

Control with `ram_overlay: true`: glob lists only `shared.ts`, grep finds
`OLDER_COMMIT_TOKEN` once, and `aft_search` finds `shared.ts` and does not
name the phantom path.

This matches the report. The phantom path is rebased onto the borrower's
root, so `read` of `crates/ck-bus/...` then fails with not-found
(`refire-C-r1.jsonl` line 15 glob `**/*foundation*`, line 40 failing read).
In the report it happened in 7 of 8 runs. See the ownership notes below
for how the remaining run could avoid borrowing.

## 2. Root cause

### 2.1 All checkouts of one repository share one artifact key

- `search_index.rs:5360-5379` `artifact_cache_key` and
  `search_index.rs:5381-5395` `artifact_cache_key_with_memo` derive the key
  from `repo_root_commit_with_retry`.
- `search_index.rs:6134-6137`: the identity is `git rev-list --max-parents=0 HEAD`,
  the root commit(s) of HEAD's history.
- `search_index.rs:5505-5510`, `5484-5486`: at a top level the key is
  `artifact_hash16(root_commit)`. The key doesn't include the checkout path, the
  git dir, HEAD or the tree.

So the live checkout, a shared or plain clone, a `cp -R` copy and a linked
worktree all resolve to `storage/index/<same key>/cache.bin`.

### 2.2 The second checkout becomes borrow-only

`configure.rs:3319-3327` calls `artifact_owner::claim_or_open_read_only`:

- Linked worktree: `artifact_owner.rs:104` always borrows (`open_read_only_borrow`).
- Clone or copy (not a linked worktree): the manifest exists, and the checkout is
  neither the same checkout (`project_scope_key`) nor the same git family.
  `artifact_owner.rs:123` compares `git_common_dir`, and a clone or copy has
  its own `.git`. So `artifact_owner.rs:148` `manifest_owner_alive` decides.
  `artifact_owner.rs:552-559` checks the owner's pid on the same host. In
  production one daemon process serves both roots, so the pid is its own and
  is always alive. The clone gets `ArtifactOwnerMode::ReadOnly`
  (`artifact_owner.rs:152`).
- `configure.rs:3344-3348`, `3380-3381`, `3448`: `ReadOnly` → no callgraph
  writer → `ctx.shared_artifacts_read_only()` is true (`context.rs:4909-4911`).

### 2.3 The borrowed snapshot isn't compared with this checkout

- `configure.rs:4195-4196`: the search load is read-only, so
  `readonly_artifacts::open_search_index_read_only` runs.
- `readonly_artifacts.rs:198-238`: the snapshot is loaded (paths rebased onto
  this root), `set_ready(true)`, and returned as `Fresh`. It compares no HEAD,
  tree or file list, even though the snapshot stores its HEAD
  (`search_index.rs:363` `git_head`, accessor `stored_git_head` at `2005`).
  The writable path uses that HEAD (`configure.rs:4362-4388`); the borrow path
  does not.
- `configure.rs:4201-4202`: `reconcile_borrowed_overlay = !read_only || ram_overlay_active()`.
  `ram_overlay_active()` is `shared_artifacts_read_only() && config.worktree.ram_overlay`
  (`context.rs:4930-4932`), and the default is `false` (`config.rs:457-460`).
  So `configure.rs:4274-4289` skips `reconcile_borrowed_snapshot_with_disk`.
- `runtime_drain.rs:2559`: the watcher only applies events to the RAM index
  when `!read_only || ram_overlay_active()`. Without the overlay, local edits
  in the borrower never reach the index either.

### 2.4 Why the #337/#338 fix doesn't apply

`SearchIndex::reconcile_borrowed_snapshot_with_disk` (`search_index.rs:2064-2169`)
works for any borrower. It walks this root, drops snapshot paths that are
missing, re-indexes stale ones, adds new ones, and never writes `cache.bin`. The
control test shows it fixes shared clones too. It is gated on the opt-in RAM
overlay flag rather than on "this root borrows", and the flag's docs
(`config.rs:445-455`, `docs/config.md:166-175`) describe it as a
linked-worktree edit overlay, not a correctness requirement.

### 2.5 Related ownership hazards (from reading the code, not reproduced)

- Clones and copies aren't linked worktrees, so they register
  `configure_artifact_access(..., borrow_only_shared = is_worktree_bridge = false)`
  (`configure.rs:3440-3444`) even while `ReadOnly`. The owner gate still
  blocks search persistence (`configure.rs:4237` returns early for read-only
  roots). Other shared-key writers that consult only `ArtifactAccess` wouldn't
  see this root as borrow-only. Not audited.
- If a clone configures while the recorded owner's pid is dead (for example first
  root bound after a daemon restart), it claims ownership. The writable load
  sees a HEAD mismatch and rebuilds from its own disk, which is correct for the
  clone, then publishes the older tree as the shared `cache.bin`. The live
  checkout, configured later, finds a live owner with a different
  `git_common_dir` and borrows the older snapshot. The phantom problem is then
  reversed: the live checkout is missing its new files. This could explain the
  1-in-8 run that didn't reproduce and is a correctness risk for the live
  checkout itself. Not reproduced here, because in-process tests can't make
  the owner pid dead.

## 3. Blast radius

Affected checkouts: any checkout that isn't the owner and whose HEAD shares the
root commit. That includes shared clones, plain clones, `cp -R` copies and
linked worktrees at any other commit or branch (all four reproduced). It also
covers the same commit with different uncommitted or untracked files, since
the snapshot includes the owner's dirty state. Only `worktree.ram_overlay: true`
avoids it today.

| surface | phantom paths? | stale content? | basis |
|---|---|---|---|
| `glob` | **yes**: index file list returned without an existence check (`glob.rs:228-251`) | n/a | reproduced |
| `grep` (indexed) | no: candidates are re-read from this checkout's disk, so missing files never match | **false negatives**: postings come from the owner's content, so text only this checkout has is never a candidate (`OLDER_COMMIT_TOKEN` → 0, `Ready`). No false positives: the live-only token → 0, because the disk re-read verifies | reproduced |
| `aft_search` path / lexical lanes | **yes**: `path_lookup`/lexical returned `extra/phantom.ts` | **false negatives** as for grep | reproduced |
| `aft_search` semantic lane | results in the in-root hybrid handler are filtered with `is_file()` (`semantic_search/mod.rs:3549`, `3573`, `3751`), so phantom files are dropped | the borrowed `semantic.bin` (`readonly_artifacts.rs:284`) was embedded from the owner's content, so ranking (and any stored snippet) reflects the live version | code reading; semantic lane disabled in the tests |
| `outline` / `zoom` / `read` | no: they read disk, so a phantom path fails like the report's `read` | no | code reading + report |
| symbol cache prewarm | tries phantom files from the borrowed file list (`configure.rs:4290` `search_index_symbol_files(&index)`), which fail harmlessly | symbol-backed lanes follow the file list | code reading |
| callgraph | only search and semantic have read-only borrow openers (`readonly_artifacts.rs:169`, `284`). Borrow-only roots don't cold-build the callgraph (see the configure test that linked worktrees must not schedule one). Whether a borrower answers from the owner's store wasn't traced | unknown | not verified |

Worst case in practice: a pinned spec-merge checkout. The agent sees files
that don't exist in its checkout (glob), and doesn't see code that does exist
(grep and search false negatives on content that differs from the live tree).
Both come back with `status: Ready` and `complete: true`.

## 4. Fix options

Costs below assume a large repository (N files, D files that differ between
the borrower and the snapshot, usually D ≪ N).

### A. Borrow only when HEAD matches

At the borrow point (`readonly_artifacts.rs:228` or `configure.rs:4264`),
compare `index.stored_git_head()` with `current_git_head(root)`. On a mismatch,
don't adopt the snapshot. Either build a private index or serve the fallback walk.

- Cost: one `rev-parse` per bind. Very cheap.
- Correctness: partial. Matching HEAD doesn't mean matching files: the snapshot
  includes the owner's uncommitted and untracked files, and the borrower has its
  own. It also stops sharing for exactly the case sharing was built for:
  a linked worktree on a feature branch.
- Cold build: every pinned checkout at a different commit pays a full private
  build (O(N) read + index) or runs on walk-fallback grep. Disk: a private
  cache per checkout if persisted.

### B. Reconcile the borrowed snapshot against this checkout (tree diff)

Compute the set of paths to re-verify from git instead of a full walk:
`git diff --name-only <snapshot HEAD> HEAD`, plus this checkout's dirty and
untracked paths (`git status --porcelain`), plus the owner's dirty and
untracked paths at publish time. The snapshot doesn't record that last set
today, so the owner would have to add a small "paths that differ from my
HEAD" list to the `cache.bin` header. Then fold only those paths into the RAM
delta, reusing the `reconcile_borrowed_snapshot_with_disk` apply code.

- Cost: O(D) file reads plus git's tree diff, which is fast. No O(N) walk and
  no rehash.
- Needs a cache format addition (owner dirty set). Without it, owner-dirty
  files are a blind spot unless they are stat-verified.
- Content hashes are blake3, not git blob ids (`search_index.rs:965`), so they
  can't be compared with `ls-tree` directly. The diff has to come from git.
- Watcher updates must also be applied (as the overlay does), or the borrower
  drifts again after bind.

### C. Existence filter on results (backstop only)

Drop result paths that don't exist on disk in glob and in the `aft_search`
path and lexical lanes, as the semantic results already do.

- Cost: one stat per returned result. Negligible.
- Fixes only the visible phantom-path symptom. Grep and search false
  negatives on changed content stay. Worth adding anyway as defense in depth.

### D. Key identity on the checkout, not the repository

Put the checkout (path, or git dir) into the artifact key, so every checkout
owns its own index.

- Cost: removes sharing completely. Every clone and worktree cold-builds
  (O(N) read + trigram build, then semantic embedding: minutes of CPU on
  big repos) and stores a full `cache.bin` + `semantic.bin` per checkout
  (disk × number of checkouts). This undoes the purpose of the borrow design
  (worktree fan-out for Mason workers).
- Also fixes the reversed-ownership hazard in 2.5.

### E. Always reconcile borrowers (turn the existing overlay machinery on for every borrow-only root)

Change the gate at `configure.rs:4201-4202` (and the watcher gate at
`runtime_drain.rs:2559`) from `ram_overlay_active()` to "this root is a
borrower". In effect, `worktree.ram_overlay` becomes the default for
correctness, while any memory-saving opt-out keeps honest disclosure. Grep
stays on the fallback walk (status `Building`) until reconciliation delivers,
which the code already does.

- Cost: at bind, a full walk (O(N) stat) plus `StatFirst` verification. For
  clones and copies every file's mtime differs from the owner's, so verification
  falls through to hashing, which reads all N files once. That is comparable to a
  cold trigram build's I/O but without rebuilding postings for unchanged files.
  RAM grows with D (the overlay delta). No extra disk. This code is already
  written and tested (`ram_overlay_worktree_indexes_changes_that_predate_the_process`
  and the control test here).
- Linked worktrees usually keep mtimes for untouched files, so the stat path
  mostly short-circuits there.

### Recommendation

**E now, with C as a backstop, then B as the optimization.**

1. E closes the correctness hole for every borrower kind with code that
   already exists and is tested. The `phantom_paths_*` tests should turn green
   and be un-ignored. It's a two-gate change plus deciding what
   `worktree.ram_overlay: false` means afterwards (an explicit "accept a
   stale borrow" opt-out that must be disclosed in the index status, or
   removing the flag).
2. C (existence filter in glob and in the `aft_search` path and lexical lanes)
   guards against any future borrow path that skips reconciliation. It is not
   the fix.
3. B replaces E's O(N) bind cost with O(D) once the owner records its dirty set
   in the snapshot. It matters for very large repositories pinned at many
   commits (the spec-merge fan-out).
4. Separately, decide on the ownership hazard in 2.5. Suggested direction:
   only a checkout whose own `git_common_dir` matches the family (the
   repository's main checkout) may claim the shared owner role, and clones and
   copies always borrow. Alternatively, key clones separately. This needs its
   own reproduction (dead-pid owner manifest) before any change.

A (HEAD match) is not recommended as the primary fix. It is both too strict
(no sharing across branches) and too weak (dirty state). D is correct but gives
up the sharing design and costs a full build plus disk for every checkout.

## 5. What was built, and what it costs

- **E:** `worktree.ram_overlay` defaults to true, so every borrow-only root runs
  `reconcile_borrowed_snapshot_with_disk` before search reports ready, and its
  watcher events reach the RAM delta afterwards. Borrow-only roots already get a
  file watcher like any other root (the configure maintenance `Watcher` stage
  does not look at the owner mode). Only the events were being thrown away. The
  key stays as an escape hatch (`false` serves the snapshot as it is).
- **C:** glob and every `aft_search` route that goes through the engine ranking
  drop returned paths that are not on disk. They make one `stat` per returned
  path and report the count as `missing_on_disk_dropped` plus a note in the text
  and a debug log. Glob refills its page from the next matches (at most 256
  extra `stat`s). `aft_search` shortens the page instead of refilling it, so
  later pages keep their offsets and present files keep their rank. Grep reports
  indexed candidates it could not read because they were missing
  (`missing_on_disk_dropped`, `complete: false`). With the escape hatch, glob's
  `total` can still count stale entries after the page, because only the page
  is checked.

Tests: `phantom_paths_*` (default config, all four checkout kinds, plus the
explicit-on control), `phantom_paths_backstop_drops_missing_paths_when_reconcile_is_off`
and `borrowed_checkout_edit_after_bind_is_found_by_grep` (real watcher).

Measured cost (release build, isolated storage dir, owner and borrower as two
`aft` stdio processes). The live checkout is a shared clone of this repository
at `0dcebd7eb` (3,947 files). The borrower is a linked worktree 300 commits
older (3,834 files, 439 files differ). Three rounds each:

| | overlay off (escape hatch) | overlay on (default) |
|---|---|---|
| configure to search `Ready` | 0.36-0.41 s | 1.01-1.12 s |
| reconcile (log) | none | walked=3798, hashed_unchanged=3473, reindexed=319, added=6, removed=113; walk 107-118 ms, verify 30-33 ms, apply 523-548 ms |
| borrower RSS at ready | 58.4-59.5 MB | 82.2-85.9 MB (+23-27 MB) |
| trigram estimate (`status`) | 2.6 MB, no delta | 14.6 MB: delta 5.85 MB packed, 975,004 postings over 66,869 trigrams, 432 superseded files |
| glob `**/*` total | 843 (includes the owner's files) | 809 |

A new linked worktree gets fresh mtimes, so every file is hashed
(`hashed_unchanged`). Most of the time goes to re-indexing the 319 changed files
into the delta.

`~/Work/OSS/opencode` (7,060 files) could not be measured this way. Its
borrowed load stops at the existing `BORROWED_INDEX_LOAD_MAX_RECORDS` budget
(100,000 file plus lookup records). The borrower logs `search index is
read-only and the shared snapshot was not adopted
(borrowed_search_index_load_budget)` and serves grep from the fallback walk
(status `Fallback`) with or without the overlay. This repository's snapshot has
94,724 lookup entries, just under that budget. So larger repositories never
borrow today, and neither the phantom paths nor the reconcile cost reach them.
