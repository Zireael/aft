# Search Benchmarks

## Semble Retrieval Benchmark

The current decision-grade retrieval benchmark lives in `benchmarks/semble/pilot.ts`. It
compares semantic, hybrid, rerank, FTS5, trigram grep, path, prefix, exact-symbol, and
structural search behavior against checked-in relevance canon. Natural-language semantic
queries and lexical/symbol suites are reported separately; do not average them into one
leaderboard.

```bash
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft/aft.exe \
  --profile quick \
  --context-mode compare \
  --context-total-tokens 4096 \
  --context-per-chunk-tokens 384 \
  --context-soft-overflow-tokens 128 \
  --output .aft-bench/context-compare.json
```

Use `--context-mode compare` to benchmark legacy public snippet behavior against the new
token-budgeted context path. The JSON report includes recall, MRR, nDCG, latency,
snippets/query, tokens/query, and max document tokens for each context mode.

In compare mode, `*-legacy` rows intentionally keep snippet context only for the top three
results. This is a benchmark compatibility cap that reproduces the old public output surface;
`*-budget` rows use AFT's token-budget request fields.

The terminal output includes a `FEATURE BRANCH BENEFIT SUMMARY` table. It pairs baseline modes
such as `aft-grep` and legacy FastEmbed/API semantic search against branch capabilities such as
Model2Vec, FTS5 search/symbol lookup, hybrid retrieval, and token-budget context selection.
Recall deltas are shown in percentage points (`pp`). When budget and legacy rows have the same
recall, the budget gain is context volume (`Tok/q`, `Snip/q`) rather than ranking.

Exact/prefix identifier suites default to lexical and symbol-aware modes. Pass
`--identifier-semantic true` only when you intentionally want semantic backends included in
identifier suites; otherwise model cold starts can distort lexical timing.

See `benchmarks/semble/QUICK-BENCHMARK.md` for runner flags and
`benchmarks/semble/METHODOLOGY.md` for scoring rules.

## Feature Showcase

For demos and user-readable summaries, use `benchmarks/semble/aft-feature-showcase.ts`. It
prints a polished report comparing baseline AFT search tools with Retrieval Intelligence
behavior, including quality notes, speed deltas, active retrieval lanes, context diagnostics,
token-budget semantic context, and recommendations.

The showcase also exercises the FTS5 and exploration tool surface:

- `fts5_index`, `fts5_doctor`, `fts5_search`, `fts5_find_symbol`, and `fts5_read_symbol`
- `semantic_doctor`
- `explain_search`, `why_missed`, `aft_orient`, `aft_impact_delta`, and `aft_context_pack`

Only retrieval tools are scored with recall/MRR/nDCG in `pilot.ts`. Exploration and diagnostic
tools are better showcased with status, latency, structured evidence, and human-readable
summaries because commands like `fts5_doctor` or `aft_impact_delta` do not have a top-k
relevance set.

```bash
bun run benchmarks/semble/aft-feature-showcase.ts \
  --binary ./target/release/aft/aft.exe \
  --project-root D:/Coding/_tools/aft-src \
  --query "where is semantic search reranking handled" \
  --expected-file crates/aft/src/commands/semantic_search.rs \
  --markdown-output .aft-bench/aft-feature-showcase.md
```

## Trigram Grep Baseline

With `search_index: true`, AFT builds a trigram index in the background and serves
grep queries from memory. The legacy mode is now called `AFT-GREP`; the same live
benchmark harness also reports `AFT-FTS5` so lexical and FTS-backed lookup can be
compared side by side.

## opencode-aft (253 files)

| Query | ripgrep | AFT-GREP | Speedup |
|-------|---------|----------|---------|
| `validate_path` | 31.4ms | 1.48ms | **21x** |
| `BinaryBridge` | 31.0ms | 1.3ms | **24x** |
| `fn handle_grep` | 31.3ms | 0.2ms | **136x** |
| `search_index` | 31.5ms | 0.4ms | **71x** |

## reth (1,878 Rust files)

| Query | ripgrep | AFT-GREP | Speedup |
|-------|---------|----------|---------|
| `impl Display for` | 98.9ms | 1.10ms | **90x** |
| `BlockNumber` | 61.6ms | 2.19ms | **28x** |
| `EthApiError` | 32.7ms | 1.31ms | **25x** |
| `fn execute` | 36.6ms | 2.19ms | **17x** |

## Chromium/base (3,953 C++ files)

| Query | ripgrep | AFT-GREP | Speedup |
|-------|---------|----------|---------|
| `WebContents` | 69.5ms | 0.29ms | **236x** |
| `StringPiece` | 51.8ms | 0.78ms | **66x** |
| `NOTREACHED` | 51.6ms | 2.16ms | **24x** |
| `base::Value` | 54.4ms | 1.13ms | **48x** |

### Live lexical comparison from `pilot.ts` (serde smoke)

The saved `ri-v31-smoke-serde.json` report shows the benchmark harness reporting
both `AFT-GREP` and `AFT-FTS5` rows directly:

| Mode | Recall | MRR | nDCG | p50(ms) | p95(ms) | Queries |
|------|--------|-----|------|---------|---------|---------|
| `lexical (rg)` | 0.0% | 0.000 | 0.000 | 149.3 | 416.6 | 10/10 |
| `AFT-GREP` | 0.0% | 0.000 | 0.000 | 61.6 | 62.7 | 4/10 |
| `AFT-FTS5` | 71.4% | 0.600 | 0.627 | 62.7 | 66.4 | 7/10 |

Rare queries see the biggest gains — the trigram index narrows candidates to a few files instantly.
High-match queries still benefit from `memchr` SIMD scanning and early termination.

Index builds in ~2s for most projects (under 2K files). Larger codebases like Chromium/base
(~4K files) take ~2 minutes for the initial build. Once built, the index persists to disk for
instant cold starts and stays fresh via file watcher and mtime verification.
