# Split query: mention-host gate run

One gate run (`cost-gate.sh --search-quality --mode evaluate --descriptor slice-descriptors/train-246.json --base-ref <merge-base>`), call graph off, AFT_SEARCH_SPLIT_TRACE set, release build of 7bf42d184. Slot and query-primary columns are the earlier gate runs of 809d13ce0 and 40685f332.

| row | kind | answer | query-only | slot | query-primary | mention-host | mention-host paired delta |
|---|---|---|---|---|---|---|---|
| 910001 | R1 | commands/ast_search.rs | - | - | - | - | +0.000 |
| 910002 | R1 | bash_background/registry.rs | 10 | 7 | 10 | 10 | +0.000 |
| 910003 | R2 | subc/mod.rs | 4 | 4 | 4 | 4 | +0.000 |
| 910004 | R2 | snapshot/mod.rs | - | - | - | - | +0.000 |
| 910005 | R3 | compress/mod.rs | 6 | 1 | 6 | 2 | +0.333 |
| 910006 | R4 | lsp/manager.rs | 9 | 1 | 5 | 3 | +0.222 |
| 910007 | R4 | subc/mod.rs | 4 | 5 | 2 | 2 | +0.250 |
| 910008 | R5 | commands/ast_search.rs | - | - | - | - | +0.000 |
| 910009 | R5 | subc/mod.rs | 4 | 4 | 4 | 4 | +0.000 |
| 910010 | R6 | commands/ast_search.rs | - | 1 | - | 2 | +0.500 |
| 910011 | R7 | inspect/manager.rs | 1 | 1 | 1 | 1 | +0.000 |
| 910012 | R7 | commands/status.rs | 3 | 4 | 2 | 3 | +0.000 |

| build | mechanism MRR@10 | hit@1 | hit@5 | real-query MRR@10 | exact recall | concept recall | paired harm rows |
|---|---|---|---|---|---|---|---|
| reference | 0.2042 | 0.167 | 0.333 | 0.2977 | 1.000 | 0.6394 | - |
| slot | 0.4244 | 0.333 | 0.667 | 0.3365 | 1.000 | 0.6394 | 910007, 910012 |
| query-primary | 0.2472 | 0.083 | 0.500 | 0.3053 | 1.000 | 0.6394 | none |
| mention-host | 0.3139 | 0.083 | 0.667 | 0.3170 | 1.000 | 0.6394 | none |

Gate result (mention-host): exit 1, failures split_rank1_required 910001 and 910002 only. The 56 non-split rows are identical to the reference.

## Placements (split_trace, first 6)

### 910001 `ast_grep_search`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | src/search_index.rs | 0 | 0.00000 |  |  |
| 2 | src/main.rs | 1 | 0.00000 |  |  |
| 3 | src/runtime_drain.rs | 2 | 0.00000 |  |  |
| 4 | callgraph_store/mod.rs | 3 | 0.00000 |  |  |
| 5 | src/logging.rs | 4 | 0.00000 |  |  |
| 6 | src/callgraph.rs | 5 | 0.00000 |  |  |

### 910002 `subagent_type`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | corpora/codegraph.json | 0 | 0.00000 |  |  |
| 2 | results/aft-grep-codegraph-2026-05-26T09-41-35-269Z.json | 1 | 0.00000 |  |  |
| 3 | results/ripgrep-codegraph-2026-05-26T09-41-56-635Z.json | 2 | 0.00000 |  |  |
| 4 | bash_background/watches.rs | 3 | 0.00000 |  |  |
| 5 | bash_background/process.rs | 4 | 0.00000 |  |  |
| 6 | commands/bash_orchestrate.rs | 5 | 0.00000 |  |  |

### 910003 `Error|Result`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 |  |  |
| 2 | src/runtime_drain.rs | 1 | 0.00000 |  |  |
| 3 | src/subc-transport.ts | 2 | 0.00000 |  |  |
| 4 | subc/mod.rs **(answer)** | 3 | 0.00000 |  |  |
| 5 | subc/health.rs | 4 | 0.00000 |  |  |
| 6 | lsp/client.rs | 5 | 0.00000 |  |  |

