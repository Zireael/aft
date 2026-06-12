//! Experimental FTS5 lexical backend for benchmark comparison.
//!
//! This module is behind the `semantic-fts5` Cargo feature and provides
//! a SQLite FTS5-based full-text search index for comparison against
//! the existing trigram-based search in `search_index.rs`.
//!
//! **Not for production use.** This is a benchmark/spike module to evaluate
//! whether FTS5's BM25 scoring and phrase/prefix queries improve symbol
//! and exact-lookup retrieval quality.

use rusqlite::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static FTS5_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Check if FTS5 is available in the bundled SQLite build.
///
/// Creates an in-memory database, attempts to create an FTS5 virtual table,
/// and returns whether the operation succeeded. This is a one-time check;
/// subsequent calls return the cached result.
pub fn check_fts5_available() -> bool {
    // Fast path: already checked
    if FTS5_AVAILABLE.load(Ordering::Relaxed) {
        return true;
    }

    let result = Connection::open_in_memory()
        .and_then(|conn| {
            conn.execute_batch("CREATE VIRTUAL TABLE _fts5_test USING fts5(content);")?;
            conn.execute_batch("DROP TABLE _fts5_test;")?;
            Ok(())
        })
        .is_ok();

    FTS5_AVAILABLE.store(result, Ordering::Relaxed);
    result
}

/// Split a code symbol into searchable tokens.
///
/// Handles common code naming conventions:
/// - CamelCase: `getUserById` → `["get", "user", "by", "id"]`
/// - snake_case: `snake_case` → `["snake", "case"]`
/// - Namespaced: `Foo::bar` → `["foo", "bar"]`
/// - Dotted: `a.b.c` → `["a", "b", "c"]`
/// - Arrow: `->method` → `["method"]`
/// - Generic: `Client<T>` → `["client", "t"]`
/// - Hyphenated: `some-name` → `["some", "name"]`
pub fn tokenize_code_symbol(symbol: &str) -> Vec<String> {
    let mut tokens = Vec::new();

    // Split on common code separators
    for part in symbol.split([':', '.', '>', '<', '(', ')', '[', ']', ' ']) {
        let part = part.trim_matches(['-', '_', '>', '<']);
        if part.is_empty() {
            continue;
        }

        // Further split CamelCase boundaries
        for sub_part in split_camel_case(part) {
            let lower = sub_part.to_lowercase();
            if !lower.is_empty() && !is_stop_token(&lower) {
                tokens.push(lower);
            }
        }
    }

    tokens
}

