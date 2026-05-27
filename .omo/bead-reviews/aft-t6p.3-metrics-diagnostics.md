# Bead Review: aft-t6p.3 — Search pipeline metrics and response diagnostics

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ⚠️ Issues found

---

## 1. Steelmanned Thesis

Add lightweight per-query and aggregate metrics collection around AFT's semantic search pipeline. Define `SearchDiagnostics` and `SearchMetrics` structs. Instrument each pipeline stage (embedding, lexical search, semantic retrieval, fusion, reranking) with timing and candidate-count collection. Add an optional `diagnostics` metadata field to the `aft_search` JSON response and a compact one-line human-readable footer. Implement rolling aggregate statistics (p50/p95/p99 latency). Add configurable warning thresholds for poor retrieval quality. Ensure query privacy: never log raw query text or code snippets by default — only hash query strings for metrics.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | Rolling aggregates can use a simple in-memory ring buffer. | A ring buffer of the last N queries works for p50/p95/p99 if N is large enough (≥100 for stable p99). But what happens across config changes or pipeline restarts? The aggregate resets. This is acceptable for MVP but should be documented. |
| A2 | Query text privacy: hashing is sufficient. | Hash of query text prevents reading the original query from logs, but if the query space is small (e.g., known code-search queries from a specific agent), hash-based identification via rainbow tables could de-anonymize. Acceptable for the threat model described, but the bead should note this is privacy *obscuring*, not privacy *protecting*. |
| A3 | Diagnostics output is additive and non-breaking. | Adding a `diagnostics` field to the `aft_search` response is additive for JSON consumers. But for the human-readable output, adding a footer line changes the output format that agents may parse. The bead should test that existing human-readable parsers (if any) still work. |
| A4 | Warning thresholds don't need "noise floor" tuning. | Zero results always triggers a warning. But what about sporadic zero-result queries in a healthy system (e.g., genuinely no relevant code for a very specific query)? The warning could generate constant noise. A deadband/rate-limit on warnings might be needed. |
| A5 | Pipeline stage latencies are independent and summable. | If stages run sequentially, total latency = sum of stage latencies. But if the pipeline has branching or parallelism (e.g., hybrid search runs lexical + semantic in parallel), stage latencies overlap. The bead should define whether it measures wall-clock or per-stage CPU time. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **Metrics memory leak**: If `SearchMetrics` accumulates per-model or per-config data without cleanup, memory grows unbounded over long-running sessions. | Low-Medium | Medium | Use fixed-size ring buffers or capped data structures. Document the retention policy. |
| F2 | **Diagnostics information disclosure**: The diagnostics object might include paths or model names that the user considers sensitive (e.g., internal server names, proprietary model identifiers). | Low | Medium | Diagnostics should include only what's documented and intentional. Peer review should verify no accidental exposure. |
| F3 | **Latency perturbation from instrumentation**: Timing measurements themselves add overhead (memory allocation for timestamps, atomic counters). In hot paths, observable overhead. | Low | Low | Use coarse timestamps (std::time::Instant) not high-frequency perf counters. Accept sub-millisecond overhead. |
| F4 | **Warning threshold mismatch with reality**: Default thresholds are too sensitive (false positives) or too lenient (miss real problems). Users can't find or configure them. | Medium | Medium | The config flag approach (`semantic_diagnostics: bool`) doesn't define threshold sensitivity. Add explicit threshold config fields or document that defaults are conservative. |
| F5 | **Concurrent access to metrics**: If the semantic search pipeline can be called concurrently (multiple queries in flight), the metrics struct needs thread-safe updates. | Low-Medium | Medium | The bead doesn't mention thread safety for aggregate metrics. Use atomic counters or a Mutex-protected ring buffer. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| aft_search includes diagnostics object | ✅ Clear | Optional, additive |
| Human-readable footer with key metrics | ✅ Clear | Compact one-line format |
| Per-query latency breakdowns per stage | ✅ Clear | Each pipeline stage instrumented |
| Score distribution computed | ✅ Clear | min/median/max/mean |
| Candidate counts per stage | ✅ Clear | Pipeline stage tracking |
| Rolling p50/p95/p99 latency | ✅ Clear | Aggregate history |
| Warning thresholds → diagnostics | ✅ Clear | Zero results, low scores, stale index |
| Warnings say "pipeline misconfigured" not "model bad" | ✅ Clear | Actionable messaging |
| Query text never logged; hash only | ✅ Clear | Privacy-by-design |
| Existing response format unbroken | ✅ Clear | Additive field only |
| All existing tests pass | ✅ Clear | Non-regression |

