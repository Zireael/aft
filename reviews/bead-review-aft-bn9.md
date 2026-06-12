# Bead Review Report: aft-bn9 (Multi-reranker API support)

**Reviewer:** Hephaestus via `the-fool` (Find the Failure Modes mode)  
**Date:** 2026-06-10  
**Bead Type:** feature  
**Verdict:** ACCEPTABLE with gaps — implementation plan needs refinement before execution.

---

## Steelmanned Thesis

AFT should support multiple reranker API formats by adding a `rerank_api_type` config field (`"chat"` | `"rerank"` | `"auto"`, default `"chat"`). This will fix the API mismatch where GTE-Reranker-Modernbert receives chat-completion format prompts instead of proper cross-encoder `/v1/rerank` input, restoring benchmark recall.

---

## Challenges / Failure Modes Found

### 1. Endpoint Probing Strategy is Naive (P1)

**The bead proposes:** "probe `/v1/rerank` health endpoint, fall back to chat if unavailable"

**Why this fails:**
- llama.cpp server does not expose a `/health` subpath under `/v1/rerank`. The `/v1/rerank` endpoint itself is a POST endpoint that expects `{query, documents, top_n}` — sending a HEAD or GET will return 405 Method Not Allowed, not a useful signal.
- A chat-completions server (e.g., vLLM, llama.cpp with chat model) will return 404 for `/v1/rerank`, but so will a server that's simply down or misconfigured. Auto-detection cannot distinguish "this is a chat server" from "this server doesn't support reranking at all."
- Probing adds 1-2 round-trip latencies to every search query when `rerank_api_type: "auto"`. The bead mentions "auto-detection cache" but provides no caching strategy (TTL, in-memory vs persistent, invalidation on config change).

**Mitigation:**
- Send a minimal valid `/v1/rerank` request with 2 dummy documents to probe, not a health check.
- Cache the detected type in `AppContext` for the process lifetime (not per-query).
- On probe failure, log the exact HTTP status and response to diagnostics.
- Consider removing `"auto"` entirely and requiring explicit configuration — it's clearer and avoids silent misdetection.

---

### 2. Cross-Encoder Response Format is Provider-Specific (P1)

**The bead assumes:** `{results: [{index, relevance_score}]}`

**Why this fails:**
- Jina AI `/v1/rerank` returns `{results: [{index, relevance_score, document: {text}}]}`
- Cohere returns `{results: [{index, relevance_score}]}`, same as the bead's assumption
- Some providers return `scores` instead of `results`, or nest under `data`
- llama.cpp's `/v1/rerank` is relatively new and may not match OpenAI's format exactly

**Mitigation:**
- Define a provider enum (`OpenAiRerank`, `JinaRerank`, `LlamaCppRerank`) with per-provider response parsers.
- Start with OpenAI-compatible format as the default, but make the parser lenient (accept both `results` and `data` top-level keys).
- Add a `rerank_provider` config field that defaults to `"openai_compatible"`.

---

### 3. No Specification of What Goes into `documents` Field (P1)

**The bead says:** "sends `{model, query, documents, top_n}`" but does not define what `documents` contains.

**Why this matters:**
- Current chat format sends: `[i] file.rs fn_name 10:20 "snippet..."`
- Cross-encoders expect raw document text, not metadata-wrapped text. Sending `[i] file.rs fn_name` prefix wastes context window and may confuse the model.
- However, stripping metadata means the reranker loses file/location context that could help relevance judgment.
- The `rerank_max_candidate_chars` config (default 2500) limits snippet size — for cross-encoders, this should probably be shorter since the model has to process all documents in a single forward pass.

**Mitigation:**
- For cross-encoder format, send clean document text (snippet only, no metadata prefix).
- Add `rerank_max_candidate_chars_cross_encoder` config (default 512 or 1024) separate from chat format, since cross-encoders typically have smaller context windows.
- Preserve file/symbol metadata outside the documents array for post-rerank mapping.

---

### 4. Default `"chat"` Silently Preserves Broken Behavior (P2)

**The bead sets default:** `rerank_api_type: "chat"`

**Why this is a trap:**
- Existing users with `rerank_enabled: true` who upgrade AFT will continue using chat format even if their reranker is a cross-encoder. No warning, no migration guide.
- The benchmark profile d currently uses GTE-Reranker-Modernbert without specifying `rerank_api_type` — with default `"chat"`, it will continue to fail.

**Mitigation:**
- On `rerank_enabled: true` with default `"chat"`, emit a one-time diagnostic warning: "Using chat-format reranking. If your reranker is a cross-encoder, set `rerank_api_type: 'rerank'`."
- Update the benchmark profile d to explicitly set `rerank_api_type: "rerank"`.

---

### 5. Missing Context Window Handling for Cross-Encoders (P2)

**The bead does not address:** Cross-encoder models have a combined context window for query + all documents.

