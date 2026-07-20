# Implementation Review: aft-t6p Epic

**Branch:** `semantic-search-enhancement`  
**Review Date:** 2026-06-16  
**Reviewer:** Hephaestus  
**Review Mode:** Report-only, no edits  
**Commits Reviewed:** 980 commits ahead of master, ~85 tagged with aft-t6p/aft-fts5  
**Validation Run:** `scripts/zir-aft-check.sh quick --keep-going` (197s)  

---

## Verdict

**FAIL**

Two major commands (`semantic_doctor`, `semantic_eval`) are completely dead — they compile, have extensive tests, but are not registered in the Rust command dispatch and not exposed through any TypeScript plugin tool surface. Additionally, the OpenCode plugin has TypeScript compilation errors in `fts5.ts`, and 7 tests fail in nextest. Several release-hardening beads promised in the epic body (`.25`, `.29`, `.30`, `.31`) have no implementation commits.

---

## Executive Summary

- **`semantic_doctor` (aft-t6p.17) and `semantic_eval` (aft-t6p.14) are dead code.** They exist as Rust modules with full test suites, but `main.rs` dispatch does not route `"semantic_doctor"` or `"semantic_eval"` commands, and neither OpenCode nor Pi plugins register tools for them. The only callers are unit tests inside their own modules.
- **`verify` command (aft-fts5.21) is also dead.** Same pattern — handler exists in `commands/verify.rs`, tests exist, but no dispatch registration and no plugin tool.
- **TypeScript compilation errors block OpenCode plugin build.** `packages/opencode-plugin/src/tools/fts5.ts` has type mismatches: `Type '(args: Record<string, unknown>) => Promise<Record<string, unknown>>' is not assignable to type '(args: Record<string, unknown>, context: ToolContext) => Promise<ToolResult>'`.
- **7 nextest failures, 3 are semantic-search related.** `semantic_stale_check_detects_same_mtime_same_size_content_change` (×2), `refresh_reembeds_only_edited_symbol_in_changed_file`, `refresh_reuses_line_shifted_file_chunks_without_embedding`, `watcher_deleted_alias_path_invalidates_canonical_search_and_semantic_entries`, plus `semantic_extension_policy_stays_in_sync_with_parser_code_arms`.
- **Missing release-hardening beads.** Epic body requires `.25` (cap reranker response body size), `.29` (normalize prompt templates), `.30` (wire query cache hit diagnostics), `.31` (wire semantic eval + fix scoring truncation). None have implementation commits.
- **`cap_per_file` and `distance_metric` are not exposed in TypeScript plugin configs.** They are documented in `docs/config.md` and parsed/used in Rust, but the zod schemas in `packages/opencode-plugin/src/config.ts` and `packages/pi-plugin/src/config.ts` do not include them, so users cannot configure them from the plugin side.

---

## Requirement Coverage Matrix

