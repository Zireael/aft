# Bead Instruction Review: aft-t6p.bench.quick

**Target:** `aft-t6p.bench.quick` — Implement targeted quick benchmark mode for AFT search backends
**Review mode:** report-only
**Date:** 2026-06-16
**Reviewer:** Hephaestus

---

## Verdict

**READY WITH REVISIONS**

The graph is well-structured with a clean sequential chain, appropriate milestone/verification closure, and correct deferral of optional future work. The main weaknesses are: (1) missing production reachability/anti-dead-code safeguards on implementation beads, (2) an implicit FTS5 dependency on `.03` that should be explicit, (3) insufficient validation commands on several beads, and (4) a missing integration-test-ownership gap between `.02` and `.07`.

---

## Executive summary

The epic defines a 9-bead sequential chain (`.00`–`.08`) plus one optional future bead (`.repoqa`) and two external future beads (`aft-t6p.bench.agentic`, `aft-t6p.bench.agentic.core`). The dependency graph is acyclic and correctly blocks the milestone on verification. All beads are appropriately typed (decision → feature → task → verification → milestone). Source references are thorough and consistent.

The most significant issues are: implementation beads (`.01`–`.05`) lack explicit anti-dead-code and production reachability safeguards — an implementer could write profile definitions, backend dispatchers, and metric calculators that compile and pass unit tests but are never actually invoked by the benchmark runner. Bead `.03` implicitly depends on FTS5 being implemented (`aft-fts5e2e.12`) but doesn't register this dependency, relying instead on skip behavior. The validation commands across beads are inconsistently thorough — `.00` and `.07` have good commands, but `.04`, `.05`, and `.06` are weak.

---

## Epic / graph review

| Issue | Impact | Recommendation |
|---|---|---|
| `.03` depends on FTS5 benchmark hooks (`aft-fts5e2e.12`) but doesn't register the dependency | If FTS5 isn't implemented, `.03` could be implemented without FTS5 support and then silently skip it, or worse, try to implement its own FTS5 path | Register `aft-fts5e2e.12` as a dependency on `.03`, or explicitly document in `.03` that FTS5 is optional-skip and the dependency is intentional |
| No bead explicitly owns integration testing of profile→runner→backend→metrics→report pipeline | `.07` (verification) can verify, but nobody *writes* the integration tests that `.07` should run | Either add a test-ownership clause to `.02` or `.05`, or split integration tests into a dedicated sub-bead |
| `.repoqa` blocks on `.05` only | `.repoqa` also needs `.01` (corpus definitions) and `.03` (backend matrix) to be meaningful | Add `.01` and `.03` as blocking dependencies for `.repoqa` |
| `.04` references `aft-t6p.33` (token-efficiency), `aft-t6p.35.1` (fixture schema), `aft-t6p.34` (methodology) as source refs but doesn't depend on them | If those beads changed the metric definitions or fixture schema, `.04` could be implemented against stale assumptions | Verify those beads are closed/accepted, or register them as dependencies |

---

## Bead-by-bead review

### aft-t6p.bench.quick.00 — Record benchmark scope decision

| Aspect | Assessment |
|---|---|
| **Verdict** | READY |
| **Type fit** | Decision — correct |
| **Objective** | Clear, concrete: record a durable scope decision |
| **Acceptance criteria** | Good — names all modes, states non-goals, explains rationale |
| **Scope** | Appropriate — docs only, no implementation |
| **Validation** | Good — `git diff --check`, grep for key terms |
| **Issues** | None significant |

### aft-t6p.bench.quick.01 — Corpus/profile definitions

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Feature — correct (adds new named-profile behavior) |
| **Objective** | Clear — add smoke/quick/extended/full profile definitions |
| **Acceptance criteria** | Mostly good, but missing: (a) production entry point — which script/module exposes profiles; (b) proof profiles are wired into the runner, not just defined |
| **Edge cases** | Good — covers missing repos, annotation gaps, Windows paths, `--pilot` compat |
| **Validation** | Decent — lists 3 profile list commands + tests + git diff |
| **Weakness** | No anti-dead-code safeguard: profiles could be defined in a module that the runner never imports |

