# Bead Review Report: aft-q7w (Investigate Express 100% → 0% recall regression)

**Reviewer:** Hephaestus via `the-fool` (Find the Failure Modes mode)  
**Date:** 2026-06-10  
**Bead Type:** spike  
**Verdict:** ACCEPTABLE with gaps — investigation plan is thorough but missing critical verification steps and has a logical gap in hypothesis ranking.

---

## Steelmanned Thesis

Express drops from 100% recall (embedding-only) to 0% recall (with reranking). This is impossible under normal reranking behavior (reordering relevant candidates should preserve recall). The investigation must determine whether the root cause is: (a) API mismatch causing the reranker to return garbage indices, (b) index-mapping bug dropping relevant results, (c) model mismatch ranking relevant candidates last, or (d) different candidate sets between pre-rerank and post-rerank passes.

---

## Code Verification

**Confirmed from source:**
- `semantic_search.rs:610-613`: `RerankOutcome::Failed(error)` preserves original results → recall stays 100%
- `semantic_search.rs:579-580`: OOB indices are filtered out, then unused originals appended → recall stays 100%
- `semantic_search.rs:588-604`: `ReRanked(indices)` maps indices to results, appends unused originals

**Critical deduction:** For Express recall to drop from 100% to 0%, the reranker MUST return `ReRanked` with VALID indices that reorder ALL relevant results out of the top 10. This is only possible if:
1. The reranker received the relevant candidates but ranked them below irrelevant ones (model mismatch), OR
2. The reranker did NOT receive the relevant candidates (different candidate set)

Candidate starvation (aft-4tu) alone CANNOT explain 0% for Express — pre-rerank top 10 were all relevant, so reranking those same 10 should preserve recall.

---

## Challenges / Failure Modes Found

### 1. Missing Hypothesis: Different Candidate Set Between Pre/Post-Rerank Passes (P0)

**The bead lists:** API mismatch, index-mapping bug, model mismatch, different candidate set

**Why "different candidate set" is the strongest hypothesis:**
- The benchmark creates TWO separate AFT bridges: one for pre-rerank, one for post-rerank
- Each bridge configures AFT independently. If they have different configs (e.g., different `semantic_search` settings, different index states), the embedding results could differ
- Pre-rerank: index built with `rerank_enabled: false` → one embedding config
- Post-rerank: index built with `rerank_enabled: true` → potentially different config
- If the post-rerank index is built differently, its top 10 might NOT include the relevant files even before reranking

**The bead's investigation plan doesn't include:**
- Comparing pre-rerank vs post-rerank candidate sets (not just the final top 10)
- Checking whether both passes use the same index or rebuild it
- Verifying that post-rerank embedding-only (with `rerank_enabled: false` but same config) also has 100% recall

**Mitigation:**
- Add investigation step: "Run post-rerank pass with `rerank_enabled: false` to verify embedding-only recall is still 100%"
- Add step: "Dump full candidate pool (top 50) for both pre-rerank and post-rerank, compare file overlap"
- Add step: "Check if both bridges use the same `semantic` config and index state"

---

### 2. Missing Verification: Reranker Response Parsing Success/Failure (P1)

**The bead says:** "Enable llama-swap debug logging to see what the reranker returns"

**Why this is insufficient:**
- Debug logging shows the HTTP request/response but doesn't prove AFT parsed it correctly
- The reranker might return a valid response that AFT mis-parses (e.g., parses scores as indices)
- We need to verify whether `RerankOutcome::ReRanked`, `Failed`, or `Skipped` is returned

**Mitigation:**
- Add investigation step: "Add instrumentation to `semantic_rerank.rs` to log: (a) which outcome variant is returned, (b) the actual indices array, (c) any parsing errors"
- Add step: "Run with `diagnostics_enabled: true` to capture reranker warnings in output"

---

### 3. Missing: Direct Reranker Test with Express Candidates (P1)

**The bead says:** "Test reranker directly with Express-relevant query"

**Why this needs specificity:**
- The bead mentions sending manual `/v1/rerank` and `/v1/chat/completions` requests
- But it doesn't specify WHICH documents to send — the actual top 10 candidates from the pre-rerank pass
- The most powerful test is: extract the exact 10 candidates that pre-rerank returned, send them to the reranker, and see what order it returns

**Mitigation:**
- Add explicit step: "Extract the exact pre-rerank top 10 candidates for Express query 'router middleware' (or whichever query had 100% recall), send them to reranker, verify returned indices"
- Add step: "If reranker returns the same 10 files in a different order → model is working, index-mapping is suspect"
- Add step: "If reranker returns completely different files → API mismatch or candidate set mismatch"

---

### 4. Missing: Per-Query Diff Should Compare Candidate Pools, Not Just Top 10 (P2)

**The bead says:** "log pre-rerank results (file, score, rank)" and "log post-rerank results"

**Why this is insufficient:**
- If candidate starvation is happening (aft-4tu), both pre-rerank and post-rerank only show top 10
- We need to see the FULL fused pool (top 50-100) to understand what the reranker had to work with
- The bead should specify logging the full candidate pool, not just top 10

