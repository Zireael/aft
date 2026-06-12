# Bead Review Report: aft-4tu (Reranker candidate starvation)

**Reviewer:** Hephaestus via `the-fool` (Find the Failure Modes mode)  
**Date:** 2026-06-10  
**Bead Type:** bug  
**Verdict:** ACCEPTABLE with minor gaps — implementation plan is sound but missing edge cases around signal preservation and score semantics.

---

## Steelmanned Thesis

The semantic search pipeline truncates fused results to `top_k` at line 556 of `semantic_search.rs` BEFORE calling `rerank_candidates()` at line 574. This makes `rerank_max_candidates` (default 20) dead config — the reranker can never receive more than `top_k` candidates. Deferring truncation until after reranking will allow `rerank_max_candidates` to actually control reranker input size, enabling recall improvements.

---

## Code Verification

**Confirmed from source:**
- `semantic_search.rs:554-557`: `fused_more_available = results.len() > top_k; results.truncate(top_k);` — truncation occurs before reranking
- `semantic_search.rs:574`: `rerank_candidates(&ctx.config().semantic, &params.query, &results)` — receives already-truncated results
- `semantic_rerank.rs:82-83`: `max_candidates = config.rerank_max_candidates.min(results.len());` — `results.len()` is capped at `top_k`

**Root cause confirmed:** The bead's diagnosis is 100% correct.

---

## Challenges / Failure Modes Found

### 1. `fused_more_available` Signal Must Be Preserved After Deferred Truncation (P1)

**The bead says:** "After reranking, truncate to `top_k` and continue with existing diagnostics/response logic"

**Why this is incomplete:**
- Line 558 computes `more_available = fused_more_available || semantic_more_available || lexical.engine_capped` BEFORE reranking
- This signal is used in the response (line ~640) to tell the caller whether more results exist beyond `top_k`
- If we defer truncation, `results.len()` after reranking could still be > `top_k`, but we need to preserve the ORIGINAL `fused_more_available` signal (which was true because the fused pool had > top_k results)
- The bead's implementation plan step 3 says "truncate to `top_k` and continue with existing diagnostics/response logic" but doesn't mention preserving `fused_more_available`

**Mitigation:**
- Store `fused_more_available` in a local variable before calling `rerank_candidates()`, then use the stored value in the response instead of recomputing it from the (now reranked) results

---

### 2. Score Array Extraction After Reranking is Semantically Ambiguous (P2)

**Current behavior (line 617):**
```rust
let scores: Vec<f32> = results.iter().map(|result| result.score).collect();
```

**Why this is a problem after deferred truncation:**
- `HybridResult.score` contains the FUSION score (embedding + lexical fusion), not the reranker score
- After reranking, the order changes but the scores stay the same — the first result might have a lower fusion score than the second
- The `low_confidence_threshold` check (lines 619-621) tests whether ALL scores are below threshold. If reranking moves a low-scoring result to position 1, the check might incorrectly flag low confidence
- The bead doesn't mention whether scores should be updated or whether the threshold check should use the original fusion order

**Mitigation:**
- Document that `score` remains the fusion score even after reranking — this is the intended behavior per ARCHITECTURE.md (reranker judgment is unreliable, scores reflect embedding/fusion)
- The threshold check should probably use the original scores before reranking, or check only the top_k after truncation

---

### 3. Diagnostic Warning Condition is Inverted (P2)

**The bead says:** "Add diagnostic warning when `rerank_max_candidates > results.len()` (partial pool)"

**Why this is wrong:**
- `rerank_max_candidates > results.len()` means the config asks for MORE candidates than we have — that's fine, we send all available candidates
- The ACTUAL problem is `rerank_max_candidates < results.len()` — the config caps the pool and some candidates are excluded from reranking
- The bead has the inequality backwards

**Mitigation:**
- Change to: "Add diagnostic warning when `rerank_max_candidates < results.len()` — indicates candidate pool is being capped"

---

### 4. Missing Edge Case: Reranker Returns Results in Different Order Than Expected (P2)

**Current behavior:** The `rerank_candidates` function returns a `Vec<usize>` of indices in order of relevance (most relevant first). The caller at lines 588-604 maps these indices back to `HybridResult` objects.

**Edge case not covered:** What if the reranker returns indices for a SUBSET of candidates (e.g., only the top 5 out of 30)? Current code:
1. Maps returned indices to results in order
2. Appends unused originals in original order

This means positions 6-30 are the original fusion order, not reranked. The bead doesn't test or document this behavior.

