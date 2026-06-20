//! Candidate types for Retrieval Intelligence v1.
//!
//! Defines CandidateEntry, CandidateProvenance, LaneContribution, FusedCandidate,
//! and CandidateSet types per §A.3 schema contract. Types only — no wiring into
//! search dispatch yet.
//!
//! Key invariants:
//! - is_vendor and is_generated flags propagate from CandidateEntry to FusedCandidate.
//! - is_exact_hit propagates when ANY contributing CandidateEntry has it.
//! - Canonical identity priority: chunk_id > symbol_id > file_path+line_range+content_hash > path+line_range.
//! - Candidates at same canonical identity from different lanes merge provenance.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::search_plan::LaneKind;

// ---------------------------------------------------------------------------
// Canonical identity — for dedup across lanes
// ---------------------------------------------------------------------------

/// Canonical identity for deduplication across lanes.
/// Priority: chunk_id > symbol_id > (file_path + line_range + content_hash) > (file_path + line_range).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalIdentity {
    /// Primary key: chunk_id if available.
    pub chunk_id: Option<u64>,
    /// Secondary key: symbol_id if available.
    pub symbol_id: Option<u64>,
    /// Tertiary key: file path.
    pub file_path: PathBuf,
    /// Line range within file.
    pub line_range: Option<(usize, usize)>,
    /// Content hash for dedup when line_range is None.
    pub content_hash: Option<u64>,
}

impl CanonicalIdentity {
    /// Build from a CandidateEntry. Uses the priority chain from REQ-005c.
    pub fn from_entry(entry: &CandidateEntry) -> Self {
        Self {
            chunk_id: entry.chunk_id,
            symbol_id: entry.symbol_id,
            file_path: entry.file_path.clone(),
            line_range: entry.line_range,
            content_hash: entry.content_hash,
        }
    }

    /// Merge key: the highest-priority non-None identifiers.
    /// Two entries with the same merge key are considered the same canonical entity.
    ///
    /// Priority chain (REQ-005c):
    /// 1. chunk_id if Some → line_range and content_hash are irrelevant.
    /// 2. symbol_id if Some → file_path and line_range are irrelevant.
    /// 3. file_path + line_range + content_hash.
    /// 4. file_path + line_range only.
    pub fn merge_key(&self) -> CanonicalMergeKey {
        CanonicalMergeKey {
            chunk_id: self.chunk_id,
            // When chunk_id is set, symbol_id is irrelevant for dedup
            symbol_id: if self.chunk_id.is_some() {
                None
            } else {
                self.symbol_id
            },
            file_path: self.file_path.clone(),
            // When chunk_id or symbol_id is set, line_range is irrelevant for dedup
            line_range: if self.chunk_id.is_some() || self.symbol_id.is_some() {
                None
            } else {
                self.line_range
            },
        }
    }
}

/// Simplified merge key for HashMap dedup.
/// chunk_id takes priority; if both are None, falls back to symbol_id, then path+line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalMergeKey {
    pub chunk_id: Option<u64>,
    pub symbol_id: Option<u64>,
    pub file_path: PathBuf,
    pub line_range: Option<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// LaneContribution — per-lane source info
// ---------------------------------------------------------------------------

/// Contribution from a single retrieval lane.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaneContribution {
    /// Which lane produced this candidate.
    pub lane: LaneKind,
    /// Rank within that lane (0-indexed).
    pub rank_in_lane: usize,
    /// Raw score from that lane.
    pub score_in_lane: f32,
    /// RRF contribution weight (1 / (k + rank)).
    pub rrf_contribution: f32,
}

// ---------------------------------------------------------------------------
// CandidateProvenance — multi-lane origin tracking
// ---------------------------------------------------------------------------

