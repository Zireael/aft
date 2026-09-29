# Semantic overlay review revision

This revision preserves the shared-base/delta algebra and V7 serialization from
[the initial delivery](semantic-shared-overlays-2026-09.md), while correcting the
review findings below. The initial delivery's late-donor-delta policy is
**superseded** by the receiving-checkout policy here.

The complete overlay was rebased onto local main
`85d8e83e8849765d1ade599cb8924b47e5997ba8` (train 235), including its live-config
lifecycle changes. The rebase applied without conflicts in configure/context;
no live-config behavior was removed. All commands use
`CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=` and native builds are serialized.

## Constant-time base file membership

`SharedSemanticBase` owns a precomputed `HashSet<PathBuf>` of relative file
identities present in embedding rows. `hide_base_files` checks this set and the
mtime map instead of scanning every embedding row for each newly added file.
Files with rows but missing mtime records remain covered. The set's path payload
is included in the process-shared base estimate, not charged to each borrower.
No on-disk fields were added.

The ignored test
`semantic_index::tests::revision_new_file_batch_has_bounded_serving_lock_time`
constructs a **1,000,000-entry immutable base**, including a file without mtime
metadata, and invalidates **512 newly added file paths**. It measures the actual
`invalidate_files` work performed while the caller holds the serving write lock;
fixture construction, freezing, and drop are outside the timed interval.

On the same debug test profile:

| Implementation | Timed invalidation work |
| --- | ---: |
| Restored per-path linear scan | 34.9703195 s |
| Precomputed file membership | 15.205834 ms |

The old scan trips the test's 500 ms bound; the fixed code also proves that the
entry-only file can be tombstoned. This is a local microbenchmark on a loaded
host, not a production latency guarantee. Hash-set lookup removes the product of
batch size and corpus entry count; path canonicalization and owned-delta retain
work remain.

## Borrowing provenance and receiving-checkout freshness

A resident donor must be the **artifact writer context**, have the matching
artifact-family cache key and semantic backend fingerprint, and be ready with
no in-flight refresh. Another borrow-only worktree is never eligible. In this
code the writer capability is exposed through
`!context.shared_artifacts_read_only()`; matching a family key alone is not
sufficient.

Even the writer's root-local delta is not copied into another checkout. A late
borrower starts from the writer's immutable base only. Before exposing rows,
both resident adoption and borrowed disk-load installation:

1. Walk the receiving root's eligible semantic source files using the existing
   ignore-aware bounded walker.
2. Strictly compare recorded content hashes against those files. Equal mtimes
   and sizes across checkouts do not establish equal contents.
3. Include added files, deleted files, and tombstones in the difference set.
4. Hide differing inherited rows before publication. With a RAM overlay enabled,
   queue changed-file refreshes immediately; no watcher event is required.

Borrow-only roots still cannot request corpus refresh. With RAM overlays
disabled, differing rows remain hidden rather than serving another checkout's
symbols. If the receiving walk exceeds `semantic.max_files`, adoption fails
explicitly instead of publishing an unverified snapshot. The disk installation
path uses the same check, so rejecting a resident donor cannot bypass freshness
through a disk fallback.

Tests cover donor eligibility with a writer-positive control, rejection of an
owner's existing delta, same-size/same-mtime different contents, added and deleted
files, configure-time adoption of a late worktree whose files differ from the
owner, and borrowed disk installation without watcher events. Independent local
refresh is compared with an index built from the borrower's own files.

The previous test asserting inherited donor deltas was deliberately changed:
that assertion described the policy the review rejected. The new test requires
zero inherited delta entries and metadata, keeps the same base Arc, and verifies
local invalidation does not alter the owner's delta.

## Shared generations and pending-install census

The weak shared-base registry already keys by artifact-family key, fingerprint,
and artifact content hash. It now allows multiple such generations to coexist.
A live old generation no longer forces later borrowers into private full-corpus
loads. A second borrower of the rebuilt generation joins its Arc; an old
borrower retains its original vectors until it drops the old Arc.

The loader moves parsed vectors into the immutable base rather than cloning the
entire private load first. It verifies the disk fingerprint/content hash again
before registration so an atomic owner replacement between identity sampling
and reading cannot register new rows under an old key. A concurrent replacement
causes this load to decline; a later load can retry the current generation.

The old hash-change test's private-copy expectation was deliberately replaced.
The regression changes both vector bytes and model fingerprint, keeps the old
borrower live, loads two new borrowers, checks their shared/new allocation and
scores, and proves the old base drops independently.

