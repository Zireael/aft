
# AFT Retrieval Intelligence — Decision-Grade Agent Implementation PRD

**Document ID:** PRD-AFT-RI-V3.1-2026-06-18  
**Version:** 3.1 — Two-Review Assimilation Patch  
**Source status:** SOURCE-VERIFIED (core Rust path) + SOURCE-CONDITIONAL (bridge/plugin surfaces, aft-tokenizer API, orientation command registration)  
**Repository:** `Zireael/aft` · branch `semantic-search-enhancement`  
**Prior version:** PRD-AFT-RI-V3-2026-06-18

> **v3.1 scope:** This is a targeted compliance patch over v3. It fixes two critical logic errors (ExactHitFloor formula, PathOnly/reranker contradiction), one propagating typo (TrigamAdapter → TrigramAdapter), and adds missing canonical sections (ACTOR/STORY, NFR, SEC, ADR-010, ADR-011, SearchPlan schema, OPS notes, hold-out REQ, query-telemetry opt-in). Architecture and scope are unchanged.

---

## §0 Review Assimilation Report

Two independent reviews were received after PRD v3. This section records every accept/reject decision before changes are applied.

### §0.1 Per-Review Extraction

| Review | Core thesis | Critical issues found | High-value additions | Decision |
|---|---|---|---|---|
| **R1** (Technical corrections) | PRD v3 has one math error, one logic contradiction, a propagating typo, and several high-value gaps | ExactHitFloor formula wrong; PathOnly/reranker contradiction; TrigamAdapter typo | Rollout stages ADR, safety lane fallback, query telemetry opt-in, orientation tools ADR, hold-out as REQ, latency gates | Mostly accept |
| **R2** (Structural completeness) | v3 needs ACTOR/STORY, formal NFR/SEC/OPS sections, SearchPlan schema, and negative control on ExactHitFloor | ExactHitFloor needs negative control for vendor/minified false positives | SearchPlan schema contract, NFR/SEC sections, candidate identity chain, actor map | Mostly accept |

### §0.2 Idea Synthesis Table

| Idea ID | Idea | Source | Decision | Destination | Rationale |
|---|---|---|---|---|---|
| R1-01 | ExactHitFloor formula wrong: `max(5, pool_rank)` allows rank-12 hits to stay at rank 12 | R1 | **Accept CRITICAL** | ADR-003 revised, REQ-005b | Mathematical error confirmed. Correct rule: promote all exact hits to top-N |
| R1-02 | PathOnly contradiction: REQ-009b says PathOnly fallback; Warning 8 says never pass PathOnly to reranker | R1 | **Accept CRITICAL** | REQ-009b revised, ADR-005 | Genuine contradiction; resolution: PathOnly stays in pool but excluded from content reranker |
| R1-03 | TrigamAdapter → TrigramAdapter (propagates into Rust identifiers) | R1 | **Accept CRITICAL** | All occurrences | Typo in code identifier is a defect, not cosmetic |
| R1-04 | Add pinned commit SHA as SRC-000 | R1 | Accept | SRC-000 new | Branch is mutable; Phase 0 records SHA; PRD should reference it |
| R1-05 | Safety lane fallback if FTS5 unavailable: TrigramBody, then DegradedLiteralBodyScan | R1 | Accept | ADR-001 revised, REQ-003 | Without this, coding agent may hard-depend on FTS5 and break non-FTS builds |
| R1-06 | Rollout stages ADR (Stage A→D from compiled-but-unavailable to default-on) | R1 | Accept | ADR-011 new | Without explicit stages, retrieval_intelligence_v2 stays permanently hidden |
| R1-07 | Candidate identity: overlapping spans from different lanes need priority rule | R1 | Partially accept | REQ-005c revised | Accept priority chain; reject byte-range as too implementation-specific for v1 |
| R1-08 | Hold-out benchmark: promote from IMPL note to REQ | R1 | Accept | REQ-016b new | Benchmark is central to local decision-making; overfitting is a real risk |
| R1-09 | Raw query telemetry: default off; store hash; raw = explicit opt-in | R1 | Accept | ADR-009 revised, REQ-014b revised, SEC-002 | Queries can contain secrets, customer names, pasted code |
| R1-10 | Add ADR-010 for orientation tools | R1 | Accept | ADR-010 new | REQ-019–021 are public API contracts; they need ADR coverage |
| R1-11 | Epic split A (core) + B (orientation) at Beads level | Partially accept | Beads handoff note only | User instruction: one epic. Track separation in JSONL is sufficient |
| R1-12 | Profile-specific latency gates (p95 budget per profile) | R1 | Accept | NFR-001 new | Without concrete gates, agents satisfy recall while making search sluggish |
| R2-01 | Actor map + STORY IDs | R2 | Partially accept | ACTOR-*, STORY-* section | Prevents Beads becoming isolated chores. UC IDs = overkill for this scale |
| R2-02 | ExactHitFloor negative control: semantically relevant > unrelated exact hit in vendor/minified | R2 | Accept | REQ-005b revised, VAL-005b | Without this, floor overrides and surfaces vendor noise |
| R2-03 | Candidate identity priority: chunk_id > symbol_id > file+range+hash > path+line_range | R2 | Partially accept | REQ-005c revised | Accept chunk_id priority and content hash; skip byte-range |
| R2-04 | SearchPlan schema table with mandatory_lanes, suppressed_lanes, lane_timeout | R2 | Accept | REQ-002 revised, §A.1 | Prevents agents inventing incompatible JSON fields |
| R2-05 | NFR section: measurable latency, recall, offline, telemetry bounds, degradation | R2 | Accept | NFR-001–005 new | Scattered latency/benchmark requirements need a canonical home |
| R2-06 | SEC section: snippet/query persistence defaults, pruning, no credentials in diagnostics | R2 | Accept | SEC-001–005 new | Code handles paths, queries, possibly secrets |
| R2-07 | OPS section: doctor behavior, stale index, recovery | Partially accept | OPS-001, OPS-005 as IMPL notes; OPS-002–004 = DEF-015 | Core OPS items (doctor, degraded lane reporting) are valuable for v1 |
| R2-08 | DOC section as blocking REQ | Defer | DEF-015 | Documentation is a separate track, not v1 scope |
| R2-09 | Interface schemas mandatory before implementation | Accept | §A.1, §A.4 | Without schemas, agents invent incompatible fields |
| R2-10 | Latency gates per profile | Accept | NFR-001 | Confirmed by both reviews; R1-12 |
| R2-13 | v3.1 compliance patch with formal canonical sections | Accept | Document structure | Both reviews agree |

### §0.3 Contradiction Resolution

| Issue | R1 position | R2 position | Resolution |
|---|---|---|---|
| ExactHitFloor formula | Promote all exact hits to top-N, ordered by exact-lane rank | Add negative control: vendor/minified exact hits must not override relevant semantic results | **Combined**: promote exact hits to top-N using exact-lane rank order, BUT exclude candidates from vendor/minified/generated files (is_generated=true or is_vendor=true) from ExactHitFloor promotion unless strict_exact hint is active |
| PathOnly/reranker | PathOnly stays in pool, excluded from content reranker | Not addressed | Accepted R1 resolution |
| Epic split | Two Beads epics at generation level | Two conceptual areas with hard boundary | User instruction: one epic. Track 6 separation in JSONL is the operational boundary |

---

## §1 Design Principles

**DP-001 — Engine owns context quality.** The benchmark reveals engine failures. The benchmark script must never compensate for missing engine context by reading source files as a default path. `benchmark_enriched` mode is an ablation only. `aft_output` must be the default benchmark mode.

**DP-002 — Exact-first invariant is a hard contract.** Exact symbol/path matches must surface before weaker semantic hits. ExactHitFloor enforces this post-RRF. Vendor/minified/generated files are excluded from floor promotion unless `strict_exact` is active.

**DP-003 — Safety-lane rule, not no-zero rule.** The body/content safety lane must always be active in auto-mode, with a minimum weight of 0.1. If FTS5 is unavailable, the safety lane degrades to TrigramBody. Expensive lanes may be near-zero for clear-intent queries when SearchPlan records the reason explicitly.

**DP-004 — PathOnly candidates never reach the content reranker.** Context-budget-exhausted candidates receive PathOnly fallback and remain in the final pool but are excluded from the content reranker. If the enriched-candidate ratio falls below `rerank_min_enriched_ratio` (default 0.5), the reranker is skipped entirely and pre-rerank order is used.

---

## §2 Actor Map and Story IDs

### Actors

| Actor | Description |
|---|---|
| **ACTOR-001** | Coding agent executing a repository modification or comprehension task |
| **ACTOR-002** | Human maintainer reviewing or tuning AFT retrieval behavior |
| **ACTOR-003** | Benchmark/evaluation runner (automated or manual) |
| **ACTOR-004** | Plugin/bridge integrator adding AFT tools to OpenCode/Pi |

### Stories

| Story | Actor | Narrative |
|---|---|---|
| **STORY-001** | ACTOR-001 | As a coding agent, I need exact symbol hits not buried by semantic consensus so I inspect the correct implementation first, not a semantically similar but wrong file. |
| **STORY-002** | ACTOR-001 | As a coding agent, I need context-enriched candidates before reranking so the reranker scores actual code, not path-only placeholders that make all candidates look equal. |
| **STORY-003** | ACTOR-002 | As a maintainer, I need `explain-search` to show lane provenance and exact-hit floor status so I can diagnose retrieval misses in under 5 minutes without reading source code. |
| **STORY-004** | ACTOR-003 | As a benchmark runner, I need `aft_output` mode to reflect only engine output so benchmark numbers expose engine deficiencies instead of hiding them. |
| **STORY-005** | ACTOR-001 | As a coding agent, I need graph-enriched search results (callers, callees, mutation risk) so I understand change impact without issuing separate navigation commands. |
| **STORY-006** | ACTOR-004 | As a plugin integrator, I need `aft_orient`, `aft_impact_delta`, and `aft_context_pack` as stable NDJSON commands so I can expose repository orientation to agents without reading AFT internals. |

---

## §3 Brownfield Invariants

