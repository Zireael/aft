# Bash watchdog I/O and retention

## Measurement boundaries

Measurements use task-folder reads, not the live `aft.db`. No live files were
copied, deleted, or rewritten. The daemon was profiled, not replaced with this
branch's binary. Thus the after figures below are fixture work counts, **not a
claim about a deployed daemon's CPU percentage**.

`work_counts` records actual persistence open attempts (including directory
handles and failed legacy-layout probes), file-read routines, and explicit
metadata/stat calls. A directory-layout exit probe attempts seven opens: four
directories, a nonexistent legacy JSON, the current metadata JSON, and the exit
file. Six succeed. It reads two files and performs seven handle-stat checks.
These are file-operation counts, not bytes, elapsed time, or counts of every
underlying `read(2)`/pathname-lookup syscall. Iterator advancement is counted at
the `ReadDir` iterator boundary, so eager collection before applying a budget
cannot look bounded.

### Live storage

The first inventory found **13,876 folders**, close to the brief's 13,908 in a
changing store. Of 10,414 terminal folders with metadata older than one hour,
10,057 referenced missing workdirs; 4,159 were undelivered, 6,255 delivered.
There were also 2,417 records still marked running. A later plain-read census
found 2,368 running records **already containing an exit marker**: finished
commands can remain recorded as running when their project unloads before exit
and never binds again. Raw PID existence overcounts live commands because PIDs
are recycled; it is not an adoption/liveness guarantee.

A later 14,194-folder snapshot also read folder mtimes: 6,702 were older than
24 hours, 6,713 between one and 24 hours, and 779 younger than one hour. Metadata
mtimes put 6,686 / 6,700 / 808 in those same buckets. Folder age and finish time
are separate evidence; only finish time authorizes the seven-day abandonment
rule. Snapshots change while the live daemon and other workers run.

An opt-in Rust census checked recorded process identity before probing exit
markers. It found **32 recorded-live tasks across 31 roots**, with 30 roots
having one task and one root having two. Their marker paths performed:

| Work | Per tick | Per minute at 500 ms |
|---|---:|---:|
| Open attempts | 224 | 26,880 |
| File reads | 64 | 7,680 |
| Explicit stat checks | 224 | 26,880 |

Per one-task root this is 7 opens / 2 reads / 7 stats per tick; the two-task root
has twice that. This is a plain-read reconstruction of the marker path, not an
inspection of private live registry memory. Root-scoped replay filters on the
canonical recorded project; the daemon's root actor owns the registry, and
`watchdog_started` is shared by registry clones. No evidence of duplicate task
polling across intended root actors was found. The duplicated cost is timers
and idle registry ownership, not evidence that all roots poll all tasks.

Fresh 5-second `ck-aft profile --pid 24823 --seconds 5` on version 0.58.2 counted
1,389 running samples: 171 classified `bash_background`, including remote-bash
workers, across 36 threads; 314 subc-loop and 893 other samples. The workload had
changed from the brief's half-in-watchdog sample. Thread ownership and measured
file operations still justify eliminating the per-registry timers. View
materialization, remote execution, and the main loop are outside this change.

### Undelivered cohorts (before deploying delivery changes)

A 13,684-folder snapshot contained 4,161 old undelivered terminals:

| Storage | Cohort | Count |
|---|---|---:|
| runner | Recorded Broca mason (`alfonso:bg_*`, requester `reserved:broca`) | 1,122 |
| runner | Legacy mason, requester/harness not recorded | 56 |
| runner | `ses_*` | 9 |
| opencode | Other session patterns, notify enabled | 2,954 |
| opencode | `ses_*` | 9 |
| opencode | Explicitly test-labelled sessions | 10 |
| opencode | Other, notify disabled | 1 |
| pi / mcp | Undelivered in this snapshot | 0 |

All but one had `notify_on_completion=true`. That flag identifies notification
eligibility, **not** explicit background launch versus foreground promotion.
Metadata stores neither that distinction nor a record of terminal watch/status
responses. Counts of historical results already returned through `bash_watch`,
`wait:true`, or `bash_status` are therefore **unknown**, not assumed zero or
assumed delivered.

