# Bead Review: aft-t6p.2 — OpenAI-compatible reranking pipeline

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ⚠️ Issues found

---

## 1. Steelmanned Thesis

Add an optional reranking pipeline to AFT's semantic search. When configured, overfetch first-stage retrieval candidates, split them into windows, send each window to an OpenAI-compatible chat/completions backend with a deterministic listwise reranking prompt, parse the returned JSON robustly (bare array, markdown-fenced, unknown IDs dropped, missing IDs appended), and return the reordered top-K results. On any failure (timeout, HTTP error, parse failure), fall back to original first-stage ordering with a logged warning — unless strict mode is configured. Full backward compatibility: disabled by default, no change to existing search behavior.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | An LLM chat/completions endpoint is a good reranker. | Chat models return tokens by auto-regressive generation — the reranking prompt asks it to "reorder these candidates" and the model generates an ordered list of IDs. This works for listwise reranking, but generation is slower and more expensive than dedicated cross-encoders (e.g., Cohere Rerank, BGE-reranker). The bead correctly labels non-OpenAI backends as out-of-scope, but should explicitly note that this approach has a cost/latency tradeoff vs. cross-encoders. |
| A2 | Deterministic reranking prompt is sufficient. | LLMs are non-deterministic by nature. Even with `temperature=0`, the same prompt can produce slightly different outputs across requests. The acceptance criteria should test that the reranking *trend* is correct (relevant items move up), not that identical ordering is guaranteed. |
| A3 | Windowed reranking preserves global ordering. | Splitting candidates into independent windows and reranking each window means candidates in window 2 could be *globally* better than all candidates in window 1, but they'll never move ahead. This is a known limitation of windowed listwise reranking — the bead should document this caveat. |
| A4 | SSRF validation is trivially reusable from embedding backends. | If embedding backend SSRF validation allows certain patterns and reranker validation mirrors it, the two must evolve together. A shared validation function should be extracted, not copy-pasted. The bead says "reuse embedding backend validation" but doesn't specify how. |
| A5 | API keys are handled safely. | The acceptance criteria say "API keys are not stored in config or logged." But if the backend URL includes an API key as a query parameter (common for some providers), the URL itself leaks the key in logs. SSRF validation should strip or mask query params for logging. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **Reranker prompt injection**: If candidate code excerpts contain text that interferes with the reranking prompt (e.g., "Ignore all previous instructions"), the LLM could reorder in unexpected ways. | Low | Medium | The prompt must clearly delimit candidates (numbered list, XML tags) and instruct the model to treat the instruction as authoritative. Add a test with a known prompt-injection candidate excerpt. |
| F2 | **Token limit exhaustion**: Code excerpts for many candidates could exceed the model's context window, causing truncated output or errors. | Medium | Medium | The window_size config and per-candidate truncation should account for the model's context limit minus the prompt overhead. This should be documented and checked at config validation time. |
| F3 | **Infinite loop on partial JSON parse failure**: The JSON parser encounters a truncated array response (e.g., closes `]` after 5 of 10 expected IDs). If the parser returns success with partial results, remaining candidates are silently dropped. | Medium | High | The parser should distinguish "valid complete array" from "valid but shorter than expected." The spec says "missing IDs appended" — this implies the routine should detect how many IDs were expected and pad. |
| F4 | **Rerank caching not considered**: If the same query and candidate set are reranked multiple times, each call incurs API cost and latency. No mention of caching. | Medium | Low (MVP) | Caching is out of scope for MVP, but should be noted as future work to avoid redesign. |
| F5 | **Strict mode undefined**: "unless strict mode is explicitly configured" — but the acceptance criteria don't define what strict mode does. Does it fail the search? Return an error? The bead mentions it but leaves it unspecified. | Low | Medium | Define strict mode behavior explicitly in the acceptance criteria. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| Disabled preserves existing ordering | ✅ Clear | Base case |
| Enabled reorders per mocked response | ✅ Clear | Core functionality |
| Invalid JSON → fallback with warning | ✅ Clear | Graceful degradation |
| Missing IDs appended in original order | ✅ Clear | Robust parsing |
| Unknown IDs silently ignored | ✅ Clear | Robust parsing |
| Timeout/failure → logged warning only | ✅ Clear | Non-fatal design |
| Config without rerank block → disabled | ✅ Clear | Backward compat |
| Validation: enabled + missing base_url → error | ✅ Clear | Config safety |
| SSRF validation on reranker base_url | ⚠️ Clear but undetailed | Reuse mechanism not specified |
| API keys not in config or logs | ⚠️ Needs detail | URL query param risk |
| All existing tests pass | ✅ Clear | Non-regression |

### Missing or Under-specified Items

1. **Strict mode undefined**: Mentioned in desired behavior but never defined in acceptance criteria. What should AFT do when reranker fails in strict mode? Fail the entire search? Return an error response? The term "strict mode" is used without definition.
2. **SSRF validation reuse mechanism**: "Reuse embedding backend validation" — is this a shared function? A trait? A config struct that both backends reference? Should be extracted to a shared utility, not copied.
3. **Performance characteristics undocumented**: No guidance on window_size defaults, expected latency added per window, or token budget estimation. The docs bead (aft-t6p.5) covers this separately, which is fine — but the bead shouldn't claim it's complete without a note.
4. **Logging of reranker warnings**: The spec says "emit warning" on fallback — where exactly? stderr? logger? AFT's existing log pattern should be called out.

### Scope Correctness

**In scope**: All appropriate. The overfetch → rerank → top-K flow is well-articulated.

**Out of scope**: Reasonable exclusions. One potential omission — **reranking prompt engineering guidance** should at least reference the prompt template mechanism from Feature 1, since the reranking prompt might benefit from configurable prompt templates too.

---

## 5. Staging Assessment

Placed as Feature 2 in the sequence. This is correct:
- Depends conceptually on Feature 1 (prompt templates) for the config pattern, but the actual reranking logic is independent.
- Must be implemented before Feature 3 (metrics) can instrument reranking latency.
- Properly separated from TUI, docs, and tests.

**One staging concern**: The bead assumes the search pipeline integration point is known and stable. If Feature 1's trait refactor changes the pipeline structure significantly, Feature 2 may need adaptation. This risk is manageable with coordination but should be noted.

---

## 6. Overall Assessment

**Comprehensiveness**: 8/10 — Strong coverage of the reranking flow, error handling, and config safety.

**Completeness**: 6/10 — Strict mode is mentioned but undefined. SSRF reuse mechanism is unspecified. The behavioral contract for "fallback with warning" lacks precision (where does the warning go?).

**Coherence**: 9/10 — Internally consistent. Config struct, trait, implementation, integration, and fallback are well-described.

**Scoping**: 9/10 — Cleanly bounded. The windowed listwise approach is the right MVP scope. Non-OpenAI backends properly deferred.

**Edge cases**: 8/10 — Excellent coverage of JSON parsing edge cases (bare array, markdown, unknown IDs, missing IDs, parse failure). Missing: prompt injection in candidate excerpts, token limit exhaustion for large windows.

**Key recommendations**:
1. **Define strict mode** explicitly in acceptance criteria (fail search? return error response?).
2. **Extract SSRF validation** to a shared utility function referenced by both embedding and reranker backends.
3. **Add a test for prompt injection** — a candidate whose code excerpt tries to hijack the reranking prompt.
4. **Document window sizing** relative to model context limits (even if just a note in the acceptance criteria).
5. **Clarify log destination** for fallback warnings (stderr? logger?).
6. **Add strict mode acceptance criteria** that match whatever definition is chosen.
