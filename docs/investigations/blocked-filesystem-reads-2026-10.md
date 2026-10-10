# Blocked filesystem reads and same-root binds (2026-10-07)

## Evidence and limits

The incident is in `aft-72545.log`, around 04:48–05:00Z. The session root was
`/Users/ufukaltinok/Work/Projects/CortexKit/subconscious`; the explicit grep target
was `/Users/ufukaltinok/Downloads/subconscious-bug-report.md`. Downloads access was
not granted to the daemon. A macOS consent-blocked `open()` is a plausible cause,
not a kernel stack trace established by this log.

Relevant log lines:

- **5661, 04:49:51.191Z:** `job=subc-24-1045 command=grep lane=PureRead state=executing age_ms=60074`.
- **5710, 04:50:23.657Z:** bind 193 reported
  `configure_phase_timings=[config_resolve=0ms,canonicalize=0ms,worktree_probe=0ms,requeued_exclusive=6153ms]`,
  `waiting_on_readers_stuck(job=subc-24-1045 command=grep lane=PureRead age_ms=92540 execution_started=true started_before_oldest_writer=true)`,
  and `rerun_as_exclusive_writer`.
- **5715–5716, 04:50:28.207Z:** route 193 was rejected with
  `bind_blocked_by_reader` after 10,704 ms against its 10,500 ms deadline; the
  grep reader was 97,090 ms old.
- **5893–5896, 04:54:30–35Z:** the same grep was still blocking route 221,
  reaching 343,959 ms at rejection.
- **5708, 04:50:21.353Z:** route 196 bound with `harness=runner`.
- **5725–5727, 04:50:34–35Z:** route 197 used `runner`, followed by route 198
  with `harness=opencode` on the same root.

The log establishes a stuck reader and exclusive requeues. It does **not** record
an effective-config diff or artifact residency for each of the 21 failed binds.
No particular incident route can therefore be attributed to artifact-only repair
from this evidence. The runner/opencode transitions are genuine changes under
AFT's current root-harness contract, not no-op binds eliminated by this patch.

## 1. Kernel I/O cannot retain the root indefinitely

Previously, a deadline checked before a synchronous filesystem call could not
interrupt that call. The executor holds the root's shared epoch guard until a
PureRead handler returns (`executor/mod.rs:4002–4015`), so an `open()` waiting for
consent retained both reader admission and an executor worker.

The initial implementation ran every filesystem call on a new helper, including
each grep candidate's read and mtime probe. That preserved deadlines but imposed
per-file thread/channel costs, and its immediate 64-thread admission rejection
incorrectly called healthy concurrent work blocked. The revision bounds first
access to a protection/filesystem boundary instead.

`bounded_io::command` probes target roots before their initial stat/canonicalize;
`directory` records one accessible/risky result per directory for the request.
(`bounded_io.rs:152,319`). Shared single-flight probes prevent concurrent Rayon readers from repeating the
same probe. Each worker remembers successful directory lookups locally. Directory
probes open and advance `ReadDir`, rather than merely checking existence. A walk
probes directories before descending; nested probes use the walk's already
isolated helper when its outer deadline is at least as strict. Normal regular
files beneath an accessible local directory are read directly, without a helper
thread or channel round trip per file.

Unknown, network/FUSE, removable macOS mounts, cloud-provider directories, and
symlink reads remain isolated per file. Classification is conservative: only
positively identified local filesystems use direct reads. Non-regular files are
excluded before open; `O_NONBLOCK` and a descriptor-type check also prevent a
regular file replaced by a FIFO after admission from parking a direct reader.
`O_NOFOLLOW` prevents a late symlink replacement from crossing the accessible
boundary on the direct path; that race is retried under isolation.

`run_with_limiter` (`bounded_io.rs:264`) waits until the smallest of the five-second OS-call bound,
request budget, and supplied scan deadline. Timeout abandons the receiver without
joining; no helper owns an actor, epoch guard, or completion token. Its 64-live-
thread limit includes abandoned calls until completion. Healthy saturation waits
on a condition variable within the deadline. An admission timeout reports
`read_io_queue_timeout` and explicitly says the filesystem call did not start;
it does not claim blocked OS access or missing permissions. An already-expired
budget reports `read_deadline_exceeded`. Only an executing helper timeout reports
`read_blocked`.

