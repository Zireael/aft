# Daemon Malloc Small investigation — September 2026

## Experiment and safety boundary

The reported production observation is **25 GB footprint / 23 GB Malloc Small**
on PID 11567 after about 2.5 hours on 0.58.0. That observation is supplied by the
operator, not independently measured here. This investigation does not attach
to, pause, signal, or profile that process or its replacement PID 96581. The
operator later reported recurrence at 17 GB footprint / 17 GB dirty Malloc Small
with only 1.8 GB RSS after 2 h 13 min; that is additional supplied evidence, not
a measurement made by this harness.

The experimental binary is built from `7cf7345d6a2d09422af5acb5524248b88f7721e2`
(the task's initial HEAD and local `main`) using the symbolized `stage` profile.
No production implementation is changed for the experiment. A small broker
(`crates/aft/examples/malloc_soak.rs`) implements the authenticated subc handshake
and launches a **separate production AFT binary**, rather than running an
in-process simulation of the index code. The Python driver
[`malloc_soak.py`](malloc_soak.py) samples only that child's explicitly recorded
PID. Storage, user config, data and cache directories are private to the run.

The workload uses an independent clone of this repository and four linked git
worktrees of that clone, all inside a private `.malloc-experiment/` directory in
the task worktree.
Five editing sessions and three additional search-only sessions use those five
roots. The search-only sessions are ordinary first-party routes that receive no
write requests; this is not a test of principal-enforced read-only permissions.
The linked worktrees exercise production borrowed artifacts and RAM overlays.
The owner receives edits that can refresh the callgraph; borrowed callgraphs
are intentionally frozen by the production design.

Each active round writes a Rust function, performs a natural-language search and
requests Tier-2 inspect categories in every root, then searches from the three
additional sessions. The driver saves every wire response, including errors.
After 40 minutes of rounds (in addition to initial binding/cold scans), it closes
all eight tool routes, leaves only the passive management route, and observes
another 33 minutes with the normal **30-minute root idle TTL**. The root
directories stay present, so this measures idle artifact eviction, not deleted
root retirement.

`MallocStackLogging=1` is set **before child launch**. `footprint -p` and `heap -s`
are sampled every three minutes; `malloc_history -allBySize` every fifth sample
and after idle eviction; `leaks --noContent` at the end. Each subprocess has a
150-second timeout and its exit status and timestamp are recorded. Profiling
can perturb timings and footprint; performance-tool mappings are reported
separately from malloc regions.

### Deliberate limitations

- Embeddings come from a deterministic local HTTP server with 768 floats per
  chunk. Real parsing, chunk strings, vectors, snapshots, refresh and search code
  run; ONNX/model-runtime allocation behavior and a real remote service do not.
- Rust and other listed LSPs are disabled in the private config to avoid
  launching multiple native Rust builds on the shared machine. Biome was still
  auto-detected during inspect; no Rust LSP/build workload was driven.
- `semantic.max_files=10000` avoids artificially clipping this repository.
  Deferred-file behavior at a saturated cap is a separate case.
- The workload is a fixed set of roots, not indefinitely increasing root
  cardinality. A plateau cannot rule out retention proportional to new roots.
- Native allocation stacks identify allocation origins, not necessarily their
  current Rust owner. A zero-leaks result alone does not prove bounded retention.

## Reproduction

Run from an isolated task checkout on macOS, never from a production storage
root. Do **not** place the experiment below `target/` or `node_modules/`:
`watcher_path_is_high_churn_infra` checks absolute path components, so such an
ancestor suppresses every change event. A 20-minute preliminary run under
`target/` demonstrated cold index allocation but zero watcher traffic and was
stopped, not counted as a valid refresh soak. Use a fresh experiment directory
for every run (the broker creates git
worktrees and the sampler records the child PID there):

```sh
cargo build --locked -p agent-file-tools --profile stage --bin aft -j 4
cargo build --locked -p agent-file-tools --profile stage --example malloc_soak -j 4
git clone --no-hardlinks . .malloc-experiment/run/repo
python3 docs/investigations/malloc_soak.py .malloc-experiment/run
```

`AFT_SOAK_ACTIVE_SECS` and `AFT_SOAK_IDLE_SECS` can shorten a smoke run, but a
shortened run is **not** evidence of the requested hour-long soak or default-TTL
eviction. The broker uses bare wire tool names (`inspect`, `search`), not the
agent-facing `aft_` names. An initial smoke attempt caught that mismatch; a
second caught a cold `inspect` Tier-2 timeout under stack logging. The final
harness records tool terminal errors without aborting the whole soak; workload
coverage must be checked from the saved responses, not inferred from an exit
code.

The isolated scaling control uses one owner and ten worktrees, disables inspect
and callgraph work, and uses the minimum five-minute idle TTL:

```sh
git clone --no-hardlinks . .malloc-experiment/scaling/repo
AFT_SOAK_SCALING=1 python3 docs/investigations/malloc_soak.py .malloc-experiment/scaling
```

It records settled census snapshots after 0, 1, 3, 5 and 10 worktrees have edited,
then after ten additional edits per worktree, then every ten seconds for six
minutes after unbind. The measured run requested one minute, which the config
resolver clamped to five; it collected three minutes of idle census and retained
the child for three more minutes, recording all eleven evictions in the daemon
log. The checked-in harness now requests five explicitly and samples six minutes
so a subsequent run also gets a post-TTL census. `AFT_SOAK_BINARY` can point to a separate built binary;
the actual scaling run used the freshly linked counted binary while macOS
`dsymutil` was still generating its debug bundle. The target binary itself
already contained the census correction.

## Eviction paths checked in source

These are code-reading facts, not substitutes for the time-series measurement:

- `subc::reap_idle_roots` (`crates/aft/src/subc/mod.rs:1593`) requires zero bound
  routes, quiesced lifecycle, expired request age, and no active maintenance,
  bind, actor or artifact blocker for an existing directory. It can schedule
  teardown persistence for an unpersisted search delta or inspect completion.
- `AppContext::evict_idle_artifacts` (`context.rs:8462`) takes the callgraph,
  search and semantic handles, clears the borrowed-index cache, inspect caches,
  symbol cache and tsconfig membership cache.
- `InspectManager::evict_idle_caches` (`inspect/manager.rs:1323`) clears generation
  handles, callgraph projection, OXC facts and completed builder state.
- Existing-directory actors remain registered after their artifacts are evicted.
  `release_idle_reopenable_resources_in_background` (`context.rs:8507`) shuts
  down LSPs and clears database pools. Only confirmed deleted roots take
  `Executor::retire_idle_actor_in_background` (`executor/mod.rs:1043`). Thus
  **registered root count alone is not an artifact-residency count**.
- `App::memory_contexts` holds `Weak<AppContext>`, not strong index ownership
  (`context.rs:2187,2239`). Borrowed-index cache capacity is four entries per
  context (`context.rs:2467`), and idle eviction clears it. Shared semantic bases
  must be compared against the process census field
  `shared_semantic_base_bytes`, not summed independently for every borrower.

## Allocation-churn leads to compare with measured stacks

The following supplied leads are hypotheses, not diagnosed causes:

| Lead | Allocation/lifetime to look for | Coverage in this workload |
| --- | --- | --- |
| Semantic top-K | `SemanticIndex::search_filtered`, `semantic_index.rs:5964`: score vector reserves one slot per chunk, dropped on return | Natural-language searches |
| Borrowed path joins | Same function, `:5974`: constructs an absolute path per shared-base chunk before filtering | Borrowed worktree searches |
| Refresh full scans and reuse clones | `build_chunk_reuse_map`, `:4782`; `refresh_invalidated_files_with_blob_reuse`, `:5659`; clones matching embed text/vector | Repeated edits, owner and overlays |
| Deferred files at cap | Same refresh function, `:5636-5748`: merges deferred paths before parsing/admission | Not exercised at the chosen cap |
| Returned refresh delta clone | `:5808`: clones new entries before extending the worker's entries | Repeated edits |
| Lexical full ranking | `SearchIndex` ranking, `search_index.rs:701-847`: candidate and ranked vectors, sort then truncate | Combined search requests |
| Async watch backlog | `BgTaskRegistry::read_artifact_range`, `bash_background/registry.rs:698-718`: reads from offset to current end into a vector | Not exercised: no bash pattern watches |
| Symbol hit hashing | `TreeSitterProvider::extract_symbols_with_cache_status`, `parser.rs:1646`, and symbol-cache freshness checks | Inspect/callgraph parsing; no isolated hit-rate experiment |

A sampled stack absent at quiescence cannot exonerate transient allocation churn.
Conversely, seeing one of these stacks at peak does not prove a leak. Distinguish
live allocation totals, VM-region dirty bytes and census-owned data before
assigning a cause.

## Measurements and conclusion

### Retained allocation origin: full-base semantic materialization

The first completed active-workload `malloc_history` sample attributes
**1,623,662,704 live bytes** to stacks containing
`SemanticIndex::materialize_shared_base`. This is not a total of historical
allocations: `-allBySize` groups outstanding allocations. At the nearby `heap`
sample there were **3,790,778 malloc nodes / 2,143,522,576 bytes**. The samples
are not simultaneous, so the ratio is indicative, not an exact partition.

Largest individual malloc stack groups in that history:

| Bytes | Calls | Allocation stack (outer → inner) |
| ---: | ---: | --- |
| 689,400,320 | 192,355 | `runtime_drain::apply_watcher_slice` → `SemanticIndex::invalidate_files` → `materialize_shared_base` → `EmbeddingEntry::clone` |
| 551,520,256 | 153,884 | `spawn_semantic_refresh_worker` → `refresh_invalidated_files_with_blob_reuse` → `materialize_shared_base` → `EmbeddingEntry::clone` |
| 137,880,064 | 38,471 | `schedule_artifact_loads` → initial refresh-worker `SemanticIndex::clone` → `EmbeddingEntry::clone` |
| 86,975,040 | 192,355 | watcher invalidation → `materialize_shared_base` (another allocation within each entry clone) |
| 69,580,032 | 153,884 | refresh worker → `materialize_shared_base` (another allocation within each entry clone) |
| 64,634,880 | 5 | `drain_semantic_refresh_events` → `apply_refresh_update` → entry-vector growth |
| 64,634,880 | 5 | refresh worker → `refresh_invalidated_files_with_blob_reuse` → entry-vector growth |

The cold semantic corpus contains **38,471 chunks / 1,765 files**. The first two
counts are exactly **5 × 38,471** and **4 × 38,471**. One tiny edited function
per root caused whole-corpus copies in both the serving index and the refresh
worker, rather than only two new chunks per root.

The ownership path is explicit in source:

1. `materialize_shared_base` (`semantic_index.rs:4449`) takes the shared `Arc`,
   clones **every** `EmbeddingEntry`, rebases each path, and clones all freshness
   maps. This is a one-time conversion of a particular shared-base handle, not
   inherently a leak on every later query.
2. `invalidate_files` calls it before removing the changed file; refresh calls
   it again on its separate worker index (`:5633`).
3. `schedule_artifact_loads` creates the refresh worker with `index.clone()`
   (`configure.rs:5416`). Thus after editing a borrowed root, two independently
   owned complete indexes can exist for that root.
4. `AppContext::memory_estimates` reads the serving `semantic_index`, not the
   worker-thread-owned index. The worker copies are not explained by the root
   semantic census. The worker slot is a join handle, not an exposed index.

This establishes **reachable per-root duplication**, amplified by active editing
worktrees, as the largest observed allocation source. It does not establish that
all 23 GB in the operator's daemon has this origin, nor that the copies survive
unbind/TTL. That distinction is evaluated by the post-unbind samples below.

For the supplied performance leads, this sample has only 63,392 outstanding
bytes on `search_filtered` stacks and 107,936 on lexical-ranking stacks, versus
1.62 GB on full-base materialization. The small returned-refresh-delta clone is
not the same as this full-corpus copy. Transient query churn may still fragment
the allocator, but it is not the dominant live allocation in this sample.

### Fix proposal (not implemented)

Preserve the immutable semantic base through edits: maintain root-local
invalidated/tombstoned file identities and an owned delta; search base entries
excluding those files plus delta entries. The refresh worker should share the
same base and retain only its delta/reuse state. If a writer needs a complete
serialization, stream base-minus-tombstones plus delta instead of permanently
materializing both copies. This requires changes to invalidation, search,
refresh, persistence and memory attribution, so it is not a safe small patch to
bundle with a measurement-only investigation.

Add worker-owned bytes to the memory census (updated on install/refresh/drop),
and distinguish the shared base, serving delta and worker delta. Regression
coverage should bind an owner and multiple borrowers, edit one file in each,
assert unchanged corpus entries remain shared, compare results with an
independent full-index oracle, then unbind and expire the TTL and assert both
serving and worker memory disappear. A per-root soft/hard admission budget is a
separate mitigation for legitimately large active root counts; allocator relief
cannot free live duplicated vectors.

### Default-TTL release: measured, not inferred

The valid run used PID **57282**, launched **2026-09-28 19:00:07Z** and sampled
through **20:32:43Z** (92.6 minutes). All eight tool routes were closed at
**19:56:22Z**. Three active edit/search rounds completed after the cold setup;
stack logging and Tier-2 deadlines made them substantially slower than a real
fleet. There were 15 writes, natural-language searches through editing and
search-only routes, and 20 inspect requests. All 20 inspect requests returned
phase failures, but logs prove successful cycles/unused-exports/complexity/
duplicates scans and semantic watcher refreshes. A callgraph cold build ran;
it did **not** reach ready before unbind, so steady ready-callgraph refresh
coverage is missing. This is not a perfect reproduction of the full production
workload.

Selected native observations (tool MB are retained as printed):

| Elapsed | Phase | Footprint | Malloc Small dirty | Live malloc bytes |
| --- | --- | ---: | ---: | ---: |
| ~0 min | startup | 19 MB | 11 MB | see evidence JSON |
| ~12 min | five-root warm-up/materialization | 3448 MB | 3063 MB | see evidence JSON |
| ~15 min | active | 3146 MB | 2803 MB | 2,143,522,576 |
| ~31 min | active | 3306 MB | 2889 MB | heap timed out |
| ~45 min | active | 3077 MB | 2712 MB | see evidence JSON |
| ~86 min | unbound, just before eviction | 1650 MB | 1330 MB | 1,121,152,928 |
| ~89–92 min | after idle eviction | 536 MB | 256 MB | 47,000,432 |

The worker copies stop being retained after unbind; the serving copies remain
until the idle reaper. At **20:28:11Z**, all four worktree roots evicted semantic,
trigram, symbols and inspect data. The owner initially reported
`search_delta_unpersisted`, scheduled teardown persistence, and evicted at
**20:28:14Z**. The effective unbound TTL starts after teardown/request touching;
the census countdown reached zero roughly 32 minutes after the broker's
Goodbye writes. The last census had **5 registered roots, 0 bound routes,
0 semantic bytes on every root**, and only **290,520 total attributed bytes**
(compared with 840,223,449 just before eviction). Registrations remained because
the directories still existed; artifacts did not.

The post-TTL allocation history contains **no live `materialize_shared_base`
stack group**. The dominant residual malloc groups were Tier-1 todo/metrics memo
hash tables (2,768,896 and 2,260,992 bytes), empty retained symbol-cache table
capacity (1,327,104 bytes), and regex/filter state. Total live malloc was about
47 MB, not gigabytes. `leaks` reported **14,459 candidate leaks / 8,043,824 bytes**,
headed by hashbrown Tier-1 memo tables; this requires a separate reachability
check (Rust interior-pointer containers can confuse conservative scanners).
It does not explain the GB-scale semantic copies and is not claimed as a
confirmed unreachable Rust leak here.

About **244 MB** of the final 536 MB footprint was Performance Tool Data. The
remaining Small-region dirty bytes exceed the total live heap: residual allocator
fragmentation/other non-payload overhead exists, but does not account for the
multi-GB active-root peak. Dirty VM regions are not a per-allocation live-byte
census. The periodic allocator observation reported zero in-use/allocated under
this instrumented run, so its slack/relief counters are not used as proof of
absence of fragmentation.

The compact, timestamped measurements and selected demangled stacks are in
[`daemon-malloc-small-2026-09-evidence.json`](daemon-malloc-small-2026-09-evidence.json).
Raw profiler reports, daemon/broker logs and wire responses remain in the task
worktree's ignored `.malloc-experiment/run4/`. Five heavy `heap`/history samples
hit their 150-second timeout; these are explicitly marked, never treated as zero
allocated bytes.

### Do both semantic copies need to exist?

**Under the current algorithm, yes during refresh; not as deep independent
copies of every unchanged entry.** The serving copy is read by queries while the
worker invalidates old entries, reads/parses files, waits for embeddings, and
constructs updates. Giving both threads the same mutable index would introduce
races or block queries across backend latency, and could publish partial/failed
refreshes. The worker also retains old embed text/vectors for reuse while the
serving index has already removed invalidated files. The worker persists between
batches and both full copies are retained even when no batch is running. Sharing
immutable unchanged entries and only owning differing files preserves that
isolation without duplicating the entire corpus.

### Interim containment option

The smallest bounded-scope mitigation is **admission control for borrowed-root
semantic materialization**, not blindly deleting the worker index. Reserve a
per-process budget for both serving and worker private payloads before a root's
first invalidation/materialization. If unavailable, continue lexical search and
return an explicit semantic partial/unavailable state; do not serve stale
semantic results as fresh. Release reservations only when both actual owners
release their data. Costs: loss of semantic coverage for excess simultaneous
editing roots, admission/rollback races to test, and conservative budgeting of
allocator overhead. It avoids the broad persistence/search representation change
but still requires correctness tests; no speculative cap is shipped here.

Earlier release of the serving semantic index at last-route unbind would reduce
the 30-minute retention window, and the worker already exits on disconnect in
this measurement. Costs: losing warm-rebind performance and ensuring outstanding
query/build jobs have stopped. It would **not** bound memory while many roots are
still bound. Simply dropping the extra worker copy after each batch shifts the
copy churn to each next batch, and copying it again from the serving index can
lose the very invalidated vectors needed for embedding reuse.

### Census correction included

The shipped change adds a root-owned atomic byte total with a worker-owned RAII
contribution. Each actual worker counts its private index on creation and after
file/corpus refresh (including errors), and removes only its own contribution
on exit/unwind. No strong context/index reference is held by the census; retiring
and replacement workers can overlap without zeroing one another. Shared bases
remain process-attributed once. The semantic plane includes these worker bytes,
with a separate `semantic_worker_bytes` breakdown in the management response.
This fixes observability, **not** the duplication itself. Estimates omit capacity
and allocator overhead, transient in-progress buffers, and changes within a
still-running refresh until its mutation boundary completes.

### Scaling control: 1, 3, 5, 10 edited worktrees

A second isolated child, **PID 85938**, ran the counted binary with one owner and
ten worktrees (eleven bound roots). No inspect or callgraph work ran in this
control, isolating semantic retention. It had no tool errors. Its private census
payload is the sum of serving and worker indexes; the single shared base remains
150,885,778 bytes across all milestones.

| Edited worktrees | Serving private bytes | Worker private bytes (including owner) | Total private semantic bytes | Physical footprint bytes |
| ---: | ---: | ---: | ---: | ---: |
| 0 | 0 | 156,838,139 | 156,838,139 | 694,945,280 |
| 1 | 157,104,832 | 313,942,971 | 471,047,803 | 1,106,052,920 |
| 3 | 471,314,496 | 628,152,635 | 1,099,467,131 | 1,938,049,448 |
| 5 | 785,524,160 | 942,362,299 | 1,727,886,459 | 2,768,571,416 |
| 10 | 1,571,092,121 | 1,727,930,260 | 3,299,022,381 | 4,848,046,640 |
| 10, after 100 additional writes | 1,571,092,111 | 1,727,930,250 | 3,299,022,361 | 4,850,192,944 |

Each newly edited worktree adds about **314.2 MB of estimated private semantic
payload** (two copies), and about **415 MB of footprint** in this corpus/vector
configuration. The extra 100 writes did **not** increase retained semantic
payload; it decreased by 20 bytes because the replacement symbol/text lengths
changed. Footprint increased only 2,146,304 bytes, not another corpus per edit.
This is direct evidence of **root-cardinality amplification**, not unbounded
per-edit growth in the fixed-root test.

After unbind, all worker bytes were zero by the recorded idle snapshots; serving
private bytes were 1,571,092,111 and the shared owner base remained until TTL.
At **21:16:26Z**, the daemon logged eviction of **all eleven roots** with the
configured effective 300,000 ms TTL. The short run's last explicit census was at
180 seconds (before TTL); post-TTL census/native reclamation was independently
observed in the 92-minute default-TTL run above. Neither the worker counter nor
the weak base registry pinned copies in that run.

**Conclusion:** the experiment finds a large, reachable and multiplicative
retention problem: editing converts each borrowed semantic corpus into full
serving and refresh-worker copies, retained for the bound root and (for the
serving side) the idle TTL. Increasing the number of edited roots keeps increasing
this live payload; repeating edits on a fixed set did not. No evidence here
supports a 7–8 GB/hour intrinsic per-edit leak. The production rate can arise
from adding larger/more edited corpora inside their retention windows, but its
exact 17–23 GB composition was not measured and is not claimed proven. The
census correction is the immediate diagnostic improvement; the shared-base delta
representation is the proper memory fix.

## Verification and remaining limits

- Narrow debug tests: `cargo test --locked -p agent-file-tools --lib semantic_worker_census -j 4`
  (2 passed), plus `commands::memory_census::tests` (7 passed).
- `cargo check --locked -p agent-file-tools --example malloc_soak -j 4` passed;
  this also typechecks the production library. Rust formatting and Python
  compilation checks passed.
- Non-vacuity: replacing the actual worker attribution read with zero made only
  `semantic_worker_census_counts_refresh_and_releases_on_exit` fail at
  “a populated worker must be counted even with no serving index”; the
  retiring-generation test stayed green. Restoring the implementation made both
  pass. No existing test expectation was reversed.
- Optimized `--profile stage --lib` tests encounter a pre-existing
  `REQUEST_TIMEOUT_TEST_ENV` cfg error in `gh_shim_relay_client_tests.rs`.
  A temporary diagnostic workaround was restored and is not included. Final
  regression verification uses the normal debug test profile. Packed dSYM
  generation also exceeded the optimized build deadline under host load; the
  linked binary was used for the scaling control, not claimed as a completed
  distribution build.
- AFT diagnostics repeatedly timed out in LSP quiescence or were interrupted by
  its own daemon restart. Cargo checks above are the authoritative compilation
  results. No package manifests or lockfiles changed.
- Fixed-root plateau and successful release are not a universal no-leak proof.
  Real-model allocation, saturated semantic file caps, bash pattern-watch
  backlogs, growing root cardinality beyond ten, and successful steady-state
  callgraph refresh need separate controls if production remains unexplained
  after the new worker census is deployed.
