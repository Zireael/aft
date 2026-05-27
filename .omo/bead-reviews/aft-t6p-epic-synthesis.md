# Epic Review Synthesis: aft-t6p — Semantic search upgrade

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Reports**: See `.omo/bead-reviews/aft-t6p.{1-6}-*.md`

---

## Epic Overview

6 beads covering:
| # | Bead | Type | Priority | Score |
|---|------|------|----------|-------|
| 1 | Embedding prompt-template support | Feature | P1 | 8/10 |
| 2 | OpenAI-compatible reranking pipeline | Feature | P1 | 8/10 |
| 3 | Search pipeline metrics and diagnostics | Feature | P1 | 8/10 |
| 4 | TUI/status integration | Story | P2 | 7/10 |
| 5 | Config documentation and examples | Task | P2 | 9/10 |
| 6 | Test suite for semantic search upgrade | Task | P1 | 9/10 |

---

## 1. Comprehensiveness

**Overall: 8/10 — The epic covers all major capability areas.** The feature beads (1-3) address the three core gaps: prompt templates for instruction-tuned models, a reranking pipeline for result quality, and metrics/diagnostics for observability. The supporting beads (4-6) cover the integration, documentation, and validation surfaces.

**What's well covered:**
- Config parsing and backward compatibility for every new feature
- Error handling across all pipeline stages (timeouts, failures, fallbacks)
- JSON parsing edge cases for reranker responses (multiple formats, missing/unknown IDs)
- Query privacy (hash-only logging, no code snippets)
- Security boundaries (SSRF validation, API key protection)
- Three distinct user personas (fastembed default, OASIS-only, OASIS+CodeRankLLM)

**Gaps identified:**
- No bead for **prompt template injection/misuse** as a security concern (malicious queries injecting into prompt templates)
- No bead for **performance benchmarking** — the epic assumes performance is acceptable without measurement
- No bead for **migration/migration script** — if the index format changes, users need a migration path

---

## 2. Completeness

**Overall: 7/10 — Beads are well-structured but have several specific omissions.**

| Aspect | Verdict |
|--------|---------|
| Acceptance criteria | ✅ Mostly strong. Feature 2 (strict mode) and bead 4 (expandable TUI) have open questions. |
| Error handling | ✅ Well-covered across all beads. Timeouts, failures, and parse errors have defined behavior. |
| Edge cases | ⚠️ Medium. Good coverage of JSON parsing. Missing: template edge cases (empty, whitespace, special chars), stale index diagnostics, concurrent metrics. |
| Implementation plans | ✅ All beads have step-by-step plans with code exploration steps. |
| Spec references | ✅ All refer to a single spec document (`docs/semantic-search-upgrade-20260524.md`) — clean traceability. |
| Interface contracts | ⚠️ Missing. No bead documents the cross-bead interface (e.g., what struct fields Feature 3 exposes for bead 4 to consume). |

**Cross-cutting omissions:**
1. **No interface contract document**: Beads 3 → 4 (metrics → TUI) and 1 → 2 (templates → reranking) share data interfaces. These interfaces aren't defined anywhere — risk of integration friction.
2. **No bead for regression testing**: The spec says "existing tests pass" but there's no explicit regression smoke test beyond `cargo test`.
3. **No performance baseline or benchmarks** — a common omission but relevant for a feature that adds latency (reranking) and memory overhead (metrics).

---

## 3. Coherence

**Overall: 9/10 — Highly coherent epic with clean internal structure.**

- **Config pattern consistency**: All beads follow the same `#[serde(default)]` / optional-field pattern for backward compatibility.
- **Pipeline integration**: The beads describe modifications to the same search pipeline in a non-overlapping way — Feature 1 changes the embedding trait, Feature 2 adds a reranking stage, Feature 3 adds instrumentation.
- **Terminology consistency**: Same terms used across all beads ("fingerprint," "fallback," "SSRF validation," "diagnostics").
- **Error handling philosophy**: Consistent non-fatal error model — failures degrade gracefully rather than breaking the search.

**Minor coherence issues:**
- Bead 4's "expandable if the TUI supports it" clause creates a forked acceptance path that's inconsistent with the deterministic ACs of other beads.
- Feature 3 mentions "reranking instrumentation" but this depends on Feature 2's pipeline integration point, which isn't stable yet.

---

## 4. Appropriate Staging

**Overall: 8/10 — Good ordering with one structural concern.**

**Current order:**
1. Prompt templates (Feature)
2. Reranking pipeline (Feature)
3. Metrics/diagnostics (Feature)
4. TUI integration (Story)
5. Config documentation (Task)
6. Test suite (Task)

**Assessment:**
- 1 → 2 → 3 is the right implementation sequence. Templates enable better embedding before reranking improves results. Metrics naturally follow both.
- 4 (TUI) correctly comes after 3 (metrics) since the TUI consumes metrics data.
- 5 (docs) and 6 (tests) are appropriately last.

