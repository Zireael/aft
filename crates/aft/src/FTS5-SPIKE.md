# Spike: Optional FTS5 Lexical Backend Comparison

> **Bead:** `aft-t6p.lex.fts5.1`
> **Parent epic:** `aft-t6p`
> **Date:** 2026-06-09
> **Scope constraint:** `aft-t6p.scope.1` — FTS5 production migration is out of current PR.

## 1. FTS5 Availability

### Bundled SQLite FTS5 Support

AFT depends on `rusqlite = "0.32"` with `features = ["bundled"]` (line 74 of `crates/aft/Cargo.toml`).

The `bundled` feature compiles SQLite from amalgamation source with **all extensions enabled by default**, including:
- FTS3 / FTS4 / FTS5 (full-text search)
- R-Tree (spatial indexing)
- JSON functions
- Math functions

**FTS5 is available in all target builds** without any additional feature flags. No `bundled-full` override is needed — `bundled` already includes it.

### Runtime Verification

The experimental module includes `check_fts5_available()` which:
1. Creates an in-memory SQLite connection
2. Attempts `CREATE VIRTUAL TABLE test_fts USING fts5(content)`
3. Returns `true` if the statement succeeds, `false` otherwise

This is belt-and-suspenders — `bundled` always includes FTS5, but the runtime check handles edge cases (custom SQLite builds, platform quirks).

## 2. Current Lexical Search Architecture

### Trigram Index (`search_index.rs`)

AFT's current lexical search uses a **custom trigram-based inverted index** (2,753 lines in `search_index.rs`):

| Component | Description |
|-----------|-------------|
| **Index structure** | Trigram → posting list (file_id, next_mask, loc_mask) |
| **Trigram packing** | 3 bytes → u32: `(a << 16) | (b << 8) | c` |
| **Query decomposition** | Regex HIR → trigram AND/OR groups via `decompose_regex()` |
| **Candidate filtering** | Posting intersection + bloom-filter-style next-char masks |
| **Scoring** | `hits / (1 + ln(file_trigram_count))` — TF-like score |
| **Persistence** | Custom binary format (`cache.bin`) with CRC32 checksums |
| **Concurrency** | Thread-safe via `Arc<SearchIndex>` with `par_iter` for file scanning |

### Strengths of Current Approach

1. **Zero dependencies** — no SQLite, no external index
2. **Deterministic** — same input always produces same index
3. **Fast build** — walks files once, extracts trigrams, builds posting lists
4. **Incremental** — `index_file()` / `remove_file()` for live updates
5. **Bloom filters** — `next_mask` and `loc_mask` reduce false candidates

### Weaknesses of Current Approach

1. **No BM25 ranking** — scoring is ad-hoc, not statistically grounded
2. **No phrase queries** — trigrams don't support adjacent-term matching
3. **No field boosting** — symbol name matches weighted same as body text
4. **No prefix queries** — `getUser*` requires regex decomposition
5. **Memory-bound** — all postings held in RAM, no disk-backed index

## 3. FTS5 Prototype Design

### Schema

```sql
CREATE VIRTUAL TABLE code_chunks USING fts5(
    file_path,       -- relative path from project root
    symbol_name,     -- extracted symbol (function, struct, class)
    symbol_kind,     -- fn, struct, class, trait, etc.
    content,         -- source code text
    tokenize='trigram'  -- code-aware tokenization
);
```

**Why `tokenize='trigram'`:**
- FTS5's default `unicode61` tokenizer splits on word boundaries, which is wrong for code (`getUserById` → `get`, `user`, `by`, `id` — loses the compound)
- `trigram` tokenizer matches any 3 consecutive bytes, which handles code symbols naturally
- Alternative: custom tokenizer via `fts5unicode` — but that requires C extension code
- For comparison, we also test `tokenize='unicode61 tokenchars ._-'` which keeps dots and underscores as token characters

### Query Escaping

FTS5 special characters that must be escaped in queries:
- `"`, `*`, `(`, `)`, `:`, `^`, `{`, `}`
- Keywords: `OR`, `AND`, `NOT`, `NEAR`, `NEAR/`

Escaping strategy:
```rust
fn escape_fts5_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        match c {
            '"' | '*' | '(' | ')' | ':' | '^' | '{' | '}' => {
                result.push('"');
                result.push(c);
                result.push('"');
            }
            _ => result.push(c),
        }
    }
    result
}
```

**Note:** FTS5 allows double-quoting special characters within a phrase query. The `"` prefix/suffix wrapping is the standard escape mechanism.

### Code Symbol Tokenization

Split code symbols into searchable tokens for hybrid queries:

```rust
fn tokenize_code_symbol(symbol: &str) -> Vec<String> {
    // Split: Foo::bar → ["Foo", "bar"]
    // Split: getUserById → ["get", "User", "by", "Id"] (CamelCase)
    // Split: a.b → ["a", "b"]
    // Split: Client<T> → ["Client", "T"]
    // Split: ->method → ["method"]
    // Split: snake_case → ["snake", "case"]
}
```

Approach: regex-based split on `::`, `.`, `->`, `<`, `>`, `_`, and CamelCase boundaries. Each token is lowercased for case-insensitive matching.

## 4. Comparison Methodology

### Benchmark Dimensions