| ID | Invariant | Enforcement |
|---|---|---|
| INV-001 | `grep`/`glob` trigram exact search behavior unchanged | Run existing grep tests before/after every change. Any regression = immediate rollback. |
| INV-002 | Exact literal substring search recall and latency must not regress | Existing benchmark smoke run. |
| INV-003 | No remote service may become required for the default AFT search path | Run `aft_search` with all remote services disabled; must succeed. |
| INV-004 | Existing `aft_search` CLI/NDJSON contract preserved by default. New capabilities are additive/opt-in. | Test existing integrations against new binary with no new flags. |
| INV-005 | Feature flags default off. All new retrieval capabilities ship disabled. | Verify `IntelligenceConfig` defaults; no new feature enabled by default. |
| INV-006 | Benchmark `aft_output` mode must reflect only AFT engine output, never benchmark-side enrichment | Benchmark default mode = `aft_output`; JSON records `rerank_context` field. |
| INV-007 | Exact symbol/path direct hits from non-vendor/non-generated files must appear in top-5 results | `ExactHitFloor` applied post-RRF. Vendor/generated files excluded from floor. Verified by VAL-005b. |

---

## §4 Evidence Ledger

### §4.1 Sources

| SRC ID | Source | Type | Confidence | Status |
|---|---|---|---|---|
| **SRC-000** | Zireael/aft · branch semantic-search-enhancement · commit SHA: TBD (Phase 0 records exact SHA; PRD references Phase 0 record) | Repository | High | SOURCE-CONDITIONAL until Phase 0 commits SHA |
| SRC-001 | Session user constraints | User | High | VERIFIED |
| SRC-002 | `crates/aft/src/commands/semantic_search.rs` | Code | High | SOURCE-VERIFIED |
| SRC-003 | `crates/aft/src/query_shape.rs` | Code | High | SOURCE-VERIFIED |
| SRC-004 | `crates/aft/src/semantic_rerank.rs` | Code | High | SOURCE-VERIFIED |
| SRC-005 | `crates/aft/src/config.rs` | Code | High | SOURCE-VERIFIED |
| SRC-006 | `crates/aft/src/fts5_planner.rs` | Code | High | SOURCE-VERIFIED |
| SRC-007 | `crates/aft/src/fts5_store.rs` | Code | High | SOURCE-VERIFIED |
| SRC-008 | `crates/aft/src/vector_store.rs` | Code | High | SOURCE-VERIFIED |
| SRC-009 | `crates/aft/src/callgraph.rs` + `callgraph_store/` | Code | High | SOURCE-VERIFIED |
| SRC-010 | `crates/aft/src/mutation_risk.rs` | Code | High | SOURCE-VERIFIED |
| SRC-011 | `crates/aft/src/ril_indexer.rs` | Code | High | SOURCE-VERIFIED |
| SRC-012 | `crates/aft/src/observability_ledger.rs` | Code | High | SOURCE-VERIFIED |
| SRC-013 | `benchmarks/semble/pilot.ts` | Code | High | SOURCE-VERIFIED |
| SRC-014 | vstash paper arXiv:2604.15484 | Research | Medium | VERIFIED |
| SRC-015 | GitHub Blackbird blog post | Prior art | High | VERIFIED |
| SRC-016 | Qdrant Hybrid Queries docs | Prior art | High | VERIFIED |
| SRC-017 | Semble README | Prior art | Medium-High | VERIFIED |
| SRC-018 | FastContext arXiv:2606.14066 | Research | Medium | VERIFIED |
| SRC-019 | RIG/SPADE paper arXiv:2601.10112 | Research | Medium | VERIFIED |
| SRC-020 | RRF literature (emergentmind, OpenSearch, Azure AI Search, TREC iKAT 2025) | Research | High | VERIFIED — confirms exact-hit demotion risk |

### §4.2 Key Findings (v3.1 additions in **bold**)

| FIND ID | Finding | Confidence | Source | PRD Impact |
|---|---|---|---|---|
| FIND-001 | `query_shape` classifier exists but is a routing heuristic, not a weighting prior | HIGH | SRC-003 | ADR-001; REQ-003 |
| FIND-002 | FTS5 multi-lane planner exists (6 lanes). Not yet wired into unified fusion | HIGH | SRC-006 | REQ-004 |
| FIND-003 | Semantic pool = top_k × 3, not configurable per SearchPlan | HIGH | SRC-002 | REQ-002 |
| FIND-004 | Current fusion has no lane provenance | HIGH | SRC-002 | REQ-005 |
| FIND-005 | `rerank_candidates()` runs BEFORE `enrich_snippets_from_source()`. ROOT CAUSE. | HIGH | SRC-002,004 | REQ-009; ADR-005 |
| FIND-006 | `snippet_line_budget()` hardcodes rank 0=20, 1-2=5, 3+=0 lines | HIGH | SRC-002 | REQ-008 |
| FIND-007 | `benchmarks/semble/pilot.ts` reads line ranges from disk when AFT snippets absent | HIGH | SRC-013 | REQ-015; INV-006 |
| FIND-008 | `callgraph_store`, `mutation_risk.rs`, `ril_indexer.rs` not used in search results | HIGH | SRC-009,010,011 | REQ-011; ADR-007 |
| FIND-009 | `VectorStore` trait prepared for pluggable backends; FlatF32 exists | HIGH | SRC-008 | DEF-002 |
| FIND-010 | `observability_ledger.rs` tracks tool-level metrics, not per-query retrieval diagnostics | MEDIUM | SRC-012 | REQ-014 |
| FIND-011 | vstash paper reports NEGATIVE rerank result in their setup | MEDIUM | SRC-014 | ADR-006 |
| FIND-012 | FastContext: compact context improves agent success vs verbose dumps | MEDIUM | SRC-018 | REQ-013 |
| FIND-013 | RRF rewards consensus; single-lane top-1 exact hits score lower than multi-lane mediocre hits | HIGH | SRC-020 | INV-007; REQ-005b; ADR-003 |
| **FIND-014** | **`max(5, pool_rank)` ExactHitFloor formula allows exact hit at natural rank 12 to remain at rank 12, violating INV-007** | **HIGH** | **R1 review + math** | **ADR-003 revised, REQ-005b** |
| **FIND-015** | **PathOnly fallback + "never pass PathOnly to reranker" are contradictory as written in v3** | **HIGH** | **R1 review** | **REQ-009b revised, ADR-005 revised** |
| **FIND-016** | **`TrigamAdapter` typo will propagate into Rust struct names, test names, docs** | **HIGH** | **R1 review** | **All occurrences** |

---

## §5 Assumptions and Constraints

| ASSUMP ID | Assumption | Confidence | Verification |
|---|---|---|---|
| ASSUMP-001 | AFT must remain local/offline. No remote service required for any default path. | HIGH | User repeated |
| ASSUMP-002 | SQLite is the durable source of truth. All new indexing and telemetry goes into SQLite. | HIGH | Source confirmed |
| ASSUMP-003 | One coding agent implements sequentially within each track. | HIGH | User stated |
| ASSUMP-004 | Local decision-making benchmark quality is sufficient. No publishable BEIR claims. | HIGH | User stated |
| ASSUMP-005 | Backward-compatible defaults. Existing behavior preserved unless user opts in. | HIGH | INV-001–005 |
| ASSUMP-006 | Tree-sitter provides sufficient syntactic graph for v1 enrichment. | MEDIUM | Source confirmed |
| ASSUMP-007 | Local branch state may differ from inspected GitHub branch. | MEDIUM | Phase 0 bead resolves |
| ASSUMP-008 | FlatF32VectorStore sufficient for expected repo scale (<100K chunks). | MEDIUM | Phase 0 latency baseline confirms |
| ASSUMP-009 | Graph syntactic/heuristic quality is sufficient for confidence-labeled enrichment. | MEDIUM | ADR-007; callgraph.rs |
| **ASSUMP-010** | **`retrieval_intelligence_v2` flag must progress through explicit rollout stages before becoming default-on; it must not remain permanently hidden.** | **HIGH** | **ADR-011; R1 review** |


---

## §6 Non-Functional Requirements

| NFR ID | Category | Requirement | Measurement | Profile |
|---|---|---|---|---|
| **NFR-001** | Latency | `retrieval_intelligence_v2` warm query p95 ≤ Phase-0 baseline p95 + 50ms | Benchmark latency column | `agent_fast`, rerank disabled |
| **NFR-001b** | Latency | `agent_deep` warm query p95 ≤ Phase-0 baseline p95 + 250ms | Benchmark latency column | `agent_deep`, without external rerank |
| **NFR-001c** | Latency | Rerank-enabled latency reported separately; never mixed into default comparison | Benchmark `rerank_enabled` column | All profiles |
| **NFR-001d** | Latency | Graph enrichment skipped or capped to top-3 results in `agent_fast` to preserve NFR-001 | Benchmark with graph enabled vs disabled | `agent_fast` |
| **NFR-002** | Recall | Exact symbol/path hits from non-vendor/non-generated files appear in top-5 under `auto` mode and top-1 under `strict_exact` | VAL-005b integration test | All modes |
| **NFR-003** | Offline | All default paths pass with remote services disabled | Run with all remote backends disabled; assert non-error result | All modes |
| **NFR-004** | Telemetry bounds | Default persistent telemetry: queries stored as hash only; snippets not stored; DB pruned at retention_days | ADR-009; aft telemetry prune | Default config |
| **NFR-005** | Degradation | When any retrieval lane is stale, unavailable, or times out, AFT must return results from remaining lanes plus a `degraded_lanes` diagnostic field | Integration test with one lane disabled | All modes |

---

## §7 Security Requirements

| SEC ID | Requirement | Default | Notes |
|---|---|---|---|
| **SEC-001** | Snippet text must not be persisted in telemetry by default | `no_snippet_persist = true` | Snippets may contain proprietary code |
| **SEC-002** | Query text persistence is configurable: `off` (store hash only), `redacted` (store sanitized), `raw` (explicit opt-in) | `telemetry_store_query = hash` | Queries may contain secrets, customer names, pasted stack traces |
| **SEC-003** | `aft telemetry prune` removes `retrieval_runs`, `candidate_scores`, `fusion_scores` rows older than `retention_days` | `retention_days = 30` | Prevents unbounded local data accumulation |
| **SEC-004** | Diagnostics and explain-search output must not include environment variables, remote API keys, or credential-adjacent strings | Enforced in diagnostic serializer | Diagnostic output may be shared |
| **SEC-005** | Local-only invariant: no telemetry leaves the local machine. All telemetry is written to SQLite only. | Hard requirement | Aligns with INV-003 and ASSUMP-001 |

