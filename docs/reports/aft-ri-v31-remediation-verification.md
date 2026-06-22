# AFT RI v31 Remediation Verification

Date: 2026-06-22
Branch: `semantic-search-enhancement`
Commit under test: `daaf46de` plus local protocol documentation correction
Binary under test: `D:/Coding/_tools/aft-src/target/release/aft/aft.exe`
Binary version: `aft 0.39.1`
Verification bead: `aft-ri-v31-remediate.15`

## Verdict

COMPLETE WITH RISKS.

The RI v31 runtime behavior is reachable through public `aft --stdio` and telemetry CLI entry points on a clean fixture. The benchmark harness emits `intent_metrics`, `context_quality`, and `rerank_context: "aft_output"`, and explicitly marks skipped or empty phases as incomplete.

Residual risk: the metric-producing smoke run for `serde` is still `status: "incomplete"` because `aft-grep` and `fts5` returned empty results for some natural-language phases. This is not hidden as a pass. The earlier documented `cortexkit/aft` smoke command produced no scored rows because that repo currently maps only to unverified no-relevance seeds; this was captured as `aft-ri-v31-remediate.17` and the protocol was corrected.

## Scope reviewed

- Public RI v2 activation via `configure` with `harness: "opencode"`.
- `semantic_search`, `explain_search`, `why_missed`, `aft_orient`, `aft_impact_delta`, and `aft_context_pack` through `aft --stdio`.
- Ranking feature provenance for definition and diagnostic queries.
- Default hash-only retrieval telemetry, raw-query opt-in, candidate/fusion score persistence, and prune command.
- Benchmark smoke output schema and CI recall gate controls.
- Protocol drift discovered during executable verification.

## Requirement coverage matrix

| Requirement | Status | Evidence | Notes |
|---|---|---|---|
| RI v2 is enabled through public configure path | PASS | Clean fixture stdio run returned `cfg.success: true` with `harness:"opencode"` and `retrieval_intelligence_v2:true`. | Docs now include `harness:"opencode"` in every configure example. |
| Search emits SearchPlan and run/result provenance | PASS | `search-plan` returned 10 responses total, top file `src/lib.rs`, `search_has_provenance: true`. | Repository-root scratch files can skew examples; clean fixture is deterministic. |
| Diagnostics run through public commands | PASS | `explain_search` returned lane weights; `why_missed` returned candidate-pool and lane diagnostics. | No helper-only proof counted. |
| Orientation, impact, and context pack reject placeholder-empty success | PASS | Fixture run: `orient_files: 2`, `context_items: 2`, `impact_health: "healthy"`. | Public commands exercised through stdio. |
| Ranking features are observable in production output | PASS | Definition query features included `exact_definition_boost` and `identifier_stem_match_boost`; diagnostic query features were `identifier_stem_match_boost`, `path_base_match_boost` and did not include `test_example_penalty`. | Test penalty still appears for definition results that hit test files, which is expected. |
| Telemetry persists hash by default and raw query only by opt-in | PASS | Default DB: `retrieval_runs=8`, `candidate_scores=14`, `fusion_scores=14`, latest `length(query_hash)=64`, `query_raw IS NULL=1`; raw opt-in persisted `secret query CandidateEntry`. | Prune returned success with `deleted_rows: 0`. |
| Benchmark smoke emits metrics and incomplete status for skipped phases | PASS WITH RISKS | `serde` smoke output contained `intent_metrics` for `NaturalLanguage` and `ExactSymbol`, context modes for 7 modes, `rerank_context:"aft_output"`, `hybrid-fe recall=0.9`, and `status:"incomplete"` with `empty result phases: aft-grep=6 fts5=3`. | Incomplete status is a real residual benchmark signal. |
| CI recall gate rejects regressions | PASS | Baseline-vs-baseline exited 0; synthetic regression exited 1 with `NaturalLanguage` recall drop 20.0%. | Matches protocol expectation. |
| Protocol examples match current runtime contract | PASS | Discovered bug `aft-ri-v31-remediate.17` created; docs updated to add `harness`, remove invalid `shutdown`, and use `serde` for metric smoke. | `shutdown` is not a public command; EOF ends stdio. |

## Reachability / wiring audit

| Artifact | Intended entry point | Caller/registration/config path | Test proves wiring? | Status |
|---|---|---|---|---|
| RI v2 search and provenance | `aft --stdio` `semantic_search` | `configure` sets project, storage, search index, semantic flag, and RI v2 intelligence flag | Yes, clean fixture stdio assertions fail without public output fields | PASS |
| Diagnostics | `aft --stdio` `explain_search`, `why_missed` | Stdio dispatcher command payloads | Yes, output includes lane weights and lane miss reasons | PASS |
| Orientation tools | `aft --stdio` `aft_orient`, `aft_impact_delta`, `aft_context_pack` | Stdio dispatcher command payloads | Yes, output contains non-empty files/items and graph health | PASS |
| Telemetry | `aft --stdio` search plus `aft telemetry prune` | Runtime search telemetry writers and telemetry CLI | Yes, SQLite rows and prune JSON prove persistence/CLI reachability | PASS |
| Benchmark runner | `bun run benchmarks/semble/pilot.ts` | CLI args `--profile smoke --repo serde --binary ...` | Yes, report schema contains metrics and incomplete reasons | PASS WITH RISKS |
| Recall gate | `bash scripts/ci-recall-gate.sh` | Shell wrapper and Node gate script | Yes, non-regression passes and synthetic regression fails | PASS |