Pending-install attribution is now sampled **after** `fork_for_refresh` freezes
the corpus. The shared registry owns the once-per-process base charge; the
queued serving index contributes only its remaining private bytes. A populated
768-dimensional fixture demonstrates that the pre-freeze count is nonzero but
the frozen worker and queued serving view both own zero private payload.

## Reader and completion audit

### C2: live length and dimension validation — already correct

`len()` delegates to `entry_count()`, which counts non-tombstoned base entries
plus owned delta entries. Both incremental dimension guards use this live view.
A new regression freezes a nonempty three-dimensional index (its private
`entries` is empty), adds a file through a two-dimensional embedder, and requires
an error with the original snapshot intact. No dimension-guard production change
was needed.

### C3: direct field readers and tombstone recovery — already correct

Production direct-field uses were inspected in `semantic_index.rs`:

- Base-only accounting and private-only construction/deserialization intentionally
  read their owned fields.
- Root accounting intentionally measures delta-owned fields, not inherited bytes.
- Search, serialization, reuse, structural persistence comparison, freshness, and
  full-corpus refresh enumerate the live base-plus-delta view or use
  `metadata_value`/`indexed_paths`.
- `remove_indexed_files` calls `remove_indexed_file_keys`, which hides matching
  base files before retaining/removing owned entries and metadata. It does not
  merely remove the delta.
- `semantic_paths_with_recorded_freshness` calls
  `recorded_file_freshness`, not the private maps. A tombstoned file without a
  local replacement returns no record.
- `files_with_changed_content` treats a missing record as changed, so even a
  tombstone whose disk bytes did not change remains recoverable.

The tombstone regression hides an unchanged file, verifies zero live entries and
no recorded freshness, verifies the file survives changed-content filtering,
and refreshes it back into the live view. The drain test also exercises the
watcher helper against a tombstone. No further reader rewrite was necessary.

### Same-worker stale completions — real and fixed

Receiver generation/epoch fences prevent a retired worker from publishing, but
they do not distinguish an older batch from a later edit within one live worker.
A completion could previously resurrect the first edit after the watcher had
hidden it for the second edit.

File completions now verify the returned freshness records against current disk
content before applying rows or acknowledging paths. Stale/ignored rows and
metadata are removed from the completion; newer pending paths are not cleared,
and eligible stale files are queued again. Verification occurs before acquiring
the serving index write lock. The existing worker-generation fence is retained.
Oversized files are a deliberate empty-result exception: extraction skips files
above the hash/parse cap and returns metadata with no rows or hash. A matching
size/mtime may acknowledge that empty completion; requiring a hash would queue
the same skipped file forever. A dedicated regression verifies no requeue and
retention of its metadata. No nonempty completion uses this exception.

Corpus completions verify their full logical snapshot against current files
before publication, not merely the pending-path set: the newer watcher request
may already have been dispatched and no longer be pending. Differences are
masked and replayed. A verification failure does not publish that snapshot.
Separate tests exercise file and corpus completions from the same worker after
a second edit, then complete the second edit successfully. The corpus test has
no pending-path shortcut, so it specifically detects missing full-snapshot
verification.

## Named red controls

The live source files were staged and an empty unstaged diff confirmed. The
reviewed defects were then restored temporarily under an explicit break marker.
The applied diff was **4 files, 59 insertions, 16 deletions**. Each test below was
run by its full exact name in a separate invocation; each invocation ran one
test, and only that named test failed. No test expectation was changed for the
mutation. The temporary unused-variable warning is from disabling the stale
filter, not from restored production code.

All test names below are under `semantic_index::tests`, except where a full
module prefix is shown.

| Restored defect | Named failing test | Captured failure |
| --- | --- | --- |
| Linear base scan | `revision_new_file_batch_has_bounded_serving_lock_time` | `new-file batch scanned the base: 34.9703195s` |
| Private fallback for new generation | `revision_rebuilt_generation_is_shared_while_old_base_is_live` | `new generation must stay shared` |
| Pre-freeze queued estimate | `commands::configure::tests::revision_pending_install_counts_only_post_freeze_private_bytes` | `pending-install must not recount the frozen corpus` |
| Borrower allowed as donor | `context::subc_lifecycle_admission_tests::revision_borrower_is_not_a_resident_semantic_donor` | `another borrower must never donate branch-local state` |
| Owner delta inherited | `revision_late_borrower_starts_from_owner_base_not_owner_delta` | `owner checkout deltas must not cross roots` |
| Resident adoption not verified | `commands::configure::tests::revision_late_adoption_masks_different_checkout_before_queries` | `different-tree donor rows must be masked before queries` |
| Disk adoption not verified | `runtime_drain::tests::revision_borrowed_disk_ready_masks_different_checkout` | `disk adoption published another tree's rows` |
| Stale file batch accepted | `runtime_drain::tests::revision_same_worker_stale_batch_cannot_resurrect_second_edit` | `stale completion resurrected first edit` |
| Stale corpus accepted | `runtime_drain::tests::revision_same_worker_stale_corpus_cannot_resurrect_second_edit` | `stale completion resurrected first edit` |

