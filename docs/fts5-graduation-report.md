# FTS5 Graduation Decision Report

**Date:** 2026-06-14
**Feature:** FTS5 Side Feature
**Status:** Experimental → Ready for Evaluation

## Executive Summary

FTS5 is an opt-in experimental full-text search backend for AFT. This report evaluates whether FTS5 meets graduation criteria to become a selectable lexical backend alongside semantic search.

## Implementation Status

### Completed (Beads 0-12)

| Bead | Description | Status |
|------|-------------|--------|
| 0 | Record resolved decisions | ✅ Done |
| 1 | Feature gating and runtime config | ✅ Done |
| 2 | Versioned SQLite schema | ✅ Done |
| 3 | File and symbol indexing | ✅ Done |
| 4 | Incremental update/delete/prune | ✅ Done |
| 5 | Query planner and safe FTS handling | ✅ Done |
| 6 | fts5_index and fts5_doctor commands | ✅ Done |
| 7 | fts5_find_symbol and fts5_read_symbol | ✅ Done |
| 8 | OpenCode plugin tool registration | ✅ Done |
| 9 | Pi plugin tool registration | ✅ Done |
| 10 | Agent-facing text rendering | ✅ Done |
| 11 | E2E fixtures and integration tests | ✅ Done |
| 12 | Benchmark modes and Semble pilot hooks | ✅ Done |

### Remaining (Beads 13-15)

| Bead | Description | Status |
|------|-------------|--------|
| 13 | Docs and graduation decision report | 🔄 In Progress |
| 14 | Verify FTS5 e2e package end to end | ⏳ Pending |
| 15 | Mark FTS5 e2e opt-in side feature complete | ⏳ Pending |

## Evaluation Criteria

### 1. Benchmark Evidence

**Status:** Not yet evaluated

FTS5 needs to demonstrate comparable or superior performance against:
- Trigram search (existing lexical backend)
- Ripgrep (external lexical baseline)

**Action:** Run `benchmarks/semble/baseline-fts5.ts` against pilot corpus and compare with `baseline-rg.ts`.

**Expected Outcome:**
- Recall@10 ≥ 90% of ripgrep baseline
- MRR ≥ 80% of ripgrep baseline
- Latency ≤ 2x ripgrep baseline

### 2. Operational Maturity

**Status:** ✅ Complete

- [x] Schema versioning (v1)
- [x] Incremental indexing (blake3 hash check)
- [x] Stale file detection and pruning
- [x] Error recovery (store open failures, parse errors)
- [x] Graceful degradation (disabled state returns clear errors)
- [x] Doctor command for health diagnostics

### 3. Agent Feedback

**Status:** Not yet evaluated

FTS5 tools are registered in OpenCode and Pi plugins. Coding agents can use:
- `fts5_search` for full-text search
- `fts5_find_symbol` for exact/prefix symbol lookup
- `fts5_read_symbol` for source retrieval
- `fts5_index` for index management
- `fts5_doctor` for health checks

**Action:** Monitor agent usage patterns and collect feedback on:
- Search result relevance
- Latency perception
- Tool discoverability
- Error message clarity

### 4. Documentation

**Status:** ✅ Complete

- [x] User docs (`docs/fts5.md`)
- [x] Architecture docs (ARCHITECTURE.md updated)
- [x] Structure docs (STRUCTURE.md updated)
- [x] Benchmark docs (benchmarks/semble/README.md updated)
- [x] Known limitations documented

### 5. E2E Validation

**Status:** ⏳ Pending

- [ ] Full end-to-end test coverage
- [ ] Cross-platform validation (macOS, Linux, Windows)
- [ ] Performance regression testing
- [ ] Edge case coverage (empty projects, large codebases, special characters)

## Risk Assessment

### High Risk

1. **Benchmark Performance**: FTS5 may underperform against trigram search for certain query types
   - Mitigation: Run benchmarks before graduation decision
   - Contingency: Keep as experimental, improve query planner

2. **Schema Migration**: Future schema changes may require data migration
   - Mitigation: Versioned schema with migration support
   - Contingency: Provide rebuild command for breaking changes

### Medium Risk

1. **Index Size**: FTS5 database may grow large for huge codebases
   - Mitigation: Configurable `max_files` cap
   - Contingency: Add compression or archival options

2. **Concurrency**: Multiple AFT processes may conflict on FTS5 database
   - Mitigation: SQLite WAL mode for concurrent reads
   - Contingency: Add file locking or process coordination

### Low Risk

1. **Tokenization**: FTS5 tokenization may not match code semantics
   - Mitigation: Use `unicode61` tokenizer with code-aware settings
   - Contingency: Add custom tokenizer for specific languages

2. **Query Planner**: Lane routing may not optimal for all query types
   - Mitigation: Extensive test coverage for query patterns
   - Contingency: Add adaptive routing based on query statistics

## Recommendation

**Conditional Graduation**

FTS5 meets most graduation criteria but requires benchmark validation before becoming a selectable lexical backend.

### Next Steps

1. **Run Benchmarks**: Execute FTS5 vs ripgrep comparison on pilot corpus
2. **Collect Agent Feedback**: Monitor usage in OpenCode/Pi environments
3. **Address Gaps**: Fix any performance or usability issues identified
4. **Final Review**: Re-evaluate graduation criteria after 2 weeks of usage

### Graduation Decision

- **If benchmarks pass**: Graduate FTS5 to selectable lexical backend
- **If benchmarks fail**: Keep as experimental, improve query planner
- **If agent feedback negative**: Re-evaluate feature design

## Appendix

### Files Changed

```
crates/aft/src/fts5_store.rs          # Database store and schema
crates/aft/src/fts5_indexer.rs        # Symbol extraction and indexing
crates/aft/src/fts5_planner.rs        # Query planning and lane routing
crates/aft/src/commands/fts5.rs       # Command handlers
crates/aft/src/fts5_experimental.rs   # Legacy spike code
packages/opencode-plugin/src/tools/fts5.ts  # OpenCode tools
packages/pi-plugin/src/tools/fts5.ts        # Pi tools
benchmarks/semble/baseline-fts5.ts          # Benchmark baseline
benchmarks/semble/pilot.ts                  # Pilot comparison
docs/fts5.md                                # User documentation
```

### Test Coverage

- Unit tests: 51 FTS5-related tests (all passing)
- Integration tests: 9 e2e tests (compile verified)
- Benchmark tests: Ripgrep and FTS5 baselines available

### Configuration

```jsonc
{
  "fts5": {
    "enabled": false,           // Default: disabled
    "auto_index": false,        // Default: manual indexing
    "index_on_start": false,    // Default: no auto-index on start
    "max_results": 20,          // Default: 20 results per query
    "max_body_chars": 2000,     // Default: 2000 chars per body
    "max_body_lines": 60,       // Default: 60 lines per body
    "raw_fts_debug": false      // Default: no debug output
  }
}
```

---

**Report Author:** Hephaestus
**Review Date:** 2026-06-14
**Next Review:** 2026-06-28
