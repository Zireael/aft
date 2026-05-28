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