/// Provenance of a fused candidate across multiple lanes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CandidateProvenance {
    /// All lanes that contributed this candidate.
    pub lanes: Vec<LaneContribution>,
    /// Whether this candidate came from graph expansion.
    pub is_graph_expansion: bool,
    /// Reason for graph expansion (if applicable).
    pub graph_expansion_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// CandidateEntry — raw candidate from a single lane
// ---------------------------------------------------------------------------

/// A raw candidate from a single retrieval lane, before fusion.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CandidateEntry {
    /// Primary canonical ID (chunk-level).
    pub chunk_id: Option<u64>,
    /// Secondary canonical ID (symbol-level).
    pub symbol_id: Option<u64>,
    /// File path relative to workspace root.
    pub file_path: PathBuf,
    /// Line range within file (1-indexed inclusive).
    pub line_range: Option<(usize, usize)>,
    /// Content hash for dedup fallback.
    pub content_hash: Option<u64>,
    /// Raw score from the source lane.
    pub score: f32,
    /// Rank within the source lane (0-indexed).
    pub rank: usize,
    /// Whether this is an exact symbol/path hit.
    pub is_exact_hit: bool,
    /// Whether this is a vendor file (excluded from ExactHitFloor).
    pub is_vendor: bool,
    /// Whether this is a generated file (excluded from ExactHitFloor).
    pub is_generated: bool,
    /// Which lane produced this candidate.
    pub source_lane: LaneKind,
}

// ---------------------------------------------------------------------------
// FusedCandidate — post-fusion candidate with merged provenance
// ---------------------------------------------------------------------------

/// A candidate after fusion across lanes, with merged provenance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FusedCandidate {
    /// Primary canonical ID.
    pub chunk_id: Option<u64>,
    /// Secondary canonical ID.
    pub symbol_id: Option<u64>,
    /// File path.
    pub file_path: PathBuf,
    /// Line range.
    pub line_range: Option<(usize, usize)>,
    /// Content hash.
    pub content_hash: Option<u64>,
    /// Merged provenance from all contributing lanes.
    pub provenance: CandidateProvenance,
    /// Reciprocal Rank Fusion score.
    pub rrf_score: f32,
    /// Whether ExactHitFloor promotion was applied.
    pub exact_hit_floor_applied: bool,
    /// Final score after all ranking stages.
    pub final_score: f32,
    /// Enriched context content (if available).
    pub context: Option<String>,
    /// Whether this candidate is a vendor file.
    pub is_vendor: bool,
    /// Whether this candidate is a generated file.
    pub is_generated: bool,
    /// Whether this candidate is an exact hit (OR'd from all contributing entries).
    pub is_exact_hit: bool,
}

// ---------------------------------------------------------------------------
// CandidateSet — candidates from a single lane
// ---------------------------------------------------------------------------

/// A set of candidates from a single retrieval lane.
#[derive(Debug, Clone)]
pub struct CandidateSet {
    /// Which lane produced these candidates.
    pub source_lane: LaneKind,
    /// The candidates from this lane.
    pub candidates: Vec<CandidateEntry>,
}

// ---------------------------------------------------------------------------
// Fusion helpers
// ---------------------------------------------------------------------------

