# AFT Search Benchmarks

This guide explains how to run retrieval quality benchmarks comparing **ripgrep**, **AFT legacy search**, and **FTS5** using the Semble corpus.

## Quick Start

```bash
# 1. Clone pilot repos (5 repos, ~50MB total)
bun run benchmarks/semble/corpus.ts sync --pilot

# 2. Run the multi-mode pilot (compares all three search backends)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft --k 10

# 3. View results
cat pilot-report.json
```

## Prerequisites

- **Bun** runtime (for TypeScript execution)
- **ripgrep** (`rg`) installed and in PATH
- **AFT binary** built with FTS5 support:
  ```bash
  cargo build --release --features semantic-fts5
  ```

## Benchmark Scripts

### 1. Multi-Mode Pilot (Recommended)

**File:** `benchmarks/semble/pilot.ts`

Compares three search backends on the same queries:
- **lexical** — ripgrep (raw text search)
- **aft-grep** — AFT trigram-indexed search
- **fts5** — AFT FTS5 full-text search

```bash
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft --k 10
```

**Output:** `pilot-report.json` with aggregate and per-category metrics.

### 2. Individual Baselines

#### Ripgrep Baseline

**File:** `benchmarks/semble/baseline-rg.ts`

```bash
bun run benchmarks/semble/baseline-rg.ts --pilot --k 10
```

**Output:** `baseline-rg-report.json`

#### AFT Legacy Search Baseline

**File:** `benchmarks/semble/baseline-aft.ts`

```bash
# Grep mode (trigram-indexed)
bun run benchmarks/semble/baseline-aft.ts --pilot --k 10 --binary ./target/release/aft

# Semantic mode (requires embedding model)
bun run benchmarks/semble/baseline-aft.ts --pilot --k 10 --binary ./target/release/aft --mode semantic

# Hybrid mode (lexical + semantic fusion)
bun run benchmarks/semble/baseline-aft.ts --pilot --k 10 --binary ./target/release/aft --mode hybrid
```

**Output:** `baseline-aft-report.json`

#### FTS5 Baseline

**File:** `benchmarks/semble/baseline-fts5.ts`

```bash
bun run benchmarks/semble/baseline-fts5.ts --pilot --k 10 --binary ./target/release/aft
```

**Output:** `baseline-fts5-report.json`

### 3. Specialized Benchmarks

#### Token Efficiency

Measures recall at different token budgets (100, 500, 1K, 2K, 5K, 10K, 50K tokens).

```bash
bun run benchmarks/semble/token-efficiency.ts --binary ./target/release/aft
```

#### Ablation Study

Compares lexical, semantic, and hybrid modes with detailed per-query analysis.

```bash
bun run benchmarks/semble/ablation.ts --binary ./target/release/aft
```

#### Cold-Start Latency

Measures index build time and first-query latency.

```bash
bun run benchmarks/semble/speed.ts --binary ./target/release/aft
```

## Metrics

### Retrieval Quality

| Metric | Formula | Description |
|--------|---------|-------------|
| **Recall@K** | `hits / total_relevant` | Fraction of relevant files found in top-K results |
| **MRR** | `1 / rank_of_first_hit` | Mean Reciprocal Rank of first relevant result |
| **nDCG@K** | `DCG / IDCG` | Normalized Discounted Cumulative Gain |

### Performance

| Metric | Description |
|--------|-------------|
| **latency_ms** | Query execution time (median over N iterations) |
| **cold_start_ms** | Time to build search index from scratch |

### Token Efficiency

| Metric | Description |
|--------|-------------|
| **recall@token_budget** | Recall achieved within a token budget |

## Annotation Format

Each query has ground truth annotations:

```json
{
  "query": "how axum Handler trait dispatches requests",
  "relevant": [
    { "path": "axum/src/handler/mod.rs" }
  ],
  "secondary": [
    { "path": "axum/src/handler/into_service.rs" }
  ],
  "category": "symbol",
  "repo_name": "axum"
}
```

### Categories

- **symbol** — Exact name lookup (e.g., "Router", "BaseModel")
- **semantic** — Implementation search (e.g., "how extractors work")
- **architecture** — Design/structure (e.g., "middleware pattern")

## Pilot Corpus

5 repos with 10 queries each (50 total):

| Repo | Language | Queries | Description |
|------|----------|---------|-------------|
| axum | Rust | 10 | Web framework — trait/API-heavy |
| express | JavaScript | 10 | Web framework — prototype-based |
| pydantic | Python | 10 | Data validation — typing-heavy |
| serde | Rust | 10 | Serialization — derive macros |
| gin | Go | 10 | Web framework — radix tree routing |

## Interpreting Results

### Example Output

```
=== Pilot Report ===
  lexical: recall=45.2% mrr=0.523 ndcg=0.481 latency=12.3ms
  aft-grep: recall=52.8% mrr=0.612 ndcg=0.558 latency=8.7ms
  fts5: recall=61.4% mrr=0.701 ndcg=0.642 latency=15.2ms
```

### What to Look For

1. **Recall@10 > 60%** — Good retrieval quality
2. **MRR > 0.6** — Relevant results appear early
3. **nDCG > 0.5** — Rankings are well-ordered
4. **FTS5 > lexical** — FTS5 outperforms raw ripgrep
5. **Latency < 50ms** — Acceptable for interactive use

### Per-Category Analysis

Check if search backends perform differently on:
- **symbol** queries (exact names) — lexical should be strong
- **semantic** queries (concepts) — FTS5/semantic should be stronger
- **architecture** queries (patterns) — hybrid approaches may win

## Regression Detection

Compare against a baseline:

```bash
# Save baseline
cp pilot-report.json baseline.json

# Run again and compare
bun run benchmarks/semble/ci.ts --baseline baseline.json --current pilot-report.json
```

Exits non-zero if recall drops >5%.

## Troubleshooting

### "AFT binary not found"

Build with FTS5 support:
```bash
cargo build --release --features semantic-fts5
```

### "Repos not cloned"

Sync the corpus first:
```bash
bun run benchmarks/semble/corpus.ts sync --pilot
```

### FTS5 commands fail

Ensure FTS5 is enabled in configure command. The benchmark scripts pass `fts5: { enabled: true }` automatically.

### Windows path issues

The scripts normalize paths to forward slashes. If you see path mismatches, check that `normalizePath()` handles your paths correctly.

## Extending the Benchmark

### Add a New Repo

1. Add entry to `benchmarks/semble/repos-pilot.json`
2. Create `benchmarks/semble/annotations/<name>.json` with 10+ queries
3. Run `bun run benchmarks/semble/import.ts --pilot` to regenerate fixtures
4. Run `bun run benchmarks/semble/corpus.ts sync --pilot` to clone

### Add a New Search Backend

1. Create `benchmarks/semble/baseline-<name>.ts`
2. Implement search function returning `{ results, latency_ms }`
3. Add to `pilot.ts` for comparison
4. Follow the pattern in `baseline-fts5.ts` for NDJSON integration

## Related Benchmarks

- **`benchmarks/aft-search/`** — AFT search fixture benchmark with Vera-compatible corpus
- **`benchmarks/codegraph-replication/`** — CodeGraph-style retrieval cases
- **`benchmarks/codegraph-vs-aft-retrieval/`** — AFT vs CodeGraph head-to-head
- **`benchmarks/settle-time/`** — Index build time measurements