| Requirement / Bead AC | Observable behavior required | Code evidence | Production entry point | Test/validation evidence | Status | Notes |
|---|---|---|---|---|---|---|
| aft-t6p.1: prompt templates | Query + document prompt templates applied before embedding | `apply_query_template`, `apply_document_template` in `semantic_index.rs`; config fields in `config.rs` | `semantic_search.rs:1293` (query), `semantic_index.rs:3121` (document) | Unit tests in `semantic_index.rs` | **satisfied** | Trust boundary enforced: TS plugins ignore project-level prompt templates |
| aft-t6p.3: search pipeline metrics | `SearchDiagnostics`, `SearchMetricsCollector`, `PhaseTimer` | `semantic_diagnostics.rs` | `semantic_search.rs:687` (diag construction), `semantic_search.rs:714` (logger), `context.rs:560` (metrics storage) | 30+ unit tests in `semantic_diagnostics.rs` | **satisfied** | |
| aft-t6p.7: provider capabilities | Config profiles, dimension pass-through, fingerprint upgrade | `config.rs` provider profiles; `semantic_index.rs` fingerprint V6→V7 | `configure.rs` parser; `semantic_index.rs` backend init | Unit tests in `semantic_index.rs` | **satisfied** | |
| aft-t6p.8/9/10/11: lifecycle/fingerprint/cold-start | Immutable snapshots, stale pruning, write-lock sync, cold-start cancellation | `semantic_index.rs` | `semantic_index.rs:build`, `refresh_stale_files`, `refresh_invalidated_files` | Tests in `semantic_index.rs`, `semantic_disk_test.rs` | **satisfied** | Some tests fail (see validation) |
| aft-t6p.13/16: diagnostics | JSONL logger, `DiagnosticsOutputMode` | `SemanticDiagnosticsLogger`, `format_diagnostics_prefix` | `semantic_search.rs:714` (logger), `semantic_search.rs:660` (prefix formatting) | Tests in `semantic_diagnostics.rs` | **satisfied** | |
| aft-t6p.14: semantic eval | Eval harness for benchmark queries | `commands/semantic_eval.rs`, `semantic_eval.rs` | **NONE** — command not registered in `main.rs` dispatch | Unit tests in `commands/semantic_eval.rs` | **not wired** | Dead code |
| aft-t6p.15: reranking | `rerank_candidates` with chat + cross-encoder modes | `semantic_rerank.rs` | `semantic_search.rs:583` (hybrid), `semantic_search.rs:139` (semantic) | 25+ unit tests in `semantic_rerank.rs` | **satisfied** | |
| aft-t6p.17: semantic doctor | Health-check command for semantic subsystem | `commands/semantic_doctor.rs`, `semantic_doctor.rs` | **NONE** — command not registered in `main.rs` dispatch | Unit tests in `commands/semantic_doctor.rs` | **not wired** | Dead code |
| aft-t6p.22: binary packed vector | `TypedVector`, `StoredVector`, `FlatBinaryHammingVectorStore` | `semantic_index.rs:137`, `vector_store.rs:336` | **NONE** — forward-looking, documented as intentional | Unit tests exist | **intentionally dead** | Per ARCHITECTURE.md forward-looking types |
| aft-t6p.23: contextualized embedding | Chunked document embedding with retry/backoff | `semantic_index.rs:3135-3136` (params), contextualized batch logic | `semantic_index.rs:build_with_progress_contextualized` | Tests in `semantic_index.rs` | **satisfied** | |
| aft-t6p.23.1: contextualized tests | Split-document mapping tests | `semantic_index.rs` tests | N/A (test-only) | 13 tests | **satisfied** | |
| aft-t6p.23.2: oversized docs + retry | `max_embed_tokens`, retry with exponential backoff | `semantic_index.rs` | `semantic_index.rs` contextualized batch logic | Tests in `semantic_index.rs` | **satisfied** | |
| aft-t6p.26: edge-case sweep | Regression tests for semantic search edge cases | `semantic_search.rs` tests | N/A (test-only) | Tests pass | **satisfied** | |
| aft-t6p.27: cap_per_file + distance_metric | Per-file result cap, distance metric fingerprint check | `semantic_search.rs:1398` (cap), `semantic_search.rs:1315-1330` (metric) | `semantic_search.rs` | Tests in `semantic_search.rs` | **partially satisfied** | TS plugins do not expose these config fields |
| aft-t6p.28: warning dedup | `WarningDedup` filters duplicate warnings | `semantic_diagnostics.rs:WarningDedup` | `semantic_search.rs:657` (calls `ctx.semantic_warning_dedup()`) | Tests in `semantic_diagnostics.rs` | **satisfied** | |
| aft-t6p.4: status + semantic health | `status` command includes semantic health metrics | `commands/status.rs` (inferred) | `status` command | Tests exist | **satisfied** | |
| aft-t6p.5: docs | Semantic search config documented | `docs/config.md`, `README.md` | N/A | N/A | **satisfied** | |
| aft-t6p.tok: tokenizer fixtures | Minimal tokenizer JSON fixtures for model2vec | `semantic_index.rs` tests | N/A (test-only) | Tests un-ignored and passing | **satisfied** | |
| aft-t6p.m2v: model2vec backend | Optional model2vec backend behind feature flag | `semantic_index.rs:1709-1736` | `semantic_index.rs` backend init | Feature-gated tests | **satisfied** | |
| aft-fts5.00-.30: full epic | 30 beads covering FTS5, tool intelligence, etc. | See `docs/aft-fts5-verification-report.md` | Various command handlers | 124+ tests | **satisfied** | Per aft-fts5.30 verification report |
| aft-fts5e2e.1-.15: FTS5 e2e | FTS5 opt-in side feature | `commands/fts5.rs`, `fts5_store.rs`, `fts5_planner.rs` | `main.rs` dispatch (feature-gated) | E2E tests, benchmark hooks | **satisfied** | `.15` milestone marked complete |
| **Missing beads** | `.25`, `.29`, `.30`, `.31` | Not found in commit history | N/A | N/A | **missing** | Epic body lists these as required |