/// Fuse multiple CandidateSets into a single deduplicated list of FusedCandidates.
///
/// Dedup uses canonical identity merge key:
/// - Same chunk_id → merge.
/// - Same symbol_id when both chunk_ids are None → merge.
/// - Same file_path + line_range when both chunk_id and symbol_id are None → merge.
///
/// Merging rules:
/// - provenance.lanes: concatenate all LaneContribution entries.
/// - is_exact_hit: OR across all contributing entries.
/// - is_vendor: OR across all contributing entries.
/// - is_generated: OR across all contributing entries.
/// - score: highest score wins.
/// - chunk_id/symbol_id: first non-None wins.
pub fn fuse_candidate_sets(sets: &[CandidateSet]) -> Vec<FusedCandidate> {
    let mut merged: HashMap<CanonicalMergeKey, FusedCandidate> = HashMap::new();

    for set in sets {
        for entry in &set.candidates {
            let identity = CanonicalIdentity::from_entry(entry);
            let key = identity.merge_key();

            let contribution = LaneContribution {
                lane: entry.source_lane,
                rank_in_lane: entry.rank,
                score_in_lane: entry.score,
                rrf_contribution: 0.0, // will be computed during RRF stage
            };

            merged
                .entry(key)
                .and_modify(|fused| {
                    // Merge: add lane contribution
                    fused.provenance.lanes.push(contribution.clone());
                    // OR flags
                    fused.is_exact_hit |= entry.is_exact_hit;
                    fused.is_vendor |= entry.is_vendor;
                    fused.is_generated |= entry.is_generated;
                    // Highest score wins
                    if entry.score > fused.rrf_score {
                        fused.rrf_score = entry.score;
                    }
                    // First non-None chunk_id wins
                    if fused.chunk_id.is_none() {
                        fused.chunk_id = entry.chunk_id;
                    }
                    // First non-None symbol_id wins
                    if fused.symbol_id.is_none() {
                        fused.symbol_id = entry.symbol_id;
                    }
                })
                .or_insert_with(|| FusedCandidate {
                    chunk_id: entry.chunk_id,
                    symbol_id: entry.symbol_id,
                    file_path: entry.file_path.clone(),
                    line_range: entry.line_range,
                    content_hash: entry.content_hash,
                    provenance: CandidateProvenance {
                        lanes: vec![contribution],
                        is_graph_expansion: false,
                        graph_expansion_reason: None,
                    },
                    rrf_score: entry.score,
                    exact_hit_floor_applied: false,
                    final_score: entry.score,
                    context: None,
                    is_vendor: entry.is_vendor,
                    is_generated: entry.is_generated,
                    is_exact_hit: entry.is_exact_hit,
                });
        }
    }

    merged.into_values().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        chunk_id: Option<u64>,
        file: &str,
        lane: LaneKind,
        score: f32,
        rank: usize,
    ) -> CandidateEntry {
        CandidateEntry {
            chunk_id,
            symbol_id: None,
            file_path: PathBuf::from(file),
            line_range: Some((1, 10)),
            content_hash: None,
            score,
            rank,
            is_exact_hit: false,
            is_vendor: false,
            is_generated: false,
            source_lane: lane,
        }
    }

    // AC-3: Single-lane candidate has provenance.lanes.len()==1
    #[test]
    fn single_lane_passthrough() {
        let set = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![entry(Some(1), "src/main.rs", LaneKind::Trigram, 0.9, 0)],
        };
        let fused = fuse_candidate_sets(&[set]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].provenance.lanes.len(), 1);
        assert_eq!(fused[0].provenance.lanes[0].lane, LaneKind::Trigram);
    }

    // AC-2: Two CandidateSets sharing same chunk_id merge into one FusedCandidate
    #[test]
    fn two_lane_merge_same_chunk_id() {
        let set1 = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![entry(Some(42), "src/lib.rs", LaneKind::Trigram, 0.8, 0)],
        };
        let set2 = CandidateSet {
            source_lane: LaneKind::FTS5Body,
            candidates: vec![entry(Some(42), "src/lib.rs", LaneKind::FTS5Body, 0.7, 1)],
        };
        let fused = fuse_candidate_sets(&[set1, set2]);
        assert_eq!(fused.len(), 1, "should merge into one candidate");
        assert_eq!(
            fused[0].provenance.lanes.len(),
            2,
            "should have 2 lane contributions"
        );
        assert_eq!(fused[0].chunk_id, Some(42));
    }

    // AC-4: is_vendor=true propagated from CandidateEntry into FusedCandidate
    #[test]
    fn is_vendor_propagated() {
        let mut e = entry(Some(1), "vendor/lib.rs", LaneKind::Trigram, 0.9, 0);
        e.is_vendor = true;
        let set = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![e],
        };
        let fused = fuse_candidate_sets(&[set]);
        assert!(fused[0].is_vendor, "is_vendor must propagate");
    }

    // AC-5: is_exact_hit=true propagated when any contributing CandidateEntry has it
    #[test]
    fn is_exact_hit_propagated_from_any_lane() {
        let mut e1 = entry(Some(1), "src/main.rs", LaneKind::Trigram, 0.8, 0);
        e1.is_exact_hit = false;
        let mut e2 = entry(Some(1), "src/main.rs", LaneKind::FTS5Symbol, 0.7, 1);
        e2.is_exact_hit = true;

        let set1 = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![e1],
        };
        let set2 = CandidateSet {
            source_lane: LaneKind::FTS5Symbol,
            candidates: vec![e2],
        };
        let fused = fuse_candidate_sets(&[set1, set2]);
        assert_eq!(fused.len(), 1);
        assert!(
            fused[0].is_exact_hit,
            "is_exact_hit must be OR'd from any lane"
        );
    }

    // AC-6: chunk_id takes dedup priority over path+line_range
    #[test]
    fn chunk_id_dedup_priority_over_path_line() {
        // Two entries with same chunk_id but different line_range should merge
        let mut e1 = entry(Some(99), "src/a.rs", LaneKind::Trigram, 0.9, 0);
        e1.line_range = Some((1, 10));
        let mut e2 = entry(Some(99), "src/a.rs", LaneKind::FTS5Body, 0.8, 1);
        e2.line_range = Some((20, 30)); // different line range

        let set1 = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![e1],
        };
        let set2 = CandidateSet {
            source_lane: LaneKind::FTS5Body,
            candidates: vec![e2],
        };
        let fused = fuse_candidate_sets(&[set1, set2]);
        assert_eq!(
            fused.len(),
            1,
            "same chunk_id must merge regardless of line_range"
        );
        assert_eq!(fused[0].provenance.lanes.len(), 2);
    }

    // is_generated propagated
    #[test]
    fn is_generated_propagated() {
        let mut e = entry(Some(1), "generated/code.rs", LaneKind::Trigram, 0.9, 0);
        e.is_generated = true;
        let set = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![e],
        };
        let fused = fuse_candidate_sets(&[set]);
        assert!(fused[0].is_generated, "is_generated must propagate");
    }

    // Highest score wins on merge
    #[test]
    fn highest_score_wins_on_merge() {
        let set1 = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![entry(Some(1), "src/lib.rs", LaneKind::Trigram, 0.5, 0)],
        };
        let set2 = CandidateSet {
            source_lane: LaneKind::FTS5Body,
            candidates: vec![entry(Some(1), "src/lib.rs", LaneKind::FTS5Body, 0.9, 1)],
        };
        let fused = fuse_candidate_sets(&[set1, set2]);
        assert_eq!(fused.len(), 1);
        assert!(
            (fused[0].rrf_score - 0.9).abs() < f32::EPSILON,
            "highest score should win"
        );
    }

    // Serde round-trip
    #[test]
    fn candidate_entry_serde_roundtrip() {
        let e = entry(Some(1), "src/main.rs", LaneKind::Trigram, 0.9, 0);
        let json = serde_json::to_string(&e).unwrap();
        let deserialized: CandidateEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.chunk_id, e.chunk_id);
        assert_eq!(deserialized.source_lane, e.source_lane);
    }

    #[test]
    fn fused_candidate_serde_roundtrip() {
        let set = CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![entry(Some(1), "src/main.rs", LaneKind::Trigram, 0.9, 0)],
        };
        let fused = fuse_candidate_sets(&[set]);
        let json = serde_json::to_string(&fused[0]).unwrap();
        let deserialized: FusedCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.chunk_id, fused[0].chunk_id);
        assert_eq!(deserialized.provenance.lanes.len(), 1);
    }
}
