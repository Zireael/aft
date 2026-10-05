# harness: inspect-truth cell gate

Baseline: 0.58.2 (`13dd9cb71`). Both tables use this revision of `run.py score`.

## Collection and verification

- Six fresh, pinned clones were collected inside `target/inspect-truth-corpus` in this worktree; AFT is the local checkout. The after binary is the unchanged Rust implementation at `52f09206136a11e1cd18ebf8348d58200bae6e55`. Baseline and after release binaries were built separately with `CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER=`. The scorer changes, not a liveness fix, are measured here.
- Collection: `python3 benchmarks/inspect-truth/run.py collect --corpus-root target/inspect-truth-corpus --aft-bin target/inspect-truth-after-aft --aft-commit 52f09206136a11e1cd18ebf8348d58200bae6e55 --diagnostics-sample 0`. Interrupted attempts were retried only for the affected project. Every attempt has a fresh storage directory; successful baseline outputs and oracle snapshots are reused, not overwritten. The unsuccessful initial Outline transient records are retained as `aft.failed-transient.json` beside the successful records.
- Both sides: `python3 benchmarks/inspect-truth/run.py score --corpus-root target/inspect-truth-corpus --aft-commit 52f09206136a11e1cd18ebf8348d58200bae6e55 --output-dir target/inspect-truth-score --slice harness`. There are six before entries and seven after entries, with one bucket set per declared language.
- Oracles: fallow 2.88.3, knip 5.85.0, rustc/Cargo 1.99.0, staticcheck 2026.2.1 (0.8.1), x/tools deadcode v0.51.0 (Go 1.26.4), and vulture 2.14. Compiler warnings, raw tool output and normalized oracle snapshots remain under the per-commit result directories. Fallow ran separately in both installed AFT plugins.
- Diagnostics sampling was disabled for this harness slice; the E/W diagnostics pass belongs to its own slice. `harness.json` records the after-output directories and has no row judgments. The harness does not label unreviewed disagreements as judged.
- TypeORM's baseline dead-code aggregate ended in `analysis_incomplete` with a failed builder attempt. The local AFT checkout's dead-code reuse worker exited without publishing a result. These cells are explicitly **unknown**, not empty sets, and do not assert dead-code accuracy. The AFT entry is new and has no before side. The unsupported-language policy is covered by the paired offline fixtures; this unchanged core binary does not yet emit the campaign's Python gap.
- Offline verification: Python 3.9.6, 28 tests passing; Pyright 1.1.411, both changed Python files checked with the audit helper on its import path, 0 errors; Biome 2.4.7, 684 files checked; rustfmt 1.10.0-stable, `cargo fmt --all` and `cargo fmt --all -- --check` passed. Mutation controls include the required unsupported/incomplete pair, judgment validation and category scoping, oracle removal/caching, paging/fallback, baseline pending, truncation, local config restoration, fresh storage, language buckets, the precision cell exception and stderr drainage.
- The mandatory `bash scripts/rust-test-gate.sh` ran serially with other Cargo invocations and failed on unrelated callgraph integration fixtures: `callgraph_callers_cross_file`, `callgraph_aliased_import_resolution`, `callgraph_callers_recursive`, and `callgraph_callers_empty_result` all returned `read_only_store_not_built`. Before that failure, the gate passed 4,870 library tests (57 ignored), 121 binary tests, the TLS test and tokenizer tests; nextest ran 403 tests, with 399 passing and four failing. The watcher/release-storm phases were not reached. No Rust implementation or fixture was changed to hide this worktree baseline failure.

## Before