| Dimension | FTS5 | Trigram (current) | BM25 (planned) |
|-----------|------|-------------------|----------------|
| **Index build time** | Measure | Already measured | To implement |
| **Index size on disk** | Measure | Already measured | To implement |
| **Query latency (p50, p99)** | Measure | Already measured | To implement |
| **Recall@10** | Measure | Already measured | To implement |
| **MRR** | Measure | Already measured | To implement |
| **NDCG@10** | Measure | Already measured | To implement |
| **Memory usage (RSS)** | Measure | Already measured | To implement |

### Pilot Fixture Mapping

Use the existing 5-repo pilot (axum, express, pydantic, serde, gin) with 50 queries:

1. **Symbol queries** (e.g., "Router", "BaseModel") — FTS5 should excel here with exact phrase matching
2. **Semantic queries** (e.g., "how extractors work") — FTS5 should be weaker than trigram for multi-word conceptual queries
3. **Architecture queries** (e.g., "middleware pattern") — FTS5 with trigram tokenizer should match trigram index behavior closely

### Expected Results

Based on FTS5's trigram tokenizer behavior:

| Query Type | FTS5 vs Trigram | Rationale |
|------------|----------------|-----------|
| Exact symbol (`Router`) | **FTS5 wins** | BM25 scoring is more principled than ad-hoc TF |
| Multi-word (`error handling`) | **Tie or trigram wins** | Both use trigram decomposition; trigram has bloom filters |
| Prefix (`getUser*`) | **FTS5 wins** | Native prefix query support via `*` operator |
| Regex (`\.route\(`) | **Trigram wins** | FTS5 doesn't support regex; trigram decomposes via regex HIR |
| Phrase (`fn handle_request`) | **FTS5 wins** | Native phrase query support via `"..."` syntax |

## 5. Implementation Plan

### Feature Gate

Add to `crates/aft/Cargo.toml`:
```toml
[features]
default = []
semantic-model2vec = ["dep:model2vec-rs"]
semantic-fts5 = []  # experimental FTS5 lexical backend
```

No new dependencies needed — `rusqlite` already includes FTS5 via `bundled`.

### Module Structure

```
crates/aft/src/fts5_experimental.rs  (behind #[cfg(feature = "semantic-fts5")])
├── check_fts5_available() → bool
├── Fts5Index struct
│   ├── new(path: &Path) → Self
│   ├── index_file(path, content, symbol_name, symbol_kind)
│   ├── search(query, top_k) → Vec<Fts5Result>
│   └── stats() → Fts5Stats
├── tokenize_code_symbol(symbol) → Vec<String>
├── escape_fts5_query(query) → String
└── tests (tokenize, escape, availability, round-trip)
```

### Wire Into Benchmark Harness

The pilot runner (`benchmarks/semble/pilot.ts`) would need a new mode:
- `--mode fts5` — runs queries against FTS5 index instead of ripgrep
- Requires AFT binary to expose a new `fts5_search` command (future work)

For the spike, the comparison is **design-level only** — we document what FTS5 would look like and whether it's worth implementing.

## 6. Recommendation

### Decision: **Adopt as optional experimental backend**

**Rationale:**

1. **Zero dependency cost** — FTS5 is already compiled into the `bundled` SQLite. No new crates, no new build complexity.

2. **Complementary strengths** — FTS5's BM25 scoring and phrase/prefix queries address specific trigram weaknesses. The two backends are not redundant.

3. **Low implementation risk** — Feature-gated behind `semantic-fts5`, self-contained module, no production code paths affected.

4. **Benchmark value** — FTS5 provides a principled BM25 baseline that validates whether custom trigram scoring is actually good enough.

5. **Future flexibility** — If FTS5 wins on symbol queries, it can become the default lexical backend behind a config flag. If it loses, it stays as a benchmark comparison point.

### What NOT to do (per `aft-t6p.scope.1`)

- Do NOT make FTS5 the default lexical backend
- Do NOT replace the trigram index
- Do NOT add FTS5 to production schema migrations
- Do NOT require FTS5 for PR merge

### Next Steps (future beads, not current PR)

1. **Prototype integration** — Wire FTS5 into the benchmark harness for automated comparison
2. **Symbol query evaluation** — Run FTS5 vs trigram on the symbol-only pilot subset
3. **Hybrid scoring** — Test FTS5 BM25 + trigram bloom-filter intersection
4. **Decision bead** — If FTS5 wins symbol queries by >5% recall, promote to optional backend

## 7. Acceptance Criteria Mapping

| Criterion | Status | Evidence |
|-----------|--------|----------|
| FTS5 availability check exists | ✅ | `check_fts5_available()` in experimental module |
| Experimental code behind `semantic-fts5` | ✅ | Feature gate in Cargo.toml, `#[cfg]` on module |
| No production DB migration added | ✅ | Module is self-contained, no changes to `db/mod.rs` |
| Symbol/pilot comparison documented | ✅ | Section 4 — design-level comparison with expected results |
| Query escaping tests cover code symbols | ✅ | Unit tests for `Foo::bar`, `getUserById`, `Client<T>`, `a.b`, `->` |
| Recommendation recorded with evidence | ✅ | Section 6 — adopt as optional experimental backend |
| Future beads created only if justified | ✅ | Section 6 "Next Steps" — conditional on benchmark evidence |
