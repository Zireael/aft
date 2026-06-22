//! RRF fusion engine with ExactHitFloor for Retrieval Intelligence v1.
//!
//! Fuses CandidateSets from multiple lanes using Reciprocal Rank Fusion (RRF),
//! deduplicates by canonical identity, and applies ExactHitFloor promotion
//! for exact symbol/path hits from non-vendor, non-generated files.

use crate::candidate::{fuse_candidate_sets, CandidateSet, FusedCandidate};
use crate::search_plan::{LaneKind, SearchPlan};

/// RRF fusion engine.
pub struct RRFFusionEngine;

impl RRFFusionEngine {
    /// Fuse multiple CandidateSets into an ordered Vec<FusedCandidate>.
    ///
    /// Steps:
    /// 1. Dedup by canonical identity (using existing fuse_candidate_sets).
    /// 2. Compute weighted RRF score per lane contribution.
    /// 3. Apply ExactHitFloor: promote non-vendor/non-generated exact hits to top.
    pub fn fuse(plan: &SearchPlan, candidate_sets: Vec<CandidateSet>) -> Vec<FusedCandidate> {
        if candidate_sets.is_empty() {
            return Vec::new();
        }

        // Step 1: Fuse and dedup
        let mut fused = fuse_candidate_sets(&candidate_sets);

        if fused.is_empty() {
            return Vec::new();
        }

        // Step 2: Compute weighted RRF score
        let rrf_k = plan.fusion.rrf_k as f32;
        Self::compute_weighted_rrf(&mut fused, plan, rrf_k);

        // Step 3: Apply ExactHitFloor
        let floor_n = plan.fusion.exact_hit_floor_n;
        Self::apply_exact_hit_floor(&mut fused, floor_n);

        // Sort: Group A candidates first (already sorted), then Group B by rrf_score
        // The ExactHitFloor already partitions into Group A ++ Group B
        fused
    }

    /// Compute weighted RRF score for each fused candidate.
    ///
    /// weighted_rrf = sum over lane contributions: weight_lane * 1.0 / (k + rank_in_lane)
    fn compute_weighted_rrf(fused: &mut [FusedCandidate], plan: &SearchPlan, rrf_k: f32) {
        for candidate in fused.iter_mut() {
            let mut score: f32 = 0.0;

            for contribution in &candidate.provenance.lanes {
                // Look up the lane weight from the plan
                let lane_weight = plan
                    .lane_weights
                    .get(&contribution.lane)
                    .copied()
                    .unwrap_or(0.0);

                // RRF formula: weight * 1 / (k + rank)
                let rrf = lane_weight * 1.0 / (rrf_k + contribution.rank_in_lane as f32);
                score += rrf;
            }

            candidate.rrf_score = score;
            candidate.final_score = score;
        }
    }

    /// Apply ExactHitFloor: promote non-vendor/non-generated exact hits to top positions.
    ///
    /// Group A = first exact_hit_floor_n exact candidates (is_exact_hit && !is_vendor && !is_generated),
    ///           ordered by exact lane rank ascending.
    /// Group B = all remaining candidates, sorted by rrf_score descending.
    /// Final pool = Group A ++ Group B.
    fn apply_exact_hit_floor(fused: &mut Vec<FusedCandidate>, floor_n: usize) {
        // Partition into exact candidates (Group A candidates) and the rest (Group B)
        let mut group_a: Vec<FusedCandidate> = Vec::new();
        let mut group_b: Vec<FusedCandidate> = Vec::new();

        for candidate in fused.drain(..) {
            // Exact hit from non-vendor, non-generated file
            if candidate.is_exact_hit && !candidate.is_vendor && !candidate.is_generated {
                group_a.push(candidate);
            } else {
                group_b.push(candidate);
            }
        }

        // Sort Group A by exact lane rank ascending (lower rank = better)
        group_a.sort_by_key(|c| {
            c.provenance
                .lanes
                .iter()
                .filter(|l| {
                    matches!(
                        l.lane,
                        LaneKind::FTS5Symbol | LaneKind::SymbolExact | LaneKind::Trigram
                    )
                })
                .map(|l| l.rank_in_lane)
                .min()
                .unwrap_or(usize::MAX)
        });

        // Cap Group A to floor_n; overflow exact hits rejoin Group B
        let promoted: Vec<FusedCandidate> = group_a.drain(..floor_n.min(group_a.len())).collect();
        group_b.extend(group_a); // overflow exact hits go to Group B

        // Sort Group B by rrf_score descending
        group_b.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Mark Group A candidates as floor-applied
        let mut result: Vec<FusedCandidate> = promoted
            .into_iter()
            .map(|mut c| {
                c.exact_hit_floor_applied = true;
                c
            })
            .collect();

        result.extend(group_b);
        *fused = result;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::CandidateEntry;
    use crate::query_shape::{QueryKind, QueryShape, ShapeWeights};
    use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};
    use std::path::PathBuf;

