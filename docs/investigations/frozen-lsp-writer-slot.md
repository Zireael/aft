# Frozen language-server stdin and the root writer slot

## Reproduction on the original transport

Run `cargo test -p agent-file-tools --lib frozen_lsp_edit_releases_writer_before_route_bind_deadline -- --nocapture`.
The fake server completes initialization and opens a document, then stops both
its stdin reader and its analysis loop until a release file appears. The test
submits a real `edit_match` as a Mutating `edit` job, changing a comment to more
than the stdin pipe can hold (2 MiB). It then submits the same root's configure
job and exercises the RouteBind deadline/refusal path. Cleanup resumes the fake
even when an assertion fails.

Before the transport fix, the run produced:

```text
subc attach: pending RouteBind route 71@1 for root /private/var/folders/18/257zzylx4h1gbkcvs4cnpqqc0000gn/T/frozen-lsp-edit-bindBTTanX crossed 10516ms (configure_request_id=subc-bind-frozen, configure_state=queued, configure_phase_timings=[idle=14553ms], blockers=[queued_behind_configure(1)], oldest_queued_writer_age_ms=Some(10496), in_flight_readers=[], reader_admissions_while_promoted_writer_waited=0)
frozen LSP reproduction: elapsed_ms=10544 edit_finished=false bind_finished=false refusal=Some(Object {"code": String("actor_not_ready"), "message": String("route bind deadline exceeded after 10523ms (deadline 10500ms): the root's configure did not finish, so AFT refuses the bind before the daemon's 12000ms bind relay times out")})
edit with frozen stdin exceeded its bound: elapsed=None
test result: FAILED. 0 passed; 1 failed
```

The fake's separate input thread matters: stopping only analysis still drains
stdin into its message queue. That exercises the already-bounded diagnostics
wait, but cannot expose an unbounded pipe write.

## Where the edit waited

Line references in this section are to base commit
`15e7f012e511552e2b3983c176126b26bf2451fa`:

* `context.rs:10203` calls `notify_file_changed_versioned` while holding the
  manager guard acquired at `10179`.
* `lsp/manager.rs:2015` sends the full `didChange`. The document is already open,
  so this is not a `didOpen` or an initialization request.
* `lsp/client.rs:1704-1708` takes the stdin writer mutex and directly calls
  `transport::write_message`. `lsp/transport.rs:121-123` writes the header,
  `write_all`s the body, and flushes a blocking `ChildStdin`, with no deadline.
  The fake is not reading; a 2 MiB frame cannot fit in the pipe. The edit is
  waiting in that write, retaining both the manager mutex and the root writer
  slot. It has not reached `didSave` or the diagnostics wait.
* The diagnostics deadline is created only at `context.rs:10224`, after the
  notification has returned, so it cannot bound that write.

The manager-lock rework already releases the manager during response waits
(`lsp/manager.rs:522-529`) and during the post-edit event wait
(`context.rs:10253-10258`). It helps concurrent manager users when the server is
merely slow to answer. It does **not** move the edit out of the executor's
writer slot or bound stdin writes. The reproduction still wedges that slot on
this assembly.

## Bounds and recovery

* One writer thread owns each server's stdin. Enqueue plus write acknowledgement
  share a 250 ms deadline. A caller never locks or writes that pipe directly.
  Reader-generated responses have a bounded enqueue; cancellation is
  best-effort and adds no wait to an expired request.
* A write/request timeout or a silent post-edit diagnostics timeout quarantines
  the live client as unresponsive. Future interactions fail locally until the
  stdout reader observes another JSON-RPC message, or the server is restarted.
  Timeout handling neither kills nor resumes the process. Normal explicit
  shutdown and idle ownership policies remain unchanged.
* A timed-out frame stays with the writer: interrupting a partial frame would
  corrupt the protocol on resume. Document versions are reserved on uncertain
  writes, but disk snapshots stay unsynced, forcing a fresh full-text resync
  instead of certifying cached diagnostics.
* Post-edit collection has one caller-owned budget, capped at 10 seconds,
  including cold startup and manager contention. Collection runs on a helper;
  the mutating job waits only within that budget. There is at most one such
  collector per context. Overlapping or expired work joins the coalesced
  document backlog, which reads the latest disk contents.
* Sync errors and deadline expiry report pending producers and `lsp_complete:
  false`, never a checked empty result. A quarantined client is reported as
  `diagnostics unknown (server not responding)`; manager contention, warming,
  and unversioned replies stay unknown without falsely blaming a silent server.
  The daemon's edit formatter carries that wording to the agent.
* A pending bind with a known live writer is refused as
  `bind_blocked_by_writer`, naming its job, tool, and age. The same census
  appears in the delayed-bind breadcrumb. Reader attribution remains intact.

The tests cover full-pipe writes and same-process recovery, silent push
diagnostics, manager contention, a real edit followed by configure/bind, the
agent-facing unknown message, and a named writer refusal before the daemon's
relay deadline. Mutation results are recorded in the delivery declaration.

## Negative controls

Each control used the staged live implementation as its restore point. The
working-tree diff was non-empty while mutated and empty after checkout and
touch of the mutated paths. No break was committed.

| Neutralized protection | Expected failing test | Observed failure / unaffected controls |
| --- | --- | --- |
| Stdin acknowledgement deadline | `frozen_lsp_stdin_write_is_bounded_and_recovers_without_restart` | `stdin write bound failed: result=Ok(()) elapsed=3.006644917s`; one failure. The edit-rendering test remained green. |
| Collector caller-side deadline | `post_edit_diagnostics_budget_includes_manager_contention` | `LSP manager contention escaped the diagnostics budget: Timeout`; one failure. The direct stdin bound/recovery test remained green. |
| Unknown result on notification failure | `frozen_lsp_edit_releases_writer_before_route_bind_deadline` | `lsp_complete` became `true` instead of `false`; one failure. The edit and bind still finished in 414 ms, isolating the result-honesty failure from the timing protection. |
| Quarantine after silent diagnostics | `frozen_lsp_diagnostics_timeout_stops_consulting_silent_server` | `diagnostics timeout did not mark the silent server unresponsive`; three other frozen-LSP tests remained green. |
| Live mutating-slot owner census | `bind_overdue_behind_a_mutating_job_names_job_tool_and_age` | `actor_not_ready` instead of `bind_blocked_by_writer`; reader attribution remained green. |

The two collector controls were compiled together but exercised in separate
runs: manager contention expires before notification, whereas the frozen
notification returns its write error before the caller's 500 ms deadline.
Quarantine and writer attribution were also independent controls exercised in
separate suites. Each run had exactly its named expected failure, not a build
failure or an unrelated red test.
