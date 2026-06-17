# AFT Semble Quick Benchmark

Decision-grade benchmark for comparing AFT retrieval modes. Measures retrieval quality (recall, MRR, nDCG) and latency across semantic, lexical, FTS5, symbol, path, and structural search modes.

## Quick start

```bash

bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile extended \
  --suite all \
  --mode rg,aft-grep,fts5_search,fts5_find_symbol_exact,fts5_find_symbol_prefix,glob,ast_search,semantic_m2v,semantic_fe,semantic_api,hybrid,rerank \
  --semantic-api-url http://localhost:8090 \
  --rerank \
  --rerank-url http://localhost:8090 \
  --allow-degrade \
  --allow-seed-canon \
  --report-json bench-report.json \
  --report-md bench-report.md \
  --verbose

# Key flags:
# - --rerank enables the reranker pass (5x oversampling by default)
# - --rerank-url points to your reranker endpoint (e.g., GTE-Reranker via vLLM/TEI)
# - --allow-degrade emits status: unavailable for missing modes instead of failing
# - --allow-seed-canon includes seed-status canon rows (all 84 are currently seeds)
# - --profile extended runs all canon queries with 3 repetitions for latency stability

# Quick smoke:
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile smoke \
  --suite all \
  --semantic-api-url http://localhost:8090 \
  --rerank \
  --rerank-url http://localhost:8090 \
  --allow-degrade \
  --report-json smoke.json


# Smoke test (fastest, 2 queries per suite)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile smoke --suite all --mode rg,aft-grep --allow-degrade --report-json smoke.json --report-md smoke.md

# Quick decision-grade run (all reviewed + seed queries)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile quick --suite all --allow-degrade --report-json quick.json --report-md quick.md

# Extended run (all canon, 3 repetitions)
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft/aft.exe --profile extended --suite all --allow-degrade --report-json extended.json
```

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
bun run benchmarks/semble/pilot.ts --binary <aft> --profile smoke --suite identifier-exact --mode fts5_find_symbol_exact --report-json result.json

# Run with rerank
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --suite semantic-nl --mode semantic_m2v --rerank --rerank-url http://localhost:8090/v1/rerank --report-json result.json

# Baseline comparison
bun run benchmarks/semble/pilot.ts --binary <aft> --profile quick --suite all --baseline prior-report.json --report-json current.json
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