| repo | language | category | status | AFT | oracle | agreed (+file) | precision | recall | oracle_rows_judged_out | aft_rows_judged | aft_rows_unjudged |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| outline | typescript | unused_exports | scored | 223 | 289 | 221 (+0) | 0.99 | 0.77 | 0 | 0 | 223 |
| outline | typescript | dead_code | scored | 386 | 104 | 81 (+0) | 0.21 | 0.78 | 0 | 0 | 386 |
| outline | typescript | todos | scored | 19 | 20 | 19 (+0) | 1.00 | 0.95 | 0 | 0 | 19 |
| typeorm | typescript | unused_exports | scored | 21 | 61 | 17 (+4) | 1.00 | 0.28 | 0 | 0 | 21 |
| typeorm | typescript | dead_code | unknown | n/a | 9 | n/a (+n/a) | n/a | n/a | 0 | 0 | 0 |
| typeorm | typescript | todos | scored | 10 | 10 | 10 (+0) | 1.00 | 1.00 | 0 | 0 | 10 |
| ripgrep | rust | dead_code | scored | 52 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 52 |
| ripgrep | rust | todos | scored | 11 | 11 | 11 (+0) | 1.00 | 1.00 | 0 | 0 | 11 |
| axum | rust | dead_code | scored | 55 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 55 |
| axum | rust | todos | scored | 5 | 5 | 5 (+0) | 1.00 | 1.00 | 0 | 0 | 5 |
| soft-serve | go | dead_code | scored | 101 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 101 |
| soft-serve | go | dead_code (functions) | scored | 98 | 20 | 5 (+0) | 0.05 | 0.25 | 0 | 0 | 98 |
| soft-serve | go | todos | scored | 33 | 33 | 33 (+0) | 1.00 | 1.00 | 0 | 0 | 33 |
| fastapi | python | dead_code | scored | 0 | 123 | 0 (+0) | n/a | 0.00 | 0 | 0 | 0 |
| fastapi | python | todos | scored | 17 | 17 | 17 (+0) | 1.00 | 1.00 | 0 | 0 | 17 |

- outline raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/outline/13dd9cb71` (corpus `d410db1b24594d71b766212282804d79ca18e5c1`).
- outline baseline_aft_truncated_scopes (excluded from comparison): []
- outline judgments_unapplied: {"harness.json": 0}

- typeorm raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/typeorm/13dd9cb71` (corpus `17e858da8d2becab1ca47826f670a4660da70142`).
- typeorm baseline_aft_truncated_scopes (excluded from comparison): [{"category": "unused_exports", "path": "packages/typeorm/src/driver/mongodb/typings.ts", "count": 134, "returned": 100}]
- typeorm judgments_unapplied: {"harness.json": 0}
- typeorm/typescript/dead_code unknown: {"unavailable": true, "complete": false, "gaps": [{"kind": "analysis_incomplete", "producer": "dead_code analysis (Tier-2)", "reason": "inspect_phase_timeout: tier2 dead_code aggregate did not complete within its phase wait budget; builder_state=last attempt failed: tier2 dead_code aggregate did not complete; builder_state=buildi (attempt 1, first at 1791163067); retry aft_inspect"}]}

- ripgrep raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/ripgrep/13dd9cb71` (corpus `3fce3b5bb0236da2df6d99672afb8a719642eca7`).
- ripgrep baseline_aft_truncated_scopes (excluded from comparison): []
- ripgrep judgments_unapplied: {"harness.json": 0}

- axum raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/axum/13dd9cb71` (corpus `f8b02f22cf10bee707bda19b58265b9e33677535`).
- axum baseline_aft_truncated_scopes (excluded from comparison): []
- axum judgments_unapplied: {"harness.json": 0}

- soft-serve raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/soft-serve/13dd9cb71` (corpus `37685d36f5b7bf0e32217ddd7c8e045c57772619`).
- soft-serve baseline_aft_truncated_scopes (excluded from comparison): []
- soft-serve judgments_unapplied: {"harness.json": 0}

- fastapi raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/fastapi/13dd9cb71` (corpus `26dbe0b00e1211b99446500fc6e402cfec6a3b99`).
- fastapi baseline_aft_truncated_scopes (excluded from comparison): []
- fastapi judgments_unapplied: {"harness.json": 0}

## After