/// Split a string on CamelCase boundaries.
///
/// `getUserById` → `["get", "User", "By", "Id"]`
/// `HTTPServer` → `["HTTP", "Server"]`
/// `parseXML` → `["parse", "XML"]`
fn split_camel_case(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut prev_was_upper = false;

    for c in s.chars() {
        if c.is_uppercase() || c.is_ascii_digit() {
            if !current.is_empty() && !prev_was_upper {
                tokens.push(std::mem::take(&mut current));
            }
            current.push(c);
            prev_was_upper = c.is_uppercase();
        } else if c == '_' || c == '-' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_was_upper = false;
        } else {
            if !current.is_empty() && prev_was_upper && current.len() > 1 {
                let last = current.pop().unwrap();
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                current.push(last);
            }
            current.push(c);
            prev_was_upper = false;
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Check if a token is a common stop word that shouldn't be indexed.
fn is_stop_token(token: &str) -> bool {
    matches!(
        token,
        "an" | "the" | "is" | "in" | "on" | "at" | "to" | "for" | "of" | "with"
    )
}

/// Escape special FTS5 query characters.
///
/// FTS5 special characters: `"`, `*`, `(`, `)`, `:`, `^`, `{`, `}`
/// These are escaped by wrapping in double quotes within a phrase query.
///
/// # Examples
///
/// ```
/// use aft::fts5_experimental::escape_fts5_query;
/// // In FTS5, the colon in "Foo::bar" must be escaped
/// assert_eq!(escape_fts5_query("Foo::bar"), "Foo\"\"::\"\"bar");
/// ```
pub fn escape_fts5_query(query: &str) -> String {
    let mut result = String::with_capacity(query.len() * 2);
    let chars: Vec<char> = query.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        match chars[i] {
            '}' => {
                result.push('"');
                result.push('"');
                result.push('}');
                result.push('"');
                i += 1;
            }
            '"' | '*' | '(' | ')' | ':' | '^' | '{' => {
                let start = i;
                while i < len && matches!(chars[i], '"' | '*' | '(' | ')' | ':' | '^' | '{') {
                    i += 1;
                }
                let group_len = i - start;
                if group_len == 1 {
                    result.push('"');
                    result.push(chars[start]);
                    result.push('"');
                } else {
                    result.push('"');
                    result.push('"');
                    for j in start..i {
                        result.push(chars[j]);
                    }
                    result.push('"');
                    result.push('"');
                }
            }
            _ => {
                result.push(chars[i]);
                i += 1;
            }
        }
    }

    result
}

/// Build an FTS5 MATCH query from a user search string.
///
/// Strategy:
/// 1. Tokenize the query into code-aware tokens
/// 2. Escape each token
/// 3. Join with implicit AND (space-separated in FTS5)
///
/// For exact symbol matches, wrap in quotes for phrase matching.
pub fn build_fts5_query(query: &str, exact: bool) -> String {
    let tokens = tokenize_code_symbol(query);

    if tokens.is_empty() {
        return escape_fts5_query(query);
    }

    if exact {
        // Phrase query for exact symbol match
        let escaped: Vec<String> = tokens.iter().map(|t| escape_fts5_query(t)).collect();
        format!("\"{}\"", escaped.join(" "))
    } else {
        // Implicit AND between tokens
        tokens
            .iter()
            .map(|t| escape_fts5_query(t))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Result from an FTS5 search.
#[derive(Debug, Clone)]
pub struct Fts5Result {
    pub file_path: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub snippet: String,
    pub rank: f64, // BM25 score (lower is better in FTS5)
}

/// Statistics about an FTS5 index.
#[derive(Debug, Clone)]
pub struct Fts5Stats {
    pub chunk_count: usize,
    pub file_count: usize,
    pub index_size_bytes: u64,
}

/// An experimental FTS5-based search index for benchmark comparison.
///
/// This is a self-contained index that can be built from source file chunks
/// and queried using FTS5's BM25 ranking. It does NOT integrate with
/// AFT's existing `SearchIndex` or `SemanticIndex`.
pub struct Fts5Index {
    conn: Connection,
}

impl Fts5Index {
    /// Create a new FTS5 index backed by an in-memory SQLite database.
    pub fn new() -> Result<Self, String> {
        if !check_fts5_available() {
            return Err("FTS5 is not available in this SQLite build".into());
        }

        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;

        conn.execute_batch(
            "CREATE VIRTUAL TABLE code_chunks USING fts5(
                file_path,
                symbol_name,
                symbol_kind,
                content,
                tokenize='trigram'
            );",
        )
        .map_err(|e| e.to_string())?;

        Ok(Self { conn })
    }

    /// Create a new FTS5 index backed by a file on disk.
    pub fn open(path: &Path) -> Result<Self, String> {
        if !check_fts5_available() {
            return Err("FTS5 is not available in this SQLite build".into());
        }

        let conn = Connection::open(path).map_err(|e| e.to_string())?;

        // Create table if it doesn't exist
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS code_chunks USING fts5(
                file_path,
                symbol_name,
                symbol_kind,
                content,
                tokenize='trigram'
            );",
        )
        .map_err(|e| e.to_string())?;

        Ok(Self { conn })
    }

    /// Index a code chunk (file or symbol).
    pub fn index_chunk(
        &self,
        file_path: &str,
        symbol_name: Option<&str>,
        symbol_kind: Option<&str>,
        content: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO code_chunks (file_path, symbol_name, symbol_kind, content) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![file_path, symbol_name, symbol_kind, content],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Search the index using BM25 ranking.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<Fts5Result>, String> {
        let fts_query = build_fts5_query(query, false);

        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, symbol_name, symbol_kind,
                        snippet(code_chunks, 3, '<mark>', '</mark>', '...', 32) as snippet,
                        rank
                 FROM code_chunks
                 WHERE code_chunks MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;

        let results = stmt
            .query_map(rusqlite::params![fts_query, top_k as i64], |row| {
                Ok(Fts5Result {
                    file_path: row.get(0)?,
                    symbol_name: row.get(1)?,
                    symbol_kind: row.get(2)?,
                    snippet: row.get(3)?,
                    rank: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Get index statistics.
    pub fn stats(&self) -> Result<Fts5Stats, String> {
        let chunk_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM code_chunks", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;

        let file_count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT file_path) FROM code_chunks",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;

        Ok(Fts5Stats {
            chunk_count: chunk_count as usize,
            file_count: file_count as usize,
            index_size_bytes: 0, // In-memory; would need DB file size for on-disk
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts5_availability_check() {
        // Should succeed with bundled SQLite
        let available = check_fts5_available();
        assert!(available, "FTS5 should be available in bundled SQLite");
    }

    #[test]
    fn tokenize_camel_case() {
        let tokens = tokenize_code_symbol("getUserById");
        assert_eq!(tokens, vec!["get", "user", "by", "id"]);
    }

    #[test]
    fn tokenize_snake_case() {
        let tokens = tokenize_code_symbol("snake_case_name");
        assert_eq!(tokens, vec!["snake", "case", "name"]);
    }

    #[test]
    fn tokenize_namespaced() {
        let tokens = tokenize_code_symbol("Foo::bar");
        assert_eq!(tokens, vec!["foo", "bar"]);
    }

    #[test]
    fn tokenize_dotted() {
        let tokens = tokenize_code_symbol("a.b.c");
        assert_eq!(tokens, vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenize_arrow() {
        let tokens = tokenize_code_symbol("->method");
        assert_eq!(tokens, vec!["method"]);
    }

    #[test]
    fn tokenize_generic() {
        let tokens = tokenize_code_symbol("Client<T>");
        assert_eq!(tokens, vec!["client", "t"]);
    }

    #[test]
    fn tokenize_hyphenated() {
        let tokens = tokenize_code_symbol("some-name");
        assert_eq!(tokens, vec!["some", "name"]);
    }

    #[test]
    fn tokenize_mixed() {
        let tokens = tokenize_code_symbol("std::io::Result<Option<String>>");
        assert_eq!(tokens, vec!["std", "io", "result", "option", "string"]);
    }

    #[test]
    fn tokenize_filters_stop_words() {
        let tokens = tokenize_code_symbol("the_value_of_a_key");
        assert_eq!(tokens, vec!["value", "a", "key"]);
    }

    #[test]
    fn escape_fts5_special_chars() {
        assert_eq!(escape_fts5_query("Foo::bar"), "Foo\"\"::\"\"bar");
        assert_eq!(escape_fts5_query("a*b"), "a\"*\"b");
        assert_eq!(escape_fts5_query("a(b)"), "a\"(\"b\")\"");
        assert_eq!(escape_fts5_query("a^b{c}"), "a\"^\"b\"{\"c\"\"}\"");
    }

    #[test]
    fn escape_fts5_no_special_chars() {
        assert_eq!(escape_fts5_query("simple_query"), "simple_query");
        assert_eq!(escape_fts5_query("getUserById"), "getUserById");
    }

    #[test]
    fn build_fts5_query_implicit_and() {
        let q = build_fts5_query("error handling", false);
        assert!(q.contains("error"));
        assert!(q.contains("handling"));
        // Tokens are joined with space (implicit AND)
        assert!(!q.contains("OR"));
    }

    #[test]
    fn build_fts5_query_exact_phrase() {
        let q = build_fts5_query("Router", true);
        // Should be wrapped in quotes for phrase matching
        assert!(q.starts_with('"'));
        assert!(q.ends_with('"'));
    }

    #[test]
    fn fts5_index_and_search() {
        if !check_fts5_available() {
            return; // Skip if FTS5 unavailable
        }

        let index = Fts5Index::new().unwrap();

        // Index some code chunks
        index
            .index_chunk(
                "src/router.rs",
                Some("Router"),
                Some("struct"),
                "pub struct Router { routes: Vec<Route> }",
            )
            .unwrap();
        index
            .index_chunk(
                "src/handler.rs",
                Some("handle_request"),
                Some("fn"),
                "async fn handle_request(req: Request) -> Response { ... }",
            )
            .unwrap();
        index
            .index_chunk(
                "src/middleware.rs",
                Some("Middleware"),
                Some("trait"),
                "pub trait Middleware { fn call(&self, req: Request) -> Request; }",
            )
            .unwrap();

        // Search for "Router"
        let results = index.search("Router", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].file_path, "src/router.rs");
        assert_eq!(results[0].symbol_name.as_deref(), Some("Router"));

        // Search for "handle"
        let results = index.search("handle", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.file_path == "src/handler.rs"));

        // Stats
        let stats = index.stats().unwrap();
        assert_eq!(stats.chunk_count, 3);
        assert_eq!(stats.file_count, 3);
    }

    #[test]
    fn split_camel_case_consecutive_uppercase() {
        let tokens = split_camel_case("HTTPServer");
        assert_eq!(tokens, vec!["HTTP", "Server"]);
    }

    #[test]
    fn split_camel_case_all_uppercase() {
        let tokens = split_camel_case("XML");
        assert_eq!(tokens, vec!["XML"]);
    }

    #[test]
    fn split_camel_case_single_char() {
        let tokens = split_camel_case("x");
        assert_eq!(tokens, vec!["x"]);
    }
}
