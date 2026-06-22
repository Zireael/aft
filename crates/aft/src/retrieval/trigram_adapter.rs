//! Trigram retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing trigram search index (search_index.rs) to produce
//! `CandidateSet` results for the trigram lane.

use std::path::{Path, PathBuf};

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::search_plan::{LaneKind, SearchPlan};

use super::{is_generated_path, is_vendor_path, RetrievalAdapter};

/// Trigram retrieval adapter.
///
/// Wraps the trigram search index to produce `CandidateSet` results.
///
pub struct TrigramAdapter {
    ranked_files: Vec<(PathBuf, f32)>,
}

impl TrigramAdapter {
    /// Create a new trigram adapter.
    pub fn new() -> Self {
        Self {
            ranked_files: Vec::new(),
        }
    }

    /// Create a trigram adapter from the existing lexical index ranking.
    pub fn from_ranked_files(ranked_files: Vec<(PathBuf, f32)>) -> Self {
        Self { ranked_files }
    }
}

impl Default for TrigramAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalAdapter for TrigramAdapter {
    fn retrieve(&self, query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        let source_lane = active_trigram_lane(plan);
        let max_candidates = plan
            .prefetch
            .iter()
            .find(|p| p.lane == source_lane)
            .or_else(|| plan.prefetch.iter().find(|p| p.lane == LaneKind::Trigram))
            .map(|p| p.max_candidates)
            .unwrap_or(50);

        let candidates = self
            .ranked_files
            .iter()
            .take(max_candidates)
            .enumerate()
            .map(|(rank, (file_path, score))| {
                trigram_ranked_file_to_entry(file_path, *score, rank, query, source_lane)
            })
            .collect();

        vec![CandidateSet {
            source_lane,
            candidates,
        }]
    }
}

fn active_trigram_lane(plan: &SearchPlan) -> LaneKind {
    if plan.active_safety_lane == LaneKind::TrigramBody
        || plan
            .prefetch
            .iter()
            .any(|p| p.lane == LaneKind::TrigramBody)
    {
        LaneKind::TrigramBody
    } else {
        LaneKind::Trigram
    }
}

fn trigram_ranked_file_to_entry(
    file_path: &Path,
    score: f32,
    rank: usize,
    query: &str,
    source_lane: LaneKind,
) -> CandidateEntry {
    let path_str = file_path.display().to_string();
    let (line_range, contains_literal) = first_literal_line(file_path, query);
    let is_vendor = is_vendor_path(&path_str);
    let is_generated = is_generated_path(&path_str);

    CandidateEntry {
        chunk_id: None,
        symbol_id: None,
        file_path: file_path.to_path_buf(),
        line_range,
        content_hash: None,
        score,
        rank,
        is_exact_hit: contains_literal && !is_vendor && !is_generated,
        is_vendor,
        is_generated,
        source_lane,
    }
}

fn first_literal_line(file_path: &Path, query: &str) -> (Option<(usize, usize)>, bool) {
    let needle = query.trim();
    if needle.is_empty() {
        return (None, false);
    }
    let Ok(content) = std::fs::read_to_string(file_path) else {
        return (None, false);
    };
    for (index, line) in content.lines().enumerate() {
        if line.contains(needle) {
            let line_number = index + 1;
            return (Some((line_number, line_number)), true);
        }
    }
    (None, false)
}

/// Build a CandidateEntry from a trigram search result.
///
/// is_exact_hit is true when the query exactly matches a symbol or path.
#[cfg(test)]
pub fn trigram_result_to_entry(
    file_path: &str,
    start_line: u32,
    end_line: u32,
    score: f32,
    rank: usize,
    is_exact: bool,
) -> CandidateEntry {
    let path_str = file_path.to_string();
    CandidateEntry {
        chunk_id: None,
        symbol_id: None,
        file_path: PathBuf::from(file_path),
        line_range: Some((start_line as usize, end_line as usize)),
        content_hash: None,
        score,
        rank,
        is_exact_hit: is_exact,
        is_vendor: is_vendor_path(&path_str),
        is_generated: is_generated_path(&path_str),
        source_lane: LaneKind::Trigram,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_shape::{QueryKind, QueryShape, ShapeWeights};
    use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};

    fn plan_with_trigram() -> SearchPlan {
        let shape = QueryShape {
            kind: QueryKind::Identifier,
            weights: ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: false,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    // AC-1: Returns CandidateSet with source_lane=TrigramBody when FTS5 is unavailable
    #[test]
    fn returns_trigram_lane() {
        let adapter = TrigramAdapter::new();
        let plan = plan_with_trigram();
        let result = adapter.retrieve("test_fn", &plan);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_lane, LaneKind::TrigramBody);
    }

    // Empty on failure (no panic)
    #[test]
    fn empty_on_unavailable() {
        let adapter = TrigramAdapter::new();
        let plan = plan_with_trigram();
        let result = adapter.retrieve("query", &plan);
        assert_eq!(result[0].candidates.len(), 0);
    }

    // is_vendor and is_generated propagated
    #[test]
    fn vendor_generated_propagated() {
        let entry = trigram_result_to_entry("node_modules/foo/index.ts", 1, 10, 0.9, 0, false);
        assert!(entry.is_vendor);
        assert!(!entry.is_generated);
    }

    // Exact hit for literal exact match
    #[test]
    fn exact_hit_for_literal_match() {
        let entry = trigram_result_to_entry("src/main.rs", 1, 10, 0.95, 0, true);
        assert!(entry.is_exact_hit);
    }

    #[test]
    fn ranked_file_becomes_exact_line_candidate() {
        let project = tempfile::tempdir().expect("temp project");
        let source = project.path().join("src/lib.rs");
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("create source");
        std::fs::write(
            &source,
            "pub struct SemanticBackendConfig {\n    pub model: String,\n}\n",
        )
        .expect("write source");

        let adapter = TrigramAdapter::from_ranked_files(vec![(source.clone(), 0.9)]);
        let plan = plan_with_trigram();
        let result = adapter.retrieve("SemanticBackendConfig", &plan);

        assert_eq!(result[0].source_lane, LaneKind::TrigramBody);
        assert_eq!(result[0].candidates.len(), 1);
        let candidate = &result[0].candidates[0];
        assert_eq!(candidate.file_path, source);
        assert_eq!(candidate.line_range, Some((1, 1)));
        assert!(candidate.is_exact_hit);
    }
}
