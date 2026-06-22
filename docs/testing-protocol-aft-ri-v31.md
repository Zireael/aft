# aft-ri-v31 Testing and Benchmarking Protocol

Date: 2026-06-22
Branch: `semantic-search-enhancement`
Epic: `aft-ri-v31-remediate`

This protocol validates Retrieval Intelligence v2 through public AFT entry points. Commands must be runnable as written from the repository root and must fail when AFT returns placeholder JSON, stale schema keys, skipped benchmark phases, or empty diagnostic/orientation payloads.

## Windows 11 Runner Prerequisites

- Git Bash or another Bash shell available as `bash`.
- Docker Desktop running for Rust compile/test checks.
- Node.js 20+ for `scripts/ci-recall-gate.mjs`.
- Bun 1.1+ for `benchmarks/semble/pilot.ts`.
- `jq` for JSON assertions.
- `sqlite3` only for telemetry database inspection.

Suggested environment:

```bash
cd D:/Coding/_tools/aft-src
export AFT_ROOT="D:/Coding/_tools/aft-src"
export AFT_BIN="${AFT_BIN:-D:/Coding/_tools/aft-src/target/release/aft/aft.exe}"
export AFT_STORAGE="${AFT_STORAGE:-D:/Coding/_tools/aft-ri-v31-smoke}"
test -x "$AFT_BIN" || { echo "AFT_BIN is not executable: $AFT_BIN"; exit 1; }
```

Do not run host `cargo` in this environment. Use:

```bash
cd "D:/Coding/_tools/aft-src" && bash scripts/zir-aft-check.sh quick --keep-going
```

## Required NDJSON Setup

Every stdio request in this protocol includes an `id`. RI v2 must be enabled through the public `configure` payload, not only with the `RETRIEVAL_INTELLIGENCE_V2` process environment override.

```bash
rm -rf "$AFT_STORAGE"
printf '%s\n' \
  "{\"id\":\"cfg-ri-v31\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"status-ri-v31","command":"status"}' \
  | "$AFT_BIN" --stdio
```

Pass criteria:

```bash
jq -e 'select(.id == "cfg-ri-v31") | .status == "ready"'
```

## Phase 1: SearchPlan and Provenance Contract

Run:

```bash
printf '%s\n' \
  "{\"id\":\"cfg-search\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"search-plan","command":"semantic_search","query":"SemanticBackendConfig","top_k":10,"diagnostics":true}' \
  | "$AFT_BIN" --stdio > "$AFT_STORAGE/search-plan.ndjson"
```

Assertions:

```bash
jq -e 'select(.id == "search-plan")
  | .status == "ready"
  and (.results | length > 0)
  and (.search_plan_debug.intent | type == "string")
  and (.search_plan_debug.lane_weights | type == "object")
  and (.retrieval_intelligence_provenance | type == "object")
  and (.retrieval_intelligence_provenance.ranking_features | type == "array")
  and (.urfk_provenance == null)
  and ((.results[0].provenance.lanes | length) > 0)
  and (.results[0].is_exact_hit | type == "boolean")
  and (.results[0].exact_hit_floor_applied | type == "boolean")
  and (.results[0].is_graph_expansion | type == "boolean")
  and (.results[0].enrichment_state | IN("enriched","not_enriched","path_only"))' \
  "$AFT_STORAGE/search-plan.ndjson"
```

This rejects the stale `.extras.search_plan_debug` and `.urfk_provenance` paths.

## Phase 2: Diagnostics

Run:

```bash
printf '%s\n' \
  "{\"id\":\"cfg-diag\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"explain-search","command":"explain_search","query":"SemanticBackendConfig"}' \
  '{"id":"why-missed-absent","command":"why_missed","query":"retry","expected_file":"src/nonexistent.rs"}' \
  '{"id":"why-missed-present","command":"why_missed","query":"SemanticBackendConfig","expected_file":"src/commands/semantic_search.rs"}' \
  | "$AFT_BIN" --stdio > "$AFT_STORAGE/diagnostics.ndjson"
```

Assertions:

```bash
jq -e 'select(.id == "explain-search")
  | (.explain_search_result.query_intent | type == "string")
  and (.explain_search_result.lane_weights | length > 0)
  and (.explain_search_result.active_safety_lane | IN("FTS5Body","TrigramBody"))' \
  "$AFT_STORAGE/diagnostics.ndjson"

jq -e 'select(.id == "why-missed-absent")
  | .why_missed_result.was_in_candidate_pool == false
  and (.why_missed_result.suggested_fix | type == "string" and length > 0)' \
  "$AFT_STORAGE/diagnostics.ndjson"

jq -e 'select(.id == "why-missed-present")
  | (.why_missed_result.missing_from_lanes | type == "array")
  and (.why_missed_result.search_execution_status | type == "string")' \
  "$AFT_STORAGE/diagnostics.ndjson"
```

