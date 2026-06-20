# AFT Retrieval Intelligence v1 — Source Baseline

**Recorded:** 2026-06-20
**Epic:** aft-ri-v31
**Branch:** `semantic-search-enhancement`

## SRC-000: HEAD Commit SHA

```
92744a184cefa522689ed786a895a6fad41fe9e9
```

**Commit message:** `docs: update architecture and structure for semantic search features`

**Recent history (5 commits):**
```
92744a18 docs: update architecture and structure for semantic search features
ea0549d2 docs(bench): add snippet enrichment plan + fix reranker document loading
ba68e039 feat(bench): add --rerank-instruction flag + update docs
6a716fc7 feat(bench): add --oversample flag for reranker oversampling
c9dd38b8 fix(bench): improve reranker document warning
```

## Branch Divergence

Local branch `semantic-search-enhancement` is **in sync** with `zireael/semantic-search-enhancement`.
No divergence detected. No PRD-revision Bead required (AC-9: not triggered).

## SOURCE-CONDITIONAL Resolutions

### AC-5: Feature Flag Name

| Item | Finding |
|------|---------|
| **Planned flag** | `retrieval_intelligence_v2` (per PRD ADR-011) |
| **Current status** | NOT YET IMPLEMENTED in Rust source |
| **Existing gate** | `#[cfg(feature = "semantic-fts5")]` in `crates/aft/Cargo.toml:27` |
| **Gate location** | `commands/mod.rs:25-26`, `main.rs:636-645` |
| **Implication** | T1c must create the `retrieval_intelligence_v2` config flag; current `semantic-fts5` is a Cargo feature, not a runtime config flag |

### AC-7: aft-tokenizer Token-Count API

| Item | Finding |
|------|---------|
| **Function** | `pub fn count_tokens(text: &str) -> usize` |
| **File** | `crates/aft-tokenizer/src/claude.rs:18` |
| **Re-export** | `crates/aft-tokenizer/src/lib.rs:5` — `pub use claude::{count_tokens, encode};` |
| **Mechanism** | BPE/Lookup hybrid matching Claude's encoding via `ai-tokenizer` |

### AC-8: NDJSON Dispatcher Entry Point

| Item | Finding |
|------|---------|
| **File** | `crates/aft/src/main.rs:545` |
| **Function** | `fn dispatch(req: RawRequest, ctx: &AppContext) -> Response` |
| **Pattern** | `match req.command.as_str()` with ~60 string arms |
| **Feature-gated arms** | Lines 636–645, gated by `#[cfg(feature = "semantic-fts5")]` |
| **Unknown command** | Line 652, returns `"unknown_command"` error |
| **Handler modules** | `crates/aft/src/commands/<name>.rs`, declared in `commands/mod.rs:1-62` |
| **Adding new commands** | Add a new match arm in `dispatch()` + handler function in `commands/` |

### AC-6: GraphHealth State

| Item | Finding |
|------|---------|
| **Type** | `GraphHealth` enum |
| **File** | `crates/aft/src/ril_indexer.rs:11` |
| **Variants** | `Disabled`, `Cold`, `Indexing`, `Healthy`, `Stale`, `Rebuilding`, `Degraded`, `Corrupt` |
| **`usable()`** | Returns `true` for `Healthy`, `Stale`, `Degraded` |
| **Related** | `GraphHealthReport` (line 136), `GraphCircuitBreaker` (line 56) |
| **Testability** | Plain enum with `PartialEq`/`Eq` + `Copy`; fully unit-testable without database |

### Reranker Config

| Field | Type | Default | Location |
|-------|------|---------|----------|
| `rerank_enabled` | `bool` | `false` | `config.rs:237` |
| `rerank_model` | `Option<String>` | `None` (fallback: `codellama/codellama:7b-instruct`) | `config.rs:240` |
| `rerank_base_url` | `Option<String>` | `None` | `config.rs:244` |
| `rerank_api_key_env` | `Option<String>` | `None` | `config.rs:247` |
| `rerank_timeout_ms` | `u64` | `15000` | `config.rs:250` |
| `rerank_max_candidates` | `usize` | `20` | `config.rs:253` |
| `rerank_max_candidate_chars` | `usize` | `2500` | `config.rs:256` |
| `rerank_api_type` | `RerankApiType` | `Chat` | `config.rs:260` |
| `rerank_max_candidate_chars_cross_encoder` | `usize` | `512` | `config.rs:264` |

## Source Files Verified

All 12 SOURCE-VERIFIED files exist:

| File | Status |
|------|--------|
| `crates/aft/src/commands/semantic_search.rs` | ✅ exists |
| `crates/aft/src/query_shape.rs` | ✅ exists |
| `crates/aft/src/config.rs` | ✅ exists |
| `crates/aft/src/semantic_rerank.rs` | ✅ exists |
| `crates/aft/src/fts5_planner.rs` | ✅ exists |
| `crates/aft/src/fts5_store.rs` | ✅ exists |
| `crates/aft/src/vector_store.rs` | ✅ exists |
| `crates/aft/src/callgraph.rs` | ✅ exists |
| `crates/aft/src/mutation_risk.rs` | ✅ exists |
| `crates/aft/src/ril_indexer.rs` | ✅ exists |
| `crates/aft/src/observability_ledger.rs` | ✅ exists |
| `benchmarks/semble/pilot.ts` | ✅ exists |

## Benchmark Schema

- **Location:** `benchmarks/semble/schema.json`
- **Schema version:** 1
- **Required fields:** `schema_version`, `repos`, `annotations`
- **Annotation categories:** `symbol`, `semantic`, `architecture`
- **Fixture baseline:** `benchmarks/baseline/schema-2026-06-18.json`

## Validation Commands

Build and benchmark validation must be run via Docker:
```bash
cd "D:/Coding/_tools/aft-src" && bash scripts/zir-aft-check.sh quick --keep-going
```

Direct `cargo` commands are forbidden outside Docker per repository policy.

## Unresolved / Notes for Track 6

- **callgraph APIs**: `callgraph.rs` exists but specific graph traversal APIs needed
  by Track 6 (t6a/t6b/t6c) must be inspected during Track 6 implementation.
- **GraphHealth on test repo**: Requires a test repository with indexed graph data
  to observe runtime state; unit-testable variants confirmed.
- **Reranker model availability**: Default reranker model (`codellama/codellama:7b-instruct`)
  requires a running inference endpoint; benchmark runs need `--rerank` flag and
  a model server.