    fn simple_plan() -> SearchPlan {
        let shape = QueryShape {
            kind: QueryKind::Identifier,
            weights: ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: true,
            search_index_ready: true,
        };
        let mut plan = SearchPlanBuilder::from_query_shape(&shape, &ctx);
        plan.fusion.rrf_k = 60;
        plan.fusion.exact_hit_floor_n = 5;
        plan
    }

    fn make_entry(
        chunk_id: Option<u64>,
        file: &str,
        lane: LaneKind,
        score: f32,
        rank: usize,
        exact: bool,
        vendor: bool,
        generated: bool,
    ) -> CandidateEntry {
        CandidateEntry {
            chunk_id,
            symbol_id: None,
            file_path: PathBuf::from(file),
            line_range: Some((1, 10)),
            content_hash: None,
            score,
            rank,
            is_exact_hit: exact,
            is_vendor: vendor,
            is_generated: generated,
            source_lane: lane,
        }
    }

    // AC-1: Single CandidateSet passthrough
    #[test]
    fn single_lane_passthrough() {
        let plan = simple_plan();
        let sets = vec![CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![
                make_entry(
                    Some(1),
                    "a.rs",
                    LaneKind::Trigram,
                    0.9,
                    0,
                    false,
                    false,
                    false,
                ),
                make_entry(
                    Some(2),
                    "b.rs",
                    LaneKind::Trigram,
                    0.8,
                    1,
                    false,
                    false,
                    false,
                ),
                make_entry(
                    Some(3),
                    "c.rs",
                    LaneKind::Trigram,
                    0.7,
                    2,
                    false,
                    false,
                    false,
                ),
            ],
        }];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].provenance.lanes.len(), 1);
    }

    // AC-2: Two-lane merge with canonical identity overlap
    #[test]
    fn two_lane_merge() {
        let plan = simple_plan();
        let sets = vec![
            CandidateSet {
                source_lane: LaneKind::Trigram,
                candidates: vec![make_entry(
                    Some(42),
                    "lib.rs",
                    LaneKind::Trigram,
                    0.8,
                    0,
                    false,
                    false,
                    false,
                )],
            },
            CandidateSet {
                source_lane: LaneKind::FTS5Body,
                candidates: vec![make_entry(
                    Some(42),
                    "lib.rs",
                    LaneKind::FTS5Body,
                    0.7,
                    1,
                    false,
                    false,
                    false,
                )],
            },
        ];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].provenance.lanes.len(), 2);
    }

    // AC-3: Higher-weight lane contributes more
    #[test]
    fn weight_sensitivity() {
        let mut plan = simple_plan();
        // Set explicit weights so FTS5Body >> Trigram
        plan.lane_weights.insert(LaneKind::FTS5Body, 2.0);
        plan.lane_weights.insert(LaneKind::Trigram, 0.5);
        // Entry at rank 0 in Trigram (weight ~0.4) vs rank 0 in FTS5Body (weight ~1.0)
        let sets_high_w = vec![CandidateSet {
            source_lane: LaneKind::FTS5Body,
            candidates: vec![make_entry(
                Some(1),
                "a.rs",
                LaneKind::FTS5Body,
                0.9,
                0,
                false,
                false,
                false,
            )],
        }];
        let sets_low_w = vec![CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: vec![make_entry(
                Some(1),
                "a.rs",
                LaneKind::Trigram,
                0.9,
                0,
                false,
                false,
                false,
            )],
        }];

        let fused_high = RRFFusionEngine::fuse(&plan, sets_high_w);
        let fused_low = RRFFusionEngine::fuse(&plan, sets_low_w);

        assert!(!fused_high.is_empty() && !fused_low.is_empty());
        assert!(
            fused_high[0].rrf_score > fused_low[0].rrf_score,
            "FTS5Body (higher weight) should have higher RRF score than Trigram (lower weight)"
        );
    }

    // AC-4: Empty input
    #[test]
    fn empty_input() {
        let plan = simple_plan();
        let fused = RRFFusionEngine::fuse(&plan, vec![]);
        assert!(fused.is_empty());
    }

    // AC-5: Exact hit from non-vendor → promoted to top
    #[test]
    fn exact_hit_promoted_to_top() {
        let plan = simple_plan();
        let sets = vec![
            CandidateSet {
                source_lane: LaneKind::Semantic,
                candidates: vec![make_entry(
                    Some(1),
                    "semantic.rs",
                    LaneKind::Semantic,
                    0.95,
                    0,
                    false,
                    false,
                    false,
                )],
            },
            CandidateSet {
                source_lane: LaneKind::FTS5Symbol,
                candidates: vec![make_entry(
                    Some(2),
                    "exact.rs",
                    LaneKind::FTS5Symbol,
                    0.3,
                    0,
                    true,
                    false,
                    false,
                )],
            },
        ];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        // Exact hit should be at position 0 despite lower semantic score
        assert!(
            fused[0].is_exact_hit,
            "first candidate should be the exact hit"
        );
        assert!(
            fused[0].exact_hit_floor_applied,
            "Group A candidate should have floor applied"
        );
    }

    // AC-6: Exact hit from vendor → stays in Group B, not promoted above semantic result
    #[test]
    fn vendor_exact_hit_not_promoted() {
        let mut plan = simple_plan();
        // Ensure semantic has a high weight so it outranks the vendor exact hit
        plan.lane_weights.insert(LaneKind::Semantic, 1.5);
        plan.lane_weights.insert(LaneKind::FTS5Symbol, 0.3);

        let sets = vec![
            CandidateSet {
                source_lane: LaneKind::Semantic,
                candidates: vec![make_entry(
                    Some(1),
                    "semantic.rs",
                    LaneKind::Semantic,
                    0.95,
                    0,
                    false,
                    false,
                    false,
                )],
            },
            CandidateSet {
                source_lane: LaneKind::FTS5Symbol,
                candidates: vec![make_entry(
                    Some(2),
                    "vendor/lib.rs",
                    LaneKind::FTS5Symbol,
                    0.3,
                    0,
                    true,
                    true,
                    false,
                )],
            },
        ];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        // Semantic hit should outrank vendor exact hit (semantic is non-vendor, vendor is in Group B)
        assert_eq!(fused.len(), 2);
        // The semantic result should come first since it has higher rrf_score
        assert_eq!(
            fused[0].file_path.file_name().unwrap().to_str().unwrap(),
            "semantic.rs",
            "semantic result should rank above vendor exact hit"
        );
    }

    // AC-7: exact_hit_floor_applied on Group A
    #[test]
    fn floor_applied_flag() {
        let plan = simple_plan();
        let sets = vec![CandidateSet {
            source_lane: LaneKind::FTS5Symbol,
            candidates: vec![make_entry(
                Some(1),
                "main.rs",
                LaneKind::FTS5Symbol,
                0.5,
                0,
                true,
                false,
                false,
            )],
        }];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        assert!(fused[0].exact_hit_floor_applied);
    }

    // AC-8: floor cap overflow
    #[test]
    fn floor_cap_overflow() {
        let mut plan = simple_plan();
        plan.fusion.exact_hit_floor_n = 2; // cap at 2

        let sets = vec![CandidateSet {
            source_lane: LaneKind::FTS5Symbol,
            candidates: vec![
                make_entry(
                    Some(1),
                    "a.rs",
                    LaneKind::FTS5Symbol,
                    0.5,
                    0,
                    true,
                    false,
                    false,
                ),
                make_entry(
                    Some(2),
                    "b.rs",
                    LaneKind::FTS5Symbol,
                    0.5,
                    1,
                    true,
                    false,
                    false,
                ),
                make_entry(
                    Some(3),
                    "c.rs",
                    LaneKind::FTS5Symbol,
                    0.5,
                    2,
                    true,
                    false,
                    false,
                ),
                make_entry(
                    Some(4),
                    "d.rs",
                    LaneKind::FTS5Symbol,
                    0.5,
                    3,
                    true,
                    false,
                    false,
                ),
            ],
        }];
        let fused = RRFFusionEngine::fuse(&plan, sets);
        // Only 2 should be in Group A (floor_applied=true)
        let group_a_count = fused.iter().filter(|c| c.exact_hit_floor_applied).count();
        assert_eq!(group_a_count, 2);
        // Overflow exact hits should be in Group B (floor_applied=false)
        assert_eq!(fused.len(), 4);
    }
}
