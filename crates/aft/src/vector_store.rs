//! Vector storage abstraction for semantic search.
//!
//! Provides a [`VectorStore`] trait that decouples vector storage and search
//! from the semantic index lifecycle. Two built-in implementations:
//!
//! * [`FlatF32VectorStore`] — flat in-memory scan over f32 vectors with cosine
//!   similarity. Preserves the existing behaviour exactly.
//! * [`FlatBinaryHammingVectorStore`] — flat in-memory Hamming search over
//!   packed binary (bit) vectors.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::semantic_index::{
    cosine_similarity, EmbeddingEntry, IndexedFileMetadata, SemanticChunk, SemanticResult,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Aggregate statistics about a vector store.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct VectorStoreStats {
    /// Number of files currently indexed.
    pub files_indexed: usize,
    /// Total chunk entries.
    pub total_entries: usize,
    /// Number of orphan entries (file no longer in manifest).
    pub orphan_count: usize,
    /// Total deleted entries since store creation (monotonic).
    pub deleted_count: usize,
    /// Kind of vectors stored.
    pub vector_kind: &'static str,
    /// Embedding dimension.
    pub dimension: usize,
    /// Distance metric in use.
    pub metric: &'static str,
}

/// A single scored chunk returned by vector search.
#[derive(Debug, Clone)]
pub(crate) struct ScoredChunk {
    /// The chunk metadata.
    pub chunk: SemanticChunk,
    /// Similarity score (higher = more relevant).
    pub score: f32,
}

/// Summary of an orphan-pruning pass.
#[derive(Debug, Clone, Default)]
pub(crate) struct PruneStats {
    /// Number of stale (zero-norm) entries removed.
    pub stale_removed: usize,
    /// Number of file-orphaned entries removed.
    pub orphan_removed: usize,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over a vector storage and search backend.
///
/// All built-in implementations store vectors in memory and perform flat
/// (exhaustive) search. Future backends (SQLite, LanceDB, etc.) implement
/// the same trait so the [`crate::semantic_index::SemanticIndex`] lifecycle
/// is decoupled from storage details.
pub(crate) trait VectorStore: std::fmt::Debug + Send + Sync {
    /// Return the embedding dimension stored.
    fn dimension(&self) -> usize;

    /// Total number of chunk entries.
    fn len(&self) -> usize;

    /// True when there are zero entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return a read-only reference to the inner entries (for serialization,
    /// test assertions, and legacy direct-access codepaths).
    fn entries_slice(&self) -> &[EmbeddingEntry];

    /// Mutable access to entries (test-only).
    #[cfg(test)]
    fn entries_mut(&mut self) -> &mut Vec<EmbeddingEntry>;

    /// Mutable access to file metadata (test-only).
    #[cfg(test)]
    fn file_metadata_mut(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata>;

    /// Search for the top-K most similar entries to `query_vector`.
    ///
    /// Returns results sorted descending by similarity score.
    fn search(&self, query_vector: &[f32], top_k: usize) -> Vec<SemanticResult>;

    /// Replace all entries for a given file.
    ///
    /// Any existing entries whose chunk path matches `file_path` are removed
    /// first, then `chunks` are inserted. This prevents stale entries when a
    /// file is re-indexed.
    fn upsert_file(&mut self, file_path: &Path, chunks: Vec<EmbeddingEntry>);

    /// Remove all entries whose chunk path matches `path`.
    fn delete_path(&mut self, path: &Path);

    /// Remove entries whose chunk path is absent from `current_files`.
    ///
    /// Returns the number of entries removed.
    fn prune_orphans(&mut self, current_files: &[PathBuf]) -> usize;

    /// Reject any entries whose vector is a zero-norm — these can't produce
    /// meaningful similarity scores.
    fn prune_stale_vectors(&mut self) -> usize;

    /// Return aggregate statistics.
    fn stats(&self) -> VectorStoreStats;
}

// ---------------------------------------------------------------------------
// FlatF32VectorStore
// ---------------------------------------------------------------------------

/// In-memory flat store for f32 vectors using cosine similarity.
///
/// This is the default store, preserving existing semantic-search behaviour.
#[derive(Debug, Clone)]
pub(crate) struct FlatF32VectorStore {
    entries: Vec<EmbeddingEntry>,
    dimension: usize,
    /// Track indexed files and their metadata for staleness detection.
    file_metadata: HashMap<PathBuf, IndexedFileMetadata>,
    /// Monotonic counter of deleted entries.
    deleted_count: usize,
}

impl FlatF32VectorStore {
    /// Direct access to the entries vector for internal mutation.
    /// SemanticIndex::build_from_chunks and refresh_stale_files need this.
    pub(crate) fn entries_mut(&mut self) -> &mut Vec<EmbeddingEntry> {
        &mut self.entries
    }

