# Bead Review: aft-t6p.1 — Embedding prompt-template support

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ⚠️ Issues found

---

## 1. Steelmanned Thesis

Add optional `query_prompt_template` and `document_prompt_template` string fields to AFT's semantic backend config. Split the `EmbeddingBackend` trait from a single `embed()` method into `embed_query()` and `embed_documents()`. Apply templates to query/document text before embedding (when configured). Update the semantic index fingerprint to include a hash of `document_prompt_template` so that document template changes trigger index rebuilds. Keep `query_prompt_template` changes diagnostic-only (no rebuild). Maintain full backward compatibility: existing configs without these fields deserialize to `None`, and all existing backends work unchanged.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | The trait split from `embed()` → `embed_query()` + `embed_documents()` can be done cleanly with a default implementation for backward compat. | The plan says "keeping a default implementation that calls embed for backward compat if needed" — but `embed` would be removed as a required method. A default impl on the new methods that calls a non-existent method doesn't compile. The actual path is either: (a) keep `embed()` as a default method and have `embed_query`/`embed_documents` delegate to it, or (b) make `embed_query`/`embed_documents` required with concrete impls in every backend. "If needed" is vague — this needs to be resolved to a concrete strategy before implementation. |
| A2 | `{query}` and `{text}` are the only placeholders needed. | What if a model needs both the query and some metadata (language, max_tokens, task type) in the prompt? A single-placeholder approach works for current models but may not generalize. The bead should either commit to extensibility (named placeholders) or explicitly limit scope. |
| A3 | Template application performance is negligible. | For batch document embedding with thousands of chunks, string replacement per chunk is fine — but if the embedding backend internally batches, the template must be applied *before* the batch enters the backend, not inside it. The bead's architecture must ensure the template is applied at the right layer. |
| A4 | "All existing tests pass unchanged" after a trait refactor. | If the trait changes signature, any mock/test that implements `EmbeddingBackend` must be updated. The trait split is *not* frictionless unless `embed()` is kept as a default method AND test impls aren't touched. The AC should clarify how existing test impls are handled. |
| A5 | Fingerprint stability is well-defined. | "None document_prompt_template always produces same hash" — this needs a canonical representation (e.g., hash the empty string, not `"None"`). Also: what about whitespace-only differences? Template `"  {query}"` vs `"{query}"` produce different embeddings but for meaningful reasons (instruction-tuned models care about whitespace). But what about None vs `""`? Neither should trigger a rebuild? The AC doesn't test this boundary. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **Template double-substitution**: If query text contains literal `{query}`, a naive `str::replace` would substitute it again, producing garbled output. | Low | Medium | Use single-pass replacement with no re-scanning. Document that template placeholders are reserved tokens. |
| F2 | **Broken config for fastembed users**: If someone accidentally configures a prompt template for fastembed/all-MiniLM-L6-v2 (which shouldn't have one), they silently get worse results with no warning. | Medium | Medium | Add a validation/warning heuristic: if the embedding model is a known non-instruction-tuned model and templates are set, emit a startup warning. |
| F3 | **Trait design that doesn't compose**: If embed() is kept as a default that delegates to embed_query, but embed_query itself uses a default that delegates to embed(), you get infinite recursion at runtime with no compile-time error. | Low | Critical | Ensure the default implementations form a DAG with no cycles. Test with a concrete backend that uses only defaults. |
| F4 | **Empty template ambiguity**: Is `""` treated as unset (same as None) or as an empty prefix? Different behaviors produce different fingerprints and different results. | Medium | Medium | The bead should normalize empty/whitespace-only templates to None at deserialization time, not at query time. |
| F5 | **Unicode/whitespace in templates**: Template strings with non-ASCII whitespace, BOM characters, or zero-width spaces could produce subtly different fingerprints and embeddings. | Low | Low | Acceptable — fingerprint hash catches intentional differences. But the bead's spec should note that BOM/encoding issues could cause surprise rebuilds. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| Existing configs deserialize without new fields | ✅ Clear | Serde default handles this |
| Default config produces raw embeddings | ✅ Clear | No templates = pass through |
| query_prompt_template transforms query | ✅ Clear | Template applied before embed_query |
| document_prompt_template transforms chunks | ✅ Clear | Template applied before embed_documents |
| document_prompt_template → fingerprint change | ✅ Clear | Hash included in fingerprint |
| query_prompt_template → no fingerprint change | ✅ Clear | Only tracked in diagnostics |
| All three backends support templates | ✅ Clear | Trait split applies to all impls |
| Existing tests pass unchanged | ⚠️ **See risk** | Trait refactor may touch test fixtures |
| New tests cover ACs | ✅ Clear | Test bead exists separately |
| cargo build + clippy pass | ✅ Clear | Standard validation |

### Missing or Under-specified Items

1. **Template validation timing**: The bead mentions "Validate or fall back gracefully" for unknown placeholders in error handling but doesn't specify *when* validation happens (config load time vs. first query). Config load time is better for user experience.
2. **Multi-placeholder templates**: The spec says "template must contain exactly one recognized placeholder." What if a template has both `{query}` and `{text}`? Error? Use the appropriate one based on context? This should be explicitly decided.
3. **Template charset/encoding**: No mention of UTF-8 normalization for template comparison or hashing. NFC vs NFD differences could cause different fingerprints for semantically identical templates.

### Scope Correctness

**In scope**: All appropriate items covered. The split into separate beads for reranking/diagnostics/TUI/docs/tests is clean.

**Out of scope**: Missing one potential item — **template validation at config parse time** could reasonably live here or in the test bead (aft-t6p.6). The test bead covers template validation in tests, but production-level validation (config parse error on missing placeholder) is only implied, not explicitly in scope.

---

## 5. Staging Assessment

The bead is positioned as Feature 1 in the implementation sequence. This ordering is correct:
- Prompt templates are a prerequisite for reranking (Feature 2) because the reranker prompt needs to apply templates.
- Metrics (Feature 3) can be implemented independently but naturally follows.
- TUI (Story 4) depends on metrics (Feature 3) being available.
- Docs (Task 5) and Tests (Task 6) are naturally last.

**Dependency check**: The epic's parent-child dependencies are shown correctly. No blocking dependencies between child beads are declared (parent-child is containment only). This is appropriate since none of the features strictly block each other — they can be implemented in parallel with some coordination.

---

## 6. Overall Assessment

**Comprehensiveness**: 8/10 — Well-structured with clear sections, scope boundaries, and acceptance criteria.

**Completeness**: 7/10 — Missing explicit decisions on: template validation timing (config load vs. first query), empty template normalization strategy, and how existing test trait impls survive the refactor.

**Coherence**: 9/10 — Internally consistent and fits cleanly into the epic's phased approach.

**Scoping**: 8/10 — Slightly larger than ideal because the trait refactor and backward compat strategy aren't fully pinned down. The actual implementation may reveal complications that should have been surfaced in the design.

**Edge cases**: 7/10 — Covers template errors and fingerprint edge cases. Missing: empty/whitespace normalization, trait recursion guard, fastembed accidental template warnings.

**Key recommendations**:
1. Resolve the trait refactor strategy *before* implementation: keep `embed()` as a default method with `embed_query()`/`embed_documents()` delegating to it (or vice versa).
2. Add an AC for empty/whitespace-only template normalization.
3. Add a startup warning when templates are configured for known non-instruction-tuned models.
4. Clarify template validation timing (parse-time preferred).
5. Specify behavior when template contains both `{query}` and `{text}` (error vs context-sensitive).