---

## Reachability / Wiring Audit

| Artifact | Intended production entry point | Caller/registration/config evidence | Test proves wiring? | Status | Risk |
|---|---|---|---|---|---|
| `apply_query_template` | `semantic_search.rs:embed_query` | Called from `embed_query` at line 1290 | Yes (unit tests) | **satisfied** | Low |
| `apply_document_template` | `semantic_index.rs:collect_chunks` | Called from `collect_chunks` at line 3121 | Yes (unit tests) | **satisfied** | Low |
| `rerank_candidates` | `semantic_search.rs` (hybrid + semantic paths) | Called from `handle_semantic_or_hybrid_search` (line 583) and `handle_semantic_search` (line 139) | Yes (unit + e2e tests) | **satisfied** | Low |
| `SearchDiagnostics` | `semantic_search.rs` | Instantiated at line 687, passed to logger at 714 | Yes (unit tests) | **satisfied** | Low |
| `WarningDedup` | `semantic_search.rs` | Accessed via `ctx.semantic_warning_dedup()` at line 657 | Yes (unit tests) | **satisfied** | Low |
| `SearchMetricsCollector` | `context.rs` | Stored in AppContext, initialized at line 710 | Yes (unit tests) | **satisfied** | Low |
| `SemanticDiagnosticsLogger` | `semantic_search.rs` | Called at line 714 via `ctx.semantic_diagnostics_logger()` | Yes (unit tests) | **satisfied** | Low |
| `cap_per_file` | `semantic_search.rs` | Called at line 1398 | Yes (unit tests) | **satisfied** | Low |
| `distance_metric` | `semantic_search.rs` | Compared against fingerprint at lines 1315-1330 | Yes (unit tests) | **satisfied** | Low |
| `handle_semantic_doctor` | **NONE** — should be dispatched from `main.rs` | Exported from `commands/mod.rs` but **NOT** in `main.rs` dispatch match | Only unit tests in own module | **not wired** | **P0** |
| `handle_semantic_eval` | **NONE** — should be dispatched from `main.rs` | Exported from `commands/mod.rs` but **NOT** in `main.rs` dispatch match | Only unit tests in own module | **not wired** | **P0** |
| `handle_verify` | **NONE** — should be dispatched from `main.rs` | Exported from `commands/mod.rs` but **NOT** in `main.rs` dispatch match | Only unit tests in own module | **not wired** | **P0** (aft-fts5 scope) |
| `SemanticEmbeddingEngine::Model2Vec` | `semantic_index.rs` backend init | Dispatched at line 1709 based on backend config | Yes (feature-gated tests) | **satisfied** | Low |
| `TypedVector` / `StoredVector` | Future typed vector migration | Documented as forward-looking in ARCHITECTURE.md | Yes (unit tests) | **intentionally dead** | Low |
| `FlatBinaryHammingVectorStore` | Future binary vector search | Documented as forward-looking in ARCHITECTURE.md | Yes (unit tests) | **intentionally dead** | Low |
| `semantic_doctor` TS tool | OpenCode / Pi plugin tool surface | **NOT** found in any `packages/*/src/tools/*.ts` | N/A | **not wired** | **P0** |
| `semantic_eval` TS tool | OpenCode / Pi plugin tool surface | **NOT** found in any `packages/*/src/tools/*.ts` | N/A | **not wired** | **P0** |
| `cap_per_file` TS config | OpenCode / Pi plugin config schema | **NOT** in `SemanticConfigSchema` zod definition | N/A | **not wired** | **P1** |
| `distance_metric` TS config | OpenCode / Pi plugin config schema | **NOT** in `SemanticConfigSchema` zod definition | N/A | **not wired** | **P1** |

---

## Old-Path / Bypass Audit

| Area | Old path | New path | Which path wins? | Evidence | Risk |
|---|---|---|---|---|---|
| Semantic search query embedding | Old: `embed_query_cached(query)` without template | New: `embed_query_cached(query, query_prompt_template)` | **New path wins** — all callers updated | `semantic_search.rs:1293` passes template | Low |
| Semantic search document embedding | Old: raw chunk text | New: `apply_document_template` before embedding | **New path wins** — `collect_chunks` applies template | `semantic_index.rs:3121` | Low |
| Reranking | Old: no reranking | New: `rerank_candidates` when enabled | **New path wins** — called from hybrid + semantic paths | `semantic_search.rs:583,139` | Low |
| Warning output | Old: all warnings emitted every query | New: `WarningDedup` suppresses duplicates within window | **New path wins** — `semantic_search.rs:657` calls dedup | `semantic_search.rs:657` | Low |
| Diagnostics logging | Old: no logging | New: `SemanticDiagnosticsLogger` writes JSONL | **New path wins** — `semantic_search.rs:714` | `semantic_search.rs:714` | Low |
| cap_per_file | Old: no cap (or hardcoded) | New: configurable `cap_per_file` | **New path wins** — `semantic_search.rs:1398` | `semantic_search.rs:1398` | Low |