**Why this fails:**
- `rerank_max_candidates: 20` × `rerank_max_candidate_chars: 2500` = 50,000 chars of documents + query prompt
- GTE-Reranker-Modernbert has a 8192-token context window (~6000-7000 chars of English text)
- Sending 20 candidates × 2500 chars will overflow the cross-encoder's context window, causing truncation or errors

**Mitigation:**
- For cross-encoder mode, calculate total document length and either:
  a. Reduce `rerank_max_candidates` dynamically to fit within context window, OR
  b. Truncate each document proportionally to fit
- Add `rerank_context_window_tokens` config (default 8192) for cross-encoder mode.
- Use the `tokenizers` crate (already a dependency via `local_embed.rs`) to count tokens before sending.

---

### 6. Test Coverage Mismatch with Implementation Plan (P2)

**Test coverage says:** "Auto-detection probes endpoints, falls back correctly"

**Implementation plan says:** "probe `/v1/rerank` health endpoint, fall back to chat if unavailable"

**The mismatch:** The test expects actual probing behavior but the plan describes a health endpoint check. These are different operations with different failure modes. The test should specify what request is sent (GET /health vs POST /v1/rerank with dummy data) and what responses are considered success/failure.

---

### 7. Missing: What Happens When Reranker Returns Fewer Results Than Candidates? (P2)

**Current behavior:** The code at lines 588-604 in `semantic_search.rs` handles `RerankOutcome::ReRanked(indices)` by:
1. Marking used indices
2. Appending unused originals in original order

**The bead doesn't mention:** Cross-encoder rerankers typically return `top_n` results (not all candidates reordered). If `top_n < candidates.len()`, the remaining candidates are silently appended — but this is actually fine with current code. However, the bead should explicitly test this.

---

### 8. Missing: Benchmark Verification Step is Under-Specified (P3)

**Acceptance criteria says:** "Benchmark profile d with GTE-Reranker-Modernbert shows restored recall"

**Why this is vague:**
- "Restored recall" doesn't define a threshold. Is 80% good enough? 95%?
- Express dropping from 100% to 0% is the canary — the acceptance criteria should explicitly require Express recall ≥ 80% post-rerank.
- The benchmark should also compare against embedding-only baseline to confirm reranker adds value.

---

## Synthesis

The bead correctly identifies the API mismatch as a root cause but underestimates the complexity of cross-encoder integration. The implementation plan is staged reasonably (config → parser → tests → benchmark) but skips critical concerns:

1. **What goes in `documents`** — needs explicit definition
2. **Context window limits** — cross-encoders have hard token limits that chat models don't
3. **Provider-specific response formats** — need lenient parsing or provider enum
4. **Auto-detection is risky** — probing a POST endpoint with HEAD/GET is unreliable

**Recommendation:** Before implementing, refine the bead to:
- Replace "auto" with explicit provider enum, or define a robust probe strategy
- Add `rerank_max_candidate_chars_cross_encoder` config
- Define exact `documents` array contents
- Update test coverage to match implementation plan
- Set concrete benchmark thresholds (Express recall ≥ 80%)

---

## Edge Cases Not Covered in Bead

| Edge Case | Severity | Notes |
|---|---|---|
| Cross-encoder returns negative scores | P2 | Current code uses indices only, not scores. If cross-encoder returns scores, should we use them? |
| Cross-encoder returns duplicate indices | P2 | Current `used[]` dedup handles this, but should be tested |
| Cross-encoder returns indices in descending score order (least relevant first) | P1 | OpenAI `/v1/rerank` returns `results` sorted by relevance_score descending. But what if a provider returns ascending? |
| `rerank_max_candidates` > fused pool size | P2 | Current code does `min(max_candidates, results.len())` — correct but untested for cross-encoder path |
| Reranker endpoint returns 200 OK with empty `results` array | P2 | Should fall back to original order |
| Reranker endpoint returns 200 OK with `results: null` | P2 | Parser should handle null gracefully |
| Network timeout during cross-encoder call | P2 | Uses same `rerank_timeout_ms` config — acceptable |
| Cross-encoder model name mismatch | P3 | llama.cpp `/v1/rerank` may require specific model name in `model` field |

---

## Overall Assessment

| Dimension | Score | Rationale |
|---|---|---|
| Completeness | 6/10 | Core path covered, critical gaps in context window, document format, and auto-detection |
| Coherence | 7/10 | Implementation plan is logical but test coverage doesn't match plan |
| Appropriate Staging | 8/10 | Good sequence: config → parser → tests → benchmark |
| Scope Appropriateness | 7/10 | In/out of scope well-defined, but some "out of scope" items (context window) are actually in-scope for viability |
| Edge Case Coverage | 5/10 | Missing context window, provider format variance, document content definition |

**Final Verdict:** REVISE before execution. The bead captures the right problem but the solution sketch needs refinement on cross-encoder specifics before an agent can implement it safely.