> **Note on SEC-002 and `why-missed`:** The `why-missed` command re-runs the query live rather than reading raw query text from telemetry. This is why query hash storage (not raw text) is sufficient for all telemetry flows.

---

## §8 Requirements (v3.1 — all changes from v3 marked)

> Requirements unchanged from v3 are listed with status `NO CHANGE`. Material changes are marked `REVISED` or `NEW`.

| REQ ID | Requirement | Priority | Source Status | ADR | v3.1 Change |
|---|---|---|---|---|---|
| REQ-001 | Phase 0: Establish source baseline. Run benchmark in `aft_output` mode. Record baseline. Commit benchmark configuration schema and **pinned commit SHA (SRC-000)**. Record HEAD commit hash. | P0 BLOCKING | VERIFIED | — | Revised: add commit SHA |
| REQ-002 | SearchPlan struct per **§A.1 schema table**: intent (QueryIntent), prefetch (Vec\<RetrieverPlan\>), fusion (FusionPlan), ranking_profile (RankingProfile), context_budget (ContextBudget), rerank (RerankPlan), diagnostics (DiagnosticLevel), **mandatory_lanes** (Vec\<LaneKind\>), **suppressed_lanes** (Vec\<{lane, reason}\>). **Lane timeout behavior**: if semantic lane exceeds latency budget, exact/FTS lanes still return; diagnostic records `lane_timeout: true`. | P0 | VERIFIED | ADR-001 | Revised: schema table, mandatory_lanes, suppressed_lanes, timeout |
| REQ-002b | Vertical slice acceptance criterion: at least one end-to-end test must prove NDJSON entry point → SearchPlan construction → diagnostics field in response — before Track 2 begins. | P0 | VERIFIED | ADR-001 | NO CHANGE |
| REQ-003 | QueryIntent maps to LANE WEIGHTS only. **Safety-lane resolution rule**: body/content safety lane = FTS5Body if available, else TrigramBody (from `search_index.rs`) if `search_index.ready()`, else DegradedLiteralBodyScan. Safety lane weight floor = 0.1 in auto-mode. Hard suppression only via `hint=strict_*`. | P0 | VERIFIED | ADR-001 | Revised: safety lane fallback chain |
| REQ-004 | Lane adapters: FTS5Adapter, SemanticAdapter, **TrigramAdapter** (corrected from TrigamAdapter), SymbolExactAdapter, PathAdapter. Each returns CandidateSet with (chunk_id, score, source_lane, rank_in_lane, is_exact_hit). | P0 | VERIFIED | ADR-002 | Revised: typo fix |
| REQ-005 | Weighted RRF fusion over all active lanes. Every fused candidate carries CandidateProvenance with lane contributions. Provenance survives to final returned results. | P0 | VERIFIED | ADR-003 | NO CHANGE |
| REQ-005b | **ExactHitFloor (revised rule):** After RRF, before ranking features, sort the candidate pool into two groups: (A) direct exact hits from non-vendor/non-generated files where `is_exact_hit=true`, (B) all other candidates. Group A is ordered by `exact_lane_rank` (ascending). Group B follows in RRF score order. Final pool = Group A (all N exact hits, capped at `exact_hit_floor_n` default 5) ++ Group B. **Negative control**: a semantically relevant result must outrank an unrelated exact substring match from a vendor/generated/minified file unless `strict_exact` hint is active. | P0 | VERIFIED | ADR-003 | **REVISED (v3.1 critical fix)** |
| REQ-005c | **Canonical candidate identity (revised priority chain)**: (1) `chunk_id` if available from indexed symbol table; (2) `symbol_id` if symbol-scoped; (3) `file_path + line_range + content_hash` (content hash as fallback); (4) `path + line_range` only as last resort. Candidates at same canonical identity are deduped and their provenance merged. All contributing lane spans preserved in provenance. | P0 | VERIFIED | ADR-002 | Revised: priority chain, content hash |
| REQ-006 | QueryIntent drives adaptive lane weights (documented in ADR-001). FTS5Body safety lane never falls below 0.1. TrigramBody is the fallback safety lane when FTS5 unavailable. | P0 | VERIFIED | ADR-001 | Revised: TrigramBody fallback |
| REQ-007 | Deterministic ranking features: (a) ExactDefinitionBoost, (b) IdentifierStemMatchBoost, (c) PathBaseMatchBoost, (d) DocCommentBoost (NL only), (e) SameFileCoherenceBoost, (f) TestExamplePenalty — ONLY when QueryIntent does not request tests. All intent-conditioned, individually feature-flagged. | P1 | VERIFIED | ADR-004 | NO CHANGE |
| REQ-008 | ContextBudget model: total_tokens, per_candidate_tokens, min_candidate_chars, mode (ContextMode), enrich_pool (EnrichPool). Replace `snippet_line_budget()` entirely. | P0 | VERIFIED | ADR-005 | NO CHANGE |
| REQ-009 | **Enrich rerank pool BEFORE `rerank_candidates()`**. ROOT CAUSE FIX. | P0 ROOT CAUSE | VERIFIED | ADR-005 | NO CHANGE |
| REQ-009b | **Context budget exhaustion and PathOnly/reranker rule (revised):** When pool_size × min_candidate_chars > total_tokens: enrich highest-ranked candidates first. Budget-exhausted candidates receive PathOnly fallback (path + line_range + signature — not empty string). **PathOnly candidates are excluded from the content reranker.** If `enriched_count / rerank_pool_size < rerank_min_enriched_ratio` (default 0.5), skip content reranker entirely and use pre-rerank order. Emit `context_exhausted=true`, `unenriched_candidate_count=N`, `reranker_skipped_reason` in diagnostics. | P0 | VERIFIED | ADR-005 | **REVISED (v3.1 critical fix — PathOnly contradiction resolved)** |
| REQ-010 | Default context profiles: `agent_fast` (total=4000, per_candidate=300, enrich_pool=FusionPool), `symbol_exact` (total=2000, per_candidate=500, mode=Signature), `agent_deep` (total=12000, per_candidate=500, enrich_pool=RerankPool). | P1 | VERIFIED | ADR-005 | NO CHANGE |
| REQ-011 | Graph enrichment P1 scope — direct available facts only: callers (max 10), callees (max 10), imported_by (max 10), mutation_risk, is_public_export, graph_confidence. `graph_context` is null (not error) when GraphHealth is Disabled/Cold. Inferred hints (test_coverage_hint, config_owner) deferred to DEF-014. | P1 | CONDITIONAL (callgraph API) | ADR-007 | NO CHANGE |
| REQ-012 | Graph confidence reflects GraphHealth. Stale facts labeled. `graph_context` omitted gracefully when graph unhealthy. | P1 | VERIFIED | ADR-007 | NO CHANGE |
| REQ-013 | ContextMode: PathOnly, Signature, SymbolBody, SymbolBodyWithDocs, LineWindow, FileOutline, Auto. Compact modes first-class. | P1 | VERIFIED | ADR-005 | NO CHANGE |
| REQ-014 | Retrieval telemetry: `retrieval_runs`, `candidate_scores`, `fusion_scores` SQLite tables. Populated when diagnostics ≥ summary. diagnostics=off = zero overhead. | P1 | VERIFIED | ADR-009 | NO CHANGE |
| REQ-014b | **Telemetry retention and privacy (revised):** `retention_days=30`, `max_rows_per_run=500`, `telemetry_store_query=hash` (default — store SHA-256 of query, not raw text), `telemetry_store_query=raw` (explicit opt-in), `no_snippet_persist=true`, `telemetry_persist=true` (set false to disable all writes). `aft telemetry prune` command. `why-missed` re-runs query live rather than reading raw query text. | P1 | VERIFIED | ADR-009 | **REVISED (v3.1 — query hash default, not raw)** |
| REQ-015 | Benchmark `--rerank-context` flag: `aft_output` (default), `benchmark_enriched` (ablation), `path_only` (ablation). Mode recorded in JSON. | P0 | VERIFIED | ADR-008 | NO CHANGE |
| REQ-016 | Benchmark context quality diagnostics: candidate_pool_size, rerank_pool_size, snippet_count, path_only_count, avg_doc_tokens, pre_rerank_recall_at_pool, post_rerank_recall_at_k, lost_relevant_after_rerank, context_exhausted, unenriched_candidate_count, **reranker_skipped_reason** per mode. | P1 | VERIFIED | ADR-008 | Revised: add reranker_skipped_reason |
| **REQ-016b** | **Benchmark hold-out set**: the benchmark corpus must separate tuning rows from hold-out rows (`hold_out: true` field in canon). At least 20% of canon queries marked hold-out. Tuning runs exclude hold-out. CI regression gate includes hold-out. Reports show both tuning and hold-out metrics separately. | **P1** | **VERIFIED** | ADR-008 | **NEW** |
| REQ-017 | `aft explain-search` NDJSON command: query_intent, lane_weights, per_lane_candidates, top_10_rrf_scores with provenance, **exact_hit_floor_applied** (per result), **reranker_skipped_reason** (if applicable), ranking_features, graph_context, rerank_delta, context_budget_used. Responds in < 500ms. | P1 | CONDITIONAL | ADR-009 | Revised: add reranker_skipped_reason |
| REQ-018 | Benchmark per-QueryIntent Recall@K, MRR breakdown. CI regression gate at -5% per intent category. | P1 | VERIFIED | ADR-008 | NO CHANGE |
| REQ-019 | `aft_orient`: primary_files, entry_symbols, dependency_symbols, test_hints, config_hints, orientation_summary. | P1 | CONDITIONAL | ADR-010 | NO CHANGE |
| REQ-020 | `aft_impact_delta`: callers_affected, tests_covering_affected, blast_radius, mutation_risk. | P1 | CONDITIONAL | ADR-010 | NO CHANGE |
| REQ-021 | `aft_context_pack`: token-budget-aware packing using aft-tokenizer. tokens_used ≤ budget × 1.10. enrichment_state per item. | P1 | CONDITIONAL (aft-tokenizer API) | ADR-010 | NO CHANGE |

## §9 Embedded Architecture Decision Records

> Only ADRs with v3.1 changes are shown in full. Others are referenced with amendment status.

---

### ADR-001 — Soft SearchPlan with safety-lane fallback (REVISED v3.1)