**Required revisions:**
1. Add acceptance criterion: "Profile definitions are imported and used by the benchmark runner entry point (e.g., `pilot.ts` or `run.ts`). A test or dry-run that selects a profile and resolves repos proves the wiring."
2. Add acceptance criterion: "If all repos in a profile have zero valid annotations, the profile reports this with a clear error or skip — not silent empty results."

### aft-t6p.bench.quick.02 — CLI/profile entrypoints

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Feature — correct (adds new CLI behavior) |
| **Objective** | Clear — expose profiles via CLI with clean skip |
| **Acceptance criteria** | Good — covers profile selection, skip records, invalid config, docs, full-mode isolation |
| **Edge cases** | Good — covers binary missing, FTS5 unavailable, model unavailable, reranker unavailable |
| **Validation** | Good — dry-run commands, unit tests, git diff |
| **Weakness** | (a) No proof the CLI path actually reaches the profile definitions from `.01`; (b) validation doesn't include running an actual smoke benchmark |

**Required revisions:**
1. Add acceptance criterion: "A smoke profile dry-run or actual run exercises the full path: CLI flag → profile resolution → repo selection → query loading → (backend skip or execution) → report output."
2. Add validation command: `bun run benchmarks/semble/run.ts --profile smoke` (actual run, not just dry-run) — or explicitly state why dry-run is sufficient.

### aft-t6p.bench.quick.03 — Backend matrix and rerank candidate-pool sweeps

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Feature — correct (adds new backend dispatch behavior) |
| **Objective** | Excellent — specifies 6 modes and 3 candidate pool values |
| **Acceptance criteria** | Good — covers matrix execution, skip, rerank sweeps, FTS5 reuse, bounded defaults |
| **Edge cases** | Excellent — covers reranker starvation, endpoint unavailability, FTS5 compile-out, wrong dimensionality, duplicates |
| **Validation** | Good — smoke matrix run, dry-run, unit tests |
| **Weakness** | (a) Missing explicit dependency on `aft-fts5e2e.12`; (b) "bounded" default runs not defined with a number; (c) no anti-dead-code proof that backend modes are actually dispatched |

**Required revisions:**
1. Register `aft-fts5e2e.12` as a dependency, or add explicit text: "FTS5 mode is optional-skip. If FTS5 benchmark hooks from `aft-fts5e2e.12` are not available, FTS5 mode is reported as skipped. The bead does not block on FTS5 implementation."
2. Define "bounded": e.g., "Default quick run executes at most 6 modes × 3 candidate pool values = 18 backend configurations, each limited to the quick profile's 120–180 queries."
3. Add acceptance criterion: "Tests or a smoke run prove each of the 6 backend modes is invoked (not just defined) and produces output or a documented skip record."

### aft-t6p.bench.quick.04 — Graded file/span and context-budget metrics

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Task — acceptable (metric computation logic, not user-visible behavior per se) |
| **Objective** | Clear — file-level, span/chunk, graded relevance, context-budget metrics |
| **Acceptance criteria** | Good — covers separation of file/span, grading, token budgets, duplicates, tests |
| **Edge cases** | Good — covers file-only annotations, approximate tokenization, secondary vs primary |
| **Validation** | Weak — only "metric unit tests" and "smoke benchmark" without specific commands |
| **Weakness** | (a) Validation commands are vague; (b) no anti-dead-code proof that metrics are computed from real search results, not just defined |

**Required revisions:**
1. Strengthen validation: "Run `bun run benchmarks/semble/run.ts --profile smoke` and verify the JSON report contains `file_recall_at_k`, `span_recall_at_k`, `context_budget_recall`, and `graded_relevance` fields. Run metric unit tests with `bun test` for the metrics module."
2. Add acceptance criterion: "Metrics are computed from actual search result output (not mocked or stubbed), and a smoke run produces non-empty metric values for at least one backend mode."

### aft-t6p.bench.quick.05 — Report schema and regression thresholds

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Task — acceptable (schema + validation logic) |
| **Objective** | Excellent — specifies all required metadata fields, schema versioning, threshold defaults |
| **Acceptance criteria** | Good — covers schema versioning, validation failure, skipped modes, thresholds, latency |
| **Edge cases** | Good — covers skipped optional modes, missing metadata, latency warning vs hard gate |
| **Validation** | Weak — only "report schema/unit tests" and "smoke benchmark report validation" |
| **Weakness** | (a) No specific validation command; (b) schema version value not specified; (c) threshold values (5 percentage points) are mentioned in design but not in acceptance criteria |

