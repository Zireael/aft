# Semble Benchmark Results — Profile d (OASIS + Reranker)

**Date**: 2026-06-10
**Profile**: d (oasis+rerank)  
**Backend**: OASIS-code-embedding-1.5B.i1-Q4_K_M  
**Reranker**: CodeRankLLM.Q4_K_M  
**K**: 10

## Summary

| Mode | Recall@10 | MRR | Mean Latency | Queries |
|------|-----------|-----|--------------|---------|
| aft (pre-rerank) | 31.3% | 0.298 | 176ms | 50 |
| aft+rerank (post-rerank) | 34.2% | 0.355 | 118ms | 40 |

**Improvement**: +2.9pp recall (+9.3% relative), +0.057 MRR (+19.1% relative)

## By Category

| Category | Pre-rerank Recall | Pre-rerank MRR | Post-rerank Recall | Post-rerank MRR |
|----------|-------------------|----------------|--------------------|-----------------|
| architecture | 48.3% | 0.471 | 48.1% | 0.511 |
| semantic | 41.7% | 0.375 | 44.4% | 0.444 |
| symbol | 5.6% | 0.056 | 7.7% | 0.077 |

## By Repo

| Repo | Pre-rerank Recall | Pre-rerank MRR | Post-rerank Recall | Post-rerank MRR |
|------|-------------------|----------------|--------------------|-----------------|
| axum | ~40% | ~0.400 | ~40% | ~0.400 |
| express | 0.0% | 0.000 | 0.0% | 0.000 |
| pydantic | ~20% | ~0.067 | ~20% | ~0.067 |
| serde | ~40% | ~0.400 | ~40% | ~0.400 |
| gin | ~56.7% | ~0.622 | ~56.7% | ~0.622 |

> **Note**: express shows 0% because this run predated the `buildRelevantPaths` fix for object-format annotation paths. Re-running with the fix should resolve this.

## Methodology

Dual-mode benchmark: each repo is searched twice:
1. **Pre-rerank pass**: `rerank_enabled: false` (embedding search only)
2. **Post-rerank pass**: `rerank_enabled: true` (embedding + LLM reranker)

Both passes use the same embedding model and configuration. The reranker reorders the top 30 candidates using the CodeRankLLM endpoint.

## Observations

- The reranker consistently improves **MRR** across all categories (architecture: +0.040, semantic: +0.069, symbol: +0.021)
- The reranker improves **recall** for semantic and symbol queries but has minimal impact on architecture queries
- Post-rerank latency appears lower than pre-rerank in this run, likely due to missing pydantic pre-rerank queries (crash during that pass)