The 2,954 opencode “other, notify enabled” records break down as follows. Dates
are UTC finish timestamps (start timestamp fallback if finish was absent).
No `alfonso:bg_*`, `alfonso:sidekick-*`, or `ses_*` occur in this specific cohort.

| Session pattern | Count | Oldest | Newest |
|---|---:|---|---|
| `session-*` | 2,125 | 2026-08-22 09:43:33 | 2026-10-05 05:39:50 |
| `__default__-*` | 24 | 2026-08-25 15:29:20 | 2026-08-26 01:24:22 |
| `repeat-*` | 176 | 2026-09-29 16:57:16 | 2026-10-05 05:38:58 |
| `persist-*` | 3 | 2026-08-25 19:13:17 | 2026-08-26 01:23:38 |
| `shared-*` | 295 | 2026-08-22 09:43:36 | 2026-10-05 05:40:00 |
| `restart-*` | 329 | 2026-08-22 09:44:11 | 2026-10-05 05:40:08 |
| `reserved-*` | 2 | 2026-08-25 15:25:46 | 2026-08-25 15:34:13 |

These are predominantly fixture-like patterns, not evidence of old OpenCode
masons. Session patterns alone cannot prove historical consumption or a live
OpenCode notification failure. The source does prove a transport-independent
gap: notification-enabled terminal watch/status replies did not acknowledge
completion. Foreground no-notify tasks already acknowledge at terminal
publication. The new reply seam consumes terminal results only for their
originating session; internal status polling and foreign observations do not.
A stale rendering snapshot is also fenced against re-enqueuing after that ack.

## Why folders accumulated, and the repairs

* Periodic cleanup previously scanned all in-memory history every minute,
  removed candidates **before** disk deletion, and had no retry after a failed
  delete. It now rotates at most 128 candidates per registry and retains failed
  deletions for retry; session bookkeeping is updated incrementally.
* Persisted GC ran once on replay, not every minute. Deleted worktrees and
  inactive harness stores might never replay again. Recurring sweeps are
  coalesced per storage root, discover sibling harness stores in bounded pages,
  and resume directory iterators rather than discovering an entire session
  before limiting work. Task and quarantine walks each advance at most 128
  entries per pass, including invalid entries and directory boundaries.
* Unloaded projects can leave exited commands marked running. GC recovers a
  validated marker only after both recorded-process checks say dead, retaining
  the exit-file timestamp as finish time. It never reruns the command.
* Uncollected completions were deliberately retained. Reply acknowledgments fix
  future collected results at their source. The approved abandonment fallback
  retires a terminal, undelivered record only when **both** recorded project and
  workdir are absent and it finished **more than seven days** ago. Missing or
  uncertain path information does not authorize retirement. Each sweep logs its
  retirement count; `bash.retired_undelivered_completions` exposes the cumulative
  per-registry count in health.
* Persisted GC keeps the existing **24-hour metadata modification grace**.
  Finished-task watchdog cleanup also retains results for **24 hours** and
  requires delivery acknowledgement. The separate handle-release and bounded
  deletion-retry safeguards remain in place, including on Windows. The original
  one-hour cleanup limit was not the persisted GC policy.

Delete failures are a proven source-code retry gap; inventory alone cannot
count how many historical deletes failed. Some genuinely uncollected records
remain intentionally retained, including records whose worktrees still exist.

## Before / after fixture work

`watchdog_cost_live_children_open_no_artifacts` uses 40 real live children,
30 registries, and 10,000 retention-eligible directory-layout finished bundles.
It drives the actual poll/reap path; scheduler ownership is tested separately.
A restored mutation reconstructs the old live-child marker probes and unbounded
history cleanup. The budgeted fixture uses the real same filesystem operations.

| Work | Before | After |
|---|---:|---:|
| Running-task open attempts / tick | 280 | 0 |
| Running-task file reads / tick | 80 | 0 |
| Running-task stats / tick | 280 | 0 |
| Running-task open attempts / minute | 33,600 | 0 |
| Running-task file reads / minute | 9,600 | 0 |
| Running-task stats / minute | 33,600 | 0 |
| Periodic cleanup candidates / 30 registries | 10,000 | 3,840 |
| Periodic cleanup open attempts | 140,000 | 53,760 |
| Periodic cleanup file reads | 10,000 | 3,840 |
| Periodic cleanup stats | 110,000 | 42,240 |
| Timer topology / periodic wakes per minute | 30 / 3,600 | 1 / 120 |

