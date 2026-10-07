# CI test flakiness investigation

## Evidence window and counting

Collected on 2026-10-07 from `cortexkit/aft`, workflow `tests.yml`, for
2026-09-16 through 2026-10-07 (UTC). `gh run list --created '>=2026-09-16'
--limit 1000` returned **657 main/train runs**. Failed-attempt logs were read
with `gh run view --attempt N --log-failed`; passing rerun job logs were read
with `gh run view --attempt N --json jobs` and `gh run view --job ID --log`.
The latter is important: a successful workflow is not evidence that a
nonblocking Windows test passed, or that a cancelled test actually ran.

The table counts **distinct (run, attempt, platform/job, test) failing events
with a subsequently executed PASS at the identical SHA**, not the repeated
failure printed in a nextest summary. Last seen is the failing run's creation
time, in UTC. Counts are confirmed recurrence counts, not total failures.
Links identify failing attempts; the final column names the passing attempt.
Same-SHA evidence establishes nondeterminism, not necessarily a timing cause.
Assertions exposing a product race must not be weakened to hide that race.

Twelve log downloads failed with transient HTTP/stream errors. Consequently
this is a lower bound, not a claim that missing logs contained no flakes.
Full-run `--log` downloads hit gh's missing-job-log request limit; narrowing
to named jobs recovered passing test evidence. Next-run candidates are
separated below rather than treating an unchanged test file as proof that
its production dependencies were unchanged.

## Confirmed same-SHA flakes