### Missing or Under-specified Items

1. **Thread safety not addressed**: The bead doesn't specify whether metrics collection must be thread-safe. AFT's request loop is single-threaded today (per ARCHITECTURE.md), but if that changes, metrics will race.
2. **Warning deadband/rate-limiting**: "Zero-result diagnostics emission" as an AC means *every* zero-result query emits a diagnostic. On a frequently empty corpus, this is noise. A rate-limit or hysteresis should be considered.
3. **Rolling aggregate window size**: The bead says "ring buffer or similar" for aggregates — but the window size affects p99 stability. A window that's too small (N=10) produces unstable p99. Should be a configurable parameter with a sensible default.
4. **Diagnostics response format not specified**: The scope says "Define SearchDiagnostics struct with all fields from spec §4" but the bead doesn't include those struct fields inline. A reader of this bead alone can't know what the diagnostics object contains — they need to cross-reference the spec document. This reduces the bead's self-containedness.
5. **Metrics reset on config change**: The bead mentions "reset or decay on config change" in error handling but doesn't define which config changes trigger reset (all config changes? only semantic config changes?).

### Scope Correctness

**In scope**: Appropriate set of instrumentation, diagnostics, aggregates, and warnings.

**Out of scope**: Clean. Persistent metrics storage is correctly deferred — MVP uses in-memory only.

---

## 5. Staging Assessment

Properly placed as Feature 3. Requires:
- Feature 2 (reranking) to be instrumented, or at least the reranking integration point to exist. The bead says "covered by Feature 2 + this bead's integration."
- Does NOT depend on Feature 1 (prompt templates) except that the pipeline code path exists.
- Provides the data source for Story 4 (TUI integration).

**Staging concern**: The bead claims reranking instrumentation is "covered by Feature 2 + this bead's integration." If Feature 2 restructures how reranking fits into the pipeline, Feature 3's instrumentation points may need to shift. A shared pipeline interface contract would reduce this risk.

---

## 6. Overall Assessment

**Comprehensiveness**: 8/10 — Strong coverage of what to instrument and how to expose it. Privacy handling is well-considered.

**Completeness**: 6/10 — Missing: thread safety model, rolling aggregate window size, diagnostics response schema (cross-refs spec instead of inlining), and warning deadband/rate-limiting.

**Coherence**: 8/10 — Good internal consistency. The diagnostics response gating model makes sense.

**Scoping**: 9/10 — Cleanly bounded. Persistent storage and alerting properly deferred.

**Edge cases**: 7/10 — Covers zero results and empty lists. Missing: concurrent query metrics safety, overlapping stage latency measurement model.

**Key recommendations**:
1. **Specify the rolling aggregate window size** as a configurable parameter with a sensible default (≥100 for stable p99).
2. **Document thread safety model** — even if single-threaded now, design for atomic or guarded access.
3. **Add warning deadband/rate-limiting** to avoid noisy repeated warnings for the same condition.
4. **Inline the diagnostics schema fields** in the bead description, or at minimum link the exact spec line. A bead reviewer shouldn't need to open the spec doc to evaluate completeness.
5. **Define config-change → metrics-reset behavior** explicitly for each metric type.