**Status:** ACCEPTED — revised  
**Context:** v3 safety-lane rule said "FTS5Body weight must not drop below 0.1". If FTS5 is unavailable or feature-gated, this rule leaves the safety lane undefined.  
**Decision:** Safety-lane resolution is a priority chain: FTS5Body (if FTS5 index available) → TrigramBody (if `search_index.ready()`) → DegradedLiteralBodyScan (error-condition fallback only). The active safety lane is recorded in `SearchPlan.active_safety_lane`. Feature flag: `retrieval_intelligence_v2`.  
**Intent weight table** (unchanged from v3): NaturalLanguage: semantic=1.5, FTS5Body=1.0 (safety), FTS5Docs=0.8, FTS5Symbol=0.6, Trigram=0.4. ExactSymbol: FTS5Symbol=3.0, Trigram=2.0, FTS5Body=0.5 (safety floor), semantic=0.4. PathLookup: Trigram=3.0, FTS5Path=2.0, FTS5Body=0.2 (safety floor), semantic=0.1. DiagnosticError: Trigram=2.5, FTS5Body=1.0, FTS5Symbol=0.8, semantic=0.3. Regex: Trigram=3.0, FTS5Body=0.1 (safety floor, documented reason), semantic=0.05 (near-zero, documented). Mixed: all=1.0.  
**Consequences:** + Prevents coding agent hard-depending on FTS5 and breaking non-FTS builds. + Safety lane always provides recall floor. - Three-tier resolution adds a small initialization check.  
**Rollback:** Set `retrieval_intelligence_v2=false`.  
**Invalidation trigger:** If TrigramBody fallback degrades symbol query recall by >10% on benchmark vs FTS5: add a separate symbol-exact fallback path.  
**Critical warning:** DO NOT implement FTS5Body as the only safety option. The TrigramBody fallback must exist.

---

### ADR-003 — Weighted RRF + ExactHitFloor (REVISED v3.1 — formula corrected)

**Status:** ACCEPTED — revised  
**Context:** FIND-014: v3 ExactHitFloor used `max(5, pool_rank)` which allows an exact hit at natural rank 12 to remain at rank 12. FIND-013: RRF rewards consensus, demoting single-lane exact hits.  
**Decision:**

```
ExactHitFloor algorithm (post-RRF, before ranking features):
1. Partition candidates into:
   - Group A: is_exact_hit=true AND (NOT is_generated) AND (NOT is_vendor)
   - Group B: all other candidates
2. Sort Group A by exact_lane_rank ascending (best exact-lane rank first)
3. Sort Group B by rrf_score descending
4. Final pool = Group A ++ Group B
5. Result: all exact hits from non-vendor/non-generated files always appear
   before non-exact candidates, regardless of RRF score

Negative control (mandatory VAL-005b-neg):
  - An exact substring match in a vendor/generated/minified file must NOT
    be promoted above a semantically relevant non-vendor result unless
    strict_exact hint is active.
  - is_vendor and is_generated classification: path heuristics
    (*/vendor/*, */node_modules/*, *generated*, *minified*, *bundled*)
    or explicit IntelligenceConfig file classification.
```

**Consequences:** + Exact symbol hits always surface before semantic hits. + Vendor noise excluded from floor. - Requires is_vendor/is_generated flag on CandidateEntry. - Two-pass sort is O(n) extra work.  
**Rollback:** Remove ExactHitFloor pass; revert to pure RRF order.  
**Invalidation trigger:** If exact hits produce false positives in top-5 (wrong but exact-named symbols surface consistently): add confidence threshold to `is_exact_hit`.

---

### ADR-005 — Context Budget with Graceful Exhaustion and PathOnly/Reranker Rule (REVISED v3.1)

**Status:** ACCEPTED — revised  
**Context:** FIND-015: v3 simultaneously said "PathOnly fallback for exhausted candidates" and "never pass PathOnly to content reranker" — contradictory as written.  
**Decision:**

```
PathOnly resolution rule:
  - Enriched candidates (have context text): eligible for content reranker
  - PathOnly candidates (budget exhausted): stay in final pool, EXCLUDED
    from content reranker input
  - reranker receives only enriched_candidates
  - if enriched_count / rerank_pool_size < rerank_min_enriched_ratio (default 0.5):
      skip content reranker entirely
      append all candidates in pre-rerank order
      emit reranker_skipped_reason = "insufficient_enriched_ratio"
  - if enriched_count == 0: skip reranker unconditionally
  - PathOnly candidate content in final result: "file:line_range [budget_exhausted]"
    or signature if available — never empty string

Diagnostics emitted:
  - context_exhausted: bool
  - unenriched_candidate_count: usize
  - reranker_skipped_reason: Option<String>
```

**Consequences:** + Reranker never receives junk input that makes all candidates look equal. + Makes budget exhaustion visible via diagnostics. - Reranker may be skipped more than expected on small total_tokens budgets (increase budget or reduce rerank_pool_size).  
**Rollback:** If reranker-skipped causes worse results than always-reranking: lower `rerank_min_enriched_ratio` to 0.2.  
**Invalidation trigger:** If benchmark shows skipped reranker consistently outperforms enabled reranker on exhausted pools: make reranker always optional and always exclude PathOnly.

---

### ADR-009 — Retrieval Telemetry with Query Hash Default (REVISED v3.1)

**Status:** ACCEPTED — revised  
**Context:** R1 review: raw query text stored by default, but queries can contain secrets, customer names, pasted stack traces, proprietary paths.  
**Decision:** Default telemetry stores SHA-256 hash of query text in `retrieval_runs.query_hash`, not raw text. `retrieval_runs.query_raw` column: populated only when `telemetry_store_query=raw` (explicit opt-in). `why-missed` command re-runs the query live using the original request rather than reading raw query text from telemetry. Snippet text not stored (`no_snippet_persist=true`). File paths stored (needed for diagnostics).  
**Privacy boundary summary:**

| Data | Default | Opt-in |
|---|---|---|
| Query hash | Stored | — |
| Query raw text | Not stored | `telemetry_store_query=raw` |
| File paths | Stored | — |
| Snippet/code text | Not stored | `no_snippet_persist=false` |
| RRF scores/ranks | Stored | — |

**Rollback:** Set `telemetry_persist=false` to disable all writes.  
**Invalidation trigger:** If `why-missed` command needs raw query for non-interactive mode (e.g., batch analysis): add `why-missed --query-file` parameter to bypass telemetry lookup.

---

### ADR-010 — Agent Orientation Commands as Public Retrieval-Intelligence APIs (NEW)

**Status:** ACCEPTED  
**Context:** REQ-019–021 add three new public NDJSON commands (`aft_orient`, `aft_impact_delta`, `aft_context_pack`). These are high-impact API contracts with graph dependencies, token budget behavior, and likely bridge/plugin exposure. They were not covered by an ADR in v3.  
**Decision:** Orientation tools are implemented as NDJSON command handlers in the same epic as URFK, sequenced after Milestone 2 (Track 6). They depend on URFK (SearchPlan, ContextBudget, graph enrichment) and must not be implemented until the core retrieval kernel is stable and verified.  
**Why same epic:** User instruction: scope retained. Track 6 boundary in JSONL provides operational separation without a second epic.  
**Source-conditional items:** NDJSON dispatcher registration path, aft-tokenizer token-count function name — both must be confirmed in Phase 0 (t0 bead AC-7, AC-8) before t6c begins.  
**Orientation summary approach:** Deterministic template, not LLM-generated. Format: "{top_symbol} is implemented in {top_file} via {top_callee_or_pattern}. {second_context}."  
**Consequences:** + Tools have clean dependency chain; they cannot accidentally run before URFK is stable. + Source-conditional items are explicitly gated. - Track 6 is late in the epic; orientation tools are the last delivered capability.  
**Rollback:** If orientation tools prove too complex for their source-conditional items: extract to a follow-on epic and ship as a patch.  
**Invalidation trigger:** If `aft_orient` is requested as a lightweight wrapper over existing tools before full URFK: create a thin `aft_orient_lite` command in a separate spike.

---

### ADR-011 — Retrieval Intelligence Rollout Stages (NEW)

**Status:** ACCEPTED  
**Context:** R1 review: without explicit rollout stages, `retrieval_intelligence_v2` remains permanently hidden and never becomes the default path, defeating the purpose of building it.  
**Decision:**

```
Stage A (during Tracks 1–3): compiled but unavailable
  - retrieval_intelligence_v2 not yet in IntelligenceConfig
  - No user impact

Stage B (Milestone 1 complete): opt-in via retrieval_intelligence_v2=true
  - Feature flag in IntelligenceConfig, default=false
  - Manual activation for testing and benchmarking

Stage C (Milestone 2 + gates pass): default-on for agent_deep profile only
  - Gate: Recall@10 per intent ≥ Phase-0 baseline in benchmark
  - Gate: NFR-001b latency satisfied
  - Gate: ExactHitFloor VAL-005b passes
  - Gate: vfy2 verification bead closed with no CRITICAL findings
  - Toggle: retrieval_intelligence_agent_deep_default=true

Stage D (Milestone 3 + gates pass): default-on for all agent search
  - Gate: vfy3 closed with no CRITICAL findings
  - Gate: NFR-001 (agent_fast) satisfied
  - Gate: INV-001, INV-002 verified via grep regression suite
  - Toggle: retrieval_intelligence_v2_default=true
```

**Consequences:** + Prevents permanent hidden capability. + Stage gates prevent premature default-on causing regressions. - Stage C requires a new config toggle.  
**Rollback:** Revert any stage by setting the appropriate toggle to false.  
**Invalidation trigger:** If benchmark shows retrieval_intelligence_v2 is worse on some query families after Milestone 2: stay at Stage B, investigate per-intent regression.

---

### ADRs with no v3.1 changes (status reference only)

- **ADR-002** (Lane provenance with CandidateProvenance): NO CHANGE
- **ADR-004** (Ranking features, intent-conditioned): NO CHANGE. Vendor/minified classification note added to impl hints.
- **ADR-006** (Optional rerank): NO CHANGE
- **ADR-007** (Graph enrichment from existing RIL/callgraph): NO CHANGE. Graph inferred hints still deferred DEF-014.
- **ADR-008** (Benchmark ablation split): NO CHANGE except REQ-016 amendment for `reranker_skipped_reason`.

---

## §10 Risk Register (v3.1 additions)

