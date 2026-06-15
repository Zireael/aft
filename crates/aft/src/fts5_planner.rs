//! FTS5 query planner with multi-lane routing and result fusion.
//!
//! The planner analyzes a user query and determines which search lanes to
//! execute, then fuses results from multiple lanes with score normalization
//! and deduplication.
//!
//! ## Lanes
//!
//! 1. `exact_symbol_sql` — exact match on symbol name (highest priority)
//! 2. `prefix_symbol_sql` — prefix match on symbol name
//! 3. `symbol_fts` — FTS5 search on symbol names (unicode61 tokenizer)
//! 4. `path_fts` — FTS5 search on file paths (trigram tokenizer)
//! 5. `body_fts` — FTS5 search on symbol bodies (trigram tokenizer)
//! 6. `short_token_fallback` — fallback for very short tokens (< 3 chars)

use crate::fts5_experimental::{build_fts5_query, escape_fts5_query, tokenize_code_symbol};
use crate::fts5_store::Fts5Store;
use rusqlite::params;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum token length for FTS5 queries (shorter tokens use fallback).
const MIN_FTS_TOKEN_LEN: usize = 3;

// ---------------------------------------------------------------------------
// Query analysis
// ---------------------------------------------------------------------------

/// Analyzed query with extracted components.
#[derive(Debug, Clone)]
pub struct AnalyzedQuery {
    /// Original query string.
    pub raw: String,
    /// Tokenized query tokens.
    pub tokens: Vec<String>,
    /// Whether the query looks like an exact symbol (single identifier).
    pub is_exact_symbol: bool,
    /// Whether the query looks like a file path.
    pub is_path: bool,
    /// Whether the query contains special FTS characters.
    pub has_fts_special_chars: bool,
    /// Shortest token length (for short_token_fallback decision).
    pub min_token_len: usize,
}

impl AnalyzedQuery {
    /// Analyze a user query.
    pub fn analyze(query: &str) -> Self {
        let tokens = tokenize_code_symbol(query);
        let min_token_len = tokens.iter().map(|t| t.len()).min().unwrap_or(0);

        // Heuristic: exact symbol if single token with no special chars
        let is_exact_symbol = tokens.len() == 1
            && !query.contains(' ')
            && !query.contains('*')
            && !query.contains('"');

        // Heuristic: path if contains / or \ or .
        let is_path = query.contains('/') || query.contains('\\') || query.contains('.');

        // Check for FTS5 special characters
        let has_fts_special_chars = query
            .chars()
            .any(|c| matches!(c, '"' | '*' | '(' | ')' | ':' | '^' | '{' | '}'));

        Self {
            raw: query.to_string(),
            tokens,
            is_exact_symbol,
            is_path,
            has_fts_special_chars,
            min_token_len,
        }
    }
}

// ---------------------------------------------------------------------------
// Lane selection
// ---------------------------------------------------------------------------

/// Which lanes to execute for a given query.
#[derive(Debug, Clone)]
pub struct LaneSelection {
    pub exact_symbol_sql: bool,
    pub prefix_symbol_sql: bool,
    pub symbol_fts: bool,
    pub path_fts: bool,
    pub body_fts: bool,
    pub short_token_fallback: bool,
}

impl LaneSelection {
    /// Determine which lanes to execute for the analyzed query.
    pub fn for_query(query: &AnalyzedQuery) -> Self {
        let has_short_tokens = query.min_token_len < MIN_FTS_TOKEN_LEN && !query.tokens.is_empty();

        Self {
            // Exact symbol SQL: always try if query looks like a single identifier
            exact_symbol_sql: query.is_exact_symbol,
            // Prefix symbol SQL: try if query is a single token (prefix match)
            prefix_symbol_sql: query.is_exact_symbol || query.tokens.len() == 1,
            // Symbol FTS: try if we have enough tokens
            symbol_fts: !query.tokens.is_empty(),
            // Path FTS: try if query looks like a path
            path_fts: query.is_path,
            // Body FTS: always try if we have tokens
            body_fts: !query.tokens.is_empty(),
            // Short token fallback: use when tokens are too short for FTS
            short_token_fallback: has_short_tokens,
        }
    }