## Test reality check

The public stdio fixture checks would fail if configure omitted `harness`, if command dispatchers were removed, if RI provenance was not serialized, or if placeholder orientation/context-pack responses were returned. Telemetry checks would fail if runtime search did not write retrieval rows or if raw query storage ignored the opt-in. The benchmark smoke check would fail if `intent_metrics`, `context_quality`, or `rerank_context` were missing.

## Old-path bypass audit

The clean fixture confirms RI v2 fields are emitted through public stdio output, not only helper/unit paths. The natural-language lexical-only fallback remains degraded when semantic search is disabled; this is surfaced through `semantic_unavailable`, `lexical_only_fallback`, and incomplete benchmark output rather than being accepted as a complete semantic pass.

## Dead/orphan code audit

No orphan-only proof was accepted. Validation evidence uses the built binary and public commands. Source code dead-code review was not repeated in this final binary-focused pass; previous remediation beads covered implementation wiring, and this report focuses on observable reachability.

## Findings

| ID | Severity | Confidence | Area | Finding | Evidence | Failure scenario | Minimal fix | Verification |
|---|---|---|---|---|---|---|---|---|
| F-001 | Medium | High | Benchmark protocol | `cortexkit/aft` smoke cannot produce scored metrics from verified canon because its seeds have no relevance entries. | Initial smoke report: `status:"incomplete"`, `no benchmark results produced`, `intent_metrics is empty`; canon summary showed `aft:4` only in `unverified-seeds.json` with empty `relevant`. | A release gate using that command could never produce metric evidence. | Use a verified canon repo such as `serde` for metric smoke; keep `aft` as incomplete negative control. | Docs patched; `serde` smoke emitted metrics and explicit incomplete reasons. |
| F-002 | Medium | High | Public protocol docs | Protocol omitted required `harness` and sent nonexistent `shutdown`. | Built binary returned missing `harness`; with harness added, `shutdown` returned unknown command. | Users following the protocol get false failures unrelated to RI behavior. | Add `harness:"opencode"` and remove shutdown; rely on EOF. | Docs patched and clean fixture run passed. |

## Missing tests

- A checked-in deterministic fixture or script would make the public protocol less sensitive to local untracked files in the repository root.
- Benchmark canon for `cortexkit/aft` still lacks verified relevance entries, so it should not be used as the metric-producing smoke target until canon review is added.

## Commands run

```text
D:/Coding/_tools/aft-src/target/release/aft/aft.exe --version
# aft 0.39.1

Clean fixture stdio verification:
# responses=10
# search_top_file=\\?\D:\Coding\_tools\aft-ri-v31-fixture\src\lib.rs
# search_has_provenance=true
# orient_files=2
# context_items=2
# impact_health=healthy
# definition_features=[exact_definition_boost, identifier_stem_match_boost, test_example_penalty]
# diagnostic_features=[identifier_stem_match_boost, path_base_match_boost]

sqlite3 D:/Coding/_tools/aft-ri-v31-fixture-storage-pass2/aft.db ...
# retrieval_runs=8, candidate_scores=14, fusion_scores=14, latest query_hash length=64, query_raw IS NULL=1

D:/Coding/_tools/aft-src/target/release/aft/aft.exe telemetry prune --storage-dir D:/Coding/_tools/aft-ri-v31-fixture-storage-pass2 --retention-days 30
# {"success":true,...,"deleted_rows":0}

Raw telemetry opt-in:
# SELECT query_raw ... -> secret query CandidateEntry

bun run benchmarks/semble/pilot.ts --profile smoke --repo serde --binary D:/Coding/_tools/aft-src/target/release/aft/aft.exe --output D:/Coding/_tools/aft-ri-v31-bench-smoke-serde.json
# status=incomplete
# incomplete_reasons=["empty result phases: aft-grep=6 fts5=3"]
# intent_metrics=[NaturalLanguage, ExactSymbol]
# context_quality modes=[lexical (rg), fts5, semantic-m2v, hybrid-m2v, semantic-fe, hybrid-fe, aft-grep]
# hybrid-fe recall=0.9

bash scripts/ci-recall-gate.sh benchmarks/baseline/schema-2026-06-20.json benchmarks/baseline/schema-2026-06-20.json
# exit 0, OK: no regressions detected

bash scripts/ci-recall-gate.sh benchmarks/baseline/schema-2026-06-20.json benchmarks/baseline/synthetic-regression.json
# exit 1, FAIL: regression detected, NaturalLanguage recall_at_10 dropped 20.0%

bd dep cycles
# No dependency cycles detected
```

## Follow-up Beads

- `aft-ri-v31-remediate.17` was created for protocol runtime drift, linked as blocking `aft-ri-v31-remediate.15`, fixed in `docs/testing-protocol-aft-ri-v31.md`, and can be closed with the evidence above.