| RISK ID | Risk | Severity | Probability | Mitigation | Kill Criterion |
|---|---|---|---|---|---|
| RISK-001 | Hard intent router implementation | CRITICAL | MEDIUM | ADR-001 warning; VAL-003 negative control | If detected in review: revise before continuing |
| RISK-002 | Context enrichment runs over wrong pool | CRITICAL | HIGH | VAL-009 root-cause test | Rerank candidates with empty snippets = rollback |
| RISK-003 | Benchmark defaults to benchmark_enriched | HIGH | MEDIUM | INV-006; VAL-015 | Fail CI if mode=benchmark_enriched in standard run |
| RISK-004 | Test penalty without intent conditioning | HIGH | MEDIUM | ADR-004; VAL-007 dual-case | If test files disappear from test queries: disable |
| RISK-005 | Graph enrichment adds >100ms to agent_fast | MEDIUM | MEDIUM | Async/limit top-3 in agent_fast | If >100ms: make graph opt-in for agent_fast |
| RISK-006 | sqlite-vec premature (pre-v1) | HIGH | LOW if ADR respected | DEF-002; never ship as default pre-v1 | Index corruption: immediate rollback |
| RISK-007 | Phase 0 branch divergence | MEDIUM | MEDIUM | t0 is blocking bead | If diverged >10%: stop, revise PRD |
| RISK-008 | Telemetry disk pressure | LOW | LOW | REQ-014b retention; aft telemetry prune | If >100MB: trigger auto-prune |
| RISK-009 | Stale vector vs fresh FTS5 contradictory candidates | MEDIUM | MEDIUM | Lane freshness in diagnostics; freshness warning | If contradictory top results persist: add freshness diagnostic |
| RISK-010 | **RRF demotes exact hits (FIND-013)** | HIGH | HIGH without floor | ADR-003 ExactHitFloor; VAL-005b | Exact hits below rank 5: verify floor applied |
| RISK-011 | **Budget exhaustion sends PathOnly to reranker** | HIGH | MEDIUM | REQ-009b revised; ADR-005 revised | PathOnly in reranker: immediate ADR-005 rollback |
| **RISK-012** | **ExactHitFloor promotes vendor/minified noise to top-5** | **HIGH** | **MEDIUM** | **REQ-005b negative control; VAL-005b-neg** | **If vendor files consistently rank top-5 on symbol queries: add is_vendor flag gate** |
| **RISK-013** | **TrigramAdapter typo propagates into Rust identifiers if not corrected before implementation** | **HIGH** | **HIGH (already in v3 JSONL)** | **All PRD and JSONL occurrences corrected in v3.1; JSONL must be regenerated** | **If typo found in code after merge: immediate rename refactor** |
| **RISK-014** | **`retrieval_intelligence_v2` stays permanently hidden without ADR-011 rollout stages** | **MEDIUM** | **HIGH without ADR-011** | **ADR-011 Stage A→D gates** | **If Stage B not reached by Milestone 2: investigate flag gate** |
| **RISK-015** | **Benchmark overfitting: ranking features tuned against visible canon; hold-out queries not tracked** | **HIGH** | **HIGH without REQ-016b** | **REQ-016b hold-out set; CI gate uses hold-out** | **If hold-out recall drops while tuning recall rises: ranking features are overfit** |

---

## §11 Roadmap (v3.1 milestone ordering unchanged)

| ROAD ID | Milestone | Timing | Blocking Gate | ADR-011 Stage |
|---|---|---|---|---|
| ROAD-001 | Phase 0: Source baseline. Branch verified. Benchmark schema committed. SHA recorded. | Week 1 | YES — blocks all | Stage A begins |
| ROAD-002 | **Milestone 1**: Root Cause + Benchmark Honesty (Tracks 1, 3 elevated, 5a/5b) | Weeks 2–6 | YES — blocks MS2. Gate: VAL-009 root cause + VAL-015 benchmark honesty + vfy1 no CRITICAL | Stage B: opt-in enabled |
| ROAD-003 | **Milestone 2**: Full URFK + Ranking (Tracks 2, 4, 5c) | Weeks 5–10 | YES — blocks MS3. Gate: VAL-005b ExactHitFloor + NFR-001 latency + Recall@10 ≥ baseline + vfy2 no CRITICAL | Stage C: agent_deep default-on |
| ROAD-004 | **Milestone 3**: Graph + Orientation (Track 6) | Weeks 9–14 | NO (parallel with MS2 end). Gate: orientation tools e2e + graph null-safe + vfy3 no CRITICAL | Stage D: all profiles default-on |
| ROAD-005 | Backend Spikes (benchmark-gated, separate) | Ongoing after MS2 | NO — never on critical path | N/A |

---

## §12 Implementation Plan (v3.1 corrections)

> Corrections from v3: all `TrigamAdapter` → `TrigramAdapter`. IMPL-010 (FTS5Adapter) through IMPL-013 (TrigramAdapter) have adapter name corrections. All others unchanged.

**Track ordering (revised from v3):**  
Track 0 → Track 1 → [Track 3 elevated PARALLEL Track 2] → Track 5 early → VFY1 → MS1 → [Track 2 remaining] → Track 4 → Track 5c → VFY2 → MS2 → Track 6 → VFY3 → MS3.

| IMPL ID | Track | Slice | Depends On | Milestone |
|---|---|---|---|---|
| IMPL-001 | TRACK 0 | Source baseline, branch verify, commit SHA (SRC-000), commit benchmark schema | — | ROAD-001 |
| IMPL-002 | TRACK 1 | QueryIntent enum, soft SearchPlan types, safety-lane fallback chain, active_safety_lane field | IMPL-001 | ROAD-002 |
| IMPL-003 | TRACK 1 | CandidateProvenance, LaneContribution, FusedCandidate, CandidateSet, is_exact_hit flag, is_vendor/is_generated flag | IMPL-002 | ROAD-002 |
| IMPL-004 | TRACK 1 | Vertical wiring slice: SearchPlan → aft_search diagnostics field, feature-flagged off | IMPL-003 | ROAD-002 |
| IMPL-005 | TRACK 3 (elevated) | ContextBudget struct, ContextMode, EnrichPool, exhaustion behavior, PathOnly fallback rule, replace `snippet_line_budget()` | IMPL-002 | ROAD-002 |
| IMPL-006 | TRACK 3 (elevated) | ROOT CAUSE: `enrich_context_pool()` BEFORE `rerank_candidates()`; PathOnly excluded from content reranker; `reranker_skipped_reason` emitted | IMPL-005 | ROAD-002 |
| IMPL-007 | TRACK 3 | Default profiles; `context_exhausted`, `unenriched_candidate_count`, `reranker_skipped_reason` in NDJSON | IMPL-006 | ROAD-002 |
| IMPL-008 | TRACK 5a | Benchmark `--rerank-context` flag; `aft_output` default; `reranker_skipped_reason` in JSON output | IMPL-006 | ROAD-002 |
| IMPL-009 | TRACK 5b | Benchmark context quality diagnostics (incl. `reranker_skipped_reason`, hold-out split per REQ-016b) | IMPL-008 | ROAD-002 |
| IMPL-010 | TRACK 2 | FTS5Adapter (wraps `fts5_planner.rs`; sets `is_exact_hit=true` for exact symbol matches) | IMPL-004 | ROAD-003 |
| IMPL-011 | TRACK 2 | SemanticAdapter (wraps VectorStore; `is_exact_hit=false` always) | IMPL-010 | ROAD-003 |
| IMPL-012 | TRACK 2 | **TrigramAdapter** (corrected; wraps `search_index.rs`; `is_exact_hit=true` for literal exact matches) | IMPL-010 | ROAD-003 |
| IMPL-013 | TRACK 2 | Weighted RRF fusion + ExactHitFloor (corrected formula: Group A before Group B) + dedup by canonical identity (chunk_id priority chain) | IMPL-010,011,012 | ROAD-003 |
| IMPL-014 | TRACK 2 | Wire URFK into `aft_search` behind `retrieval_intelligence_v2` flag | IMPL-013 | ROAD-003 |
| IMPL-015 | TRACK 4 | Telemetry tables with query-hash-default, `no_snippet_persist=true`, retention, pruning (ADR-009 revised) | IMPL-014 | ROAD-003 |
| IMPL-016 | TRACK 4 | `aft explain-search` + `aft why-missed` commands (why-missed re-runs query live, does not read raw telemetry) | IMPL-015 | ROAD-003 |
| IMPL-017 | TRACK 4 | Deterministic ranking features (6, intent-conditioned, vendor/generated aware) | IMPL-013 | ROAD-003 |
| IMPL-018 | TRACK 5c | Per-QueryIntent Recall@K; CI regression gate (-5%); hold-out set (REQ-016b) | IMPL-009 | ROAD-003/4 |
| IMPL-019 | TRACK 6 | Graph enrichment (direct facts: callers, callees, imported_by, mutation_risk; no inferred hints) | IMPL-014 | ROAD-004 |
| IMPL-020 | TRACK 6 | Graph expansion candidates (callers/imported_by as secondary URFK candidates; is_graph_expansion=true) | IMPL-019 | ROAD-004 |
| IMPL-021 | TRACK 6 | `aft_orient`, `aft_impact_delta`, `aft_context_pack` orientation tools | IMPL-020 | ROAD-004 |
| IMPL-021b | TRACK 6 (deferred within track) | Graph inferred hints: test_coverage_hint, config_owner inference | IMPL-019 | DEF-014 |
| IMPL-022 | SPIKE | sqlite-vec optional backend (after v1.0) | ROAD-003 complete | ROAD-005 |
| IMPL-023 | SPIKE | Tantivy optional lexical sidecar (after FTS5 proves insufficient) | ROAD-003 complete | ROAD-005 |

## §13 Validation Case Ledger (v3.1 — changes marked)