    /// Get list of active lane names.
    pub fn active_lanes(&self) -> Vec<&'static str> {
        let mut lanes = Vec::new();
        if self.exact_symbol_sql {
            lanes.push("exact_symbol_sql");
        }
        if self.prefix_symbol_sql {
            lanes.push("prefix_symbol_sql");
        }
        if self.symbol_fts {
            lanes.push("symbol_fts");
        }
        if self.path_fts {
            lanes.push("path_fts");
        }
        if self.body_fts {
            lanes.push("body_fts");
        }
        if self.short_token_fallback {
            lanes.push("short_token_fallback");
        }
        lanes
    }
}

// ---------------------------------------------------------------------------
// Search result with lane info
// ---------------------------------------------------------------------------

/// A search result from a specific lane.
#[derive(Debug, Clone)]
pub struct LaneResult {
    pub symbol_id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub snippet: String,
    /// Raw score from the lane (lower is better for FTS5 rank, higher for SQL).
    pub raw_score: f64,
    /// Normalized score (higher is better, 0.0-1.0 range).
    pub normalized_score: f64,
    /// Lane that produced this result.
    pub lane: String,
    /// Qualified name path (v3+).
    pub name_path: String,
    /// Duplicate index within same file/name/kind (v3+).
    pub duplicate_index: i32,
}

// ---------------------------------------------------------------------------
// Fused result
// ---------------------------------------------------------------------------

/// A deduplicated, scored result from the query planner.
#[derive(Debug, Clone)]
pub struct FusedResult {
    pub symbol_id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub snippet: String,
    /// Final fused score (higher is better).
    pub score: f64,
    /// Best lane that produced this result.
    pub best_lane: String,
    /// All lanes that matched this result.
    pub matched_lanes: Vec<String>,
    /// Qualified name path (v3+).
    pub name_path: String,
    /// Duplicate index within same file/name/kind (v3+).
    pub duplicate_index: i32,
}

// ---------------------------------------------------------------------------
// QueryPlanner
// ---------------------------------------------------------------------------

/// FTS5 query planner with multi-lane routing and result fusion.
pub struct QueryPlanner<'a> {
    store: &'a Fts5Store,
}

impl<'a> QueryPlanner<'a> {
    /// Create a new query planner.
    pub fn new(store: &'a Fts5Store) -> Self {
        Self { store }
    }