**Required revisions:**
1. Add validation command: "Run smoke benchmark, validate output against schema with `ajv` or equivalent TypeScript type check, and verify baseline comparison produces pass/fail verdicts."
2. Add acceptance criterion: "Default regression thresholds are: recall/nDCG drops > 5pp = hard fail; latency increases > 50% = warning. These values are documented and tested."
3. Specify initial schema version (e.g., `v1`) in acceptance criteria.

### aft-t6p.bench.quick.06 — Document methodology, limitations, and commands

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Task — correct (documentation only) |
| **Objective** | Clear — comprehensive docs covering all modes, metrics, limitations |
| **Acceptance criteria** | Good — covers commands, mode distinction, interpretation, claim prevention |
| **Validation** | Weak — only "docs lint if available" and "run documented smoke command" |
| **Weakness** | Validation is too vague; docs accuracy is not verifiable without running the actual commands |

**Required revisions:**
1. Add validation command: "Run every command documented in the README and confirm it produces the expected output or a documented skip. Specifically: `bun run benchmarks/semble/run.ts --profile smoke`, `bun run benchmarks/semble/run.ts --profile quick`, and the extended/full dry-run commands."
2. Add acceptance criterion: "Every command in the documentation was executed at least once during this bead's implementation and the output was verified to match what the docs describe."

### aft-t6p.bench.quick.07 — Verify implementation end to end

| Aspect | Assessment |
|---|---|
| **Verdict** | READY |
| **Type fit** | Task (verification) — correct |
| **Objective** | Clear — independent verification of all child beads |
| **Acceptance criteria** | Excellent — coverage matrix, reachability, tests, full-path isolation, verdict |
| **Procedure** | Excellent — well-structured verification procedure with required audit tables |
| **Weakness** | Minor — "Tests or validation prove..." could be more specific about what test infrastructure should exist |

**Notes:** This bead is well-written. The required audit tables (requirement coverage, reachability, test reality, dead code, risks) are exactly what the verification scenario playbook requires. No revisions needed.

### aft-t6p.bench.quick.08 — Milestone closure

| Aspect | Assessment |
|---|---|
| **Verdict** | READY |
| **Type fit** | Milestone — correct (no implementation work) |
| **Objective** | Clear — close after verification |
| **Acceptance criteria** | Good — verification result, commands documented, optional work excluded |
| **Weakness** | None significant |

### aft-t6p.bench.quick.repoqa — RepoQA adapter (optional)

| Aspect | Assessment |
|---|---|
| **Verdict** | READY WITH REVISIONS |
| **Type fit** | Task (adapter) — correct |
| **Objective** | Clear — optional function-level semantic retrieval adapter |
| **Acceptance criteria** | Good — optional/manual, deterministic mapping, schema reuse, docs, skip handling |
| **Weakness** | (a) Only blocks on `.05` — should also depend on `.01` (corpus definitions) and `.03` (backend matrix); (b) no validation command for running the adapter |

**Required revisions:**
1. Add blocking dependencies on `.01` and `.03`.
2. Add validation command: "If RepoQA dataset is available: `bun run benchmarks/semble/repoqa-adapter.ts --fixture <tiny-fixture>`. If not: verify the adapter reports 'dataset not found' with a clear actionable message."

---

## Missing anti-dead-code safeguards

| Bead ID | Missing safeguard | Why it matters | Exact instruction to add |
|---|---|---|---|
| `.01` | No proof profile definitions are imported by the runner | Profiles could exist in a module that nothing imports — dead code | Add: "A smoke or dry-run command exercises the profile definitions end-to-end. The profile module is imported by the runner entry point." |
| `.03` | No proof backend modes are dispatched, not just defined | Backend mode registry could exist without the runner ever calling it | Add: "A smoke run invokes at least 2 backend modes and produces output or documented skip for each. The mode registry is imported and used by the runner." |
| `.04` | No proof metrics are computed from real results | Metrics could be defined but never called during a benchmark run | Add: "A smoke run produces a report containing computed metric values (not placeholder zeros). The metrics module is imported by the report generator." |
| `.05` | No proof report validation runs against actual output | Schema could exist but never be applied to real reports | Add: "A smoke run produces a report that passes schema validation. The validation function is called during report generation." |

