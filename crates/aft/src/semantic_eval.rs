//! Local semantic retrieval eval harness.
//!
//! Provides a small, dependency-free format and scoring surface so users can
//! measure whether their embedding model and chunking choices retrieve the
//! files and symbols they expect for a known set of natural-language queries.
//!
//! # File format
//!
//! Each line of `.aft/semantic-eval.jsonl` is one [`EvalCase`]:
//!
//! ```text
//! {"query":"where is JWT validation handled","expected_paths":["src/auth/session.ts","src/middleware/auth.ts"]}
//! {"query":"how is the semantic index refreshed","expected_symbols":["refresh_semantic_index","SemanticIndex::refresh"]}
//! ```
//!
//! Expected paths are matched exactly or by suffix (so a query that says
//! `"src/auth/session.ts"` matches a retrieved `"src/auth/session.ts"` *and*
//! `"some/prefix/src/auth/session.ts"`). Expected symbols match the symbol
//! name (with optional `::` / `.` separators) by case-sensitive equality.
//!
//! # Scoring
//!
//! Each case is scored against an ordered list of retrieved (path, symbol)
//! pairs. Two headline metrics are produced:
//!
//! - **recall@k** — fraction of cases where at least one expected hit is in
//!   the first *k* retrieved results.
//! - **mrr** — mean reciprocal rank across cases, treating the first
//!   position of *any* matching hit as the rank (1-indexed). Cases with no
//!   hit contribute 0.
//!
//! Both metrics are simple, well-known, and easy to interpret. They make no
//! claim about absolute model quality; they are a measurement, not a
//! verdict. Use them to compare configurations, not to grade models.

use std::collections::HashSet;
use std::path::Path;

/// A single eval case — one query and what the user expects to retrieve.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct EvalCase {
    /// The natural-language query to run.
    pub query: String,
    /// Paths the user expects to find in the top results.
    /// Empty/missing is fine — the case is then path-blind.
    #[serde(default)]
    pub expected_paths: Vec<String>,
    /// Symbols the user expects to find in the top results.
    /// Empty/missing is fine — the case is then symbol-blind.
    #[serde(default)]
    pub expected_symbols: Vec<String>,
    /// Optional override for `k` used by recall@k for this case.
    /// Falls back to the runner's default `k` if absent.
    #[serde(default)]
    pub top_k: Option<usize>,
}

impl EvalCase {
    /// Returns true when the case has at least one path or symbol expectation.
    pub fn has_expectations(&self) -> bool {
        !self.expected_paths.is_empty() || !self.expected_symbols.is_empty()
    }
}

/// A retrieved result — what the search pipeline returned for a single query.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct RetrievedHit {
    /// Path of the file the hit came from.
    pub path: String,
    /// Optional symbol name within the file. Empty/None means path-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
}

/// Per-case scoring outcome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalCaseResult {
    /// 1-based index of the case in the original suite.
    pub index: usize,
    /// Echo of the original query.
    pub query: String,
    /// 1-based rank of the first matching hit, or 0 when nothing matched.
    pub first_hit_rank: usize,
    /// Reciprocal rank contribution (0.0 when no hit).
    pub reciprocal_rank: f64,
    /// True when at least one expected hit appears in the top `k`.
    pub hit_in_top_k: bool,
    /// True when at least one expected hit appears anywhere in the retrieved
    /// set (even if outside `k`).
    pub hit_anywhere: bool,
    /// The `k` used for this case.
    pub k: usize,
    /// Number of retrieved results scored (truncated to `k`).
    pub retrieved_count: usize,
    /// Total number of expected paths/symbols in the case.
    pub expectation_count: usize,
    /// Number of expected paths/symbols that appeared anywhere in the
    /// retrieved set (counted, not just boolean).
    pub expectations_matched: usize,
}

/// Aggregate scoring across a whole suite.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalSummary {
    /// Total cases in the suite.
    pub total: usize,
    /// Cases that contributed a non-zero reciprocal rank.
    pub hits_in_top_k: usize,
    /// `hits_in_top_k / total`. 0.0 when `total == 0`.
    pub recall_at_k: f64,
    /// Mean reciprocal rank across all cases.
    pub mrr: f64,
    /// `k` used to score recall (the runner default, not per-case).
    pub k: usize,
    /// Per-case results in input order.
    pub cases: Vec<EvalCaseResult>,
}

impl EvalSummary {
    /// Render a one-line human-readable summary suitable for `aft doctor`.
    pub fn render_line(&self) -> String {
        format!(
            "eval: {}/{} hits, recall@{}={:.3}, mrr={:.3}",
            self.hits_in_top_k, self.total, self.k, self.recall_at_k, self.mrr
        )
    }
}