| repo | language | category | status | AFT | oracle | agreed (+file) | precision | recall | oracle_rows_judged_out | aft_rows_judged | aft_rows_unjudged |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| outline | typescript | unused_exports | scored | 223 | 289 | 221 (+0) | 0.99 | 0.77 | 0 | 0 | 223 |
| outline | typescript | dead_code | scored | 386 | 104 | 81 (+0) | 0.21 | 0.78 | 0 | 0 | 386 |
| outline | typescript | todos | scored | 19 | 20 | 19 (+0) | 1.00 | 0.95 | 0 | 0 | 19 |
| typeorm | typescript | unused_exports | scored | 21 | 61 | 17 (+4) | 1.00 | 0.28 | 0 | 0 | 21 |
| typeorm | typescript | dead_code | scored | 75 | 9 | 0 (+4) | 0.05 | 0.00 | 0 | 0 | 75 |
| typeorm | typescript | todos | scored | 10 | 10 | 10 (+0) | 1.00 | 1.00 | 0 | 0 | 10 |
| ripgrep | rust | dead_code | scored | 52 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 52 |
| ripgrep | rust | todos | scored | 11 | 11 | 11 (+0) | 1.00 | 1.00 | 0 | 0 | 11 |
| axum | rust | dead_code | scored | 55 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 55 |
| axum | rust | todos | scored | 5 | 5 | 5 (+0) | 1.00 | 1.00 | 0 | 0 | 5 |
| soft-serve | go | dead_code | scored | 101 | 0 | 0 (+0) | 0.00 | n/a | 0 | 0 | 101 |
| soft-serve | go | dead_code (functions) | scored | 98 | 20 | 5 (+0) | 0.05 | 0.25 | 0 | 0 | 98 |
| soft-serve | go | todos | scored | 33 | 33 | 33 (+0) | 1.00 | 1.00 | 0 | 0 | 33 |
| fastapi | python | dead_code | scored | 0 | 123 | 0 (+0) | n/a | 0.00 | 0 | 0 | 0 |
| fastapi | python | todos | scored | 17 | 17 | 17 (+0) | 1.00 | 1.00 | 0 | 0 | 17 |
| aft | rust | dead_code | unknown | n/a | 0 | n/a (+n/a) | n/a | n/a | 0 | 0 | 0 |
| aft | rust | todos | scored | 0 | 6 | 0 (+0) | n/a | 0.00 | 0 | 0 | 0 |
| aft | typescript | unused_exports | scored | 110 | 159 | 110 (+0) | 1.00 | 0.69 | 0 | 0 | 110 |
| aft | typescript | dead_code | unknown | n/a | 36 | n/a (+n/a) | n/a | n/a | 0 | 0 | 0 |
| aft | typescript | todos | scored | 0 | 0 | 0 (+0) | n/a | n/a | 0 | 0 | 0 |

- outline raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/outline/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `d410db1b24594d71b766212282804d79ca18e5c1`).
- outline baseline_aft_truncated_scopes (excluded from comparison): []
- outline judgments_unapplied: {"harness.json": 0}

- typeorm raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/typeorm/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `17e858da8d2becab1ca47826f670a4660da70142`).
- typeorm baseline_aft_truncated_scopes (excluded from comparison): [{"category": "unused_exports", "path": "packages/typeorm/src/driver/mongodb/typings.ts", "count": 134, "returned": 100}]
- typeorm judgments_unapplied: {"harness.json": 0}

- ripgrep raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/ripgrep/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `3fce3b5bb0236da2df6d99672afb8a719642eca7`).
- ripgrep baseline_aft_truncated_scopes (excluded from comparison): []
- ripgrep judgments_unapplied: {"harness.json": 0}

- axum raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/axum/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `f8b02f22cf10bee707bda19b58265b9e33677535`).
- axum baseline_aft_truncated_scopes (excluded from comparison): []
- axum judgments_unapplied: {"harness.json": 0}

- soft-serve raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/soft-serve/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `37685d36f5b7bf0e32217ddd7c8e045c57772619`).
- soft-serve baseline_aft_truncated_scopes (excluded from comparison): []
- soft-serve judgments_unapplied: {"harness.json": 0}

- fastapi raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/fastapi/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `26dbe0b00e1211b99446500fc6e402cfec6a3b99`).
- fastapi baseline_aft_truncated_scopes (excluded from comparison): []
- fastapi judgments_unapplied: {"harness.json": 0}

- aft raw output: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/6d75dd56448a4a9c/2be5fae243ecd1a69e8a0e26ef5da711b9e2a4b8e31fc3ea5735634ccc2a602f/bg_dispatch_3551a8ce7882fc47a0a03195/target/inspect-truth-corpus/_results/aft/52f09206136a11e1cd18ebf8348d58200bae6e55` (corpus `7b7e5959431a647993f771b89c0ed88cbff88a12`).
- aft baseline_aft_truncated_scopes (excluded from comparison): []
- aft judgments_unapplied: {"harness.json": 0}
- aft/rust/dead_code unknown: {"unavailable": true, "complete": false, "gaps": [{"kind": "analysis_incomplete", "producer": "dead_code analysis (Tier-2)", "reason": "tier2 reuse worker exited without publishing a result; retry aft_inspect"}]}
- aft/typescript/dead_code unknown: {"unavailable": true, "complete": false, "gaps": [{"kind": "analysis_incomplete", "producer": "dead_code analysis (Tier-2)", "reason": "tier2 reuse worker exited without publishing a result; retry aft_inspect"}]}
- aft git status --porcelain before:
```text

```
- aft git status --porcelain after:
```text

```

## Cell rule

- aft: new, no baseline
- PASS: no cell lowered.
