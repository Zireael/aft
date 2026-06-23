# AFT Semble Quick Benchmark

Decision-grade benchmark for comparing AFT retrieval modes. Measures retrieval quality (recall, MRR, nDCG) and latency across semantic, lexical, FTS5, symbol, path, and structural search modes.

## Quick start

```bash

bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile extended \
  --semantic-api-url http://localhost:8090 \
  --rerank \
  --rerank-url http://localhost:8090 \
  --rerank-instruction "Given a code search query, retrieve relevant code snippets that answer the query." \
  --context-mode compare \
  --context-total-tokens 4096 \
  --context-per-chunk-tokens 384 \
  --context-soft-overflow-tokens 128 \
  --output bench-report.json \
  --verbose

# Key flags:
# - --rerank enables the reranker pass (10x oversampling by default)
# - --rerank-url points to your reranker endpoint (e.g., GTE-Reranker via vLLM/TEI)
# - --rerank-instruction passes the model-specific instruction as the `instruct` field
# - --profile extended runs all canon queries with 3 repetitions for latency stability
# - --context-mode compare runs legacy and token-budgeted semantic context variants

# Quick smoke:
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile smoke \
  --semantic-api-url http://localhost:8090 \
  --rerank \
  --rerank-url http://localhost:8090 \
  --output smoke.json


# Smoke test (fastest, 2 queries per suite)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile smoke --output smoke.json

# Quick decision-grade run (all reviewed + seed queries)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile quick --output quick.json

# Extended run (all canon, 3 repetitions)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile extended --output extended.json
```

## CLI Reference

| Flag | Default | Description |
|------|---------|-------------|
| `--binary <path>` | `aft` | Path to AFT binary |
| `--profile <name>` | `smoke` | Run profile: smoke, quick, extended, manual-full |
| `--k <n>` | `10` | Top-k for recall@k, MRR, nDCG@k |
| `--backend <name>` | `model2vec` | Default embedding backend for non-API modes |
| `--semantic-api-url <url>` | — | OpenAI-compatible embedding API URL |
| `--semantic-api-model <name>` | auto-detected | Embedding model name for API backend |
| `--rerank` | off | Enable reranker post-retrieval pass |
| `--rerank-url <url>` | `http://127.0.0.1:8090/v1/rerank` | Reranker API URL (auto-normalizes to /v1/rerank) |
| `--rerank-model <name>` | `GTE-Reranker-Modernbert` | Reranker model name |
| `--rerank-instruction <txt>` | — | Instruction prompt for reranker (e.g., "Given a web search query...") |
| `--oversample <n>` | `10` | Reranker oversampling multiplier (fetches k×n candidates) |
| `--query-prompt <txt>` | auto for CodeRankEmbed | Query prompt template for embedding (e.g., "Represent this query...: {query}") |
| `--interactive` | off | Interactive mode: discover models, select interactively |
| `--cache-dir <dir>` | `.bench-cache` | Repo cache directory |
| `--repo <name>` | all pilot repos | Limit the semantic NL suite to a repo name or owner/name |
| `--output <path>` | `pilot-report.json` | Save JSON report; parent directories are created automatically |
| `--context-mode <mode>` | `legacy` | Semantic public context behavior: `legacy`, `budget`, or `compare` |
| `--context-total-tokens <n>` | AFT preset | Total token budget for `budget`/`compare` modes |
| `--context-per-chunk-tokens <n>` | AFT preset | Per-candidate token budget for `budget`/`compare` modes |
| `--context-soft-overflow-tokens <n>` | `0` | Extra tokens allowed for the final snippet crossing the total budget |
| `--identifier-semantic <bool>` | `false` | Include semantic backends in identifier exact/prefix suites |
| `--verbose` | off | Verbose output (chunk sizes, model verification, rerank warnings) |
| `--include-lexical <bool>` | `true` | Include lexical identifier queries |
| `--help`, `-h` | — | Show help |

## Semantic API model discovery

When `--semantic-api-url` is provided, the benchmark can auto-discover and classify models:

- **Embedding models** — probed via `/v1/embeddings` with test input
- **Reranker models** — probed via `/v1/rerank` with test query/documents
- **Chat/LLM models** — classified as non-embedding, non-reranker

If `--semantic-api-model` and `--rerank-model` are both specified, full discovery is skipped to preserve GPU memory. The benchmark only verifies the specified models are available.

### CodeRankEmbed prompt template

CodeRankEmbed requires a query prefix for optimal retrieval:
```
Represent this query for searching relevant code: <your query>
```

The benchmark auto-applies this when model name contains "coderankembed". Override with `--query-prompt "Your prefix: {query}"`.

### Reranker instruction

Some reranker models (e.g., GTE-Reranker) support an instruction prompt:
```bash
--rerank-instruction "Given a code search query, retrieve relevant code snippets that answer the query."
```

