# AFT benchmarks

This directory contains reproducible benchmark harnesses. Docker is the preferred entry point so a host machine does **not** need local AFT, Bun, Rust, CodeGraph, or OpenCode installs.

| Benchmark | Entry point | Measures | Notes |
| --- | --- | --- | --- |
| `aft-search/` | `bun run bench:aft-search` or `make run-aft-search` | AFT `aft_search` retrieval on the in-tree fixture suite; set `AFT_SEARCH_MODE=external` for the Vera-compatible corpus. | Builds AFT from this checkout inside Docker. Existing local Python commands still work. |
| `codegraph-replication/` | `bun run bench:codegraph-replication` (local) or `bun run bench:codegraph-replication:docker` | Structured CodeGraph-style retrieval cases run through AFT and lexical baselines. | Docker command builds AFT and Bun in-container. |
| `codegraph-vs-aft-retrieval/` | `bun run bench:codegraph-vs-aft-retrieval` or `make run-codegraph-vs-aft-retrieval` | **Path A:** no-LLM retrieval quality: AFT vs CodeGraph on identical cases with Recall/MRR/P@k. | Two images: one builds AFT, one installs `@colbymchenry/codegraph@0.9.6`. |
| `codegraph-vs-aft-agent/` | `bun run bench:codegraph-vs-aft-agent` or `make run-codegraph-vs-aft-agent` | **Path B:** OpenCode agent A/B: AFT plugin vs CodeGraph MCP on deterministic tasks. | Uses `opencode-go/deepseek-v4-flash-free` via the zen endpoint. Mount auth or set `OPENCODE_API_KEY`; `AGENT_DRY_RUN=1` only validates harness shape. |

Results are written under each benchmark's `results/` directory. These directories are ignored by default; small sample outputs for the new AFT-vs-CodeGraph suites are committed to document the JSON/Markdown shape.

## Common Docker examples

```bash
# Existing AFT search fixture benchmark
bun run bench:aft-search

# Existing CodeGraph replication benchmark in Docker
bun run bench:codegraph-replication:docker

# Path A: run both retrieval drivers on the default opencode-aft corpus
bun run bench:codegraph-vs-aft-retrieval

# Optional Path A targets: clone pinned external corpora inside the containers
RETRIEVAL_CORPUS=ripgrep PREPARE_TARGET=1 bun run bench:codegraph-vs-aft-retrieval
RETRIEVAL_CORPUS=elasticsearch PREPARE_TARGET=1 bun run bench:codegraph-vs-aft-retrieval

# Path B real agent run (requires auth)
OPENCODE_API_KEY=... AGENT_TASK_LIMIT=3 bun run bench:codegraph-vs-aft-agent

# Path B harness-only smoke/sample, no LLM calls
AGENT_DRY_RUN=1 AGENT_TASK_LIMIT=2 bun run bench:codegraph-vs-aft-agent
```

## Caveats

- Retrieval quality and agent success are separate axes. Path A has no LLM and should not be used to infer agent behavior. Path B uses an LLM and therefore has run-to-run variance.
- CodeGraph is compared through its published CLI/MCP surface, not through private internals.
- AFT Docker images build the current checkout. CodeGraph Docker images install the pinned npm package listed in each Dockerfile.
- The default Path A corpus targets this checkout because it is a real, available codebase in CI/worktrees. Optional pinned external corpora live in version-controlled `corpora/` manifests.
