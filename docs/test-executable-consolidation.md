# Integration test executable consolidation

Cargo auto-discovery is disabled for `agent-file-tools`. Five entrypoints under
`crates/aft/tests/{engine,list_envelope,semantic,alert,rest}/main.rs` include the
existing sources as modules. The `engine_*` and `search_b2_*` tests share one executable;
the list-envelope core and conformance directories retain their own structure
inside the list-envelope executable.

Sources stay at their original paths because search-quality descriptors and
source-relative fixture includes refer to them. `RANKING_FENCE_PREFIXES`, the
search-quality descriptor validator, and the inline tool-call parity fixture
path remain unchanged. No descriptor changes class because of this fold.

## Isolation retained

- `integration`: existing general integration harness, including its existing
  process-state synchronization.
- `watcher_integration`: live watchers must not run alongside unrelated
  process-heavy tests; CI also runs this harness single-threaded to avoid
  overlapping live-watcher setup and teardown load.
- `sandbox_launch_probe`: CI runs these probes single-threaded with mandatory
  Landlock enforcement on Linux.
- `semantic_test`: one test changes process cwd before launching its child.
- `semantic_embed_working_set_test` and
  `semantic_embed_overflow_recovery_working_set_test`: each declares a counting
  global allocator and measures the entire process heap.
- `daemon_malloc_small_growth_repro`: ignored macOS measurements also use a
  counting global allocator. Keeping this separate preserves their manual use.

Other ignored benchmarks/probes remain modules in the aggregate harnesses, with
their ignore attributes intact. `compress_spike` and `synapse_live_test` are
ordinary runnable tests, not omitted probes. The navigation profile remains
behind its explicit `aft_views_lazy_benchmark` cfg; the semantic chunk census
remains behind its existing feature. New test sources must be wired into an
entrypoint or explicitly declared in Cargo.toml because auto-discovery is off.

The disk persistence tests launch ignored tests in the current executable.
Their exact child filter now derives from `module_path!()` with the crate name
removed, so it works in both the old integration harness and the new nested
semantic harness. The helper modules are reused rather than compiled twice,
which preserves their test counts too.

## Measured inventory and cold builds

Measurements used the fresh isolated task worktree on macOS, baseline revision
`5db920506eecde299a87b78450130e447e4805a6`, and empty Cargo target directories
before each cold build. Dependencies were not copied from another build. The
Cargo registry/toolchain and machine were shared with other workers.

| Measurement | Before | After |
| --- | ---: | ---: |
| `cargo test -p agent-file-tools --no-run` test executables | 81 | 16 |
| Integration test executables within that count | 77 | 12 |
| Cold `--no-run` wall time | 440.18 s | 1076.11 s |
| Cold build user CPU time | 381.10 s | 317.64 s |
| Cold build system CPU time | 80.24 s | 107.21 s |
| `cargo nextest list -p agent-file-tools` test count | 7305 | 7305 |
| Ignored tests in that inventory | 84 | 84 |

The executable count falls by 65 (80.2%). These wall-time samples are not a
controlled performance comparison: the final cold build ran on a heavily
contended machine. No cold-build wall-time speedup is claimed.

The comparison checks the complete sorted multiset of test names, kinds and
ignore flags, not just counts. Former standalone targets gain their source
module prefix; existing aggregate targets and isolation exceptions keep their
names. The normalized inventories have the same SHA-256:
`382c8c0ea65fdbda87ebb4f0e0abc3d7a45386dcf47dbbb959c41a22ca5eb796`.
Temporarily omitting `compress_spike` made the comparison's
`test_fold_preserves_all_test_names_and_ignore_flags` fail with 7305 versus
7304; the source was restored and the comparison passed again.

The CI inventory proof independently covers all workspace tests exactly once:
4519 lib/bin tests, 2762 nextest integration tests and 27 watcher tests, with no
doctest cases. The LSP nextest group now selects module prefixes rather than the
removed standalone LSP binary names. The compression benchmark command selects
`rest` with the `compress_spike::` filter. Existing scripts and workflows naming
`integration`, `watcher_integration` and `sandbox_launch_probe` remain valid.

### Re-applied on the train 274 tree