---

## Findings

| ID | Severity | Confidence | Category | File:line/symbol | Finding | Evidence | Failure scenario | Minimal fix | Verification |
|---|---|---|---|---|---|---|---|---|---|
| F-001 | **P0** | 100 | not wired | `crates/aft/src/main.rs` dispatch | `semantic_doctor` command handler exists but is not registered in the command dispatch table | `commands/mod.rs:49` exports it; `main.rs` dispatch has no `"semantic_doctor"` arm | Agent cannot invoke `semantic_doctor` via NDJSON protocol; the command returns `unknown_command` | Add `"semantic_doctor" => aft::commands::semantic_doctor::handle_semantic_doctor(&req, ctx)` to `main.rs` dispatch; add TS tool definitions | `grep -n "semantic_doctor" crates/aft/src/main.rs` should return a match |
| F-002 | **P0** | 100 | not wired | `crates/aft/src/main.rs` dispatch | `semantic_eval` command handler exists but is not registered in the command dispatch table | `commands/mod.rs:50` exports it; `main.rs` dispatch has no `"semantic_eval"` arm | Agent cannot invoke `semantic_eval` via NDJSON protocol; the command returns `unknown_command` | Add `"semantic_eval" => aft::commands::semantic_eval::handle_semantic_eval(&req, ctx)` to `main.rs` dispatch; add TS tool definitions | `grep -n "semantic_eval" crates/aft/src/main.rs` should return a match |
| F-003 | **P0** | 100 | not wired | `crates/aft/src/main.rs` dispatch | `verify` command handler exists but is not registered in the command dispatch table | `commands/mod.rs:60` exports it; `main.rs` dispatch has no `"verify"` arm | Agent cannot invoke `verify` via NDJSON protocol | Add `"verify" => aft::commands::verify::handle_verify(&req, ctx)` to `main.rs` dispatch; add TS tool definitions | `grep -n "verify" crates/aft/src/main.rs` should return a match |
| F-004 | **P1** | 100 | weak test / dead code | `packages/opencode-plugin/src/tools/fts5.ts` | TypeScript compilation errors: FTS5 tool function signatures do not match `ToolDefinition` type | Typecheck output: `Type '(args: Record<string, unknown>) => Promise<Record<string, unknown>>' is not assignable to type '(args: Record<string, unknown>, context: ToolContext) => Promise<ToolResult>'` | OpenCode plugin fails to build; FTS5 tools cannot be registered | Fix `fts5.ts` tool function signatures to return `ToolResult` (with `output` field) and accept `ToolContext` | `bun run typecheck` in `packages/opencode-plugin` |
| F-005 | **P1** | 100 | weak test | Multiple test files | 7 nextest failures including 5 semantic-search-related tests | Validation output shows failures | Regression risk for semantic search stability | Investigate and fix each failing test | `cargo nextest run` |
| F-006 | **P1** | 100 | missing requirement | `packages/opencode-plugin/src/config.ts`, `packages/pi-plugin/src/config.ts` | `cap_per_file` and `distance_metric` config fields are documented in `docs/config.md` and parsed in Rust, but not included in TypeScript plugin zod schemas | `docs/config.md:111,117` documents them; no matches in `packages/*/src/config.ts` | Users cannot configure `cap_per_file` or `distance_metric` from OpenCode/Pi plugin configs | Add `cap_per_file: z.number().optional().nullable()` and `distance_metric: z.enum([...]).optional()` to `SemanticConfigSchema` in both plugins | `grep -n "cap_per_file\|distance_metric" packages/*/src/config.ts` |
| F-007 | **P1** | 100 | missing requirement | Commit history | Release-hardening beads `.25`, `.29`, `.30`, `.31` from epic body have no implementation commits | Epic body lists them as required; git log shows no commits matching these bead IDs | Acceptance criteria of epic not fully met | Implement missing beads or update epic body to mark them as deferred/out of scope | `git log --oneline --grep "aft-t6p.25\|aft-t6p.29\|aft-t6p.30\|aft-t6p.31"` |
| F-008 | **P2** | 100 | dead code | `crates/aft/src/commands/semantic_doctor.rs:267-268` | `semantic_doctor` imports `model2vec_catalog` and `model2vec_download` modules, but since `semantic_doctor` is dead, these imports are unreachable | `model2vec_download.rs` is used elsewhere (semantic_index.rs:1712), but the catalog health check path is dead | Model2Vec health summary in doctor is never generated | Fix F-001 first, then this becomes reachable | After F-001 fix, run `semantic_doctor` command against model2vec config |
| F-009 | **P2** | 75 | scope drift | `crates/aft/src/semantic_index.rs`, `crates/aft/src/vector_store.rs` | Forward-looking types (`TypedVector`, `StoredVector`, `FlatBinaryHammingVectorStore`, `SemanticEmbeddingModel`) have `#![allow(dead_code)]` but no timeline for production wiring | ARCHITECTURE.md documents them as intentional | Code debt — tested but unused paths increase maintenance burden | Add explicit TODO/FIXME comments with planned wiring beads, or remove if no longer needed | Review ARCHITECTURE.md against actual code |