---

## Acceptance criteria defects

| Bead ID | Weak criterion | Failure mode it permits | Replacement criterion |
|---|---|---|---|
| `.01` | "Existing pilot behavior remains available or has a clear compatibility alias" | Pilot could be aliased to a function that returns empty results — technically "available" but broken | "Existing pilot behavior produces the same output as before the profile refactor, verified by running the pilot command and comparing results." |
| `.02` | "Full mode cannot be triggered accidentally by normal CI or default test commands" | CI could trigger full mode through an indirect path (e.g., a test helper that calls the benchmark runner without profile flags) | "No code path in `package.json` scripts, CI configs, or test helpers invokes the benchmark runner with `--profile full` or without an explicit profile flag." |
| `.03` | "Default commands remain bounded and do not accidentally launch slow full-corpus/model sweeps" | "Bounded" is undefined — 18 backend configs × 180 queries could still be slow | "Default quick run completes within a documented time bound (e.g., <10 minutes on reference hardware). The matrix is explicitly enumerated, not dynamically expanded." |
| `.05` | "Default regression thresholds are documented and tested" | Thresholds could be documented but never enforced in the comparison logic | "A test asserts that a mock report with recall drop of 6pp triggers a regression verdict, and a report with 4pp drop passes." |

---

## Missing edge/failure cases

| Bead ID | Missing case | Why it matters | Suggested acceptance/test requirement |
|---|---|---|---|
| `.01` | All repos in a profile have zero valid annotations | Profile would silently produce empty benchmark results | Add: "Profile validation fails or reports a clear skip when the resolved annotation count is zero." |
| `.02` | Profile resolves to zero queries (e.g., all repos excluded) | Runner could hang or produce an empty report with no indication | Add: "If profile resolution yields zero queries, the runner exits with a clear error message naming the profile and the reason." |
| `.03` | All optional backends are unavailable (no semantic, no reranker, no FTS5) | Quick run would only run lexical baselines — valid but should be explicitly reported | Add: "When all optional backends are skipped, the report includes a warning section listing which modes were skipped and why." |
| `.04` | Annotation has span data but the search result doesn't return span information | Span metrics would be silently missing | Add: "When annotations include spans but results lack span data, span metrics are reported as `not_applicable` with a count of affected queries." |
| `.05` | Two consecutive runs produce identical metrics (no regression, no improvement) | Comparison could treat identical results as neither pass nor fail | Add: "Identical baseline/current metrics produce a 'no regression' verdict with exit code 0." |

---

## Sequencing / dependency defects

| Bead ID | Problem | Required graph change |
|---|---|---|
| `.03` | Implicit dependency on `aft-fts5e2e.12` (FTS5 benchmark hooks) not registered | Either register as a blocking dependency, or add explicit text: "FTS5 is optional-skip. This bead does not block on FTS5 implementation." |
| `.repoqa` | Only blocks on `.05` — needs `.01` (corpus) and `.03` (backend matrix) to be meaningful | Add blocking dependencies on `.01` and `.03` |
| `.04` | References `aft-t6p.33`, `aft-t6p.35.1`, `aft-t6p.34` as source refs but doesn't depend on them | Verify those beads are closed, or register as dependencies if they affect metric definitions |
| `.07` | No integration-test-ownership bead exists | `.07` can verify, but nobody writes the integration tests. Add a test clause to `.02` or `.05`, or split into a dedicated sub-bead |

---

## Test instruction defects