Protection boundaries are checked inside as well as outside the project: a
project can itself be on a removable/network volume, or an in-root symlink can
reach a protected path. The fast path assumes a positively identified accessible
local directory remains on that local filesystem during the request. The cache
is request-scoped, not a permanent permission grant. Network/removable/cloud
locations do not gain direct-read eligibility merely because a directory probe
succeeded.

The shared helper covers local read stat/open/streaming/directory/media work
(`commands/read.rs:1385`), hashline source capture, parser source reads used by
outline, outline discovery (`commands/outline.rs:2179`), grep/glob discovery,
path resolution, authorization probes, and listing/mtime probes. Authorization
fails closed on timeout and does not memoize a timed-out lexical root identity.
Only owned filesystem work is detached, not an entire command that could later
publish actor state.

The FIFO regression `blocked_open_returns_before_fifo_writer_arrives` exercises
the bounded helper's real kernel open. The grep and executor regressions inject
a real FIFO open into the first-access directory probe for a regular target;
they require that the actual probe fired, so skipping the helper cannot produce
a spurious pass. `blocked_file_read_releases_root_reader_and_executor_slot`
(`executor/tests.rs:32`) still submits a real read and a writer on one executor
root and requires both responses before a FIFO writer arrives. Tests release the
FIFO on failure. Separate tests reject stale FIFO index entries and a FIFO
replacement after regular-file admission. The timeout tests were moved from
opening FIFO search targets to the directory probe deliberately: non-regular
search targets are now excluded, while permission-blocked first access must
still be bounded.

## 2. Grep's deadline reaches the blocking operation

The base already gave explicit files a ten-second deadline
(`grep_executor.rs` at base `0a6eeb56f`, lines 616–631), but only the search loops
observed it. `fallback_search_file` called synchronous `read_searchable_text`
at base line 1406. Indexed verification likewise read synchronously at
`search_index.rs` base lines 3328 and 3664. A deadline after an unreturned open
was not a bound.

The delivery forwards the deadline into the filesystem helper:

- **Explicit file:** `grep_executor.rs:627–675` supplies the deadline to the
  common file verifier; its actual read is at **1545**.
- **Fallback/explicit ignored directory:** the walk is isolated at
  `grep_executor.rs:1156`, and its file verifier gets the common scan deadline
  created at **1422**. The glob fallback uses the bounded walk at **1067**.
- **Indexed candidate verification:** `search_index.rs:3680,5794` passes
  `GrepScanDeadline.at` to the read. Timeout marks the scan incomplete and is not
  counted as a missing-on-disk index entry. The alternate indexed examination
  path forwards its budget at **3333**.
- **Whole request:** `bounded_io.rs:152` starts the grep budget before scope
  resolution, so metadata/probes cannot reset it per root or per file.
  `search_index.rs:3901` clamps an indexed scan to that request deadline.
  Rayon verification inherits the request's budget/error state
  (`search_index.rs:3118,3320` and the fallback verifier), so a swallowed local
  I/O failure cannot become a complete zero-match tool answer.

`grep_scan_deadline_bounds_blocked_file_open` proves the fallback verifier's
80 ms scan budget, not merely the shared five-second bound. The indexed test
`indexed_grep_scan_deadline_bounds_blocked_file_open` indexes a normal file and
then blocks its first-access directory probe with a FIFO. It exercises the
verification boundary, not walk admission. Its companion
`indexed_grep_never_opens_a_stale_fifo_entry` verifies exclusion instead of
opening a stale non-regular entry. Mutation proofs must neutralize the revised
helper's deadline enforcement: a scan's request scope and explicit deadline now
both bound the same probe, so deleting only one redundant deadline input is no
longer a valid negative control.

## 3. Identical-config artifact repair is not a root mutation

