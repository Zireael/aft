# Benchmark Methodology Decision

**Decision bead:** `aft-t6p.bench.quick.00`
**Date:** 2026-06-16
**Status:** Active

## Purpose

This document records the binding methodology decisions for the AFT Semble quick benchmark package. It exists so that every agent and CI check can verify the benchmark does not silently drift into misleading measurement.

## Decision 1: No runtime-generated relevance truth

**Rule:** The benchmark must never use a runtime search pass (ripgrep, grep, or any other search mode) to generate `allRelevant` or any relevance ground truth during execution.

**Rationale:** If ripgrep is both the oracle and a contestant, its own scores are circular. The benchmark exists to compare modes against an independent truth.

**Implementation:** Relevance canon is loaded from checked-in JSON files under `benchmarks/semble/canon/`. Runtime `rg` is allowed only as a baseline contestant mode, scored against the same checked-in canon as every other mode.

**Banned patterns:**
- `allRelevant = rg.results.map(...)` during benchmark execution
- Any function that derives identifier relevance from a runtime search pass
- Treating `rgSearch()` output as ground truth for scoring

## Decision 2: Suite-separated aggregation

**Rule:** Each benchmark suite must be aggregated independently. Suites must not be mixed into a single leaderboard.

**Included suites:**

| Suite | Query type | Primary modes |
|---|---|---|
| `semantic_nl` | Natural-language queries | semantic, hybrid, rerank |
| `identifier_exact` | Exact symbol names | fts5_find_symbol_exact, aft-grep, fts5_search |
| `identifier_prefix` | Symbol name prefixes | fts5_find_symbol_prefix, aft-grep, fts5_search |
| `path_lookup` | File path/glob queries | glob, fts5_search |
| `structural` | AST pattern queries | ast_search |

**Rationale:** Identifier queries and natural-language queries test fundamentally different retrieval capabilities. Mixing them into one average hides whether lexical or semantic modes are winning in their respective strengths.

## Decision 3: Attempt rows with strict denominators

**Rule:** Every requested `(suite, mode, query)` triple must emit exactly one attempt row, regardless of success or failure.

**Attempt statuses:**
- `ok` — results returned, scored normally
- `empty` — search completed, zero results returned (score = 0)
- `error` — search failed with an error (score = 0, error recorded)
- `unavailable` — mode not available in this build/config (score = 0, warning emitted)
- `timeout` — search exceeded time limit (score = 0, timeout recorded)

**Denominator rule:** Aggregate metrics (recall@k, MRR, nDCG@k) are computed over ALL attempt rows for a `(suite, mode)` pair, including empty/error/unavailable/timeout rows scored as zero. Dropping failed attempts inflates averages and hides real degradation.

## Decision 4: Latency decomposition

**Rule:** The benchmark must distinguish at minimum these latency components:

| Component | Definition |
|---|---|
| `configure_ms` | Time to send configure and receive acknowledgment |
| `index_update_ms` | Time for FTS5 index update after file changes |
| `model_load_ms` | Time to load embedding model (semantic modes) |
| `warm_query_ms` | Time for the actual search query (first query warms cache) |
| `candidate_generation_ms` | Time to generate candidate results before reranking |
| `rerank_ms` | Time for reranker to re-order candidates |
| `end_to_end_ms` | Total wall-clock time for the full search operation |

**Rationale:** Presenting cold-start time as "query latency" misleads mode comparisons. A semantic mode with 8s model load and 50ms query is fast at query time but slow end-to-end. Both perspectives are valid; they must not be conflated.

## Decision 5: Hybrid and rerank pairing

**Rule:** When a hybrid or rerank mode is benchmarked, the report must include:
- Candidate pool size (how many results the base search returned before reranking)
- Pre-rerank metrics (recall, MRR, nDCG on the candidate pool)
- Post-rerank metrics (recall, MRR, nDCG after reranking)
- Rerank latency separately from candidate generation latency

**Rationale:** A reranker that improves nDCG from 0.4 to 0.7 is valuable. A reranker that improves nDCG from 0.01 to 0.02 is not, even though the relative improvement is larger. Paired metrics make this visible.

## Decision 6: Mode eligibility

**Rule:** Each canon query may declare `eligible_modes`. A mode is only scored against a query if it appears in the query's `eligible_modes` list, or if the list is absent (meaning all modes are eligible).

**Rationale:** Scoring `ast_search` against natural-language queries, or `glob` against identifier queries, produces meaningless zeros that dilute mode-specific aggregates.

## Decision 7: Semantic context cap modes

**Rule:** Semantic modes must label which public AFT context-cap behavior they used:

| Mode | Behavior |
|---|---|
| `legacy` | The historical public `semantic_search` snippet policy. Useful as a compatibility baseline. |
| `budget` | Token-budgeted context filtering with total, per-candidate, and soft-overflow budgets. |
| `compare` | Runs both legacy and budget variants so quality, latency, snippet count, and token count can be compared side by side. |

**Rationale:** Retrieval quality can change when the reranker and user-facing output receive more than the legacy small snippet set. The benchmark must make that tradeoff visible instead of silently mixing old and new context policies.

**Required metrics:** Reports must include at least snippets/query, tokens/query, max document tokens, recall, MRR, nDCG, and latency per semantic context mode.

## Decision 8: Identifier semantic modes are opt-in

**Rule:** Identifier suites default to lexical and symbol-aware modes. Semantic backends are included only when the run explicitly passes `--identifier-semantic true`.

**Rationale:** Exact and prefix symbol lookup should be judged by lexical/symbol tools first. Starting embedding models during identifier-only runs adds cold-start noise, can trigger unrelated API context-window failures, and makes the lexical benchmark look slower than it is.

## Included AFT-native modes

| Mode | Command | Notes |
|---|---|---|
| `aft-grep` | `grep` | Trigram-indexed lexical search |
| `fts5_search` | `fts5_search` | FTS5 full-text search |
| `fts5_find_symbol_exact` | `fts5_find_symbol` (mode=exact) | Exact symbol lookup |
| `fts5_find_symbol_prefix` | `fts5_find_symbol` (mode=prefix) | Prefix symbol lookup |
| `glob` | `glob` | File path pattern matching |
| `ast_search` | `ast_search` | Structural AST pattern search |
| `semantic` | `semantic_search` | Dense embedding search |
| `hybrid` | `semantic_search` (hybrid) | Lexical + semantic fusion |
| `rerank` | `semantic_search` + rerank | Post-retrieval reranking |

**External baseline:** `rg` (ripgrep) is included as a baseline contestant only. It must never generate ground truth.

## Compliance check

Agents and CI can verify compliance with:

```bash
# Verify no runtime rg oracle pattern exists in active benchmark code
grep -rn "allRelevant.*rg\|rg.*allRelevant\|rgSearch.*ground\|allRelevant = rg" benchmarks/semble/*.ts
# Expected: zero matches in active code, or matches only in docs/comments warning about removed behavior

# Verify canon files exist
ls benchmarks/semble/canon/*.json

# Verify attempt rows include empty/error/unavailable
grep -c '"status"' benchmarks/semble/pilot-report.json
```
