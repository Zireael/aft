# aft-ri-v31 — Testing & Benchmarking Protocol

**Date:** 2026-06-21
**Branch:** `semantic-search-enhancement`
**Epic:** aft-ri-v31 (AFT Retrieval Intelligence v1)

---

## Phase 0: Push & Build

### Step 0.1 — Push the branch

```bash
cd D:/Coding/_tools/aft-src
git push origin semantic-search-enhancement
```

### Step 0.2 — Trigger CI build

Push to `iter/**` or `semantic-search-enhancement` triggers `e2e-iter.yml` (macOS + Linux only). For a full binary build on all platforms, trigger `build-aft.yml` manually:

```bash
gh workflow run build-aft.yml \
  -f branch=semantic-search-enhancement \
  -f platforms=all
```

Or push a tag for a full release build:

```bash
git tag v0.39.0-rc1
git push origin v0.39.0-rc1
```

### Step 0.3 — Wait for build completion

```bash
gh run list --workflow=build-aft.yml --limit=1 --json databaseId,status,conclusion
```

### Step 0.4 — Download the binary

```bash
# Linux x64
gh run download <run-id> -n aft-linux-x64 -D /tmp/aft-bin
chmod +x /tmp/aft-bin/aft

# macOS arm64
gh run download <run-id> -n aft-darwin-arm64 -D /tmp/aft-bin

# Windows x64
gh run download <run-id> -n aft-win32-x64 -D /tmp/aft-bin
```

### Step 0.5 — Verify binary works

```bash
/tmp/aft-bin/aft --version
# Expected: aft v0.39.x
```

---

## Phase 1: Flag-Gated Feature Validation

All new features are behind `retrieval_intelligence_v2=true`. The flag defaults to `false` — existing behavior is unchanged.

### Step 1.1 — Baseline: flag=OFF produces identical output

```bash
# Run without flag — should produce standard semantic search output
echo '{"command":"semantic_search","query":"SemanticBackendConfig","diagnostics":true}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null | jq '{status, result_count: (.results | length)}'
```

Verify:
- `status: "ready"`
- `result_count > 0`
- No `search_plan_debug` in extras
- No `urfk_provenance` in extras

### Step 1.2 — Flag=ON: SearchPlan built and returned

```bash
echo '{"command":"semantic_search","query":"SemanticBackendConfig","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '{status, has_search_plan: (.extras.search_plan_debug != null), extras_keys: (.extras | keys)}'
```

Verify:
- `has_search_plan: true`
- `search_plan_debug` contains `intent`, `lane_weights`, `prefetch`, `fusion`

### Step 1.3 — Exact symbol query: ExactHitFloor working

```bash
echo '{"command":"semantic_search","query":"SemanticBackendConfig","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.results[:5] | map({file, score, is_exact: (.extras.is_exact_hit // false)})'
```

Verify:
- Symbol `SemanticBackendConfig` appears in top-5 results
- At least one result has `is_exact_hit: true`

### Step 1.4 — Vendor exclusion: vendor files not in top-5

```bash
echo '{"command":"semantic_search","query":"BinaryBridge","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.results[:5] | map(.file) | map(select(test("vendor|node_modules"))) | length'
```

Verify:
- Output is `0` — no vendor/node_modules files in top-5

---

## Phase 2: Telemetry Validation

### Step 2.1 — Telemetry persists by default

```bash
# Run a search (telemetry persists by default)
echo '{"command":"semantic_search","query":"test query","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null > /dev/null

# Check the database
sqlite3 .aft/index.sqlite "SELECT COUNT(*) FROM retrieval_runs;"
# Expected: >= 1
```

### Step 2.2 — query_raw is NULL by default (hash mode)

```bash
sqlite3 .aft/index.sqlite "SELECT query_hash, query_raw FROM retrieval_runs ORDER BY rowid DESC LIMIT 1;"
# Expected: <hash> | (null)
```

### Step 2.3 — query_raw populated in raw mode

```bash
echo '{"command":"semantic_search","query":"secret query","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true TELEMETRY_STORE_QUERY=raw /tmp/aft-bin/aft --stdio 2>/dev/null > /dev/null

sqlite3 .aft/index.sqlite "SELECT query_raw FROM retrieval_runs WHERE query_raw IS NOT NULL LIMIT 1;"
# Expected: secret query
```

### Step 2.4 — Prune old runs

```bash
/tmp/aft-bin/aft telemetry prune
# Expected: Pruned N rows older than 30 days
```

---

## Phase 3: Diagnostic Commands

### Step 3.1 — explain_search

```bash
echo '{"command":"explain_search","query":"SemanticBackendConfig"}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '{
    intent: .extras.explain_search_result.query_intent,
    safety_lane: .extras.explain_search_result.active_safety_lane,
    lane_count: (.extras.explain_search_result.lane_weights | length),
    degraded: .extras.explain_search_result.degraded_lanes
  }'
```

Verify:
- `intent` is `"Identifier"` (single uppercase word)
- `safety_lane` is `"FTS5Body"` or `"TrigramBody"`
- `lane_count > 0`
- `degraded` may be empty (normal) or list low-weight lanes

### Step 3.2 — why_missed for absent file

```bash
echo '{"command":"why_missed","query":"retry","expected_file":"src/nonexistent.rs"}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.extras.why_missed_result | {in_pool: .was_in_candidate_pool, fix: .suggested_fix}'
```

Verify:
- `in_pool: false`
- `fix` contains actionable suggestion