A bind already starts with a shared epoch hold (`executor/mod.rs:4021–4038`)
and asks for an exclusive rerun only when configure needs one. The unnecessary
promotion was in configure's artifact-residency predicates:

- At base `configure.rs:1589–1628`, the early fast path required configured
  artifacts to be resident or loading.
- At base `configure.rs:3275–3284`, the later equivalent-config/warm-key path
  also rejected missing trigram/semantic artifacts, falling through to the
  exclusive rerun at base **3406–3409**.

An eviction changes neither root configuration nor namespace. Identical binds
now keep the existing root/harness/config, topology, lifecycle, and runtime-health
checks but admit missing trigram/semantic reloads without publishing a new root
configuration. Both paths queue reload start gates in existing post-ack
configure maintenance (`configure.rs:3005,3035,3290,3322`). Disk/build work does
not start before the acknowledgment. Admission remains serialized by the
artifact-reload guard (`configure.rs:4204–4233`), and existing generation/lifecycle
checks govern the worker and publication. Same-session rebinds queue repair when
needed without repeating session bash replay.

The two `identical_bind_repairs_evicted_artifact_beside_reader_*` tests
(`configure.rs:7694,7699`) use real configure, evict its trigram plane, and hold a
real PureRead on the root. They require an identical bind to acknowledge before
that reader is released, preserve config and generation, admit but not start the
reload before post-ack maintenance, and ultimately serve real grep matches.
Restoring both old residency predicates independently reddens each named test.

**Incident impact:** the read timeout prevents the Downloads grep from holding
any of the incident's binds for six minutes. The artifact-repair change removes
same-harness, same-effective-config, artifact-only promotions; the supplied log
cannot identify which incident binds, if any, met that exact condition. Genuine
runner/opencode configuration transitions (5708,5725–5727) still use the writer
barrier and wait for existing readers, now bounded by the read timeout. Changes
to resolved config, root/topology, and quiesced or failed runtimes retain their
existing barriers. The existing
`config_changing_bind_waits_for_the_held_tail_before_reconfiguring` test is
unchanged and passes. No TypeScript route-replacement code was changed.

## Proposal only: harness-only binds as per-route provenance

A future design could treat a harness-only transition, with otherwise identical
resolved core configuration, as a shared attach rather than a root reconfigure.
This is **not implemented here**. It would require moving all remaining harness
namespace/routing consumers from root-global state to route/request identity:
backup/checkpoint storage, runtime/session replay ownership, semantic route
harness, and other root-harness consumers. Merely ignoring harness in equality
would leave consumers using the previous route's namespace or routing identity.

The contracts that would move include configure fingerprint's harness identity
(`configure.rs:2519`), published `Config.harness`/semantic route metadata, root
`set_harness` and storage namespace selection, and the subc tests deliberately
requiring harness-changing binds to rerun exclusively
(`subc/mod.rs:16477–16512` and the bind-reserve tests immediately following).
Route isolation and differing harness-resolved capabilities would need their own
preservation tests before changing that contract.

## Verification caveat

The broad integration filter `read` also selected the unrelated
`callgraph_ops_return_building_then_ready_async` fixture. That test received
`callgraph_unavailable` with `read_only_store_not_built` in this isolated worktree,
not its expected `callgraph_building`. No assertion was rewritten. Narrow public
command/read-freshness integration gates passed. All cargo tests used a created,
throwaway HOME/XDG tree and `env -u AFT_STORAGE_DIR`; the Windows cross-check was
intentionally skipped as requested. Changes touch the search-quality fence files
`search_index.rs` and `lib.rs`, but change I/O bounds, not ranking/routing/scoring.

## Revision measurements: 10,000 verified files

The committed `grep_io_10k_measurement` test (`grep_executor.rs:1887`) creates
100 directories × 100 regular Rust files, each containing one `needle` match.
Fixture creation and index construction are outside the timer. It runs the real
`handle_grep` with `max_results=10001`, asserts both 10,000 searched files and
10,000 matches, uses a private four-thread Rayon pool, and reports five samples
and their median separately for indexed and fallback modes. Each run gets a fresh
request budget/cache. Helper spawns and channel round trips are counted at the
actual helper dispatch, not inferred from file counts. Counters are test-only and
request-scoped so other concurrent daemon/test work cannot pollute them.

