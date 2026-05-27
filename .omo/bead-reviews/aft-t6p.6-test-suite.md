# Bead Review: aft-t6p.6 — Test suite for semantic search upgrade

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ✅ Minor observations

---

## 1. Steelmanned Thesis

Add comprehensive unit and integration tests covering all new functionality from the semantic search upgrade: config parsing, prompt template application, fingerprint changes, reranker JSON parsing (bare arrays, markdown-fenced, unknown IDs, missing IDs), reranker fallback behavior (timeout, HTTP error, parse failure), metrics calculation (min/median/max/mean), and zero-result/low-score diagnostics emission. Integration tests use mocked HTTP servers for embedding and reranker interaction. All existing tests must continue to pass.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | Mocked HTTP servers (wiremock or similar) exist in the project's test infrastructure. | The bead says "using wiremock or similar" — this implies the author doesn't know what HTTP test infrastructure AFT already has. Discovering or building test HTTP infrastructure could be significant work. The bead should first investigate what exists. |
| A2 | Unit tests are sufficient for all non-HTTP functionality. | Metrics calculation, config parsing, and template application are pure functions — perfect for unit tests. But fingerprint computation may involve hashing with external dependencies. Is the hash function injected or hardcoded? If hardcoded, unit tests are fine. If using a hash from an external crate, minimal concern. |
| A3 | Integration tests with mocked servers provide sufficient coverage. | Mocked servers verify that the client sends the right request format and handles the right response format. They don't verify actual network behavior (timeouts, connection errors, DNS failures, TLS issues). The bead should call this out as a known limitation. |
| A4 | All 19 acceptance criteria can be written as deterministic tests. | Some diagnostics behavior (low-score emissions, warning thresholds) depends on configurable threshold values. Tests must use explicit known-good thresholds. If thresholds are externalized (config file), tests need config overrides. This is manageable but should be defined. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **Mock HTTP server doesn't simulate real failure modes**: A simple wiremock stub returns a canned 500 error, but real failures include: slow responses, connection resets, TLS errors, chunked encoding issues, and DNS failures. Tests that only use stubs may pass while real-world error handling is broken. | Medium | Medium | Add at least one integration test per failure mode category using appropriate mock patterns (slow response → delay injector, connection reset → close socket, etc.). |
| F2 | **Fingerprint test brittleness**: The test asserts that a document_prompt_template change alters the fingerprint. If the fingerprint includes a hash that depends on serialization order (e.g., a JSON map), the hash may differ across Rust versions or serde versions, causing a non-deterministic test. | Low | Medium | Use deterministic serialization (e.g., BTreeMap for config fields) and pin the hash function version in tests. |
| F3 | **Metrics calculation overflow**: min/median/max/mean calculation on large candidate lists with extreme score values could overflow or lose precision. The test should include edge cases (very large scores, NaN, negative scores if applicable). | Low | Low | Add boundary-value tests for metrics calculation. |
| F4 | **Integration test flakiness from port conflicts**: If multiple tests spin up mock HTTP servers on the same port, parallel test execution causes random failures. | Medium | Medium | Use port 0 (OS-assigned) for mock servers, or use a sequential test mode for integration tests. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| Config parsing: missing rerank block | ✅ Clear | Negative test |
| Config parsing: rerank block present | ✅ Clear | Positive test |
| Query prompt template application | ✅ Clear | Pure function test |
| Document prompt template application | ✅ Clear | Pure function test |
| Template validation: unknown placeholders | ✅ Clear | Error handling test |
| Fingerprint: document prompt change → changes | ✅ Clear | Regression prevention |
| Fingerprint: only query prompt → no change | ✅ Clear | Differential test |
| Reranker JSON: bare array parsed | ✅ Clear | Format 1 |
| Reranker JSON: markdown-fenced parsed | ✅ Clear | Format 2 |
| Reranker JSON: unknown IDs ignored | ✅ Clear | Robustness test |
| Reranker JSON: missing IDs appended | ✅ Clear | Robustness test |
| Reranker fallback: error → original ordering | ✅ Clear | Resilience test |
| Metrics: min/median/max/mean | ✅ Clear | Core math test |
| Diagnostics: zero results → warning | ✅ Clear | Threshold test |
| Diagnostics: low score → warning | ✅ Clear | Threshold test |
| Integration: embedding receives prompted query | ✅ Clear | HTTP verification |
| Integration: embedding receives prompted docs | ✅ Clear | HTTP verification |
| Integration: reranker reorders candidates | ✅ Clear | HTTP verification |
| Integration: reranker failure → original order | ✅ Clear | Failover verification |
| All existing tests pass | ✅ Clear | Non-regression |

