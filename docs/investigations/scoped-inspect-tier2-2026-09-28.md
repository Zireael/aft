# Scoped inspect and Tier-2 request deadlines

## Reproduction

Baseline: `38d5d8c6ae907b98022e32252c60ecb1da2ab6f4` (v0.58.0).
The operator checkout was not touched. An isolated clone of that revision and a
linked worktree of that clone supplied the main-checkout/worktree comparison.
Both contained this repository, not a reduced source fixture. The original
`commands/inspect.rs` was rebuilt for the baseline measurements; the only
addition to that original source was a non-vacuity experiment comment.

Run standalone `aft` with line-delimited JSON on stdin, keeping stdin open until
each response arrives. Configure each root with a separate, initially empty
storage directory, `search_index: false`, `semantic_search: false`,
`callgraph_store: false`, and `inspect.diagnostics_timeout_ms: 15000` in a user
config tier. Use `AFT_LSP_RUST_BINARY=<fake-lsp-server>` and
`AFT_FAKE_LSP_PULL=1` to remove real rust-analyzer startup variance. Send:

```json
{"id":"diagnostics","command":"inspect","scope":"crates/aft/src/commands/inspect.rs","sections":"diagnostics"}
{"id":"default","command":"inspect","scope":"crates/aft/src/commands/inspect.rs"}
```

Measurements, excluding configure (seconds):

| Root | Sections | Original | Patched |
| --- | --- | ---: | ---: |
| Main checkout clone | diagnostics | 10.235 | 2.639 |
| Main checkout clone | omitted | 10.251 | 2.120 |
| Linked worktree | diagnostics | 10.273 | 2.441 |
| Linked worktree | omitted | 10.045 | 1.475 |

All original requests failed in `tier2_rescan`, category `dead_code`, with
`inspect_request_timeout`. The work budget was 10 seconds because of the
5-second terminal reserve. Each completed `lsp_start` and `lsp_quiescence` for
Rust; the second linked-worktree request also completed the unused-exports
Tier-2 phase. For example:

```text
inspect_request_timeout: tier2_rescan could not complete within the 15000ms request budget (5000ms terminal reserve)
```

All patched requests succeeded with `complete: false` and named Tier-2 gaps.
Their `wait_stamp.phases` contained `lsp_start`, `lsp_quiescence`, and
`stat_verification`; no `tier2_rescan` or `callgraph_ready` phase ran.
The fake server had not analyzed the requested file, so diagnostics correctly
included an `uncovered_file` gap too. These measurements establish orchestration
behavior, not real rust-analyzer latency. In particular the failure reproduces
with the callgraph builder disabled: a cold callgraph build is not necessary.

## Cause and cache behavior

References below refer to the implementation in this change (manager/context
references are unchanged from the baseline):

- `crates/aft/src/commands/inspect.rs:430`: `sections` is parsed as drill-down
  selection, not an execution-category filter. Default sections mean summary
  only, not diagnostics only. The loop at line 478 previously started every
  active Tier-2 category whenever `ctx.inspect_writer()` was true, regardless
  of scope or sections. It calls `tier2_run_with_reuse_blocking_fresh` at line
  527, then receives completion at line 572 under the shared request deadline.
- `crates/aft/src/inspect/manager.rs:2249`: Tier-2 expands the job to project
  scope before doing reuse/scanning. A scoped output does not limit its work.
  At lines 2292–2325 a cache miss can also wait for interactive cold-build
  admission; competing cold builds can therefore worsen this latency.
- `crates/aft/src/commands/configure.rs:3380`: callgraph writer capability is
  disabled for worktree bridges, but inspect writer capability is enabled on
  local storage regardless of worktree status. A borrow-only callgraph does
  **not** imply a read-only inspect cache.
- `crates/aft/src/context.rs:4936`: inspect storage is keyed by the canonical
  root's project-scope key. Worktrees do not simply borrow the owner's inspect
  aggregate. At line 5011 automatic Tier-2 refresh is disabled for worktrees,
  making cold scoped requests particularly likely to have no aggregate yet.
- `crates/aft/src/inspect/manager.rs:2277`: unchanged jobs can quick-reuse their
  own cache; worktrees do not intrinsically force a full rescan on every call.
  No owner-cache sharing or writer-capability changes are necessary for this fix.

## Behavior change and regression coverage

Scoped requests now use `tier2_read_cached_readonly`, never schedule or join
Tier-2 work. Its existing stat-verification and scope-projection path
(`manager.rs:2039–2170`) determines whether a cached aggregate is usable.
Unavailable or stale results become category-specific `tier2_unavailable` gaps,
without count fields or empty findings lists. Worktree misses explicitly name
worktree unavailability. Unscoped requests retain blocking-fresh behavior.
This remains a cached/partial summary contract, not a change to the meaning of
`sections` as drill-down selection.

`inspect_command_test::scoped_inspect_does_not_wait_for_blocked_tier2` creates a
real Git worktree, stalls an explicit Tier-2 run in that worktree with the
existing file-release hook, and calls the public blocking inspect handler with
both diagnostics sections and default sections in both roots. Responses must
arrive before the gate is released, retain diagnostics, name unavailable
Tier-2 categories without zero counts, and contain actual completed phase logs
with no Tier-2 phases. Reinstating the original unconditional writer loop makes
only this named test fail with `scoped inspect waited for blocked Tier-2:
Timeout`; restoring the bypass makes it pass.

Three older diagnostics tests asserted whole-response completeness after
proving diagnostics coverage. They now assert diagnostics completeness/absence
of uncovered-file gaps instead: missing Tier-2 analysis may legitimately make
the overall scoped response incomplete. Their findings and coverage assertions
are preserved.

Remaining costs are explicit: LSP startup/quiescence still runs, Tier-1 scans
still run, root stat verification still runs, and available Tier-2 caches still
undergo their existing freshness checks. This change removes Tier-2 rescan and
cold-build waits; it does not promise every scoped request is instantaneous.