| Bead ID | Weakness | Better test requirement |
|---|---|---|
| `.01` | "bun test for touched benchmark modules, if available" — too vague | "Write or update tests that: (a) resolve each named profile and assert expected repo counts; (b) validate profile resolution against the lockfile; (c) fail if a profile references a repo not in the lockfile." |
| `.02` | "unit tests for argument parsing and skip reasons" — doesn't test the integration path | "Write tests that: (a) parse CLI flags for each profile; (b) simulate a missing binary and assert skip output; (c) simulate a missing reranker and assert skip record in JSON output." |
| `.03` | "unit tests for mode registry and rerank sweep expansion" — doesn't prove modes are dispatched | "Write tests that: (a) register all 6 modes and assert they appear in the mode list; (b) expand candidate pool [20, 50, 100] and assert 3 sweep configurations; (c) mock a search call for each mode and assert it was invoked." |
| `.04` | "metric unit tests" — no specificity | "Write tests that: (a) compute recall@k for a known result set with primary and secondary annotations; (b) compute span metrics when spans are present; (c) return `not_applicable` when spans are absent; (d) handle duplicate results; (e) compute context-budget recall at 1k/2k/4k/8k token thresholds." |
| `.05` | "report schema/unit tests" — no specificity | "Write tests that: (a) validate a complete report against the schema; (b) reject a report missing required metadata; (c) assert regression verdict for a 6pp recall drop; (d) assert no-regression for a 3pp drop; (e) assert warning for a 60% latency increase." |

---

## Recommended skills/tools additions

| Bead ID | Local skill/tool or category | Required? | Why | When to use |
|---|---|---|---|---|
| `.01` | code search / semantic search | Yes | Need to inspect existing corpus loader code and lockfile schema before adding profiles | Before editing `corpus.ts`, `repos-pilot.json`, `repos.json` |
| `.02` | test/integration validation | Yes | CLI entrypoints need integration tests, not just unit tests | When writing skip-handling and profile-selection tests |
| `.03` | performance/latency benchmarking | Yes | Backend matrix runs need timing and bounded execution | When implementing the mode registry and sweep controls |
| `.04` | test/integration validation | Yes | Metric computation needs test fixtures with known expected outputs | When writing metric unit tests with primary/secondary/span cases |
| `.05` | test/integration validation | Yes | Schema validation needs negative test cases | When testing report validation failure modes |
| `.07` | code review / implementation verification | Yes | Verification bead needs thorough code review of all child bead outputs | During verification execution |

---

## Missing verifier / approval / spike Beads

| Needed Bead | Type | Priority | Blocks? | Acceptance summary |
|---|---|---|---|---|
| Integration test ownership for profile→runner→backend→metrics pipeline | task (test) | Medium | Should block `.07` | Integration tests exist that exercise the full pipeline end-to-end; tests fail if any production wiring is removed |
| Corpus fixture validation spike (if Semble annotation availability for 8-repo quick profile is uncertain) | spike | Low | Could block `.01` | Confirms that the recommended 8 repos have sufficient annotations in the Semble corpus; documents any substitutions needed |

---

## Suggested patched Bead text

### aft-t6p.bench.quick.01 — Additions to acceptance criteria

```markdown
## Acceptance criteria

- [ ] `smoke`, `quick`, `quick-extended`, and `full` are represented as named profiles or equivalent deterministic selectors.
- [ ] `quick` defaults to 8 repos unless annotation availability requires a documented substitution.
- [ ] `quick-extended` adds 4 repos without changing `quick` defaults.
- [ ] Profile validation fails loudly for missing repos/annotations and reports skipped cases.
- [ ] Existing pilot behavior remains available and produces the same output as before the profile refactor, verified by running the pilot command and comparing results.
- [ ] Profile definitions are imported and used by the benchmark runner entry point (e.g., `pilot.ts` or `run.ts`). A smoke or dry-run that selects a profile and resolves repos proves the wiring.
- [ ] If all repos in a profile have zero valid annotations, the profile reports this with a clear error or skip — not silent empty results.
```

### aft-t6p.bench.quick.03 — Additions to dependency and acceptance criteria