    /// Read-only slice of all entries for serialization and introspection.
    pub(crate) fn entries_slice(&self) -> &[EmbeddingEntry] {
        &self.entries
    }

    pub(crate) fn new(dimension: usize) -> Self {
        Self {
            entries: Vec::new(),
            dimension,
            file_metadata: HashMap::new(),
            deleted_count: 0,
        }
    }

    /// Construct from pre-built parts (used during deserialization).
    pub(crate) fn from_parts(
        entries: Vec<EmbeddingEntry>,
        dimension: usize,
        file_metadata: HashMap<PathBuf, IndexedFileMetadata>,
    ) -> Self {
        Self {
            entries,
            dimension,
            file_metadata,
            deleted_count: 0,
        }
    }

    /// Consume and return the inner parts.
    pub(crate) fn into_parts(self) -> (Vec<EmbeddingEntry>, HashMap<PathBuf, IndexedFileMetadata>) {
        (self.entries, self.file_metadata)
    }

    /// Borrow the file metadata.
    pub(crate) fn file_metadata(&self) -> &HashMap<PathBuf, IndexedFileMetadata> {
        &self.file_metadata
    }

    /// Mutable borrow of file metadata.
    pub(crate) fn file_metadata_mut(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        &mut self.file_metadata
    }

    /// Set the store dimension (keeps in sync with snapshot dimension).
    pub(crate) fn set_dimension(&mut self, dim: usize) {
        self.dimension = dim;
    }
}

impl VectorStore for FlatF32VectorStore {
    fn dimension(&self) -> usize {
        self.dimension
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn entries_slice(&self) -> &[EmbeddingEntry] {
        &self.entries
    }

    #[cfg(test)]
    fn entries_mut(&mut self) -> &mut Vec<EmbeddingEntry> {
        &mut self.entries
    }

    #[cfg(test)]
    fn file_metadata_mut(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        &mut self.file_metadata
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> Vec<SemanticResult> {
        if self.entries.is_empty() || query_vector.len() != self.dimension {
            return Vec::new();
        }

        let mut scored: Vec<(f32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let mut score = cosine_similarity(query_vector, &entry.vector);
                if entry.chunk.exported {
                    score *= 1.1;
                }
                (score, i)
            })
            .collect();

        // Sort descending by score
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .take(top_k)
            .map(|(score, idx)| {
                let entry = &self.entries[idx];
                SemanticResult {
                    file: entry.chunk.file.clone(),
                    name: entry.chunk.name.clone(),
                    kind: entry.chunk.kind.clone(),
                    start_line: entry.chunk.start_line,
                    end_line: entry.chunk.end_line,
                    exported: entry.chunk.exported,
                    snippet: entry.chunk.snippet.clone(),
                    score,
                    source: "semantic",
                }
            })
            .collect()
    }

    fn upsert_file(&mut self, file_path: &Path, chunks: Vec<EmbeddingEntry>) {
        self.delete_path(file_path);
        self.entries.extend(chunks);
    }

    fn delete_path(&mut self, path: &Path) {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.chunk.file != path);
        self.deleted_count += before - self.entries.len();
        self.file_metadata.remove(path);
    }

    fn prune_orphans(&mut self, current_files: &[PathBuf]) -> usize {
        let current_set: std::collections::HashSet<&Path> =
            current_files.iter().map(PathBuf::as_path).collect();
        let before = self.entries.len();
        self.entries
            .retain(|entry| current_set.contains(entry.chunk.file.as_path()));
        let removed = before - self.entries.len();
        if removed > 0 {
            self.deleted_count += removed;
        }

        // Also remove orphaned metadata entries
        self.file_metadata
            .retain(|path, _| current_set.contains(path.as_path()));

        removed
    }

    fn prune_stale_vectors(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let norm: f32 = entry.vector.iter().map(|v| v * v).sum();
            norm > 0.0
        });
        let pruned = before - self.entries.len();
        if pruned > 0 {
            self.deleted_count += pruned;
        }
        pruned
    }

