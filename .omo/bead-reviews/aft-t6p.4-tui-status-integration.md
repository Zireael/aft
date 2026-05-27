# Bead Review: aft-t6p.4 — TUI/status integration for semantic search diagnostics

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ⚠️ Issues found

---

## 1. Steelmanned Thesis

Extend AFT's TUI/status panel to show semantic search pipeline health: index status (ready/building/empty/stale/unavailable), embedding backend name and model, index entry count, last query latency, last query score distribution (min/median/max), rerank status (enabled/disabled, model name, latency), and low-confidence warnings. Display as a compact one-line summary by default with expandable details if the TUI supports it. Hide entirely when semantic search is not configured.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | A TUI/status component exists and is easy to extend. | The bead says "Find existing TUI/status component" as a first implementation step — meaning the author doesn't know the existing structure. This is a discovery bead masquerading as a story. The implementation plan's first two steps ("Locate TUI/status component" and "Understand its rendering pattern") are investigation, not implementation. This should be a pre-condition, not part of the work. |
| A2 | The TUI supports expandable details. | "One-line summary by default; expandable details if the TUI supports it" — if the TUI doesn't support expansion, all the details must fit in one line (which would be unreadable) or it's always expanded (which violates "avoid noisy UI"). The acceptance criteria should determine what happens in the non-expandable case. |
| A3 | Metrics from Feature 3 will be available with the right shape. | The bead "depends on Feature 3 (metrics/diagnostics) being implemented." If Feature 3's SearchMetrics/SearchDiagnostics structs don't expose exactly the fields the TUI needs, the TUI bead has to transform them — or Feature 3 has to be extended. This interface dependency should be explicitly documented (what struct fields the TUI reads). |
| A4 | The one-line summary can meaningfully capture all states. | "ready, backend/model, chunk count, last query latency" in one line could be dense. For example: "Semantic: ready | OASIS-code-embedding | 12,345 chunks | last: 142ms". That's arguably two lines worth of info compressed into one. The "one line" constraint may force cryptic abbreviations. |
| A5 | "No semantic search panel shown" is the right empty state. | When semantic search is not configured, no panel is shown. That's clean. But what about the transition state — when the user *just* configured semantic search and the TUI hasn't picked it up yet? Is there a brief flash of missing-then-appearing panel? Should be handled in the TUI update cycle. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **TUI framework doesn't support dynamic content**: The TUI library AFT uses may not support conditionally rendering panels based on runtime config changes. If the TUI is static (built at startup), adding a semantic search panel requires a restart. | Medium | High | The implementation plan should include an investigation step to determine how dynamic the TUI is. If it's static, the bead must be restructured. |
| F2 | **Refresh race**: The TUI polls metrics at some interval. If a query completes between poll ticks, the "last query" metrics shown are stale or from a different query. | Low | Low | Acceptable — "last query" means "last observed query at poll time." Document this latency. |
| F3 | **Long model names break layout**: "openai_compatible/oasis-code-embedding-v2.1" could exceed the status line width, wrapping or truncating ugly. | Medium | Medium | The bead should include a truncation/ellipsis strategy for long names. |
| F4 | **Panel flickers during index rebuild**: When the index is rebuilding, status transitions through multiple states. If the TUI updates at a high rate, the user sees a rapid flickering of "indexing" ↔ "ready". | Low | Medium | Debounce the status display — show a stable state and only update when the state has been stable for >N ms. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| Status line visible when configured | ✅ Clear | Core functionality |
| Index status displayed correctly | ✅ Clear | 5 states defined |
| Embedding backend + model shown | ✅ Clear | Backend and model name |
| Index entry count displayed | ✅ Clear | Numeric count |
| Last query latency shown | ✅ Clear | On next query |
| Score min/median/max shown | ✅ Clear | On next query |
| Rerank status shown | ✅ Clear | Enabled/disabled |
| Reranker model shown when enabled | ✅ Clear | Model name |
| Rerank latency shown | ✅ Clear | When applicable |
| Fallback message on reranker failure | ✅ Clear | "rerank failed, fallback used" |
| Low-confidence warning | ✅ Clear | Warning indicator |
| No panel when not configured | ✅ Clear | Clean empty state |
| One-line + expandable if supported | ⚠️ **Under-specified** | What if TUI doesn't support expand? |

### Missing or Under-specified Items

1. **Expandable details — the "if" problem**: The most critical issue. "Expandable details if the TUI supports it" means the acceptance criteria split into two mutually exclusive paths. If the TUI doesn't support expandability, the entire detailed view must fit in one line — which contradicts the "show all these fields" requirement. The bead needs to commit to one approach or design for both.
2. **No polling/update mechanism defined**: How does the TUI refresh? On timer? On pipeline event? On manual trigger? The bead doesn't specify how new diagnostics data reaches the TUI.
3. **No layout or wireframe**: For a visual change, the acceptance criteria are purely textual. A rough layout sketch or wireframe would catch layout issues before implementation.
4. **Long name truncation strategy**: Model names, backend names, and status strings can vary in length. No truncation/ellipsis strategy is defined.
5. **Color/styling**: The bead doesn't mention color coding for status (green=ready, yellow=building, red=unavailable) or warning indicators. Not required but would improve UX.

### Scope Correctness

**In scope**: Well-defined list of status fields.

**Out of scope**: Clean — no redesign, no non-semantic changes, no persistent storage.

---

## 5. Staging Assessment

Placed as Story 4 — after the metrics/diagnostics feature. This is correct because the TUI consumes metrics data.

**Staging concern (repeat)**: The bead's first two implementation steps are investigation ("Locate TUI/status component," "Understand its rendering pattern"). This is a discovery activity that should be a pre-condition. If the TUI framework is unsuitable for dynamic panels, the entire bead's approach needs to change. The bead should either:
- (a) Include TUI framework investigation as a pre-implementation discovery phase, OR
- (b) Be restructured as a spike first, then a story.

---

## 6. Overall Assessment

**Comprehensiveness**: 7/10 — Good coverage of what status fields to display. Could use more detail on the TUI interaction model.

**Completeness**: 5/10 — The "expandable if supported" clause is an open existential question for the bead. The polling/update mechanism and data flow from Feature 3 to TUI are unspecified.

**Coherence**: 8/10 — Internally consistent. All status fields serve the diagnostic purpose stated.

**Scoping**: 7/10 — The discovery-vs-implementation ambiguity (first two steps are investigation) suggests this bead's scope includes unknown unknowns about the TUI framework.

**Edge cases**: 6/10 — Covers the major states. Missing: TUI refresh timing, layout overflow for long names, and the non-expandable TUI fallback.

**Key recommendations**:
1. **Determine TUI expandability BEFORE accepting this bead**: Create a pre-condition or spike to verify whether the TUI supports expandable detail panels. Without this, the acceptance criteria cannot be written definitively.
2. **Define the polling/update mechanism**: How does diagnostics data flow from SearchMetrics to the TUI display? Event-driven? Timer-based? Each approach has different complexity.
3. **Add a truncation strategy** for long model/backend names.
4. **Consider a simple wireframe** of the one-line summary and expanded detail view to validate layout before coding.
5. **Define color/styling convention** for status states if the TUI supports colors.
