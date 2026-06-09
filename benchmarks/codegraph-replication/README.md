# CodeGraph replication benchmark

Deterministic retrieval-quality benchmark for comparing AFT with CodeGraph-style retrieval and lexical baselines. It replicates CodeGraph's structured eval methodology (`searchNodes` and `findRelevantContext` cases scored by recall/MRR/found/missed symbols) while running through AFT's real tool dispatch path.

This is intentionally separate from `benchmarks/aft-search/`: that benchmark is Vera-compatible and measures AFT search against Vera-style file/line ground truth. This benchmark is for apples-to-apples CodeGraph comparisons and keeps CodeGraph's symbol/context case shape.

## What it measures

For each corpus case and driver, the harness records:

- recall over expected symbols (CodeGraph-compatible pass threshold: `recall >= 0.5`)
- MRR from the first relevant ranked result
- Precision@1, @5, @10
- found/missed symbols and files
- real wall-clock latency around the actual driver dispatch, with per-query median/p95 samples when `--runs > 1`

It also emits CodeGraph-like JSON plus a markdown summary for publishing tables.

## Relationship to CodeGraph's eval

CodeGraph's original structured eval lives in `codegraph/__tests__/evaluation/` and has 12 Elasticsearch-oriented cases. This harness includes:

- `corpora/codegraph-original.json`: exact upstream cases, for codebases containing those symbols.
- `corpora/codegraph.json`: an AFT-side translation preserving the same 6 `searchNodes` + 6 `findRelevantContext` shapes, but substituting `opencode-aft` symbols.
- `corpora/aft.json`: small supplemental AFT tool-surface cases for outline/zoom/navigate.

What is not replicated: CodeGraph's agent A/B tmux harness, token/cost behavior, graph edge density, and CodeGraph-specific kind filtering. AFT's `aft_search` currently returns ranked symbol/file results rather than a subgraph, so `edgeCount`/`edgeDensity` are omitted instead of faked.

The plan file at `.alfonso/plans/codegraph-benchmark-replication.md` documents the mapping and notes patterns borrowed from `code-review-graph` (pinned repo metadata, task categories, deterministic report metadata) without depending on that project.

## Run it

From the repo root:

```bash
bun run bench:codegraph-replication --driver aft --corpus codegraph
bun run bench:codegraph-replication --driver aft-grep --corpus codegraph
bun run bench:codegraph-replication --driver ripgrep --corpus codegraph
bun run bench:codegraph-replication --driver list-files --corpus codegraph
```

Options:

```bash
--driver aft|aft-grep|ripgrep|list-files
--corpus codegraph|codegraph-original|aft|/path/to/custom.json
--codebase /path/to/repo                 # default: this checkout
--binary /path/to/target/release/aft     # default: target/release/aft
--topK 10
--runs 3
--out-dir benchmarks/codegraph-replication/results
--ready-timeout-ms 600000
```

`aft` and `aft-grep` use `@cortexkit/aft-bridge`'s `BinaryBridge` against the local `target/release/aft`, configure real search indexes, then dispatch the real bridge commands (`semantic_search`, `grep`, and for supplemental cases `outline`/`zoom`/navigate commands).

## Add cases

Add JSON cases to an existing corpus or pass a custom corpus file:

```json
{
  "id": "search-class-exact",
  "query": "BinaryBridge",
  "api": "searchNodes",
  "tool": "aft_search",
  "expectedSymbols": ["BinaryBridge"],
  "expectedFiles": ["packages/aft-bridge/src/bridge.ts"],
  "kinds": ["class"],
  "category": "symbol_lookup",
  "options": { "searchLimit": 10 }
}
```

Useful `tool` values: `aft_search`, `aft_grep`, `aft_outline`, `aft_zoom`, `aft_navigate`. `aft-grep`, `ripgrep`, and `list-files` drivers ignore AFT-specific tool hints and run their own baseline behavior against the case query.

## Current results

Run on this `opencode-aft` worktree against `corpora/codegraph.json` with `--runs 1`, `topK=10`:

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | median ms | p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| AFT `aft_search` | 12 | 12 | 0.833 | 0.958 | 0.917 | 0.650 | 0.500 | 3.3 | 52.7 |
| AFT `grep` | 12 | 6 | 0.521 | 0.583 | 0.583 | 0.400 | 0.367 | 13.2 | 21.0 |
| ripgrep `rg -F` | 12 | 6 | 0.521 | 0.583 | 0.583 | 0.400 | 0.367 | 33.2 | 36.4 |
| list-files sanity | 12 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.1 | 0.3 |

Saved reports from this run:

- `results/aft-codegraph-2026-05-26T09-41-23-339Z.json`
- `results/aft-grep-codegraph-2026-05-26T09-41-35-269Z.json`
- `results/ripgrep-codegraph-2026-05-26T09-41-56-635Z.json`
- `results/list-files-codegraph-2026-05-26T09-42-18-550Z.json`

## Caveats

- The translated `codegraph` corpus preserves methodology, not literal Elasticsearch targets. Use `codegraph-original` when evaluating a compatible external checkout.
- `aft_search` has no kind filter today, so `kinds` are reported as metadata only.
- The lexical baselines use exact fixed-string query matching; they are intentionally weak on natural-language `findRelevantContext` cases.
- Single-run latency is noisy. Use `--runs 3` or higher for publishable latency numbers.
- Token-efficiency and agent behavior are intentionally out of scope for v1; those belong to a separate benchmark axis.

## Docker

The local Bun command above is unchanged. To run in a reproducible container that builds AFT and installs Bun/Ripgrep inside Docker:

```bash
bun run bench:codegraph-replication:docker
# or
make run-codegraph-replication
```

Useful overrides:

```bash
CODEGRAPH_REPLICATION_DRIVER=aft-grep CODEGRAPH_REPLICATION_CORPUS=aft bun run bench:codegraph-replication:docker
```