## Phase 3: Orientation, Impact, and Context Pack

Run:

```bash
printf '%s\n' \
  "{\"id\":\"cfg-orient\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"orient-pipeline","command":"aft_orient","query":"semantic search pipeline","depth":2}' \
  '{"id":"impact-semantic","command":"aft_impact_delta","symbol":"handle_semantic_search","change_type":"signature"}' \
  '{"id":"context-pack","command":"aft_context_pack","query":"search pipeline","token_budget":4000}' \
  | "$AFT_BIN" --stdio > "$AFT_STORAGE/orientation.ndjson"
```

Assertions that reject placeholder-empty success:

```bash
jq -e 'select(.id == "orient-pipeline")
  | (.orient_result.primary_files | length > 0)
  and (.orient_result.entry_symbols | length > 0)
  and (.orient_result.orientation_summary | type == "string" and length > 0)
  and (.orient_result.orientation_summary != "unknown is implemented in unknown")
  and (.orient_result.latency_ms < 1000)' \
  "$AFT_STORAGE/orientation.ndjson"

jq -e 'select(.id == "impact-semantic")
  | (.impact_delta_result.symbol | type == "string")
  and (.impact_delta_result.change_type == "signature")
  and (
    (.impact_delta_result.graph.health == "healthy"
      and (.impact_delta_result.blast_radius.symbol_count > 0)
      and (.impact_delta_result.mutation_risk != "Unknown"))
    or
    (.impact_delta_result.graph.health != "healthy"
      and (.impact_delta_result.graph.degraded_reason | type == "string" and length > 0))
  )' \
  "$AFT_STORAGE/orientation.ndjson"

jq -e 'select(.id == "context-pack")
  | (.context_pack_result.pack | length > 0)
  and (.context_pack_result.tokens_used > 0)
  and (.context_pack_result.tokens_used <= (.context_pack_result.token_budget * 1.10))
  and ((.context_pack_result.omission_reason // "") | test("placeholder|scaffold"; "i") | not)' \
  "$AFT_STORAGE/orientation.ndjson"
```

## Phase 4: Ranking Features

Run:

```bash
printf '%s\n' \
  "{\"id\":\"cfg-ranking\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"rank-definition","command":"semantic_search","query":"CandidateEntry","top_k":10,"diagnostics":true}' \
  '{"id":"rank-diagnostic","command":"semantic_search","query":"E0433 unresolved import","top_k":10,"diagnostics":true}' \
  | "$AFT_BIN" --stdio > "$AFT_STORAGE/ranking.ndjson"
```

Assertions:

```bash
jq -e 'select(.id == "rank-definition")
  | (.retrieval_intelligence_provenance.ranking_features | length > 0)
  and ([.retrieval_intelligence_provenance.ranking_features[].applied[].feature]
    | index("exact_definition_boost") != null)' \
  "$AFT_STORAGE/ranking.ndjson"

jq -e 'select(.id == "rank-diagnostic")
  | ([.retrieval_intelligence_provenance.ranking_features[].applied[].feature]
    | index("test_example_penalty") == null)' \
  "$AFT_STORAGE/ranking.ndjson"
```

## Phase 5: Telemetry

Run:

```bash
rm -rf "$AFT_STORAGE-telemetry"
printf '%s\n' \
  "{\"id\":\"cfg-telemetry\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE-telemetry\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"search-telemetry","command":"semantic_search","query":"test query","top_k":5}' \
  | "$AFT_BIN" --stdio > /dev/null
```

Assertions:

```bash
sqlite3 "$AFT_STORAGE-telemetry/aft.db" 'SELECT COUNT(*) FROM retrieval_runs;' | jq -e 'tonumber >= 1'
sqlite3 "$AFT_STORAGE-telemetry/aft.db" 'SELECT query_raw IS NULL FROM retrieval_runs ORDER BY CAST(timestamp AS INTEGER) DESC LIMIT 1;' | jq -e 'tonumber == 1'
"$AFT_BIN" telemetry prune --storage-dir "$AFT_STORAGE-telemetry" --retention-days 30 | jq -e '.success == true and (.deleted_rows | type == "number")'
```

Raw-query mode remains opt-in:

```bash
rm -rf "$AFT_STORAGE-telemetry-raw"
printf '%s\n' \
  "{\"id\":\"cfg-raw-telemetry\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE-telemetry-raw\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true,\"telemetry\":{\"telemetry_store_query\":\"raw\"}}}" \
  '{"id":"search-raw-telemetry","command":"semantic_search","query":"secret query","top_k":5}' \
  | "$AFT_BIN" --stdio > /dev/null

sqlite3 "$AFT_STORAGE-telemetry-raw/aft.db" "SELECT query_raw FROM retrieval_runs WHERE query_raw = 'secret query' LIMIT 1;" \
  | jq -R -e '. == "secret query"'
```