| VAL ID | Requirement | Test | Method | Negative Control | v3.1 Change |
|---|---|---|---|---|---|
| VAL-001 | REQ-001 | Benchmark runs; schema committed; SHA recorded | Run benchmark; check schema file | Fail: benchmark crashes or SHA absent | Revised: SHA check |
| VAL-002 | REQ-002 | SearchPlan serializes with all fields per §A.1 schema | Unit test for all fields | Fail: any field missing | NO CHANGE |
| VAL-002b | REQ-002b | Vertical slice: NDJSON semantic_search → `search_plan_debug` field present | Integration test flag=on | Fail: no diagnostics field | NO CHANGE |
| VAL-003 | REQ-003 | NaturalLanguage query: FTS5Body weight ≥ 0.1 in SearchPlan; safety lane recorded in `active_safety_lane` | Assert weight and field | Fail: FTS5Body < 0.1 or `active_safety_lane` absent | Revised: `active_safety_lane` field |
| VAL-003b | REQ-003 | With FTS5 disabled: `active_safety_lane = TrigramBody`; body coverage maintained | Integration test with FTS5 disabled | Fail: no safety lane when FTS5 disabled | **NEW** |
| VAL-004 | REQ-004 | Each adapter (**TrigramAdapter** corrected) returns CandidateSet with correct `source_lane` | Unit test per adapter | Fail: wrong `source_lane` | Revised: typo fix |
| VAL-005 | REQ-005 | Fused candidates carry CandidateProvenance with all contributing lanes | Unit test: two-lane overlap → `provenance.lanes.len()==2` | Fail: single-lane provenance for multi-lane candidate | NO CHANGE |
| VAL-005b | REQ-005b + INV-007 | Exact symbol query: exact hit from non-vendor/non-generated file appears in top-5 | Integration test on known symbol | Fail: exact hit at position 6+ | **REVISED (v3.1 critical)** |
| **VAL-005b-neg** | REQ-005b | **Exact substring match in vendor/minified file does NOT appear in top-5 on a symbol query unless `strict_exact` active** | **Integration test on vendor file with substring match** | **Pass: vendor file below position 5** | **NEW (negative control)** |
| VAL-005c | REQ-005c | Same file+line_range from two lanes → one FusedCandidate with `provenance.lanes.len()==2` | Unit test: FTS5Body + Semantic same location | Fail: two separate candidates | NO CHANGE |
| VAL-005d | REQ-005c | `chunk_id` takes priority over `symbol_id` and `path+line_range` for dedup | Unit test: three candidates same chunk_id | Fail: dedup by lower-priority key | **NEW** |
| VAL-006 | REQ-006 | Lane weights follow documented intent table; safety lane resolved correctly | Unit test | Fail: weights inverted | NO CHANGE |
| VAL-007 | REQ-007 | TestExamplePenalty disabled for DiagnosticError; enabled for ExactSymbol non-test query | Two integration tests | Fail: test files penalized on test-intent query | NO CHANGE |
| VAL-008 | REQ-008 | ContextBudget applied; total_tokens respected within 10% | Integration test | Fail: total exceeded >10% | NO CHANGE |
| VAL-009 | REQ-009 ROOT CAUSE | With rerank enabled: `snippet_count + unenriched_candidate_count == rerank_pool_size` in diagnostics BEFORE rerank | Assert diagnostic fields | FAIL = critical regression | NO CHANGE |
| VAL-009b | REQ-009b | Budget exhaustion: `context_exhausted=true`; PathOnly fallback non-empty; `reranker_skipped_reason` present if ratio < min_ratio | Integration test with small budget | Fail: empty snippet or PathOnly reaching reranker | **REVISED: reranker_skipped_reason added** |
| **VAL-009c** | **REQ-009b** | **PathOnly candidates are excluded from content reranker input** | **Integration: inspect reranker candidates; assert no PathOnly entries** | **Fail: PathOnly candidate in reranker input** | **NEW (contradiction resolution)** |
| **VAL-009d** | **REQ-009b** | **When enriched_count / rerank_pool_size < 0.5: reranker skipped; `reranker_skipped_reason = "insufficient_enriched_ratio"` in diagnostics** | **Integration: force small budget with large pool; check diagnostics** | **Fail: reranker invoked with <50% enriched candidates** | **NEW** |
| VAL-010 | REQ-010 | Profile `agent_fast` → correct fields; `symbol_exact` → Signature mode; `agent_deep` → RerankPool | Unit tests | Fail: profile mismatch | NO CHANGE |
| VAL-011 | REQ-011 | `graph_context.callers` populated for known function when GraphHealth=Healthy; no inferred hints present | Integration test | Fail: `graph_context` absent or has inferred hints | NO CHANGE |
| VAL-012 | REQ-012 | `graph_context` null (not error) when graph Disabled | Integration test | Fail: error or empty object | NO CHANGE |
| VAL-013 | REQ-013 | PathOnly mode returns only path+line_range | Unit test | Fail: body text in PathOnly | NO CHANGE |
| VAL-014 | REQ-014 | `retrieval_runs` row created per search with diagnostics=summary; zero with off | Integration test + SQLite count | Fail: rows with off | NO CHANGE |
| **VAL-014b** | **REQ-014b** | **Default run: `retrieval_runs.query_hash` populated; `query_raw` column NULL** | **Integration test; SELECT query_raw FROM retrieval_runs → NULL** | **Fail: raw query text stored by default** | **NEW (query hash default)** |
| **VAL-014c** | **REQ-014b** | **With `telemetry_store_query=raw`: `query_raw` column populated** | **Integration test with opt-in flag** | **Fail: query_raw NULL when opt-in active** | **NEW** |
| VAL-015 | REQ-015 | Default benchmark run: `rerank_context=aft_output` in JSON | Run benchmark; jq `.rerank_context` | Fail: benchmark_enriched as default | NO CHANGE |
| VAL-016 | REQ-016 | Benchmark JSON has context_quality block with all fields including `reranker_skipped_reason` | JSON schema check | Fail: field missing | Revised: reranker_skipped_reason |
| **VAL-016b** | **REQ-016b** | **Benchmark JSON shows separate tuning and hold-out metrics; CI gate uses hold-out queries** | **Run benchmark; check hold-out keys in output; verify CI script uses hold-out** | **Fail: hold-out metrics absent or CI uses only tuning queries** | **NEW** |
| VAL-017 | REQ-017 | `explain-search` returns all fields including `exact_hit_floor_applied` and `reranker_skipped_reason` in <500ms | Run command; check fields; time it | Fail: field absent or >500ms | Revised: reranker_skipped_reason |
| VAL-018 | REQ-018 | Benchmark has per-intent Recall@K; CI exits 1 on -5% regression | Synthetic regression test | Fail: CI passes on -10% regression | NO CHANGE |
| VAL-019 | REQ-019 | `aft_orient` returns non-empty `primary_files`, `orientation_summary` in <500ms | Integration test | Fail: empty or crash | NO CHANGE |
| VAL-020 | REQ-020 | `aft_impact_delta`: `callers_affected` non-empty for known function | Integration test | Fail: empty blast radius | NO CHANGE |
| VAL-021 | REQ-021 | `aft_context_pack`: `tokens_used` ≤ budget × 1.10 | Integration test; count tokens | Fail: budget exceeded >10% | NO CHANGE |

---

## §14 Traceability Matrix (v3.1 additions in bold)

| Finding / Review → Requirement | ADR | Implementation | Validation |
|---|---|---|---|
| FIND-001 → REQ-003 | ADR-001 | IMPL-002 | VAL-003 |
| FIND-002 → REQ-004 | ADR-002 | IMPL-010 | VAL-004 |
| FIND-003 → REQ-002 | ADR-001 | IMPL-002 | VAL-002 |
| FIND-004 → REQ-005 | ADR-002, ADR-003 | IMPL-013 | VAL-005 |
| FIND-005 → REQ-009 ROOT CAUSE | ADR-005 | IMPL-006 | VAL-009 CRITICAL |
| FIND-006 → REQ-008 | ADR-005 | IMPL-005 | VAL-008 |
| FIND-007 → REQ-015 | ADR-008 | IMPL-008 | VAL-015 |
| FIND-008 → REQ-011, 012 | ADR-007 | IMPL-019 | VAL-011, 012 |
| FIND-010 → REQ-014 | ADR-009 | IMPL-015 | VAL-014 |
| FIND-011 → ADR-006 | ADR-006 | IMPL-014 | VAL-009 |
| FIND-012 → REQ-013 | ADR-005 | IMPL-005 | VAL-013 |
| **FIND-013 → REQ-005b, INV-007** | **ADR-003** | **IMPL-013** | **VAL-005b CRITICAL** |
| **FIND-014 → REQ-005b revised** | **ADR-003 revised** | **IMPL-013 corrected formula** | **VAL-005b, VAL-005b-neg** |
| **FIND-015 → REQ-009b revised** | **ADR-005 revised** | **IMPL-006 PathOnly rule** | **VAL-009b, VAL-009c, VAL-009d** |
| **FIND-016 → REQ-004 (TrigramAdapter)** | **ADR-002** | **IMPL-012 corrected** | **VAL-004** |
| R1-04 → SRC-000 | — | IMPL-001 | VAL-001 |
| R1-05 → REQ-003, ADR-001 | ADR-001 | IMPL-002 | VAL-003b |
| R1-06 → ADR-011 | ADR-011 | All milestones | ROAD-002-004 gates |
| R1-07, R2-03 → REQ-005c | ADR-002 | IMPL-013 | VAL-005d |
| R1-08, R2-11 → REQ-016b | ADR-008 | IMPL-018 | VAL-016b |
| R1-09, R2-12 → REQ-014b revised | ADR-009 revised | IMPL-015 | VAL-014b, VAL-014c |
| R1-10 → ADR-010 | ADR-010 | IMPL-021 | VAL-019-021 |
| R1-12, R2-10 → NFR-001 | — | IMPL-014 gate | Benchmark latency column |
| R2-01 → ACTOR/STORY section | — | — | Context for all Beads |
| R2-02 → REQ-005b negative control | ADR-003 | IMPL-013 | VAL-005b-neg CRITICAL |
| R2-04 → REQ-002 schema | ADR-001 | IMPL-002 | VAL-002 |
| R2-05 → NFR-001–005 | — | All milestones | Benchmark/integration gates |
| R2-06 → SEC-001–005 | ADR-009 | IMPL-015 | VAL-014b, VAL-014c |
| R2-07 → OPS-001, OPS-005 (IMPL notes) | — | IMPL-016 (explain-search degraded lanes) | VAL-017 |

---

## §15 Deferred Scope (v3.1 additions)