The fold was re-applied unchanged to a tree that had gained one top-level
test source since it was written, `tool_provider_conformance.rs`, now a module
of `rest`. It resolves the `aft` binary through `tests/helpers/aft_binary.rs`,
which prefers `AFT_TEST_AFT_BINARY`, then nextest's `NEXTEST_BIN_EXE_aft`, then
Cargo's build-time path, so it keeps working from a relocated nextest archive.
Pointing `AFT_TEST_AFT_BINARY` at a missing file makes its tests fail at
launch, which shows the folded module goes through that helper.
`cargo nextest list -p agent-file-tools --run-ignored all`: 82 to 16 test
executables, 7541 tests and 89 ignored before and after, and the sorted
multiset of (source, test name, ignore flag) is identical once the folded
module prefix is accounted for.

## Verification limitations

All five folded suites pass: alert 15, engine 204, list-envelope 165, rest 218
(with 7 ignored), and semantic 49 (with 3 ignored). Disk persistence tests also
pass in the retained integration harness: 22 passed, 3 ignored. Formatting,
Windows warnings-denied test checking, TypeScript typechecking, search-quality
self-tests, and the independent CI inventory proof pass.

The full Cargo test command and Rust gate are **not green**. The former stopped
on an unchanged library test about duplicate semantic reload workers; the gate
failed on four other unchanged library timing/state tests before reaching its
inventory phase. A bounded all-integration run reported six failures in the
unchanged integration harness (five real-rust-analyzer tests and a shared-DB
latency test), a transient rest migration failure that passed in the separate
folded-suite run, and semantic child tests that were not selected because their
exact filters lacked the new outer module prefix. Deriving those child filters
from the enclosing module path corrected the semantic failures. The run
hit its 20-minute limit when starting the watcher harness. A separate serial
watcher run passed 26 tests and failed the unchanged linked-worktree semantic
quiet-window test. Isolated baseline executables passed the semantic-reload and
one real-rust-analyzer test, consistent with load-sensitive failures, but a full
baseline suite was not rerun. Full gate success therefore remains unverified.

## Fake LSP server

No shared cache was added. After consolidation, caching the fake server can
avoid at most one ordinary executable's per-worktree macOS first-run security
scan (its empty test
harness is another target). That is a much smaller opportunity than the 65
removed harnesses. Consider it only if post-fold measurements still identify
fake-server first execution as a material bottleneck; a shared cache would need
its own invalidation, compatibility and concurrency design.

## Native Windows pre-push slice

Run `scripts/windows-gate.sh` **before** `scripts/train-push.sh` to catch native
Windows x64/MSVC failures without waiting for a train. This is an operator smoke,
not a replacement for three-platform CI; train-push behavior is unchanged.
Python 3, Git, OpenSSH and the provisioned Windows builder must be available.
The VM budget is four vCPUs / 16 GiB: both Cargo jobs and test threads stay at four.

```sh
scripts/windows-gate.sh --plan                     # inspect origin/main..HEAD
scripts/windows-gate.sh --base origin/main         # run touched Rust slices
scripts/windows-gate.sh --full                     # lib + integration
scripts/windows-gate.sh --filter bash_background::persistence
scripts/windows-gate.sh --timeout-minutes 30 --full # default cap: 60 minutes
scripts/windows-gate.sh --status                   # C: free space, target size, lock
```

`AFT_WINDOWS_GATE_SSH_CONFIG` overrides
`~/Work/Projects/CortexKit/prefrontal/script/windows-vm/ssh-config`. Connections
use its `windows-build-vm` alias and current operator key; strict host-key checking
is always on. VM ownership, start/stop and snapshot procedures live in prefrontal's
`docs/runbooks/windows-build-vm.md`. The gate does not change those services.

The printed plan uses committed `git diff --name-only <base>..HEAD` (not the
working tree). Source files select their top-level lib module; integration files
select their named module in the integration harness. Cargo manifests/lockfiles,
build scripts, lib/context/config, executor and db changes broaden to the whole
lib suite. Other touched test harnesses and the aft binary are selected too.
`--filter` overrides the diff with an ad-hoc lib slice; with `--full` it filters
both lib and integration. Empty slices fail rather than silently reporting green.
A diff with no Rust changes explicitly selects nothing; use an override to run.
Every run first executes the production storage/config isolation test.