Every red ended with `test result: FAILED. 0 passed; 1 failed`. The four files
were restored using `git checkout -- <paths> && touch <paths>`, and the unstaged
diff was empty afterwards. The restored revision tests, ignored large-base
benchmark, and serial semantic suite passed. Raw output is retained in
`target/overlay-evidence/revision-red-{0..8}.log` and the machine-readable summary
in `target/overlay-evidence/revision-mutations.json`.

## Verification notes

A parallel broad semantic run tripped the existing
`cold_configure_starts_callgraph_before_semantic` test: its global event capture
included unrelated fixture roots. The isolated test passed, and the full serial
semantic suite passed. No ordering assertion was weakened. Scoped AFT inspection
returned fresh completion but explicitly reported no authoritative LSP reports
for these files; native Rust checks are the typechecking authority.

The additional oversized-empty-result control disabled its no-row exception.
`runtime_drain::tests::revision_oversized_empty_completion_does_not_retry_forever`
then failed alone with `unchanged oversized skip was requeued`. Its applied diff
was one insertion/one deletion in `runtime_drain.rs`; the staged state was
restored to an empty unstaged diff. The subsequent restored revision suite passed.

Final verification on train 235:

- `cargo test --locked -p agent-file-tools --lib revision_ -j 2`: **14 passed,
  1 ignored** (the filter also includes three existing revision-named tests).
- `cargo test --locked -p agent-file-tools --lib
  revision_new_file_batch_has_bounded_serving_lock_time -j 2 -- --ignored
  --nocapture`: **1 passed**, 15.205834 ms timed work.
- `cargo test --locked -p agent-file-tools --lib semantic -j 2 --
  --test-threads=1`: **330 passed, 3 ignored**.
- `cargo test --locked -p agent-file-tools --lib config_live -j 2 --
  --test-threads=1`: **32 passed**.
- `cargo check --locked -p agent-file-tools --lib -j 2`: passed.

Compact timing, checks, and exact named mutation outputs are in
[`semantic-shared-overlays-review-2026-09-evidence.json`](semantic-shared-overlays-review-2026-09-evidence.json).

## Final ranking fence

The complete `engine_unwired` search-quality gate passed on the train-235-based
revision, including exact recall, the 26-row concept replay, the 49-row paged
real-query replay, and page-size invariance. Reference files were not changed.
The explicit canonical-byte comparison of all ranked rows also passed:

- **49 rows; 274,761 canonical bytes**.
- Reference and candidate SHA-256 are both
  `37aaacf461fba56d0d023a5fbc7b8dc79cdc4cd09ac40306d1540b0aeadf8268`.
- Executable SHA-256:
  `962a051accfa710d084f4d7806dbef3c47fa540426a3017257bb914fbfd3c173`.

```sh
CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= \
  CARGO_PROFILE_STAGE_DEBUG=0 CARGO_PROFILE_STAGE_SPLIT_DEBUGINFO=off \
  cargo build --locked -p agent-file-tools --profile stage --bin aft -j 2

CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= \
  scripts/telemetry/cost-gate.sh --search-quality --mode evaluate \
  --descriptor "$PWD/target/overlay-evidence/descriptor.json" \
  --base-ref 85d8e83e8849765d1ade599cb8924b47e5997ba8 --head HEAD \
  --binary "$PWD/target/overlay-evidence/aft-revision-final" \
  --score-output "$PWD/target/overlay-evidence/train235-ranking/score.json" \
  --ready-timeout 1800
```

The descriptor is the unchanged `engine_unwired` descriptor from the initial
verification. The copied executable contains source from `9af8a48c3`; the
subsequent documentation commit does not change executable behavior. Earlier
pre-rebase or superseded verification chains were stopped and are not counted
as final passes. The local `main` branch itself was not modified: integration
was performed in the isolated task branch for reviewed delivery.
