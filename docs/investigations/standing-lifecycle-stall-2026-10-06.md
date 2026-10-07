# Standing tick stall on 2026-10-06

## Finding

The permanent holder was **executor worker 1's root lifecycle admission**, not
a standing-root mutex. Semantic query reload recursively acquired the same
non-reentrant `SubcLifecycleAdmission::unbound` mutex that its worker-start
closure already held. The frame loop then waited on that root while enumerating
sessions in the standing tick. Moving the tick off the frame loop alone does
not remove the deadlock: the newer standing actor's reconciliation mutex would
carry the root's permanent hold into unrelated bind handoffs.

The implementation and regressions are based on `082f57c3d5cc0c0408b8a5c76968e874cb2b52d1`.
Source line numbers below refer to the captured executable, not the patched tree.

## Symbolization

Both original captures were available under the local AFT diagnostics directory.
Every AFT address in every thread was symbolized against UUID
`565EA9CAF4233DD4A1AE847BE350166B`, DWARF file
`aft.dSYM/Contents/Resources/DWARF/aft-dbd23699f37f7b88`, load address
`0x102e88000`. Inline frames are essential: plain `atos` reports only
`StandingActor::tick:200` for the main waiter, hiding the lifecycle read.

```sh
atos -i -o <DWARF-file> -l 0x102e88000 \
  0x103ccacdc 0x103089b08 0x10416c75c 0x10416c7d4 0x103c2db28
```

| Capture | Original SHA-256 | Threads | Unique AFT addresses |
| --- | --- | ---: | ---: |
| `stall-20261006T000314Z-18642.txt` | `8abe262177715cc992ff79fc181bd3e065d071ea2629399c80a5ed7fadf3714c` | 727 | 615 |
| `stall-20261005T224441Z-18642.txt` | `234d76e979efa8948c6911fe1c7ef1dd05c3ccf0f27a5475051fd42cdc567ded` | 781 | 813 |

Complete symbolized captures, including inline frames, are retained in the task
worktree at `target/standing-stall-investigation/stall-*.inline.txt`; original
unsymbolized system frames are retained unchanged. These machine-local diagnostic
artifacts are not committed. The filtered log is `log-window.txt` in that directory.

### Holder: Thread_65649131, aft-executor-worker-1 (492/492 samples)

```text
executor::worker_loop:3733
  -> run_lane_jobs:3934
  -> subc::handle_tool_call -> run_tool_call -> dispatch:1277
  -> commands::semantic_search::handle_semantic_search:869
  -> trigger_semantic_index_reload_if_evicted:4425 [0x10416c75c]
     -> AppContext::run_if_subc_bound_generation:4598 (inlined)
     -> SubcLifecycleAdmission::run_if_current:395 (inlined; owns unbound mutex)
     -> reload closure:4426
  -> schedule_missing_artifact_loads:4228 [0x103e09040]
  -> schedule_artifact_loads:4844 [0x1030336a4]
  -> AppContext::heavy_root_work_allowed:6228 [0x103089b08]
     -> is_unbound:407 -> is_bound:399 (inlined; re-locks unbound mutex)
  -> parking_lot::RawMutex::lock_slow:262 [0x104d54028]
  -> _pthread_cond_wait -> __psynch_cvwait
```

This is a same-thread recursive acquisition, not a slow worker, checkpoint join,
executor queue wait, SQLite busy wait, or filesystem wait. There is no timeout
that can free the holder: it cannot reach the end of its own admission closure.

### Main waiter: Thread_65649061 (492/492 samples)

```text
run_subc_mode -> run_subc_mode_inner -> run_module_loop:5167
  -> StandingActor::tick:200 [0x103ccacdc]
     -> resume_entries_without_bound_session:223 (inlined)
     -> session filter closure:215
     -> AppContext::subc_unbound_quiesced:4574
     -> is_unbound:407 -> is_bound:399
     -> Mutex<RawMutex, bool>::lock
  -> RawMutex::lock_slow:262 -> _pthread_cond_wait -> __psynch_cvwait
```

Two other executor workers wait on lifecycle admission:

* Thread_65649137, worker 7: `handle_semantic_search` ->
  `trigger_semantic_index_reload_if_evicted:4425` `[0x10416c7d4]` -> inlined
  `run_if_current:392` (initial acquisition) -> `RawMutex::lock_slow`.
* Thread_65649139, worker 9: `drain_configure_tail:6455` ->
  `run_configure_maintenance_unit:6628` ->
  `run_configure_maintenance_unit_inner:7055` `[0x103c2db28]` -> inlined
  `subc_unbound_quiesced` -> `is_bound` -> `RawMutex::lock_slow`.

The self-deadlock on worker 1 is established by the inline stack and code.
The sample does not contain mutex addresses or root arguments; assigning every
other lifecycle waiter to that exact mutex/root is an inference, not a memory
inspection. No standing holder stack appears elsewhere in either capture.

### Standing lock inventory

| Lock | Evidence in the captured executable / composition with the newer actor |
| --- | --- |
| `StandingActor::reconciliation` | Does not exist in the captured source. The newer actor holds it around `tick`, so its blocking session enumeration must be removed. |
| `observed_config` | Clone guard ends before `roots.reconcile`; not held at the sampled session enumeration. |
| `reconciliation_failures` | Failure/success bookkeeping and logging finish before session enumeration; no captured holder. |
| `owned_actors` | Observation clones ownership and releases it; retirement returns before session enumeration. No captured holder. |
| `StandingRootsInner::database_path` | Cloned/updated in a separate short scope; not held by the sampled waiter. |
| `StandingRootsInner::state` | Reconciliation has returned before the sampled waiter; no other thread is in standing-root reconciliation or publication. |
| `ArtifactLifecycle::state` / publication epoch | No captured checkpoint, bind revocation, or standing publication stack. These are distinct from the `Mutex<bool>` identified by inline symbolization. |

`wait_for_case_a_checkpoint` is already bounded at two seconds per lifecycle.
Its condition-variable wait releases the lifecycle state mutex. The newer
`begin_session_bind` retains reconciliation during that bounded wait, but it is
not the permanent hold in this capture and its existing two-second policy is
unchanged. Standing reconciliation still performs root resolution and bounded
SQLite busy handling; this change is not a universal deadline for arbitrary
operating-system filesystem calls.

### Earlier capture: a different, transient stall

At 22:44Z the main thread is in the runtime's `kevent` poll for 321 of 399
samples. Its standing tick executes reconciliation for seven samples, including
`StandingRoots::reconcile:315` -> `db::open`, before taking the roots state
mutex. It also reaches observation and session enumeration. The dominant
non-poll main stack is memory census allocation (53 samples), waiting in
`_xzm_fork_lock_wait` rather than on a standing lock. Executor worker 2 is in
recursive filesystem deletion (`delete_file:420` -> `remove_dir_all` ->
`unlinkat`); worker 4 is parsing in `zoom`. There is no semantic reload recursion
or permanent standing-lock holder in that earlier sample.

### Census covering every thread

The counts below group complete symbolized stacks, not just named threads.
Channel/select receiver groups include root wake and bash manager threads;
their filesystem/persistence branches were also inspected. None adds a standing
lock holder. The health rollup and view publication perform independent SQLite
work in the later capture; seven executor workers and the executor scheduler
are idle on their channel receivers.

| Stack family | 00:03Z | 22:44Z |
| --- | ---: | ---: |
| Main | 1 | 1 |
| Executor (scheduler plus ten workers) | 11 | 11 |
| Log writer | 1 | 1 |
| Status emitters | 81 | 83 |
| Inspect workers / Rayon workers | 8 / 18 | 8 / 18 |
| Channel receivers / select receivers | 82 / 61 | 84 / 62 |
| Signal listener | 1 | 1 |
| Config watches | 8 | 8 |
| FSEvents loops / backends / watcher filters | 156 / 74 / 74 | 164 / 78 / 78 |
| Stall watchdog / health rollup | 1 / 1 | 1 / 1 |
| Semantic builders / HTTP runtime | 2 / 1 | 2 / 1 |
| Bash persistence | 18 | 19 |
| Filesystem lock heartbeats | 55 | 60 |
| Callgraph refresh | 1 | 1 |
| LSP stderr / stdin-or-reap / readers | 23 / 23 / 23 | 23 / 24 / 23 |
| System workqueue | 1 | 1 |
| View publication / Tokio workers | 1 / 1 | 0 / 28 |
| **Total** | **727** | **781** |