| DEF ID | Item | Revisit Trigger |
|---|---|---|
| DEF-001 | Build/test intelligence (cargo_metadata, npm graph, RIG export) | Agent benchmarks show build/test is top miss class after Phase 3 |
| DEF-002 | sqlite-vec as default backend | sqlite-vec v1.0 stable AND FlatF32 latency fails |
| DEF-003 | Tantivy sidecar | FTS5 Recall@10 < 85% of trigram on symbol queries after ms2 |
| DEF-004 | AQE-lite deterministic query alternatives | Confuser query miss rate > 20% after Phase 4 |
| DEF-005 (PERM REJECT) | snapvec | Python-native, unusable from Rust binary |
| DEF-006 | LanceDB/Qdrant sidecar | Corpus > 500K chunks AND FlatF32+sqlite-vec insufficient |
| DEF-007 | Native MCP server | URFK + orientation tools proven; then expose as MCP |
| DEF-008 | SCIP/LSP graph import | Syntactic graph proven insufficient for agent benchmarks |
| DEF-011 | Vendor/minified file classification beyond path heuristics | After v1 ranking features benchmarked and RISK-012 assessed |
| DEF-012 | Query-intent ambiguity score in explain-search | After explain-search stable |
| DEF-013 | Cross-repo/multi-root workspaces | After v1 single-repo complete |
| DEF-014 | Graph inferred hints (test_coverage_hint, config_owner) | After P1 direct graph facts validated in agent benchmarks |
| **DEF-015** | **OPS doctor command (OPS-002/003/004); DOC section** | **After Milestone 3; separate documentation/ops track** |

---

## §16 OPS Implementation Notes (v3.1 — from R2-07)

> These are implementation notes for v1 beads, not blocking requirements. OPS-001 and OPS-005 are implemented as part of Track 4 (explain-search). OPS-002–004 are deferred to DEF-015.

**OPS-001 (implement in Track 4):** `aft explain-search` already serves as the primary diagnostic surface. It should report the current state of: feature flag, schema version, index freshness per lane (FTS5 / vector / callgraph), and active safety lane.

**OPS-005 (implement in Track 4):** `aft explain-search` must report degraded lanes rather than failing silently. If a lane times out or returns an error, the response includes `degraded_lanes: [{ lane, reason, fallback_used }]`.

---

## §17 Downstream Beads Handoff Contract (v3.1)

### Source status

SOURCE-VERIFIED for: `semantic_search.rs`, `query_shape.rs`, `config.rs`, `semantic_rerank.rs`, `fts5_planner.rs`, `fts5_store.rs`, `vector_store.rs`, `callgraph.rs`, `mutation_risk.rs`, `ril_indexer.rs`, `observability_ledger.rs`, `benchmarks/semble/pilot.ts`.

SOURCE-CONDITIONAL for: NDJSON dispatcher registration path, `aft-tokenizer` token-count function name, orientation command registration, exact graph inferred hint schema. Phase 0 bead (IMPL-001) resolves all conditional items before Track 6 begins.

**Pinned commit:** Branch is mutable. Phase 0 commits HEAD SHA to `benchmarks/baseline/schema-2026-06-18.json` and to repo as SRC-000. All implementation beads execute against this pinned revision.

### Critical warnings — verbatim in all Beads

> ⚠ **WARNING 1:** DO NOT implement a hard query intent router. QueryIntent changes lane WEIGHTS only. body/content safety lane weight must never drop below 0.1 in auto-mode.

> ⚠ **WARNING 2 (ROOT CAUSE):** DO NOT run context enrichment after `rerank_candidates()`. `enrich_context_pool()` MUST execute over the full rerank pool BEFORE `rerank_candidates()`.

> ⚠ **WARNING 3:** DO NOT use `benchmark_enriched` mode as the default benchmark path. `aft_output` must be the default. If `benchmark_enriched` beats `aft_output`, that is an engine bug report.

> ⚠ **WARNING 4:** DO NOT apply ExactHitFloor inside RRF computation. It is a POST-RRF partition (Group A exact hits before Group B non-exact hits). The formula is NOT `max(5, pool_rank)`.

> ⚠ **WARNING 5:** DO NOT apply test/example/stub/generated penalty when QueryIntent requests tests. Intent-conditioning on penalty is non-negotiable.

> ⚠ **WARNING 6:** DO NOT let graph expansion hide or replace direct-hit provenance. Expansion candidates carry `is_graph_expansion=true`.

> ⚠ **WARNING 7:** DO NOT introduce remote service dependencies as required runtime path.

> ⚠ **WARNING 8:** DO NOT pass PathOnly candidates to the content reranker. PathOnly candidates stay in the final pool but are excluded from content reranker input. If enriched ratio < `rerank_min_enriched_ratio` (default 0.5), skip reranker entirely.

> ⚠ **WARNING 9 (NEW):** DO NOT use `TrigamAdapter` as the struct name. The correct spelling is **`TrigramAdapter`**.

> ⚠ **WARNING 10 (NEW):** DO NOT apply ExactHitFloor to candidates from vendor, minified, or generated files (where `is_vendor=true` or `is_generated=true`). Vendor noise must not surface to top-5 via the floor mechanism.

> ⚠ **WARNING 11 (NEW):** DO NOT store raw query text in telemetry by default. Default is `telemetry_store_query=hash` (SHA-256 of query). Raw text requires explicit opt-in `telemetry_store_query=raw`.

### Milestone closure gates

**Milestone 1** (blocks MS2): VAL-009 root cause confirmed (`snippet_count + unenriched == rerank_pool_size`); VAL-009c PathOnly excluded from reranker; VAL-015 benchmark `aft_output` default; vfy1 no CRITICAL findings; grep tests unchanged.

**Milestone 2** (blocks MS3): VAL-005b ExactHitFloor working e2e; VAL-005b-neg vendor noise excluded; NFR-001 latency gate; Recall@10 per intent ≥ baseline; VAL-014b query hash default; vfy2 no CRITICAL findings.

**Milestone 3** (epic closure): orientation tools e2e; VAL-011 graph context populated; VAL-011 no inferred hints; VAL-016b hold-out metrics in benchmark; vfy3 no CRITICAL findings; ADR-011 Stage D gate satisfied.

### Beads epic structure (one epic, six tracks, three verifications, three milestones, two spikes)

```
aft-ri-v31 (root epic)
  t0   Source baseline — BLOCKING
  t1a  QueryIntent + SearchPlan types + safety-lane fallback chain
  t1b  CandidateProvenance + FusedCandidate + is_vendor/is_generated flag
  t1c  Vertical wiring slice
  t3a  ContextBudget model; PathOnly/reranker rule; replace snippet_line_budget()
  t3b  ROOT CAUSE: enrich_context_pool() BEFORE rerank_candidates()
  t3c  Context profiles + reranker_skipped_reason in NDJSON
  t5a  Benchmark --rerank-context flag; aft_output default
  t5b  Context quality diagnostics + hold-out schema (REQ-016b)
  vfy1 Verification gate 1 → blocks MS1
  ms1  Milestone 1: Root Cause + Benchmark Honesty
  t2a  FTS5Adapter (is_exact_hit for exact symbol hits)
  t2b  SemanticAdapter
  t2c  TrigramAdapter [NOTE: CORRECTED SPELLING from v3 TrigamAdapter]
  t2d  Weighted RRF fusion + ExactHitFloor (corrected partition formula) + dedup (chunk_id priority)
  t2e  Wire URFK into aft_search; ADR-011 Stage B
  t4a  Telemetry tables + query-hash-default + retention + pruning
  t4b  aft explain-search + aft why-missed (why-missed re-runs live)
  t4c  Ranking features (is_vendor/is_generated aware)
  t5c  Per-intent Recall@K + CI gate + hold-out split
  vfy2 Verification gate 2 → blocks MS2
  ms2  Milestone 2: Full URFK + Ranking; ADR-011 Stage C
  t6a  Graph enrichment (direct facts only; no inferred hints)
  t6b  Graph expansion candidates
  t6c  aft_orient + aft_impact_delta + aft_context_pack
  vfy3 Verification gate 3 → blocks MS3
  ms3  Milestone 3: Graph + Orientation; ADR-011 Stage D
  spk1 SPIKE: sqlite-vec (deferred, after v1.0)
  spk2 SPIKE: Tantivy (deferred, after FTS5 benchmark)
```

**Key v3.1 Bead changes from v3 JSONL:**
- All `TrigamAdapter` occurrences → `TrigramAdapter` (IMPL-012, t2c bead, all test names)
- t2d: ExactHitFloor formula corrected (Group A/B partition, not max formula)
- t3b: PathOnly/reranker contradiction resolved (exclude PathOnly from reranker; reranker_skipped_reason)
- t3a: PathOnly rule explicitly part of ContextBudget model
- t4a: query-hash-default; `query_raw` column opt-in only
- t5b: REQ-016b hold-out schema added
- t1b: `is_vendor`, `is_generated` flags on CandidateEntry (needed by ExactHitFloor)
- t2d: `is_vendor`, `is_generated` used in ExactHitFloor Group A filter

---

## §18 Reflection Review

**Verdict:** Pass with caveats — two caveats remain after v3.1 fixes.

### Findings resolved in v3.1

| Severity | Finding | Fix applied |
|---|---|---|
| CRITICAL | ExactHitFloor formula wrong (`max(5,pool_rank)`) | ADR-003 revised: Group A/B partition. REQ-005b, VAL-005b-neg |
| CRITICAL | PathOnly/reranker contradiction | ADR-005 revised: PathOnly excluded from reranker; `reranker_skipped_reason`. REQ-009b |
| CRITICAL | TrigramAdapter typo propagates into code | All occurrences corrected; RISK-013; Beads handoff WARNING 9 |
| HIGH | No pinned commit SHA in source ledger | SRC-000 added; IMPL-001 records SHA; REQ-001 revised |
| HIGH | Safety lane undefined when FTS5 unavailable | ADR-001 revised: TrigramBody fallback chain; REQ-003; VAL-003b |
| HIGH | No rollout ADR; flag stays permanently hidden | ADR-011: Stage A→D; ASSUMP-010; RISK-014 |
| HIGH | Orientation tools have no ADR | ADR-010 added |
| HIGH | No ACTOR/STORY section | §2 added |
| HIGH | NFR section missing; latency gates scattered | §6 NFR-001–005 added |
| HIGH | SEC section missing; query stored raw by default | §7 SEC-001–005; ADR-009 revised; REQ-014b revised |
| HIGH | Hold-out benchmark only an IMPL note | REQ-016b; VAL-016b |
| HIGH | Candidate identity priority chain incomplete | REQ-005c revised: chunk_id priority, content hash fallback; VAL-005d |
| MEDIUM | SearchPlan missing schema contract | REQ-002 revised with schema table reference; §A.1 |
| MEDIUM | Vendor/minified ExactHitFloor false positives | REQ-005b negative control; VAL-005b-neg; RISK-012; WARNING 10 |