| Test | Platform | Count | Last seen (UTC) | Failure text / cause class | Failing run → passing attempt |
| --- | --- | ---: | --- | --- | --- |
| `Pi enabled config toggle > disabled config registers nothing without resolving a binary or creating a bridge pool` | macOS | 1 | 09-22 14:32 | Expected disabled log message; received empty string after fixed 600 ms sleep. Log flush timing. | [35741038342](https://github.com/cortexkit/aft/actions/runs/35741038342/attempts/1) → 2 |
| `bash tool adapter > transport-dead fallback recovers on the next successful module call` (Pi) | macOS | 1 | 09-21 00:36 | `timed out after 20000ms`. Child/host fallback liveness. | [35548243286](https://github.com/cortexkit/aft/actions/runs/35548243286/attempts/1) → 2 |
| `bash_background_persistence_test::bash_status_legacy_persisted_task_is_quarantined_on_replay` | macOS | 1 | 09-18 23:51 | `replay left the legacy metadata file in place, but a direct quarantine of it succeeds`. Persistence ordering/race. | [35407268324](https://github.com/cortexkit/aft/actions/runs/35407268324/attempts/1) → 2 |
| `bash_pty_test::pty_windows_wrapper_script_runs_utf8_command` | Windows | 1 | 10-04 18:37 | Output contained only terminal negotiation/title escape sequences. PTY readiness. | [37225148728](https://github.com/cortexkit/aft/actions/runs/37225148728/attempts/1) → 2 |
| `bash_pty_test::pty_write_python_repl_round_trip` | Windows | 1 | 09-19 04:02 | `timed out waiting for "pty-repl-ok"`; last output ended at `>>> `. PTY readiness/input. | [35420238745](https://github.com/cortexkit/aft/actions/runs/35420238745/attempts/1) → 2 |
| `bash_token_count_test::bash_completed_frame_compressed_tokens_reflect_compression` | macOS | 3 | 09-30 21:38 | `expected compressed token count 613 to be lower than original 613`. Output/compression snapshot race, not a latency assertion. | [36097850252](https://github.com/cortexkit/aft/actions/runs/36097850252/attempts/1), [36706127153](https://github.com/cortexkit/aft/actions/runs/36706127153/attempts/1), [36780704669](https://github.com/cortexkit/aft/actions/runs/36780704669/attempts/1) → 2 |
| `callgraph_test::callgraph_rust_use_imported_short_callers_resolve` | Linux | 1 | 10-03 20:16 | `timed out after 60s waiting for response line from aft stdout (child still running)`. Product/maintenance liveness. | [37150941148](https://github.com/cortexkit/aft/actions/runs/37150941148/attempts/1) → 2 |
| `doctor --issue body integration > tool failures section is included in assembled body and survives cap helper` | macOS | 1 | 10-03 05:11 | `timed out after 5000ms`. Cold imports/default Bun timeout. | [37098997194](https://github.com/cortexkit/aft/actions/runs/37098997194/attempts/1) → 2 |
| `e2e semantic search tool > aft_search degrades to a lexical fallback when semantic is disabled` | Linux, opencode-ndjson | 1 | 09-29 01:38 | E2E fallback assertion failed; same-SHA rerun passed. Requires product/fixture investigation, not a relaxed deadline. | [36508835945](https://github.com/cortexkit/aft/actions/runs/36508835945/attempts/1) → 2 |
| `effective_path_cache_test::valid_cache_skips_sleeping_shell_and_returns_ping_quickly` | Linux | 1 | 09-24 14:24 | `served PATH "" does not include cached entry /cached/login/bin`. Child fixture/startup ordering. | [36012527382](https://github.com/cortexkit/aft/actions/runs/36012527382/attempts/1) → 2 |
| `inspect_command_test::scoped_rust_inspect_drops_a_fixed_compiler_error_with_real_rust_analyzer` | Linux | 1 | 10-01 00:26 | `control: the first check reports the error` failed. Real-server diagnostics generation/readiness. | [36796192789](https://github.com/cortexkit/aft/actions/runs/36796192789/attempts/1) → 2 |
| `inspect_command_test::scoped_rust_inspect_reports_a_removed_field_after_an_outside_edit_with_real_rust_analyzer` | Linux | 1 | 10-05 15:13 | `rust-analyzer's own error for the removed field is missing`. Real-server edit/check ordering. | [37331104560](https://github.com/cortexkit/aft/actions/runs/37331104560/attempts/1) → 2 |
| `inspect_command_test::unscoped_inspect_names_an_unfinished_cargo_check` | Linux, Windows | 2 | 10-01 17:41 | Expected unfinished check but response reported success. Slow-producer scheduling race. | [36843386006](https://github.com/cortexkit/aft/actions/runs/36843386006/attempts/1), [36901251069](https://github.com/cortexkit/aft/actions/runs/36901251069/attempts/1) → 2 |
| `inspect_engine_test::inspect_engine_roots_share_bounded_named_thread_pool` | Windows | 1 | 09-30 13:48 | `matches!(first.join().expect("first scan"), JobOutcome::Fresh { .. })`. Shared queue/admission. | [36724403748](https://github.com/cortexkit/aft/actions/runs/36724403748/attempts/1) → 2 |
| `lsp_diagnostics_test::watcher_overflow_sends_no_per_file_flood` | Windows | 1 | 09-30 11:49 | `2 notifications`, expected 1. Watcher forwarding/barrier race. | [36710915667](https://github.com/cortexkit/aft/actions/runs/36710915667/attempts/1) → 2 |
| `per_checkout_9::callgraph_ops_report_a_keyless_view_generation_as_disabled` | macOS | 1 | 10-02 22:15 | Expected disabled callgraph response; got unsuccessful callers response. View publication/maintenance race. | [37071482497](https://github.com/cortexkit/aft/actions/runs/37071482497/attempts/1) → 2 |
| `per_checkout_9::standalone_view_publication_never_blocks_requests` | Windows | 1 | 10-05 13:53 | `a request waited 6002 ms for the view publication` (5 s assertion). Scheduling-sensitive latency assertion. | [37320286746](https://github.com/cortexkit/aft/actions/runs/37320286746/attempts/1) → 2 |
| `shared_db_contention_test::status_answers_within_a_second_while_another_process_holds_the_write_lock` | Windows | 2 | 10-03 21:30 | `status exceeded 1s while aft.db was write-locked: [1.1683217s]`. Sampling-window/latency assertion. | [36626746920](https://github.com/cortexkit/aft/actions/runs/36626746920/attempts/1), [37155371550](https://github.com/cortexkit/aft/actions/runs/37155371550/attempts/1) → 2 |
| `standalone_search_deferred_test::standalone_inspect_preserves_partial_results_when_rust_keeps_indexing` | Windows | 1 | 10-06 06:09 | Expected partial inspect state but response reported success. Slow LSP producer readiness. | [37422176496](https://github.com/cortexkit/aft/actions/runs/37422176496/attempts/1) → 2 |
| `status_bar_text_test::standalone_status_bar_trails_text_on_change_and_not_on_unchanged_result` | Windows | 2 | 09-25 03:02 | `aft_inspect failed`, `dead_code ... callgraph_unavailable`. Background maintenance/readiness. | [36077367695](https://github.com/cortexkit/aft/actions/runs/36077367695/attempts/1), [36088716976](https://github.com/cortexkit/aft/actions/runs/36088716976/attempts/1) → 2 |
| `subc transport parity sweep > server-rendered text matches NDJSON for representative tool calls` | Linux, opencode-subc | 1 | 09-30 13:48 | Parity assertion failed; same-SHA rerun passed. Product/fixture ordering. | [36724403748](https://github.com/cortexkit/aft/actions/runs/36724403748/attempts/1) → 2 |
| `subc_bridge_test::subc_bridge_goodbye_cancels_queued_read_before_same_root_rebind` | Windows | 1 | 10-01 07:39 | `subc mode exits cleanly: ConnectionLost`. Transport teardown. | [36831632794](https://github.com/cortexkit/aft/actions/runs/36831632794/attempts/1) → 2 |
| `subc_bridge_test::subc_bridge_health_check_returns_root_status_report` | Windows | 1 | 09-20 19:34 | `connection teardown must cancel queued root maintenance`. Queue teardown ordering. | [35532736043](https://github.com/cortexkit/aft/actions/runs/35532736043/attempts/1) → 2 |
| `subc_bridge_test::subc_bridge_l3_coalesces_already_bound_route_burst` | Linux | 1 | 09-22 02:19 | `subc mode exits cleanly: ConnectionLost`. Transport teardown. | [35679021255](https://github.com/cortexkit/aft/actions/runs/35679021255/attempts/1) → 2 |
| `subc_detach_test::subc_drain_exits_with_many_live_lsp_servers_in_one_deadline` | macOS | 1 | 09-30 23:00 | `exit took 2.129190417s` (2 s assertion). Shutdown latency/scheduling. | [36788805779](https://github.com/cortexkit/aft/actions/runs/36788805779/attempts/1) → 2 |
| `subc_detach_test::subc_drain_with_slow_writer_persists_terminal_line` | Linux | 1 | 10-03 21:30 | `shutdown did not save half the index wait by overlapping: combined=1521 ms, sequential=1702 ms`. Timing used as proxy for phase overlap. | [37155371550](https://github.com/cortexkit/aft/actions/runs/37155371550/attempts/1) → 2 |
| `trace_to_symbol_test::trace_to_symbol_no_path_reports_complete_no_path_found` | Linux | 1 | 10-05 11:17 | `timed out after 60s waiting for response line from aft stdout (child still running)`. Product/maintenance liveness. | [37301998630](https://github.com/cortexkit/aft/actions/runs/37301998630/attempts/1) → 2 |

Total: **27 names, 32 confirmed failing events**. Historical names are retained
above; a rename/removal in the current tree is not silently counted as a fix.

## Static inventory at 0a6eeb56

Searched `crates/aft/src` (including inline lib/bin tests), `crates/aft/tests`
and `packages/*` for `elapsed()`, `Instant::now() +`, `sleep`, `setTimeout`,
`toBeLessThan`, environment mutation, process globals and loopback ports.
This is a risk triage, not a statement that every sleep is wrong: a bounded
poll reads an actual condition, an injected producer delay exercises a
mechanism, and a negative receive timeout must remain short.

### High-risk timing assertions and sleep-then-assert fixtures

| Area/files | Risk / mechanism to inspect |
| --- | --- |
| `src/subc/push.rs` | Two `< 50 ms` assertions on saturated queues. Prove return while consumers have not drained; retain bounded channel waits only against hangs. |
| `src/inspect/manager.rs` (`tier2_deadline_tests`) | 250 ms sleeping producer and `< 250 ms` cancellation assertion. Gate the producer until permit release; assert cancellation/permit census. |
| `src/commands/inspect.rs` (`deferred_terminal_tests`) | Remaining wall-clock budget asserted after a receive. Assert the selected phase deadline/reserve before waiting, and the terminal outcome before releasing the producer. |
| `src/views/parent/tests.rs`, `tests/integration/per_checkout_9.rs` | 600 ms/2 s configure/discovery bounds; 3 s delayed discovery; 5 s publication latency. Need a controllable discovery/publication gate rather than a delay whose completion races the assertion. |
| `src/commands/health_digest.rs` | Passive digest `< 250 ms`. Existing LSP/cold-build counters prove no spawning, not that quiescence was never waited on. |
| `src/callgraph_store/mod.rs`, `src/readonly_artifacts.rs` | `< 250 ms`/1 s locked-schema read assertions; fixed 100 ms sleep before releasing writer. Prove return with the writer still held. |
| `src/effective_path.rs` | 4.5 s and 8 s probe assertions against 10 s sleeping shells; total budget should be checked via candidate invocation/kill markers. |
| `src/commands/bash_regex_match.rs` | `< 1 s` used as a complexity proxy. Requires algorithm/operation-count instrumentation to prove linearity without a performance assertion. |
| `src/subc/mod.rs`, `src/subc/wire.rs` | Deadline/teardown tests contain elapsed assertions; distinguish protocol deadline selection from scheduler timing. |
| `tests/integration/subc_detach_test.rs` | 2 s exit and budget+150 ms LSP assertions. Existing phase-event hook proves overlap; server census and orphan checks prove teardown. |
| `tests/integration/shared_db_contention_test.rs` | Historical 1 s sampling replaced by a held-lock fixture at base, but a response latency ceiling remains. |
| `tests/integration/standalone_search_deferred_test.rs`, `lsp_manager_test.rs`, `state_commands_test.rs` | Fixed slow producers and `< 2/5/15 s` assertions. Need producer-release barriers for ordering, not a sleep race. |
| `tests/integration/bash_reply_timing_contract_test.rs`, `bash_background_test.rs`, `bash_foreground_background_architecture_test.rs` | Completion/timeout wall-clock contracts and fixed sleeps. Existing process-sentinel fixtures can replace some bounds; polling bounds alone are not evidence of the intended timeout path. |
| `src/executor/tests.rs`, `src/subc/stall_watchdog.rs`, `src/context.rs`, `src/config_live_tests.rs` | Sleep-then-check progress/debounce/queue state. Check whether the test has an injected clock, started signal or drain barrier before changing it. |
| `packages/aft-bridge/src/__tests__/subc-transport.test.ts`, `subc-route-open-reload-window.test.ts` | Real-time route-open/call-budget bounds mixed with deterministic fake-clock assertions. Fake-clock comparisons are not flaky wall-clock assertions. |
| `packages/aft-bridge/src/__tests__/bash-host-fallback.test.ts`, `bash-hints.test.ts` | Abort/performance wall-clock assertions. Assert child cancellation or unresolved-stage ordering instead. |
| `packages/opencode-plugin/src/__tests__/subagent-detect.test.ts` | Lookup timeout ±50/500 ms assertions on loaded event loops. Assert fallback while host lookup remains unresolved and timeout result caching. |
| `packages/opencode-plugin/test/cancellation/effect-cancellation.test.ts` | `< 250 ms` interruption assertion. Assert finalizers and unresolved work ordering. |
| `packages/aft-bridge/src/__tests__/config-watch.test.ts`, `npm-resolver.test.ts`, `revivable-transport.test.ts`; `packages/opencode-plugin/src/__tests__/tui-notification-socket.test.ts`, `v2-result-metadata.test.ts`, `auto-update-checker/cache.test.ts` | Fixed sleeps before notification/revival/process/cache assertions; require actual condition/barrier. |

Paths in this table beginning `src/` or `tests/` are under `crates/aft/`.

### Shared process state

- Rust PATH tests in `src/tool_path.rs` mutate PATH. The process-env lock only
  serializes participants; unrelated LSP/git/child tests can still read it.
  Prefer a subprocess environment or an injected lookup (production API change).
- Rust env mutations also occur in `src/main.rs`, `src/context.rs`,
  `src/cli/warmup.rs`, `src/gh_shim.rs`, `src/bash_background/mod.rs`,
  `src/test_storage.rs`, `src/inspect/manager.rs` and integration fixtures
  (`branch_switch_test`, `per_checkout_semantic_runtime`, `subc_bridge_test`,
  `subc_storm_test`). RA/test hook settings can leak to concurrent readers even
  where a writer holds the environment lock.
- Global executor/cold-build admission, inspect engine queue, parent registry,
  bash registry and refresh/debounce settings must be checked for root-keyed
  ownership or isolated test constructors. A global mutex is not per-test state.
- TypeScript env/global mocks appear in bridge `paths`, `migration`,
  `onnx-concurrent-install`, Pi config/init fixtures, OpenCode `tui-preferences`,
  and CLI doctor/host-shell tests. `acquireEnv` and serial suites protect
  cooperating mutators, not arbitrary concurrent readers. New test-only
  subprocess fixtures should receive their own HOME/XDG namespace.
- Loopback `127.0.0.1:0` listeners in semantic, watcher, subc and fake-server
  fixtures reserve their sockets while serving; these are not fixed-port
  collisions. URL literals in config goldens (4096/11434) are not listeners.
  A port returned after dropping its reservation remains a race.
- Storage helpers already require explicit isolated `storage_dir` for Rust
  contexts. Test commands must still clear ambient `AFT_STORAGE_DIR`, and child
  processes need isolated HOME/XDG directories, not the operator's store.

## Verification and follow-up

This first commit preserves evidence and the static inventory before test
changes. Per-area fixes, mutation outputs, stress counts and explicit reasons
for tests requiring production changes will be recorded below. No ranking
files or production behavior are changed by this investigation.

The first Mac baseline compile queued behind other worktrees for 30 minutes
and was terminated before tests ran. Subsequent native checks use the remote
Linux build service per the task reviewer's direction; Windows compilation
remains the named local cross-check.

### Batch: Rust inspect deadline mechanisms

- The ignoring producer now waits for a release signal, not a 250 ms sleep.
  The test checks permit release while that producer cannot have completed;
  its root and limiter are per invocation. Positive waits have a 30 s hang
  ceiling.
- The cooperative test now asserts observed cancellation, the deadline-owned
  timeout outcome and an empty permit census, not `< 250 ms` elapsed time.
  Its name changed from `returns_within_grace` to `observes_cancellation` to
  disclose that this is a mechanism contract, not a latency SLA.
- The shared-request test checks the reserved terminal budget structurally
  and the named phase-timeout outcome while its producer is still gated. It
  no longer asserts remaining wall-clock budget after scheduler wakeup.
- Remote Linux gate: `cargo test -p agent-file-tools --lib --
  tier2_deadline_tests shared_request_deadline_returns_a_named_tier2_terminal_before_slow_work
  --test-threads 4`: **3 passed, 0 failed**.
- Mutation: withholding the permit from the deadline controller failed only
  `tier2_pass_deadline_releases_limiter_while_ignoring_stub_still_runs` with
  `deadline did not release limiter slot`; both other tests passed.
- Mutation: completing without cancellation failed only
  `tier2_pass_deadline_cooperative_stub_observes_cancellation` with
  `deadline must own the cancellation request`; both other tests passed.
- Mutation: sending Fresh before releasing the slow producer failed only
  `shared_request_deadline_returns_a_named_tier2_terminal_before_slow_work`
  with the `inspect_phase_timeout` outcome assertion; both other tests passed.
- Each control used staged live files and had a non-empty `git diff --stat`
  during the mutation and an empty one after `git checkout --` plus `touch`.
- Stress: temporary test-side helpers ran **each of the three changed tests
  20 times, with four concurrent contenders** (four lanes × five iterations).
  Output: `both tier2 deadline tests passed 20/20 with four concurrent
  contenders`; `shared-request deadline test passed 20/20 with four concurrent
  contenders`. **2 helper tests passed, 0 failed**. Helpers were removed and
  the final-source 3-test gate passed again. No retries or synthetic CPU load.
- `cargo fmt --all -- --check`: exit 0, rustfmt 1.10.0-stable. Compiler baseline
  is rustc 1.99.0 / cargo 1.99.0; the remote commands reported `ran remotely on
  ck-motor`.