/// Strip trailing commas before `}` or `]` so hand-edited JSONL files
/// with trailing commas parse correctly.
fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        out.push(bytes[i] as char);
        // Look for `,` followed by optional whitespace then `}` or `]`.
        if bytes[i] == b',' {
            let mut j = i + 1;
            while j < len
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < len && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma (and any trailing whitespace we already consumed).
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Parse a JSONL document into eval cases.
///
/// Each non-empty, non-comment line must be a valid JSON object with a
/// `query` string field. Trailing commas, blank lines, and `#` comment
/// lines are tolerated so eval files can be hand-edited.
pub fn parse_jsonl(text: &str) -> Result<Vec<EvalCase>, String> {
    let mut out = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cleaned = strip_trailing_commas(trimmed);
        let case: EvalCase =
            serde_json::from_str(&cleaned).map_err(|e| format!("line {}: {e}", line_no + 1))?;
        if case.query.trim().is_empty() {
            return Err(format!("line {}: query must be non-empty", line_no + 1));
        }
        out.push(case);
    }
    Ok(out)
}

/// True when `retrieved_path` matches an expected path.
///
/// Matches:
/// - exact string equality, or
/// - `retrieved_path` ends with `expected_path` (after a path separator),
///   so users can write `"src/auth/session.ts"` and still match a
///   retrieved `"x/src/auth/session.ts"`.
pub fn path_matches(retrieved_path: &str, expected_path: &str) -> bool {
    if retrieved_path == expected_path {
        return true;
    }
    // Normalize backslashes to forward slashes for cross-platform comparison.
    let retrieved_fwd = retrieved_path.replace('\\', "/");
    let expected_fwd = expected_path.replace('\\', "/");
    if retrieved_fwd == expected_fwd {
        return true;
    }
    // Strip trailing slashes for comparison — "src/auth/" should match "src/auth".
    let retrieved_stripped = retrieved_fwd.trim_end_matches('/');
    let expected_stripped = expected_fwd.trim_end_matches('/');
    if retrieved_stripped == expected_stripped {
        return true;
    }
    // Check if the normalized paths have the same filename.
    let retrieved = Path::new(retrieved_stripped);
    let expected = Path::new(expected_stripped);
    if let (Some(retrieved_file), Some(expected_file)) =
        (retrieved.file_name(), expected.file_name())
    {
        if retrieved_file != expected_file {
            return false;
        }
    }
    // Check that the retrieved path ends with the expected path at a separator boundary.
    // e.g., "repo/src/auth.rs" should match "src/auth.rs" but NOT "xxsrc/auth.rs".
    if retrieved_stripped.ends_with(expected_stripped) {
        let suffix_start = retrieved_stripped.len() - expected_stripped.len();
        if suffix_start == 0 || retrieved_stripped.as_bytes().get(suffix_start - 1) == Some(&b'/') {
            return true;
        }
    }
    false
}

/// True when a retrieved symbol matches an expected symbol.
///
/// `expected` may be written with `::` or `.` (Rust vs. other-language
/// separators); the retrieved side is compared as-given, then with the
/// `::` ↔ `.` substitution.
pub fn symbol_matches(retrieved: &str, expected: &str) -> bool {
    if retrieved == expected {
        return true;
    }
    let retrieved_norm = retrieved.replace("::", ".");
    let expected_norm = expected.replace("::", ".");
    if retrieved_norm == expected_norm {
        return true;
    }
    // Suffix match: "validateToken" expected matches retrieved "Auth::validateToken".
    let last_segment = expected_norm
        .rsplit('.')
        .next()
        .unwrap_or(expected_norm.as_str());
    if last_segment == retrieved_norm
        || retrieved_norm.ends_with(&format!(".{last_segment}"))
        || retrieved_norm.ends_with(&format!("::{last_segment}"))
    {
        return true;
    }
    false
}