After cleanup, the new shared persisted pass advances **128 entries**, deletes
127 more bundles, and performs 2,670 opens / 381 reads / 2,286 stats. Together,
a new full minute costs **56,430 opens / 4,221 reads / 44,526 stats**, versus
**173,600 / 19,600 / 143,600** for the old tick + periodic-cleanup minute.
Old persisted GC was once-only, not another periodic minute cost. A reconstructed
startup pass after that full deletion scans the remaining 40 recent running
records: 272 opens / 40 reads / 240 stats / 101 iterator advances, deleting none.

Owned children use `try_wait`; PTYs retain their waiter/reader contract. Adopted
Unix tasks use macOS/BSD `EVFILT_PROC NOTE_EXIT` or Linux pidfd readiness. A
failed/unsupported observer falls back to marker probes at 0.5, 1, 2, 4, then
5-second intervals: **15 probes in the first simulated minute, 12/minute once
warm**, rather than 120. For 40 fallback tasks this bounds the first minute to
4,200 opens / 1,200 reads / 4,200 stats, and warm minutes to 3,360 / 960 / 3,360.
Kernel-backed live adopted tasks open no task artifacts; kernel descriptors are
one-time observer resources, not per-tick marker opens. Refused markers still
settle an adopted process once it is known dead. Windows retains its existing
marker/child polling behavior and was compile-checked, not runtime-tested.

## Verification and mutation proofs

The initial five cost/retention/lifecycle regressions failed on the base code.
Terminal reply ack, unobserved-exit recovery, late snapshot fencing, and refused
marker settlement each had a named red before their fix. Restored mutations
individually reddened the corresponding tests for live/adopted polling, fallback
backoff, scheduler ownership, both cleanup budgets, failed-delete retries,
recurring GC, inactive harness discovery, abandonment, recovery, reply ack,
late snapshot fencing, and refused-marker settling.

Replay/reminder tests keep their original assertions. Their readiness checks
now read metadata rather than consuming a terminal status reply. The envelope
fixture inspects the pending completion before the terminal status reply.
Acknowledging during internal terminal publication makes each such fixture fail
individually, including foreground promotion, restored completion replay, Pi
watch replay, DB-hint completion trailers, and real envelope output. Full red
names, captured assertions, and staged-diff/empty-restoration pairs are in the
worker delivery declaration, not uncommitted log files.

All Rust gates use `CARGO_BUILD_JOBS=4`, one cargo invocation at a time, and
`--test-threads 4`. Tests run with throwaway HOME/XDG directories and no
`AFT_STORAGE_DIR`; Rust toolchain/cache locations remain available. Native
macOS tests cover kqueue; Windows verification is compile-only. Linux and other
BSD runtime behavior were not exercised on this macOS host.

Final restored-source gates (Cargo/rustc 1.99.0): background library **206
passed**, response finalizers **23 passed**, binary pending responses **8
passed**, bash integration **329 passed**, and list-envelope **175 passed**;
all had zero failures. Windows `cargo check --target x86_64-pc-windows-gnu --tests
-p agent-file-tools` finished with `-D warnings -A deprecated` and both compiler
wrappers explicitly disabled. Rustfmt 1.10.0 checked formatting; Biome 2.4.7
checked 695 files. The opt-in live marker census separately ran one passing
test. Scoped inspection remained partial because rust-analyzer was still
indexing and the checkout call-graph view was unavailable; compiler/test gates,
not that partial inspection, are the authoritative verification.

Reproduction entry points:

* `scripts/bash-watchdog-inventory.py`: plain-read live inventory and cohort dates.
* `cargo test -p agent-file-tools --lib -- watchdog_cost_ --test-threads 4 --nocapture`:
  fixture counts and regression fences (set isolated HOME/XDG first).
* `watchdog_cost_live_storage_marker_census` is ignored by default: run its
  filter with `--ignored` and explicitly supply `AFT_WATCHDOG_CENSUS_ROOT`.