    fn stats(&self) -> VectorStoreStats {
        VectorStoreStats {
            files_indexed: self.file_metadata.len(),
            total_entries: self.entries.len(),
            orphan_count: 0,
            deleted_count: self.deleted_count,
            vector_kind: "dense_f32",
            dimension: self.dimension,
            metric: "cosine",
        }
    }
}

// ---------------------------------------------------------------------------
// FlatBinaryHammingVectorStore
// ---------------------------------------------------------------------------

/// Bit count (population count) for Hamming distance on packed u64 words.
fn popcount64(x: u64) -> u32 {
    x.count_ones()
}

/// Compute Hamming distance between two packed-bit vectors stored as `&[u64]`.
fn hamming_distance(a: &[u64], b: &[u64]) -> u32 {
    a.iter().zip(b.iter()).map(|(x, y)| popcount64(x ^ y)).sum()
}

/// In-memory flat store for packed binary (bit) vectors using Hamming distance.
///
/// Each binary vector is stored as `Vec<u64>` where every bit represents one
/// dimension. The number of u64 words needed is `ceil(dim / 64)`.
#[derive(Debug, Clone)]
pub(crate) struct FlatBinaryHammingVectorStore {
    entries: Vec<EmbeddingEntry>,
    /// Raw binary vectors, one `Vec<u64>` per entry (same index as `entries`).
    packed: Vec<Vec<u64>>,
    dimension: usize,
    words_per_vector: usize,
    file_metadata: HashMap<PathBuf, IndexedFileMetadata>,
    deleted_count: usize,
}

impl FlatBinaryHammingVectorStore {
    pub(crate) fn new(dimension: usize) -> Self {
        let words = dimension.div_ceil(64);
        Self {
            entries: Vec::new(),
            packed: Vec::new(),
            dimension,
            words_per_vector: words,
            file_metadata: HashMap::new(),
            deleted_count: 0,
        }
    }

    /// Convert a binary f32 vector (each element 0.0 or 1.0) to packed u64.
    fn pack_float32(vec: &[f32], words: usize) -> Vec<u64> {
        let mut packed = vec![0u64; words];
        for (i, &v) in vec.iter().enumerate() {
            if v > 0.5 {
                packed[i / 64] |= 1u64 << (i % 64);
            }
        }
        packed
    }

    /// Convert a binary u8 vector (each element 0 or 1) to packed u64.
    fn pack_u8(vec: &[u8], words: usize) -> Vec<u64> {
        let mut packed = vec![0u64; words];
        for (i, &v) in vec.iter().enumerate() {
            if v > 0 {
                packed[i / 64] |= 1u64 << (i % 64);
            }
        }
        packed
    }

    /// Pack the vector stored in an `EmbeddingEntry`, returning both the
    /// entry and its packed representation.
    fn pack_entry(entry: EmbeddingEntry, words: usize) -> (EmbeddingEntry, Vec<u64>) {
        let packed = Self::pack_float32(&entry.vector, words);
        (entry, packed)
    }
}