/// Score a single case against its retrieved hits.
///
/// `k` is the runner default; the case's own `top_k` (if set) overrides it.
/// Hits beyond `k` still count toward `hit_anywhere` and
/// `expectations_matched` but not toward `first_hit_rank` or `hit_in_top_k`.
pub fn score_case(case: &EvalCase, retrieved: &[RetrievedHit], default_k: usize) -> EvalCaseResult {
    let k = case.top_k.unwrap_or(default_k).max(1);
    let expectation_count = case.expected_paths.len() + case.expected_symbols.len();
    let truncated = retrieved;

    let mut first_hit_rank: Option<usize> = None;
    let mut expectations_matched: HashSet<String> = HashSet::new();

    for (idx, hit) in truncated.iter().enumerate() {
        let rank = idx + 1;
        let mut hit_this_position = false;
        for expected in &case.expected_paths {
            if path_matches(&hit.path, expected) {
                hit_this_position = true;
                expectations_matched.insert(format!("path:{expected}"));
            }
        }
        if let Some(sym) = &hit.symbol {
            for expected in &case.expected_symbols {
                if symbol_matches(sym, expected) {
                    hit_this_position = true;
                    expectations_matched.insert(format!("sym:{expected}"));
                }
            }
        }
        if hit_this_position && first_hit_rank.is_none() {
            first_hit_rank = Some(rank);
        }
    }

    let first_hit_rank_val = first_hit_rank.unwrap_or(0);
    let hit_in_top_k = first_hit_rank_val > 0 && first_hit_rank_val <= k;
    let hit_anywhere = first_hit_rank_val > 0;
    let reciprocal_rank = if first_hit_rank_val > 0 {
        1.0 / first_hit_rank_val as f64
    } else {
        0.0
    };

    EvalCaseResult {
        index: 0, // patched by `score_suite`
        query: case.query.clone(),
        first_hit_rank: first_hit_rank_val,
        reciprocal_rank,
        hit_in_top_k,
        hit_anywhere,
        k,
        retrieved_count: truncated.len(),
        expectation_count,
        expectations_matched: expectations_matched.len(),
    }
}