### 910004 `Error|Result`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | drafts/2026-08-09-hashline-edit-surface-refire4.md | 0 | 0.00000 |  |  |
| 2 | transaction/mod.rs | 1 | 0.00000 |  |  |
| 3 | src/backup.rs | 2 | 0.00000 |  |  |
| 4 | src/checkpoint.rs | 3 | 0.00000 |  |  |
| 5 | commands/restore_checkpoint.rs | 4 | 0.00000 |  |  |
| 6 | src/context.rs | 5 | 0.00000 |  |  |

### 910005 `^pub struct (FormatContext|HostEscalationAttempt|PinOwner|ServerKey|LspManager|L`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | tools/bash.ts | 0 | 0.00000 |  |  |
| 2 | compress/mod.rs **(answer)** | 5 | 0.00120 |  |  |
| 3 | compress/vitest.rs | 1 | 0.00000 |  |  |
| 4 | compress/cargo.rs | 7 | 0.00120 |  |  |
| 5 | examples/tsc_compression_perf_probe.rs | 2 | 0.00000 |  |  |
| 6 | bash_background/registry.rs | 3 | 0.00000 |  |  |

### 910006 `shutdown_all`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 |  |  |
| 2 | subc/mod.rs | 3 | 0.00060 |  |  |
| 3 | lsp/manager.rs **(answer)** | 8 | 0.00120 |  | subc/mod.rs |
| 4 | src/runtime_drain.rs | 1 | 0.00000 |  |  |
| 5 | src/subc-transport.ts | 2 | 0.00000 |  |  |
| 6 | subc/health.rs | 4 | 0.00000 |  |  |

### 910007 `shutdown_all`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | src/error-contract.ts | 0 | 0.00000 |  |  |
| 2 | subc/mod.rs **(answer)** | 3 | 0.00060 |  |  |
| 3 | lsp/manager.rs | 8 | 0.00120 |  | subc/mod.rs |
| 4 | src/runtime_drain.rs | 1 | 0.00000 |  |  |
| 5 | src/subc-transport.ts | 2 | 0.00000 |  |  |
| 6 | subc/health.rs | 4 | 0.00000 |  |  |

### 910010 `handle_ast_search`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | release-notes/v0.37.0.md | 0 | 0.00000 |  |  |
| 2 | commands/ast_search.rs **(answer)** | None | 0.00000 | yes |  |
| 3 | release-notes/v0.43.0.md | 1 | 0.00000 |  |  |
| 4 | src/workflow-hints.ts | 2 | 0.00000 |  |  |
| 5 | release-notes/v0.36.0.md | 3 | 0.00000 |  |  |
| 6 | src/workflow-hints.ts | 4 | 0.00000 |  |  |

### 910011 `Tier2PhaseTimings|Error`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | inspect/manager.rs **(answer)** | 0 | 0.00120 |  |  |
| 2 | src/logging.rs | 2 | 0.00060 |  |  |
| 3 | src/context.rs | 1 | 0.00000 |  |  |
| 4 | src/runtime_drain.rs | 3 | 0.00000 |  |  |
| 5 | inspect/cache.rs | 4 | 0.00000 |  |  |
| 6 | inspect/phase_log.rs | 5 | 0.00000 |  |  |

### 910012 `tracked_files|Error`

| # | path | base | anchor | admitted | supports |
|---|---|---|---|---|---|
| 1 | src/protocol.rs | 0 | 0.00038 |  |  |
| 2 | src/backup.rs | 6 | 0.00075 |  | src/protocol.rs |
| 3 | commands/status.rs **(answer)** | 2 | 0.00038 |  |  |
| 4 | commands/restore_checkpoint.rs | 1 | 0.00000 |  |  |
| 5 | src/checkpoint.rs | 3 | 0.00038 |  |  |
| 6 | commands/checkpoint.rs | 5 | 0.00038 |  |  |

