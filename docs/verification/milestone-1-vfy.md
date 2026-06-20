# Milestone 1 Verification Report — aft-ri-v31-vfy1

**Date:** 2026-06-20
**Branch:** `semantic-search-enhancement`
**Base commit:** 92744a184cefa522689ed786a895a6fad41fe9e9

## Verdict: READY with warnings

## Requirement Coverage Matrix

| Bead | Criterion | Required behavior | Evidence | Status |
|------|-----------|-------------------|----------|--------|
| t1a | AC-4 | FTS5Body >= 0.1 all intents | search_plan.rs `weights_for_intent` — all 9 intents have FTS5Body >= 0.1; belt-and-suspenders floor in `from_query_shape` | PASS |
| t1a | AC-5 | active_safety_lane=TrigramBody when FTS5 disabled | search_plan.rs `resolve_safety_lane` returns TrigramBody when fts5_available=false; test `trigram_body_fallback_when_fts5_disabled` | PASS |
| t1c | AC-1 | flag=false output byte-identical | semantic_search.rs: flag guard produces `None` SearchPlan; no `search_plan_debug` in response; code path unchanged | PASS |
| t3a | AC-7 | snippet_line_budget() absent | grep confirms only comment reference; function removed, logic inlined | PASS |
| t3b | AC-1 VAL-009 ROOT CAUSE | enrich before rerank | semantic_search.rs: `enrich_context_pool` called BEFORE `rerank_candidates` when flag=true AND enrich_pool=RerankPool | PASS |
| t3b | AC-3 VAL-009c | PathOnly excluded from reranker | `rerank_skipped_by_budget` guard skips `rerank_candidates` entirely when enriched ratio insufficient | PASS |
| t3b | AC-4 VAL-009d | reranker_skipped_reason when enriched < 50% | `enrich_context_pool` returns `insufficient_enriched_ratio` when ratio < 0.5; test `reranker_skipped_insufficient_ratio` | PASS |
| t5a | AC-1 | aft_output default | pilot.ts: `let rerankContext = "aft_output"` | PASS |
| t5a | AC-3 | Zero source file reads in aft_output | pilot.ts: `applyRerank` skips `readFileSync` when `rerankContext === "aft_output"` | PASS |
| t5b | AC-7 | 20%+ canon queries hold_out | structural.json: 2/10 = 20% hold_out=true | PASS (fixed) |

## Brownfield Invariants

| Invariant | Status | Evidence |
|-----------|--------|----------|
| INV-001 grep/glob/trigram unchanged | PASS | No changes to grep_executor, pattern_compile, search_index modules |
| INV-002 Exact literal substring unchanged | PASS | Literal search path uses same `pattern_compile::compile(literal=true)` |
| INV-003 No remote service required | PASS | All new code is local; SearchPlan is entirely in-process |
| INV-005 Feature flags default off | PASS | `retrieval_intelligence_v2: bool` with `#[serde(default)]` → false |
| INV-006 Benchmark aft_output default | PASS | `rerankContext = "aft_output"` in pilot.ts |

## Reachability Audit

1. **enrich_context_pool() → rerank_candidates() ordering:** Confirmed in semantic_search.rs lines 631-648. enrich runs first, then rerank (or skip).
2. **flag=false path unchanged:** `ri_v2_enabled` guard at line 135 produces `None` SearchPlan; all extras insertions are gated by `if let Some(plan)`.
3. **snippet_line_budget truly absent:** grep confirms only 1 comment reference, no production code.

## Old-path Bypass Audit

- flag=false: executes old `enrich_snippets_from_source` after truncation (line 722). Path preserved.
- TrigramBody/DegradedLiteralBodyScan: exist in `resolve_safety_lane` fallback chain. Reachable when FTS5 disabled.

## Test Reality Check

- 3140 tests run, 3133 passed, 7 pre-existing failures (watcher + TS typecheck).
- 10 search_plan tests, 12 context_budget tests, 22 candidate tests — all passing.
- No new test regressions introduced.

## Files Changed (cumulative)

| File | Change summary |
|------|----------------|
| `crates/aft/src/search_plan.rs` | New: QueryIntent, LaneKind, SearchPlan, SearchPlanBuilder |
| `crates/aft/src/candidate.rs` | New: CandidateEntry, FusedCandidate, CandidateProvenance |
| `crates/aft/src/context_budget.rs` | New: ContextBudget, ContextBudgetResult, EnrichPool |
| `crates/aft/src/commands/semantic_search.rs` | Flag guard, enrich_context_pool, search_plan_debug |
| `crates/aft/src/config.rs` | Added `intelligence` field to Config |
| `crates/aft/src/intelligence_config.rs` | Added `retrieval_intelligence_v2` flag |
| `crates/aft/src/lib.rs` | Module registrations |
| `benchmarks/semble/pilot.ts` | --rerank-context flag, context_quality block |
| `benchmarks/semble/canon/structural.json` | hold_out field |
| `benchmarks/baseline/*` | Schema + README |
| `docs/reports/aft-ri-source-baseline.md` | Source baseline |

## Warnings

1. The `enrich_context_pool` function uses a rough token estimate (~4 tokens/line) for budget tracking. This should be refined with the actual `aft-tokenizer` API in a future Bead.
2. The benchmark `context_quality` computation is approximate — it uses the report's result arrays rather than true engine diagnostics. For production-quality metrics, the AFT diagnostics field should be parsed from the NDJSON response.
3. The `reranker_skipped_reason` is tracked in the return type of `applyRerank` but not yet populated from AFT diagnostics in the benchmark script.

## Handoff

- Milestone 1 gate: VFY1 is **READY** (all critical criteria pass, hold-out fixed to 20%).
- Next: `aft-ri-v31-ms1` can be closed with this verification evidence.
- After MS1: Track 2 (adapter implementations) can begin.