/// Score a whole suite. `default_k` is the global cutoff for recall@k; cases
/// may override it with `top_k`.
pub fn score_suite(
    cases: &[EvalCase],
    results: &[Vec<RetrievedHit>],
    default_k: usize,
) -> EvalSummary {
    assert_eq!(cases.len(), results.len(), "cases/results length mismatch");
    let mut case_results = Vec::with_capacity(cases.len());
    let mut hits_in_top_k = 0usize;
    let mut mrr_sum = 0.0f64;
    for (idx, (case, retrieved)) in cases.iter().zip(results.iter()).enumerate() {
        let mut result = score_case(case, retrieved, default_k);
        result.index = idx;
        if result.hit_in_top_k {
            hits_in_top_k += 1;
        }
        mrr_sum += result.reciprocal_rank;
        case_results.push(result);
    }
    let total = cases.len();
    let recall_at_k = if total == 0 {
        0.0
    } else {
        hits_in_top_k as f64 / total as f64
    };
    let mrr = if total == 0 {
        0.0
    } else {
        mrr_sum / total as f64
    };
    EvalSummary {
        total,
        hits_in_top_k,
        recall_at_k,
        mrr,
        k: default_k,
        cases: case_results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(path: &str, symbol: Option<&str>) -> RetrievedHit {
        RetrievedHit {
            path: path.to_string(),
            symbol: symbol.map(|s| s.to_string()),
        }
    }

    fn case(query: &str, paths: &[&str], symbols: &[&str]) -> EvalCase {
        EvalCase {
            query: query.to_string(),
            expected_paths: paths.iter().map(|s| s.to_string()).collect(),
            expected_symbols: symbols.iter().map(|s| s.to_string()).collect(),
            top_k: None,
        }
    }

    #[test]
    fn parse_jsonl_accepts_valid_lines() {
        let text = r#"{"query":"q1","expected_paths":["a.rs"]}
{"query":"q2","expected_symbols":["foo"]}
"#;
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].query, "q1");
        assert_eq!(cases[1].expected_symbols, vec!["foo".to_string()]);
    }

    #[test]
    fn parse_jsonl_skips_blank_and_comment_lines() {
        let text = r#"
# header comment
{"query":"q1"}

   # indented comment
{"query":"q2"}
"#;
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases.len(), 2);
    }

    #[test]
    fn parse_jsonl_rejects_invalid_json() {
        let text = r#"{"query":"q1","expected_paths":["a.rs"]}
not json
"#;
        let err = parse_jsonl(text).unwrap_err();
        assert!(err.contains("line 2"), "got: {err}");
    }

    #[test]
    fn parse_jsonl_rejects_empty_query() {
        let text = r#"{"query":"   "}
"#;
        let err = parse_jsonl(text).unwrap_err();
        assert!(err.contains("query must be non-empty"), "got: {err}");
    }

    #[test]
    fn parse_jsonl_rejects_missing_query_field() {
        let text = r#"{"expected_paths":["a.rs"]}
"#;
        let err = parse_jsonl(text).unwrap_err();
        assert!(err.contains("line 1"), "got: {err}");
    }

    #[test]
    fn parse_jsonl_accepts_empty_expectations() {
        let text = r#"{"query":"q1"}
"#;
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases.len(), 1);
        assert!(!cases[0].has_expectations());
    }

    #[test]
    fn parse_jsonl_parses_top_k_override() {
        let text = r#"{"query":"q1","top_k":3}
"#;
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases[0].top_k, Some(3));
    }

    #[test]
    fn parse_jsonl_accepts_trailing_commas() {
        let text = r#"{"query":"q1","expected_paths":["a.rs"],}
{"query":"q2","expected_symbols":["foo"],}
"#;
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].query, "q1");
        assert_eq!(cases[1].expected_symbols, vec!["foo".to_string()]);
    }

    #[test]
    fn parse_jsonl_accepts_trailing_commas_with_whitespace() {
        let text = "{\"query\":\"q1\",\"expected_paths\":[\"a.rs\"],   }\n";
        let cases = parse_jsonl(text).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].query, "q1");
    }

    #[test]
    fn strip_trailing_commas_only_removes_before_braces() {
        // Commas NOT followed by } or ] should be kept.
        let input = r#"{"a":"b,c","d":[1,2]}"#;
        let cleaned = strip_trailing_commas(input);
        assert_eq!(cleaned, input);
    }

    #[test]
    fn has_expectations_true_for_paths() {
        let c = case("q", &["a.rs"], &[]);
        assert!(c.has_expectations());
    }

    #[test]
    fn has_expectations_true_for_symbols() {
        let c = case("q", &[], &["foo"]);
        assert!(c.has_expectations());
    }

    #[test]
    fn has_expectations_false_when_both_empty() {
        let c = case("q", &[], &[]);
        assert!(!c.has_expectations());
    }

    #[test]
    fn path_matches_exact() {
        assert!(path_matches("src/auth.rs", "src/auth.rs"));
    }

    #[test]
    fn path_matches_suffix_with_separator() {
        assert!(path_matches("repo/src/auth.rs", "src/auth.rs"));
    }

    #[test]
    fn path_matches_suffix_backslash() {
        assert!(path_matches("repo\\src\\auth.rs", "src\\auth.rs"));
    }

    #[test]
    fn path_matches_rejects_unrelated() {
        assert!(!path_matches("src/other.rs", "src/auth.rs"));
    }

    #[test]
    fn path_matches_rejects_partial_filename() {
        // "auth.rs" should not match "xauth.rs"
        assert!(!path_matches("xauth.rs", "auth.rs"));
    }

    #[test]
    fn symbol_matches_exact() {
        assert!(symbol_matches("foo", "foo"));
    }

    #[test]
    fn symbol_matches_qualified() {
        assert!(symbol_matches("Auth::foo", "Auth.foo"));
        assert!(symbol_matches("Auth.foo", "Auth::foo"));
    }

    #[test]
    fn symbol_matches_suffix_qualified() {
        // expected="foo" should match retrieved "Auth::foo"
        assert!(symbol_matches("Auth::foo", "foo"));
        assert!(symbol_matches("Auth.foo", "foo"));
    }

    #[test]
    fn symbol_matches_rejects_unrelated() {
        assert!(!symbol_matches("bar", "foo"));
    }

    #[test]
    fn score_case_hit_at_rank_1() {
        let c = case("q", &["src/auth.rs"], &[]);
        let r = score_case(&c, &[hit("src/auth.rs", None)], 5);
        assert_eq!(r.first_hit_rank, 1);
        assert!(r.hit_in_top_k);
        assert!(r.hit_anywhere);
        assert!((r.reciprocal_rank - 1.0).abs() < 1e-9);
        assert_eq!(r.expectations_matched, 1);
    }

    #[test]
    fn score_case_hit_at_rank_3() {
        let c = case("q", &["src/auth.rs"], &[]);
        let r = score_case(
            &c,
            &[
                hit("src/other.rs", None),
                hit("src/another.rs", None),
                hit("src/auth.rs", None),
            ],
            5,
        );
        assert_eq!(r.first_hit_rank, 3);
        assert!((r.reciprocal_rank - 1.0 / 3.0).abs() < 1e-9);
        assert!(r.hit_in_top_k);
    }

    #[test]
    fn score_case_no_hit_yields_zero_reciprocal_rank() {
        let c = case("q", &["src/auth.rs"], &[]);
        let r = score_case(
            &c,
            &[hit("src/other.rs", None), hit("src/another.rs", None)],
            5,
        );
        assert_eq!(r.first_hit_rank, 0);
        assert_eq!(r.reciprocal_rank, 0.0);
        assert!(!r.hit_in_top_k);
        assert!(!r.hit_anywhere);
        assert_eq!(r.expectations_matched, 0);
    }

    #[test]
    fn score_case_hit_outside_top_k_is_anywhere_not_top_k() {
        let c = case("q", &["src/auth.rs"], &[]);
        let r = score_case(
            &c,
            &[
                hit("src/a.rs", None),
                hit("src/b.rs", None),
                hit("src/auth.rs", None), // rank 3
            ],
            2,
        );
        assert_eq!(r.first_hit_rank, 3);
        assert!(!r.hit_in_top_k);
        assert!(r.hit_anywhere);
    }

    #[test]
    fn score_case_symbol_match_uses_symbol_field() {
        let c = case("q", &[], &["validateToken"]);
        let r = score_case(
            &c,
            &[
                hit("src/auth.rs", Some("not_it")),
                hit("src/auth.rs", Some("validateToken")),
            ],
            5,
        );
        assert_eq!(r.first_hit_rank, 2);
        assert!(r.hit_in_top_k);
    }

    #[test]
    fn score_case_counts_each_unique_expectation_once() {
        let c = case("q", &["src/auth.rs", "src/middleware/auth.ts"], &[]);
        let r = score_case(
            &c,
            &[
                hit("src/auth.rs", None),
                hit("src/auth.rs", None), // duplicate, should not re-count
                hit("src/middleware/auth.ts", None),
            ],
            5,
        );
        assert_eq!(r.expectations_matched, 2);
    }

    #[test]
    fn score_case_per_case_top_k_override() {
        let c = case("q", &["src/auth.rs"], &[]).top_k_set(2);
        let r = score_case(
            &c,
            &[
                hit("src/a.rs", None),
                hit("src/b.rs", None),
                hit("src/auth.rs", None),
            ],
            5,
        );
        assert_eq!(r.k, 2);
        assert!(!r.hit_in_top_k); // rank 3 > k=2
        assert!(r.hit_anywhere);
    }

    // Tiny test-only helper to set top_k on a case (avoids `mut` in test fns).
    impl EvalCase {
        fn top_k_set(mut self, k: usize) -> Self {
            self.top_k = Some(k);
            self
        }
    }

    #[test]
    fn score_suite_aggregates_recall_and_mrr() {
        let cases = vec![
            case("q1", &["a.rs"], &[]),
            case("q2", &["b.rs"], &[]),
            case("q3", &["c.rs"], &[]),
        ];
        let results = vec![
            vec![hit("a.rs", None), hit("x.rs", None)], // hit @ 1
            vec![hit("x.rs", None), hit("b.rs", None)], // hit @ 2
            vec![hit("x.rs", None), hit("y.rs", None)], // miss
        ];
        let s = score_suite(&cases, &results, 5);
        assert_eq!(s.total, 3);
        assert_eq!(s.hits_in_top_k, 2);
        assert!((s.recall_at_k - 2.0 / 3.0).abs() < 1e-9);
        // MRR = (1/1 + 1/2 + 0) / 3
        assert!((s.mrr - (1.0 + 0.5 + 0.0) / 3.0).abs() < 1e-9);
    }

    #[test]
    fn score_suite_empty_suite_yields_zero() {
        let s = score_suite(&[], &[], 5);
        assert_eq!(s.total, 0);
        assert_eq!(s.recall_at_k, 0.0);
        assert_eq!(s.mrr, 0.0);
    }

    #[test]
    fn score_suite_assigns_1_based_index() {
        let cases = vec![case("q1", &["a.rs"], &[]), case("q2", &["b.rs"], &[])];
        let results = vec![vec![hit("a.rs", None)], vec![hit("b.rs", None)]];
        let s = score_suite(&cases, &results, 5);
        assert_eq!(s.cases[0].index, 0);
        assert_eq!(s.cases[1].index, 1);
    }

    #[test]
    fn summary_render_line_contains_metrics() {
        let s = EvalSummary {
            total: 3,
            hits_in_top_k: 2,
            recall_at_k: 0.6667,
            mrr: 0.5,
            k: 5,
            cases: vec![],
        };
        let line = s.render_line();
        assert!(line.contains("2/3"));
        assert!(line.contains("recall@5"));
        assert!(line.contains("mrr"));
    }

    #[test]
    fn path_matches_handles_trailing_separator() {
        assert!(path_matches("src/auth/", "src/auth/"));
        // Trailing-slash expected should match exact dir.
        assert!(path_matches("src/auth/", "src/auth"));
    }
}
