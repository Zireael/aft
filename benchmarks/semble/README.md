# AFT Semble Benchmark Suite

## Overview

This directory contains a local adaptation of the [Semble](https://github.com/MinishLab/semble) benchmark for evaluating AFT's semantic search capabilities. The benchmark measures retrieval quality (recall, MRR, NDCG) and latency across different search modes.

## Directory Structure

```
benchmarks/semble/
├── schema.json              # JSON Schema for benchmark fixtures
├── repos.json               # Full 63-repo Semble lockfile (reference)
├── repos-pilot.json         # 5-repo pilot subset
├── annotations/             # Per-repo query annotations
│   ├── axum.json
│   ├── express.json
│   ├── gin.json
│   ├── pydantic.json
│   └── serde.json
├── fixtures.json            # Generated pilot fixture (schema v1)
├── import.ts                # Semble annotation importer
├── corpus.ts                # Repo clone/cache tooling
├── baseline-rg.ts           # Ripgrep lexical-only baseline
├── speed.ts                 # Cold-start index + query latency
├── pilot.ts                 # Multi-mode pilot runner
├── token-efficiency.ts      # Recall@token_budget curves
├── ablation.ts              # Mode comparison (lexical/semantic/hybrid)
├── ci.ts                    # CI regression detection
├── PILOT_SELECTION.md       # Rationale for pilot repo selection
└── README.md                # This file
```

## Quick Start

### 1. Sync the pilot corpus

```bash
bun run benchmarks/semble/corpus.ts sync --pilot
```

Clones 5 repos (axum, express, pydantic, serde, gin) into `.bench-cache/` and checks out pinned commits.

### 2. Run the baseline benchmarks

```bash
# Ripgrep lexical baseline
bun run benchmarks/semble/baseline-rg.ts --pilot --k 10

# FTS5 baseline
bun run benchmarks/semble/baseline-fts5.ts --pilot --k 10
```

Runs search against all annotations and produces recall@10, MRR, and latency metrics.

### 3. Run the full pilot

```bash
bun run benchmarks/semble/pilot.ts --cache-dir .bench-cache --k 10
```

Compares lexical mode across all 50 queries. When AFT binary is available, also runs semantic, hybrid, and reranked modes.

### 4. Check for regressions

```bash
bun run benchmarks/semble/ci.ts --baseline baseline.json --current pilot-report.json
```

Exits non-zero if recall drops more than 5% from baseline.

## Pilot Corpus

### Selected Repos (5)

| Repo | Language | Queries | Description |
|------|----------|---------|-------------|
| axum | Rust | 10 | Web framework — trait/API-heavy |
| express | JavaScript | 10 | Web framework — prototype-based |
| pydantic | Python | 10 | Data validation — typing-heavy |
| serde | Rust | 10 | Serialization — derive macros |
| gin | Go | 10 | Web framework — radix tree routing |

**Total:** 50 queries across 4 languages

### Selection Criteria

1. **Language diversity** — Rust, JavaScript, Python, Go
2. **Symbol density** — well-defined public APIs (traits, structs, classes)
3. **Codebase size** — small enough to clone locally (<50MB)
4. **Annotation quality** — 10+ queries per repo with category mix

## Annotation Schema

Each annotation has:

```json
{
  "query": "natural language or symbol query",
  "relevant": ["path/to/file.rs"],
  "secondary": ["optional/secondary.rs"],
  "category": "symbol|semantic|architecture",
  "repo_name": "axum"
}
```

### Categories

- **symbol** — exact name lookup (e.g., "Router", "BaseModel")
- **semantic** — implementation search (e.g., "how extractors work")
- **architecture** — design/structure (e.g., "middleware pattern")

## Metrics

### Retrieval Quality

- **recall@k** — fraction of relevant files found in top-k results
- **MRR** — reciprocal rank of first relevant result
- **NDCG@k** — normalized discounted cumulative gain

### Performance

- **cold_start_ms** — time to build semantic index from scratch
- **latency_ms** — query execution time (median over N iterations)

### Token Efficiency

- **recall@token_budget** — recall achieved within a token budget
- Tested at: 100, 500, 1K, 2K, 5K, 10K, 50K tokens

## Reproducibility

### Pinned Commits

Every repo is pinned to a specific commit SHA in `repos-pilot.json`. The corpus tooling checks out these exact commits:

```bash
bun run benchmarks/semble/corpus.ts check --pilot
```

### Path Matching

Annotation paths are relative to `benchmark_root` in the repo. The importer supports both:
- Full-file paths: `"path/to/file.rs"`
- Line-range targets: `{"path": "file.js", "start_line": 125, "end_line": 230}`

### Extending the Pilot

To add a new repo:

1. Add entry to `repos-pilot.json`
2. Create `annotations/<name>.json` with 10+ queries
3. Run `bun run benchmarks/semble/import.ts --pilot` to regenerate fixtures
4. Run `bun run benchmarks/semble/corpus.ts sync --pilot` to clone

To use the full 63-repo set:

```bash
bun run benchmarks/semble/import.ts --input benchmarks/semble --output full-fixtures.json
bun run benchmarks/semble/corpus.ts sync
```

## Benchmark Sources

- **Upstream**: [MinishLab/semble](https://github.com/MinishLab/semble/tree/main/benchmarks)
- **repos.json**: 63 pinned repos across 18 languages
- **annotations/**: Human-authored query relevance judgments
- **Schema**: AFT-adapted (schema_version: 1) with provenance metadata

## Known Limitations

1. **Lexical baseline only** — current scripts run ripgrep; AFT semantic/hybrid modes require the `aft` binary
2. **Small pilot** — 50 queries across 5 repos; not statistically significant for production decisions
3. **No reranker** — reranked mode requires AFT binary with reranker configured
4. **Category imbalance** — more architecture queries than semantic in some repos