### Residual accepted risks

| Risk | Reason accepted | Revisit trigger |
|---|---|---|
| RISK-007 branch divergence | Phase 0 bead is blocking; resolved before implementation | If divergence detected in Phase 0: stop, revise PRD |
| RISK-009 stale vector vs fresh FTS5 | Lane freshness diagnostic added as OPS-001 note in Track 4 | If contradictory top results persist: add freshness warning |
| SOURCE-CONDITIONAL for orientation commands | Phase 0 confirms dispatcher path before Track 6 | If Phase 0 reveals incompatible registrations: architecture bead before t6c |
| ADR-011 Stage C/D timing | Stage gates are explicit; benchmark evidence required | If Stage B benchmark shows regression: stay at Stage B |

---

## §A Appendix — Type Schemas

### §A.1 SearchPlan Schema Contract

```rust
pub struct SearchPlan {
    pub intent: QueryIntent,
    pub lane_weights: HashMap<LaneKind, f32>,
    pub mandatory_lanes: Vec<LaneKind>,       // never skipped regardless of weight
    pub suppressed_lanes: Vec<SuppressedLane>, // explicitly suppressed with reason
    pub prefetch: Vec<RetrieverPlan>,
    pub fusion: FusionPlan,
    pub ranking_profile: RankingProfile,
    pub context_budget: ContextBudget,
    pub rerank: RerankPlan,
    pub diagnostics: DiagnosticLevel,
    pub active_safety_lane: LaneKind,         // FTS5Body | TrigramBody | DegradedLiteralBodyScan
    pub feature_flag_state: FeatureFlagState, // for diagnostics
}

pub struct SuppressedLane {
    pub lane: LaneKind,
    pub reason: String,  // e.g., "near-zero for Regex intent"
}

pub struct RetrieverPlan {
    pub lane: LaneKind,
    pub weight: f32,
    pub max_candidates: usize,
    pub is_safety_lane: bool,
    pub latency_budget_ms: Option<u64>,  // if exceeded: emit lane_timeout=true, continue others
}

pub enum QueryIntent {
    NaturalLanguage, ExactSymbol, SymbolPrefix, PathLookup,
    Literal, Regex, DiagnosticError, RelatedCode, Mixed,
}

pub enum LaneKind {
    Trigram, FTS5Symbol, FTS5Body, FTS5Path, FTS5Docs,
    Semantic, SymbolExact, GraphExpansion,
    TrigramBody,           // fallback safety lane when FTS5 unavailable
    DegradedLiteralBodyScan, // last-resort error-condition fallback
}
```

### §A.2 ContextBudget and Exhaustion

```rust
pub struct ContextBudget {
    pub total_tokens: usize,         // default 4000
    pub per_candidate_tokens: usize, // default 300
    pub min_candidate_chars: usize,  // default 80
    pub mode: ContextMode,
    pub enrich_pool: EnrichPool,     // MUST be RerankPool when rerank enabled
    pub rerank_min_enriched_ratio: f32, // default 0.5; below this: skip reranker
}

pub struct ContextBudgetResult {
    pub context_exhausted: bool,
    pub unenriched_candidate_count: usize,
    pub reranker_skipped_reason: Option<String>, // "insufficient_enriched_ratio" | "no_enriched_candidates"
}

pub enum EnrichPool { FinalTopK, FusionPool, RerankPool }
pub enum ContextMode { PathOnly, Signature, SymbolBody, SymbolBodyWithDocs, LineWindow, FileOutline, Auto }
```

### §A.3 CandidateProvenance

```rust
pub struct CandidateProvenance {
    pub lanes: Vec<LaneContribution>,
    pub is_graph_expansion: bool,
    pub graph_expansion_reason: Option<String>,
}

pub struct CandidateEntry {
    pub chunk_id: Option<u64>,      // primary canonical ID
    pub symbol_id: Option<u64>,     // secondary canonical ID
    pub file_path: PathBuf,
    pub line_range: Option<(usize, usize)>,
    pub content_hash: Option<u64>,  // for dedup fallback
    pub score: f32,
    pub rank: usize,
    pub is_exact_hit: bool,
    pub is_vendor: bool,            // ExactHitFloor exclusion flag
    pub is_generated: bool,         // ExactHitFloor exclusion flag
    pub source_lane: LaneKind,
}
```

### §A.4 Telemetry Schema

```sql
-- query_hash: SHA-256 of query text (always stored)
-- query_raw: raw query text (NULL by default; populated only when telemetry_store_query=raw)
CREATE TABLE retrieval_runs (
    run_id INTEGER PRIMARY KEY,
    query_hash TEXT NOT NULL,
    query_raw TEXT,           -- NULL by default
    query_kind TEXT,
    timestamp INTEGER,
    latency_ms INTEGER,
    profile TEXT,
    backend_config TEXT,
    context_exhausted BOOLEAN,
    reranker_skipped_reason TEXT
);

CREATE TABLE candidate_scores (
    run_id INTEGER REFERENCES retrieval_runs(run_id),
    chunk_id INTEGER,
    source_lane TEXT,
    raw_rank INTEGER,
    raw_score REAL,
    normalized_score REAL,
    is_exact_hit BOOLEAN,
    exact_hit_floor_applied BOOLEAN
    -- NOTE: no snippet_text column when no_snippet_persist=true
);

CREATE TABLE fusion_scores (
    run_id INTEGER REFERENCES retrieval_runs(run_id),
    chunk_id INTEGER,
    rrf_score REAL,
    exact_hit_floor_applied BOOLEAN,
    final_score REAL,
    provenance_json TEXT  -- JSON of CandidateProvenance (no snippet content)
);
```

### §A.5 Benchmark Context Quality Schema

```json
{
  "mode": "semantic-fe+rerank",
  "rerank_context": "aft_output",
  "candidate_pool_size": 100,
  "rerank_pool_size": 50,
  "snippet_count": 47,
  "path_only_count": 3,
  "unenriched_candidate_count": 3,
  "avg_doc_tokens": 168,
  "max_doc_tokens": 900,
  "total_doc_tokens": 8400,
  "pre_rerank_recall_at_pool": 0.84,
  "post_rerank_recall_at_k": 0.72,
  "lost_relevant_after_rerank": ["src/retry.rs"],
  "context_exhausted": false,
  "reranker_skipped_reason": null,
  "intent_distribution": { "NaturalLanguage": 12, "ExactSymbol": 8, "PathLookup": 3 },
  "tuning_recall_at_10": 0.78,
  "holdout_recall_at_10": 0.71
}
```

---

## §19 Closeout and Compound Learning

### What v3.1 adds over v3

1. **ExactHitFloor formula corrected** — From `max(5, pool_rank)` (wrong) to Group A/B partition sort. Critical for INV-007.
2. **PathOnly/reranker contradiction resolved** — PathOnly candidates excluded from content reranker. `reranker_min_enriched_ratio` gate. `reranker_skipped_reason` in diagnostics.
3. **TrigramAdapter typo fixed everywhere** — Will no longer propagate into Rust identifiers.
4. **Pinned commit SHA (SRC-000)** — Branch is mutable; commit SHA anchors SOURCE-VERIFIED claims.
5. **Safety lane fallback chain** — FTS5Body → TrigramBody → DegradedLiteralBodyScan. Prevents hard-dependency on FTS5.
6. **ADR-011 rollout stages (A→D)** — `retrieval_intelligence_v2` now has an explicit path from opt-in to default-on.
7. **ADR-010 orientation tools** — Public API contracts now have decision record.
8. **ACTOR/STORY section** — Prevents Beads becoming isolated technical chores.
9. **NFR section (NFR-001–005)** — Latency gates, recall gates, offline, telemetry bounds, degradation behavior.
10. **SEC section (SEC-001–005)** — Snippet/query persistence defaults, pruning, no credentials in diagnostics.
11. **REQ-016b hold-out benchmark** — Prevents benchmark canon overfitting.
12. **Query hash default (REQ-014b revised)** — Raw query text requires explicit opt-in.
13. **Candidate identity priority chain** — chunk_id > symbol_id > file+range+hash.
14. **SearchPlan schema contract** — mandatory_lanes, suppressed_lanes, lane_timeout, active_safety_lane.
15. **Negative control for ExactHitFloor** — Vendor/minified files excluded from floor promotion. VAL-005b-neg.
16. **OPS-001, OPS-005 implementation notes** — Doctor behavior and degraded lane reporting in explain-search.
17. **RISK-012 through RISK-015** — Four new risks covering vendor noise floor, typo propagation, flag hiding, and benchmark overfitting.

### Decisions rejected in v3.1

| Rejected | Reason |
|---|---|
| Epic split A+B as separate JSONL epics | User instruction: one epic. Track separation is sufficient. |
| UC IDs for every requirement | Over-engineering for this project scale. |
| DOC section as blocking REQ | Out of scope for v1; deferred to DEF-015. |
| Byte-range in candidate identity | Too implementation-specific for v1; content hash is sufficient fallback. |

### Future revisit triggers

- FTS5 Recall@10 < 85% of trigram on symbol queries after MS2 → open aft-tantivy-spike (DEF-003)
- FlatF32 latency p95 > 300ms on repo > 50K chunks → open aft-sqlite-vec-spike after v1.0 (DEF-002)
- Agent benchmarks show build/test graph is top miss class after Phase 3 → open aft-rig-build-graph-v2 (DEF-001)
- `benchmark_enriched` beats `aft_output` by > 15% nDCG consistently → engine context bug; fix before next milestone
- Rerank shows consistent +5% nDCG@10 at latency < 300ms p95 → revisit ADR-006, make rerank opt-out
- ExactHitFloor causes incorrect vendor results in top-5 consistently → add confidence threshold to `is_exact_hit`
- Hold-out recall drops while tuning recall rises → ranking features overfit; retune with hold-out included
- `reranker_skipped_reason` fires >30% of queries → increase `total_tokens` defaults or reduce `rerank_pool_size`