Only a Git bundle of the exact HEAD and its history reaches the product checkout;
uncommitted/untracked product files are never sent. The gate's PowerShell control
helper is transferred separately, not compiled as product source. A verified
ancestor already in the locked guest checkout permits an incremental bundle;
first use or snapshot rollback automatically uses full history. The guest fetches
and checks out the specified SHA detached in `C:\build\aft\repo`, keeping Cargo's
`C:\build\aft\target` and toolchain/dependency caches warm. The x64 VS dev shell
is sourced for each remote command; provisioning and vm-smoke are never modified.

A guest-side exclusive lock names its holder, start and cap. Before creating run
data or taking that lock, the gate checks `C:\build\maintenance.lock` and refuses
with **VM in maintenance**, exit **75** (test/setup failures use exit 1). The
supervisor rechecks before acquiring its lock; it never creates or removes the
maintenance marker. The VM owner can place that marker, let any existing gate
finish, then maintain/reboot/snapshot the guest. `--status` is read-only and also
reports the marker. Local tests inspect the actual encoded remote command and
fake its exit 75; no marker is created on the real guest by these checks.

Busy runs refuse;
expired abandoned locks are reclaimed, never a still-held lock. The supervisor
bounds transfer/build/tests, kills the complete process tree on timeout/cancel,
and uses a kill-on-close Windows Job Object to cover supervisor/SSH death. Fresh
HOME/USERPROFILE/APPDATA/LOCALAPPDATA, XDG and temp directories are created per
run under `C:\build\aft\runs`; ambient AFT storage/config overrides and injected
Git config are removed. Test fixture Git identity is disposable. Cleanup deletes
run data after stopping descendants, leaving repo and target warm. Abandoned
run directories are reclaimed on a later run. Output streams live and repeats
libtest failing names, panic blocks and compilation/setup failures at the end.

Local control checks: `python3 scripts/lib/test_windows_gate.py`,
`bash -n scripts/windows-gate.sh`, `shellcheck -S warning scripts/windows-gate.sh`.

### Native proof and timing

On the OVH Server 2022 x64 VM, Cargo/Rust **1.99.0 MSVC** and PowerShell **5.1**,
`--filter bash_background::persistence` against origin/main
`0a6eeb56f711a48afbbe1d1600ef152d5b14cca3` produced:

```text
Guest exact commit: 0a6eeb56f711a48afbbe1d1600ef152d5b14cca3
cargo 1.99.0 (5f94df478 2026-08-27)
host: x86_64-pc-windows-msvc
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4939 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 4930 filtered out; finished in 0.03s
GATE PASSED
exit 0
```

A disposable local-only branch added one `#[cfg(windows)]` panic to that module.
The native failure was named in live output and repeated by the end summary:

```text
Guest exact commit: 2c2cf3a13a5b27123fba69412563eb18a00d407f
test bash_background::persistence::windows_gate_deliberate_failure ... FAILED
thread 'bash_background::persistence::windows_gate_deliberate_failure' (3548) panicked at crates\aft\src\bash_background\persistence.rs:2729:5:
NON-VACUITY BREAK: deliberately failing native Windows test
test result: FAILED. 10 passed; 1 failed; 0 ignored; 0 measured; 4930 filtered out; finished in 0.37s
GATE FAILED
exit 1
```

The other ten tests and isolation preflight stayed green. The proof branch/ref
was deleted, its test removed, and the guest reset to the clean task commit with
another passing 1 + 10 test run; no mutant is part of the delivery.

Two further runs of the same filter/commit
`c76726ed1f70129f259d2920c02eb1a4da1734a9` took **207.70 s cold** (empty gate-owned
target, registry cache retained; Cargo build 2m 32s) and **42.65 s warm** (Cargo
0.46s preflight / 0.41s slice). Both exited 0 with 1 + 10 tests passed. Wall time
includes SSH, bundle upload and cleanup. Target reset was done under the guest
lock; the final target was left warm. A live reservation smoke also verified a
busy refusal naming holder/age, a 45s transfer timeout, and lock/run-dir cleanup.

First use found no checkout and initialized it from a full-history bundle.
After the VM's maintenance snapshot, the existing origin/main checkout and
2.52 GiB target survived; the red proof used a verified incremental bundle.
`ssh -G` for both hops then showed the rotated operator key
`~/.ssh/cortexkit_runner_operator_ed25519`; subsequent runs took it from the
SSH config, never from a hardcoded identity option.