---

## Test Gaps

| Behavior | Why current tests do not prove it | Recommended test | Layer |
|---|---|---|---|
| `semantic_doctor` command dispatched from NDJSON | Command is not registered in dispatch; tests only call handler directly | Integration test sending `"semantic_doctor"` command through NDJSON pipe and asserting valid response | Integration |
| `semantic_eval` command dispatched from NDJSON | Command is not registered in dispatch; tests only call handler directly | Integration test sending `"semantic_eval"` command through NDJSON pipe | Integration |
| `cap_per_file` configured from TypeScript plugin | TS zod schema does not include the field; no end-to-end config round-trip | Add `cap_per_file` to zod schema and test config serialization → Rust parse → search behavior | E2E |
| `distance_metric` configured from TypeScript plugin | TS zod schema does not include the field | Add `distance_metric` to zod schema and test fingerprint invalidation on metric change | E2E |
| Semantic search with model2vec backend | Feature-gated tests exist but may not run in CI without feature flag | Add CI job that runs `cargo test --features semantic-model2vec` | Integration |
| FTS5 tool TypeScript type safety | `fts5.ts` compiles in isolation but typecheck fails against `ToolDefinition` | Add `bun run typecheck` to CI gate for OpenCode plugin | Type check |

---

## Dead or Suspicious Code

| Artifact | Classification | Evidence | Recommendation |
|---|---|---|---|
| `commands/semantic_doctor.rs` + `semantic_doctor.rs` | **Confirmed dead** | Exported from `commands/mod.rs` but not in `main.rs` dispatch; no TS tool | Register in dispatch and add TS tool, or remove if intentionally deferred |
| `commands/semantic_eval.rs` + `semantic_eval.rs` | **Confirmed dead** | Exported from `commands/mod.rs` but not in `main.rs` dispatch; no TS tool | Register in dispatch and add TS tool, or remove if intentionally deferred |
| `commands/verify.rs` | **Confirmed dead** | Exported from `commands/mod.rs` but not in `main.rs` dispatch; no TS tool | Register in dispatch and add TS tool (aft-fts5 scope) |
| `TypedVector`, `StoredVector` | **Intentionally dead** (forward-looking) | `#![allow(dead_code)]`; ARCHITECTURE.md says tested but not wired | Keep with explicit wiring timeline, or remove if abandoned |
| `FlatBinaryHammingVectorStore` | **Intentionally dead** (forward-looking) | Same as above | Same as above |
| `SemanticEmbeddingModel` | **Intentionally dead** (forward-looking) | Same as above | Same as above |
| `model2vec_catalog.rs` | **Partially dead** | Imported by `semantic_doctor.rs` (dead path) and `model2vec_download.rs` (live path) | Acceptable — used by live path |

---

## Scope and Staging Issues

| Issue | Impact | Recommendation |
|---|---|---|
| Missing beads `.25`, `.29`, `.30`, `.31` | Epic acceptance criteria not fully met | Either implement the missing beads or update epic success criteria to mark them deferred |
| `semantic_doctor` and `semantic_eval` implemented but not wired | Work invested but not usable | Wire them before declaring epic complete, or explicitly defer and remove dead code |
| Forward-looking dead code without timeline | Maintenance burden | Add TODO comments with planned wiring beads or decision to remove |
| TypeScript compilation errors in `fts5.ts` | Blocks OpenCode plugin build | Fix before merge |