impl VectorStore for FlatBinaryHammingVectorStore {
    fn dimension(&self) -> usize {
        self.dimension
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn entries_slice(&self) -> &[EmbeddingEntry] {
        &self.entries
    }

    #[cfg(test)]
    fn entries_mut(&mut self) -> &mut Vec<EmbeddingEntry> {
        &mut self.entries
    }

    #[cfg(test)]
    fn file_metadata_mut(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        &mut self.file_metadata
    }

    fn search(&self, query_vector: &[f32], top_k: usize) -> Vec<SemanticResult> {
        if self.entries.is_empty() || query_vector.len() != self.dimension {
            return Vec::new();
        }

        let query_packed = Self::pack_float32(query_vector, self.words_per_vector);
        let mut scored: Vec<(f32, usize)> = self
            .packed
            .iter()
            .enumerate()
            .map(|(i, packed)| {
                // Hamming distance — lower = more similar. Convert to a
                // similarity score in [0, 1] where 1 = identical.
                let dist = hamming_distance(&query_packed, packed);
                let max_dist = (self.dimension as u32).min(dist);
                let score = if max_dist == 0 {
                    1.0
                } else {
                    1.0 - (dist as f32 / self.dimension as f32)
                };
                (score, i)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .take(top_k)
            .map(|(score, idx)| {
                let entry = &self.entries[idx];
                SemanticResult {
                    file: entry.chunk.file.clone(),
                    name: entry.chunk.name.clone(),
                    kind: entry.chunk.kind.clone(),
                    start_line: entry.chunk.start_line,
                    end_line: entry.chunk.end_line,
                    exported: entry.chunk.exported,
                    snippet: entry.chunk.snippet.clone(),
                    score,
                    source: "semantic",
                }
            })
            .collect()
    }

    fn upsert_file(&mut self, file_path: &Path, chunks: Vec<EmbeddingEntry>) {
        self.delete_path(file_path);
        let words = self.words_per_vector;
        for entry in chunks {
            let packed = Self::pack_float32(&entry.vector, words);
            self.entries.push(entry);
            self.packed.push(packed);
        }
    }

    fn delete_path(&mut self, path: &Path) {
        let before = self.entries.len();
        let mut retained_entries = Vec::with_capacity(self.entries.len());
        let mut retained_packed = Vec::with_capacity(self.packed.len());
        for (entry, packed) in self.entries.drain(..).zip(self.packed.drain(..)) {
            if entry.chunk.file != path {
                retained_entries.push(entry);
                retained_packed.push(packed);
            }
        }
        let removed = before - retained_entries.len();
        self.entries = retained_entries;
        self.packed = retained_packed;
        self.deleted_count += removed;
        self.file_metadata.remove(path);
    }

    fn prune_orphans(&mut self, current_files: &[PathBuf]) -> usize {
        let current_set: std::collections::HashSet<&Path> =
            current_files.iter().map(PathBuf::as_path).collect();
        let before = self.entries.len();
        let mut retained_entries = Vec::with_capacity(self.entries.len());
        let mut retained_packed = Vec::with_capacity(self.packed.len());
        for (entry, packed) in self.entries.drain(..).zip(self.packed.drain(..)) {
            if current_set.contains(entry.chunk.file.as_path()) {
                retained_entries.push(entry);
                retained_packed.push(packed);
            }
        }
        let removed = before - retained_entries.len();
        self.entries = retained_entries;
        self.packed = retained_packed;
        if removed > 0 {
            self.deleted_count += removed;
        }
        self.file_metadata
            .retain(|path, _| current_set.contains(path.as_path()));
        removed
    }

    fn prune_stale_vectors(&mut self) -> usize {
        let before = self.entries.len();
        let mut retained_entries = Vec::with_capacity(self.entries.len());
        let mut retained_packed = Vec::with_capacity(self.packed.len());
        for (entry, packed) in self.entries.drain(..).zip(self.packed.drain(..)) {
            let norm: f32 = entry.vector.iter().map(|v| v * v).sum();
            if norm > 0.0 {
                retained_entries.push(entry);
                retained_packed.push(packed);
            }
        }
        let pruned = before - retained_entries.len();
        self.entries = retained_entries;
        self.packed = retained_packed;
        if pruned > 0 {
            self.deleted_count += pruned;
        }
        pruned
    }

    fn stats(&self) -> VectorStoreStats {
        VectorStoreStats {
            files_indexed: self.file_metadata.len(),
            total_entries: self.entries.len(),
            orphan_count: 0,
            deleted_count: self.deleted_count,
            vector_kind: "binary_packed",
            dimension: self.dimension,
            metric: "hamming",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_entry(file: &str, name: &str, vector: Vec<f32>) -> EmbeddingEntry {
        let chunk = SemanticChunk {
            file: PathBuf::from(file),
            name: name.to_string(),
            kind: crate::symbols::SymbolKind::Function,
            start_line: 0,
            end_line: 10,
            exported: false,
            embed_text: String::new(),
            snippet: String::new(),
        };
        let chunk_hash = crate::semantic_index::compute_chunk_hash(&chunk);
        EmbeddingEntry {
            chunk,
            vector,
            chunk_hash,
        }
    }

    // ── FlatF32VectorStore tests ────────────────────────────────────────

    #[test]
    fn f32_store_search_returns_top_k_sorted() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![
                make_entry("a.rs", "func_a", vec![1.0, 0.0, 0.0]),
                make_entry("a.rs", "func_b", vec![0.0, 1.0, 0.0]),
            ],
        );
        store.upsert_file(
            Path::new("b.rs"),
            vec![make_entry("b.rs", "func_c", vec![0.0, 0.0, 1.0])],
        );

        // Query closest to [1,0,0]
        let results = store.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "func_a");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn f32_store_search_empty_returns_empty() {
        let store = FlatF32VectorStore::new(3);
        let results = store.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn f32_store_search_dimension_mismatch_returns_empty() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f", vec![1.0, 0.0, 0.0])],
        );
        let results = store.search(&[1.0, 0.0], 5); // 2 dims vs 3
        assert!(results.is_empty());
    }

    #[test]
    fn f32_store_len_and_is_empty() {
        let mut store = FlatF32VectorStore::new(3);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());

        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f", vec![1.0, 0.0, 0.0])],
        );
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn f32_store_entries_slice_read_only() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f", vec![1.0, 0.0, 0.0])],
        );
        let slice = store.entries_slice();
        assert_eq!(slice.len(), 1);
        assert_eq!(slice[0].chunk.name, "f");
    }

    #[test]
    fn f32_store_delete_path_removes_entries() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f1", vec![1.0, 0.0, 0.0])],
        );
        store.upsert_file(
            Path::new("b.rs"),
            vec![make_entry("b.rs", "f2", vec![0.0, 1.0, 0.0])],
        );
        store.delete_path(Path::new("a.rs"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.entries_slice()[0].chunk.name, "f2");
    }

    #[test]
    fn f32_store_prune_orphans_removes_stale() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f1", vec![1.0, 0.0, 0.0])],
        );
        store.upsert_file(
            Path::new("b.rs"),
            vec![make_entry("b.rs", "f2", vec![0.0, 1.0, 0.0])],
        );
        let removed = store.prune_orphans(&[PathBuf::from("b.rs")]);
        assert_eq!(removed, 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn f32_store_prune_stale_vectors_removes_zero_norm() {
        let mut store = FlatF32VectorStore::new(3);
        store.upsert_file(
            Path::new("a.rs"),
            vec![
                make_entry("a.rs", "f1", vec![1.0, 0.0, 0.0]),
                make_entry("a.rs", "f2", vec![0.0, 0.0, 0.0]), // zero norm
            ],
        );
        let pruned = store.prune_stale_vectors();
        assert_eq!(pruned, 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn f32_store_stats() {
        let mut store = FlatF32VectorStore::new(384);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f", vec![1.0, 0.0, 0.0])],
        );
        let stats = store.stats();
        assert_eq!(stats.dimension, 384);
        assert_eq!(stats.total_entries, 1);
        assert_eq!(stats.vector_kind, "dense_f32");
        assert_eq!(stats.metric, "cosine");
    }

    #[test]
    fn f32_store_exported_entry_boosted() {
        let mut store = FlatF32VectorStore::new(3);
        let mut entry = make_entry("a.rs", "exported_fn", vec![1.0, 0.0, 0.0]);
        entry.chunk.exported = true;
        let mut entry2 = make_entry("a.rs", "private_fn", vec![0.99, 0.01, 0.0]);
        entry2.chunk.exported = false;

        store.upsert_file(Path::new("a.rs"), vec![entry, entry2]);

        let results = store.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        // Exported entry should rank higher due to 1.1x boost
        assert_eq!(results[0].name, "exported_fn");
    }

    // ── FlatBinaryHammingVectorStore tests ──────────────────────────────

    #[test]
    fn hamming_store_search_identical_vector() {
        let mut store = FlatBinaryHammingVectorStore::new(8);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry(
                "a.rs",
                "f",
                vec![1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
            )],
        );
        let results = store.search(&[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert!(
            (results[0].score - 1.0).abs() < 1e-6,
            "identical should score 1.0, got {}",
            results[0].score
        );
    }

    #[test]
    fn hamming_store_search_ranking() {
        let mut store = FlatBinaryHammingVectorStore::new(8);
        // Vector A: 10101010 (4 bits set)
        // Vector B: 11110000 (4 bits set)
        // Query:    10101010 (identical to A)
        store.upsert_file(
            Path::new("a.rs"),
            vec![
                make_entry(
                    "a.rs",
                    "vec_a",
                    vec![1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
                ),
                make_entry(
                    "b.rs",
                    "vec_b",
                    vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                ),
            ],
        );
        let results = store.search(&[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "vec_a"); // identical
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn hamming_store_empty_returns_empty() {
        let store = FlatBinaryHammingVectorStore::new(8);
        let results = store.search(&[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn hamming_store_prune_stale_vectors() {
        let mut store = FlatBinaryHammingVectorStore::new(8);
        store.upsert_file(
            Path::new("a.rs"),
            vec![
                make_entry("a.rs", "f1", vec![1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]),
                make_entry("a.rs", "f2", vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            ],
        );
        let pruned = store.prune_stale_vectors();
        assert_eq!(pruned, 1);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn hamming_store_delete_path() {
        let mut store = FlatBinaryHammingVectorStore::new(8);
        store.upsert_file(
            Path::new("a.rs"),
            vec![make_entry("a.rs", "f1", vec![1.0; 8])],
        );
        store.upsert_file(
            Path::new("b.rs"),
            vec![make_entry("b.rs", "f2", vec![0.0; 8])],
        );
        store.delete_path(Path::new("a.rs"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn hamming_store_stats() {
        let store = FlatBinaryHammingVectorStore::new(128);
        let stats = store.stats();
        assert_eq!(stats.dimension, 128);
        assert_eq!(stats.vector_kind, "binary_packed");
        assert_eq!(stats.metric, "hamming");
    }

    #[test]
    fn hamming_distance_identical_is_zero() {
        let a = vec![0xAAAAAAAAAAAAAAAAu64, 0xAAAAAAAAAAAAAAAAu64];
        let b = vec![0xAAAAAAAAAAAAAAAAu64, 0xAAAAAAAAAAAAAAAAu64];
        assert_eq!(hamming_distance(&a, &b), 0);
    }

    #[test]
    fn hamming_distance_all_different() {
        let a = vec![0xAAAAAAAAAAAAAAAAu64]; // 10101010...
        let b = vec![0x5555555555555555u64]; // 01010101...
        assert_eq!(hamming_distance(&a, &b), 64);
    }

    #[test]
    fn popcount64_correct() {
        assert_eq!(popcount64(0), 0);
        assert_eq!(popcount64(1), 1);
        assert_eq!(popcount64(0xFF), 8);
        assert_eq!(popcount64(u64::MAX), 64);
    }

    // ── Binary packed-vector decode tests ───────────────────────────────

    #[test]
    fn binary_decode_exact_byte_aligned() {
        // 8 dimensions = 1 byte, byte 0xAA = 10101010
        let val = serde_json::json!("qg=="); // base64 of 0xAA
        let result = crate::semantic_index::parse_embedding_value(
            &val,
            crate::config::OutputEncoding::Base64Binary,
            "test",
            Some(8),
        )
        .unwrap();
        assert_eq!(result.len(), 8);
        assert_eq!(result[0], 0.0);
        assert_eq!(result[1], 1.0);
        assert_eq!(result[2], 0.0);
        assert_eq!(result[3], 1.0);
        assert_eq!(result[4], 0.0);
        assert_eq!(result[5], 1.0);
        assert_eq!(result[6], 0.0);
        assert_eq!(result[7], 1.0);
    }

    #[test]
    fn binary_decode_non_byte_aligned() {
        // 5 dimensions = 1 byte (padded to 8 bits), byte 0x15 = 00010101
        // bits 0..4: 1,0,1,0,1
        let val = serde_json::json!("FQ=="); // base64 of 0x15
        let result = crate::semantic_index::parse_embedding_value(
            &val,
            crate::config::OutputEncoding::Base64Binary,
            "test",
            Some(5),
        )
        .unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(result[0], 1.0);
        assert_eq!(result[1], 0.0);
        assert_eq!(result[2], 1.0);
        assert_eq!(result[3], 0.0);
        assert_eq!(result[4], 1.0);
    }

    #[test]
    fn binary_decode_padding_bits_masked() {
        // 3 dimensions = 1 byte, byte 0x07 = 00000111
        // bits 0..2: 1,1,1 (the remaining 5 bits are padding and should be 0.0)
        let val = serde_json::json!("Bw=="); // base64 of 0x07
        let result = crate::semantic_index::parse_embedding_value(
            &val,
            crate::config::OutputEncoding::Base64Binary,
            "test",
            Some(3),
        )
        .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], 1.0);
        assert_eq!(result[1], 1.0);
        assert_eq!(result[2], 1.0);
    }

    #[test]
    fn binary_decode_too_short_returns_error() {
        // 1 byte but we ask for 16 dimensions (needs 2 bytes)
        let val = serde_json::json!("AA=="); // base64 of 0x00
        let err = crate::semantic_index::parse_embedding_value(
            &val,
            crate::config::OutputEncoding::Base64Binary,
            "test",
            Some(16),
        )
        .unwrap_err();
        assert!(err.contains("too short"), "got: {err}");
    }
}
