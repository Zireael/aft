# Plan: Configurable Snippet Enrichment for Oversampling

## Problem

AFT's `enrich_snippets_from_source()` only enriches **top-3** results with live snippets:
- Rank 0: 20 lines
- Ranks 1-2: 5 lines
- Rank 3+: **no snippet** (empty string)

This limits reranking and RRF fusion pipelines that use oversampling (k×10 or k×20 candidates). With 100 candidates but only 3 snippets, the reranker receives 97 file paths it can't meaningfully score.

## Current Implementation

```rust
// crates/aft/src/commands/semantic_search.rs

fn snippet_line_budget(global_rank: usize) -> usize {
    match global_rank {
        0 => 20,
        1 | 2 => 5,
        _ => 0,  // ← rank 3+ gets nothing
    }
}

fn enrich_snippets_from_source(results: &mut [HybridResult]) -> bool {
    // ... reads source files, applies budget per rank
}
```

Called at line 648:
```rust
let snippets_incomplete = enrich_snippets_from_source(&mut results);
```

## Proposed Changes

### 1. Add config option to `SemanticConfig`

**File:** `crates/aft/src/config.rs`

```rust
pub struct SemanticConfig {
    // ... existing fields ...

    /// Maximum number of results to enrich with source snippets (default: 3).
    /// Higher values provide more context for reranking and fusion pipelines
    /// at the cost of increased latency and token usage.
    /// Set to 0 to disable snippet enrichment entirely.
    #[serde(default = "default_snippet_enrichment_limit")]
    pub snippet_enrichment_limit: usize,

    /// Line budget for snippet enrichment per rank tier.
    /// Format: [rank0_lines, rank1_lines, rank2_lines, default_lines]
    /// Default: [20, 5, 5, 0]
    #[serde(default = "default_snippet_line_budgets")]
    pub snippet_line_budgets: [usize; 4],
}

fn default_snippet_enrichment_limit() -> usize { 3 }
fn default_snippet_line_budgets() -> [usize; 4] { [20, 5, 5, 0] }
```

### 2. Update `snippet_line_budget()` to use config

**File:** `crates/aft/src/commands/semantic_search.rs`

```rust
fn snippet_line_budget(global_rank: usize, config: &SemanticConfig) -> usize {
    if global_rank >= config.snippet_enrichment_limit {
        return 0;
    }
    match global_rank {
        0 => config.snippet_line_budgets[0],
        1 => config.snippet_line_budgets[1],
        2 => config.snippet_line_budgets[2],
        _ => config.snippet_line_budgets[3],
    }
}
```

### 3. Update `enrich_snippets_from_source()` signature

**File:** `crates/aft/src/commands/semantic_search.rs`

```rust
fn enrich_snippets_from_source(
    results: &mut [HybridResult],
    config: &SemanticConfig,
) -> bool {
    // ... existing logic, but call snippet_line_budget(rank, config)
}
```

Update all call sites to pass config.

### 4. Add NDJSON protocol parameter

**File:** `crates/aft/src/commands/semantic_search.rs`

Add optional `snippet_enrichment_limit` to the semantic_search command params:

```rust
pub struct SemanticSearchParams {
    // ... existing fields ...
    pub snippet_enrichment_limit: Option<usize>,
}
```

When provided, override the config value for this request.

### 5. Add TypeScript bridge support

**File:** `packages/aft-bridge/src/bridge.ts` or tool definition files

Pass through the new parameter when calling semantic_search.

### 6. Update benchmark script

**File:** `benchmarks/semble/pilot.ts`

Add `--snippet-limit <n>` flag:

```typescript
let snippetLimit = 3; // default

// In CLI parsing:
case "--snippet-limit": snippetLimit = parseInt(args[++i], 10) || 3; break;

// In semantic search call:
const resp = await session.call({
  command: "semantic_search",
  query,
  topK: k,
  snippet_enrichment_limit: snippetLimit,
}, 30_000);
```

## Usage Examples

### Benchmark with full snippet enrichment

```bash
# Enrich all 100 candidates (k=10, oversample=10)
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile smoke --suite all \
  --semantic-api-url http://localhost:8090 \
  --rerank --oversample 10 \
  --snippet-limit 100 \
  --verbose
```

### Conservative enrichment (default behavior)

```bash
# Only top-3 get snippets (current behavior)
bun run benchmarks/semble/pilot.ts \
  --binary ./target/release/aft \
  --profile smoke --suite all \
  --snippet-limit 3
```

### AFT config file

```json
// ~/.config/aft/config.json or project .aft/config.json
{
  "semantic": {
    "snippet_enrichment_limit": 10,
    "snippet_line_budgets": [20, 10, 5, 3]
  }
}
```

## Impact Analysis

### Performance

| snippet_enrichment_limit | Files read per query | Latency impact |
|-------------------------|---------------------|----------------|
| 3 (default) | ~2-3 | baseline |
| 10 | ~5-8 | +20-50ms |
| 50 | ~15-25 | +100-200ms |
| 100 | ~30-50 | +200-500ms |

### Token usage

| snippet_enrichment_limit | Tokens per query (reranker) |
|-------------------------|----------------------------|
| 3 | ~500 |
| 10 | ~2,000 |
| 50 | ~10,000 |
| 100 | ~20,000 |

### Quality

More snippets → reranker has better candidates to reorder → potentially higher recall.

## Implementation Order

1. **Config changes** (config.rs) — add fields with defaults
2. **Budget function** (semantic_search.rs) — update snippet_line_budget
3. **Enrichment function** (semantic_search.rs) — update enrich_snippets_from_source
4. **Protocol** (semantic_search.rs) — add param to command handler
5. **Bridge** (TypeScript) — pass through parameter
6. **Benchmark** (pilot.ts) — add --snippet-limit flag
7. **Tests** — verify existing behavior unchanged with default config
8. **Documentation** — update QUICK-BENCHMARK.md

## Risks

1. **Memory usage** — reading more files increases peak memory
2. **Token limits** — reranker may hit context window with many snippets
3. **Latency** — more disk reads per query
4. **Breaking change** — defaults preserve current behavior, no risk

## Alternatives Considered

1. **Increase rank-budgeted lines** — simpler but less flexible
2. **Lazy snippet loading** — load on-demand in reranker, not in semantic search
3. **Separate "rerank mode"** — different command that always returns full snippets

Option 1 (configurable budget) is chosen for flexibility and backward compatibility.