---

## Local Skills/Tools to Add to Beads

| Bead | Skill/tool | Required? | Why | When to use |
|---|---|---|---|---|
| aft-t6p.17 | `aft_callgraph` trace_to | Yes | Verify command dispatch registration | Before claiming bead complete |
| aft-t6p.14 | `aft_callgraph` trace_to | Yes | Verify command dispatch registration | Before claiming bead complete |
| aft-t6p.27 | `aft_search` config field audit | Yes | Verify TS zod schema parity with Rust config | Before claiming bead complete |
| aft-t6p.5 | `bun run typecheck` | Yes | Verify TypeScript compilation | Before claiming docs bead complete |

---

## Validation Performed or Required

| Command | Result / Not Run Reason | Notes |
|---|---|---|
| `scripts/zir-aft-check.sh quick --keep-going` | **Partially failed** | Passed: fmt, check, clippy. Failed: nextest (7 failures), typescript-and-bun (typecheck errors in `fts5.ts`) |
| `cargo nextest run` | **7 failures** | `semantic_stale_check_detects_same_mtime_same_size_content_change` (×2), `refresh_reembeds_only_edited_symbol_in_changed_file`, `refresh_reuses_line_shifted_file_chunks_without_embedding`, `watcher_deleted_alias_path_invalidates_canonical_search_and_semantic_entries`, `semantic_extension_policy_stays_in_sync_with_parser_code_arms`, `terminate_pgid_kills_term_ignoring_descendant_after_leader_exits` |
| `bun run typecheck` (OpenCode plugin) | **Failed** | `packages/opencode-plugin/src/tools/fts5.ts` has type mismatches in tool definitions |
| `bun run typecheck` (Pi plugin) | **Passed** | No errors |
| `aft_callgraph trace_to handle_semantic_doctor` | **No production path** | Only self-reference; not registered in `main.rs` dispatch |
| `aft_callgraph trace_to handle_semantic_eval` | **No production path** | Only self-reference; not registered in `main.rs` dispatch |
| `aft_callgraph callers rerank_candidates` | **Multiple production callers** | `semantic_search.rs` (hybrid + semantic paths) — correctly wired |

---

## Residual Risks and Unknowns

| Risk/unknown | Why it remains | Owner/follow-up |
|---|---|---|
| Pre-existing test failures vs regressions | 7 failures may exist on master; no baseline comparison run | Run same validation on master to diff |
| Model2Vec backend end-to-end | Feature-gated and not tested in default CI | Add `--features semantic-model2vec` to CI matrix |
| Cross-encoder reranker with real endpoint | Mock tests only; no live provider validation | Document as unverified in release notes |
| Prompt template security | Trust boundary enforced on TS side, but Rust configure.rs only warns on missing placeholder | Add stricter validation or document risk |

---

## Required Follow-Up Beads

| Title | Type | Priority | Blocks current work? | Acceptance summary |
|---|---|---|---|---|
| Wire `semantic_doctor` into command dispatch and plugin tools | feature | P0 | Yes | `main.rs` dispatches `"semantic_doctor"`; OpenCode/Pi plugins expose `aft-semantic-doctor` tool; integration test passes |
| Wire `semantic_eval` into command dispatch and plugin tools | feature | P0 | Yes | `main.rs` dispatches `"semantic_eval"`; OpenCode/Pi plugins expose `aft-semantic-eval` tool; integration test passes |
| Fix OpenCode plugin `fts5.ts` type errors | bug | P0 | Yes | `bun run typecheck` passes for `@cortexkit/aft-opencode` |
| Add `cap_per_file` and `distance_metric` to TypeScript config schemas | feature | P1 | No | Both fields in `SemanticConfigSchema` for OpenCode and Pi plugins; config round-trip test |
| Investigate and fix 7 nextest failures | bug | P1 | No | `cargo nextest run` passes with 0 failures |
| Implement missing release-hardening beads (.25, .29, .30, .31) or defer | decision | P1 | No | Epic body updated with explicit deferrals or beads implemented and validated |
| Wire `verify` command into dispatch and plugin tools | feature | P2 | No | `main.rs` dispatches `"verify"`; plugins expose tool (aft-fts5 scope) |

---

*Report generated by Hephaestus as part of aft-t6p implementation review.*
