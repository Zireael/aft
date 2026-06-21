//! Retrieval lane adapters for Retrieval Intelligence v1.
//!
//! Each adapter wraps an existing search infrastructure (FTS5, trigram, semantic)
//! and produces `CandidateSet` results per lane, feeding into the fusion pipeline.

#[cfg(feature = "semantic-fts5")]
pub mod fts5_adapter;

pub mod fusion;
pub mod graph_enrichment;
pub mod ranking_features;
pub mod semantic_adapter;
pub mod trigram_adapter;

#[cfg(feature = "semantic-fts5")]
pub use fts5_adapter::Fts5Adapter;
pub use fusion::RRFFusionEngine;
pub use graph_enrichment::enrich_with_graph_context;
pub use ranking_features::{apply_ranking_features, RankingFeaturesConfig};
pub use semantic_adapter::SemanticAdapter;
pub use trigram_adapter::TrigramAdapter;

use crate::candidate::CandidateSet;
use crate::search_plan::SearchPlan;

/// Trait for retrieval lane adapters.
///
/// Each adapter knows how to query one retrieval engine and return
/// `CandidateSet` results that can be fused across lanes.
pub trait RetrievalAdapter {
    /// Given a query and search plan, produce CandidateSets per active lane.
    fn retrieve(&self, query: &str, plan: &SearchPlan) -> Vec<CandidateSet>;
}

/// Check if a file path looks like a vendor or generated file.
pub fn is_vendor_path(path: &str) -> bool {
    let lower = path.replace('\\', "/");
    // Match "/vendor/" anywhere in the path (handles "src/vendor/bar.rs")
    let has_vendor = lower.contains("/vendor/")
        // Also match paths starting with "vendor/"
        || lower.starts_with("vendor/");
    has_vendor || lower.contains("/node_modules/") || lower.starts_with("node_modules/")
}

/// Check if a file path looks like a generated file.
pub fn is_generated_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("generated")
        || lower.contains(".generated.")
        || lower.ends_with(".gen.rs")
        || lower.ends_with(".gen.ts")
        || lower.ends_with("_gen.rs")
}
