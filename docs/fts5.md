# FTS5 Side Feature

## Overview

FTS5 is an opt-in experimental full-text search backend for AFT. It provides fast symbol name, body text, and file path search using SQLite FTS5 virtual tables. FTS5 is designed as a lightweight alternative to semantic search for code navigation and discovery.

**Status:** Experimental (opt-in side feature)

## Enabling FTS5

### Compile-time

FTS5 is behind the `semantic-fts5` Cargo feature flag. Build with:

```bash
cargo build --features semantic-fts5
```

### Runtime

Enable in `aft.jsonc`:

```jsonc
{
  "fts5": {
    "enabled": true,
    "auto_index": true,        // Auto-index on configure
    "index_on_start": false,   // Index on process start
    "max_results": 20,         // Max results per query
    "max_body_chars": 2000,    // Max body text per result
    "max_body_lines": 60       // Max body lines per result
  }
}
```

## Commands

### fts5_index

Manage the FTS5 index:

```json
{
  "command": "fts5_index",
  "action": "status"       // "status" | "update" | "rebuild" | "prune"
}
```

- **status**: Check index health (file count, symbol count, freshness)
- **update**: Incrementally index changed files
- **rebuild**: Clear and reindex everything
- **prune**: Remove files no longer on disk

### fts5_search

Search across all indexed content:

```json
{
  "command": "fts5_search",
  "query": "SemanticBackendConfig",
  "scope": "all",          // "all" | "symbols" | "bodies" | "paths"
  "top_k": 10
}
```

### fts5_find_symbol

Find a symbol by exact or prefix name:

```json
{
  "command": "fts5_find_symbol",
  "name": "SemanticBackendConfig",
  "mode": "exact",         // "exact" | "prefix"
  "top_k": 10
}
```

### fts5_read_symbol

Read canonical source for a symbol:

```json
{
  "command": "fts5_read_symbol",
  "symbol_id": 42,
  "name": "SemanticBackendConfig",
  "file": "crates/aft/src/config.rs",
  "context_lines": 0
}
```

### fts5_doctor

Diagnose FTS5 index health and configuration:

```json
{
  "command": "fts5_doctor"
}
```

Reports: compiled status, FTS5 availability, runtime config, index health, and warnings.

## Architecture

### Database Store

FTS5 uses a dedicated SQLite database separate from the callgraph store:

- **Path**: `<project_root>/.aft/fts5.sqlite`
- **Schema**: Versioned (v1), auto-created on first access
- **Tables**: `fts5_meta`, `fts5_files`, `fts5_symbols`, `fts5_symbols_fts`, `fts5_symbol_bodies_fts`, `fts5_paths_fts`

### Index Lifecycle

1. **Configure**: Parse FTS5 config from `aft.jsonc`
2. **Index**: Extract symbols via tree-sitter, store in SQLite
3. **Search**: Route queries through planner to appropriate lanes
4. **Update**: Incrementally reindex changed files (blake3 hash check)
5. **Prune**: Remove stale files from index

### Query Planner

The planner routes queries to multiple search lanes:

1. **exact_symbol_sql**: Exact name match via SQL
2. **prefix_symbol_sql**: Prefix match via SQL LIKE
3. **symbol_fts**: FTS5 virtual table search on symbol names
4. **path_fts**: FTS5 virtual table search on file paths
5. **body_fts**: FTS5 virtual table search on symbol bodies
6. **short_token_fallback**: Fallback for very short queries

Results are fused, scored, deduplicated, and returned with lane attribution.

## Plugin Integration

### OpenCode

FTS5 commands are registered as tools in `packages/opencode-plugin/src/tools/fts5.ts`:

- `fts5_search`
- `fts5_index`
- `fts5_find_symbol`
- `fts5_read_symbol`
- `fts5_doctor`

### Pi

FTS5 commands are registered as tools in `packages/pi-plugin/src/tools/fts5.ts` with the same interface.

## Known Limitations

1. **Experimental**: FTS5 is not the default search backend
2. **No semantic understanding**: FTS5 is lexical-only, no embedding-based similarity
3. **Single-project**: One FTS5 database per project root
4. **No cross-repo**: Cannot search across multiple repositories
5. **No reranking**: Does not support LLM-based reranking

## Benchmark Comparison

Compare FTS5 against other search modes:

```bash
# Ripgrep lexical baseline
bun run benchmarks/semble/baseline-rg.ts --pilot --k 10

# FTS5 baseline
bun run benchmarks/semble/baseline-fts5.ts --pilot --k 10

# Full pilot comparison
bun run benchmarks/semble/pilot.ts --binary ./target/release/aft --k 10
```

## Graduation Criteria

FTS5 may graduate to a selectable lexical backend when:

1. **Benchmark evidence**: FTS5 recall/MRR matches or exceeds trigram search
2. **Operational maturity**: Schema migrations, error recovery, and edge cases handled
3. **Agent feedback**: Coding agents report improved search experience
4. **Documentation**: Complete user and developer docs
5. **E2E validation**: Full end-to-end test coverage

## Files

- `crates/aft/src/fts5_store.rs`: Database store and schema
- `crates/aft/src/fts5_indexer.rs`: Symbol extraction and indexing
- `crates/aft/src/fts5_planner.rs`: Query planning and lane routing
- `crates/aft/src/commands/fts5.rs`: Command handlers
- `crates/aft/src/fts5_experimental.rs`: Legacy spike code
- `packages/opencode-plugin/src/tools/fts5.ts`: OpenCode tool definitions
- `packages/pi-plugin/src/tools/fts5.ts`: Pi tool definitions
- `benchmarks/semble/baseline-fts5.ts`: Benchmark baseline
- `benchmarks/semble/pilot.ts`: Pilot comparison runner