### Step 3.3 — why_missed for present file

```bash
echo '{"command":"why_missed","query":"SemanticBackendConfig","expected_file":"src/commands/semantic_search.rs"}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.extras.why_missed_result | {in_pool: .was_in_candidate_pool, missing: .missing_from_lanes}'
```

Verify:
- `in_pool` is either true or false depending on whether the file appears in search results

---

## Phase 4: Orientation Commands

### Step 4.1 — aft_orient

```bash
echo '{"command":"aft_orient","query":"semantic search pipeline","depth":2}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '{
    primary_files: (.extras.orient_result.primary_files | length),
    symbols: (.extras.orient_result.entry_symbols | length),
    summary: .extras.orient_result.orientation_summary,
    latency_ms: .extras.orient_result.latency_ms
  }'
```

Verify:
- `primary_files > 0`
- `symbols > 0`
- `summary` is a deterministic string (not empty, not JSON)
- `latency_ms < 500`

### Step 4.2 — aft_impact_delta

```bash
echo '{"command":"aft_impact_delta","symbol":"handle_semantic_search","change_type":"signature"}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.extras.impact_delta_result | {symbol, change_type, blast_radius}'
```

Verify:
- Returns valid JSON with `symbol` and `blast_radius`

### Step 4.3 — aft_context_pack

```bash
echo '{"command":"aft_context_pack","query":"search pipeline","token_budget":4000}' \
  | /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.extras.context_pack_result | {budget: .token_budget, used: .tokens_used, pack_items: (.pack | length)}'
```

Verify:
- Returns valid JSON
- `tokens_used <= 4400` (budget * 1.10)

---

## Phase 5: Ranking Features

### Step 5.1 — Exact definition boost

```bash
echo '{"command":"semantic_search","query":"CandidateEntry","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '.results[:3] | map({file, score})'
```

Verify:
- The file containing the `CandidateEntry` definition ranks above files that merely reference it

### Step 5.2 — Test penalty disabled for error queries

```bash
echo '{"command":"semantic_search","query":"E0433 unresolved import","diagnostics":true}' \
  | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null \
  | jq '[.results[] | select(.file | test("test|spec"))] | length'
```

Verify:
- Test files appear in results (not penalized for DiagnosticError intent)

---

## Phase 6: Benchmark Validation

### Step 6.1 — Run quick benchmark

```bash
cd benchmarks/semble
npx ts-node pilot.ts --profile smoke --repo cortexkit/aft 2>&1 | tail -20
```

### Step 6.2 — Check intent_metrics in output

```bash
npx ts-node pilot.ts --profile smoke --repo cortexkit/aft 2>&1 \
  | jq '.intent_metrics | keys'
# Expected: ["NaturalLanguage", "ExactSymbol", "PathLookup", ...]
```

### Step 6.3 — Check per-intent recall

```bash
npx ts-node pilot.ts --profile smoke --repo cortexkit/aft 2>&1 \
  | jq '.intent_metrics.NaturalLanguage.recall_at_10'
```

### Step 6.4 — CI regression gate

```bash
# Run full benchmark
npx ts-node pilot.ts --profile quick 2>&1 > /tmp/current-run.json

# Compare against baseline
bash scripts/ci-recall-gate.sh benchmarks/baseline/schema-2026-06-20.json /tmp/current-run.json
echo "Exit: $?"
# Expected: 0 (no regression) or 1 (regression detected)
```

---

## Phase 7: End-to-End Smoke Test

### Step 7.1 — Full search pipeline with all features

```bash
echo '{
  "command": "semantic_search",
  "query": "how does the reranker work",
  "diagnostics": true,
  "top_k": 10
}' | RETRIEVAL_INTELLIGENCE_V2=true /tmp/aft-bin/aft --stdio 2>/dev/null | jq '{
  status,
  result_count: (.results | length),
  has_plan: (.extras.search_plan_debug != null),
  has_provenance: (.extras.urfk_provenance != null),
  top_result: (.results[0] | {file, score}),
  diagnostics: .extras.search_plan_debug.intent
}'
```

Verify:
- `status: "ready"`
- `result_count > 0`
- `has_plan: true`
- `has_provenance: true`
- `top_result.file` is a real Rust source file
- `diagnostics` shows intent classification

---

## Expected Output Summary

| Phase | What | Pass Criteria |
|-------|------|---------------|
| 0 | Build | Binary runs, `--version` returns |
| 1 | Flag gating | flag=OFF identical; flag=ON adds SearchPlan + ExactHitFloor |
| 2 | Telemetry | Tables created, query_raw NULL, prune works |
| 3 | Diagnostics | explain_search + why_missed return valid JSON |
| 4 | Orientation | aft_orient returns files/symbols/summary <500ms |
| 5 | Ranking | Exact definition boost, test penalty gating |
| 6 | Benchmark | intent_metrics present, CI gate functional |
| 7 | E2E | Full pipeline with all features working together |

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `unknown_command: aft_orient` | Binary not built with latest code | Rebuild from latest commit |
| `search_plan_debug` missing | Flag not set | Add `RETRIEVAL_INTELLIGENCE_V2=true` |
| Telemetry tables missing | First run, no search executed | Run a semantic_search first |
| CI gate exits 1 | Recall regression | Check baseline thresholds in `benchmarks/baseline/` |
| `graph_context` empty | GraphHealth=Disabled | Normal — graph not indexed yet |
| `tokens_used > budget` | Placeholder not wired | Expected for v1 — context_pack is scaffolding |