## Phase 6: Benchmark Smoke and CI Gate

One-command smoke test:

```bash
cd D:/Coding/_tools/aft-src && \
bun run benchmarks/semble/pilot.ts \
  --profile smoke \
  --repo serde \
  --binary "$AFT_BIN" \
  --output .aft-bench/ri-v31-smoke-serde.json
```

Smoke metric assertions:

```bash
jq -e '(.status == "complete" or (.status == "incomplete" and (.incomplete_reasons | length > 0)))
  and .profile == "smoke"
  and .rerank_context == "aft_output"
  and (.intent_metrics | type == "object" and length > 0)
  and (.context_quality | type == "object" and length > 0)' \
  .aft-bench/ri-v31-smoke-serde.json
```

The smoke target uses `serde` because it has verified identifier canon entries. `cortexkit/aft` currently maps only to unverified seeds with no relevance entries, so it is useful as an incomplete-report negative control but not as metric-producing smoke evidence.

If a repo cannot be cloned, a backend cannot produce results, or all phases are empty, the report must be explicit:

```bash
jq -e 'select(.status == "incomplete")
  | (.incomplete_reasons | length > 0)' \
  .aft-bench/ri-v31-smoke-serde.json
```

CI gate controls:

```bash
bash scripts/ci-recall-gate.sh \
  benchmarks/baseline/schema-2026-06-20.json \
  benchmarks/baseline/schema-2026-06-20.json
# Expected: exit 0

bash scripts/ci-recall-gate.sh \
  benchmarks/baseline/schema-2026-06-20.json \
  benchmarks/baseline/synthetic-regression.json
# Expected: exit 1 with "FAIL: regression detected"
```

The gate must also fail any current report with `status: "incomplete"`, missing `intent_metrics`, missing `context_quality`, or `rerank_context` other than `aft_output`.

## Phase 7: End-to-End Contract Smoke

Run:

```bash
printf '%s\n' \
  "{\"id\":\"cfg-e2e\",\"command\":\"configure\",\"harness\":\"opencode\",\"project_root\":\"$AFT_ROOT\",\"storage_dir\":\"$AFT_STORAGE\",\"search_index\":true,\"semantic_search\":false,\"intelligence\":{\"retrieval_intelligence_v2\":true}}" \
  '{"id":"e2e-search","command":"semantic_search","query":"how does the reranker work","diagnostics":true,"top_k":10}' \
  | "$AFT_BIN" --stdio > "$AFT_STORAGE/e2e.ndjson"
```

Assertions:

```bash
jq -e 'select(.id == "e2e-search")
  | .status == "ready"
  and (.results | length > 0)
  and (.search_plan_debug != null)
  and (.retrieval_intelligence_provenance != null)
  and (.urfk_provenance == null)
  and ((.results[0].file | type == "string") and (.results[0].file | test("\\.rs$")))
  and ((.results[0].provenance.lanes | length) > 0)
  and (.results[0].enrichment_state | IN("enriched","not_enriched","path_only"))' \
  "$AFT_STORAGE/e2e.ndjson"
```

## Pass Criteria Summary

| Phase | Required evidence |
|---|---|
| Setup | Public `configure` enables RI v2 and every request has an `id`. |
| Search | Top-level `search_plan_debug`, `retrieval_intelligence_provenance`, and per-result provenance are present. |
| Diagnostics | `explain_search` and `why_missed` execute real retrieval diagnostics. |
| Orientation | `aft_orient`, `aft_impact_delta`, and `aft_context_pack` reject empty placeholder success. |
| Ranking | Ranking feature diagnostics prove production ranking feature application. |
| Telemetry | Retrieval rows persist query hash by default and raw query only by opt-in. |
| Benchmark | Smoke report emits `intent_metrics`, `context_quality`, `rerank_context=aft_output`, and incomplete status for skipped phases. |
| Gate | Non-regression fixture passes; synthetic >5% regression fails. |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `unknown_command` | Binary is stale | Rebuild in the Docker-backed check flow, then rerun protocol. |
| `search_plan_debug` missing | RI v2 was not enabled through `configure` | Re-run setup with `intelligence.retrieval_intelligence_v2=true`. |
| `.extras.*` checks fail | Protocol or script is stale | Use the top-level fields in this file. |
| `aft_context_pack` has zero tokens/items | Context pack is not wired to real retrieval | Treat as a failure; do not pass the phase. |
| `aft_impact_delta` has degraded graph | Graph index is unavailable | Accept only if `graph.degraded_reason` is non-empty and explicit. |
| Benchmark status is `incomplete` | Repo clone/backend/result phase skipped | Fix prerequisites or backend setup before using the run as pass evidence. |
| CI gate exits 1 | Regression or invalid benchmark schema | Inspect `REGRESSIONS` and `SCHEMA FAILURES` output. |