This is sent as the `instruct` field in the `/v1/rerank` request body.

## Profiles

| Profile | Queries | Repetitions | Seed rows | Use case |
|---------|---------|-------------|-----------|----------|
| `smoke` | 2 per suite | 1 | No | Fastest validation |
| `quick` | All reviewed + seed | 1 | Yes | Decision-grade |
| `extended` | All canon | 3 | Yes | Latency stability |
| `manual-full` | Full corpus | 5 | Yes | Manual only |

## Suites

Benchmark suites are **segregated by query type**. Modes are not mixed into one leaderboard.

| Suite | Query type | Primary modes | Description |
|-------|-----------|---------------|-------------|
| `semantic_nl` | Natural language | semantic, hybrid, rerank | "how does X work" queries |
| `identifier_exact` | Exact symbol names | fts5_find_symbol_exact, aft-grep, fts5_search | "Router", "BinaryBridge" |
| `identifier_prefix` | Symbol prefixes | fts5_find_symbol_prefix, aft-grep, fts5_search | "fn handle_" |
| `path_lookup` | File paths/globs | glob, fts5_search | "src/main.rs" |
| `structural` | AST patterns | ast_search | `struct $NAME { $$$ }` |

## Modes

| Benchmark Mode | AFT Tool (OpenCode/Pi) | Rust Command | Notes |
|----------------|----------------------|--------------|-------|
| `rg` | `bash` → ripgrep | — | Baseline only, never generates ground truth |
| `aft-grep` | `grep` | `grep` | Trigram-indexed lexical search |
| `fts5_search` | `aft_fts5_search` | `fts5_search` | FTS5 full-text search |
| `fts5_find_symbol_exact` | `aft_find_symbol` (mode=exact) | `fts5_find_symbol` | Exact symbol lookup |
| `fts5_find_symbol_prefix` | `aft_find_symbol` (mode=prefix) | `fts5_find_symbol` | Prefix symbol lookup |
| `glob` | `glob` | `glob` | File path pattern matching |
| `ast_search` | `ast_grep_search` | `ast_search` | Structural AST pattern search |
| `semantic_m2v` | `aft_search` | `semantic_search` | Model2Vec embeddings |
| `semantic_fe` | `aft_search` | `semantic_search` | FastEmbed embeddings |
| `semantic_api` | `aft_search` | `semantic_search` | OpenAI-compatible API |
| `hybrid` | `aft_search` + `aft_fts5_search` | `semantic_search` + `fts5_search` | RRF fusion of lexical + semantic |
| `rerank` | `aft_search` + rerank endpoint | `semantic_search` + `/v1/rerank` | Post-retrieval reranking |

## Metric model

### Attempt rows

Every `(suite, mode, query)` triple emits exactly one attempt row:

- `status: ok` — results returned, scored normally
- `status: empty` — search completed, zero results (score = 0)
- `status: error` — search failed (score = 0, error recorded)
- `status: unav` — mode not available (score = 0, reason recorded)

**Denominator rule:** Aggregates (recall@k, MRR, nDCG@k) are computed over ALL attempt rows, including empty/error/unavailable rows scored as zero. Dropping failed attempts inflates averages.

### Latency decomposition

| Component | Definition |
|-----------|-----------|
| `configure_ms` | Configure command time |
| `index_update_ms` | FTS5 index update time |
| `model_load_ms` | Embedding model load time |
| `warmup_ms` | Warmup query time |
| `query_ms` | Actual search query time |
| `rerank_ms` | Reranker time |
| `end_to_end_ms` | Total wall-clock time |

### Rerank pairing

Rerank attempts include paired pre/post metrics:

- `pre_rerank_recall`, `pre_rerank_mrr`, `pre_rerank_ndcg`
- `post_rerank_recall`, `post_rerank_mrr`, `post_rerank_ndcg`
- `rerank_delta_ndcg` (post - pre)
- `candidate_pool_size`, `rerank_pool_size`

**Oversampling**: The reranker receives k×oversample candidates (default: 10×). Higher oversampling gives the reranker more choices to reorder, potentially improving recall at the cost of latency.

**Snippet extraction**: The reranker uses `snippet` fields from semantic search results (tree-sitter-based code blocks), not arbitrary line ranges or whole files.

### Context budget comparison

Use `--context-mode compare` to run semantic backends twice: once with legacy public
`semantic_search` snippet behavior and once with token-budgeted context filtering. Reports
include quality metrics plus token/context metrics (`snippets_per_query`,
`tokens_per_query`, and `max_doc_tokens`) so the tradeoff is visible.

```bash
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft/aft.exe \
  --profile quick \
  --semantic-api-url http://localhost:8090 \
  --context-mode compare \
  --context-total-tokens 4096 \
  --context-per-chunk-tokens 384 \
  --context-soft-overflow-tokens 128 \
  --output .aft-bench/context-compare.json
```