### Missing or Under-specified Items

1. **No test for stale index diagnostics**: The acceptance criteria for Feature 3 says "Warning thresholds emit diagnostics for ... stale index." But this bead's test list doesn't include a test for stale index warning emission.
2. **No test for concurrent/sequential safety**: If there are thread-safety concerns in metrics (from bead 3 review), the test bead should include concurrent access tests.
3. **No test for config backward compatibility**: The test bead tests that "missing rerank block" parses correctly — but doesn't test that a config file from before the upgrade (no semantic-search section at all) still works. The most critical backward-compat case is the *complete absence* of any new config.
4. **No explicit test for edge cases in template application**: Tests cover "unknown placeholders handled gracefully" but don't test: empty template string, template with only whitespace, template with both `{query}` and `{text}`, template with special characters (newlines, unicode).
5. **No guidance on mock HTTP server pattern**: The bead says "using wiremock or similar" but doesn't specify whether the project already has a mock HTTP pattern. If not, this is significant setup work that's not scoped.

### Scope Correctness

**In scope**: Thorough and comprehensive. Every feature bead's functionality is represented.

**Out of scope**: Reasonable — no E2E tests with real endpoints, no performance tests, no benchmarks.

---

## 5. Staging Assessment

Placed last (6th). This is correct — tests should come after or in parallel with implementation. The bead doesn't have any blocking dependencies declared beyond the epic parent, which is fine — tests are naturally last.

**Staging note**: The test bead could productively run *in parallel* with Features 1-3 once the module interfaces are defined. Test-driven development would have the tests *before* the implementation, but the bead is structured as a test-suite task rather than TDD. This is a stylistic choice, not a flaw.

---

## 6. Overall Assessment

**Comprehensiveness**: 9/10 — 19 acceptance criteria covering all major functionality areas. The test layer split (unit vs integration) is clear.

**Completeness**: 7/10 — Missing: stale index diagnostic test, backward-compat test for pre-upgrade configs, edge cases in template application, and concurrent metrics test. The test bead references "stale index" from Feature 3's ACs but doesn't test it.

**Coherence**: 10/10 — Perfectly coherent with the feature beads. Each test maps clearly to a feature AC.

**Scoping**: 9/10 — Well-bounded. Mock server discovery (if the project lacks one) is the only hidden scope risk.

**Edge cases**: 7/10 — Good coverage of reranker JSON parsing edge cases. Template edge cases (empty, whitespace, multiple placeholders) and stale index diagnostics could be added.

**Key recommendations**:
1. **Add a stale index diagnostics test** — Feature 3 includes this in its ACs but the test bead doesn't cover it.
2. **Add backward-compat test** — test that a completely pre-upgrade config file (with no semantic-search section whatsoever) parses correctly.
3. **Add template edge-case tests** — empty string, whitespace-only, special characters, both placeholders in single template.
4. **Add a concurrency test for metrics** if Feature 3 is designed for thread-safe metrics collection.
5. **Investigate existing mock HTTP infrastructure** as a pre-condition — if the project doesn't have wiremock or an equivalent, this bead's scope expands significantly.