Host: Darwin 27.0.0 arm64, Mac17,6, 18 logical CPUs, 128 GiB RAM. Toolchain:
Rust/cargo 1.99.0, debug test profile, `CARGO_BUILD_JOBS=4`. The local machine was
heavily contended by other compile-slot users. The worker session predated remote
routing; the parent explicitly authorized local background builds after a plain
cargo invocation demonstrably did not route remotely.

Production baselines were applied temporarily inside this worktree, never in the
parent checkout. `origin/main` was `0a6eeb56f711a48afbbe1d1600ef152d5b14cca3`.
For its measurement, only a test-only helper module and the identical temporary
benchmark harness were added; production grep/read/walk code was unchanged. The
prior delivery baseline was `a85e954ec1c1f28922e36721bcedb38ce56e4a5b`, with only
test-only helper counters and the same harness. Live source was staged before
each control, its applied non-empty diff was captured, and restored source had an
empty `git diff --stat`. Temporary baseline code and binaries are not committed.

### Helper costs (every run had these exact counts)

| Implementation | Indexed spawns / round trips | Fallback spawns / round trips |
|---|---:|---:|
| origin/main | 0 / 0 | 0 / 0 |
| Prior per-file-helper delivery | 10,012 / 10,012 | 20,010 / 20,010 |
| Revised boundary-cache delivery | 106 / 106 | 4 / 4 |

The indexed count is O(directories), including fixed root work. The fallback
walk reuses its already bounded helper for directory probes, so its count is O(1).
The non-ignored `ten_thousand_file_grep_uses_directory_probes_not_per_file_helpers`
test (`grep_executor.rs:1870`) enforces at most 132 spawns/round trips in either
mode and requires non-zero cold-probe counters. Disabling both caches produced
10,008 helpers and failed that exact guard. The FIFO-replacement and saturation
error tests also failed under their respective safety/error-label mutations.

### First five-run wall-time series (milliseconds)

| Implementation / mode | Samples | Median |
|---|---|---:|
| origin/main indexed | 870.344, 762.067, 769.792, 733.265, 690.309 | 762.067 |
| Revised indexed | 979.434, 869.036, 912.819, 884.655, 761.705 | 884.655 |
| Prior delivery indexed | 836.347, 769.771, 829.272, 874.773, 743.871 | 829.272 |
| origin/main fallback | 835.276, 817.670, 844.457, 856.587, 808.980 | 835.276 |
| Revised fallback | 852.915, 787.966, 786.297, 868.809, 858.979 | 852.915 |
| Prior delivery fallback | 1373.901, 1478.772, 1340.677, 1495.674, 1247.084 | 1373.901 |

A second back-to-back run of the saved main/revised binaries avoided a compile
between their measurements and again ran five samples each:

| Implementation / mode | Samples | Median |
|---|---|---:|
| origin/main indexed | 1092.641, 1016.171, 886.922, 812.984, 714.329 | 886.922 |
| Revised indexed | 943.912, 998.137, 1091.030, 945.385, 932.002 | 945.385 |
| origin/main fallback | 924.499, 866.229, 859.000, 886.795, 1074.242 | 886.795 |
| Revised fallback | 1294.183, 1375.284, 1377.576, 1507.019, 1511.664 | 1377.576 |

These timings do **not** establish a wall-time improvement over main: the revised
indexed median is 16.1% higher in the first series and 6.6% higher in the second;
fallback is 2.1% higher in the first and 55.3% higher in the second. The latter
series also took much longer to construct the revised fixture outside the timer,
consistent with substantial machine/filesystem contention. The deterministic
helper counts establish removal of per-file spawn/channel overhead; elapsed-time
parity is not claimed. Local direct reads still perform type/symlink admission
and descriptor checks to avoid opening non-regular or boundary-crossing files.