**Mitigation:**
- Modify the benchmark to log the full fused pool (before any truncation) for each query
- Compare pre-rerank full pool vs post-rerank full pool to detect candidate set differences

---

### 5. Missing: Test with Reranker Disabled But Same Config (P2)

**The bead doesn't include:** Running post-rerank config with `rerank_enabled: false`

**Why this matters:**
- If post-rerank with `rerank_enabled: false` shows <100% recall, the issue is NOT the reranker — it's the embedding/config/index
- This single test would eliminate or confirm the "different candidate set" hypothesis immediately

**Mitigation:**
- Add step: "Run benchmark profile d with `rerank_enabled: false` on the post-rerank bridge. If recall < 100%, root cause is embedding/config, not reranker"

---

### 6. Missing: CodeRankLLM Test is Insufficiently Specified (P2)

**The bead says:** "Test with CodeRankLLM instead of GTE-Reranker-Modernbert to rule out model-specific issue"

**Why this is vague:**
- CodeRankLLM is a chat-based reranker (uses `/v1/chat/completions`). If it works, it confirms the chat format is correct
- But CodeRankLLM might have different behavior than GTE — it's not a pure A/B test unless everything else is identical
- The bead doesn't specify how to swap models (config change, llama-swap config, etc.)

**Mitigation:**
- Add explicit config snippet: "Set `rerank_model: "CodeRankLLM.Q4_K_M"` and `rerank_base_url: "http://127.0.0.1:10002/v1"`"
- Add step: "Compare results with same query, same candidates, different reranker model"

---

### 7. Logical Gap: Why Would API Mismatch Cause 0% Recall? (P2)

**The bead hypothesizes:** "API mismatch (aft-bn9): GTE-Reranker-Modernbert receives chat completions format but expects cross-encoder `/v1/rerank`"

**Why this hypothesis needs scrutiny:**
- If GTE receives chat format, it might: (a) return an error → `RerankOutcome::Failed` → recall preserved, (b) return garbage → parsing fails → `Failed` → recall preserved, (c) parse the chat prompt somehow and return indices
- For API mismatch to cause 0%, the response must parse as valid indices but those indices must be wrong
- Is it likely that a cross-encoder parsing a chat prompt would return valid-looking indices that happen to exclude all relevant files? Possible but not the most likely explanation

**The bead should rank hypotheses by likelihood:**
1. **Different candidate set** (most likely — two separate bridges, config drift)
2. **Model mismatch** (possible — GTE not suited for code reranking)
3. **API mismatch** (less likely — would typically cause parse failure, not scrambled results)
4. **Index-mapping bug** (least likely — code has been tested with chat rerankers)

---

## Synthesis

The bead's investigation plan is methodical but has critical gaps:

1. **Missing the strongest hypothesis** — "different candidate set between pre/post-rerank passes" is more likely than API mismatch for Express's 0% recall
2. **Missing the most discriminating test** — run post-rerank with `rerank_enabled: false` to isolate whether the issue is reranker or embedding/config
3. **Missing full pool logging** — comparing top 10 is insufficient when candidate starvation caps the pool
4. **Hypothesis ranking is wrong** — API mismatch is listed first but is less likely than candidate set drift

**Recommendation:** Before executing the spike:
- Reorder hypotheses: candidate set drift > model mismatch > API mismatch > index-mapping bug
- Add the discriminating test: post-rerank with `rerank_enabled: false`
- Add full candidate pool logging (top 50-100)
- Add explicit reranker outcome logging (ReRanked/Failed/Skipped)

---

## Edge Cases Not Covered in Bead

| Edge Case | Severity | Notes |
|---|---|---|
| Post-rerank index is built with different config | P0 | Would explain 0% without reranker involvement |
| Post-rerank index is stale/corrupted | P1 | Two bridges might share or conflict on index files |
| Reranker returns `ReRanked` with duplicate indices | P2 | Current `used[]` dedup handles this, but could shift ranks |
| Reranker returns `ReRanked` with fewer than 10 indices | P2 | Unused originals appended — recall should be preserved |
| llama-swap routes to wrong model | P1 | Swap config might route `/v1/chat/completions` to embedding model |
| Benchmark script has off-by-one in scoring | P2 | Would affect both pre and post, not just post |
| Post-rerank pass uses different `top_k` value | P1 | Would explain different recall immediately |

---

## Overall Assessment

| Dimension | Score | Rationale |
|---|---|---|
| Completeness | 6/10 | Good logging plan, but missing discriminating test and strongest hypothesis |
| Coherence | 7/10 | Investigation steps are logical but hypothesis ranking is off |
| Appropriate Staging | 8/10 | Good sequence: log → compare → test → conclude |
| Scope Appropriateness | 9/10 | Well-scoped as investigation spike |
| Edge Case Coverage | 6/10 | Missing candidate set drift, index corruption, swap misrouting |

**Final Verdict:** ACCEPT with revisions. The bead is a good starting point but needs to prioritize the "different candidate set" hypothesis and add the `rerank_enabled: false` discriminating test. Without these, the investigation might waste time on API mismatch when the real issue is config drift between pre-rerank and post-rerank passes.