**Concern**: Bead 4's first two implementation steps are *investigation* (find TUI component, understand rendering pattern). This means the bead has unknown scope. If the TUI framework doesn't support dynamic panels, bead 4's approach needs fundamental rethinking. **Recommendation**: Move TUI framework discovery to a pre-condition or separate spike before bead 4 is started.

**Dependency concern**: No bead has a blocking dependency declared — all use parent-child containment only. For beads 5 (docs) and 6 (tests), blocking dependencies on Features 1-3 would prevent writing docs/tests against an outdated spec.

---

## 5. Appropriate Scoping

**Overall: 8/10 — Beads are generally well-sized with clear boundaries.**

| Bead | Scope Assessment |
|------|-----------------|
| Feature 1 | ✅ Good. The trait split is the riskiest part — resolves cleanly if the design is pinned down. |
| Feature 2 | ✅ Good. Well-bounded with clear out-of-scope items. |
| Feature 3 | ✅ Good. Metrics scope is contained. |
| Story 4 | ⚠️ Risky — unknown TUI framework capabilities could expand scope mid-implementation. |
| Task 5 | ✅ Excellent. Tightly bounded documentation scope. |
| Task 6 | ⚠️ Slightly optimistic — mock HTTP infrastructure discovery is an unstated dependency. |

**Cross-bead scope concerns:**
- The reranking **prompt** (Feature 2) and embedding **templates** (Feature 1) use different mechanisms. Feature 2's reranking prompt is hardcoded, while Feature 1's templates are configurable. If users want to customize the reranking prompt in the future, Feature 2 would need a template mechanism too — this is Future Work but worth noting.
- The **metrics struct** (Feature 3) and **TUI display** (Bead 4) have a producer-consumer relationship that's not explicitly defined. Scope drift in one affects the other.

---

## 6. Happy Paths and Edge Cases

**Happy paths: ✅ Well-covered.** Each bead has explicit, testable acceptance criteria for the happy path (config loads, templates apply, reranking reorders, metrics get collected, UI shows status).

**Edge cases: ⚠️ Medium completeness.**

| Edge case | Covered by |
|-----------|-----------|
| Reranker disabled → original behavior | 2, 6 |
| JSON parse failure → fallback | 2, 6 |
| Missing/unknown IDs in reranker response | 2, 6 |
| Timeout → fallback | 2, 6 |
| Zero results → warning | 3, 6 |
| No semantic search config → clean UI | 4 |
| Empty template string → treat as unset | 1 |
| Unicode/whitespace in templates | 1 (partial) |
| Double-substitution of placeholders | Not covered |
| Prompt injection in reranker candidates | Not covered |
| Stale index warning | 3 (mentioned), 6 (not tested) |
| Concurrent metrics access | Not covered |
| Metrics memory growth | Not covered |
| Non-expandable TUI fallback | 4 (ambiguous) |

---

## Cross-Cutting Findings

### Across all beads

| Issue | Severity | Applies to |
|-------|----------|------------|
| **Interface contracts undefined** | Medium | Beads 1↔2, 3↔4 |
| **No blocking dependencies** | Low | All beads |
| **Spec/fingerprint drift risk** | Medium | Beads 5, 6 (docs/tests written against changing spec) |
| **No performance/benchmarking scope** | Low | Epic-level |

### Per-bead key issues

| Bead | Top Issue |
|------|-----------|
| 1 | Trait refactor strategy undefined — "default that calls embed" is ambiguous |
| 2 | Strict mode undefined — mentioned but never specified |
| 3 | Thread safety and rolling window size unspecified |
| 4 | TUI expandability is an unverified assumption — changes the entire approach |
| 5 | Rustdoc and CHANGELOG updates not scoped |
| 6 | Stale index diagnostic test missing; mock HTTP infra is an unknown |

---

## Recommendations Summary

1. **🔴 Define trait refactor strategy** (Bead 1): Resolve whether `embed()` stays as a default with `embed_query`/`embed_documents` delegating to it, or the reverse.
2. **🔴 Define strict mode** (Bead 2): What happens when reranker fails in strict mode — does the search fail or return an error?
3. **🔴 Verify TUI framework capabilities** (Bead 4): Before starting work, confirm whether the TUI supports dynamic/expandable panels.
4. **🟡 Add interface contracts**: Define the struct fields that Feature 3 exposes for Bead 4, and the pipeline integration point that Feature 2 provides for Feature 3.
5. **🟡 Add blocking dependencies**: Beads 5 and 6 should block on Features 1-3 to prevent docs/tests drift.
6. **🟡 Add stale index diagnostic test** (Bead 6): Feature 3's AC mentions it, test bead should cover it.
7. **🟡 Add template edge case tests** (Beads 1, 6): Empty templates, whitespace, special chars, both placeholders.
8. **🟢 Clarify metrics thread safety** (Bead 3): Even if single-threaded today, document the model.
9. **🟢 Add Rustdoc and CHANGELOG to docs bead** (Bead 5).
10. **🟢 Investigate mock HTTP infrastructure** as a pre-condition (Bead 6).

**Legend**: 🔴 Must-fix before implementation | 🟡 Should-fix | 🟢 Nice-to-have