    /// Plan and execute a search query.
    pub fn search(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<FusedResult>, crate::fts5_store::Fts5StoreError> {
        let analyzed = AnalyzedQuery::analyze(query);
        let lanes = LaneSelection::for_query(&analyzed);
        let active = lanes.active_lanes();

        if active.is_empty() {
            return Ok(Vec::new());
        }

        // Execute each active lane
        let mut all_results: Vec<LaneResult> = Vec::new();

        if lanes.exact_symbol_sql {
            self.execute_exact_symbol_sql(&analyzed, &mut all_results)?;
        }
        if lanes.prefix_symbol_sql {
            self.execute_prefix_symbol_sql(&analyzed, &mut all_results)?;
        }
        if lanes.symbol_fts {
            self.execute_symbol_fts(&analyzed, top_k, &mut all_results)?;
        }
        if lanes.path_fts {
            self.execute_path_fts(&analyzed, top_k, &mut all_results)?;
        }
        if lanes.body_fts {
            self.execute_body_fts(&analyzed, top_k, &mut all_results)?;
        }
        if lanes.short_token_fallback {
            self.execute_short_token_fallback(&analyzed, top_k, &mut all_results)?;
        }

        // Fuse and deduplicate results
        let fused = self.fuse_results(all_results, top_k);

        Ok(fused)
    }

    // -----------------------------------------------------------------------
    // Lane execution
    // -----------------------------------------------------------------------

    /// Exact symbol SQL lookup (highest priority).
    fn execute_exact_symbol_sql(
        &self,
        query: &AnalyzedQuery,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        if query.tokens.is_empty() {
            return Ok(());
        }

        let name = &query.tokens[0];
        let symbols = self.store.get_symbol_by_name(name)?;

        for sym in symbols {
            // Look up the file by the symbol's file_id, not by empty path.
            // The previous code used get_file_by_path("") which always returned
            // None, then fell back to a FileRecord with an empty path.
            let file = self.store.get_file_by_id(sym.file_id)?.unwrap_or_else(|| {
                crate::fts5_store::FileRecord {
                    id: sym.file_id,
                    path: format!("<unknown file_id={}>", sym.file_id),
                    hash: String::new(),
                    mtime_secs: 0,
                    indexed_at: 0,
                    size_bytes: 0,
                    generation: 0,
                }
            });

            results.push(LaneResult {
                symbol_id: sym.id,
                file_id: sym.file_id,
                file_path: file.path,
                symbol_name: sym.name,
                symbol_kind: sym.kind,
                start_line: sym.start_line,
                end_line: sym.end_line,
                snippet: sym.body.clone(),
                raw_score: 0.0, // Exact match = best score
                normalized_score: 1.0,
                lane: "exact_symbol_sql".to_string(),
                name_path: sym.name_path.clone(),
                duplicate_index: sym.duplicate_index,
            });
        }

        Ok(())
    }

    /// Prefix symbol SQL lookup.
    fn execute_prefix_symbol_sql(
        &self,
        query: &AnalyzedQuery,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        if query.tokens.is_empty() {
            return Ok(());
        }

        let prefix = &query.tokens[0];

        // Use SQL LIKE for prefix match
        let sql =
            "SELECT id, file_id, name, kind, start_line, end_line, body, name_path, duplicate_index
                   FROM fts5_symbols
                   WHERE name LIKE ?1
                   ORDER BY name
                   LIMIT 50";

        let pattern = format!("{prefix}%");
        let mut stmt = self.store.conn.prepare(sql).map_err(|e| {
            crate::fts5_store::Fts5StoreError::Other(format!("prepare failed: {e}"))
        })?;

        let rows = stmt
            .query_map(params![pattern], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i32>(8)?,
                ))
            })
            .map_err(|e| crate::fts5_store::Fts5StoreError::Other(format!("query failed: {e}")))?;

        for row in rows.flatten() {
            let (sym_id, file_id, name, kind, start, end, body, name_path, dup_idx) = row;
            // Skip if already found by exact match
            if results.iter().any(|r| r.symbol_id == sym_id) {
                continue;
            }

            // Score: shorter prefix match = higher score
            let prefix_ratio = prefix.len() as f64 / name.len() as f64;

            results.push(LaneResult {
                symbol_id: sym_id,
                file_id,
                file_path: String::new(), // Will be filled in during fusion
                symbol_name: name,
                symbol_kind: kind,
                start_line: start,
                end_line: end,
                snippet: body,
                raw_score: prefix_ratio,
                normalized_score: prefix_ratio * 0.8, // Weight down from exact
                lane: "prefix_symbol_sql".to_string(),
                name_path,
                duplicate_index: dup_idx,
            });
        }

        Ok(())
    }

    /// Symbol FTS search (unicode61 tokenizer).
    fn execute_symbol_fts(
        &self,
        query: &AnalyzedQuery,
        top_k: usize,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        if query.tokens.is_empty() {
            return Ok(());
        }

        let fts_query = build_fts5_query(&query.raw, false);

        let sql = "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line, s.body,
                          s.name_path, s.duplicate_index, rank
                   FROM fts5_symbols_fts fts
                   JOIN fts5_symbols s ON s.id = fts.rowid
                   WHERE fts5_symbols_fts MATCH ?1
                   ORDER BY rank
                   LIMIT ?2";

        let mut stmt = self.store.conn.prepare(sql).map_err(|e| {
            crate::fts5_store::Fts5StoreError::Other(format!("prepare failed: {e}"))
        })?;

        let rows = stmt
            .query_map(params![fts_query, top_k as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i32>(8)?,
                    row.get::<_, f64>(9)?,
                ))
            })
            .map_err(|e| crate::fts5_store::Fts5StoreError::Other(format!("query failed: {e}")))?;

        for row in rows.flatten() {
            let (sym_id, file_id, name, kind, start, end, body, name_path, dup_idx, rank) = row;
            // Skip if already found by exact/prefix
            if results.iter().any(|r| r.symbol_id == sym_id) {
                continue;
            }

            // Normalize FTS5 rank (lower is better → invert)
            let normalized = 1.0 / (1.0 + rank.abs());

            results.push(LaneResult {
                symbol_id: sym_id,
                file_id,
                file_path: String::new(),
                symbol_name: name,
                symbol_kind: kind,
                start_line: start,
                end_line: end,
                snippet: body,
                raw_score: rank,
                normalized_score: normalized * 0.6, // Weight
                lane: "symbol_fts".to_string(),
                name_path,
                duplicate_index: dup_idx,
            });
        }

        Ok(())
    }

    /// Path FTS search (trigram tokenizer).
    fn execute_path_fts(
        &self,
        query: &AnalyzedQuery,
        top_k: usize,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        if query.tokens.is_empty() {
            return Ok(());
        }

        // Use trigram FTS for path search
        let fts_query = escape_fts5_query(&query.raw);

        let sql = "SELECT f.id, f.path, rank
                   FROM fts5_paths_fts fts
                   JOIN fts5_files f ON f.id = fts.rowid
                   WHERE fts5_paths_fts MATCH ?1
                   ORDER BY rank
                   LIMIT ?2";

        let mut stmt = self.store.conn.prepare(sql).map_err(|e| {
            crate::fts5_store::Fts5StoreError::Other(format!("prepare failed: {e}"))
        })?;

        let rows = stmt
            .query_map(params![fts_query, top_k as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .map_err(|e| crate::fts5_store::Fts5StoreError::Other(format!("query failed: {e}")))?;

        for row in rows.flatten() {
            let (file_id, path, rank) = row;
            let normalized = 1.0 / (1.0 + rank.abs());

            results.push(LaneResult {
                symbol_id: -1, // Path match, no specific symbol
                file_id,
                file_path: path,
                symbol_name: String::new(),
                symbol_kind: "path_match".to_string(),
                start_line: 0,
                end_line: 0,
                snippet: String::new(),
                raw_score: rank,
                normalized_score: normalized * 0.4, // Lower weight for paths
                lane: "path_fts".to_string(),
            });
        }

        Ok(())
    }

    /// Body FTS search (trigram tokenizer).
    fn execute_body_fts(
        &self,
        query: &AnalyzedQuery,
        top_k: usize,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        if query.tokens.is_empty() {
            return Ok(());
        }

        let fts_query = escape_fts5_query(&query.raw);

        let sql = "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line, s.body,
                          s.name_path, s.duplicate_index, rank
                   FROM fts5_symbol_bodies_fts fts
                   JOIN fts5_symbols s ON s.id = fts.rowid
                   WHERE fts5_symbol_bodies_fts MATCH ?1
                   ORDER BY rank
                   LIMIT ?2";

        let mut stmt = self.store.conn.prepare(sql).map_err(|e| {
            crate::fts5_store::Fts5StoreError::Other(format!("prepare failed: {e}"))
        })?;

        let rows = stmt
            .query_map(params![fts_query, top_k as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i32>(8)?,
                    row.get::<_, f64>(9)?,
                ))
            })
            .map_err(|e| crate::fts5_store::Fts5StoreError::Other(format!("query failed: {e}")))?;

        for row in rows.flatten() {
            let (sym_id, file_id, name, kind, start, end, body, name_path, dup_idx, rank) = row;
            // Skip if already found by higher-priority lane
            if results.iter().any(|r| r.symbol_id == sym_id) {
                continue;
            }

            let normalized = 1.0 / (1.0 + rank.abs());

            results.push(LaneResult {
                symbol_id: sym_id,
                file_id,
                file_path: String::new(),
                symbol_name: name,
                symbol_kind: kind,
                start_line: start,
                end_line: end,
                snippet: body,
                raw_score: rank,
                normalized_score: normalized * 0.3, // Lowest weight for body
                lane: "body_fts".to_string(),
                name_path,
                duplicate_index: dup_idx,
            });
        }

        Ok(())
    }

    /// Short token fallback (SQL LIKE for tokens < 3 chars).
    fn execute_short_token_fallback(
        &self,
        query: &AnalyzedQuery,
        top_k: usize,
        results: &mut Vec<LaneResult>,
    ) -> Result<(), crate::fts5_store::Fts5StoreError> {
        // Use LIKE for short tokens that can't use FTS effectively
        let sql = "SELECT id, file_id, name, kind, start_line, end_line, body,
                          name_path, duplicate_index
                   FROM fts5_symbols
                   WHERE name LIKE ?1 OR body LIKE ?2
                   ORDER BY
                     CASE WHEN name LIKE ?1 THEN 0 ELSE 1 END,
                     length(name)
                   LIMIT ?3";

        let pattern = format!("%{}%", query.raw);
        let mut stmt = self.store.conn.prepare(sql).map_err(|e| {
            crate::fts5_store::Fts5StoreError::Other(format!("prepare failed: {e}"))
        })?;

        let rows = stmt
            .query_map(params![pattern, pattern, top_k as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i32>(8)?,
                ))
            })
            .map_err(|e| crate::fts5_store::Fts5StoreError::Other(format!("query failed: {e}")))?;

        for row in rows.flatten() {
            let (sym_id, file_id, name, kind, start, end, body, name_path, dup_idx) = row;
            // Skip if already found
            if results.iter().any(|r| r.symbol_id == sym_id) {
                continue;
            }

            results.push(LaneResult {
                symbol_id: sym_id,
                file_id,
                file_path: String::new(),
                symbol_name: name,
                symbol_kind: kind,
                start_line: start,
                end_line: end,
                snippet: body,
                raw_score: 0.5,
                normalized_score: 0.2, // Low weight for fallback
                lane: "short_token_fallback".to_string(),
                name_path,
                duplicate_index: dup_idx,
            });
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Result fusion
    // -----------------------------------------------------------------------

    /// Fuse results from multiple lanes, deduplicate, and rank.
    fn fuse_results(&self, results: Vec<LaneResult>, top_k: usize) -> Vec<FusedResult> {
        if results.is_empty() {
            return Vec::new();
        }

        // Group by symbol_id (or file_id for path matches)
        let mut grouped: std::collections::HashMap<i64, Vec<LaneResult>> =
            std::collections::HashMap::new();

        for result in results {
            let key = if result.symbol_id > 0 {
                result.symbol_id
            } else {
                // Path match: use negative file_id as key
                -result.file_id
            };
            grouped.entry(key).or_default().push(result);
        }

        // For each group, compute fused score
        let mut fused: Vec<FusedResult> = grouped
            .into_values()
            .map(|group| {
                let best = group
                    .iter()
                    .max_by(|a, b| {
                        a.normalized_score
                            .partial_cmp(&b.normalized_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap();

                let matched_lanes: Vec<String> = group.iter().map(|r| r.lane.clone()).collect();

                // Fused score: max normalized score + bonus for multi-lane matches
                let lane_bonus = (matched_lanes.len() as f64 - 1.0) * 0.1;
                let fused_score = best.normalized_score + lane_bonus;

                FusedResult {
                    symbol_id: best.symbol_id,
                    file_id: best.file_id,
                    file_path: best.file_path.clone(),
                    symbol_name: best.symbol_name.clone(),
                    symbol_kind: best.symbol_kind.clone(),
                    start_line: best.start_line,
                    end_line: best.end_line,
                    snippet: best.snippet.clone(),
                    score: fused_score,
                    best_lane: best.lane.clone(),
                    matched_lanes,
                    name_path: best.name_path.clone(),
                    duplicate_index: best.duplicate_index,
                }
            })
            .collect();

        // Sort by score (descending)
        fused.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Truncate to top_k
        fused.truncate(top_k);

        fused
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_exact_symbol() {
        let q = AnalyzedQuery::analyze("Foo");
        assert!(q.is_exact_symbol);
        assert!(!q.is_path);
        assert_eq!(q.tokens, vec!["foo"]);
    }

    #[test]
    fn analyze_path_query() {
        let q = AnalyzedQuery::analyze("src/main.rs");
        assert!(!q.is_exact_symbol);
        assert!(q.is_path);
    }

    #[test]
    fn analyze_camel_case() {
        let q = AnalyzedQuery::analyze("getUserById");
        // Multi-token camelCase is NOT treated as exact symbol
        assert!(!q.is_exact_symbol);
        assert_eq!(q.tokens, vec!["get", "user", "by", "id"]);
    }

    #[test]
    fn lane_selection_for_exact_symbol() {
        let q = AnalyzedQuery::analyze("Foo");
        let lanes = LaneSelection::for_query(&q);
        assert!(lanes.exact_symbol_sql);
        assert!(lanes.prefix_symbol_sql);
        assert!(lanes.symbol_fts);
        assert!(!lanes.path_fts);
        assert!(lanes.body_fts);
    }

    #[test]
    fn lane_selection_for_path() {
        let q = AnalyzedQuery::analyze("src/main.rs");
        let lanes = LaneSelection::for_query(&q);
        assert!(!lanes.exact_symbol_sql);
        assert!(lanes.path_fts);
    }

    #[test]
    fn lane_selection_for_short_token() {
        let q = AnalyzedQuery::analyze("id");
        let lanes = LaneSelection::for_query(&q);
        assert!(lanes.short_token_fallback);
    }

    #[test]
    fn exact_symbol_sql_returns_correct_file_path() {
        let store = Fts5Store::open_in_memory().unwrap();

        // Insert a file with a known path
        let file_id = store
            .upsert_file("src/lib.rs", "abc", 1000, 512, 1)
            .unwrap();

        // Insert a symbol referencing that file
        store
            .upsert_symbol(
                file_id,
                "MyFunc",
                "function",
                5,
                15,
                "fn MyFunc() {}",
                "",
                "",
            )
            .unwrap();

        // Search for the exact symbol
        let planner = QueryPlanner::new(&store);
        let results = planner.search("MyFunc", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol_name, "MyFunc");
        // The file path must be the real path, not empty or unknown
        assert_eq!(
            results[0].file_path, "src/lib.rs",
            "exact symbol lookup must return the correct file path"
        );
    }

    #[test]
    fn fuse_deduplicates_by_symbol_id() {
        let results = vec![
            LaneResult {
                symbol_id: 1,
                file_id: 1,
                file_path: "a.rs".into(),
                symbol_name: "Foo".into(),
                symbol_kind: "struct".into(),
                start_line: 1,
                end_line: 5,
                snippet: "struct Foo {}".into(),
                raw_score: 0.0,
                normalized_score: 1.0,
                lane: "exact_symbol_sql".into(),
                name_path: "".into(),
                duplicate_index: 0,
            },
            LaneResult {
                symbol_id: 1,
                file_id: 1,
                file_path: "a.rs".into(),
                symbol_name: "Foo".into(),
                symbol_kind: "struct".into(),
                start_line: 1,
                end_line: 5,
                snippet: "struct Foo {}".into(),
                raw_score: -2.0,
                normalized_score: 0.33,
                lane: "body_fts".into(),
                name_path: "".into(),
                duplicate_index: 0,
            },
        ];

        let store = Fts5Store::open_in_memory().unwrap();
        let planner = QueryPlanner::new(&store);
        let fused = planner.fuse_results(results, 10);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].matched_lanes.len(), 2);
        assert!(fused[0].score > 1.0); // Bonus for multi-lane
    }

    #[test]
    fn name_path_included_in_search_results() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/lib.rs", "abc", 1000, 512, 1)
            .unwrap();

        // Insert a nested symbol with name_path
        store
            .upsert_symbol(
                file_id,
                "inner",
                "function",
                5,
                10,
                "fn inner() {}",
                "MyClass::inner",
                "abc123",
            )
            .unwrap();

        let planner = QueryPlanner::new(&store);
        let results = planner.search("inner", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name_path, "MyClass::inner");
    }

    #[test]
    fn duplicate_index_disambiguates_same_name_symbols() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/lib.rs", "abc", 1000, 512, 1)
            .unwrap();

        // Insert two symbols with same name (different kinds)
        let id1 = store
            .upsert_symbol(
                file_id,
                "overloaded",
                "function",
                1,
                5,
                "fn overloaded(a: i32) {}",
                "",
                "hash1",
            )
            .unwrap();

        let id2 = store
            .upsert_symbol(
                file_id,
                "overloaded",
                "function",
                7,
                11,
                "fn overloaded(a: i32, b: i32) {}",
                "",
                "hash2",
            )
            .unwrap();

        assert_ne!(id1, id2);

        let sym1 = store.get_symbol(id1).unwrap().unwrap();
        let sym2 = store.get_symbol(id2).unwrap().unwrap();

        assert_ne!(sym1.duplicate_index, sym2.duplicate_index);
    }
}
