# AFT FTS5 Search Benchmarks

Measure **Recall@1/5/10**, **MRR@10**, and **nDCG@10** for ripgrep, AFT legacy search, and FTS5 on the Semble and Vera corpora.

## Prerequisites

| Tool | Purpose | Install |
|------|---------|---------|
| **Bun** | TypeScript benchmark runner | `curl -fsSL https://bun.sh/install \| bash` |
| **ripgrep** (`rg`) | Lexical baseline | `apt install ripgrep` / `brew install ripgrep` |
| **AFT binary** | All AFT search modes | `cargo build --release --features semantic-fts5` |
| **Python 3** | Vera-compatible benchmarks | System Python |
| **uv** | Python dependency management (Vera only) | `pip install uv` |

## Two Benchmark Suites

### 1. Semble Suite (50 queries, 5 repos)

Uses the [Semble](https://github.com/MinishLab/semble) corpus with human-authored relevance judgments across axum, express, pydantic, serde, and gin.

**Quick start:**
```bash
# Clone pilot repos (~50MB)
bun run benchmarks/semble/corpus.ts sync --pilot

# Run all three backends and compare
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft --k 10
```

**Individual baselines:**
```bash
# Ripgrep only
bun run benchmarks/semble/baseline-rg.ts --pilot --k 10

# AFT grep (trigram-indexed)
bun run benchmarks/semble/baseline-aft.ts --pilot --k 10 --binary ./target/release/aft

# FTS5
bun run benchmarks/semble/baseline-fts5.ts --pilot --k 10 --binary ./target/release/aft
```

**Output:** `pilot-report.json` with aggregate and per-category metrics.

### 2. Vera-Compatible Suite (21 tasks)

Uses [Vera](https://github.com/CortexKit/vera)'s published task corpus with line-range ground truth.

**Setup:**
```bash
cd benchmarks/aft-search
uv run python setup_corpus.py
```

**Run:**
```bash
cd benchmarks/aft-search
uv run python run_external.py --binary ../../target/release/aft
```

**FTS5-specific fixtures (12 queries):**
```bash
cd benchmarks/aft-search
python3 run_fts5_bench.py --binary ../../target/release/aft --project-root ../..
```

## Metrics

| Metric | Formula | What It Measures |
|--------|---------|------------------|
| **Recall@K** | `hits / total_relevant` | Fraction of relevant files in top-K |
| **MRR** | `1 / rank_of_first_hit` | How early the first relevant result appears |
| **nDCG@K** | `DCG / IDCG` | Quality of result ordering |

The `pilot.ts` script reports all three at K=10 (configurable via `--k`).

## Reading Results

### pilot-report.json structure

```json
{
  "k": 10,
  "aggregate": {
    "lexical":  { "mean_recall": 0.45, "mean_mrr": 0.52, "mean_ndcg": 0.48, "mean_latency_ms": 12.3 },
    "fts5":     { "mean_recall": 0.61, "mean_mrr": 0.70, "mean_ndcg": 0.64, "mean_latency_ms": 15.2 },
    "aft-grep": { "mean_recall": 0.53, "mean_mrr": 0.61, "mean_ndcg": 0.56, "mean_latency_ms": 8.7 }
  },
  "by_category": {
    "symbol":      { "fts5": { "mean_recall": 0.72, "mean_mrr": 0.81, "mean_ndcg": 0.75 } },
    "semantic":    { "fts5": { "mean_recall": 0.54, "mean_mrr": 0.62, "mean_ndcg": 0.57 } },
    "architecture": { "fts5": { "mean_recall": 0.48, "mean_mrr": 0.55, "mean_ndcg": 0.51 } }
  }
}
```

### What to look for

- **FTS5 vs lexical**: FTS5 should outperform ripgrep on symbol and semantic queries
- **Per-category**: Check if FTS5 is stronger on exact symbols vs. natural language
- **Latency**: FTS5 should be < 50ms per query for interactive use
- **aft-grep vs FTS5**: Compare trigram-indexed vs. FTS5 full-text search

## Regression Detection

```bash
# Save a baseline
cp pilot-report.json baseline.json

# After code changes, compare
bun run benchmarks/semble/ci.ts --baseline baseline.json --current pilot-report.json
```

Exits non-zero if recall drops > 5% (configurable via `--threshold`).

## Additional Benchmarks

| Script | Purpose |
|--------|---------|
| `benchmarks/semble/speed.ts` | Cold-start index + query latency |
| `benchmarks/semble/token-efficiency.ts` | Recall@token_budget curves (100–50K tokens) |
| `benchmarks/semble/ablation.ts` | Lexical vs semantic vs hybrid ablation |
| `benchmarks/aft-search/run.py` | AFT semantic baseline on in-tree fixtures |
| `benchmarks/aft-search/run-fusion-quality` | Hybrid fusion ranking investigation |

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `AFT binary not found` | Build: `cargo build --release --features semantic-fts5` |
| `Repos not cloned` | Run: `bun run benchmarks/semble/corpus.ts sync --pilot` |
| FTS5 commands fail | Ensure `fts5: { enabled: true }` in configure (scripts do this automatically) |
| Windows path issues | Scripts normalize paths to `/`; check `normalizePath()` if mismatches occur |
| `select` import error (Python) | Use Python 3.5+; `select` is Unix-only on some platforms |

## File Map

```
benchmarks/semble/
├── pilot.ts              ← Main comparison: rg vs aft-grep vs fts5
├── baseline-rg.ts        ← Ripgrep lexical baseline
├── baseline-aft.ts       ← AFT grep/semantic/hybrid baseline
├── baseline-fts5.ts      ← FTS5 baseline
├── corpus.ts             ← Clone/pin repos
├── fixtures.json         ← 50-query pilot fixture
├── annotations/          ← Per-repo relevance judgments
├── ci.ts                 ← Regression detection
├── speed.ts              ← Latency measurements
├── token-efficiency.ts   ← Recall@token curves
└── ablation.ts           ← Mode comparison

benchmarks/aft-search/
├── run.py                ← AFT semantic baseline (in-tree)
├── run_external.py       ← Vera 21-task corpus runner
├── run_fts5_bench.py     ← FTS5 quality benchmark
├── setup_corpus.py       ← Clone Vera corpus
├── metrics.py            ← Recall/MRR/nDCG formulas
├── fts5-fixtures.json    ← 12 FTS5-specific queries
└── external-fixtures.json ← 21 Vera tasks
```