## Log corroboration

The inclusive 23:55Z–00:11Z window of `aft-18642.log` contains 1,407 lines.
There is no standing reconciliation refusal, checkpoint join, or standing build
message attributing a long hold. The last perf tick is 00:02:35.771Z. The final
bind burst is 00:02:52.090/.108/.136Z (channels 2729–2731), and all three have
`bound to root` lines by .197Z. Thus 00:02:52 is the last successful burst, not
evidence that those three requests themselves were refused.

Afterward, configure maintenance logs a bounded SQLite view-load failure at
00:02:58.577Z and `callgraph store unavailable at configure maintenance` at
00:03:02.534Z. The watchdog reports `subc_frame_loop`, `stalled_for_ms=15076`,
at 00:03:14.840Z. No later route bind or perf tick appears in this PID's window;
the only subsequent lines are minute-spaced embedding-backend connection
refusals from two independent builders, continuing through 00:11:40.385Z.
The supplied supervisor restart time (00:10:54Z) is external context, not an
event recorded in this log. The earlier 81.6-second inspect warning at 23:55:27
completed before the outage; it is not the holder in the stall stack.

## Repair and regression coverage

* Capture semantic-view eligibility before entering lifecycle admission and
  pass it through the admitted missing-artifact scheduler and artifact loader.
  Keep generation admission around worker installation, preserving unbind and
  superseding-configure fencing. The semantic refresh-disconnect restart uses
  its existing pre-admission eligibility snapshot; the search-only reload and
  disconnect paths explicitly pass false for semantic-view work. Ordinary
  configure scheduling captures eligibility outside lifecycle admission too.
* Track held lifecycle mutex identities per thread in debug builds. Every
  blocking lifecycle acquisition checks this marker before parking. Clones
  share the same identity; separate roots and non-blocking probes are allowed,
  and unwinding clears the marker. The mutex remains non-reentrant.
* Standing enumeration uses a non-blocking lifecycle probe. A contended root
  logs a debug skip, conservatively reserves its artifact family, and has no
  standing pass submitted. Other families and bind handoffs continue.

`evicted_semantic_view_reload_completes_under_bound_generation` exercises the
exact views-enabled, ready-but-evicted semantic reload with a two-second deadline.
It was red before the scheduling fix, with `recursive root lifecycle lock
acquisition`, and green afterward. Restoring the recursive read makes only this
test red; all three lifecycle-gate controls still pass.

`standing_tick_skips_busy_lifecycle_and_acks_another_root_bind` holds a root's
real admission closure, requests a real standing tick, and uses the production
single-threaded module loop to require `RouteBindAck` for another root before the
10.5-second bind deadline, while the first root remains held. It is not a test
accepting deadline refusal. Restoring the blocking lifecycle read makes this
test red; the other 28 standing-filter tests pass. The finite holder is released
before teardown on failure, so the mutation does not hang the suite.

`lifecycle_gate_recursive_acquisition_panics_instead_of_parking` proves that a
cloned admission's recursive read panics before a one-second deadline. Disabling
the detection makes only that test red with a timeout; the other two lifecycle
gate tests remain green. All mutations were made against a staged live tree and
restored with an empty unstaged diff.

Final verification used a throwaway HOME and created XDG data/config/cache/state/
runtime directories, real CARGO_HOME and RUSTUP_HOME, and no global
`AFT_STORAGE_DIR`. Configure unit coverage passed 148 tests (three manual tests
ignored), lifecycle admission passed 14, runtime drains passed 58, and binary
fixtures passed 121. The Windows warning-denying `--tests` cross-check completed;
it is compile-only, not a Windows runtime test. No search ranking/routing,
configuration schema, package manifest, or lockfile changed.