**Mitigation:**
- Add test case: "reranker returns fewer indices than candidates → remaining candidates appended in original order"
- Document this behavior in the bead

---

### 5. Benchmark Harness Changes are In Scope But Under-Specified (P2)

**The bead says:** "Update benchmark harness to support separate `--candidate-k` and `--eval-k`"

**Why this is vague:**
- The benchmark is a TypeScript file (`run-semble-bench.ts`), not Rust
- The bead's scope says "Update benchmark to test with `--candidate-k 50 --eval-k 10`" but doesn't specify what changes are needed
- The `makeBenchResult` function currently scores all returned results — it needs to score only top `eval_k` while passing `candidate_k` to the search

**Mitigation:**
- Add explicit implementation step: "Modify `AftBridge.search()` call in benchmark to pass `candidate_k` instead of `k`, then slice results to `eval_k` before scoring"

---

### 6. Missing Edge Case: `rerank_enabled: false` Path Must Still Truncate (P2)

**The bead says:** "`rerank_enabled: false` → no reranking, truncation happens at fusion"

**Why this is correct but needs explicit implementation:**
- If `rerank_enabled` is false, we MUST truncate at fusion (current behavior) because there's no later truncation step
- The bead's implementation plan removes the truncation at line 556, which means the `rerank_enabled: false` path would return > top_k results
- The plan needs an explicit branch: if `rerank_enabled`, defer truncation; else, truncate at fusion

**Mitigation:**
- Add implementation step: "Add conditional: if `rerank_enabled`, defer truncation; else, keep current truncation at fusion"

---

### 7. Missing Edge Case: `top_k = 0` or `top_k = 1` (P3)

**Current behavior:**
- `top_k = 0`: `results.truncate(0)` → empty results. Reranker is skipped (`results.len() < 2`)
- `top_k = 1`: `results.truncate(1)` → 1 result. Reranker is skipped (`results.len() < 2`)

**After deferred truncation:**
- `top_k = 0`: fused pool may have >0 results, but we still need to return empty. Reranker should be skipped.
- `top_k = 1`: fused pool may have >1 results, reranker receives >1 candidates, returns reordered results, then truncated to 1

This changes behavior for `top_k = 1`: previously reranker was skipped, now it runs. Is this desirable? Probably yes, but the bead should mention it.

**Mitigation:**
- Add edge case to test coverage: "`top_k = 1` with `rerank_enabled` → reranker runs on fused pool, returns top 1"

---

## Synthesis

The bead correctly identifies and diagnoses the root cause. The implementation plan is mostly sound but has minor gaps:

1. **Preserve `fused_more_available` signal** — store before reranking, use stored value in response
2. **Fix inverted diagnostic warning** — warn when `rerank_max_candidates < results.len()`
3. **Add conditional truncation** — truncate at fusion only when `rerank_enabled: false`
4. **Clarify score semantics** — document that scores remain fusion scores after reranking
5. **Specify benchmark changes** — explicit TypeScript modifications needed

The bead is AGENT-READY with minor revisions. An agent could implement this successfully by following the plan and addressing the gaps above.

---

## Edge Cases Not Covered in Bead

| Edge Case | Severity | Notes |
|---|---|---|
| `fused_more_available` signal lost after reranking | P1 | Must store before reranking |
| Score array semantics after reranking | P2 | Scores remain fusion scores, not reranker scores |
| Diagnostic warning condition inverted | P2 | Bead says `>` but should be `<` |
| `rerank_enabled: false` path needs explicit truncation | P2 | Otherwise returns > top_k results |
| `top_k = 1` behavior changes | P3 | Previously skipped reranker, now runs it |
| Reranker returns subset of candidates | P2 | Unused originals appended — needs test |
| Benchmark harness TypeScript changes | P2 | Under-specified |

---

## Overall Assessment

| Dimension | Score | Rationale |
|---|---|---|
| Completeness | 7/10 | Core fix correct, but missing signal preservation, conditional truncation, and benchmark specifics |
| Coherence | 8/10 | Clear problem statement and solution, but diagnostic warning is inverted |
| Appropriate Staging | 9/10 | Single-file change with good test coverage — minimal blast radius |
| Scope Appropriateness | 9/10 | Well-scoped, doesn't creep into fusion or embedding logic |
| Edge Case Coverage | 6/10 | Missing signal preservation, score semantics, and subset returns |

**Final Verdict:** ACCEPT with minor revisions. The bead is nearly agent-ready. Fix the inverted diagnostic condition, add `fused_more_available` preservation, and specify the benchmark harness changes.
