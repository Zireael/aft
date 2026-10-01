# Split query: measured replay before the mention-host change

Gate configuration: call graph off (AFT_SEARCH_BENCH_CALLGRAPH unset), AFT_SEARCH_SPLIT_TRACE set, the pinned evidence tree and vector packs of the benchmark harness. One aft process per build and manifest; each row sent once as a raw `semantic_search` request with top_k 50 (the harness's paged requests give the same order: every replay passed page invariance in the harness).

Builds:
- **slot**: tag `split-slot-baseline` (809d13ce0), release build. It emits no split trace.
- **query-primary**: release build of 40685f332; the later commits up to 0f58a6137 change comments, docs, the descriptor and list-surface exclusions only, no ranking code.
- **query-only**: the same request without `pattern`, sent to the query-primary process (identical to the slot process's prose-only lists).

Ranks are 1-based within the first 10 collapsed paths; '-' means not in the first 10.

## gate rows

| row | kind | answer | query-only | slot | query-primary |
|---|---|---|---|---|---|
| 910001 | R1 | commands/ast_search.rs | - | - | - |
| 910002 | R1 | bash_background/registry.rs | 10 | 7 | 10 |
| 910003 | R2 | subc/mod.rs | 4 | 4 | 4 |
| 910004 | R2 | snapshot/mod.rs | - | - | - |
| 910005 | R3 | compress/mod.rs | 6 | 1 | 6 |
| 910006 | R4 | lsp/manager.rs | 9 | 1 | 5 |
| 910007 | R4 | subc/mod.rs | 4 | 5 | 2 |
| 910008 | R5 | commands/ast_search.rs | - | - | - |
| 910009 | R5 | subc/mod.rs | 4 | 4 | 4 |
| 910010 | R6 | commands/ast_search.rs | - | 1 | - |
| 910011 | R7 | inspect/manager.rs | 1 | 1 | 1 |
| 910012 | R7 | commands/status.rs | 3 | 4 | 2 |

### followup-census:910001  pattern `ast_grep_search`  answer `crates/aft/src/commands/ast_search.rs`

prose_only.ranked_paths (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

slot ranked (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/search_index.rs | 0 | 0.00000 | 0.00000 |  |  |
| 2 | src/main.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | src/runtime_drain.rs | 2 | 0.00000 | 0.00000 |  |  |
| 4 | callgraph_store/mod.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | src/logging.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | src/callgraph.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | commands/ast_replace.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | src/parser.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | src/cold_build_limiter.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | semantic_search/mod.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

### followup-census:910002  pattern `subagent_type`  answer `crates/aft/src/bash_background/registry.rs`

prose_only.ranked_paths (first 10): `corpora/codegraph.json`, `results/aft-grep-codegraph-2026-05-26T09-41-35-269Z.json`, `results/ripgrep-codegraph-2026-05-26T09-41-56-635Z.json`, `bash_background/watches.rs`, `bash_background/process.rs`, `commands/bash_orchestrate.rs`, `bash_background/persistence.rs`, `integration/bash_background_persistence_test.rs`, `integration/bash_background_test.rs`, `bash_background/registry.rs`

slot ranked (first 10): `bash_background/watches.rs`, `bash_background/process.rs`, `commands/bash_orchestrate.rs`, `bash_background/persistence.rs`, `integration/bash_background_persistence_test.rs`, `integration/bash_background_test.rs`, `bash_background/registry.rs`, `bash_background/mod.rs`, `commands/bash_status.rs`, `tools/bash.ts`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | corpora/codegraph.json | 0 | 0.00000 | 0.00000 |  |  |
| 2 | results/aft-grep-codegraph-2026-05-26T09-41-35-269Z.json | 1 | 0.00000 | 0.00000 |  |  |
| 3 | results/ripgrep-codegraph-2026-05-26T09-41-56-635Z.json | 2 | 0.00000 | 0.00000 |  |  |
| 4 | bash_background/watches.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | bash_background/process.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | commands/bash_orchestrate.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | bash_background/persistence.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | integration/bash_background_persistence_test.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | integration/bash_background_test.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | bash_background/registry.rs **(answer)** | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `corpora/codegraph.json`, `results/aft-grep-codegraph-2026-05-26T09-41-35-269Z.json`, `results/ripgrep-codegraph-2026-05-26T09-41-56-635Z.json`, `bash_background/watches.rs`, `bash_background/process.rs`, `commands/bash_orchestrate.rs`, `bash_background/persistence.rs`, `integration/bash_background_persistence_test.rs`, `integration/bash_background_test.rs`, `bash_background/registry.rs`

### followup-census:910003  pattern `Error|Result`  answer `crates/aft/src/subc/mod.rs`

prose_only.ranked_paths (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

slot ranked (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `callgraph_store/mod.rs`, `lsp/client.rs`, `src/context.rs`, `src/revivable-transport.ts`, `lsp/manager.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 | 0.00000 |  |  |
| 2 | src/runtime_drain.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | src/subc-transport.ts | 2 | 0.00000 | 0.00000 |  |  |
| 4 | subc/mod.rs **(answer)** | 3 | 0.00000 | 0.00000 |  |  |
| 5 | subc/health.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | lsp/client.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | src/revivable-transport.ts | 6 | 0.00000 | 0.00000 |  |  |
| 8 | src/context.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | lsp/manager.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | src/watcher_filter.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

### followup-census:910004  pattern `Error|Result`  answer `crates/aft/src/hashline/snapshot/mod.rs`

prose_only.ranked_paths (first 10): `drafts/2026-08-09-hashline-edit-surface-refire4.md`, `transaction/mod.rs`, `src/backup.rs`, `src/checkpoint.rs`, `commands/restore_checkpoint.rs`, `src/context.rs`, `commands/edit_match.rs`, `src/config.rs`, `apply/mod.rs`, `src/root_cache.rs`

slot ranked (first 10): `src/backup.rs`, `transaction/mod.rs`, `src/checkpoint.rs`, `commands/restore_checkpoint.rs`, `src/context.rs`, `commands/edit_match.rs`, `src/config.rs`, `apply/mod.rs`, `src/root_cache.rs`, `src/search_index.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | drafts/2026-08-09-hashline-edit-surface-refire4.md | 0 | 0.00000 | 0.00000 |  |  |
| 2 | transaction/mod.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | src/backup.rs | 2 | 0.00000 | 0.00000 |  |  |
| 4 | src/checkpoint.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | commands/restore_checkpoint.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | src/context.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | commands/edit_match.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | src/config.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | apply/mod.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | src/root_cache.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `drafts/2026-08-09-hashline-edit-surface-refire4.md`, `transaction/mod.rs`, `src/backup.rs`, `src/checkpoint.rs`, `commands/restore_checkpoint.rs`, `src/context.rs`, `commands/edit_match.rs`, `src/config.rs`, `apply/mod.rs`, `src/root_cache.rs`

### followup-census:910005  pattern `^pub struct (FormatContext|HostEscalationAttempt|PinOwner|ServerKey|LspManager|LspChildRegistry|FileId|InspectSnapshot|DispatchHandles|SwiftSyntax|PhpSyntax|KotlinSyntax|CSharpSyntax|CSyntax|ExternalToolResult|WriteResult|FileFreshness|AlertEngine|SemanticIndexFingerprint|SearchIndex|ShapeWeights|ListEnvelope|LockGuard|AppContext|Config|TomlFilter|PytestCompressor|CargoCompressor|BunCompressor|OutputProbe)\b`  answer `crates/aft/src/compress/mod.rs`

prose_only.ranked_paths (first 10): `tools/bash.ts`, `compress/vitest.rs`, `examples/tsc_compression_perf_probe.rs`, `bash_background/registry.rs`, `compression-tokens/spike.ts`, `compress/mod.rs`, `compress/go.rs`, `compress/cargo.rs`, `examples/compression_dispatch_bench.rs`, `compress/toml_filter.rs`

slot ranked (first 10): `compress/mod.rs`, `compress/cargo.rs`, `compress/toml_filter.rs`, `tools/bash.ts`, `compress/vitest.rs`, `examples/tsc_compression_perf_probe.rs`, `bash_background/registry.rs`, `compression-tokens/spike.ts`, `compress/go.rs`, `examples/compression_dispatch_bench.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | tools/bash.ts | 0 | 0.00000 | 0.00000 |  |  |
| 2 | compress/vitest.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | examples/tsc_compression_perf_probe.rs | 2 | 0.00000 | 0.00000 |  |  |
| 4 | bash_background/registry.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | compression-tokens/spike.ts | 4 | 0.00000 | 0.00000 |  |  |
| 6 | compress/mod.rs **(answer)** | 5 | 0.00000 | 0.00000 |  |  |
| 7 | compress/go.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | compress/cargo.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | examples/compression_dispatch_bench.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | compress/toml_filter.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `tools/bash.ts`, `compress/vitest.rs`, `examples/tsc_compression_perf_probe.rs`, `bash_background/registry.rs`, `compression-tokens/spike.ts`, `compress/mod.rs`, `compress/go.rs`, `compress/cargo.rs`, `examples/compression_dispatch_bench.rs`, `compress/toml_filter.rs`

### followup-census:910006  pattern `shutdown_all`  answer `crates/aft/src/lsp/manager.rs`

prose_only.ranked_paths (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

slot ranked (first 10): `lsp/manager.rs`, `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/context.rs`, `src/revivable-transport.ts`, `src/watcher_filter.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 | 0.00000 |  |  |
| 2 | subc/mod.rs | 3 | 0.00060 | 0.00000 |  |  |
| 3 | src/runtime_drain.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/subc-transport.ts | 2 | 0.00000 | 0.00000 |  |  |
| 5 | lsp/manager.rs **(answer)** | 8 | 0.00120 | 0.00000 |  |  |
| 6 | subc/health.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | src/context.rs | 7 | 0.00060 | 0.00000 |  |  |
| 8 | lsp/client.rs | 5 | 0.00000 | 0.00000 |  |  |
| 9 | src/revivable-transport.ts | 6 | 0.00000 | 0.00000 |  |  |
| 10 | src/main.rs | 12 | 0.00060 | 0.00000 |  |  |

query-primary ranked (first 10): `src/error-contract.ts`, `subc/mod.rs`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `lsp/manager.rs`, `subc/health.rs`, `src/context.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/main.rs`

### followup-census:910007  pattern `shutdown_all`  answer `crates/aft/src/subc/mod.rs`

prose_only.ranked_paths (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

slot ranked (first 10): `lsp/manager.rs`, `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/context.rs`, `src/revivable-transport.ts`, `src/watcher_filter.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 | 0.00000 |  |  |
| 2 | subc/mod.rs **(answer)** | 3 | 0.00060 | 0.00000 |  |  |
| 3 | src/runtime_drain.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/subc-transport.ts | 2 | 0.00000 | 0.00000 |  |  |
| 5 | lsp/manager.rs | 8 | 0.00120 | 0.00000 |  |  |
| 6 | subc/health.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | src/context.rs | 7 | 0.00060 | 0.00000 |  |  |
| 8 | lsp/client.rs | 5 | 0.00000 | 0.00000 |  |  |
| 9 | src/revivable-transport.ts | 6 | 0.00000 | 0.00000 |  |  |
| 10 | src/main.rs | 12 | 0.00060 | 0.00000 |  |  |

query-primary ranked (first 10): `src/error-contract.ts`, `subc/mod.rs`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `lsp/manager.rs`, `subc/health.rs`, `src/context.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/main.rs`

### followup-census:910008  pattern ``  answer `crates/aft/src/commands/ast_search.rs`

prose_only.ranked_paths (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

slot ranked (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

query-primary: no split trace (blank pattern runs as query alone).

query-primary ranked (first 10): `src/search_index.rs`, `src/main.rs`, `src/runtime_drain.rs`, `callgraph_store/mod.rs`, `src/logging.rs`, `src/callgraph.rs`, `commands/ast_replace.rs`, `src/parser.rs`, `src/cold_build_limiter.rs`, `semantic_search/mod.rs`

### followup-census:910009  pattern `   `  answer `crates/aft/src/subc/mod.rs`

prose_only.ranked_paths (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

slot ranked (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

query-primary: no split trace (blank pattern runs as query alone).

query-primary ranked (first 10): `src/error-contract.ts`, `src/runtime_drain.rs`, `src/subc-transport.ts`, `subc/mod.rs`, `subc/health.rs`, `lsp/client.rs`, `src/revivable-transport.ts`, `src/context.rs`, `lsp/manager.rs`, `src/watcher_filter.rs`

### followup-census:910010  pattern `handle_ast_search`  answer `crates/aft/src/commands/ast_search.rs`

prose_only.ranked_paths (first 10): `release-notes/v0.37.0.md`, `release-notes/v0.43.0.md`, `src/workflow-hints.ts`, `release-notes/v0.36.0.md`, `src/workflow-hints.ts`, `release-notes/v0.33.0.md`, `audits/agent-text-habit-audit-2026-07.md`, `release-notes/v0.20.0.md`, `S03/S03-RESEARCH.md`, `S04/S04-RESEARCH.md`

slot ranked (first 10): `commands/ast_search.rs`, `release-notes/v0.37.0.md`, `release-notes/v0.43.0.md`, `src/workflow-hints.ts`, `release-notes/v0.36.0.md`, `src/workflow-hints.ts`, `release-notes/v0.33.0.md`, `audits/agent-text-habit-audit-2026-07.md`, `release-notes/v0.20.0.md`, `S03/S03-RESEARCH.md`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | release-notes/v0.37.0.md | 0 | 0.00000 | 0.00000 |  |  |
| 2 | release-notes/v0.43.0.md | 1 | 0.00000 | 0.00000 |  |  |
| 3 | src/workflow-hints.ts | 2 | 0.00000 | 0.00000 |  |  |
| 4 | release-notes/v0.36.0.md | 3 | 0.00000 | 0.00000 |  |  |
| 5 | src/workflow-hints.ts | 4 | 0.00000 | 0.00000 |  |  |
| 6 | release-notes/v0.33.0.md | 5 | 0.00000 | 0.00000 |  |  |
| 7 | audits/agent-text-habit-audit-2026-07.md | 6 | 0.00000 | 0.00000 |  |  |
| 8 | release-notes/v0.20.0.md | 7 | 0.00000 | 0.00000 |  |  |
| 9 | S03/S03-RESEARCH.md | 8 | 0.00000 | 0.00000 |  |  |
| 10 | S04/S04-RESEARCH.md | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `release-notes/v0.37.0.md`, `release-notes/v0.43.0.md`, `src/workflow-hints.ts`, `release-notes/v0.36.0.md`, `src/workflow-hints.ts`, `release-notes/v0.33.0.md`, `audits/agent-text-habit-audit-2026-07.md`, `release-notes/v0.20.0.md`, `S03/S03-RESEARCH.md`, `S04/S04-RESEARCH.md`

### followup-census:910011  pattern `Tier2PhaseTimings|Error`  answer `crates/aft/src/inspect/manager.rs`

prose_only.ranked_paths (first 10): `inspect/manager.rs`, `src/context.rs`, `src/logging.rs`, `src/runtime_drain.rs`, `inspect/cache.rs`, `inspect/phase_log.rs`, `subc/health.rs`, `commands/inspect.rs`, `inspect/tier2_scheduler.rs`, `inspect/job.rs`

slot ranked (first 10): `inspect/manager.rs`, `src/context.rs`, `src/logging.rs`, `src/runtime_drain.rs`, `inspect/cache.rs`, `inspect/phase_log.rs`, `subc/health.rs`, `commands/inspect.rs`, `inspect/tier2_scheduler.rs`, `inspect/job.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | inspect/manager.rs **(answer)** | 0 | 0.00120 | 0.00000 |  |  |
| 2 | src/logging.rs | 2 | 0.00060 | 0.00000 |  |  |
| 3 | src/context.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/runtime_drain.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | inspect/cache.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | inspect/phase_log.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | subc/health.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | commands/inspect.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | inspect/tier2_scheduler.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | inspect/job.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `inspect/manager.rs`, `src/logging.rs`, `src/context.rs`, `src/runtime_drain.rs`, `inspect/cache.rs`, `inspect/phase_log.rs`, `subc/health.rs`, `commands/inspect.rs`, `inspect/tier2_scheduler.rs`, `inspect/job.rs`

### followup-census:910012  pattern `tracked_files|Error`  answer `crates/aft/src/commands/status.rs`

prose_only.ranked_paths (first 10): `src/protocol.rs`, `commands/restore_checkpoint.rs`, `commands/status.rs`, `src/checkpoint.rs`, `subc/mod.rs`, `commands/checkpoint.rs`, `src/backup.rs`, `src/protocol.ts`, `src/context.rs`, `lsp/manager.rs`

slot ranked (first 10): `src/backup.rs`, `src/protocol.rs`, `commands/restore_checkpoint.rs`, `commands/status.rs`, `src/checkpoint.rs`, `subc/mod.rs`, `commands/checkpoint.rs`, `src/context.rs`, `src/protocol.ts`, `lsp/manager.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/protocol.rs | 0 | 0.00038 | 0.00000 |  |  |
| 2 | commands/status.rs **(answer)** | 2 | 0.00038 | 0.00000 |  |  |
| 3 | commands/restore_checkpoint.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/checkpoint.rs | 3 | 0.00038 | 0.00000 |  |  |
| 5 | src/backup.rs | 6 | 0.00075 | 0.00000 |  |  |
| 6 | commands/checkpoint.rs | 5 | 0.00038 | 0.00000 |  |  |
| 7 | subc/mod.rs | 4 | 0.00000 | 0.00000 |  |  |
| 8 | src/protocol.ts | 7 | 0.00000 | 0.00000 |  |  |
| 9 | src/context.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | lsp/manager.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `src/protocol.rs`, `commands/status.rs`, `commands/restore_checkpoint.rs`, `src/checkpoint.rs`, `src/backup.rs`, `commands/checkpoint.rs`, `subc/mod.rs`, `src/protocol.ts`, `src/context.rs`, `lsp/manager.rs`

## tuning rows

| row | kind | answer | query-only | slot | query-primary |
|---|---|---|---|---|---|
| 920001 | R2 | cli/profile.rs | - | 1 | - |
| 920002 | R7 | src/main.rs | 6 | 6 | 4 |
| 920003 | R2 | commands/edit_match.rs | - | - | 1 |
| 920004 | R7 | commands/configure.rs | - | - | - |
| 920005 | R5 | src/search_index.rs | - | - | - |
| 920006 | R4 | integration/subc_detach_test.rs | 1 | 2 | 1 |
| 920007 | R7 | src/context.rs | - | - | - |
| 920008 | R2 | subc/mod.rs | 7 | 1 | 7 |
| 920009 | R4 | subc/mod.rs | - | 5 | - |
| 920010 | R4 | commands/bash_drain_completions.rs | 1 | 2 | 1 |
| 920011 | R4 | subc/health.rs | 3 | 3 | 2 |

### followup-census:920001  pattern `remove_file|Error`  answer `crates/aft/src/cli/profile.rs`

prose_only.ranked_paths (first 10): `release-notes/v0.47.0.md`, `docs/v0.49-unified-tool-surface-spec.md`, `ARCHITECTURE.md`, `src/onnx-runtime.ts`, `release-notes/v0.26.1.md`, `src/lsp-github-install.ts`, `investigations/path-resolution-audit-2026-09.md`, `drafts/2026-09-09-aft-search-quality-b1-real-query-benchmark-manifest-gate-baseline-and-remeasure.md`, `src/lsp-github-install.ts`, `release-notes/v0.49.0.md`

slot ranked (first 10): `cli/profile.rs`, `src/semantic_index.rs`, `Cargo.toml`, `src/parser.rs`, `src/context.rs`, `src/config_resolve.rs`, `callgraph_store/mod.rs`, `compress/mod.rs`, `commands/callgraph_store_adapter.rs`, `src/sandbox_profile.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | release-notes/v0.47.0.md | 0 | 0.00000 | 0.00000 |  |  |
| 2 | docs/v0.49-unified-tool-surface-spec.md | 1 | 0.00000 | 0.00000 |  |  |
| 3 | ARCHITECTURE.md | 2 | 0.00000 | 0.00000 |  |  |
| 4 | src/onnx-runtime.ts | 3 | 0.00000 | 0.00000 |  |  |
| 5 | src/semantic_index.rs | 22 | 0.00001 | 0.00000 | yes |  |
| 6 | src/search_index.rs | 24 | 0.00001 | 0.00000 | yes |  |
| 7 | inspect/manager.rs | 67 | 0.00001 | 0.00000 | yes |  |
| 8 | bash_background/persistence.rs | 125 | 0.00001 | 0.00000 | yes |  |
| 9 | release-notes/v0.26.1.md | 4 | 0.00000 | 0.00000 |  |  |
| 10 | src/lsp-github-install.ts | 5 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `release-notes/v0.47.0.md`, `docs/v0.49-unified-tool-surface-spec.md`, `ARCHITECTURE.md`, `src/onnx-runtime.ts`, `src/semantic_index.rs`, `src/search_index.rs`, `inspect/manager.rs`, `bash_background/persistence.rs`, `release-notes/v0.26.1.md`, `src/lsp-github-install.ts`

### followup-census:920002  pattern `set_progress_sender|Result`  answer `crates/aft/src/main.rs`

prose_only.ranked_paths (first 10): `src/context.rs`, `subc/mod.rs`, `commands/configure.rs`, `src/bg-notifications.ts`, `subc/wire.rs`, `src/main.rs`, `bash_background/registry.rs`, `src/runtime_drain.rs`, `src/bg-notifications.ts`, `src/index.ts`

slot ranked (first 10): `src/context.rs`, `subc/mod.rs`, `commands/configure.rs`, `subc/wire.rs`, `src/bg-notifications.ts`, `src/main.rs`, `bash_background/registry.rs`, `src/runtime_drain.rs`, `src/bg-notifications.ts`, `src/index.ts`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/context.rs | 0 | 0.00120 | 0.00000 |  |  |
| 2 | subc/mod.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | commands/configure.rs | 2 | 0.00000 | 0.00000 |  |  |
| 4 | src/main.rs **(answer)** | 5 | 0.00060 | 0.00000 |  |  |
| 5 | src/bg-notifications.ts | 3 | 0.00000 | 0.00000 |  |  |
| 6 | subc/wire.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | src/runtime_drain.rs | 7 | 0.00060 | 0.00000 |  |  |
| 8 | bash_background/registry.rs | 6 | 0.00000 | 0.00000 |  |  |
| 9 | src/bg-notifications.ts | 8 | 0.00000 | 0.00000 |  |  |
| 10 | src/index.ts | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `src/context.rs`, `subc/mod.rs`, `commands/configure.rs`, `src/main.rs`, `src/bg-notifications.ts`, `subc/wire.rs`, `src/runtime_drain.rs`, `bash_background/registry.rs`, `src/bg-notifications.ts`, `src/index.ts`

### followup-census:920003  pattern `format_skip_reasons|Result`  answer `crates/aft/src/commands/edit_match.rs`

prose_only.ranked_paths (first 10): `src/bg-notifications.ts`, `tools/hoisted.ts`, `commands/apply_patch.rs`, `src/subc_format.rs`, `src/format.rs`, `src/backup.rs`, `src/bg-notifications.ts`, `tui/sidebar.tsx`, `cli/index.rs`, `src/edit.rs`

slot ranked (first 10): `tools/hoisted.ts`, `src/bg-notifications.ts`, `commands/apply_patch.rs`, `src/subc_format.rs`, `src/format.rs`, `src/backup.rs`, `src/bg-notifications.ts`, `cli/index.rs`, `tui/sidebar.tsx`, `src/edit.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | commands/edit_match.rs **(answer)** | 124 | 0.00120 | 0.00000 | yes |  |
| 2 | src/bg-notifications.ts | 0 | 0.00000 | 0.00000 |  |  |
| 3 | src/subc_format.rs | 3 | 0.00060 | 0.00000 |  |  |
| 4 | tools/hoisted.ts | 1 | 0.00000 | 0.00000 |  |  |
| 5 | commands/apply_patch.rs | 2 | 0.00000 | 0.00000 |  |  |
| 6 | src/format.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | src/backup.rs | 5 | 0.00000 | 0.00000 |  |  |
| 8 | src/bg-notifications.ts | 6 | 0.00000 | 0.00000 |  |  |
| 9 | tui/sidebar.tsx | 7 | 0.00000 | 0.00000 |  |  |
| 10 | cli/index.rs | 8 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `commands/edit_match.rs`, `src/bg-notifications.ts`, `src/subc_format.rs`, `tools/hoisted.ts`, `commands/apply_patch.rs`, `src/format.rs`, `src/backup.rs`, `src/bg-notifications.ts`, `tui/sidebar.tsx`, `cli/index.rs`

### followup-census:920004  pattern `build_status_snapshot|Error`  answer `crates/aft/src/commands/configure.rs`

prose_only.ranked_paths (first 10): `release-notes/v0.52.0.md`, `src/build_breaker.rs`, `commands/doctor.ts`, `commands/status.rs`, `src/context.rs`, `subc/health.rs`, `src/runtime_drain.rs`, `inspect/manager.rs`, `src/fs_lock.rs`, `src/bg-notifications.ts`

slot ranked (first 10): `src/build_breaker.rs`, `commands/status.rs`, `commands/doctor.ts`, `src/context.rs`, `src/runtime_drain.rs`, `subc/health.rs`, `inspect/manager.rs`, `src/fs_lock.rs`, `src/bg-notifications.ts`, `lib/build-breaker.ts`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | release-notes/v0.52.0.md | 0 | 0.00000 | 0.00000 |  |  |
| 2 | commands/status.rs | 3 | 0.00120 | 0.00000 |  |  |
| 3 | src/build_breaker.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | commands/doctor.ts | 2 | 0.00000 | 0.00000 |  |  |
| 5 | src/runtime_drain.rs | 6 | 0.00060 | 0.00000 |  |  |
| 6 | src/context.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | subc/health.rs | 5 | 0.00000 | 0.00000 |  |  |
| 8 | inspect/manager.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | src/fs_lock.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | src/bg-notifications.ts | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `release-notes/v0.52.0.md`, `commands/status.rs`, `src/build_breaker.rs`, `commands/doctor.ts`, `src/runtime_drain.rs`, `src/context.rs`, `subc/health.rs`, `inspect/manager.rs`, `src/fs_lock.rs`, `src/bg-notifications.ts`

### followup-census:920005  pattern ``  answer `crates/aft/src/search_index.rs`

prose_only.ranked_paths (first 10): `src/checkpoint.rs`, `migration/mod.rs`, `oxc_engine/mod.rs`, `scanners/dead_code.rs`, `views/mod.rs`, `commands/outline.rs`, `scanners/unused_exports.rs`, `commands/callgraph_store_adapter.rs`, `src/standing_roots.rs`, `src/context.rs`

slot ranked (first 10): `src/checkpoint.rs`, `migration/mod.rs`, `oxc_engine/mod.rs`, `scanners/dead_code.rs`, `views/mod.rs`, `commands/outline.rs`, `scanners/unused_exports.rs`, `commands/callgraph_store_adapter.rs`, `src/standing_roots.rs`, `src/context.rs`

query-primary: no split trace (blank pattern runs as query alone).

query-primary ranked (first 10): `src/checkpoint.rs`, `migration/mod.rs`, `oxc_engine/mod.rs`, `scanners/dead_code.rs`, `views/mod.rs`, `commands/outline.rs`, `scanners/unused_exports.rs`, `commands/callgraph_store_adapter.rs`, `src/standing_roots.rs`, `src/context.rs`

### followup-census:920006  pattern `run_subc_mode_for_test`  answer `crates/aft/tests/integration/subc_detach_test.rs`

prose_only.ranked_paths (first 10): `integration/subc_detach_test.rs`, `src/context.rs`, `tests/gh_shim_runtime_context_test.rs`, `integration/subc_bridge_test.rs`, `integration/lsp_diagnostics_test.rs`, `integration/bash_background_persistence_test.rs`, `subc/mod.rs`, `tests/sandbox_launch_probe.rs`, `integration/branch_switch_test.rs`, `lsp/manager.rs`

slot ranked (first 10): `subc/mod.rs`, `integration/subc_detach_test.rs`, `src/context.rs`, `integration/subc_bridge_test.rs`, `tests/gh_shim_runtime_context_test.rs`, `integration/lsp_diagnostics_test.rs`, `integration/bash_background_persistence_test.rs`, `tests/sandbox_launch_probe.rs`, `integration/branch_switch_test.rs`, `lsp/manager.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | integration/subc_detach_test.rs **(answer)** | 0 | 0.00000 | 0.00000 |  |  |
| 2 | integration/subc_bridge_test.rs | 3 | 0.00060 | 0.00000 |  |  |
| 3 | src/context.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | subc/mod.rs | 6 | 0.00120 | 0.00000 |  |  |
| 5 | tests/gh_shim_runtime_context_test.rs | 2 | 0.00000 | 0.00000 |  |  |
| 6 | integration/lsp_diagnostics_test.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | integration/bash_background_persistence_test.rs | 5 | 0.00000 | 0.00000 |  |  |
| 8 | tests/sandbox_launch_probe.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | integration/branch_switch_test.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | lsp/manager.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `integration/subc_detach_test.rs`, `integration/subc_bridge_test.rs`, `src/context.rs`, `subc/mod.rs`, `tests/gh_shim_runtime_context_test.rs`, `integration/lsp_diagnostics_test.rs`, `integration/bash_background_persistence_test.rs`, `tests/sandbox_launch_probe.rs`, `integration/branch_switch_test.rs`, `lsp/manager.rs`

### followup-census:920007  pattern `mark_file_diagnostics_stale|Error`  answer `crates/aft/src/context.rs`

prose_only.ranked_paths (first 10): `release-notes/v0.30.2.md`, `release-notes/v0.44.0.md`, `ARCHITECTURE.md`, `release-notes/v0.22.0.md`, `release-notes/v0.26.1.md`, `release-notes/v0.36.1.md`, `release-notes/v0.39.4.md`, `release-notes/v0.49.1.md`, `reports/verbatim-audit.md`, `src/fuzzy_match.rs`

slot ranked (first 10): `src/backup.rs`, `lsp/document.rs`, `src/alert_state.rs`, `lsp/diagnostics.rs`, `integration/binding.rs`, `inspect/diagnostics_category.rs`, `commands/lsp_diagnostics.rs`, `lsp/manager.rs`, `commands/inspect.rs`, `src/edit.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | release-notes/v0.30.2.md | 0 | 0.00000 | 0.00000 |  |  |
| 2 | release-notes/v0.44.0.md | 1 | 0.00000 | 0.00000 |  |  |
| 3 | ARCHITECTURE.md | 2 | 0.00000 | 0.00000 |  |  |
| 4 | release-notes/v0.22.0.md | 3 | 0.00000 | 0.00000 |  |  |
| 5 | release-notes/v0.26.1.md | 4 | 0.00000 | 0.00000 |  |  |
| 6 | release-notes/v0.36.1.md | 5 | 0.00000 | 0.00000 |  |  |
| 7 | release-notes/v0.39.4.md | 6 | 0.00000 | 0.00000 |  |  |
| 8 | release-notes/v0.49.1.md | 7 | 0.00000 | 0.00000 |  |  |
| 9 | reports/verbatim-audit.md | 8 | 0.00000 | 0.00000 |  |  |
| 10 | src/fuzzy_match.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `release-notes/v0.30.2.md`, `release-notes/v0.44.0.md`, `ARCHITECTURE.md`, `release-notes/v0.22.0.md`, `release-notes/v0.26.1.md`, `release-notes/v0.36.1.md`, `release-notes/v0.39.4.md`, `release-notes/v0.49.1.md`, `reports/verbatim-audit.md`, `src/fuzzy_match.rs`

### followup-census:920008  pattern `drain_on_shutdown|Result`  answer `crates/aft/src/subc/mod.rs`

prose_only.ranked_paths (first 10): `lsp/manager.rs`, `src/runtime_drain.rs`, `src/watcher_filter.rs`, `lsp/client.rs`, `src/bridge.ts`, `src/context.rs`, `subc/mod.rs`, `bash_background/registry.rs`, `src/subc-transport.ts`, `src/search_index.rs`

slot ranked (first 10): `subc/mod.rs`, `lsp/manager.rs`, `callgraph_store/mod.rs`, `src/runtime_drain.rs`, `src/watcher_filter.rs`, `lsp/client.rs`, `src/bridge.ts`, `src/context.rs`, `bash_background/registry.rs`, `src/subc-transport.ts`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | lsp/manager.rs | 0 | 0.00000 | 0.00000 |  |  |
| 2 | src/runtime_drain.rs | 1 | 0.00000 | 0.00000 |  |  |
| 3 | src/watcher_filter.rs | 2 | 0.00000 | 0.00000 |  |  |
| 4 | src/response_finalize.rs | 17 | 0.00030 | 0.00000 | yes |  |
| 5 | lsp/client.rs | 3 | 0.00000 | 0.00000 |  |  |
| 6 | src/bridge.ts | 4 | 0.00000 | 0.00000 |  |  |
| 7 | subc/mod.rs **(answer)** | 6 | 0.00030 | 0.00000 |  |  |
| 8 | src/context.rs | 5 | 0.00000 | 0.00000 |  |  |
| 9 | bash_background/registry.rs | 7 | 0.00000 | 0.00000 |  |  |
| 10 | src/main.rs | 10 | 0.00060 | 0.00000 |  |  |

query-primary ranked (first 10): `lsp/manager.rs`, `src/runtime_drain.rs`, `src/watcher_filter.rs`, `src/response_finalize.rs`, `lsp/client.rs`, `src/bridge.ts`, `subc/mod.rs`, `src/context.rs`, `bash_background/registry.rs`, `src/main.rs`

### followup-census:920009  pattern `kill_running_tasks_for_root`  answer `crates/aft/src/subc/mod.rs`

prose_only.ranked_paths (first 10): `investigations/bash-task-erasure-census-2026-08-30.md`, `scripts/build-schema.ts`, `release-notes/v0.47.0.md`, `release-notes/v0.50.0.md`, `src/config.rs`, `release-notes/v0.54.0.md`, `release-notes/v0.37.0.md`, `STRUCTURE.md`, `src/config.ts`, `subc/manifest.rs`

slot ranked (first 10): `bash_background/registry.rs`, `src/context.rs`, `bash_background/persistence.rs`, `tools/permissions.ts`, `subc/mod.rs`, `src/artifact_owner.rs`, `executor/mod.rs`, `bash_background/mod.rs`, `commands/configure.rs`, `src/root_cache.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | investigations/bash-task-erasure-census-2026-08-30.md | 0 | 0.00060 | 0.00000 |  |  |
| 2 | bash_background/registry.rs | 50 | 0.00120 | 0.00000 | yes |  |
| 3 | scripts/build-schema.ts | 1 | 0.00000 | 0.00000 |  |  |
| 4 | release-notes/v0.47.0.md | 2 | 0.00000 | 0.00000 |  |  |
| 5 | release-notes/v0.50.0.md | 3 | 0.00000 | 0.00000 |  |  |
| 6 | src/config.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | release-notes/v0.54.0.md | 5 | 0.00000 | 0.00000 |  |  |
| 8 | release-notes/v0.37.0.md | 6 | 0.00000 | 0.00000 |  |  |
| 9 | STRUCTURE.md | 7 | 0.00000 | 0.00000 |  |  |
| 10 | src/config.ts | 8 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `investigations/bash-task-erasure-census-2026-08-30.md`, `bash_background/registry.rs`, `scripts/build-schema.ts`, `release-notes/v0.47.0.md`, `release-notes/v0.50.0.md`, `src/config.rs`, `release-notes/v0.54.0.md`, `release-notes/v0.37.0.md`, `STRUCTURE.md`, `src/config.ts`

### followup-census:920010  pattern `pending_pattern_matches_for_session`  answer `crates/aft/src/commands/bash_drain_completions.rs`

prose_only.ranked_paths (first 10): `commands/bash_drain_completions.rs`, `src/bg-notifications.ts`, `src/runtime_drain.rs`, `src/bg-notifications.ts`, `src/context.rs`, `bash_background/registry.rs`, `executor/mod.rs`, `src/response_finalize.rs`, `lsp/manager.rs`, `inspect/manager.rs`

slot ranked (first 10): `bash_background/registry.rs`, `commands/bash_drain_completions.rs`, `src/bg-notifications.ts`, `src/runtime_drain.rs`, `src/bg-notifications.ts`, `src/context.rs`, `executor/mod.rs`, `src/response_finalize.rs`, `lsp/manager.rs`, `inspect/manager.rs`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | commands/bash_drain_completions.rs **(answer)** | 0 | 0.00060 | 0.00000 |  |  |
| 2 | bash_background/registry.rs | 5 | 0.00120 | 0.00000 |  |  |
| 3 | src/bg-notifications.ts | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/runtime_drain.rs | 2 | 0.00000 | 0.00000 |  |  |
| 5 | src/bg-notifications.ts | 3 | 0.00000 | 0.00000 |  |  |
| 6 | src/context.rs | 4 | 0.00000 | 0.00000 |  |  |
| 7 | executor/mod.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | src/response_finalize.rs | 7 | 0.00000 | 0.00000 |  |  |
| 9 | lsp/manager.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | inspect/manager.rs | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `commands/bash_drain_completions.rs`, `bash_background/registry.rs`, `src/bg-notifications.ts`, `src/runtime_drain.rs`, `src/bg-notifications.ts`, `src/context.rs`, `executor/mod.rs`, `src/response_finalize.rs`, `lsp/manager.rs`, `inspect/manager.rs`

### followup-census:920011  pattern `active_suspensions_for_root_at|Error`  answer `crates/aft/src/subc/health.rs`

prose_only.ranked_paths (first 10): `src/build_breaker.rs`, `inspect/manager.rs`, `subc/health.rs`, `src/context.rs`, `lsp/child_registry.rs`, `src/runtime_drain.rs`, `subc/mod.rs`, `commands/doctor.ts`, `callgraph_store/mod.rs`, `lib/build-breaker.ts`

slot ranked (first 10): `src/build_breaker.rs`, `inspect/manager.rs`, `subc/health.rs`, `src/context.rs`, `lsp/child_registry.rs`, `src/runtime_drain.rs`, `subc/mod.rs`, `commands/doctor.ts`, `callgraph_store/mod.rs`, `lib/build-breaker.ts`

query-primary split_trace placements (first 10):

| # | path | base position | anchor | edge | admitted | supports |
|---|---|---|---|---|---|---|
| 1 | src/build_breaker.rs | 0 | 0.00120 | 0.00000 |  |  |
| 2 | subc/health.rs **(answer)** | 2 | 0.00060 | 0.00000 |  |  |
| 3 | inspect/manager.rs | 1 | 0.00000 | 0.00000 |  |  |
| 4 | src/context.rs | 3 | 0.00000 | 0.00000 |  |  |
| 5 | lsp/child_registry.rs | 4 | 0.00000 | 0.00000 |  |  |
| 6 | src/runtime_drain.rs | 5 | 0.00000 | 0.00000 |  |  |
| 7 | subc/mod.rs | 6 | 0.00000 | 0.00000 |  |  |
| 8 | commands/doctor.ts | 7 | 0.00000 | 0.00000 |  |  |
| 9 | callgraph_store/mod.rs | 8 | 0.00000 | 0.00000 |  |  |
| 10 | lib/build-breaker.ts | 9 | 0.00000 | 0.00000 |  |  |

query-primary ranked (first 10): `src/build_breaker.rs`, `subc/health.rs`, `inspect/manager.rs`, `src/context.rs`, `lsp/child_registry.rs`, `src/runtime_drain.rs`, `subc/mod.rs`, `commands/doctor.ts`, `callgraph_store/mod.rs`, `lib/build-breaker.ts`