```markdown
## Dependencies

Add: `aft-fts5e2e.12` (FTS5 benchmark hooks) as a blocking dependency.
Rationale: FTS5 mode integration requires FTS5 benchmark hooks. If FTS5 is not implemented, FTS5 mode is skipped — register the dependency to make this explicit.

## Acceptance criteria

- [ ] Quick benchmark can run the agreed backend matrix or cleanly skip unavailable optional modes.
- [ ] Rerank candidate-pool sweeps include 20, 50, and 100 initial candidates.
- [ ] Reports include pre-rerank recall/candidate coverage and post-rerank ranking metrics.
- [ ] FTS5 mode integration reuses the FTS5 benchmark hooks rather than duplicating production behavior.
- [ ] Default commands remain bounded: default quick run executes at most 6 modes × 3 candidate pool values = 18 backend configurations, each limited to the quick profile's 120–180 queries.
- [ ] A smoke run invokes at least 2 backend modes and produces output or documented skip for each. The mode registry is imported and used by the runner.
- [ ] When all optional backends are skipped, the report includes a warning section listing which modes were skipped and why.
```

### aft-t6p.bench.quick.repoqa — Additions to dependencies

```markdown
## Dependencies

Add blocking dependencies on:
- `aft-t6p.bench.quick.01` — needs corpus/profile definitions to resolve query targets
- `aft-t6p.bench.quick.03` — needs backend matrix to run AFT search modes

Existing dependency on `aft-t6p.bench.quick.05` remains (needs report schema).
```

### aft-t6p.bench.quick.04 — Additions to validation and acceptance criteria

```markdown
## Validation commands

- metric unit tests: `bun test benchmarks/semble/metrics` (or equivalent)
- smoke benchmark producing file-level and context-budget metrics: `bun run benchmarks/semble/run.ts --profile smoke`
- verify report contains `file_recall_at_k`, `span_recall_at_k`, `context_budget_recall`, and `graded_relevance` fields
- fixture validation tests for file-only vs span-level labels
- `git diff --check`

## Acceptance criteria

- [ ] Quick reports separate file-level and span/chunk-level metrics.
- [ ] Primary and secondary relevance are graded, not collapsed without explanation.
- [ ] Token/line-budget metrics are present and explicitly exact or approximate.
- [ ] Metrics handle duplicate results and unavailable annotation detail deterministically.
- [ ] Tests cover primary/secondary/file-only/span-level scoring cases.
- [ ] Metrics are computed from actual search result output (not mocked or stubbed), and a smoke run produces non-empty metric values for at least one backend mode.
- [ ] When annotations include spans but results lack span data, span metrics are reported as `not_applicable` with a count of affected queries.
```

### aft-t6p.bench.quick.05 — Additions to validation and acceptance criteria

```markdown
## Validation commands

- report schema/unit tests: `bun test benchmarks/semble/schema` (or equivalent)
- smoke benchmark report validation: `bun run benchmarks/semble/run.ts --profile smoke` then validate output against schema
- baseline/current comparison test with expected pass/fail outcomes
- `git diff --check`

## Acceptance criteria

- [ ] Quick benchmark emits schema-versioned JSON (initial version: `v1`) with required reproducibility metadata.
- [ ] Report validation fails for missing required metadata.
- [ ] Baseline/current comparison handles skipped optional modes explicitly.
- [ ] Default regression thresholds are documented and tested: recall/nDCG drops > 5pp = hard fail; latency increases > 50% = warning.
- [ ] Latency is warning-oriented unless an explicit hard threshold is configured.
- [ ] A test asserts that a mock report with recall drop of 6pp triggers a regression verdict, and a report with 4pp drop passes.
- [ ] Identical baseline/current metrics produce a 'no regression' verdict with exit code 0.
```

---

## Final implementation order

The recommended order from the epic is correct:

1. `.00` — Record decision (no code, unblocks everything)
2. `.01` — Corpus/profile definitions (foundation for all runners)
3. `.02` — CLI entrypoints (wires profiles to runnable commands)
4. `.03` — Backend matrix (adds the comparison dimension)
5. `.04` — Graded metrics (adds the evaluation depth)
6. `.05` — Report schema (makes output machine-comparable)
7. `.06` — Docs (documents the finalized system)
8. `.07` — Verification (independent audit)
9. `.08` — Milestone (closure checkpoint)

**Optional (after `.05`):**
- `.repoqa` — RepoQA adapter

**Future (after `.08`):**
- `aft-t6p.bench.agentic` — agentic benchmark adapters
- `aft-t6p.bench.agentic.core` — CORE-Bench adapter

No changes to the implementation order are needed. The sequencing is logically sound.
