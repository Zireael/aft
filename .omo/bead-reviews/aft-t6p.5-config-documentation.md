# Bead Review: aft-t6p.5 — Config documentation and examples

**Reviewed by**: Hephaestus (the-fool + ce-code-review lenses)
**Date**: 2026-05-24
**Status**: ✅ Minor observations

---

## 1. Steelmanned Thesis

Update AFT's README (and any other config doc files) to document the new prompt template fields, reranking configuration, security boundaries (SSRF policy, no API keys in logs), performance implications, fingerprint rebuild triggers, and metrics interpretation. Provide three complete example configs: (A) default fastembed setup (no templates, no reranking), (B) OASIS-only with prompt templates, (C) OASIS + CodeRankLLM reranker.

---

## 2. the-fool: Questioned Assumptions

| # | Assumption | Challenge |
|---|-----------|-----------|
| A1 | README is the right and only documentation surface. | The bead says "any other config doc files in the repository" as a secondary target. If the project has a wiki, a docs/ directory, or inline Rust doc comments on config structs, updating only README leaves gaps. For a crate like AFT, the config structs likely have `#[doc]` annotations that generate API docs — those should be updated too. |
| A2 | Three example configs cover all common setups. | The three examples (fastembed, OASIS-only, OASIS+CodeRankLLM) are a reasonable MVP. But there are other configurations: Ollama with reranking, multiple embedding backends, hybrid search configs. Are these covered elsewhere? The bead doesn't say whether the examples are exhaustive or representative. |
| A3 | Users will find and read the updated docs. | Documentation is only useful if discoverable. If the README is long and the new section is buried, users may miss it. The bead should specify where in the README the new content goes (new section? subsection of existing config?). |
| A4 | Performance implications can be concisely documented without actual benchmarks. | "Performance implications of reranking" section needs concrete numbers or at least relative guidance (e.g., "reranking adds ~200-500ms per query window"). Without benchmark data, the section risks being vague. |

---

## 3. the-fool: Failure Modes (Pre-mortem)

| # | Failure | Likelihood | Impact | Mitigation |
|---|---------|-----------|--------|------------|
| F1 | **Documentation drifts from implementation**: If Feature 1 or Feature 2 changes the config shape during implementation, the docs bead may be written against an outdated spec. | Medium | Medium | The docs bead should be updated LAST, after implementation is stable. The staging already has it as 5th, which is correct — but coordination with Features 1-3 is essential. |
| F2 | **Example configs contain secrets or placeholders that look like secrets**: Example C (OASIS+CodeRankLLM) needs a reranker base_url. If the example uses a placeholder like `http://localhost:8080/v1` that's fine, but if it uses `https://api.example.com` it could confuse users about whether they need an API key. | Low | Low | Use clear placeholder patterns (`<your-openai-compatible-endpoint>`, `localhost:8080`). |
| F3 | **Docs describe features that aren't implemented yet**: If the docs bead is completed before all the features, the READM could promise behavior that doesn't work yet. | Medium | Medium | The docs bead should have a hard dependency (blocking) on Features 1-3, not just sequential ordering. |

---

## 4. ce-code-review: Coverage & Completeness

### Acceptance Criteria Completeness

| AC | Verdict | Notes |
|----|---------|-------|
| Documents query_prompt_template/document_prompt_template | ✅ Clear | Required field docs |
| Explains when to configure prompts (when not to) | ✅ Clear | Most models leave unset |
| Documents rerank config block | ✅ Clear | All fields explained |
| Performance implications section | ✅ Clear | General guidance |
| Security boundaries (SSRF, no API keys in logs) | ✅ Clear | Important safety doc |
| Fingerprint/rebuild explanation | ✅ Clear | Index rebuild trigger |
| How to interpret diagnostics/metrics | ✅ Clear | User-facing guidance |
| Three example configs (fastembed, OASIS, OASIS+CodeRankLLM) | ✅ Clear | Concrete examples |
| No unrelated doc changes | ✅ Clear | Scope discipline |

### Missing or Under-specified Items

1. **Rustdoc updates not mentioned**: The config structs in `crates/aft/src/` likely have doc comments that generate API-level documentation. These should be updated alongside the README for consistency.
2. **No section placement guidance**: "Update README config section" is vague — which section? Under what heading? Should it be a new subsection of an existing "Semantic Search" section? A reader needs to know where to look.
3. **No mention of CHANGELOG or migration notes**: If the config shape changes significantly, users migrating from a previous version need a migration guide or CHANGELOG entry.

### Scope Correctness

**In scope**: Appropriately limited to documentation. The three example configs are particularly well-chosen — they cover the most likely upgrade paths.

**Out of scope**: Reasonable. The bead doesn't try to document implementation internals.

---

## 5. Staging Assessment

Placed 5th in the sequence. This is correct — documentation should come after implementation is stable. However, the bead should have a **blocking dependency** on Features 1-3 (prompt templates, reranking, metrics) rather than just parent-child containment. Otherwise a motivated implementer could write docs against a spec that changes during implementation.

---

## 6. Overall Assessment

**Comprehensiveness**: 9/10 — The documentation gap analysis is thorough and well-organized.

**Completeness**: 7/10 — Missing Rustdoc updates, section placement guidance, and CHANGELOG/migration notes. The interaction with inline API documentation (rustdoc on config structs) should be addressed.

**Coherence**: 10/10 — Perfectly coherent. The three example configs are well-thought-out and cover the major use cases.

**Scoping**: 10/10 — Tight and well-bounded. Documentation-only scope is respected.

**Edge cases**: 9/10 — The gaps listed are documentation-writing concerns, not functional gaps. The bead is straightforward.

**Key recommendations**:
1. **Add a blocking dependency** on Features 1-3 (not just ordering) to prevent docs drift.
2. **Specify section placement** in the README (e.g., under "Config → Semantic Search → Advanced").
3. **Include Rustdoc updates** on config struct fields alongside README changes.
4. **Consider a CHANGELOG entry** for the new config fields.