The soft-overflow budget is intentionally small. It lets AFT include the last useful snippet
when it crosses the total cap by a little, instead of throwing away near-fit context.

In compare mode, the benchmark applies a compatibility cap to `*-legacy` rows: only the first
three ranked results keep snippet context. This reproduces the old public semantic output
surface even when the current AFT binary can return more snippets. Budget rows keep the context
selected by AFT's token-budget request fields.

The terminal output also includes `FEATURE BRANCH BENEFIT SUMMARY`, a separate table comparing
legacy/baseline modes (`aft-grep`, legacy FastEmbed/API semantic search) against branch features
(Model2Vec, FTS5 search/symbol lookup, and token-budget context). Use this table for a quick
read on whether the branch improved quality, context volume, or latency for each capability.
Recall deltas in this table are reported in percentage points (`pp`), so `+58.1pp` means recall
went from, for example, `16.1%` to `74.2%`.

Budget and legacy rows can have identical recall because the base file ranking is unchanged;
the budget feature changes how much snippet context AFT returns for downstream reranking and
agent context. In that case, inspect `Tok/q Δ` and `Snip/q Δ` rather than expecting recall to move.

## Canon files

Checked-in relevance canon at `benchmarks/semble/canon/`:

| File | Queries | Purpose |
|------|---------|---------|
| `identifier-exact.json` | 31 | Exact symbol/file queries |
| `identifier-prefix.json` | 14 | Prefix symbol queries |
| `path-lookup.json` | 29 | File path queries |
| `structural.json` | 10 | AST pattern queries |
| `unverified-seeds.json` | 8 | Unpinned seed queries |
| `repos.json` | — | Pinned repo metadata |
| `mode-matrix.json` | — | Suite-to-mode eligibility |

### Review status

- `seed` — Generated, not yet human-validated
- `reviewed` — Human-validated against pinned checkout
- `rejected` — Invalid, excluded from scoring
- `needs_update` — Path or symbol changed

Seed rows are included in `quick` and `extended` profiles. Smoke profile requires `reviewed` rows only.

### Validation

```bash
bun run benchmarks/semble/tools/validate-lexical-canon.ts benchmarks/semble/canon
```

Checks: schema version, required fields, duplicate IDs, valid review status.

## Commands

```bash
# Validate canon
bun run benchmarks/semble/tools/validate-lexical-canon.ts benchmarks/semble/canon

# Run specific suite + mode
bun run benchmarks/semble/pilot.ts --binary <aft> --profile smoke --include-lexical true --output result.json

# Run with rerank (default 10x oversampling)
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --rerank --rerank-url http://localhost:8090/v1/rerank --output result.json

# Run with aggressive oversampling (20x)
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --rerank --oversample 20 --output result.json

# Run with reranker instruction
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --rerank --rerank-instruction "Given a code search query, retrieve relevant code snippets that answer the query." --output result.json

# Run with custom query prompt for embedding
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --semantic-api-url http://localhost:8090 --query-prompt "Represent this query for searching relevant code: {query}" --output result.json

# Interactive mode (discover and select models)
bun run benchmarks/semble/pilot.ts --binary <aft> --profile smoke --semantic-api-url http://localhost:8090 --rerank --rerank-url http://localhost:8090 --interactive --output result.json
```

## Limitations

1. **Small corpus** — 5 pinned repos, 84 seed queries. Not statistically significant for production decisions.
2. **Seed canon** — All identifier/path/structural queries are seed status. Validate against pinned checkouts before hard CI gates.
3. **Local variability** — Results depend on hardware, cache state, and model availability.
4. **Feature-gated modes** — FTS5 modes require `--features semantic-fts5`. Unavailable modes emit `status: unavailable`.
5. **Decision support only** — Quick-mode results guide backend/ranking choices. They are not publishable external claims without further validation.
6. **No runtime oracle** — Relevance truth comes from checked-in canon files, never from a runtime ripgrep pass.

## Methodology decisions

See [METHODOLOGY.md](METHODOLOGY.md) for binding decisions:
- No runtime ripgrep oracle
- Suite-separated aggregation
- Strict attempt-row denominators
- Latency decomposition
- Paired rerank metrics
- Mode eligibility per canon query

## Extending

### Add a new repo

1. Add entry to `benchmarks/semble/canon/repos.json`
2. Create `benchmarks/semble/annotations/<name>.json`
3. Run `bun run benchmarks/semble/import.ts --pilot`
4. Run `bun run benchmarks/semble/corpus.ts sync --pilot`

### Promote seed rows

1. Clone repo at pinned revision
2. Verify each query's `relevant` paths exist and contain expected symbols
3. Change `review_status` from `"seed"` to `"reviewed"`
4. Run validator
5. Commit
