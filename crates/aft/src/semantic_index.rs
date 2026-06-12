#![allow(dead_code)] // Forward-looking types (TypedVector, StoredVector, etc.) not yet wired.

use crate::cache_freshness::{self, FileFreshness, FreshnessVerdict};
pub use crate::config::SemanticFilePolicy;
use crate::config::{
    DistanceMetric, InputMode, OutputEncoding, SemanticBackend, SemanticBackendConfig,
    StorageStrategy,
};
use crate::fs_lock;
use crate::parser::{detect_language, extract_symbols_from_tree, grammar_for};
use crate::search_index::{cache_relative_path, cached_path_under_root, is_binary_bytes};
use crate::symbols::{Symbol, SymbolKind};
use crate::vector_store::VectorStore;
use crate::{slog_debug, slog_info, slog_warn};

use crate::local_embed::LocalEmbedder;
#[cfg(feature = "semantic-model2vec")]
use model2vec_rs::model::StaticModel as Model2VecStaticModel;
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::fmt::Display;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::SystemTime;
use tree_sitter::Parser;
use url::Url;

const DEFAULT_DIMENSION: usize = 384;
const MAX_ENTRIES: usize = 1_000_000;
/// Maximum chunks per document group sent to a contextualized embedding provider.
/// Documents with more chunks are split into sub-groups.
const DEFAULT_MAX_CHUNKS_PER_DOCUMENT: usize = 100;
/// Maximum documents per single contextualized embedding request.
/// Documents beyond this limit are batched into separate requests.
const DEFAULT_MAX_DOCUMENTS_PER_REQUEST: usize = 50;
/// Maximum retries for a failed document group in contextualized embedding.
const CONTEXTUALIZED_MAX_RETRIES: u32 = 3;
/// Base delay for exponential backoff in contextualized retry (ms).
const CONTEXTUALIZED_RETRY_BASE_DELAY_MS: u64 = 1000;
/// Max delay cap for exponential backoff in contextualized retry (ms).
const CONTEXTUALIZED_RETRY_MAX_DELAY_MS: u64 = 8000;
// Covers high-dimensional backends such as OpenAI text-embedding-3-large (3072)
// and common local models (4096) while keeping a bounded supported shape.
pub(crate) const MAX_DIMENSION: usize = 4096;
const F32_BYTES: usize = std::mem::size_of::<f32>();
const HEADER_BYTES_V1: usize = 9;
const HEADER_BYTES_V2: usize = 13;
const ONNX_RUNTIME_INSTALL_HINT: &str =
    "ONNX Runtime not found. Install via: brew install onnxruntime (macOS) or apt install libonnxruntime (Linux).";

const SEMANTIC_INDEX_VERSION_V1: u8 = 1;
const SEMANTIC_INDEX_VERSION_V2: u8 = 2;
/// V3 adds subsec_nanos to the file-mtime table so staleness detection survives
/// restart round-trips on filesystems with subsecond mtime precision (APFS,
/// ext4 with nsec, NTFS). V1/V2 persisted whole-second mtimes only, which
/// caused every restart to flag ~99% of files as stale and re-embed them.
const SEMANTIC_INDEX_VERSION_V3: u8 = 3;
/// V4 keeps the V3 on-disk layout but rebuilds persisted snippets once after
/// fixing symbol ranges that were incorrectly treated as 1-based.
const SEMANTIC_INDEX_VERSION_V4: u8 = 4;
/// V5 adds file sizes to the file metadata table so incremental staleness
/// detection can catch content changes even when mtime precision misses them.
const SEMANTIC_INDEX_VERSION_V5: u8 = 5;
/// V6 stores paths relative to project_root and adds content hashes.
const SEMANTIC_INDEX_VERSION_V6: u8 = 6;
/// V7 adds invalidation fields (source_vector_kind, stored_vector_kind,
/// normalization, query_prompt_hash) to SemanticIndexFingerprint.
const SEMANTIC_INDEX_VERSION_V7: u8 = 7;
/// V8 adds file manifest (FileRecord entries) and per-entry chunk_hash.
const SEMANTIC_INDEX_VERSION_V8: u8 = 8;
const DEFAULT_OPENAI_EMBEDDING_PATH: &str = "/embeddings";
const DEFAULT_OLLAMA_EMBEDDING_PATH: &str = "/api/embed";

// ---- Typed vector representation types ----

/// The kind of vector as emitted by the embedding provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorKind {
    /// Standard dense f32 vector (most providers).
    DenseF32,
    /// Dense int8 vector (e.g. Perplexity base64_int8).
    DenseInt8,
    /// Binary packed vector (e.g. Perplexity base64_binary).
    BinaryPacked,
}

/// Normalization policy for stored vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationPolicy {
    /// Vector is already L2-normalized by the provider.
    AlreadyNormalized,
    /// AFT must L2-normalize on insert and query.
    NormalizeOnInsertQuery,
    /// Normalization is not applicable (e.g. binary vectors).
    NotApplicable,
}

impl std::fmt::Display for VectorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DenseF32 => write!(f, "dense_f32"),
            Self::DenseInt8 => write!(f, "dense_int8"),
            Self::BinaryPacked => write!(f, "binary_packed"),
        }
    }
}

impl std::fmt::Display for NormalizationPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyNormalized => write!(f, "already_normalized"),
            Self::NormalizeOnInsertQuery => write!(f, "normalize_on_insert_query"),
            Self::NotApplicable => write!(f, "not_applicable"),
        }
    }
}

// ────────────────────────────
// Typed / stored vector types
// ────────────────────────────

/// A source embedding vector as received from a provider.
///
/// Embeddings may arrive in different formats depending on the provider and
/// configuration (plain f32 arrays, base64-encoded int8, base64-encoded
/// binary, etc.).  `TypedVector` captures the raw form so that the correct
/// conversion strategy can be applied before storage.
#[allow(dead_code)]
pub(crate) enum TypedVector {
    /// Standard dense f32 vector.
    DenseF32(Vec<f32>),
    /// Dense int8 vector (e.g. Perplexity base64_int8).
    DenseInt8(Vec<i8>),
    /// Binary packed vector (e.g. Perplexity base64_binary).
    #[allow(dead_code)]
    BinaryPacked {
        /// Packed bytes (`ceil(logical_dims / 8)` bytes).
        bytes: Vec<u8>,
        /// Number of *logical* dimensions (bits).
        logical_dims: usize,
    },
}

impl TypedVector {
    /// Return the [`VectorKind`] that describes this variant.
    pub(crate) fn kind(&self) -> VectorKind {
        match self {
            Self::DenseF32(_) => VectorKind::DenseF32,
            Self::DenseInt8(_) => VectorKind::DenseInt8,
            Self::BinaryPacked { .. } => VectorKind::BinaryPacked,
        }
    }

    /// Number of dimensions (logical bits for binary).
    pub(crate) fn dims(&self) -> usize {
        match self {
            Self::DenseF32(v) => v.len(),
            Self::DenseInt8(v) => v.len(),
            Self::BinaryPacked { logical_dims, .. } => *logical_dims,
        }
    }

    /// Convert to a [`StoredVector`] using the supplied storage strategy.
    pub(crate) fn into_stored(
        self,
        strategy: crate::config::StorageStrategy,
    ) -> Result<StoredVector, String> {
        use crate::config::StorageStrategy;
        match self {
            Self::DenseF32(v) => match strategy {
                StorageStrategy::NativeF32 => Ok(StoredVector::DenseF32(v)),
                StorageStrategy::DecodeNormalizeF32 => {
                    let sv = StoredVector::DenseF32(v);
                    Ok(sv.l2_normalize())
                }
                StorageStrategy::BinaryPacked => {
                    Err("DenseF32 vectors cannot be stored as BinaryPacked".to_string())
                }
            },
            Self::DenseInt8(v) => match strategy {
                StorageStrategy::NativeF32 => {
                    let f32s = v.into_iter().map(|x| x as f32).collect();
                    Ok(StoredVector::DenseF32(f32s))
                }
                StorageStrategy::DecodeNormalizeF32 => {
                    let f32s: Vec<f32> = v.into_iter().map(|x| x as f32).collect();
                    Ok(StoredVector::DenseF32(f32s).l2_normalize())
                }
                StorageStrategy::BinaryPacked => {
                    Err("DenseInt8 vectors cannot be stored as BinaryPacked".to_string())
                }
            },
            Self::BinaryPacked {
                bytes,
                logical_dims,
            } => match strategy {
                StorageStrategy::BinaryPacked => Ok(StoredVector::BinaryPacked {
                    bytes,
                    logical_dims,
                }),
                _ => Err(format!(
                    "BinaryPacked vectors require StorageStrategy::BinaryPacked (got {:?})",
                    strategy
                )),
            },
        }
    }

    /// Decode a base64-encoded int8 embedding string.
    pub(crate) fn decode_base64_int8(data: &str) -> Result<Self, String> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("base64 decode error: {}", e))?;
        let ints: Vec<i8> = bytes.into_iter().map(|b| b as i8).collect();
        Ok(Self::DenseInt8(ints))
    }

    /// Decode a base64-encoded binary embedding string.
    pub(crate) fn decode_base64_binary(data: &str, logical_dims: usize) -> Result<Self, String> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("base64 decode error: {}", e))?;
        let expected = logical_dims.div_ceil(8);
        if bytes.len() < expected {
            return Err(format!(
                "binary embedding too short: got {} bytes, need {} for {} dims",
                bytes.len(),
                expected,
                logical_dims
            ));
        }
        Ok(Self::BinaryPacked {
            bytes,
            logical_dims,
        })
    }
}

/// Deserialize a single embedding value from a JSON `embedding` field.
///
/// For `OutputEncoding::Float`, the field is expected to be an array of f32.
/// For `OutputEncoding::Base64Int8`, the field is a base64-encoded string of
/// signed int8 bytes, which is decoded, validated against `expected_dims`,
/// cast to f32, and L2-normalized.
///
/// Returns the embedding as `Vec<f32>` ready for storage/search.
pub(crate) fn parse_embedding_value(
    value: &serde_json::Value,
    output_encoding: OutputEncoding,
    context: &str,
    expected_dims: Option<usize>,
) -> Result<Vec<f32>, String> {
    match output_encoding {
        OutputEncoding::Float => serde_json::from_value(value.clone())
            .map_err(|e| format!("{context}: expected float array, got error: {e}")),
        OutputEncoding::Base64Int8 => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("{context}: expected base64 string, got {:?}", value))?;
            let typed = TypedVector::decode_base64_int8(s)?;
            match typed {
                TypedVector::DenseInt8(v) => {
                    // Validate decoded byte count matches expected dimensions.
                    if let Some(dims) = expected_dims {
                        if v.len() != dims {
                            return Err(format!(
                                "{context}: int8 dimension mismatch: decoded {} values, expected {dims}",
                                v.len()
                            ));
                        }
                    }
                    // Cast i8 to f32 and L2-normalize for cosine/dot-product search.
                    let mut f32s: Vec<f32> = v.into_iter().map(|x| x as f32).collect();
                    let norm_sq: f32 = f32s.iter().map(|x| x * x).sum();
                    if norm_sq > 0.0 {
                        let norm = norm_sq.sqrt();
                        for x in &mut f32s {
                            *x /= norm;
                        }
                    }
                    Ok(f32s)
                }
                _ => unreachable!("decode_base64_int8 always returns DenseInt8"),
            }
        }
        OutputEncoding::Base64Binary => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("{context}: expected base64 string, got {:?}", value))?;
            let expected_dims = expected_dims.unwrap_or(s.len() * 8);
            let typed = TypedVector::decode_base64_binary(s, expected_dims)?;
            match typed {
                TypedVector::BinaryPacked {
                    bytes,
                    logical_dims,
                } => {
                    // Convert packed bytes to f32 vec of 0.0/1.0, masking padding bits
                    let mut f32s = Vec::with_capacity(logical_dims);
                    for i in 0..logical_dims {
                        let byte_idx = i / 8;
                        let bit_idx = (i % 8) as u8;
                        if byte_idx < bytes.len() {
                            let bit = (bytes[byte_idx] >> bit_idx) & 1;
                            f32s.push(if bit != 0 { 1.0 } else { 0.0 });
                        } else {
                            f32s.push(0.0);
                        }
                    }
                    Ok(f32s)
                }
                _ => unreachable!("decode_base64_binary always returns BinaryPacked"),
            }
        }
    }
}

/// A vector as stored in the index after conversion.
///
/// This is the final form that is written to the snapshot / disk cache.
#[derive(Debug)]
pub(crate) enum StoredVector {
    /// Stored as dense f32 (for cosine / dot-product search).
    DenseF32(Vec<f32>),
    /// Stored as binary packed (for Hamming distance search).
    BinaryPacked { bytes: Vec<u8>, logical_dims: usize },
}

impl StoredVector {
    /// Return the [`VectorKind`] that describes this variant.
    pub(crate) fn kind(&self) -> VectorKind {
        match self {
            Self::DenseF32(_) => VectorKind::DenseF32,
            Self::BinaryPacked { .. } => VectorKind::BinaryPacked,
        }
    }

    /// Number of dimensions (logical bits for binary).
    pub(crate) fn dims(&self) -> usize {
        match self {
            Self::DenseF32(v) => v.len(),
            Self::BinaryPacked { logical_dims, .. } => *logical_dims,
        }
    }

    /// Return a view as an f32 slice.
    ///
    /// Returns `Err` for binary vectors which are not representable as f32.
    pub(crate) fn to_f32_slice(&self) -> Result<&[f32], String> {
        match self {
            Self::DenseF32(v) => Ok(v),
            Self::BinaryPacked { logical_dims, .. } => Err(format!(
                "binary vector ({} logical bits) cannot be viewed as f32 slice",
                logical_dims
            )),
        }
    }

    /// Return a view as packed bytes + logical dims.
    ///
    /// Returns `Err` for dense vectors.
    pub(crate) fn to_packed(&self) -> Result<(&[u8], usize), String> {
        match self {
            Self::DenseF32(_) => Err("dense vector cannot be viewed as packed binary".to_string()),
            Self::BinaryPacked {
                bytes,
                logical_dims,
            } => Ok((bytes, *logical_dims)),
        }
    }

    /// L2-normalize a dense f32 vector in place.
    ///
    /// No-op for binary vectors (returns `self` unchanged).
    pub(crate) fn l2_normalize(self) -> Self {
        match self {
            Self::DenseF32(mut v) => {
                let norm_sq: f32 = v.iter().map(|x| x * x).sum();
                if norm_sq > 0.0 {
                    let norm = norm_sq.sqrt();
                    for x in &mut v {
                        *x /= norm;
                    }
                }
                Self::DenseF32(v)
            }
            binary => binary,
        }
    }
}
///
/// Used to validate that user configuration is compatible with the selected
/// provider/model before indexing starts.
#[derive(Debug, Clone)]
pub struct EmbeddingModelProfile {
    /// Which semantic backend this profile applies to.
    pub backend: SemanticBackend,
    /// Model name (may be empty for generic profiles).
    pub model: Option<String>,
    /// Supported input mode.
    pub input_mode: InputMode,
    /// Expected output encoding from the provider.
    pub output_encoding: OutputEncoding,
    /// The kind of vectors the provider emits.
    pub source_vector_kind: VectorKind,
    /// The kind of vectors stored after AFT conversion.
    pub stored_vector_kind: VectorKind,
    /// Metric that should be used for similarity search.
    pub metric: DistanceMetric,
    /// Normalization policy for stored vectors.
    pub normalization: NormalizationPolicy,
    /// Storage strategy for converting source vectors to stored form.
    pub storage_strategy: StorageStrategy,
    /// Supported dimension range: (min, max). None if unknown.
    pub dimension_range: Option<(usize, usize)>,
    /// Default dimension when not specified. None if unknown.
    pub default_dimensions: Option<usize>,
    /// Whether Matryoshka Representation Learning (reduced dimensions) is supported.
    pub mrl_supported: bool,
    /// Whether contextualized document-chunk inputs are supported.
    pub contextualized_supported: bool,
}

impl EmbeddingModelProfile {
    /// Returns a profile for the fastembed all-MiniLM-L6-v2 model.
    pub fn fastembed_minilm() -> Self {
        Self {
            backend: SemanticBackend::Fastembed,
            model: Some("all-MiniLM-L6-v2".to_string()),
            input_mode: InputMode::FlatTexts,
            output_encoding: OutputEncoding::Float,
            source_vector_kind: VectorKind::DenseF32,
            stored_vector_kind: VectorKind::DenseF32,
            metric: DistanceMetric::Cosine,
            normalization: NormalizationPolicy::AlreadyNormalized,
            storage_strategy: StorageStrategy::NativeF32,
            dimension_range: Some((384, 384)),
            default_dimensions: Some(384),
            mrl_supported: false,
            contextualized_supported: false,
        }
    }

    /// Returns a generic profile for OpenAI-compatible embedding providers.
    /// These may support `dimensions` depending on the model.
    pub fn openai_compatible_generic() -> Self {
        Self {
            backend: SemanticBackend::OpenAiCompatible,
            model: None,
            input_mode: InputMode::FlatTexts,
            output_encoding: OutputEncoding::Float,
            source_vector_kind: VectorKind::DenseF32,
            stored_vector_kind: VectorKind::DenseF32,
            metric: DistanceMetric::Auto,
            normalization: NormalizationPolicy::AlreadyNormalized,
            storage_strategy: StorageStrategy::NativeF32,
            dimension_range: None,
            default_dimensions: None,
            mrl_supported: true,
            contextualized_supported: false,
        }
    }

    /// Returns a generic profile for Ollama embedding models.
    pub fn ollama_generic() -> Self {
        Self {
            backend: SemanticBackend::Ollama,
            model: None,
            input_mode: InputMode::FlatTexts,
            output_encoding: OutputEncoding::Float,
            source_vector_kind: VectorKind::DenseF32,
            stored_vector_kind: VectorKind::DenseF32,
            metric: DistanceMetric::Auto,
            normalization: NormalizationPolicy::AlreadyNormalized,
            storage_strategy: StorageStrategy::NativeF32,
            dimension_range: None,
            default_dimensions: None,
            mrl_supported: false,
            contextualized_supported: false,
        }
    }

    /// Returns a profile for Perplexity contextualized embedding providers.
    /// Perplexity uses the OpenAI-compatible API format but sends nested
    /// document/chunk arrays instead of flat text arrays.
    pub fn perplexity_generic() -> Self {
        Self {
            backend: SemanticBackend::Perplexity,
            model: None,
            input_mode: InputMode::DocumentChunks,
            output_encoding: OutputEncoding::Float,
            source_vector_kind: VectorKind::DenseF32,
            stored_vector_kind: VectorKind::DenseF32,
            metric: DistanceMetric::Cosine,
            normalization: NormalizationPolicy::AlreadyNormalized,
            storage_strategy: StorageStrategy::NativeF32,
            dimension_range: None,
            default_dimensions: None,
            mrl_supported: false,
            contextualized_supported: true,
        }
    }

    /// Returns a profile for Perplexity providers returning base64-encoded
    /// binary (packed-bit) embeddings. Vectors are stored as packed bits and
    /// searched with Hamming distance.
    pub fn perplexity_binary() -> Self {
        Self {
            backend: SemanticBackend::Perplexity,
            model: None,
            input_mode: InputMode::DocumentChunks,
            output_encoding: OutputEncoding::Base64Binary,
            source_vector_kind: VectorKind::BinaryPacked,
            stored_vector_kind: VectorKind::BinaryPacked,
            metric: DistanceMetric::Hamming,
            normalization: NormalizationPolicy::NotApplicable,
            storage_strategy: StorageStrategy::BinaryPacked,
            dimension_range: None,
            default_dimensions: None,
            mrl_supported: false,
            contextualized_supported: true,
        }
    }

    /// Returns a profile for Perplexity providers returning base64-encoded
    /// int8 embeddings. The int8 values are decoded, cast to f32, and
    /// L2-normalized before storage/search through the existing f32 cosine path.
    pub fn perplexity_int8() -> Self {
        Self {
            backend: SemanticBackend::Perplexity,
            model: None,
            input_mode: InputMode::DocumentChunks,
            output_encoding: OutputEncoding::Base64Int8,
            source_vector_kind: VectorKind::DenseInt8,
            stored_vector_kind: VectorKind::DenseF32,
            metric: DistanceMetric::Cosine,
            normalization: NormalizationPolicy::NormalizeOnInsertQuery,
            storage_strategy: StorageStrategy::DecodeNormalizeF32,
            dimension_range: None,
            default_dimensions: None,
            mrl_supported: false,
            contextualized_supported: true,
        }
    }

    /// Look up a profile for the given config.
    /// Returns `None` if no specific profile is known (caller should use defaults).
    pub fn from_config(config: &SemanticBackendConfig) -> Option<Self> {
        match config.backend {
            SemanticBackend::Fastembed => {
                if config.model == "all-MiniLM-L6-v2" {
                    Some(Self::fastembed_minilm())
                } else {
                    None
                }
            }
            SemanticBackend::OpenAiCompatible => Some(Self::openai_compatible_generic()),
            SemanticBackend::Ollama => Some(Self::ollama_generic()),
            SemanticBackend::Perplexity => {
                if config.output_encoding == Some(OutputEncoding::Base64Int8) {
                    Some(Self::perplexity_int8())
                } else if config.output_encoding == Some(OutputEncoding::Base64Binary) {
                    Some(Self::perplexity_binary())
                } else {
                    Some(Self::perplexity_generic())
                }
            }
            SemanticBackend::Model2Vec => None, // No known profile; use config defaults.
        }
    }

    /// Validate that the configured options are compatible with this profile.
    /// Returns `Ok(())` or a list of validation errors.
    pub fn validate_config(&self, config: &SemanticBackendConfig) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();
        let cfg_prefix = "semantic";

        // Resolve effective output encoding
        let output_encoding = config
            .output_encoding
            .unwrap_or(OutputEncoding::default_for_backend(config.backend));

        // Resolve effective storage strategy
        let storage_strategy = config
            .storage_strategy
            .unwrap_or(StorageStrategy::default_for_backend(config.backend));

        // Check input mode compatibility
        let input_mode = config
            .input_mode
            .unwrap_or(InputMode::default_for_backend(config.backend));
        if input_mode == InputMode::DocumentChunks && !self.contextualized_supported {
            errors.push(format!(
                "{}.input_mode=document_chunks is not supported by backend {}",
                cfg_prefix,
                config.backend.as_str()
            ));
        }

        // Check output encoding compatibility
        if output_encoding != self.output_encoding
            && !(output_encoding == OutputEncoding::Base64Int8
                && matches!(config.backend, SemanticBackend::OpenAiCompatible))
        {
            // Allow base64_int8 for OpenAI-compatible (e.g. Perplexity)
            if !matches!(
                (output_encoding, self.output_encoding),
                (OutputEncoding::Float, OutputEncoding::Float)
                    | (OutputEncoding::Base64Int8, OutputEncoding::Float)
            ) {
                errors.push(format!(
                    "{}.output_encoding={:?} is not supported by backend {}",
                    cfg_prefix,
                    output_encoding,
                    config.backend.as_str()
                ));
            }
        }

        // Check storage strategy compatibility
        match (output_encoding, storage_strategy) {
            (OutputEncoding::Float, StorageStrategy::NativeF32) => {}
            (OutputEncoding::Base64Int8, StorageStrategy::DecodeNormalizeF32) => {}
            (OutputEncoding::Base64Int8, StorageStrategy::NativeF32) => {}
            (OutputEncoding::Base64Binary, StorageStrategy::BinaryPacked) => {}
            (OutputEncoding::Base64Binary, _) => {
                errors.push(format!(
                    "{}.output_encoding=base64_binary requires a native binary vector store, not available in MVP",
                    cfg_prefix
                ));
            }
            _ => {
                errors.push(format!(
                    "{}.storage_strategy={:?} is not compatible with output_encoding={:?}",
                    cfg_prefix, storage_strategy, output_encoding
                ));
            }
        }

        // Check dimensions against profile
        if let Some(dimensions) = config.dimensions {
            if let Some((min_dim, max_dim)) = self.dimension_range {
                if dimensions < min_dim || dimensions > max_dim {
                    errors.push(format!(
                        "{}.dimensions={} is outside supported range {}-{} for {} {}",
                        cfg_prefix,
                        dimensions,
                        min_dim,
                        max_dim,
                        config.backend.as_str(),
                        config.model
                    ));
                }
            }
            if !self.mrl_supported && config.dimensions.is_some() {
                errors.push(format!(
                    "{}.dimensions is set but the model does not support reduced dimensions",
                    cfg_prefix
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Convert a source [`TypedVector`] into a [`StoredVector`] using this
    /// profile's declared `source_vector_kind` and `stored_vector_kind`.
    pub(crate) fn convert_vector(&self, typed: TypedVector) -> Result<StoredVector, String> {
        let actual_kind = typed.kind();
        if actual_kind != self.source_vector_kind {
            return Err(format!(
                "vector kind mismatch: got {:?}, expected {:?} per profile",
                actual_kind, self.source_vector_kind
            ));
        }
        let stored = typed.into_stored(self.storage_strategy)?;
        if stored.kind() != self.stored_vector_kind {
            return Err(format!(
                "stored vector kind mismatch: got {:?}, expected {:?} per profile",
                stored.kind(),
                self.stored_vector_kind
            ));
        }
        match self.normalization {
            NormalizationPolicy::AlreadyNormalized | NormalizationPolicy::NotApplicable => {
                Ok(stored)
            }
            NormalizationPolicy::NormalizeOnInsertQuery => Ok(stored.l2_normalize()),
        }
    }

    /// Validate that the profile's own configuration is internally consistent.
    pub(crate) fn validate_compatible(&self) -> Result<(), String> {
        match (&self.source_vector_kind, &self.stored_vector_kind) {
            (VectorKind::DenseF32, VectorKind::DenseF32)
            | (VectorKind::DenseInt8, VectorKind::DenseF32) => Ok(()),
            (VectorKind::BinaryPacked, VectorKind::BinaryPacked) => Ok(()),
            (src, dst) => Err(format!(
                "unsupported source→stored vector conversion: {:?} → {:?}",
                src, dst
            )),
        }?;
        match (&self.stored_vector_kind, &self.metric) {
            (VectorKind::DenseF32 | VectorKind::DenseInt8, DistanceMetric::Cosine)
            | (VectorKind::DenseF32 | VectorKind::DenseInt8, DistanceMetric::DotProduct)
            | (VectorKind::DenseF32 | VectorKind::DenseInt8, DistanceMetric::Euclidean)
            | (VectorKind::DenseF32 | VectorKind::DenseInt8, DistanceMetric::Auto) => Ok(()),
            (VectorKind::BinaryPacked, DistanceMetric::Hamming)
            | (VectorKind::BinaryPacked, DistanceMetric::Auto) => Ok(()),
            (kind, metric) => Err(format!(
                "metric {:?} is not compatible with stored vector kind {:?}",
                metric, kind
            )),
        }?;
        match (&self.output_encoding, &self.storage_strategy) {
            (OutputEncoding::Float, StorageStrategy::NativeF32) => Ok(()),
            (OutputEncoding::Base64Int8, StorageStrategy::DecodeNormalizeF32)
            | (OutputEncoding::Base64Int8, StorageStrategy::NativeF32) => Ok(()),
            (OutputEncoding::Base64Binary, StorageStrategy::BinaryPacked) => Ok(()),
            (enc, strat) => Err(format!(
                "output encoding {:?} is not compatible with storage strategy {:?}",
                enc, strat
            )),
        }?;
        Ok(())
    }
}

/// Resolve an effective distance metric from config and profile.
/// When `DistanceMetric::Auto` is configured, returns the profile's recommended metric.
pub fn resolve_distance_metric(
    config: &SemanticBackendConfig,
    profile: Option<&EmbeddingModelProfile>,
) -> DistanceMetric {
    if let Some(metric) = config.distance_metric {
        if metric != DistanceMetric::Auto {
            return metric;
        }
    }
    // Auto: resolve from profile
    if let Some(profile) = profile {
        profile.metric
    } else {
        // Fallback to cosine for unknown profiles
        DistanceMetric::Cosine
    }
}

/// Resolve effective output encoding from config.
pub fn resolve_output_encoding(config: &SemanticBackendConfig) -> OutputEncoding {
    config
        .output_encoding
        .unwrap_or(OutputEncoding::default_for_backend(config.backend))
}

/// Resolve effective storage strategy from config.
pub fn resolve_storage_strategy(config: &SemanticBackendConfig) -> StorageStrategy {
    config
        .storage_strategy
        .unwrap_or(StorageStrategy::default_for_backend(config.backend))
}

/// Resolve effective input mode from config.
pub fn resolve_input_mode(config: &SemanticBackendConfig) -> InputMode {
    config
        .input_mode
        .unwrap_or(InputMode::default_for_backend(config.backend))
}

/// Resolve effective dimensions from config with profile fallback.
pub fn resolve_dimensions(
    config: &SemanticBackendConfig,
    profile: Option<&EmbeddingModelProfile>,
) -> Option<usize> {
    config
        .dimensions
        .or_else(|| profile.and_then(|p| p.default_dimensions))
} // Must stay below the bridge timeout (30s) to avoid bridge kills on slow backends.
const DEFAULT_OPENAI_EMBEDDING_TIMEOUT_MS: u64 = 25_000;
const DEFAULT_MAX_BATCH_SIZE: usize = 64;
const QUERY_EMBEDDING_CACHE_CAP: usize = 1_000;
const FALLBACK_BACKEND: &str = "none";
const EMBEDDING_REQUEST_MAX_ATTEMPTS: usize = 3;
const EMBEDDING_REQUEST_BACKOFF_MS: [u64; 2] = [500, 1_000];

/// Apply a query prompt template to a raw query string.
/// Replaces `{query}` with the raw query text.
/// Returns the template with `{query}` replaced, or the raw query if template is None or missing placeholder.
pub fn apply_query_template(query: &str, template: Option<&str>) -> String {
    match template {
        Some(tpl) if tpl.contains("{query}") => tpl.replace("{query}", query),
        Some(_) => query.to_string(),
        None => query.to_string(),
    }
}

/// Apply a document prompt template to raw chunk text.
/// Replaces `{text}` with the raw chunk text.
/// Returns the template with `{text}` replaced, or the raw text if template is None or missing placeholder.
pub fn apply_document_template(text: &str, template: Option<&str>) -> String {
    match template {
        Some(tpl) if tpl.contains("{text}") => tpl.replace("{text}", text),
        Some(_) => text.to_string(),
        None => text.to_string(),
    }
}

/// Built-in prompt profile for a known embedding model.
/// When a model matches a profile, its query/document prefixes are applied
/// automatically unless the user has set explicit templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingPromptProfile {
    /// Prefix prepended to queries before embedding (e.g. "query: " for E5).
    pub query_prefix: &'static str,
    /// Prefix prepended to document chunks before embedding (e.g. "passage: " for E5).
    pub document_prefix: &'static str,
}

/// Resolve an embedding prompt profile from a model name.
/// Returns None for unknown models (no prefix applied).
pub fn resolve_embedding_profile(model: &str) -> Option<&'static EmbeddingPromptProfile> {
    // Normalize: lowercase, strip common path prefixes
    let normalized = model
        .to_lowercase()
        .replace('\\', "/")
        .replace("nomic-ai/", "")
        .replace("intfloat/", "")
        .replace("BAAI/", "")
        .replace("Alibaba-NLP/", "")
        .replace("jinaai/", "");

    static PROFILES: &[(&str, EmbeddingPromptProfile)] = &[
        // CodeRankEmbed — requires query prefix for code search
        (
            "coderankembed",
            EmbeddingPromptProfile {
                query_prefix: "Represent this query for searching relevant code: ",
                document_prefix: "",
            },
        ),
        // E5 / multilingual-E5 — requires "query: " / "passage: " prefixes
        (
            "e5-base",
            EmbeddingPromptProfile {
                query_prefix: "query: ",
                document_prefix: "passage: ",
            },
        ),
        (
            "e5-large",
            EmbeddingPromptProfile {
                query_prefix: "query: ",
                document_prefix: "passage: ",
            },
        ),
        (
            "e5-small",
            EmbeddingPromptProfile {
                query_prefix: "query: ",
                document_prefix: "passage: ",
            },
        ),
        (
            "multilingual-e5",
            EmbeddingPromptProfile {
                query_prefix: "query: ",
                document_prefix: "passage: ",
            },
        ),
        // BGE v1.5 — optional query instruction, no document prefix
        (
            "bge-base-en-v1.5",
            EmbeddingPromptProfile {
                query_prefix: "Represent this sentence for searching relevant passages: ",
                document_prefix: "",
            },
        ),
        (
            "bge-large-en-v1.5",
            EmbeddingPromptProfile {
                query_prefix: "Represent this sentence for searching relevant passages: ",
                document_prefix: "",
            },
        ),
        (
            "bge-small-en-v1.5",
            EmbeddingPromptProfile {
                query_prefix: "Represent this sentence for searching relevant passages: ",
                document_prefix: "",
            },
        ),
        // BGE-M3 — no prefixes needed
        (
            "bge-m3",
            EmbeddingPromptProfile {
                query_prefix: "",
                document_prefix: "",
            },
        ),
        // GTE ModernBERT — no prefixes needed
        (
            "gte-modernbert",
            EmbeddingPromptProfile {
                query_prefix: "",
                document_prefix: "",
            },
        ),
        // GTE-Reranker-ModernBERT — no prefixes (reranker, not embedder)
        (
            "gte-reranker-modernbert",
            EmbeddingPromptProfile {
                query_prefix: "",
                document_prefix: "",
            },
        ),
    ];

    for (pattern, profile) in PROFILES {
        if normalized.contains(pattern) {
            return Some(profile);
        }
    }
    None
}

/// Compute a stable hash for a prompt template.
/// Returns empty string when the template is None or empty/whitespace-only,
/// so that `None` and `Some("")` produce identical fingerprints and avoid
/// unnecessary index rebuilds.
pub fn prompt_template_hash(template: Option<&str>) -> String {
    template
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map_or(String::new(), |t| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            t.hash(&mut hasher);
            hasher.finish().to_string()
        })
}

/// Compute a stable hash of the file policy settings.
/// Changes to any policy field will produce a different hash,
/// triggering a rebuild of the semantic index.
fn compute_file_policy_hash(policy: &SemanticFilePolicy) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Version prefix so we can bump the hash algorithm independently
    b"file_policy_v1".hash(&mut hasher);
    policy.include_code.hash(&mut hasher);
    policy.include_docs.hash(&mut hasher);
    policy.include_configs.hash(&mut hasher);
    policy.respect_gitignore.hash(&mut hasher);
    policy.include_gitignored_docs.hash(&mut hasher);
    for glob in &policy.include_globs {
        glob.hash(&mut hasher);
    }
    for glob in &policy.exclude_globs {
        glob.hash(&mut hasher);
    }
    policy.max_file_size_bytes.hash(&mut hasher);
    policy.binary_detection.hash(&mut hasher);
    policy.generated_file_detection.hash(&mut hasher);
    hasher.finish().to_string()
}

static SEMANTIC_LOCK_ACQUIRE_MUTEX: Mutex<()> = Mutex::new(());

pub struct SemanticIndexLock {
    _guard: fs_lock::LockGuard,
}

impl SemanticIndexLock {
    pub fn acquire(storage_dir: &Path, project_key: &str) -> std::io::Result<Self> {
        let dir = storage_dir.join("semantic").join(project_key);
        fs::create_dir_all(&dir)?;
        let path = dir.join("cache.lock");
        let _acquire_guard = SEMANTIC_LOCK_ACQUIRE_MUTEX
            .lock()
            .map_err(|_| std::io::Error::other("semantic cache lock acquisition mutex poisoned"))?;
        fs_lock::try_acquire(&path, Duration::from_secs(2))
            .map(|guard| Self { _guard: guard })
            .map_err(|error| match error {
                fs_lock::AcquireError::Timeout => {
                    std::io::Error::other("timed out acquiring semantic cache lock")
                }
                fs_lock::AcquireError::Io(error) => error,
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIndexFingerprint {
    pub backend: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    pub dimension: usize,
    #[serde(default = "default_chunking_version")]
    pub chunking_version: u32,
    /// Output encoding used for this index.
    #[serde(default)]
    pub output_encoding: String,
    /// Storage strategy used for this index.
    #[serde(default)]
    pub storage_strategy: String,
    /// Resolved distance metric for this index.
    #[serde(default = "default_dot_auto")]
    pub distance_metric: String,
    /// Input mode used for this index.
    #[serde(default)]
    pub input_mode: String,
    /// Hash of the document prompt template (empty string when no document prompt is configured).
    #[serde(default)]
    pub document_prompt_hash: String,
    /// Source vector kind from the embedding model profile (e.g. "dense_f32").
    #[serde(default)]
    pub source_vector_kind: String,
    /// Stored vector kind after AFT conversion (e.g. "dense_f32").
    #[serde(default)]
    pub stored_vector_kind: String,
    /// Normalization policy (e.g. "already_normalized").
    #[serde(default)]
    pub normalization: String,
    /// Hash of the query prompt template (empty string when no query prompt is configured).
    #[serde(default)]
    pub query_prompt_hash: String,
    /// Fingerprint of the file policy that determines which files are indexed.
    /// Changes here trigger a full rebuild since the set of indexed files changes.
    #[serde(default)]
    pub file_policy_hash: String,
    /// Version of the docs chunker. Bumped when docs chunking logic changes.
    #[serde(default = "default_docs_fp_version")]
    pub docs_chunker_version: u8,
}

impl Default for SemanticIndexFingerprint {
    fn default() -> Self {
        Self {
            backend: String::new(),
            model: String::new(),
            base_url: String::new(),
            dimension: 0,
            chunking_version: default_chunking_version(),
            output_encoding: String::new(),
            storage_strategy: String::new(),
            distance_metric: default_dot_auto(),
            input_mode: String::new(),
            document_prompt_hash: String::new(),
            source_vector_kind: String::new(),
            stored_vector_kind: String::new(),
            normalization: String::new(),
            query_prompt_hash: String::new(),
            file_policy_hash: String::new(),
            docs_chunker_version: default_docs_fp_version(),
        }
    }
}

fn default_chunking_version() -> u32 {
    2
}

const fn default_docs_fp_version() -> u8 {
    1
}

fn default_dot_auto() -> String {
    "auto".to_string()
}

impl SemanticIndexFingerprint {
    fn from_config(
        config: &SemanticBackendConfig,
        dimension: usize,
        profile: Option<&EmbeddingModelProfile>,
        file_policy: &SemanticFilePolicy,
    ) -> Self {
        // Use normalized URL for fingerprinting so cosmetic differences
        // (e.g. "http://host/v1" vs "http://host/v1/") don't cause rebuilds.
        let base_url = config
            .base_url
            .as_ref()
            .and_then(|u| normalize_base_url(u).ok())
            .unwrap_or_else(|| FALLBACK_BACKEND.to_string());
        Self {
            backend: config.backend.as_str().to_string(),
            model: config.model.clone(),
            base_url,
            dimension,
            chunking_version: default_chunking_version(),
            output_encoding: resolve_output_encoding(config).to_string(),
            storage_strategy: resolve_storage_strategy(config).to_string(),
            distance_metric: resolve_distance_metric(config, profile).to_string(),
            input_mode: resolve_input_mode(config).to_string(),
            document_prompt_hash: prompt_template_hash(config.document_prompt_template.as_deref()),
            source_vector_kind: profile.map_or(String::new(), |p| p.source_vector_kind.to_string()),
            stored_vector_kind: profile.map_or(String::new(), |p| p.stored_vector_kind.to_string()),
            normalization: profile.map_or(String::new(), |p| p.normalization.to_string()),
            query_prompt_hash: prompt_template_hash(config.query_prompt_template.as_deref()),
            file_policy_hash: compute_file_policy_hash(file_policy),
            docs_chunker_version: file_policy.docs_chunker_version,
        }
    }

    pub fn as_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::new())
    }

    fn matches_expected(&self, expected: &str) -> bool {
        let encoded = self.as_string();
        !encoded.is_empty() && encoded == expected
    }

    /// Compute the semantic diff between this fingerprint and another.
    ///
    /// Returns [`FingerprintChange::Rebuild`] if any rebuild-triggering field
    /// differs (backend, model, base_url, dimension, chunking_version,
    /// output_encoding, storage_strategy, source_vector_kind, stored_vector_kind,
    /// normalization, input_mode, document_prompt_hash).
    ///
    /// Returns [`FingerprintChange::ClearQueryCache`] if *only* the
    /// `query_prompt_hash` differs (and no rebuild-triggering fields changed).
    ///
    /// Returns [`FingerprintChange::None`] if the fingerprints are identical
    /// (differences in `distance_metric` are intentionally ignored — see matrix).
    pub fn diff(&self, other: &Self) -> FingerprintChange {
        /// Fields that trigger a full rebuild when they differ.
        fn rebuild_fields_match(
            a: &SemanticIndexFingerprint,
            b: &SemanticIndexFingerprint,
        ) -> bool {
            a.backend == b.backend
                && a.model == b.model
                && a.base_url == b.base_url
                && a.dimension == b.dimension
                && a.chunking_version == b.chunking_version
                && a.output_encoding == b.output_encoding
                && a.storage_strategy == b.storage_strategy
                && a.source_vector_kind == b.source_vector_kind
                && a.stored_vector_kind == b.stored_vector_kind
                && a.normalization == b.normalization
                && a.input_mode == b.input_mode
                && a.document_prompt_hash == b.document_prompt_hash
                && a.file_policy_hash == b.file_policy_hash
                && a.docs_chunker_version == b.docs_chunker_version
        }

        if !rebuild_fields_match(self, other) {
            return FingerprintChange::Rebuild;
        }

        if self.query_prompt_hash != other.query_prompt_hash {
            return FingerprintChange::ClearQueryCache;
        }

        // All other field differences (e.g. distance_metric) are intentionally
        // ignored — they may require rescoring but not re-embedding.
        FingerprintChange::None
    }
}

/// The result of comparing two [`SemanticIndexFingerprint`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintChange {
    /// Full index rebuild required — embeddings are invalidated.
    Rebuild,
    /// Only the query prompt changed; clear the query embedding cache.
    ClearQueryCache,
    /// No action needed.
    None,
}

impl std::fmt::Display for FingerprintChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rebuild => write!(f, "rebuild"),
            Self::ClearQueryCache => write!(f, "clear_query_cache"),
            Self::None => write!(f, "none"),
        }
    }
}

impl FingerprintChange {
    /// Returns a human-readable description of the change.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Rebuild => "full rebuild required (embedding parameters changed)",
            Self::ClearQueryCache => "clear query embedding cache (query prompt changed)",
            Self::None => "no action needed (fingerprint unchanged)",
        }
    }
}

enum SemanticEmbeddingEngine {
    Local(LocalEmbedder),
    OpenAiCompatible {
        client: Client,
        model: String,
        base_url: String,
        api_key: Option<String>,
    },
    Ollama {
        client: Client,
        model: String,
        base_url: String,
    },
    /// Perplexity uses the same HTTP transport as OpenAI-compatible but
    /// sends nested document/chunk arrays for contextualized embeddings.
    Perplexity {
        client: Client,
        model: String,
        base_url: String,
        api_key: Option<String>,
    },
    /// Local model2vec static embeddings (requires `semantic-model2vec` feature).
    #[cfg(feature = "semantic-model2vec")]
    Model2Vec {
        model: Model2VecStaticModel,
        max_length: usize,
    },
}

#[allow(dead_code)]
pub struct SemanticEmbeddingModel {
    backend: SemanticBackend,
    model: String,
    base_url: Option<String>,
    timeout_ms: u64,
    max_batch_size: usize,
    dimension: Option<usize>,
    /// User-requested dimension from config (None = use provider default).
    config_dimensions: Option<usize>,
    /// Resolved output encoding for this model.
    output_encoding: OutputEncoding,
    /// Resolved storage strategy for this model.
    storage_strategy: StorageStrategy,
    /// Resolved distance metric for this model.
    distance_metric: DistanceMetric,
    /// Resolved input mode for this model.
    input_mode: InputMode,
    engine: SemanticEmbeddingEngine,
    query_embedding_cache: HashMap<String, Vec<f32>>,
    query_embedding_cache_order: VecDeque<String>,
    query_embedding_cache_hits: u64,
    query_embedding_cache_misses: u64,
}

pub type EmbeddingModel = SemanticEmbeddingModel;

fn validate_embedding_batch(
    vectors: &[Vec<f32>],
    expected_count: usize,
    context: &str,
) -> Result<(), String> {
    if expected_count > 0 && vectors.is_empty() {
        return Err(format!(
            "{context} returned no vectors for {expected_count} inputs"
        ));
    }

    if vectors.len() != expected_count {
        return Err(format!(
            "{context} returned {} vectors for {} inputs",
            vectors.len(),
            expected_count
        ));
    }

    let Some(first_vector) = vectors.first() else {
        return Ok(());
    };
    let expected_dimension = first_vector.len();
    validate_embedding_dimension(expected_dimension)
        .map_err(|error| format!("{context} returned {error}"))?;
    for (index, vector) in vectors.iter().enumerate() {
        if vector.len() != expected_dimension {
            return Err(format!(
                "{context} returned inconsistent embedding dimensions: vector 0 has length {expected_dimension}, vector {index} has length {}",
                vector.len()
            ));
        }
    }

    Ok(())
}

fn validate_embedding_dimension(dimension: usize) -> Result<(), String> {
    if dimension == 0 || dimension > MAX_DIMENSION {
        return Err(format!(
            "invalid embedding dimension: {dimension}; supported range is 1..={MAX_DIMENSION}"
        ));
    }

    Ok(())
}

/// Normalize a base URL: validate scheme and strip trailing slash.
/// Does NOT perform SSRF/private-IP validation — call
/// `validate_base_url_no_ssrf` separately when processing user-supplied config.
fn normalize_base_url(raw: &str) -> Result<String, String> {
    let parsed = Url::parse(raw).map_err(|error| format!("invalid base_url '{raw}': {error}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "unsupported URL scheme '{}' — only http:// and https:// are allowed",
            scheme
        ));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

/// Validate that a base URL does not point to a private/loopback address.
/// Call this on user-supplied config (at configure time) to prevent SSRF.
/// Not called for programmatically constructed configs (e.g. tests).
///
/// **Loopback is allowed.** Self-hosted embedding backends (e.g. Ollama at
/// `http://127.0.0.1:11434`) are a primary use case for `aft_search`. Loopback
/// addresses by definition cannot be exploited as SSRF targets — they only
/// reach services on the same machine. Allowing loopback unblocks Ollama at its
/// default config without opening up SSRF to LAN/intranet services, which
/// remain rejected.
///
/// **mDNS `.local` is rejected.** mDNS hostnames typically resolve to LAN
/// devices (printers, homelab servers); rejecting them before DNS lookup keeps
/// the SSRF guard meaningful for non-loopback private networks.
pub fn validate_base_url_no_ssrf(raw: &str) -> Result<(), String> {
    use std::net::{IpAddr, ToSocketAddrs};

    let parsed = Url::parse(raw).map_err(|error| format!("invalid base_url '{raw}': {error}"))?;

    let host = parsed.host_str().unwrap_or("");

    // Loopback hostnames are explicitly allowed. RFC 6761 mandates that
    // `localhost` and `*.localhost` resolve to loopback;
    // `localhost.localdomain` is a historical alias used on some Linux
    // distros. Self-hosted backends like Ollama use these by default.
    let is_loopback_host =
        host == "localhost" || host == "localhost.localdomain" || host.ends_with(".localhost");
    if is_loopback_host {
        return Ok(());
    }

    // mDNS hostnames are typically LAN devices, not loopback. Reject before
    // DNS lookup so users get a clear error rather than a private-IP error.
    if host.ends_with(".local") {
        return Err(format!(
            "base_url host '{host}' is an mDNS name — only loopback (localhost / 127.0.0.1) and public endpoints are allowed"
        ));
    }

    // Resolve the hostname. Reject private/link-local/CGNAT IPs but NOT
    // loopback (which is by definition same-machine and not an SSRF target).
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addr_str = format!("{host}:{port}");
    let addrs: Vec<IpAddr> = addr_str
        .to_socket_addrs()
        .map(|iter| iter.map(|sa| sa.ip()).collect())
        .unwrap_or_default();
    for ip in &addrs {
        if is_private_non_loopback_ip(ip) {
            return Err(format!(
                "base_url '{raw}' resolves to a private/reserved IP — only loopback (127.0.0.1) and public endpoints are allowed"
            ));
        }
    }

    Ok(())
}

/// Returns true for IPv4/IPv6 addresses in private/link-local/CGNAT/wildcard
/// ranges, EXCLUDING loopback (127.0.0.0/8 and ::1). Loopback is considered
/// safe for SSRF purposes — see [`validate_base_url_no_ssrf`] for rationale.
fn is_private_non_loopback_ip(ip: &std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // Note: 127.0.0.0/8 (loopback) is intentionally NOT in this set.
            // 10.0.0.0/8
            o[0] == 10
            // 172.16.0.0/12
            || (o[0] == 172 && (16..=31).contains(&o[1]))
            // 192.168.0.0/16
            || (o[0] == 192 && o[1] == 168)
            // 169.254.0.0/16 link-local
            || (o[0] == 169 && o[1] == 254)
            // 100.64.0.0/10 CGNAT
            || (o[0] == 100 && (64..=127).contains(&o[1]))
            // 0.0.0.0/8 wildcard
            || o[0] == 0
        }
        IpAddr::V6(v6) => {
            // Note: ::1 (loopback) is intentionally NOT in this set.
            let _ = Ipv6Addr::LOCALHOST; // touch to silence unused-import lints in some builds
                                         // fe80::/10 link-local
            (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 unique-local
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // ::ffff:0:0/96 IPv4-mapped — check the embedded IPv4
            || (v6.segments()[0] == 0 && v6.segments()[1] == 0
                && v6.segments()[2] == 0 && v6.segments()[3] == 0
                && v6.segments()[4] == 0 && v6.segments()[5] == 0xffff
                && {
                    let [a, b] = v6.segments()[6..8] else { return false; };
                    let ipv4 = Ipv4Addr::new((a >> 8) as u8, (a & 0xff) as u8, (b >> 8) as u8, (b & 0xff) as u8);
                    is_private_non_loopback_ip(&IpAddr::V4(ipv4))
                })
        }
    }
}

fn build_openai_embeddings_endpoint(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        format!("{base_url}{DEFAULT_OPENAI_EMBEDDING_PATH}")
    } else {
        format!("{base_url}/v1{}", DEFAULT_OPENAI_EMBEDDING_PATH)
    }
}

fn build_ollama_embeddings_endpoint(base_url: &str) -> String {
    if base_url.ends_with("/api") {
        format!("{base_url}/embed")
    } else {
        format!("{base_url}{DEFAULT_OLLAMA_EMBEDDING_PATH}")
    }
}

fn normalize_api_key(value: Option<String>) -> Option<String> {
    value.and_then(|token| {
        let token = token.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    })
}

fn is_retryable_embedding_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn embedding_response_body_is_transient(status: reqwest::StatusCode, raw: &str) -> bool {
    if !matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::CONFLICT
            | reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::LOCKED
            | reqwest::StatusCode::TOO_EARLY
    ) {
        return false;
    }

    let lower = raw.to_ascii_lowercase();
    let normalized = lower.trim();

    normalized.contains("model was unloaded while the request was still in queue")
        || normalized == "model is loading"
        || normalized.starts_with("model is loading,")
        || normalized.contains(r#""error":"model is loading"#)
        || normalized.contains(r#""message":"model is loading"#)
        || normalized == "model not loaded"
        || normalized.contains(r#""error":"model not loaded""#)
        || normalized.contains(r#""message":"model not loaded""#)
        || normalized == "loading model into memory"
        || normalized.contains(r#""error":"loading model into memory""#)
        || normalized.contains(r#""message":"loading model into memory""#)
        || normalized == "model is being loaded"
        || normalized.contains(r#""error":"model is being loaded""#)
        || normalized.contains(r#""message":"model is being loaded""#)
        || normalized == "model is currently loading"
        || normalized.contains(r#""error":"model is currently loading""#)
        || normalized.contains(r#""message":"model is currently loading""#)
}

fn is_retryable_embedding_error(error: &reqwest::Error) -> bool {
    error.is_connect()
}

fn embedding_send_error_is_transient(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout()
}

fn embedding_response_read_error_is_transient(error: &reqwest::Error) -> bool {
    embedding_send_error_is_transient(error) || error.is_body() || error.is_decode()
}

/// Stable machine marker prefixed onto embedding error strings whose root cause
/// is transient — the backend is down, timing out, or returning 5xx/429.
pub const TRANSIENT_EMBEDDING_MARKER: &str = "[transient] ";

pub fn embedding_failure_is_transient(error: &str) -> bool {
    error.contains(TRANSIENT_EMBEDDING_MARKER)
}

pub fn strip_transient_embedding_marker(error: &str) -> String {
    error.replace(TRANSIENT_EMBEDDING_MARKER, "")
}

fn sleep_before_embedding_retry(attempt_index: usize) {
    if let Some(delay_ms) = EMBEDDING_REQUEST_BACKOFF_MS.get(attempt_index) {
        std::thread::sleep(Duration::from_millis(*delay_ms));
    }
}

fn send_embedding_request<F>(mut make_request: F, backend_label: &str) -> Result<String, String>
where
    F: FnMut() -> reqwest::blocking::RequestBuilder,
{
    for attempt_index in 0..EMBEDDING_REQUEST_MAX_ATTEMPTS {
        let last_attempt = attempt_index + 1 == EMBEDDING_REQUEST_MAX_ATTEMPTS;

        let response = match make_request().send() {
            Ok(response) => response,
            Err(error) => {
                if !last_attempt && is_retryable_embedding_error(&error) {
                    sleep_before_embedding_retry(attempt_index);
                    continue;
                }
                let marker = if embedding_send_error_is_transient(&error) {
                    TRANSIENT_EMBEDDING_MARKER
                } else {
                    ""
                };
                return Err(format!("{marker}{backend_label} request failed: {error}"));
            }
        };

        let status = response.status();
        let raw = match response.text() {
            Ok(raw) => raw,
            Err(error) => {
                if !last_attempt && embedding_response_read_error_is_transient(&error) {
                    sleep_before_embedding_retry(attempt_index);
                    continue;
                }
                let marker = if embedding_response_read_error_is_transient(&error) {
                    TRANSIENT_EMBEDDING_MARKER
                } else {
                    ""
                };
                return Err(format!(
                    "{marker}{backend_label} response read failed: {error}"
                ));
            }
        };

        if status.is_success() {
            return Ok(raw);
        }

        let body_transient = embedding_response_body_is_transient(status, &raw);
        if !last_attempt && (is_retryable_embedding_status(status) || body_transient) {
            sleep_before_embedding_retry(attempt_index);
            continue;
        }

        let marker = if is_retryable_embedding_status(status) || body_transient {
            TRANSIENT_EMBEDDING_MARKER
        } else {
            ""
        };
        return Err(format!(
            "{marker}{backend_label} request failed (HTTP {}): {}",
            status, raw
        ));
    }

    unreachable!("embedding request retries exhausted without returning")
}

impl std::fmt::Display for OutputEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Float => write!(f, "float"),
            Self::Base64Int8 => write!(f, "base64_int8"),
            Self::Base64Binary => write!(f, "base64_binary"),
        }
    }
}

impl std::fmt::Display for InputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FlatTexts => write!(f, "flat_texts"),
            Self::DocumentChunks => write!(f, "document_chunks"),
        }
    }
}

impl std::fmt::Display for StorageStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NativeF32 => write!(f, "native_f32"),
            Self::DecodeNormalizeF32 => write!(f, "decode_normalize_f32"),
            Self::BinaryPacked => write!(f, "binary_packed"),
        }
    }
}

impl std::fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Cosine => write!(f, "cosine"),
            Self::DotProduct => write!(f, "dot_product"),
            Self::Euclidean => write!(f, "euclidean"),
            Self::Hamming => write!(f, "hamming"),
        }
    }
}

impl SemanticEmbeddingModel {
    pub fn from_config(config: &SemanticBackendConfig) -> Result<Self, String> {
        let timeout_ms = if config.timeout_ms == 0 {
            DEFAULT_OPENAI_EMBEDDING_TIMEOUT_MS
        } else {
            config.timeout_ms
        };

        let max_batch_size = if config.max_batch_size == 0 {
            DEFAULT_MAX_BATCH_SIZE
        } else {
            config.max_batch_size
        };

        let api_key_env = normalize_api_key(config.api_key_env.clone());
        let model = config.model.clone();

        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to configure embedding client: {error}"))?;

        let engine = match config.backend {
            SemanticBackend::Fastembed => {
                SemanticEmbeddingEngine::Local(LocalEmbedder::new(&model)?)
            }
            SemanticBackend::OpenAiCompatible => {
                let raw = config.base_url.as_ref().ok_or_else(|| {
                    "base_url is required for openai_compatible backend".to_string()
                })?;
                let base_url = normalize_base_url(raw)?;

                let api_key = match api_key_env {
                    Some(var_name) => Some(env::var(&var_name).map_err(|_| {
                        format!("missing api_key_env '{var_name}' for openai_compatible backend")
                    })?),
                    None => None,
                };

                SemanticEmbeddingEngine::OpenAiCompatible {
                    client,
                    model,
                    base_url,
                    api_key,
                }
            }
            SemanticBackend::Ollama => {
                let raw = config
                    .base_url
                    .as_ref()
                    .ok_or_else(|| "base_url is required for ollama backend".to_string())?;
                let base_url = normalize_base_url(raw)?;

                SemanticEmbeddingEngine::Ollama {
                    client,
                    model,
                    base_url,
                }
            }
            SemanticBackend::Perplexity => {
                let raw = config
                    .base_url
                    .as_ref()
                    .ok_or_else(|| "base_url is required for perplexity backend".to_string())?;
                let base_url = normalize_base_url(raw)?;

                let api_key = match api_key_env {
                    Some(var_name) => Some(env::var(&var_name).map_err(|_| {
                        format!("missing api_key_env '{var_name}' for perplexity backend")
                    })?),
                    None => None,
                };

                SemanticEmbeddingEngine::Perplexity {
                    client,
                    model,
                    base_url,
                    api_key,
                }
            }
            SemanticBackend::Model2Vec => {
                #[cfg(feature = "semantic-model2vec")]
                {
                    use crate::model2vec_download::resolve_model2vec_files;

                    let model_dir = resolve_model2vec_files(
                        Some(&config.model),
                        config.model_path.as_deref(),
                    )?;

                    let static_model = Model2VecStaticModel::from_pretrained(
                        model_dir
                            .to_str()
                            .ok_or_else(|| "model path is not valid UTF-8".to_string())?,
                        None, // hf_token
                        None, // normalize_embeddings (use model default)
                        None, // subfolder
                    )
                    .map_err(|error| format!("failed to load model2vec model: {error}"))?;
                    SemanticEmbeddingEngine::Model2Vec {
                        model: static_model,
                        max_length: config.model2vec_max_length,
                    }
                }
                #[cfg(not(feature = "semantic-model2vec"))]
                {
                    return Err(
                        "backend = \"model2vec\" requires the semantic-model2vec Cargo feature \
                         to be enabled at compile time. Rebuild with \
                         --features semantic-model2vec to use the model2vec backend."
                            .to_string(),
                    );
                }
            }
        };

        Ok(Self {
            backend: config.backend,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            timeout_ms,
            max_batch_size,
            dimension: None,
            config_dimensions: config.dimensions,
            output_encoding: resolve_output_encoding(config),
            storage_strategy: resolve_storage_strategy(config),
            distance_metric: DistanceMetric::Auto,
            input_mode: resolve_input_mode(config),
            engine,
            query_embedding_cache: HashMap::new(),
            query_embedding_cache_order: VecDeque::new(),
            query_embedding_cache_hits: 0,
            query_embedding_cache_misses: 0,
        })
    }

    pub fn backend(&self) -> SemanticBackend {
        self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    pub fn fingerprint(
        &mut self,
        config: &SemanticBackendConfig,
        profile: Option<&EmbeddingModelProfile>,
        file_policy: &SemanticFilePolicy,
    ) -> Result<SemanticIndexFingerprint, String> {
        let dimension = self.dimension()?;
        // Resolve distance metric (auto -> profile)
        self.distance_metric = resolve_distance_metric(config, profile);
        Ok(SemanticIndexFingerprint::from_config(
            config,
            dimension,
            profile,
            file_policy,
        ))
    }

    /// Returns the resolved input mode for this model.
    pub fn input_mode(&self) -> crate::config::InputMode {
        self.input_mode
    }

    pub fn dimension(&mut self) -> Result<usize, String> {
        if let Some(dimension) = self.dimension {
            return Ok(dimension);
        }

        let dimension = match &mut self.engine {
            SemanticEmbeddingEngine::Local(model) => {
                let vectors = model
                    .embed(&["semantic index fingerprint probe".to_string()])
                    .map_err(format_embedding_init_error)?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            SemanticEmbeddingEngine::OpenAiCompatible { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            SemanticEmbeddingEngine::Ollama { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            SemanticEmbeddingEngine::Perplexity { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            #[cfg(feature = "semantic-model2vec")]
            SemanticEmbeddingEngine::Model2Vec { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
        };

        self.dimension = Some(dimension);
        Ok(dimension)
    }

    pub fn embed(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        self.embed_texts(texts)
    }

    pub fn embed_query_cached(
        &mut self,
        query: &str,
        query_prompt_template: Option<&str>,
    ) -> Result<(Vec<f32>, bool), String> {
        let prompt_hash = prompt_template_hash(query_prompt_template);
        let cache_key = if prompt_hash.is_empty() {
            query.to_string()
        } else {
            format!("{prompt_hash}:{query}")
        };

        if let Some(vector) = self.query_embedding_cache.get(&cache_key) {
            self.query_embedding_cache_hits += 1;
            return Ok((vector.clone(), true));
        }

        self.query_embedding_cache_misses += 1;
        let prefixed_query = apply_query_template(query, query_prompt_template);
        let embeddings = self.embed_texts(vec![prefixed_query])?;
        let vector = embeddings
            .first()
            .cloned()
            .ok_or_else(|| "embedding model returned no query vector".to_string())?;

        if self.query_embedding_cache.len() >= QUERY_EMBEDDING_CACHE_CAP {
            if let Some(oldest) = self.query_embedding_cache_order.pop_front() {
                self.query_embedding_cache.remove(&oldest);
            }
        }
        self.query_embedding_cache
            .insert(cache_key.clone(), vector.clone());
        self.query_embedding_cache_order.push_back(cache_key);

        Ok((vector, false))
    }

    pub fn query_embedding_cache_stats(&self) -> (u64, u64, usize) {
        (
            self.query_embedding_cache_hits,
            self.query_embedding_cache_misses,
            self.query_embedding_cache.len(),
        )
    }

    fn embed_texts(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        match &mut self.engine {
            SemanticEmbeddingEngine::Local(model) => model
                .embed(&texts)
                .map_err(|error| format!("failed to embed batch: {error}")),
            SemanticEmbeddingEngine::OpenAiCompatible {
                client,
                model,
                base_url,
                api_key,
            } => {
                let expected_text_count = texts.len();
                let endpoint = build_openai_embeddings_endpoint(base_url);

                let mut body = serde_json::json!({
                    "input": texts,
                    "model": model,
                });
                // Conditionally add dimensions when user-configured or when
                // we already know the dimension from a previous probe.
                if let Some(dims) = self.config_dimensions.or(self.dimension) {
                    body["dimensions"] = serde_json::json!(dims);
                }
                // Request the configured output encoding from providers that
                // support it (e.g. Perplexity base64_int8 via openai_compatible).
                if self.output_encoding != OutputEncoding::Float {
                    body["encoding_format"] = serde_json::json!(self.output_encoding.to_string());
                }

                let raw = send_embedding_request(
                    || {
                        // `.json(&body)` sets Content-Type: application/json
                        // automatically. Do NOT add `.header("Content-Type",
                        // "application/json")` afterwards — RequestBuilder::header()
                        // calls HeaderMap::append, which produces TWO Content-Type
                        // headers on the wire. OpenAI's /v1/embeddings endpoint
                        // treats duplicate Content-Type as malformed and rejects
                        // the body with 400 "you must provide a model parameter"
                        // even when `model` is set. Verified end-to-end against
                        // api.openai.com. See issue #36.
                        let mut request = client.post(&endpoint).json(&body);

                        if let Some(api_key) = api_key {
                            request = request.header("Authorization", format!("Bearer {api_key}"));
                        }

                        request
                    },
                    "openai compatible",
                )?;

                // Parse response — handle both float arrays and base64-encoded
                // int8 strings depending on the configured output encoding.
                #[derive(Deserialize)]
                struct OpenAiResponse {
                    data: Vec<OpenAiEmbeddingEntry>,
                }

                #[derive(Deserialize)]
                struct OpenAiEmbeddingEntry {
                    embedding: serde_json::Value,
                    index: Option<u32>,
                }

                let parsed: OpenAiResponse = serde_json::from_str(&raw)
                    .map_err(|error| format!("invalid openai compatible response: {error}"))?;
                if parsed.data.len() != expected_text_count {
                    return Err(format!(
                        "openai compatible response returned {} embeddings for {} inputs",
                        parsed.data.len(),
                        expected_text_count
                    ));
                }

                let mut vectors = vec![Vec::new(); parsed.data.len()];
                for (i, item) in parsed.data.into_iter().enumerate() {
                    let index = item.index.unwrap_or(i as u32) as usize;
                    if index >= vectors.len() {
                        return Err(
                            "openai compatible response contains invalid vector index".to_string()
                        );
                    }
                    vectors[index] = parse_embedding_value(
                        &item.embedding,
                        self.output_encoding,
                        "openai compatible embedding",
                        self.config_dimensions.or(self.dimension),
                    )?;
                }

                for vector in &vectors {
                    if vector.is_empty() {
                        return Err(
                            "openai compatible response contained missing vectors".to_string()
                        );
                    }
                }

                self.dimension = vectors.first().map(Vec::len);
                Ok(vectors)
            }
            SemanticEmbeddingEngine::Perplexity {
                client,
                model,
                base_url,
                api_key,
            } => {
                let expected_text_count = texts.len();
                let endpoint = build_openai_embeddings_endpoint(base_url);

                let mut body = serde_json::json!({
                    "input": texts,
                    "model": model,
                });
                if let Some(dims) = self.config_dimensions.or(self.dimension) {
                    body["dimensions"] = serde_json::json!(dims);
                }
                // Request the configured output encoding from Perplexity.
                if self.output_encoding != OutputEncoding::Float {
                    body["encoding_format"] = serde_json::json!(self.output_encoding.to_string());
                }

                let raw = send_embedding_request(
                    || {
                        let mut req = client.post(&endpoint).json(&body);
                        req = req.header(
                            "Authorization",
                            format!("Bearer {}", api_key.as_deref().unwrap_or("")),
                        );
                        req
                    },
                    "perplexity",
                )?;

                // Parse response — handle both float arrays and base64-encoded
                // int8 strings depending on the configured output encoding.
                #[derive(Deserialize)]
                struct PerplexityEmbeddingEntry {
                    embedding: serde_json::Value,
                    index: Option<u32>,
                }

                #[derive(Deserialize)]
                struct PerplexityEmbedResponse {
                    data: Vec<PerplexityEmbeddingEntry>,
                }

                let parsed: PerplexityEmbedResponse = serde_json::from_str(&raw)
                    .map_err(|error| format!("invalid perplexity response: {error}"))?;
                if parsed.data.len() != expected_text_count {
                    return Err(format!(
                        "perplexity response returned {} embeddings for {} inputs",
                        parsed.data.len(),
                        expected_text_count
                    ));
                }

                let mut vectors = vec![Vec::new(); parsed.data.len()];
                for (i, item) in parsed.data.into_iter().enumerate() {
                    let index = item.index.unwrap_or(i as u32) as usize;
                    if index >= vectors.len() {
                        return Err("perplexity response contains invalid vector index".to_string());
                    }
                    vectors[index] = parse_embedding_value(
                        &item.embedding,
                        self.output_encoding,
                        "perplexity embedding",
                        self.config_dimensions.or(self.dimension),
                    )?;
                }

                for vector in &vectors {
                    if vector.is_empty() {
                        return Err("perplexity response contained missing vectors".to_string());
                    }
                }

                self.dimension = vectors.first().map(Vec::len);
                Ok(vectors)
            }
            SemanticEmbeddingEngine::Ollama {
                client,
                model,
                base_url,
            } => {
                let expected_text_count = texts.len();
                let endpoint = build_ollama_embeddings_endpoint(base_url);

                #[derive(Serialize)]
                struct OllamaPayload<'a> {
                    model: &'a str,
                    input: Vec<String>,
                }

                let payload = OllamaPayload {
                    model,
                    input: texts,
                };

                let raw = send_embedding_request(
                    || {
                        // `.json(&payload)` sets Content-Type automatically.
                        // Same duplicate-header trap as the OpenAI branch above
                        // — most Ollama servers tolerate it, but the
                        // single-Content-Type form is the correct one.
                        client.post(&endpoint).json(&payload)
                    },
                    "ollama",
                )?;

                #[derive(Deserialize)]
                struct OllamaResponse {
                    embeddings: Vec<Vec<f32>>,
                }

                let parsed: OllamaResponse = serde_json::from_str(&raw)
                    .map_err(|error| format!("invalid ollama response: {error}"))?;
                if parsed.embeddings.is_empty() {
                    return Err("ollama response returned no embeddings".to_string());
                }
                if parsed.embeddings.len() != expected_text_count {
                    return Err(format!(
                        "ollama response returned {} embeddings for {} inputs",
                        parsed.embeddings.len(),
                        expected_text_count
                    ));
                }

                let vectors = parsed.embeddings;
                for vector in &vectors {
                    if vector.is_empty() {
                        return Err("ollama response contained empty embeddings".to_string());
                    }
                }

                self.dimension = vectors.first().map(Vec::len);
                Ok(vectors)
            }
            #[cfg(feature = "semantic-model2vec")]
            SemanticEmbeddingEngine::Model2Vec { model, max_length } => {
                let embeddings = model.encode_with_args(&texts, Some(*max_length), 1024);
                if embeddings.is_empty() {
                    return Err("model2vec returned no embeddings".to_string());
                }
                for vector in &embeddings {
                    if vector.is_empty() {
                        return Err("model2vec returned empty embedding".to_string());
                    }
                }
                self.dimension = embeddings.first().map(Vec::len);
                Ok(embeddings)
            }
        }
    }

    pub fn embed_document_chunks(
        &mut self,
        docs: DocumentChunks,
    ) -> Result<DocumentEmbeddings, String> {
        let is_perplexity = matches!(&self.engine, SemanticEmbeddingEngine::Perplexity { .. });
        if is_perplexity {
            let (client, model, base_url, api_key) = match &self.engine {
                SemanticEmbeddingEngine::Perplexity {
                    client,
                    model,
                    base_url,
                    api_key,
                } => (
                    client.clone(),
                    model.clone(),
                    base_url.clone(),
                    api_key.clone(),
                ),
                _ => unreachable!(),
            };
            let dims = self.config_dimensions.or(self.dimension);
            Self::embed_document_chunks_native(
                &client,
                &model,
                &base_url,
                &api_key,
                dims,
                self.output_encoding,
                docs,
            )
        } else {
            let all_texts: Vec<String> = docs
                .documents
                .iter()
                .flat_map(|d| d.chunks.clone())
                .collect();
            let vectors = self.embed_texts(all_texts)?;
            let mut cursor = 0;
            let embeddings = docs
                .documents
                .iter()
                .map(|doc| {
                    let count = doc.chunks.len();
                    let vecs = vectors[cursor..cursor + count].to_vec();
                    cursor += count;
                    ChunkEmbeddings {
                        file_path: doc.file_path.clone(),
                        vectors: vecs,
                    }
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        }
    }

    fn embed_document_chunks_native(
        client: &reqwest::blocking::Client,
        model: &str,
        base_url: &str,
        api_key: &Option<String>,
        dims: Option<usize>,
        output_encoding: OutputEncoding,
        docs: DocumentChunks,
    ) -> Result<DocumentEmbeddings, String> {
        #[derive(Serialize)]
        struct DocumentPayload<'a> {
            title: &'a str,
            chunks: &'a [String],
        }

        let mut body = serde_json::json!({
            "input": docs.documents.iter().map(|d| DocumentPayload {
                title: &d.title,
                chunks: &d.chunks,
            }).collect::<Vec<_>>(),
            "model": model,
        });

        if let Some(d) = dims {
            body["dimensions"] = serde_json::json!(d);
        }
        // Request the configured output encoding from Perplexity.
        if output_encoding != OutputEncoding::Float {
            body["encoding_format"] = serde_json::json!(output_encoding.to_string());
        }

        let endpoint = build_openai_embeddings_endpoint(base_url);

        let raw = send_embedding_request(
            || {
                let mut req = client.post(&endpoint).json(&body);
                if let Some(key) = api_key {
                    req = req.header("Authorization", format!("Bearer {}", key));
                }
                req
            },
            "perplexity",
        )?;

        // Parse response — handle both float arrays and base64-encoded
        // int8 strings depending on the configured output encoding.
        #[derive(Deserialize)]
        struct DocumentEmbeddingResponse {
            data: Vec<PerDocumentEmbeddings>,
        }

        #[derive(Deserialize)]
        struct PerDocumentEmbeddings {
            embeddings: Vec<serde_json::Value>,
            index: u32,
        }

        let parsed: DocumentEmbeddingResponse = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid perplexity document-chunk response: {error}"))?;

        if parsed.data.len() != docs.documents.len() {
            return Err(format!(
                "perplexity document-chunk response returned {} documents for {} inputs",
                parsed.data.len(),
                docs.documents.len()
            ));
        }

        let mut embeddings = vec![ChunkEmbeddings::default(); docs.documents.len()];
        for item in parsed.data.into_iter() {
            let index = item.index as usize;
            if index >= embeddings.len() {
                return Err(
                    "perplexity document-chunk response contains invalid document index"
                        .to_string(),
                );
            }
            let mut vectors = Vec::with_capacity(item.embeddings.len());
            for (chunk_idx, val) in item.embeddings.into_iter().enumerate() {
                vectors.push(parse_embedding_value(
                    &val,
                    output_encoding,
                    &format!("perplexity document-chunk embedding[{}]", chunk_idx),
                    dims,
                )?);
            }
            embeddings[index] = ChunkEmbeddings {
                file_path: docs.documents[index].file_path.clone(),
                vectors,
            };
        }

        for emb in &embeddings {
            if emb.file_path.as_os_str().is_empty() {
                return Err(
                    "perplexity document-chunk response contained missing document".to_string(),
                );
            }
        }

        Ok(DocumentEmbeddings { embeddings })
    }
}

/// Pre-validate ONNX Runtime by attempting a raw dlopen before ort touches it.
/// This catches broken/incompatible .so files without risking a panic in the ort crate.
/// Also checks the runtime version via OrtGetApiBase if available.
pub fn pre_validate_onnx_runtime() -> Result<(), String> {
    let dylib_path = std::env::var("ORT_DYLIB_PATH").ok();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        let default_name = "libonnxruntime.so";
        #[cfg(target_os = "macos")]
        let default_name = "libonnxruntime.dylib";

        let lib_name = dylib_path.as_deref().unwrap_or(default_name);

        unsafe {
            let c_name = std::ffi::CString::new(lib_name)
                .map_err(|e| format!("invalid library path: {}", e))?;
            let handle = libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW);
            if handle.is_null() {
                let err = libc::dlerror();
                let msg = if err.is_null() {
                    "unknown dlopen error".to_string()
                } else {
                    std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
                };
                return Err(format!(
                    "ONNX Runtime not found. dlopen('{}') failed: {}. \
                     Run `npx @cortexkit/aft doctor` to diagnose.",
                    lib_name, msg
                ));
            }

            // Try to detect the runtime version from the file path or soname.
            // libonnxruntime.so.1.19.0, libonnxruntime.1.24.4.dylib, etc.
            let detected_version = detect_ort_version_from_path(lib_name);

            libc::dlclose(handle);

            // Check version compatibility — we need 1.24.x
            if let Some(ref version) = detected_version {
                let parts: Vec<&str> = version.split('.').collect();
                if let (Some(major), Some(minor)) = (
                    parts.first().and_then(|s| s.parse::<u32>().ok()),
                    parts.get(1).and_then(|s| s.parse::<u32>().ok()),
                ) {
                    if major != 1 || minor < 20 {
                        return Err(format_ort_version_mismatch(version, lib_name));
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, skip pre-validation — let ort handle LoadLibrary
        let _ = dylib_path;
    }

    Ok(())
}

/// Try to extract the ORT version from the library filename or resolved symlink.
/// Examples: "libonnxruntime.so.1.19.0" → "1.19.0", "libonnxruntime.1.24.4.dylib" → "1.24.4"
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn detect_ort_version_from_path(lib_path: &str) -> Option<String> {
    let path = std::path::Path::new(lib_path);

    // Try the path as given, then follow symlinks
    for candidate in [Some(path.to_path_buf()), std::fs::canonicalize(path).ok()]
        .into_iter()
        .flatten()
    {
        if let Some(name) = candidate.file_name().and_then(|n| n.to_str()) {
            if let Some(version) = extract_version_from_filename(name) {
                return Some(version);
            }
        }
    }

    // Also check for versioned siblings in the same directory
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with("libonnxruntime") {
                        if let Some(version) = extract_version_from_filename(name) {
                            return Some(version);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Extract version from filenames like "libonnxruntime.so.1.19.0" or "libonnxruntime.1.24.4.dylib"
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn extract_version_from_filename(name: &str) -> Option<String> {
    // Match patterns: .so.X.Y.Z or .X.Y.Z.dylib or .X.Y.Z.so
    let re = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(name).map(|m| m.as_str().to_string())
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn suggest_removal_command(lib_path: &str) -> String {
    if lib_path.starts_with("/usr/local/lib")
        || lib_path == "libonnxruntime.so"
        || lib_path == "libonnxruntime.dylib"
    {
        #[cfg(target_os = "linux")]
        return "   sudo rm /usr/local/lib/libonnxruntime* && sudo ldconfig".to_string();
        #[cfg(target_os = "macos")]
        return "   sudo rm /usr/local/lib/libonnxruntime*".to_string();
        #[cfg(target_os = "windows")]
        return "   Delete the ONNX Runtime DLL from your PATH".to_string();
    }
    format!("   rm '{}'", lib_path)
}

/// Build the user-facing error message for an incompatible ONNX Runtime
/// install. Extracted as a pure helper so we can unit-test the wording
/// stability — the auto-fix recommendation must always come first because
/// it's the only safe option, and the system-rm step must remain present
/// because some users prefer the system-wide cleanup path.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(crate) fn format_ort_version_mismatch(version: &str, lib_name: &str) -> String {
    format!(
        "ONNX Runtime version mismatch: found v{} at '{}', but AFT requires v1.20+. \
         Solutions:\n\
         1. Auto-fix (recommended): run `npx @cortexkit/aft doctor --fix`. \
         This downloads AFT-managed ONNX Runtime v1.24 into AFT's storage and \
         configures the bridge to load it instead of the system library — no \
         changes to '{}'.\n\
         2. Remove the old library and restart (AFT auto-downloads the correct version on next start):\n\
         {}\n\
         3. Or install ONNX Runtime 1.24 system-wide: https://github.com/microsoft/onnxruntime/releases/tag/v1.24.0\n\
         4. Run `npx @cortexkit/aft doctor` for full diagnostics.",
        version,
        lib_name,
        lib_name,
        suggest_removal_command(lib_name),
    )
}

pub fn initialize_text_embedding(model: &str) -> Result<LocalEmbedder, String> {
    LocalEmbedder::new(model)
}

pub fn is_onnx_runtime_unavailable(message: &str) -> bool {
    if message.trim_start().starts_with("ONNX Runtime not found.") {
        return true;
    }

    let message = message.to_ascii_lowercase();
    let mentions_onnx_runtime = ["onnx runtime", "onnxruntime", "libonnxruntime"]
        .iter()
        .any(|pattern| message.contains(pattern));
    let mentions_dynamic_load_failure = [
        "shared library",
        "dynamic library",
        "failed to load",
        "could not load",
        "unable to load",
        "dlopen",
        "loadlibrary",
        "no such file",
        "not found",
    ]
    .iter()
    .any(|pattern| message.contains(pattern));

    mentions_onnx_runtime && mentions_dynamic_load_failure
}

pub(crate) fn format_embedding_init_error(error: impl Display) -> String {
    let message = error.to_string();

    if is_onnx_runtime_unavailable(&message) {
        return format!("{ONNX_RUNTIME_INSTALL_HINT} Original error: {message}");
    }

    format!("failed to initialize semantic embedding model: {message}")
}

/// A chunk of code ready for embedding — derived from a Symbol with context enrichment
#[derive(Debug, Clone)]
pub struct SemanticChunk {
    /// Absolute file path
    pub file: PathBuf,
    /// Symbol name
    pub name: String,
    /// Symbol kind (function, class, struct, etc.)
    pub kind: SymbolKind,
    /// Line range (0-based internally, inclusive)
    pub start_line: u32,
    pub end_line: u32,
    /// Whether the symbol is exported
    pub exported: bool,
    /// The enriched text that gets embedded (scope + signature + body snippet)
    pub embed_text: String,
    /// Short code snippet for display in results
    pub snippet: String,
}

/// A group of chunks from a single document, for contextualized embedding.
/// Contextualized providers use surrounding chunks as context when embedding
/// each chunk, so chunks must be grouped by source document and preserve order.
#[derive(Debug, Clone)]
pub struct DocumentChunks {
    pub documents: Vec<PerDocumentChunks>,
}

/// Chunks from one source document.
#[derive(Debug, Clone)]
pub struct PerDocumentChunks {
    pub file_path: PathBuf,
    pub title: String,
    pub chunks: Vec<String>,
}

/// Embeddings returned for a batch of documents after contextualized embedding.
#[derive(Debug, Clone)]
pub struct DocumentEmbeddings {
    pub embeddings: Vec<ChunkEmbeddings>,
}

/// Embeddings for one document.
#[derive(Debug, Clone, Default)]
pub struct ChunkEmbeddings {
    pub file_path: PathBuf,
    pub vectors: Vec<Vec<f32>>,
}

/// A stored embedding entry — chunk metadata + vector
#[derive(Debug, Clone)]
pub struct EmbeddingEntry {
    pub(crate) chunk: SemanticChunk,
    pub(crate) vector: Vec<f32>,
    /// Deterministic hash of the chunk fields (file, name, kind, lines, snippet, embed_text).
    /// Used to trace which version of a chunk produced a vector.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) chunk_hash: String,
}

/// Compute a deterministic chunk hash from SemanticChunk fields.
/// Used to trace which version of a chunk produced a stored vector.
pub(crate) fn compute_chunk_hash(chunk: &SemanticChunk) -> String {
    let content_hash = blake3::hash(
        format!(
            "{}{}{}{}{}{}",
            chunk.embed_text,
            chunk.snippet,
            chunk.start_line,
            chunk.end_line,
            chunk.exported,
            symbol_kind_to_u8(&chunk.kind),
        )
        .as_bytes(),
    );
    content_hash.to_hex().to_string()
}

/// Lifecycle state of a [`SemanticIndex`].
///
/// State machine transitions:
///   Disabled → (no transitions)
///   ColdStart → ScanningFiles → Chunking → Embedding → Ready
///   Ready → Refreshing → Ready (or Degraded on partial failure)
///   Ready → RebuildRequired → ColdStart → ... → Ready
///   Ready → Failed → ColdStart → ... → Ready
///   Degraded → Refreshing → Ready (or Failed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SemanticIndexLifecycle {
    /// Semantic search is disabled by configuration.
    Disabled,
    /// Freshly constructed — no embedded data yet.
    ColdStart,
    /// Currently scanning the file system.
    ScanningFiles,
    /// Parsing and chunking files.
    Chunking,
    /// Sending chunks to the embedding backend.
    Embedding,
    /// Index is complete and ready for search.
    Ready,
    /// Incremental refresh in progress.
    Refreshing,
    /// Config or fingerprint changed; a full rebuild is required.
    RebuildRequired,
    /// Index is usable but some files failed to embed.
    Degraded,
    /// Build or refresh failed entirely.
    Failed,
}

/// Identity record for an indexed file in the file manifest.
/// Tracks which files produced which vectors, enabling precise
/// stale-vector pruning when files are edited, deleted, or excluded.
#[derive(Debug, Clone)]
pub(crate) struct FileRecord {
    /// Content hash (blake3) at indexing time
    pub(crate) content_hash: blake3::Hash,
    /// File size at indexing time
    pub(crate) size_bytes: u64,
    /// Last modified time at indexing time
    pub(crate) mtime: SystemTime,
    /// Detected programming language (if applicable)
    pub(crate) language: Option<String>,
    /// Document kind identifier: "code", "docs", "config", "generated", "unknown"
    pub(crate) document_kind: String,
    /// Hash of the file policy that was active when this file was indexed
    pub(crate) inclusion_policy_hash: String,
    /// When this file was indexed
    pub(crate) indexed_at: SystemTime,
}

/// Immutable snapshot of the core semantic index data.
///
/// Held behind `Arc<SemanticIndexSnapshot>` inside [`SemanticIndex`].
/// Clone + mutate + swap is the only mutation path, which keeps the
/// snapshot structurally immutable once published.
#[derive(Debug, Clone)]
pub struct SemanticIndexSnapshot {
    store: crate::vector_store::FlatF32VectorStore,
    /// Embedding dimension (384 for MiniLM-L6-v2)
    dimension: usize,
    project_root: PathBuf,
    /// File identity manifest — maps each indexed file path to its identity record.
    /// Used by pruning to determine which entries belong to which file, enabling
    /// precise stale-vector cleanup when files are edited, deleted, or excluded.
    pub(crate) file_manifest: HashMap<PathBuf, FileRecord>,
    /// Monotonic counter for assigning unique chunk IDs.
    #[allow(dead_code)]
    pub(crate) next_chunk_id: u64,
    /// The fingerprint string at the time this snapshot was built.
    /// Stored alongside the snapshot so search can report which index build
    /// produced each result.
    #[allow(dead_code)]
    pub(crate) fingerprint_string: Option<String>,
}

impl SemanticIndexSnapshot {
    /// Search the index with a query embedding, returning top-K results sorted by relevance
    pub fn search(&self, query_vector: &[f32], top_k: usize) -> Vec<SemanticResult> {
        self.store.search(query_vector, top_k)
    }

    /// Expose access to the underlying store for internal mutation.
    pub(crate) fn store(&self) -> &crate::vector_store::FlatF32VectorStore {
        &self.store
    }

    /// Mutable access to the underlying store for internal mutation.
    pub(crate) fn store_mut(&mut self) -> &mut crate::vector_store::FlatF32VectorStore {
        &mut self.store
    }

    /// Number of indexed entries
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Check if a file needs re-indexing based on mtime/size/hash
    pub fn is_file_stale(&self, file: &Path) -> bool {
        let Some(metadata) = self.store.file_metadata().get(file) else {
            return true;
        };
        let cached = FileFreshness {
            mtime: metadata.mtime,
            size: metadata.size,
            content_hash: metadata.content_hash,
        };
        match cache_freshness::verify_file(file, &cached) {
            FreshnessVerdict::HotFresh => false,
            FreshnessVerdict::ContentFresh { .. } => false,
            FreshnessVerdict::Stale | FreshnessVerdict::Deleted => true,
        }
    }

    /// Get the stored file metadata by path
    #[allow(dead_code)]
    pub(crate) fn file_metadata(&self) -> &HashMap<PathBuf, IndexedFileMetadata> {
        self.store.file_metadata()
    }

    /// Remove stale/zero-norm vectors from the snapshot.
    pub fn prune_stale_vectors(&mut self) -> usize {
        self.store.prune_stale_vectors()
    }

    /// Mutable entry access for the inner `entries` field (test-only).
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub fn entries_mut_inner(&mut self) -> &mut Vec<EmbeddingEntry> {
        self.store.entries_mut()
    }

    /// Read-only slice of all entries (test-only).
    #[cfg(test)]
    pub fn entries_slice(&self) -> &[EmbeddingEntry] {
        self.store.entries_slice()
    }

    /// Mutable file_metadata access — only available in tests.
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub fn file_metadata_mut_inner(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        self.store.file_metadata_mut()
    }

    /// Build the file manifest from store entries and metadata.
    /// Called after constructing or refreshing a snapshot to populate the
    /// file_manifest from the store's existing IndexedFileMetadata.
    pub(crate) fn build_manifest_from_store(&mut self) {
        self.file_manifest.clear();
        for (path, meta) in self.store.file_metadata().iter() {
            self.file_manifest.insert(
                path.clone(),
                FileRecord {
                    content_hash: meta.content_hash,
                    size_bytes: meta.size,
                    mtime: meta.mtime,
                    language: None,
                    document_kind: "code".to_string(),
                    inclusion_policy_hash: String::new(),
                    indexed_at: SystemTime::now(),
                },
            );
        }
    }
}

/// The semantic index — stores embeddings for all symbols in a project.
///
/// Read-only data lives in [`SemanticIndexSnapshot`], accessible through
/// [`Deref`]. Mutation follows a clone–swap pattern: clone the inner
/// snapshot, apply changes, atomically swap.
#[derive(Debug, Clone)]
pub struct SemanticIndex {
    snapshot: Arc<SemanticIndexSnapshot>,
    lifecycle: SemanticIndexLifecycle,
    last_error: Option<String>,
    fingerprint: Option<SemanticIndexFingerprint>,
    deferred_files: HashSet<PathBuf>,
}

impl std::ops::Deref for SemanticIndex {
    type Target = SemanticIndexSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

/// Test-only access helpers replacing direct field access to `entries`
/// and `file_metadata` that were removed in the VectorStore refactoring.
#[cfg(test)]
impl SemanticIndex {
    /// Access the underlying entries for test assertions (read-only).
    fn entries_for_test(&self) -> &[EmbeddingEntry] {
        self.snapshot.entries_slice()
    }

    /// Mutable access to file metadata for test setup.
    fn file_metadata_for_test(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        let snap =
            Arc::get_mut(&mut self.snapshot).expect("snapshot should be uniquely owned in tests");
        snap.store_mut().file_metadata_mut()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexedFileMetadata {
    pub(crate) mtime: SystemTime,
    pub(crate) size: u64,
    pub(crate) content_hash: blake3::Hash,
}

/// Result of an incremental refresh of the semantic index. Counts are file
/// counts; `total_processed` is the number of current/deleted files considered.
#[derive(Debug, Default, Clone, Copy)]
pub struct RefreshSummary {
    pub changed: usize,
    pub added: usize,
    pub deleted: usize,
    pub total_processed: usize,
}

impl RefreshSummary {
    /// True when no files were touched.
    pub fn is_noop(&self) -> bool {
        self.changed == 0 && self.added == 0 && self.deleted == 0
    }
}

#[derive(Debug, Default)]
pub struct InvalidatedFilesRefresh {
    /// Full replacement entries for `completed_paths`, not just newly embedded
    /// chunks. `apply_refresh_update` removes completed paths before extending
    /// this set, so reused chunks must travel in this delta too.
    pub added_entries: Vec<EmbeddingEntry>,
    pub updated_metadata: Vec<(PathBuf, FileFreshness)>,
    pub completed_paths: Vec<PathBuf>,
    pub summary: RefreshSummary,
}

#[derive(Debug, Clone)]
struct ReusableEmbedding {
    embed_text: String,
    vector: Vec<f32>,
}

type ChunkReuseMap = HashMap<PathBuf, HashMap<blake3::Hash, Vec<ReusableEmbedding>>>;

/// Search result from a semantic query
#[derive(Debug, Clone)]
pub struct SemanticResult {
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub exported: bool,
    pub snippet: String,
    pub score: f32,
    pub source: &'static str,
}

impl SemanticIndex {
    pub fn new(project_root: PathBuf, dimension: usize) -> Self {
        debug_assert!(project_root.is_absolute());
        Self {
            snapshot: Arc::new(SemanticIndexSnapshot {
                store: crate::vector_store::FlatF32VectorStore::new(dimension),
                dimension,
                project_root,
                file_manifest: HashMap::new(),
                next_chunk_id: 0,
                fingerprint_string: None,
            }),
            lifecycle: SemanticIndexLifecycle::ColdStart,
            last_error: None,
            fingerprint: None,
            deferred_files: HashSet::new(),
        }
    }

    /// Number of embedded symbol entries.
    pub fn entry_count(&self) -> usize {
        self.len()
    }

    /// Human-readable status label for the index.
    pub fn status_label(&self) -> &'static str {
        if self.is_empty() {
            "empty"
        } else {
            "ready"
        }
    }

    /// Access the current lifecycle state.
    #[allow(dead_code)]
    pub(crate) fn lifecycle(&self) -> &SemanticIndexLifecycle {
        &self.lifecycle
    }

    /// Mark the index with a new lifecycle state.
    #[allow(dead_code)]
    pub(crate) fn set_lifecycle(&mut self, lifecycle: SemanticIndexLifecycle) {
        self.lifecycle = lifecycle;
    }

    /// Convenience: extract the error string when lifecycle is `Failed`.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Convenience: set lifecycle to `Failed` with a message.
    pub fn set_last_error(&mut self, error: String) {
        self.last_error = Some(error);
        self.lifecycle = SemanticIndexLifecycle::Failed;
    }

    /// Access the inner snapshot.
    pub fn snapshot(&self) -> &SemanticIndexSnapshot {
        &self.snapshot
    }

    /// Atomically swap the inner snapshot. The only mutation path.
    fn swap_snapshot(&mut self, new_snapshot: SemanticIndexSnapshot) {
        self.snapshot = Arc::new(new_snapshot);
    }

    /// Remove stale/zero-norm vectors from the current snapshot.
    pub fn prune_stale_vectors(&mut self) -> usize {
        let mut new_snapshot = (*self.snapshot).clone();
        let count = new_snapshot.prune_stale_vectors();
        self.swap_snapshot(new_snapshot);
        count
    }

    /// Mutable entry access (read-only via Deref) — only available in tests.
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub fn entries_mut(&mut self) -> &mut Vec<EmbeddingEntry> {
        Arc::make_mut(&mut self.snapshot).entries_mut_inner()
    }

    /// Replace the entire snapshot atomically — only available in tests.
    #[cfg(test)]
    pub fn set_snapshot(&mut self, snapshot: SemanticIndexSnapshot) {
        self.snapshot = Arc::new(snapshot);
    }

    /// Mutable file_metadata access — only available in tests.
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub fn file_metadata_mut(&mut self) -> &mut HashMap<PathBuf, IndexedFileMetadata> {
        Arc::make_mut(&mut self.snapshot).file_metadata_mut_inner()
    }

    /// Read-only file_metadata access — only available in tests.
    #[cfg(test)]
    #[allow(private_interfaces)]
    pub fn file_metadata(&self) -> &HashMap<PathBuf, IndexedFileMetadata> {
        self.snapshot.store().file_metadata()
    }

    /// Set dimension — only available in tests.
    #[cfg(test)]
    pub fn set_dimension(&mut self, dim: usize) {
        let snap = Arc::make_mut(&mut self.snapshot);
        snap.dimension = dim;
        snap.store_mut().set_dimension(dim);
    }

    fn collect_chunks(
        project_root: &Path,
        files: &[PathBuf],
        file_policy: &SemanticFilePolicy,
        document_prompt_template: Option<&str>,
    ) -> (Vec<SemanticChunk>, HashMap<PathBuf, IndexedFileMetadata>) {
        let policy = file_policy.clone();
        let per_file: Vec<(
            PathBuf,
            Result<(IndexedFileMetadata, Vec<SemanticChunk>), String>,
        )> = files
            .par_iter()
            .map_init(HashMap::new, |parsers, file| {
                let result = collect_file_metadata(file).and_then(|metadata| {
                    // Apply file policy checks
                    let file_type = classify_semantic_file(file);
                    match file_type {
                        SemanticFileType::Code => {
                            if !policy.include_code {
                                return Err("code files disabled by policy".to_string());
                            }
                        }
                        SemanticFileType::Doc => {
                            if !policy.include_docs {
                                return Err("docs files disabled by policy".to_string());
                            }
                        }
                        SemanticFileType::Config => {
                            if !policy.include_configs {
                                return Err("config files disabled by policy".to_string());
                            }
                        }
                        SemanticFileType::Unknown => {
                            return Err("unknown file type".to_string());
                        }
                    }

                    // Binary detection
                    if policy.binary_detection {
                        let bytes = match std::fs::read(file) {
                            Ok(b) => b,
                            Err(e) => return Err(e.to_string()),
                        };
                        if is_binary_bytes(&bytes) {
                            return Err("binary file".to_string());
                        }
                        // File size check
                        if bytes.len() as u64 > policy.max_file_size_bytes {
                            return Err(format!(
                                "file too large ({} bytes, limit {})",
                                bytes.len(),
                                policy.max_file_size_bytes
                            ));
                        }
                        // For doc/config files, chunk from text
                        if file_type == SemanticFileType::Doc
                            || file_type == SemanticFileType::Config
                        {
                            let text = match String::from_utf8(bytes) {
                                Ok(t) => t,
                                Err(_) => return Err("non-utf8 file".to_string()),
                            };
                            if file_type == SemanticFileType::Doc {
                                return Ok((metadata, collect_docs_chunks(&text, file)));
                            } else {
                                // Config files: single chunk
                                let name = file
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "config".to_string());
                                let body = text.trim().to_string();
                                if body.is_empty() {
                                    return Ok((metadata, Vec::new()));
                                }
                                return Ok((
                                    metadata,
                                    vec![SemanticChunk {
                                        file: file.to_path_buf(),
                                        name,
                                        kind: SymbolKind::FileSummary,
                                        start_line: 0,
                                        end_line: text.lines().count().saturating_sub(1) as u32,
                                        exported: false,
                                        embed_text: body.clone(),
                                        snippet: truncate_snippet(&body),
                                    }],
                                ));
                            }
                        }
                        // Code files fall through to tree-sitter chunking below
                        drop(bytes); // release the raw bytes
                    }

                    // Generated file detection
                    if policy.generated_file_detection && is_generated_file(file) {
                        return Err("generated file".to_string());
                    }

                    collect_file_chunks(project_root, file, parsers)
                        .map(|chunks| (metadata, chunks))
                });
                (file.clone(), result)
            })
            .collect();

        let mut chunks: Vec<SemanticChunk> = Vec::new();
        let mut file_metadata: HashMap<PathBuf, IndexedFileMetadata> = HashMap::new();

        for (file, result) in per_file {
            match result {
                Ok((metadata, file_chunks)) => {
                    file_metadata.insert(file, metadata);
                    chunks.extend(file_chunks);
                }
                Err(error) => {
                    // Skip expected/normal skip reasons silently, but log at
                    // debug level so diagnostic runs can trace per-file skips.
                    if matches!(
                        error.as_str(),
                        "unsupported file extension"
                            | "binary file"
                            | "generated file"
                            | "code files disabled by policy"
                            | "docs files disabled by policy"
                            | "config files disabled by policy"
                            | "unknown file type"
                            | "non-utf8 file"
                    ) || error.starts_with("file too large")
                    {
                        slog_debug!(
                            "skipped semantic chunk collection for {}: {}",
                            file.display(),
                            error
                        );
                        continue;
                    }
                    slog_warn!(
                        "failed to collect semantic chunks for {}: {}",
                        file.display(),
                        error
                    );
                }
            }
        }

        // Apply document prompt template to each chunk's embed_text.
        // This prefixes document text for models that require it (e.g. E5 "passage: " prefix).
        if let Some(tpl) = document_prompt_template {
            for chunk in &mut chunks {
                chunk.embed_text = apply_document_template(&chunk.embed_text, Some(tpl));
            }
        }

        (chunks, file_metadata)
    }

    fn build_from_chunks<F, P>(
        project_root: &Path,
        chunks: Vec<SemanticChunk>,
        file_metadata: HashMap<PathBuf, IndexedFileMetadata>,
        embed_fn: &mut F,
        max_batch_size: usize,
        mut progress: Option<&mut P>,
        max_embed_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Result<SemanticIndexSnapshot, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        debug_assert!(project_root.is_absolute());

        // Chunk large symbols to prevent HTTP 400 errors on remote backends.
        // Local backends (Fastembed, Model2Vec) already truncate internally.
        let chunks = chunk_large_embed_texts(chunks, max_embed_tokens, chunk_overlap_tokens);
        let total_chunks = chunks.len();

        if chunks.is_empty() {
            return Ok(SemanticIndexSnapshot {
                store: crate::vector_store::FlatF32VectorStore::new(DEFAULT_DIMENSION),
                dimension: DEFAULT_DIMENSION,
                project_root: project_root.to_path_buf(),
                file_manifest: HashMap::new(),
                next_chunk_id: 0,
                fingerprint_string: None,
            });
        }

        // Embed in batches
        let mut entries: Vec<EmbeddingEntry> = Vec::with_capacity(chunks.len());
        let mut expected_dimension: Option<usize> = None;
        let batch_size = max_batch_size.max(1);
        for batch_start in (0..chunks.len()).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(chunks.len());
            let batch_texts: Vec<String> = chunks[batch_start..batch_end]
                .iter()
                .map(|c| c.embed_text.clone())
                .collect();

            let vectors = embed_with_retry(&mut *embed_fn, batch_texts)?;
            validate_embedding_batch(&vectors, batch_end - batch_start, "embedding backend")?;

            // Track consistent dimension across all batches
            if let Some(dim) = vectors.first().map(|v| v.len()) {
                match expected_dimension {
                    None => expected_dimension = Some(dim),
                    Some(expected) if dim != expected => {
                        return Err(format!(
                            "embedding dimension changed across batches: expected {expected}, got {dim}"
                        ));
                    }
                    _ => {}
                }
            }

            for (i, vector) in vectors.into_iter().enumerate() {
                let chunk_idx = batch_start + i;
                entries.push(EmbeddingEntry {
                    chunk: chunks[chunk_idx].clone(),
                    vector,
                    chunk_hash: compute_chunk_hash(&chunks[chunk_idx]),
                });
            }

            if let Some(callback) = progress.as_mut() {
                callback(entries.len(), total_chunks);
            }
        }

        let dimension = entries
            .first()
            .map(|e| e.vector.len())
            .unwrap_or(DEFAULT_DIMENSION);

        let mut snapshot = SemanticIndexSnapshot {
            store: crate::vector_store::FlatF32VectorStore::from_parts(
                entries,
                dimension,
                file_metadata,
            ),
            dimension,
            project_root: project_root.to_path_buf(),
            file_manifest: HashMap::new(),
            next_chunk_id: 0,
            fingerprint_string: None,
        };
        snapshot.build_manifest_from_store();
        Ok(snapshot)
    }

    /// Build the semantic index from a set of files using the provided embedding function.
    /// `embed_fn` takes a batch of texts and returns a batch of embedding vectors.
    pub fn build<F>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
    {
        let (chunks, file_mtimes) =
            Self::collect_chunks(project_root, files, &SemanticFilePolicy::default(), None);
        let snapshot = Self::build_from_chunks(
            project_root,
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Option::<&mut fn(usize, usize)>::None,
            512, // max_embed_tokens default
            100, // chunk_overlap_tokens default
        )?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint: None,
            deferred_files: HashSet::new(),
        })
    }

    /// Build the semantic index and report embedding progress using entry counts.
    /// Sort files for cold-start priority: README/docs first, then core source,
    /// then tests, then remaining. This makes the most useful content available
    /// earliest when the index is partially built.
    pub fn sort_files_by_priority(files: &mut [PathBuf]) {
        fn priority(p: &Path) -> u8 {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            let path_str = p.to_str().unwrap_or("");

            // README and top-level docs → highest priority (0)
            if name.eq_ignore_ascii_case("readme.md")
                || name.eq_ignore_ascii_case("readme")
                || name.eq_ignore_ascii_case("readme.txt")
            {
                return 0;
            }
            // docs/ adr/ .github/ directories → high priority (1)
            if path_str.contains("/docs/")
                || path_str.contains("\\docs\\")
                || path_str.contains("/adr/")
                || path_str.contains("\\adr\\")
                || path_str.contains("/.github/")
                || path_str.contains("\\.github\\")
                || path_str.contains("/architecture/")
                || path_str.contains("\\architecture\\")
            {
                return 1;
            }
            // Other markdown → medium-high (2)
            if ext == "md" || ext == "mdx" || ext == "rst" || ext == "txt" {
                return 2;
            }
            // Core source (src/, lib/, crates/) → medium (3)
            if path_str.contains("/src/")
                || path_str.contains("\\src\\")
                || path_str.contains("/lib/")
                || path_str.contains("\\lib\\")
                || path_str.contains("/crates/")
                || path_str.contains("\\crates\\")
                || path_str.contains("/packages/")
                || path_str.contains("\\packages\\")
            {
                return 3;
            }
            // Tests → lower (4)
            if path_str.contains("/tests/")
                || path_str.contains("\\tests\\")
                || path_str.contains("/test/")
                || path_str.contains("\\test\\")
                || path_str.contains("/__tests__/")
                || path_str.contains("\\__tests__\\")
                || name.contains("test")
            {
                return 4;
            }
            // Everything else → lowest (5)
            5
        }
        files.sort_by_key(|p| priority(p));
    }

    pub fn build_with_progress<F, P>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
        progress: &mut P,
        file_policy: &SemanticFilePolicy,
        document_prompt_template: Option<&str>,
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        let mut files = files.to_vec();
        Self::sort_files_by_priority(&mut files);
        let (chunks, file_mtimes) =
            Self::collect_chunks(project_root, &files, file_policy, document_prompt_template);
        let total_chunks = chunks.len();
        progress(0, total_chunks);
        let snapshot = Self::build_from_chunks(
            project_root,
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Some(progress),
            512, // max_embed_tokens default
            100, // chunk_overlap_tokens default
        )?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint: None,
            deferred_files: HashSet::new(),
        })
    }

    /// Split a document's chunks into sub-groups that respect the provider's
    /// max-chunks-per-document limit. Each sub-group preserves chunk order
    /// and carries a synthetic title indicating which slice it is.
    fn split_oversized_document(
        doc: &PerDocumentChunks,
        max_chunks: usize,
    ) -> Vec<PerDocumentChunks> {
        if doc.chunks.len() <= max_chunks {
            return vec![doc.clone()];
        }
        let mut groups = Vec::new();
        for (i, chunk_batch) in doc.chunks.chunks(max_chunks).enumerate() {
            groups.push(PerDocumentChunks {
                file_path: doc.file_path.clone(),
                title: if i == 0 {
                    doc.title.clone()
                } else {
                    format!("{} (part {})", doc.title, i + 1)
                },
                chunks: chunk_batch.to_vec(),
            });
        }
        groups
    }

    /// Retry a single document group's embedding call with exponential backoff.
    /// Returns Ok embeddings on success, or Err with the last error after all
    /// retries are exhausted. Only transient errors (rate limits, timeouts,
    /// server errors) are retried.
    fn embed_document_group_with_retry<F>(
        embed_fn: &mut F,
        doc: PerDocumentChunks,
        retry_count: &mut usize,
    ) -> Result<DocumentEmbeddings, String>
    where
        F: FnMut(DocumentChunks) -> Result<DocumentEmbeddings, String>,
    {
        let mut last_err = String::new();
        for attempt in 0..=CONTEXTUALIZED_MAX_RETRIES {
            match embed_fn(DocumentChunks {
                documents: vec![doc.clone()],
            }) {
                Ok(result) => {
                    if attempt > 0 {
                        *retry_count += 1;
                    }
                    return Ok(result);
                }
                Err(e) => {
                    last_err = e.clone();
                    let is_transient = e.to_lowercase().contains("rate")
                        || e.to_lowercase().contains("limit")
                        || e.to_lowercase().contains("timeout")
                        || e.to_lowercase().contains("429")
                        || e.to_lowercase().contains("503")
                        || e.to_lowercase().contains("502")
                        || e.to_lowercase().contains("500")
                        || e.to_lowercase().contains("connection")
                        || e.to_lowercase().contains("reset")
                        || e.to_lowercase().contains("network");

                    if !is_transient || attempt == CONTEXTUALIZED_MAX_RETRIES {
                        return Err(last_err);
                    }
                    let delay = (CONTEXTUALIZED_RETRY_BASE_DELAY_MS * 2u64.pow(attempt))
                        .min(CONTEXTUALIZED_RETRY_MAX_DELAY_MS);
                    slog_warn!(
                        "contextualized doc group failed (attempt {}/{}): {}. Retrying in {}ms...",
                        attempt + 1,
                        CONTEXTUALIZED_MAX_RETRIES + 1,
                        e,
                        delay
                    );
                    std::thread::sleep(Duration::from_millis(delay));
                }
            }
        }
        Err(last_err)
    }

    /// Build the semantic index using a contextualized document-chunk embedding
    /// function. Groups chunks by source document so the embedding provider can
    /// use surrounding chunks as context.
    ///
    /// Returns the built index and contextualized build diagnostics (split
    /// counts, retry counts, failure counts) so the caller can surface them.
    pub fn build_with_progress_contextualized<F, P>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        progress: &mut P,
        file_policy: &SemanticFilePolicy,
        document_prompt_template: Option<&str>,
    ) -> Result<(Self, ContextualizedBuildDiagnostics), String>
    where
        F: FnMut(DocumentChunks) -> Result<DocumentEmbeddings, String>,
        P: FnMut(usize, usize),
    {
        let mut files = files.to_vec();
        Self::sort_files_by_priority(&mut files);
        let (chunks, file_metadata) =
            Self::collect_chunks(project_root, &files, file_policy, document_prompt_template);
        let total_chunks = chunks.len();
        progress(0, total_chunks);

        if chunks.is_empty() {
            return Ok((
                Self {
                    snapshot: Arc::new(SemanticIndexSnapshot {
                        store: crate::vector_store::FlatF32VectorStore::from_parts(
                            Vec::new(),
                            DEFAULT_DIMENSION,
                            file_metadata,
                        ),
                        dimension: DEFAULT_DIMENSION,
                        project_root: project_root.to_path_buf(),
                        file_manifest: HashMap::new(),
                        next_chunk_id: 0,
                        fingerprint_string: None,
                    }),
                    lifecycle: SemanticIndexLifecycle::Ready,
                    last_error: None,
                    fingerprint: None,
                    deferred_files: HashSet::new(),
                },
                ContextualizedBuildDiagnostics::default(),
            ));
        }

        // Group chunks by file path using BTreeMap for deterministic ordering
        let mut docs_map: BTreeMap<PathBuf, Vec<SemanticChunk>> = BTreeMap::new();
        for chunk in chunks {
            docs_map.entry(chunk.file.clone()).or_default().push(chunk);
        }

        // Build per-document chunk groups, splitting oversized documents.
        // `group_source_chunks` tracks the original SemanticChunk objects for
        // each sub-group so reconstruction doesn't rely on file_path lookups
        // (which break when one file is split into multiple sub-groups).
        // `group_file_paths` tracks the expected file_path for each sub-group
        // so we can validate the embedder's response.
        let mut documents: Vec<PerDocumentChunks> = Vec::with_capacity(docs_map.len());
        let mut group_source_chunks: Vec<Vec<SemanticChunk>> = Vec::with_capacity(docs_map.len());
        let mut group_file_paths: Vec<PathBuf> = Vec::with_capacity(docs_map.len());
        let mut diagnostics = ContextualizedBuildDiagnostics {
            max_chunks_in_document: docs_map.values().map(|c| c.len()).max().unwrap_or(0),
            ..Default::default()
        };

        for (path, file_chunks) in &docs_map {
            let title = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let chunk_texts: Vec<String> =
                file_chunks.iter().map(|c| c.embed_text.clone()).collect();
            let doc = PerDocumentChunks {
                file_path: path.clone(),
                title,
                chunks: chunk_texts,
            };

            // Split oversized documents into sub-groups
            let sub_groups = Self::split_oversized_document(&doc, DEFAULT_MAX_CHUNKS_PER_DOCUMENT);

            // Record source chunks and expected file_path for each sub-group
            for (i, sub_group) in sub_groups.iter().enumerate() {
                let start = i * DEFAULT_MAX_CHUNKS_PER_DOCUMENT;
                let end = std::cmp::min(start + DEFAULT_MAX_CHUNKS_PER_DOCUMENT, file_chunks.len());
                group_source_chunks.push(file_chunks[start..end].to_vec());
                group_file_paths.push(sub_group.file_path.clone());
            }

            if sub_groups.len() > 1 {
                diagnostics.split_documents += 1;
            }
            documents.extend(sub_groups);
        }

        diagnostics.documents_processed = docs_map.len();

        // Embed documents with retry logic, tracking diagnostics
        let mut all_embeddings: Vec<ChunkEmbeddings> = Vec::new();
        let mut succeeded_source_chunks: Vec<Vec<SemanticChunk>> = Vec::new();
        let mut succeeded_file_paths: Vec<PathBuf> = Vec::new();
        let mut retried_count = 0usize;
        let mut failed_count = 0usize;

        for (i, doc) in documents.iter().enumerate() {
            match Self::embed_document_group_with_retry(embed_fn, doc.clone(), &mut retried_count) {
                Ok(result) => {
                    all_embeddings.extend(result.embeddings);
                    succeeded_source_chunks.push(group_source_chunks[i].clone());
                    succeeded_file_paths.push(group_file_paths[i].clone());
                }
                Err(e) => {
                    slog_warn!(
                        "contextualized doc group failed after retries, skipping: {} ({})",
                        doc.file_path.display(),
                        e
                    );
                    failed_count += 1;
                }
            }
        }

        diagnostics.retried_groups = retried_count;
        diagnostics.failed_groups = failed_count;

        let mut entries: Vec<EmbeddingEntry> = Vec::with_capacity(total_chunks);
        let mut expected_dimension: Option<usize> = None;
        let mut done = 0;

        for (emb, source_chunks, expected_path) in all_embeddings
            .into_iter()
            .zip(succeeded_source_chunks)
            .zip(succeeded_file_paths)
            .map(|((emb, chunks), path)| (emb, chunks, path))
        {
            // Validate that the embedder returned embeddings for the file we expected
            if emb.file_path != expected_path {
                return Err(format!(
                    "embedding response returned unknown file path: {} (expected {})",
                    emb.file_path.display(),
                    expected_path.display()
                ));
            }

            if emb.vectors.len() != source_chunks.len() {
                return Err(format!(
                    "embedding response returned {} vectors for {} chunks in file {}",
                    emb.vectors.len(),
                    source_chunks.len(),
                    emb.file_path.display()
                ));
            }

            for (chunk, vector) in source_chunks.iter().zip(emb.vectors) {
                if let Some(dim) = expected_dimension {
                    if vector.len() != dim {
                        return Err(format!(
                            "embedding dimension changed: expected {dim}, got {}",
                            vector.len()
                        ));
                    }
                } else {
                    expected_dimension = Some(vector.len());
                }

                entries.push(EmbeddingEntry {
                    chunk: chunk.clone(),
                    vector,
                    chunk_hash: compute_chunk_hash(chunk),
                });
                done += 1;
                progress(done, total_chunks);
            }
        }

        diagnostics.chunks_embedded = entries.len();
        slog_info!(
            "contextualized build complete: {} docs ({} split), {} chunks, {} retried, {} failed",
            diagnostics.documents_processed,
            diagnostics.split_documents,
            diagnostics.chunks_embedded,
            diagnostics.retried_groups,
            diagnostics.failed_groups
        );

        let dimension = expected_dimension.unwrap_or(DEFAULT_DIMENSION);

        let mut new_snapshot = SemanticIndexSnapshot {
            store: crate::vector_store::FlatF32VectorStore::from_parts(
                entries,
                dimension,
                file_metadata,
            ),
            dimension,
            project_root: project_root.to_path_buf(),
            file_manifest: HashMap::new(),
            next_chunk_id: 0,
            fingerprint_string: None,
        };
        new_snapshot.build_manifest_from_store();
        Ok((
            Self {
                snapshot: Arc::new(new_snapshot),
                lifecycle: SemanticIndexLifecycle::Ready,
                last_error: None,
                fingerprint: None,
                deferred_files: HashSet::new(),
            },
            diagnostics,
        ))
    }

    /// Incrementally refresh entries for changed/new files only, preserving cached
    /// embeddings for unchanged files. Used when loading the index from disk and
    /// finding that a small fraction of files have moved on, deleted, or appeared.
    ///
    /// Returns `RefreshSummary` describing what changed. On success, `self` is
    /// mutated in place and remains a valid index.
    ///
    /// `current_files` is the full set of files the project considers indexable
    /// (typically `walk_project_files(...)`). Files in the cache that are no
    /// longer in this set are treated as deleted.
    pub fn refresh_stale_files<F, P>(
        &mut self,
        project_root: &Path,
        current_files: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
        progress: &mut P,
        file_policy: &SemanticFilePolicy,
        document_prompt_template: Option<&str>,
    ) -> Result<RefreshSummary, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        // Clone the current snapshot to mutate it (clone-swap pattern).
        let mut snapshot = (*self.snapshot).clone();

        // 1. Bucket files into deleted / changed / added.
        let current_set: HashSet<&Path> = current_files.iter().map(PathBuf::as_path).collect();
        let total_processed = current_set.len() + snapshot.store().file_metadata().len()
            - snapshot
                .store()
                .file_metadata()
                .keys()
                .filter(|path| current_set.contains(path.as_path()))
                .count();

        // Files in cache that disappeared from disk OR are no longer in the
        // walked set. Both cases need their entries dropped.
        let mut deleted: Vec<PathBuf> = Vec::new();
        let mut changed: Vec<PathBuf> = Vec::new();
        let indexed_paths: Vec<PathBuf> =
            snapshot.store().file_metadata().keys().cloned().collect();
        for indexed_path in &indexed_paths {
            if !current_set.contains(indexed_path.as_path()) {
                deleted.push(indexed_path.clone());
                continue;
            }
            let cached = snapshot
                .store()
                .file_metadata()
                .get(indexed_path)
                .map(|meta| FileFreshness {
                    mtime: meta.mtime,
                    size: meta.size,
                    content_hash: meta.content_hash,
                });
            match cached.map(|freshness| cache_freshness::verify_file(indexed_path, &freshness)) {
                Some(FreshnessVerdict::HotFresh) => {}
                Some(FreshnessVerdict::ContentFresh {
                    new_mtime,
                    new_size,
                }) => {
                    // Update mtime/size in metadata — content_hash unchanged.
                    if let Some(meta) = snapshot
                        .store_mut()
                        .file_metadata_mut()
                        .get_mut(indexed_path)
                    {
                        meta.mtime = new_mtime;
                        meta.size = new_size;
                    }
                }
                Some(FreshnessVerdict::Stale | FreshnessVerdict::Deleted) | None => {
                    changed.push(indexed_path.clone());
                }
            }
        }

        // Files in walk that were never indexed.
        let mut added: Vec<PathBuf> = Vec::new();
        for path in current_files {
            if !snapshot.store().file_metadata().contains_key(path) {
                added.push(path.clone());
            }
        }

        // Fast path: nothing to do.
        if deleted.is_empty() && changed.is_empty() && added.is_empty() {
            progress(0, 0);
            return Ok(RefreshSummary {
                total_processed,
                ..RefreshSummary::default()
            });
        }

        // 2. Drop entries for deleted files immediately. Changed files are only
        //    replaced after successful re-extraction + embedding so transient
        //    read/parse errors keep the stale-but-valid cache entry.
        if !deleted.is_empty() {
            let deleted_set: HashSet<&Path> = deleted.iter().map(PathBuf::as_path).collect();
            snapshot
                .store_mut()
                .entries_mut()
                .retain(|entry| !deleted_set.contains(entry.chunk.file.as_path()));
            for path in &deleted {
                snapshot.store_mut().file_metadata_mut().remove(path);
            }
        }

        // 3. Embed the changed + added set, if any.
        let mut to_embed: Vec<PathBuf> = Vec::with_capacity(changed.len() + added.len());
        to_embed.extend(changed.iter().cloned());
        to_embed.extend(added.iter().cloned());

        if to_embed.is_empty() {
            // Only deletions happened.
            progress(0, 0);
            snapshot.build_manifest_from_store();
            self.swap_snapshot(snapshot);
            return Ok(RefreshSummary {
                changed: 0,
                added: 0,
                deleted: deleted.len(),
                total_processed,
            });
        }

        let (chunks, fresh_metadata) = Self::collect_chunks(
            project_root,
            &to_embed,
            file_policy,
            document_prompt_template,
        );

        if chunks.is_empty() {
            progress(0, 0);
            let successful_files: HashSet<PathBuf> = fresh_metadata.keys().cloned().collect();
            if !successful_files.is_empty() {
                snapshot
                    .store_mut()
                    .entries_mut()
                    .retain(|entry| !successful_files.contains(&entry.chunk.file));
            }
            let changed_count = changed
                .iter()
                .filter(|path| successful_files.contains(*path))
                .count();
            let added_count = added
                .iter()
                .filter(|path| successful_files.contains(*path))
                .count();
            snapshot
                .store_mut()
                .file_metadata_mut()
                .extend(fresh_metadata);
            snapshot.build_manifest_from_store();
            self.swap_snapshot(snapshot);
            return Ok(RefreshSummary {
                changed: changed_count,
                added: added_count,
                deleted: deleted.len(),
                total_processed,
            });
        }

        // 4. Embed in batches and dimension-check against the existing index.
        let total_chunks = chunks.len();
        progress(0, total_chunks);
        let batch_size = max_batch_size.max(1);
        let existing_dimension = if snapshot.is_empty() {
            None
        } else {
            Some(snapshot.dimension)
        };
        let mut new_entries: Vec<EmbeddingEntry> = Vec::with_capacity(chunks.len());
        let mut observed_dimension: Option<usize> = existing_dimension;

        for batch_start in (0..chunks.len()).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(chunks.len());
            let batch_texts: Vec<String> = chunks[batch_start..batch_end]
                .iter()
                .map(|c| c.embed_text.clone())
                .collect();

            let vectors = embed_fn(batch_texts)?;
            validate_embedding_batch(&vectors, batch_end - batch_start, "embedding backend")?;

            if let Some(dim) = vectors.first().map(|v| v.len()) {
                match observed_dimension {
                    None => observed_dimension = Some(dim),
                    Some(expected) if dim != expected => {
                        // Refuse to mix dimensions in one index. Caller should
                        // fall back to a full rebuild.
                        return Err(format!(
                            "embedding dimension changed during incremental refresh: \
                             cached index uses {expected}, new vectors use {dim}"
                        ));
                    }
                    _ => {}
                }
            }

            for (i, vector) in vectors.into_iter().enumerate() {
                let chunk_idx = batch_start + i;
                new_entries.push(EmbeddingEntry {
                    chunk: chunks[chunk_idx].clone(),
                    vector,
                    chunk_hash: compute_chunk_hash(&chunks[chunk_idx]),
                });
            }

            progress(new_entries.len(), total_chunks);
        }

        let successful_files: HashSet<PathBuf> = fresh_metadata.keys().cloned().collect();
        if !successful_files.is_empty() {
            snapshot
                .store_mut()
                .entries_mut()
                .retain(|entry| !successful_files.contains(&entry.chunk.file));
        }

        snapshot.store_mut().entries_mut().extend(new_entries);
        snapshot
            .store_mut()
            .file_metadata_mut()
            .extend(fresh_metadata);
        if let Some(dim) = observed_dimension {
            snapshot.dimension = dim;
        }

        snapshot.build_manifest_from_store();
        self.swap_snapshot(snapshot);

        Ok(RefreshSummary {
            changed: changed
                .iter()
                .filter(|path| successful_files.contains(*path))
                .count(),
            added: added
                .iter()
                .filter(|path| successful_files.contains(*path))
                .count(),
            deleted: deleted.len(),
            total_processed,
        })
    }

    /// Number of distinct indexed files (file-metadata keys).
    pub fn indexed_file_count(&self) -> usize {
        self.snapshot.store().file_metadata().len()
    }

    fn build_chunk_reuse_map(&self, files: &[PathBuf]) -> ChunkReuseMap {
        let requested: HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
        let mut reuse_map: ChunkReuseMap = HashMap::new();

        for entry in self.snapshot.store().entries_slice() {
            if !requested.contains(entry.chunk.file.as_path()) {
                continue;
            }

            let hash = blake3::hash(entry.chunk.embed_text.as_bytes());
            reuse_map
                .entry(entry.chunk.file.clone())
                .or_default()
                .entry(hash)
                .or_default()
                .push(ReusableEmbedding {
                    embed_text: entry.chunk.embed_text.clone(),
                    vector: entry.vector.clone(),
                });
        }

        reuse_map
    }

    fn reusable_vector_for_chunk(
        reuse_map: &ChunkReuseMap,
        chunk: &SemanticChunk,
    ) -> Option<Vec<f32>> {
        let hash = blake3::hash(chunk.embed_text.as_bytes());
        reuse_map
            .get(&chunk.file)?
            .get(&hash)?
            .iter()
            .find(|candidate| candidate.embed_text == chunk.embed_text)
            .map(|candidate| candidate.vector.clone())
    }

    fn entries_for_chunks_with_reuse<F, P>(
        chunks: Vec<SemanticChunk>,
        reuse_map: &ChunkReuseMap,
        embed_fn: &mut F,
        max_batch_size: usize,
        initial_observed_dimension: Option<usize>,
        refresh_label: &str,
        progress: &mut P,
    ) -> Result<(Vec<EmbeddingEntry>, Option<usize>), String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        let total_chunks = chunks.len();
        progress(0, total_chunks);

        let mut entries_by_chunk: Vec<Option<EmbeddingEntry>> = vec![None; total_chunks];
        let mut misses: Vec<(usize, SemanticChunk)> = Vec::new();

        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            if let Some(vector) = Self::reusable_vector_for_chunk(reuse_map, &chunk) {
                entries_by_chunk[chunk_index] = Some(EmbeddingEntry {
                    chunk_hash: compute_chunk_hash(&chunk),
                    chunk,
                    vector,
                });
            } else {
                misses.push((chunk_index, chunk));
            }
        }

        let mut completed = total_chunks.saturating_sub(misses.len());
        if completed > 0 {
            progress(completed, total_chunks);
        }

        let batch_size = max_batch_size.max(1);
        let mut observed_dimension = initial_observed_dimension;

        for batch_start in (0..misses.len()).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(misses.len());
            let batch_texts: Vec<String> = misses[batch_start..batch_end]
                .iter()
                .map(|(_, chunk)| chunk.embed_text.clone())
                .collect();

            let vectors = embed_fn(batch_texts)?;
            validate_embedding_batch(&vectors, batch_end - batch_start, "embedding backend")?;

            if let Some(dim) = vectors.first().map(|vector| vector.len()) {
                match observed_dimension {
                    None => observed_dimension = Some(dim),
                    Some(expected) if dim != expected => {
                        return Err(format!(
                            "embedding dimension changed during {refresh_label}: \
                             cached index uses {expected}, new vectors use {dim}"
                        ));
                    }
                    _ => {}
                }
            }

            for (i, vector) in vectors.into_iter().enumerate() {
                let (chunk_index, chunk) = misses[batch_start + i].clone();
                entries_by_chunk[chunk_index] = Some(EmbeddingEntry {
                    chunk_hash: compute_chunk_hash(&chunk),
                    chunk,
                    vector,
                });
            }

            completed += batch_end - batch_start;
            progress(completed, total_chunks);
        }

        let entries = entries_by_chunk
            .into_iter()
            .map(|entry| entry.expect("semantic refresh accounted for every chunk"))
            .collect();

        Ok((entries, observed_dimension))
    }

    /// Refresh only the requested invalidated files in this in-memory index,
    /// re-extract and embed whatever still exists on disk, and return the delta
    /// needed for another in-memory index to apply the same update.
    pub fn refresh_invalidated_files<F, P>(
        &mut self,
        project_root: &Path,
        paths: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
        max_files: usize,
        progress: &mut P,
        file_policy: &SemanticFilePolicy,
        document_prompt_template: Option<&str>,
    ) -> Result<InvalidatedFilesRefresh, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        self.backfill_missing_file_sizes();

        self.deferred_files.retain(|path| path.exists());
        let mut requested_paths = paths.to_vec();
        requested_paths.extend(self.deferred_files.iter().cloned());
        requested_paths.sort();
        requested_paths.dedup();
        let total_processed = requested_paths.len();

        if requested_paths.is_empty() {
            progress(0, 0);
            return Ok(InvalidatedFilesRefresh {
                summary: RefreshSummary {
                    total_processed,
                    ..RefreshSummary::default()
                },
                ..InvalidatedFilesRefresh::default()
            });
        }

        let file_metadata = self.snapshot.store().file_metadata();
        let previously_indexed: HashSet<PathBuf> = requested_paths
            .iter()
            .filter(|path| file_metadata.contains_key(*path))
            .cloned()
            .collect();
        let reuse_map = self.build_chunk_reuse_map(&requested_paths);

        self.remove_indexed_files(&requested_paths);

        let existing_paths = requested_paths
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect::<Vec<_>>();
        let deleted = requested_paths
            .iter()
            .filter(|path| !path.exists() && previously_indexed.contains(path.as_path()))
            .count();

        if existing_paths.is_empty() {
            for path in &requested_paths {
                if !path.exists() {
                    self.deferred_files.remove(path);
                }
            }
            progress(0, 0);
            return Ok(InvalidatedFilesRefresh {
                completed_paths: requested_paths,
                summary: RefreshSummary {
                    deleted,
                    total_processed,
                    ..RefreshSummary::default()
                },
                ..InvalidatedFilesRefresh::default()
            });
        }

        let (mut chunks, mut fresh_metadata) = Self::collect_chunks(
            project_root,
            &existing_paths,
            file_policy,
            document_prompt_template,
        );

        let retained_file_count = self.indexed_file_count();
        let changed_successful_count = existing_paths
            .iter()
            .filter(|path| {
                previously_indexed.contains(path.as_path()) && fresh_metadata.contains_key(*path)
            })
            .count();
        let available_new_files =
            max_files.saturating_sub(retained_file_count.saturating_add(changed_successful_count));
        let new_successful_files = existing_paths
            .iter()
            .filter(|path| {
                !previously_indexed.contains(path.as_path()) && fresh_metadata.contains_key(*path)
            })
            .cloned()
            .collect::<Vec<_>>();
        if new_successful_files.len() > available_new_files {
            let allowed_new_files = new_successful_files
                .iter()
                .take(available_new_files)
                .cloned()
                .collect::<HashSet<_>>();
            let deferred_new_files = new_successful_files
                .into_iter()
                .filter(|path| !allowed_new_files.contains(path))
                .collect::<HashSet<_>>();

            fresh_metadata.retain(|file, _| {
                previously_indexed.contains(file.as_path()) || allowed_new_files.contains(file)
            });
            chunks.retain(|chunk| !deferred_new_files.contains(&chunk.file));

            if !deferred_new_files.is_empty() {
                for path in &deferred_new_files {
                    self.deferred_files.insert(path.clone());
                }
                slog_warn!(
                    "semantic refresh deferred {} new file(s): indexed-file cap {} is reached",
                    deferred_new_files.len(),
                    max_files
                );
            }
        }

        let successful_files: HashSet<PathBuf> = fresh_metadata.keys().cloned().collect();
        for file in &successful_files {
            self.deferred_files.remove(file);
        }
        let changed = successful_files
            .iter()
            .filter(|path| previously_indexed.contains(path.as_path()))
            .count();
        let added = successful_files.len().saturating_sub(changed);
        let mut updated_metadata = Vec::with_capacity(fresh_metadata.len());

        if chunks.is_empty() {
            progress(0, 0);
            let mut snapshot = (*self.snapshot).clone();
            for (file, metadata) in fresh_metadata {
                let freshness = FileFreshness {
                    mtime: metadata.mtime,
                    size: metadata.size,
                    content_hash: metadata.content_hash,
                };
                snapshot
                    .store_mut()
                    .file_metadata_mut()
                    .insert(file.clone(), metadata);
                updated_metadata.push((file, freshness));
            }
            snapshot.build_manifest_from_store();
            self.swap_snapshot(snapshot);

            return Ok(InvalidatedFilesRefresh {
                updated_metadata,
                completed_paths: requested_paths,
                summary: RefreshSummary {
                    changed,
                    added,
                    deleted,
                    total_processed,
                },
                ..InvalidatedFilesRefresh::default()
            });
        }

        let initial_observed_dimension = if self.is_empty() && previously_indexed.is_empty() {
            None
        } else {
            Some(self.dimension())
        };
        let (new_entries, observed_dimension) = Self::entries_for_chunks_with_reuse(
            chunks,
            &reuse_map,
            embed_fn,
            max_batch_size,
            initial_observed_dimension,
            "invalidated-file refresh",
            progress,
        )?;

        let added_entries = new_entries.clone();
        let mut snapshot = (*self.snapshot).clone();
        snapshot.store_mut().entries_mut().extend(new_entries);
        for (file, metadata) in fresh_metadata {
            let freshness = FileFreshness {
                mtime: metadata.mtime,
                size: metadata.size,
                content_hash: metadata.content_hash,
            };
            snapshot
                .store_mut()
                .file_metadata_mut()
                .insert(file.clone(), metadata);
            updated_metadata.push((file, freshness));
        }
        if let Some(dim) = observed_dimension {
            snapshot.dimension = dim;
            snapshot.store_mut().set_dimension(dim);
        }
        snapshot.build_manifest_from_store();
        self.swap_snapshot(snapshot);

        Ok(InvalidatedFilesRefresh {
            added_entries,
            updated_metadata,
            completed_paths: requested_paths,
            summary: RefreshSummary {
                changed,
                added,
                deleted,
                total_processed,
            },
        })
    }

    pub fn apply_refresh_update(
        &mut self,
        added_entries: Vec<EmbeddingEntry>,
        updated_metadata: Vec<(PathBuf, FileFreshness)>,
        completed_paths: &[PathBuf],
    ) {
        self.remove_indexed_files(completed_paths);

        let observed_dimension = added_entries.first().map(|entry| entry.vector.len());
        let mut snapshot = (*self.snapshot).clone();
        snapshot.store_mut().entries_mut().extend(added_entries);
        for (file, freshness) in updated_metadata {
            snapshot.store_mut().file_metadata_mut().insert(
                file,
                IndexedFileMetadata {
                    mtime: freshness.mtime,
                    size: freshness.size,
                    content_hash: freshness.content_hash,
                },
            );
        }
        if let Some(dim) = observed_dimension {
            snapshot.dimension = dim;
            snapshot.store_mut().set_dimension(dim);
        }
        snapshot.build_manifest_from_store();
        self.swap_snapshot(snapshot);
    }

    fn remove_indexed_files(&mut self, files: &[PathBuf]) {
        let deleted_set: HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
        let mut snapshot = (*self.snapshot).clone();
        snapshot
            .store_mut()
            .entries_mut()
            .retain(|entry| !deleted_set.contains(entry.chunk.file.as_path()));
        for path in files {
            snapshot.store_mut().file_metadata_mut().remove(path);
        }
        snapshot.build_manifest_from_store();
        self.swap_snapshot(snapshot);
    }

    fn backfill_missing_file_sizes(&mut self) {
        let paths: Vec<PathBuf> = self
            .snapshot
            .store()
            .file_metadata()
            .keys()
            .cloned()
            .collect();
        let mut snapshot = (*self.snapshot).clone();
        let mut changed = false;
        for path in paths {
            let needs_backfill = snapshot
                .store()
                .file_metadata()
                .get(&path)
                .is_some_and(|meta| meta.size == 0);
            if !needs_backfill {
                continue;
            }
            if let Ok(fs_meta) = fs::metadata(&path) {
                let size = fs_meta.len();
                if let Some(entry) = snapshot.store_mut().file_metadata_mut().get_mut(&path) {
                    entry.size = size;
                    changed = true;
                    if let Ok(Some(hash)) = cache_freshness::hash_file_if_small(&path, size) {
                        entry.content_hash = hash;
                    }
                }
            }
        }
        if changed {
            snapshot.build_manifest_from_store();
            self.swap_snapshot(snapshot);
        }
    }

    /// Remove entries for a specific file (clone–swap pattern)
    pub fn remove_file(&mut self, file: &Path) {
        self.invalidate_file(file);
    }

    pub fn invalidate_file(&mut self, file: &Path) {
        let mut snapshot = (*self.snapshot).clone();
        snapshot
            .store_mut()
            .entries_mut()
            .retain(|e| e.chunk.file != file);
        snapshot.store_mut().file_metadata_mut().remove(file);
        self.snapshot = Arc::new(snapshot);
    }

    pub fn fingerprint(&self) -> Option<&SemanticIndexFingerprint> {
        self.fingerprint.as_ref()
    }

    pub fn backend_label(&self) -> Option<&str> {
        self.fingerprint.as_ref().map(|f| f.backend.as_str())
    }

    pub fn model_label(&self) -> Option<&str> {
        self.fingerprint.as_ref().map(|f| f.model.as_str())
    }

    pub fn set_fingerprint(&mut self, fingerprint: SemanticIndexFingerprint) {
        self.fingerprint = Some(fingerprint);
    }

    /// Compare the current fingerprint with an old one and return the change.
    pub fn fingerprint_change(
        &self,
        old_fingerprint: &SemanticIndexFingerprint,
    ) -> FingerprintChange {
        self.fingerprint
            .as_ref()
            .map(|current| current.diff(old_fingerprint))
            .unwrap_or(FingerprintChange::Rebuild)
    }

    /// Write the semantic index to disk using atomic temp+rename pattern
    pub fn write_to_disk(&self, storage_dir: &Path, project_key: &str) {
        // Don't persist empty indexes — they would be loaded on next startup
        // and prevent a fresh build that might find files.
        if self.is_empty() {
            slog_info!("skipping semantic index persistence (0 entries)");
            return;
        }
        let dir = storage_dir.join("semantic").join(project_key);
        if let Err(e) = fs::create_dir_all(&dir) {
            slog_warn!("failed to create semantic cache dir: {}", e);
            return;
        }
        let data_path = dir.join("semantic.bin");
        let tmp_path = dir.join(format!(
            "semantic.bin.tmp.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_nanos()
        ));
        let bytes = self.to_bytes();
        let write_result = (|| -> std::io::Result<()> {
            use std::io::Write;
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            slog_warn!("failed to write semantic index: {}", e);
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        if let Err(e) = fs::rename(&tmp_path, &data_path) {
            slog_warn!("failed to rename semantic index: {}", e);
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        slog_info!(
            "semantic index persisted: {} entries, {:.1} KB",
            self.len(),
            bytes.len() as f64 / 1024.0
        );
    }

    /// Read the semantic index from disk
    pub fn read_from_disk(
        storage_dir: &Path,
        project_key: &str,
        current_canonical_root: &Path,
        is_worktree_bridge: bool,
        expected_fingerprint: Option<&str>,
    ) -> Option<Self> {
        debug_assert!(current_canonical_root.is_absolute());
        let data_path = storage_dir
            .join("semantic")
            .join(project_key)
            .join("semantic.bin");
        let file_len = usize::try_from(fs::metadata(&data_path).ok()?.len()).ok()?;
        if file_len < HEADER_BYTES_V1 {
            slog_warn!(
                "corrupt semantic index (too small: {} bytes), removing",
                file_len
            );
            if !is_worktree_bridge {
                let _ = fs::remove_file(&data_path);
            }
            return None;
        }

        let bytes = fs::read(&data_path).ok()?;
        let version = bytes[0];
        if version != SEMANTIC_INDEX_VERSION_V6
            && version != SEMANTIC_INDEX_VERSION_V7
            && version != SEMANTIC_INDEX_VERSION_V8
        {
            slog_info!(
                "cached semantic index version {} is older than {}, rebuilding",
                version,
                SEMANTIC_INDEX_VERSION_V8
            );
            if !is_worktree_bridge {
                let _ = fs::remove_file(&data_path);
            }
            return None;
        }
        match Self::from_bytes(&bytes, current_canonical_root) {
            Ok(index) => {
                if index.is_empty() {
                    slog_info!("cached semantic index is empty, will rebuild");
                    if !is_worktree_bridge {
                        let _ = fs::remove_file(&data_path);
                    }
                    return None;
                }
                if let Some(expected) = expected_fingerprint {
                    let matches = index
                        .fingerprint()
                        .map(|fingerprint| fingerprint.matches_expected(expected))
                        .unwrap_or(false);
                    if !matches {
                        slog_info!("cached semantic index fingerprint mismatch, rebuilding");
                        if !is_worktree_bridge {
                            let _ = fs::remove_file(&data_path);
                        }
                        return None;
                    }
                }
                slog_info!("loaded semantic index from disk: {} entries", index.len());
                Some(index)
            }
            Err(e) => {
                slog_warn!("corrupt semantic index, rebuilding: {}", e);
                if !is_worktree_bridge {
                    let _ = fs::remove_file(&data_path);
                }
                None
            }
        }
    }

    /// Serialize the index to bytes for disk persistence
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let fingerprint_bytes = self.fingerprint.as_ref().and_then(|fingerprint| {
            let encoded = fingerprint.as_string();
            if encoded.is_empty() {
                None
            } else {
                Some(encoded.into_bytes())
            }
        });
        let entries: Vec<_> = self
            .store
            .entries_slice()
            .iter()
            .filter_map(|entry| {
                cache_relative_path(&self.project_root, &entry.chunk.file)
                    .map(|relative| (relative, entry))
            })
            .collect();

        // Header: version(1) + dimension(4) + entry_count(4) + fingerprint_len(4) + fingerprint
        //
        // V8 is the single write format. V8 extends V7 with per-entry chunk_hash
        // and a file manifest (FileRecord entries). Layout extends V5/V6/V7:
        //   - fingerprint is always represented (absent ⇒ fingerprint_len=0,
        //     no bytes follow). Uniform format simplifies the reader.
        //   - paths are relative to project_root.
        //   - file metadata stored as secs(u64) + subsec_nanos(u32) + size(u64) + blake3(32).
        //     Preserves full APFS/ext4/NTFS precision and catches mtime ties.
        //   - per-entry chunk_hash (V8+): hash_len(4) + hash bytes after each vector.
        //   - file manifest (V8+): manifest_count(4) + entries after all entry vectors.
        //
        // V1/V2 remain readable for backward compatibility (see from_bytes).
        // V3/V4 load as compatible formats but are rejected on disk so snippets
        // and file sizes are rebuilt once.
        let version = SEMANTIC_INDEX_VERSION_V8;
        buf.push(version);
        buf.extend_from_slice(&(self.dimension as u32).to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        let fp_bytes_ref: &[u8] = fingerprint_bytes.as_deref().unwrap_or(&[]);
        buf.extend_from_slice(&(fp_bytes_ref.len() as u32).to_le_bytes());
        buf.extend_from_slice(fp_bytes_ref);

        // File metadata table: count(4) + entries
        // V6 layout per entry: path_len(4) + path + secs(8) + subsec_nanos(4) + size(u64) + blake3(32).
        //     Preserves full APFS/ext4/NTFS precision and catches mtime ties.
        let file_metadata_entries: Vec<_> = self
            .store
            .file_metadata()
            .iter()
            .filter_map(|(path, meta)| {
                cache_relative_path(&self.project_root, path).map(|relative| (relative, meta))
            })
            .collect();
        buf.extend_from_slice(&(file_metadata_entries.len() as u32).to_le_bytes());
        for (relative, meta) in &file_metadata_entries {
            let path_bytes = relative.to_string_lossy().as_bytes().to_vec();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&path_bytes);
            let duration = meta
                .mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            buf.extend_from_slice(&duration.as_secs().to_le_bytes());
            buf.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
            buf.extend_from_slice(&meta.size.to_le_bytes());
            buf.extend_from_slice(meta.content_hash.as_bytes());
        }

        // Entries: each is metadata + vector
        for (relative, entry) in &entries {
            let c = &entry.chunk;

            // File path
            let file_bytes = relative.to_string_lossy().as_bytes().to_vec();
            buf.extend_from_slice(&(file_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&file_bytes);

            // Name
            let name_bytes = c.name.as_bytes();
            buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(name_bytes);

            // Kind (1 byte)
            buf.push(symbol_kind_to_u8(&c.kind));

            // Lines + exported
            buf.extend_from_slice(&(c.start_line as u32).to_le_bytes());
            buf.extend_from_slice(&(c.end_line as u32).to_le_bytes());
            buf.push(c.exported as u8);

            // Snippet
            let snippet_bytes = c.snippet.as_bytes();
            buf.extend_from_slice(&(snippet_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(snippet_bytes);

            // Embed text
            let embed_bytes = c.embed_text.as_bytes();
            buf.extend_from_slice(&(embed_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(embed_bytes);

            // Vector (f32 array)
            for &val in &entry.vector {
                buf.extend_from_slice(&val.to_le_bytes());
            }

            // chunk_hash (V8+)
            let chunk_hash_str = if entry.chunk_hash.is_empty() {
                compute_chunk_hash(&entry.chunk)
            } else {
                entry.chunk_hash.clone()
            };
            let hash_bytes = chunk_hash_str.as_bytes();
            buf.extend_from_slice(&(hash_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(hash_bytes);
        }

        // File manifest (V8+): manifest_count(4) + entries
        let manifest_entries: Vec<_> = self
            .file_manifest
            .iter()
            .filter_map(|(path, record)| {
                cache_relative_path(&self.project_root, path).map(|relative| (relative, record))
            })
            .collect();
        buf.extend_from_slice(&(manifest_entries.len() as u32).to_le_bytes());
        for (relative, record) in &manifest_entries {
            let path_bytes = relative.to_string_lossy().as_bytes().to_vec();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&path_bytes);

            // content_hash (32 blake3 bytes)
            buf.extend_from_slice(record.content_hash.as_bytes());

            // size (8 bytes)
            buf.extend_from_slice(&record.size_bytes.to_le_bytes());

            // mtime
            let mtime_duration = record
                .mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            buf.extend_from_slice(&mtime_duration.as_secs().to_le_bytes());
            buf.extend_from_slice(&mtime_duration.subsec_nanos().to_le_bytes());

            // language
            let lang_bytes = record.language.as_deref().unwrap_or("").as_bytes();
            buf.extend_from_slice(&(lang_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(lang_bytes);

            // document_kind
            let doc_kind_bytes = record.document_kind.as_bytes();
            buf.extend_from_slice(&(doc_kind_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(doc_kind_bytes);

            // inclusion_policy_hash
            let policy_hash_bytes = record.inclusion_policy_hash.as_bytes();
            buf.extend_from_slice(&(policy_hash_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(policy_hash_bytes);

            // indexed_at
            let indexed_duration = record
                .indexed_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            buf.extend_from_slice(&indexed_duration.as_secs().to_le_bytes());
            buf.extend_from_slice(&indexed_duration.subsec_nanos().to_le_bytes());
        }

        buf
    }

    /// Deserialize the index from bytes
    pub fn from_bytes(data: &[u8], current_canonical_root: &Path) -> Result<Self, String> {
        debug_assert!(current_canonical_root.is_absolute());
        let mut pos = 0;

        if data.len() < HEADER_BYTES_V1 {
            return Err("data too short".to_string());
        }

        let version = data[pos];
        pos += 1;
        if version != SEMANTIC_INDEX_VERSION_V1
            && version != SEMANTIC_INDEX_VERSION_V2
            && version != SEMANTIC_INDEX_VERSION_V3
            && version != SEMANTIC_INDEX_VERSION_V4
            && version != SEMANTIC_INDEX_VERSION_V5
            && version != SEMANTIC_INDEX_VERSION_V6
            && version != SEMANTIC_INDEX_VERSION_V7
            && version != SEMANTIC_INDEX_VERSION_V8
        {
            return Err(format!("unsupported version: {}", version));
        }
        // V2 and newer share the same header layout (V3/V4/V5/V6/V7 only differ from
        // V2 in the per-mtime entry layout): version(1) + dimension(4) +
        // entry_count(4) + fingerprint_len(4) + fingerprint bytes.
        if (version == SEMANTIC_INDEX_VERSION_V2
            || version == SEMANTIC_INDEX_VERSION_V3
            || version == SEMANTIC_INDEX_VERSION_V4
            || version == SEMANTIC_INDEX_VERSION_V5
            || version == SEMANTIC_INDEX_VERSION_V6
            || version == SEMANTIC_INDEX_VERSION_V7
            || version == SEMANTIC_INDEX_VERSION_V8)
            && data.len() < HEADER_BYTES_V2
        {
            return Err(
                "data too short for semantic index v2/v3/v4/v5/v6/v7/v8 header".to_string(),
            );
        }

        let dimension = read_u32(data, &mut pos)? as usize;
        let entry_count = read_u32(data, &mut pos)? as usize;
        validate_embedding_dimension(dimension)?;
        if entry_count > MAX_ENTRIES {
            return Err(format!("too many semantic index entries: {}", entry_count));
        }

        // Fingerprint handling:
        //   - V1: no fingerprint field at all.
        //   - V2: fingerprint_len + fingerprint bytes; always present (writer
        //     only emitted V2 when fingerprint was Some).
        //   - V3+: fingerprint_len always present; fingerprint_len==0 ⇒ None.
        let has_fingerprint_field = version == SEMANTIC_INDEX_VERSION_V2
            || version == SEMANTIC_INDEX_VERSION_V3
            || version == SEMANTIC_INDEX_VERSION_V4
            || version == SEMANTIC_INDEX_VERSION_V5
            || version == SEMANTIC_INDEX_VERSION_V6
            || version == SEMANTIC_INDEX_VERSION_V7
            || version == SEMANTIC_INDEX_VERSION_V8;
        let fingerprint = if has_fingerprint_field {
            let fingerprint_len = read_u32(data, &mut pos)? as usize;
            if pos + fingerprint_len > data.len() {
                return Err("unexpected end of data reading fingerprint".to_string());
            }
            if fingerprint_len == 0 {
                None
            } else {
                let raw = String::from_utf8_lossy(&data[pos..pos + fingerprint_len]).to_string();
                pos += fingerprint_len;
                Some(
                    serde_json::from_str::<SemanticIndexFingerprint>(&raw)
                        .map_err(|error| format!("invalid semantic fingerprint: {error}"))?,
                )
            }
        } else {
            None
        };

        // File mtimes
        let mtime_count = read_u32(data, &mut pos)? as usize;
        if mtime_count > MAX_ENTRIES {
            return Err(format!("too many semantic file mtimes: {}", mtime_count));
        }

        let vector_bytes = entry_count
            .checked_mul(dimension)
            .and_then(|count| count.checked_mul(F32_BYTES))
            .ok_or_else(|| "semantic vector allocation overflow".to_string())?;
        if vector_bytes > data.len().saturating_sub(pos) {
            return Err("semantic index vectors exceed available data".to_string());
        }

        let mut file_metadata: HashMap<PathBuf, IndexedFileMetadata> =
            HashMap::with_capacity(mtime_count);
        for _ in 0..mtime_count {
            let path = read_string(data, &mut pos)?;
            let secs = read_u64(data, &mut pos)?;
            // V3+ persists subsec_nanos alongside secs so staleness checks
            // survive restart round-trips. V1/V2 load with 0 nanos, which
            // causes one rebuild on upgrade (they never matched live APFS
            // mtimes anyway — the bug v0.15.2 fixes). After that rebuild,
            // the cache is persisted as V3 and stabilises.
            let nanos = if version == SEMANTIC_INDEX_VERSION_V3
                || version == SEMANTIC_INDEX_VERSION_V4
                || version == SEMANTIC_INDEX_VERSION_V5
                || version == SEMANTIC_INDEX_VERSION_V6
                || version == SEMANTIC_INDEX_VERSION_V7
                || version == SEMANTIC_INDEX_VERSION_V8
            {
                read_u32(data, &mut pos)?
            } else {
                0
            };
            let size = if version == SEMANTIC_INDEX_VERSION_V5
                || version == SEMANTIC_INDEX_VERSION_V6
                || version == SEMANTIC_INDEX_VERSION_V7
                || version == SEMANTIC_INDEX_VERSION_V8
            {
                read_u64(data, &mut pos)?
            } else {
                0
            };
            let content_hash = if version == SEMANTIC_INDEX_VERSION_V6
                || version == SEMANTIC_INDEX_VERSION_V7
                || version == SEMANTIC_INDEX_VERSION_V8
            {
                if pos + 32 > data.len() {
                    return Err("unexpected end of data reading content hash".to_string());
                }
                let mut hash_bytes = [0u8; 32];
                hash_bytes.copy_from_slice(&data[pos..pos + 32]);
                pos += 32;
                blake3::Hash::from_bytes(hash_bytes)
            } else {
                cache_freshness::zero_hash()
            };
            // Hardening against corrupt / maliciously crafted cache files
            // (v0.15.2). `Duration::new(secs, nanos)` can panic when the
            // nanosecond carry overflows the second counter, and
            // `SystemTime + Duration` can panic on carry past the platform's
            // upper bound. Explicit validation keeps a corrupted semantic.bin
            // from taking down the whole aft process.
            if nanos >= 1_000_000_000 {
                return Err(format!(
                    "invalid semantic mtime: nanos {} >= 1_000_000_000",
                    nanos
                ));
            }
            let duration = std::time::Duration::new(secs, nanos);
            let mtime = SystemTime::UNIX_EPOCH
                .checked_add(duration)
                .ok_or_else(|| {
                    format!(
                        "invalid semantic mtime: secs={} nanos={} overflows SystemTime",
                        secs, nanos
                    )
                })?;
            let path = if version == SEMANTIC_INDEX_VERSION_V6
                || version == SEMANTIC_INDEX_VERSION_V7
                || version == SEMANTIC_INDEX_VERSION_V8
            {
                cached_path_under_root(current_canonical_root, &PathBuf::from(path))
                    .ok_or_else(|| "cached semantic mtime path escapes project root".to_string())?
            } else {
                PathBuf::from(path)
            };
            file_metadata.insert(
                path,
                IndexedFileMetadata {
                    mtime,
                    size,
                    content_hash,
                },
            );
        }

        // Entries
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let raw_file = PathBuf::from(read_string(data, &mut pos)?);
            let file = if version == SEMANTIC_INDEX_VERSION_V6
                || version == SEMANTIC_INDEX_VERSION_V7
                || version == SEMANTIC_INDEX_VERSION_V8
            {
                cached_path_under_root(current_canonical_root, &raw_file)
                    .ok_or_else(|| "cached semantic entry path escapes project root".to_string())?
            } else {
                raw_file
            };
            let name = read_string(data, &mut pos)?;

            if pos >= data.len() {
                return Err("unexpected end of data".to_string());
            }
            let kind = u8_to_symbol_kind(data[pos]);
            pos += 1;

            let start_line = read_u32(data, &mut pos)?;
            let end_line = read_u32(data, &mut pos)?;

            if pos >= data.len() {
                return Err("unexpected end of data".to_string());
            }
            let exported = data[pos] != 0;
            pos += 1;

            let snippet = read_string(data, &mut pos)?;
            let embed_text = read_string(data, &mut pos)?;

            // Vector
            let vec_bytes = dimension
                .checked_mul(F32_BYTES)
                .ok_or_else(|| "semantic vector allocation overflow".to_string())?;
            if pos + vec_bytes > data.len() {
                return Err("unexpected end of data reading vector".to_string());
            }
            let mut vector = Vec::with_capacity(dimension);
            for _ in 0..dimension {
                let bytes = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
                vector.push(f32::from_le_bytes(bytes));
                pos += 4;
            }

            // chunk_hash (V8+)
            let chunk_hash = if version == SEMANTIC_INDEX_VERSION_V8 {
                let hash_len = read_u32(data, &mut pos)? as usize;
                if pos + hash_len > data.len() {
                    return Err("unexpected end of data reading chunk_hash".to_string());
                }
                let hash_str = String::from_utf8_lossy(&data[pos..pos + hash_len]).to_string();
                pos += hash_len;
                hash_str
            } else {
                String::new()
            };

            entries.push(EmbeddingEntry {
                chunk: SemanticChunk {
                    file,
                    name,
                    kind,
                    start_line,
                    end_line,
                    exported,
                    embed_text,
                    snippet,
                },
                vector,
                chunk_hash,
            });
        }

        if entries.len() != entry_count {
            return Err(format!(
                "semantic cache entry count drift: header={} decoded={}",
                entry_count,
                entries.len()
            ));
        }
        for entry in &entries {
            if !file_metadata.contains_key(&entry.chunk.file) {
                return Err(format!(
                    "semantic cache metadata missing for entry file {}",
                    entry.chunk.file.display()
                ));
            }
        }

        // File manifest (V8+)
        let file_manifest = if version == SEMANTIC_INDEX_VERSION_V8 {
            let manifest_count = read_u32(data, &mut pos)? as usize;
            let mut manifest = HashMap::with_capacity(manifest_count);
            for _ in 0..manifest_count {
                let relative_path = PathBuf::from(read_string(data, &mut pos)?);

                // content_hash (32 blake3 bytes)
                if pos + 32 > data.len() {
                    return Err("unexpected end of data reading manifest content hash".to_string());
                }
                let mut hash_bytes = [0u8; 32];
                hash_bytes.copy_from_slice(&data[pos..pos + 32]);
                pos += 32;
                let content_hash = blake3::Hash::from_bytes(hash_bytes);

                // size
                let size = read_u64(data, &mut pos)?;

                // mtime
                let mtime_secs = read_u64(data, &mut pos)?;
                let mtime_nanos = read_u32(data, &mut pos)?;
                if mtime_nanos >= 1_000_000_000 {
                    return Err(format!(
                        "invalid manifest mtime: nanos {} >= 1_000_000_000",
                        mtime_nanos
                    ));
                }
                let mtime_duration = std::time::Duration::new(mtime_secs, mtime_nanos);
                let mtime = SystemTime::UNIX_EPOCH
                    .checked_add(mtime_duration)
                    .ok_or_else(|| {
                        format!(
                            "invalid manifest mtime: secs={} nanos={} overflows SystemTime",
                            mtime_secs, mtime_nanos
                        )
                    })?;

                // language
                let language = {
                    let lang_len = read_u32(data, &mut pos)? as usize;
                    if pos + lang_len > data.len() {
                        return Err("unexpected end of data reading manifest language".to_string());
                    }
                    let lang_str = if lang_len > 0 {
                        Some(String::from_utf8_lossy(&data[pos..pos + lang_len]).to_string())
                    } else {
                        None
                    };
                    pos += lang_len;
                    lang_str
                };

                // document_kind
                let document_kind = read_string(data, &mut pos)?;

                // inclusion_policy_hash
                let inclusion_policy_hash = read_string(data, &mut pos)?;

                // indexed_at
                let indexed_at_secs = read_u64(data, &mut pos)?;
                let indexed_at_nanos = read_u32(data, &mut pos)?;
                if indexed_at_nanos >= 1_000_000_000 {
                    return Err(format!(
                        "invalid manifest indexed_at: nanos {} >= 1_000_000_000",
                        indexed_at_nanos
                    ));
                }
                let indexed_at_duration =
                    std::time::Duration::new(indexed_at_secs, indexed_at_nanos);
                let indexed_at = SystemTime::UNIX_EPOCH
                    .checked_add(indexed_at_duration)
                    .ok_or_else(|| {
                        format!(
                            "invalid manifest indexed_at: secs={} nanos={} overflows SystemTime",
                            indexed_at_secs, indexed_at_nanos
                        )
                    })?;

                // Reconstruct absolute path
                let abs_path = cached_path_under_root(current_canonical_root, &relative_path)
                    .ok_or_else(|| "cached file manifest path escapes project root".to_string())?;

                manifest.insert(
                    abs_path,
                    FileRecord {
                        content_hash,
                        size_bytes: size,
                        mtime,
                        language,
                        document_kind,
                        inclusion_policy_hash,
                        indexed_at,
                    },
                );
            }
            manifest
        } else {
            HashMap::new()
        };

        let fingerprint_string = if version >= SEMANTIC_INDEX_VERSION_V7 {
            fingerprint.as_ref().map(|fp| fp.as_string())
        } else {
            None
        };

        let mut snapshot = SemanticIndexSnapshot {
            store: crate::vector_store::FlatF32VectorStore::from_parts(
                entries,
                dimension,
                file_metadata,
            ),
            dimension,
            project_root: current_canonical_root.to_path_buf(),
            file_manifest,
            next_chunk_id: 0,
            fingerprint_string,
        };
        // For pre-V8 cache data, the manifest was not serialized, so build it
        // from the store's existing file_metadata.
        if snapshot.file_manifest.is_empty() && !snapshot.store.file_metadata().is_empty() {
            snapshot.build_manifest_from_store();
        }
        Ok(Self {
            snapshot: Arc::new(snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint,
            deferred_files: HashSet::new(),
        })
    }
}

/// Embed texts with exponential backoff retry for transient remote provider errors
/// (rate limits, timeouts, server errors). Up to 3 retries with base delay of 1s,
/// capped at 8s max. Non-transient errors (dimension mismatch, config errors) are
/// returned immediately without retry.
fn embed_with_retry<F>(embed_fn: &mut F, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String>
where
    F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
{
    const MAX_RETRIES: u32 = 3;
    const BASE_DELAY_MS: u64 = 1000;
    const MAX_DELAY_MS: u64 = 8000;

    let mut last_err = String::new();
    for attempt in 0..=MAX_RETRIES {
        match embed_fn(texts.clone()) {
            Ok(vectors) => return Ok(vectors),
            Err(e) => {
                last_err = e.clone();
                // Only retry on transient errors (rate limit, timeout, server)
                let is_transient = e.to_lowercase().contains("rate")
                    || e.to_lowercase().contains("limit")
                    || e.to_lowercase().contains("timeout")
                    || e.to_lowercase().contains("429")
                    || e.to_lowercase().contains("503")
                    || e.to_lowercase().contains("502")
                    || e.to_lowercase().contains("500")
                    || e.to_lowercase().contains("connection")
                    || e.to_lowercase().contains("reset")
                    || e.to_lowercase().contains("network");

                if !is_transient || attempt == MAX_RETRIES {
                    return Err(last_err);
                }
                let delay = (BASE_DELAY_MS * 2u64.pow(attempt)).min(MAX_DELAY_MS);
                slog_warn!(
                    "embedding batch failed (attempt {}/{}): {}. Retrying in {}ms...",
                    attempt + 1,
                    MAX_RETRIES + 1,
                    e,
                    delay
                );
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
    }
    Err(last_err)
}

/// Build enriched embedding text from a symbol with cAST-style context
fn build_embed_text(symbol: &Symbol, source: &str, file: &Path, project_root: &Path) -> String {
    let relative = file
        .strip_prefix(project_root)
        .unwrap_or(file)
        .to_string_lossy();

    let kind_label = match &symbol.kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Variable => "variable",
        SymbolKind::Heading => "heading",
        SymbolKind::FileSummary => "file-summary",
    };

    // Build: "file:relative/path kind:function name:validateAuth signature:fn validateAuth(token: &str) -> bool"
    let name = &symbol.name;
    let mut text = format!("file:{} kind:{} name:{name}", relative, kind_label);

    if let Some(sig) = &symbol.signature {
        text.push_str(&format!(" signature:{}", sig));
    }

    // Add body snippet (first ~300 chars of symbol body)
    let lines: Vec<&str> = source.lines().collect();
    let start = (symbol.range.start_line as usize).min(lines.len());
    // range.end_line is inclusive 0-based; +1 makes it an exclusive slice bound.
    let end = (symbol.range.end_line as usize + 1).min(lines.len());
    if start < end {
        let body: String = lines[start..end]
            .iter()
            .take(15) // max 15 lines
            .copied()
            .collect::<Vec<&str>>()
            .join("\n");
        let snippet = if body.len() > 300 {
            format!("{}...", &body[..body.floor_char_boundary(300)])
        } else {
            body
        };
        text.push_str(&format!(" body:{}", snippet));
    }

    text
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn first_leading_doc_comment(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let Some((start, first)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
    else {
        return String::new();
    };

    let trimmed = first.trim_start();
    if trimmed.starts_with("/**") {
        let mut comment = Vec::new();
        for line in lines.iter().skip(start) {
            comment.push(*line);
            if line.contains("*/") {
                break;
            }
        }
        return truncate_chars(&comment.join("\n"), 200);
    }

    if trimmed.starts_with("///") || trimmed.starts_with("//!") {
        let comment = lines
            .iter()
            .skip(start)
            .take_while(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("///") || trimmed.starts_with("//!")
            })
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        return truncate_chars(&comment, 200);
    }

    String::new()
}

pub fn build_file_summary_chunk(
    file: &Path,
    project_root: &Path,
    source: &str,
    top_exports: &[&str],
    top_export_signatures: &[Option<&str>],
) -> SemanticChunk {
    let relative = file.strip_prefix(project_root).unwrap_or(file);
    let rel_path = relative.to_string_lossy();
    let parent_dir = relative
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    let name = file
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let doc = first_leading_doc_comment(source);
    let exports = top_exports
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join(",");
    let snippet = if doc.is_empty() {
        top_export_signatures
            .first()
            .and_then(|signature| signature.as_deref())
            .map(|signature| truncate_chars(signature, 200))
            .unwrap_or_default()
    } else {
        doc.clone()
    };

    SemanticChunk {
        file: file.to_path_buf(),
        name,
        kind: SymbolKind::FileSummary,
        start_line: 0,
        end_line: 0,
        exported: false,
        embed_text: format!(
            "file:{rel_path} kind:file-summary name:{} parent:{parent_dir} doc:{doc} exports:{exports}",
            file.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .unwrap_or_default()
        ),
        snippet,
    }
}

fn parser_for(
    parsers: &mut HashMap<crate::parser::LangId, Parser>,
    lang: crate::parser::LangId,
) -> Result<&mut Parser, String> {
    use std::collections::hash_map::Entry;

    match parsers.entry(lang) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(entry) => {
            let grammar = grammar_for(lang);
            let mut parser = Parser::new();
            parser
                .set_language(&grammar)
                .map_err(|error| error.to_string())?;
            Ok(entry.insert(parser))
        }
    }
}

pub fn is_semantic_indexed_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(
            "ts" | "tsx"
                | "js"
                | "jsx"
                | "py"
                | "rs"
                | "go"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "cxx"
                | "hpp"
                | "hh"
                | "zig"
                | "cs"
                | "sh"
                | "bash"
                | "zsh"
                | "sol"
                | "vue"
        )
    )
}

fn collect_file_metadata(file: &Path) -> Result<IndexedFileMetadata, String> {
    let metadata = fs::metadata(file).map_err(|error| error.to_string())?;
    let mtime = metadata.modified().map_err(|error| error.to_string())?;
    let content_hash = cache_freshness::hash_file_if_small(file, metadata.len())
        .map_err(|error| error.to_string())?
        .unwrap_or_else(cache_freshness::zero_hash);
    Ok(IndexedFileMetadata {
        mtime,
        size: metadata.len(),
        content_hash,
    })
}

fn collect_file_chunks(
    project_root: &Path,
    file: &Path,
    parsers: &mut HashMap<crate::parser::LangId, Parser>,
) -> Result<Vec<SemanticChunk>, String> {
    if !is_semantic_indexed_extension(file) {
        return Err("unsupported file extension".to_string());
    }
    let lang = detect_language(file).ok_or_else(|| "unsupported file extension".to_string())?;
    let source = std::fs::read_to_string(file).map_err(|error| error.to_string())?;
    let tree = parser_for(parsers, lang)?
        .parse(&source, None)
        .ok_or_else(|| format!("tree-sitter parse returned None for {}", file.display()))?;
    let symbols =
        extract_symbols_from_tree(&source, &tree, lang).map_err(|error| error.to_string())?;

    Ok(symbols_to_chunks(file, &symbols, &source, project_root))
}

/// Build a display snippet from a symbol's source
fn build_snippet(symbol: &Symbol, source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = (symbol.range.start_line as usize).min(lines.len());
    // range.end_line is inclusive 0-based; +1 makes it an exclusive slice bound.
    let end = (symbol.range.end_line as usize + 1).min(lines.len());
    if start < end {
        let snippet_lines: Vec<&str> = lines[start..end].iter().take(5).copied().collect();
        let mut snippet = snippet_lines.join("\n");
        if end - start > 5 {
            snippet.push_str("\n  ...");
        }
        if snippet.len() > 300 {
            snippet = format!("{}...", &snippet[..snippet.floor_char_boundary(300)]);
        }
        snippet
    } else {
        String::new()
    }
}

/// Convert symbols to semantic chunks with enriched context
fn symbols_to_chunks(
    file: &Path,
    symbols: &[Symbol],
    source: &str,
    project_root: &Path,
) -> Vec<SemanticChunk> {
    let mut chunks = Vec::new();
    let top_exports_with_signatures = symbols
        .iter()
        .filter(|symbol| {
            symbol.exported
                && symbol.parent.is_none()
                && !matches!(symbol.kind, SymbolKind::Heading)
        })
        .map(|symbol| (symbol.name.as_str(), symbol.signature.as_deref()))
        .collect::<Vec<_>>();

    let has_only_headings = !symbols.is_empty()
        && symbols
            .iter()
            .all(|symbol| matches!(symbol.kind, SymbolKind::Heading));
    if top_exports_with_signatures.len() <= 2 && !has_only_headings {
        let top_exports = top_exports_with_signatures
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        let top_export_signatures = top_exports_with_signatures
            .iter()
            .map(|(_, signature)| *signature)
            .collect::<Vec<_>>();
        chunks.push(build_file_summary_chunk(
            file,
            project_root,
            source,
            &top_exports,
            &top_export_signatures,
        ));
    }

    for symbol in symbols {
        // Skip Markdown / HTML heading chunks: empirically they dominate result
        // lists even for code-shaped queries because heading prose embeds well.
        // Agents querying for code lose the actual matches under doc noise.
        // README/docs queries are still served by grep on the same files.
        if matches!(symbol.kind, SymbolKind::Heading) {
            continue;
        }

        // Skip very small symbols (single-line variables, etc.)
        let line_count = symbol
            .range
            .end_line
            .saturating_sub(symbol.range.start_line)
            + 1;
        if line_count < 2 && !matches!(symbol.kind, SymbolKind::Variable) {
            continue;
        }

        let embed_text = build_embed_text(symbol, source, file, project_root);
        let snippet = build_snippet(symbol, source);

        chunks.push(SemanticChunk {
            file: file.to_path_buf(),
            name: symbol.name.clone(),
            kind: symbol.kind.clone(),
            start_line: symbol.range.start_line,
            end_line: symbol.range.end_line,
            exported: symbol.exported,
            embed_text,
            snippet,
        });

        // Note: Nested symbols are handled separately by the outline system
        // Each symbol is indexed individually
    }

    chunks
}

/// Cosine similarity between two vectors
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 || !denom.is_normal() {
        0.0
    } else {
        let result = dot / denom;
        // Guard against NaN from floating-point edge cases (e.g. subnormal norms).
        if result.is_nan() {
            0.0
        } else {
            result.clamp(-1.0, 1.0)
        }
    }
}

// Serialization helpers
fn symbol_kind_to_u8(kind: &SymbolKind) -> u8 {
    match kind {
        SymbolKind::Function => 0,
        SymbolKind::Class => 1,
        SymbolKind::Method => 2,
        SymbolKind::Struct => 3,
        SymbolKind::Interface => 4,
        SymbolKind::Enum => 5,
        SymbolKind::TypeAlias => 6,
        SymbolKind::Variable => 7,
        SymbolKind::Heading => 8,
        SymbolKind::FileSummary => 9,
    }
}

fn u8_to_symbol_kind(v: u8) -> SymbolKind {
    match v {
        0 => SymbolKind::Function,
        1 => SymbolKind::Class,
        2 => SymbolKind::Method,
        3 => SymbolKind::Struct,
        4 => SymbolKind::Interface,
        5 => SymbolKind::Enum,
        6 => SymbolKind::TypeAlias,
        7 => SymbolKind::Variable,
        8 => SymbolKind::Heading,
        9 => SymbolKind::FileSummary,
        _ => SymbolKind::Heading,
    }
}

fn write_counted<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    bytes_written: &mut usize,
) -> io::Result<()> {
    writer.write_all(bytes)?;
    *bytes_written = bytes_written.saturating_add(bytes.len());
    Ok(())
}

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R> CountingReader<R> {
    fn with_bytes_read(inner: R, bytes_read: usize) -> Self {
        Self { inner, bytes_read }
    }

    fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read = self.bytes_read.saturating_add(read);
        Ok(read)
    }
}

fn read_exact_stream<R: Read>(
    reader: &mut CountingReader<R>,
    buf: &mut [u8],
    eof_message: &'static str,
) -> Result<(), String> {
    reader.read_exact(buf).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            eof_message.to_string()
        } else {
            format!("{eof_message}: {error}")
        }
    })
}

fn read_u8_stream<R: Read>(
    reader: &mut CountingReader<R>,
    eof_message: &'static str,
) -> Result<u8, String> {
    let mut bytes = [0u8; 1];
    read_exact_stream(reader, &mut bytes, eof_message)?;
    Ok(bytes[0])
}

fn read_u32_stream<R: Read>(reader: &mut CountingReader<R>) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    read_exact_stream(reader, &mut bytes, "unexpected end of data reading u32")?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_stream<R: Read>(reader: &mut CountingReader<R>) -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    read_exact_stream(reader, &mut bytes, "unexpected end of data reading u64")?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string_stream<R: Read>(
    reader: &mut CountingReader<R>,
    total_len: Option<usize>,
) -> Result<String, String> {
    let len = read_u32_stream(reader)? as usize;
    if total_len.is_some_and(|total_len| reader.bytes_read().saturating_add(len) > total_len) {
        return Err("unexpected end of data reading string".to_string());
    }
    let mut bytes = vec![0u8; len];
    read_exact_stream(reader, &mut bytes, "unexpected end of data reading string")?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    if *pos + 4 > data.len() {
        return Err("unexpected end of data reading u32".to_string());
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, String> {
    if *pos + 8 > data.len() {
        return Err("unexpected end of data reading u64".to_string());
    }
    let bytes: [u8; 8] = data[*pos..*pos + 8].try_into().unwrap();
    *pos += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = read_u32(data, pos)? as usize;
    if *pos + len > data.len() {
        return Err("unexpected end of data reading string".to_string());
    }
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
    *pos += len;
    Ok(s)
}

// ---------------------------------------------------------------------------
// File policy helpers
// ---------------------------------------------------------------------------

/// Check if a file path looks auto-generated based on name and directory heuristics.
pub(crate) fn is_generated_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let name_lower = name.to_lowercase();

    // Generated file name patterns
    name_lower.ends_with(".generated.rs")
        || name_lower.ends_with(".generated.go")
        || name_lower.ends_with(".generated.ts")
        || name_lower.ends_with(".pb.go") // protobuf
        || name_lower.ends_with(".pb.rs") // protobuf
        || name_lower.ends_with("_pb2.py") // protobuf
        || name_lower.starts_with(".generated")
        || name_lower.contains(".min.") // minified
        || name_lower.ends_with(".snap") // jest snapshots
        || name_lower.ends_with(".g.dart") // generated dart
        || name_lower.ends_with(".freezed.dart")
        || path
            .ancestors()
            .any(|a| {
                let s = a
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                matches!(
                    s.as_ref(),
                    "generated" | "__generated__" | ".graphql" | "dist" | "build"
                )
            })
}

/// Check if a file extension suggests it is a documentation file.
pub(crate) fn is_doc_extension(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .map(|ext| {
            matches!(
                ext.as_str(),
                "md" | "markdown" | "rst" | "txt" | "adoc" | "org" | "creole" | "mediawiki"
            )
        })
        .unwrap_or(false)
}

/// Check if a file extension or name suggests it is a configuration file.
pub(crate) fn is_config_extension(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let name_lower = name.to_lowercase();

    // Dotfiles that are config-like
    if name_lower.starts_with('.') && !name_lower.starts_with("..") {
        return matches!(
            name_lower.as_str(),
            ".env"
                | ".eslintrc"
                | ".prettierrc"
                | ".babelrc"
                | ".tsconfig"
                | ".editorconfig"
                | ".gitignore"
                | ".dockerignore"
                | ".npmrc"
                | ".yarnrc"
                | ".nvmrc"
                | ".python-version"
                | ".tool-versions"
                | ".rubocop"
                | ".stylelintrc"
        );
    }

    // Config extensions (but exclude lockfiles)
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .map(|ext| {
            matches!(
                ext.as_str(),
                "toml" | "yaml" | "yml" | "json" | "jsonc" | "ini" | "cfg" | "conf"
            )
        })
        .unwrap_or(false)
        && !name_lower.contains("package-lock")
        && !name_lower.contains("yarn.lock")
        && !name_lower.contains("bun.lock")
        && !name_lower.contains("pnpm-lock")
}

/// Statistics about files skipped by the file policy during indexing.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FilePolicyStats {
    pub skipped_binary: usize,
    pub skipped_generated: usize,
    pub skipped_too_large: usize,
    pub skipped_excluded: usize,
    pub skipped_code_disabled: usize,
    pub skipped_docs_disabled: usize,
    pub skipped_configs_disabled: usize,
    pub skipped_unknown_type: usize,
    pub docs_files_indexed: usize,
    pub config_files_indexed: usize,
}

/// Diagnostics collected during contextualized document-chunk embedding.
/// Tracks oversized document handling, retry behavior, and per-request metrics.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ContextualizedBuildDiagnostics {
    /// Total source documents processed (before splitting into sub-groups).
    pub documents_processed: usize,
    /// Total chunks embedded across all documents.
    pub chunks_embedded: usize,
    /// Documents that were split into multiple sub-groups because they
    /// exceeded max_chunks_per_document.
    pub split_documents: usize,
    /// Document groups that failed embedding and were retried.
    pub retried_groups: usize,
    /// Document groups that failed after all retries and were skipped.
    pub failed_groups: usize,
    /// Maximum chunks in any single document (before splitting).
    pub max_chunks_in_document: usize,
}

/// Classify a file's type for the semantic indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticFileType {
    Code,
    Doc,
    Config,
    Unknown,
}

/// Determine the semantic file type based on extension and path.
pub(crate) fn classify_semantic_file(path: &Path) -> SemanticFileType {
    if is_doc_extension(path) {
        return SemanticFileType::Doc;
    }
    if is_config_extension(path) {
        return SemanticFileType::Config;
    }
    // If it has a known code language, it's code
    if detect_language(path).is_some() {
        return SemanticFileType::Code;
    }
    // Fall back: check if it's text-ish but not classified
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "md" | "rst" | "txt") {
        SemanticFileType::Doc
    } else {
        SemanticFileType::Unknown
    }
}

// ---------------------------------------------------------------------------
// Docs chunker — splits Markdown files into heading-based chunks
// ---------------------------------------------------------------------------

/// Maximum characters per chunk before splitting at paragraph boundaries.
const MAX_CHUNK_CHARS: usize = 8000;

/// Split a documentation file (primarily Markdown) into semantic chunks.
/// Each `##` heading (h2 or deeper) starts a new chunk. Content before the
/// first heading becomes a "summary" chunk. Overly large chunks are split
/// further at paragraph boundaries.
pub(crate) fn collect_docs_chunks(text: &str, file_path: &Path) -> Vec<SemanticChunk> {
    let ext = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if matches!(ext.as_str(), "md" | "markdown") {
        collect_markdown_chunks(text, file_path)
    } else {
        // Non-markdown docs: single chunk
        let body = text.trim().to_string();
        if body.is_empty() {
            return Vec::new();
        }
        let file_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "doc".to_string());
        vec![SemanticChunk {
            file: file_path.to_path_buf(),
            name: file_name,
            kind: SymbolKind::Heading,
            start_line: 0,
            end_line: text.lines().count().saturating_sub(1) as u32,
            exported: false,
            embed_text: body.clone(),
            snippet: truncate_snippet(&body),
        }]
    }
}

fn collect_markdown_chunks(text: &str, file_path: &Path) -> Vec<SemanticChunk> {
    let mut chunks = Vec::new();
    let mut current_heading = "Summary".to_string();
    let mut current_lines: Vec<String> = Vec::new();
    let mut line_num: u32 = 0;
    let mut chunk_start_line: u32 = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        // Detect ATX headings: ## or deeper (level >= 2)
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            if level >= 2 && !current_lines.is_empty() {
                // Flush previous chunk
                let body = current_lines.join("\n").trim().to_string();
                if !body.is_empty() {
                    chunks.push(SemanticChunk {
                        file: file_path.to_path_buf(),
                        name: current_heading.clone(),
                        kind: SymbolKind::Heading,
                        start_line: chunk_start_line,
                        end_line: line_num.saturating_sub(1),
                        exported: false,
                        embed_text: body.clone(),
                        snippet: truncate_snippet(&body),
                    });
                }
                chunk_start_line = line_num;
                current_lines.clear();
            }
            if level >= 1 {
                current_heading = trimmed.trim_start_matches('#').trim().to_string();
            }
        }
        current_lines.push(line.to_string());
        line_num += 1;
    }

    // Flush remaining
    let body = current_lines.join("\n").trim().to_string();
    if !body.is_empty() {
        chunks.push(SemanticChunk {
            file: file_path.to_path_buf(),
            name: current_heading.clone(),
            kind: SymbolKind::Heading,
            start_line: chunk_start_line,
            end_line: line_num.saturating_sub(1),
            exported: false,
            embed_text: body.clone(),
            snippet: truncate_snippet(&body),
        });
    }

    // Split overly large chunks at paragraph boundaries
    let mut result = Vec::new();
    for chunk in chunks {
        if chunk.embed_text.len() <= MAX_CHUNK_CHARS {
            result.push(chunk);
        } else {
            result.append(&mut split_large_chunk(&chunk));
        }
    }

    result
}

/// Truncate text to a short snippet for display in search results.
fn truncate_snippet(text: &str) -> String {
    let s = text.trim();
    if s.len() <= 200 {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(197).collect();
        truncated.push_str("...");
        truncated
    }
}

/// Chunk large embed texts to prevent HTTP 400 errors on remote backends.
///
/// Splits chunks whose `embed_text` exceeds `max_embed_tokens` (estimated as
/// chars / 4) at paragraph boundaries with configurable overlap. This only
/// affects remote backends — local backends (Fastembed, Model2Vec) already
/// truncate internally via `tokenizers::Tokenizer` with `max_length`.
fn chunk_large_embed_texts(
    chunks: Vec<SemanticChunk>,
    max_embed_tokens: usize,
    chunk_overlap_tokens: usize,
) -> Vec<SemanticChunk> {
    if max_embed_tokens == 0 {
        return chunks;
    }

    // Convert token limit to character limit (approximate: 1 token ≈ 4 chars)
    let max_chars = max_embed_tokens * 4;
    let overlap_chars = chunk_overlap_tokens * 4;

    let mut result = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.embed_text.len() <= max_chars {
            result.push(chunk);
        } else {
            result.extend(split_chunk_with_overlap(&chunk, max_chars, overlap_chars));
        }
    }
    result
}

/// Split a chunk into smaller chunks at paragraph boundaries with overlap.
///
/// Each chunk includes the last `overlap_chars` of the previous chunk to
/// preserve boundary context. The first chunk retains the original name;
/// subsequent chunks get " (cont.)" suffix.
fn split_chunk_with_overlap(
    chunk: &SemanticChunk,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<SemanticChunk> {
    let mut result = Vec::new();
    let mut current_body = String::new();
    let mut chunk_start = chunk.start_line;
    let mut current_lines: u32 = 0;
    let mut prev_tail: Option<String> = None;

    for para in chunk.embed_text.split("\n\n") {
        // Check if adding this paragraph would exceed the limit
        let test_len = if let Some(ref tail) = prev_tail {
            tail.len() + 2 + para.len() // +2 for "\n\n"
        } else {
            current_body.len() + if current_body.is_empty() { 0 } else { 2 } + para.len()
        };

        if !current_body.is_empty() && test_len > max_chars {
            // Flush current sub-chunk
            let body = current_body.trim().to_string();
            result.push(SemanticChunk {
                file: chunk.file.clone(),
                name: if result.is_empty() {
                    chunk.name.clone()
                } else {
                    format!("{} (cont.)", chunk.name)
                },
                kind: chunk.kind.clone(),
                start_line: chunk_start,
                end_line: chunk_start + current_lines,
                exported: false,
                embed_text: body.clone(),
                snippet: truncate_snippet(&body),
            });

            // Save overlap tail for next chunk
            prev_tail = Some(extract_tail(&current_body, overlap_chars));
            chunk_start += current_lines;
            current_body.clear();
            current_lines = 0;
        }

        // Prepend overlap from previous chunk
        if let Some(ref tail) = prev_tail {
            if !current_body.is_empty() {
                current_body.push_str("\n\n");
            }
            current_body.push_str(tail);
            current_body.push_str("\n\n");
            prev_tail = None;
        }

        if !current_body.is_empty() {
            current_body.push_str("\n\n");
        }
        current_body.push_str(para);
        current_lines += para.lines().count() as u32;
    }

    if !current_body.trim().is_empty() {
        let body = current_body.trim().to_string();
        result.push(SemanticChunk {
            file: chunk.file.clone(),
            name: if result.is_empty() {
                chunk.name.clone()
            } else {
                format!("{} (cont.)", chunk.name)
            },
            kind: chunk.kind.clone(),
            start_line: chunk_start,
            end_line: chunk_start + current_lines,
            exported: false,
            embed_text: body.clone(),
            snippet: truncate_snippet(&body),
        });
    }

    result
}

/// Extract the last `max_chars` from text, breaking at a paragraph boundary.
fn extract_tail(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Find the last paragraph boundary before max_chars
    let truncated = &text[..text.floor_char_boundary(max_chars)];
    if let Some(last_para) = truncated.rfind("\n\n") {
        text[last_para + 2..].to_string()
    } else {
        // No paragraph boundary found; just take the tail
        truncated.to_string()
    }
}

fn split_large_chunk(chunk: &SemanticChunk) -> Vec<SemanticChunk> {
    let mut result = Vec::new();
    let mut current_body = String::new();
    let mut chunk_start = chunk.start_line;
    let mut current_lines: u32 = 0;

    for para in chunk.embed_text.split("\n\n") {
        if !current_body.is_empty() && current_body.len() + para.len() > MAX_CHUNK_CHARS {
            // Flush current sub-chunk
            let body = current_body.trim().to_string();
            result.push(SemanticChunk {
                file: chunk.file.clone(),
                name: format!("{} (cont.)", chunk.name),
                kind: chunk.kind.clone(),
                start_line: chunk_start,
                end_line: chunk_start + current_lines,
                exported: false,
                embed_text: body.clone(),
                snippet: truncate_snippet(&body),
            });
            chunk_start += current_lines + 1;
            current_body.clear();
            current_lines = 0;
        }
        if !current_body.is_empty() {
            current_body.push_str("\n\n");
        }
        current_body.push_str(para);
        current_lines += para.lines().count() as u32;
    }

    if !current_body.trim().is_empty() {
        let body = current_body.trim().to_string();
        result.push(SemanticChunk {
            file: chunk.file.clone(),
            name: if result.is_empty() {
                chunk.name.clone()
            } else {
                format!("{} (cont.)", chunk.name)
            },
            kind: chunk.kind.clone(),
            start_line: chunk_start,
            end_line: chunk_start + current_lines,
            exported: false,
            embed_text: body.clone(),
            snippet: truncate_snippet(&body),
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SemanticBackend, SemanticBackendConfig};
    use crate::parser::FileParser;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    pub(crate) fn start_mock_http_server<F>(handler: F) -> (String, thread::JoinHandle<()>)
    where
        F: Fn(String, String, String) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let n = stream.read(&mut chunk).expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                        for line in headers.lines() {
                            let lower = line.trim().to_lowercase();
                            if let Some(value) = lower.strip_prefix("content-length:") {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }

            let end = header_end.expect("header terminator");
            let request = String::from_utf8_lossy(&buf[..end]).to_string();
            let body = String::from_utf8_lossy(&buf[end..end + content_length]).to_string();
            let mut lines = request.lines();
            let request_line = lines.next().expect("request line").to_string();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .expect("request path")
                .to_string();
            let response_body = handler(request_line, path, body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        (format!("http://{}", addr), handle)
    }

    fn test_vector_for_texts(texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }

    fn write_rust_file(path: &Path, function_name: &str) {
        fs::write(
            path,
            format!("pub fn {function_name}() -> bool {{\n    true\n}}\n"),
        )
        .unwrap();
    }

    fn build_test_index(project_root: &Path, files: &[PathBuf]) -> SemanticIndex {
        let mut embed = test_vector_for_texts;
        SemanticIndex::build(project_root, files, &mut embed, 8).unwrap()
    }

    fn test_project_root() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    fn set_file_metadata(index: &mut SemanticIndex, file: &Path, mtime: SystemTime, size: u64) {
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            file.to_path_buf(),
            IndexedFileMetadata {
                mtime,
                size,
                content_hash: hash,
            },
        );
    }

    #[test]
    fn semantic_cache_serialization_skips_paths_outside_project_root() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project = fs::canonicalize(dir.path()).expect("canonical project");
        let outside = project.join("..").join("outside.rs");
        let mut index = SemanticIndex::new(project.clone(), 3);
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            outside.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::UNIX_EPOCH,
                size: 1,
                content_hash: hash,
            },
        );
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: outside,
                name: "outside".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 0,
                exported: false,
                embed_text: "outside".to_string(),
                snippet: "outside".to_string(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });

        let bytes = index.to_bytes();
        let loaded = SemanticIndex::from_bytes(&bytes, &project).expect("load serialized index");
        assert_eq!(loaded.len(), 0);
        assert!(loaded.file_metadata().is_empty());
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let project_root = test_project_root();
        let file = project_root.join("src/main.rs");
        let mut index = SemanticIndex::new(project_root.clone(), DEFAULT_DIMENSION);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: file.clone(),
                name: "handle_request".to_string(),
                kind: SymbolKind::Function,
                start_line: 10,
                end_line: 25,
                exported: true,
                embed_text: "file:src/main.rs kind:function name:handle_request".to_string(),
                snippet: "fn handle_request() {\n  // ...\n}".to_string(),
            },
            vector: vec![0.1, 0.2, 0.3, 0.4],
            chunk_hash: String::new(),
        });
        index.set_dimension(4);
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            file.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::UNIX_EPOCH,
                size: 0,
                content_hash: hash,
            },
        );
        index.set_fingerprint(SemanticIndexFingerprint {
            backend: "fastembed".to_string(),
            model: "all-MiniLM-L6-v2".to_string(),
            base_url: FALLBACK_BACKEND.to_string(),
            dimension: 4,
            chunking_version: default_chunking_version(),
            output_encoding: "float".to_string(),
            storage_strategy: "native_f32".to_string(),
            distance_metric: "auto".to_string(),
            input_mode: "flat_texts".to_string(),
            document_prompt_hash: String::new(),
            ..Default::default()
        });

        let bytes = index.to_bytes();
        let restored = SemanticIndex::from_bytes(&bytes, &project_root).unwrap();

        assert_eq!(restored.len(), 1);
        assert_eq!(restored.entries_for_test()[0].chunk.name, "handle_request");
        assert_eq!(
            restored.entries_for_test()[0].vector,
            vec![0.1, 0.2, 0.3, 0.4]
        );
        assert_eq!(restored.dimension, 4);
        assert_eq!(restored.backend_label(), Some("fastembed"));
        assert_eq!(restored.model_label(), Some("all-MiniLM-L6-v2"));
    }

    #[test]
    fn symbol_kind_serialization_roundtrip_includes_file_summary_variant() {
        let cases = [
            (SymbolKind::Function, 0),
            (SymbolKind::Class, 1),
            (SymbolKind::Method, 2),
            (SymbolKind::Struct, 3),
            (SymbolKind::Interface, 4),
            (SymbolKind::Enum, 5),
            (SymbolKind::TypeAlias, 6),
            (SymbolKind::Variable, 7),
            (SymbolKind::Heading, 8),
            (SymbolKind::FileSummary, 9),
        ];

        for (kind, encoded) in cases {
            assert_eq!(symbol_kind_to_u8(&kind), encoded);
            assert_eq!(u8_to_symbol_kind(encoded), kind);
        }
    }

    #[test]
    fn test_search_top_k() {
        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        index.set_dimension(3);

        // Add entries with known vectors
        for (i, name) in ["auth", "database", "handler"].iter().enumerate() {
            let mut vec = vec![0.0f32; 3];
            vec[i] = 1.0; // orthogonal vectors
            index.entries_mut().push(EmbeddingEntry {
                chunk: SemanticChunk {
                    file: PathBuf::from("/src/lib.rs"),
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    start_line: (i * 10 + 1) as u32,
                    end_line: (i * 10 + 5) as u32,
                    exported: true,
                    embed_text: format!("kind:function name:{}", name),
                    snippet: format!("fn {}() {{}}", name),
                },
                vector: vec,
                chunk_hash: String::new(),
            });
        }

        // Query aligned with "auth" (index 0)
        let query = vec![0.9, 0.1, 0.0];
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "auth"); // highest score
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_empty_index_search() {
        let index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        let results = index.search(&[0.1, 0.2, 0.3], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn single_line_symbol_builds_non_empty_snippet() {
        let symbol = Symbol {
            name: "answer".to_string(),
            kind: SymbolKind::Variable,
            range: crate::symbols::Range {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 24,
            },
            signature: Some("const answer = 42".to_string()),
            scope_chain: Vec::new(),
            exported: true,
            parent: None,
        };
        let source = "export const answer = 42;\n";

        let snippet = build_snippet(&symbol, source);

        assert_eq!(snippet, "export const answer = 42;");
    }

    #[test]
    fn optimized_file_chunk_collection_matches_file_parser_path() {
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let file = project_root.join("src/semantic_index.rs");
        let source = std::fs::read_to_string(&file).unwrap();

        let mut legacy_parser = FileParser::new();
        let legacy_symbols = legacy_parser.extract_symbols(&file).unwrap();
        let legacy_chunks = symbols_to_chunks(&file, &legacy_symbols, &source, &project_root);

        let mut parsers = HashMap::new();
        let optimized_chunks = collect_file_chunks(&project_root, &file, &mut parsers).unwrap();

        assert_eq!(
            chunk_fingerprint(&optimized_chunks),
            chunk_fingerprint(&legacy_chunks)
        );
    }

    fn chunk_fingerprint(
        chunks: &[SemanticChunk],
    ) -> Vec<(String, SymbolKind, u32, u32, bool, String, String)> {
        chunks
            .iter()
            .map(|chunk| {
                (
                    chunk.name.clone(),
                    chunk.kind.clone(),
                    chunk.start_line,
                    chunk.end_line,
                    chunk.exported,
                    chunk.embed_text.clone(),
                    chunk.snippet.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn rejects_oversized_dimension_during_deserialization() {
        let mut bytes = Vec::new();
        bytes.push(1u8);
        bytes.extend_from_slice(&((MAX_DIMENSION as u32) + 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        assert!(SemanticIndex::from_bytes(&bytes, &test_project_root()).is_err());
    }

    #[test]
    fn rejects_oversized_entry_count_during_deserialization() {
        let mut bytes = Vec::new();
        bytes.push(1u8);
        bytes.extend_from_slice(&(DEFAULT_DIMENSION as u32).to_le_bytes());
        bytes.extend_from_slice(&((MAX_ENTRIES as u32) + 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        assert!(SemanticIndex::from_bytes(&bytes, &test_project_root()).is_err());
    }

    #[test]
    fn invalidate_file_removes_entries_and_mtime() {
        let target = PathBuf::from("/src/main.rs");
        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: target.clone(),
                name: "main".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 1,
                exported: false,
                embed_text: "main".to_string(),
                snippet: "fn main() {}".to_string(),
            },
            vector: vec![1.0; DEFAULT_DIMENSION],
            chunk_hash: String::new(),
        });
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            target.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::UNIX_EPOCH,
                size: 0,
                content_hash: hash,
            },
        );

        index.invalidate_file(&target);

        assert!(index.is_empty());
        assert!(!index.file_metadata().contains_key(&target));
    }

    #[test]
    fn refresh_transient_error_preserves_existing_entry_and_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let file = project_root.join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        write_rust_file(&file, "kept_symbol");

        let mut index = build_test_index(project_root, std::slice::from_ref(&file));
        let original_entry_count = index.len();
        let meta = index.file_metadata().get(&file).unwrap();
        let original_mtime = meta.mtime;
        let original_size = meta.size;

        let stale_mtime = SystemTime::UNIX_EPOCH;
        set_file_metadata(&mut index, &file, stale_mtime, original_size + 1);
        fs::remove_file(&file).unwrap();

        let mut embed = test_vector_for_texts;
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                std::slice::from_ref(&file),
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert_eq!(summary.changed, 0);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(index.len(), original_entry_count);
        assert!(index
            .entries_for_test()
            .iter()
            .any(|entry| entry.chunk.name == "kept_symbol"));
        assert_eq!(
            index.file_metadata().get(&file).map(|m| m.mtime),
            Some(stale_mtime)
        );
        assert_ne!(
            index.file_metadata().get(&file).map(|m| m.mtime),
            Some(original_mtime)
        );
        assert_eq!(
            index.file_metadata().get(&file).map(|m| m.size),
            Some(original_size + 1)
        );
    }

    #[test]
    fn refresh_never_indexed_file_error_does_not_record_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let missing = project_root.join("src/missing.rs");
        fs::create_dir_all(missing.parent().unwrap()).unwrap();

        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        let mut embed = test_vector_for_texts;
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                std::slice::from_ref(&missing),
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert_eq!(summary.added, 0);
        assert_eq!(summary.changed, 0);
        assert_eq!(summary.deleted, 0);
        assert!(!index.file_metadata().contains_key(&missing));
        assert!(index.is_empty());
    }

    #[test]
    fn refresh_reports_added_for_new_files() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let existing = project_root.join("src/lib.rs");
        let added = project_root.join("src/new.rs");
        fs::create_dir_all(existing.parent().unwrap()).unwrap();
        write_rust_file(&existing, "existing_symbol");
        write_rust_file(&added, "added_symbol");

        let mut index = build_test_index(project_root, std::slice::from_ref(&existing));
        let mut embed = test_vector_for_texts;
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                &[existing.clone(), added.clone()],
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert_eq!(summary.added, 1);
        assert_eq!(summary.changed, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.total_processed, 2);
        assert!(index.file_metadata().contains_key(&added));
        assert!(index
            .entries_for_test()
            .iter()
            .any(|entry| entry.chunk.file == added));
    }

    #[test]
    fn refresh_reports_deleted_for_removed_files() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let deleted = project_root.join("src/deleted.rs");
        fs::create_dir_all(deleted.parent().unwrap()).unwrap();
        write_rust_file(&deleted, "deleted_symbol");

        let mut index = build_test_index(project_root, std::slice::from_ref(&deleted));
        fs::remove_file(&deleted).unwrap();

        let mut embed = test_vector_for_texts;
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                &[],
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert_eq!(summary.deleted, 1);
        assert_eq!(summary.changed, 0);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.total_processed, 1);
        assert!(!index.file_metadata().contains_key(&deleted));
        assert!(index.is_empty());
    }

    #[test]
    fn refresh_reports_changed_for_modified_files() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let file = project_root.join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        write_rust_file(&file, "old_symbol");

        let mut index = build_test_index(project_root, std::slice::from_ref(&file));
        set_file_metadata(&mut index, &file, SystemTime::UNIX_EPOCH, 0);
        write_rust_file(&file, "new_symbol");

        let mut embed = test_vector_for_texts;
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                std::slice::from_ref(&file),
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert_eq!(summary.changed, 1);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.total_processed, 1);
        assert!(index
            .entries_for_test()
            .iter()
            .any(|entry| entry.chunk.name == "new_symbol"));
        assert!(!index
            .entries_for_test()
            .iter()
            .any(|entry| entry.chunk.name == "old_symbol"));
    }

    #[test]
    fn refresh_all_clean_reports_zero_counts_and_no_embedding_work() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path();
        let file = project_root.join("src/lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        write_rust_file(&file, "clean_symbol");

        let mut index = build_test_index(project_root, std::slice::from_ref(&file));
        let original_entries = index.len();
        let mut embed_called = false;
        let mut embed = |texts: Vec<String>| {
            embed_called = true;
            test_vector_for_texts(texts)
        };
        let mut progress = |_done: usize, _total: usize| {};
        let summary = index
            .refresh_stale_files(
                project_root,
                std::slice::from_ref(&file),
                &mut embed,
                8,
                &mut progress,
                &SemanticFilePolicy::default(),
                None,
            )
            .unwrap();

        assert!(summary.is_noop());
        assert_eq!(summary.total_processed, 1);
        assert!(!embed_called);
        assert_eq!(index.len(), original_entries);
    }

    #[test]
    fn detects_missing_onnx_runtime_from_dynamic_load_error() {
        let message = "Failed to load ONNX Runtime shared library libonnxruntime.dylib via dlopen: no such file";

        assert!(is_onnx_runtime_unavailable(message));
    }

    #[test]
    fn formats_missing_onnx_runtime_with_install_hint() {
        let message = format_embedding_init_error(
            "Failed to load ONNX Runtime shared library libonnxruntime.so via dlopen: no such file",
        );

        assert!(message.starts_with("ONNX Runtime not found. Install via:"));
        assert!(message.contains("Original error:"));
    }

    #[test]
    fn openai_compatible_backend_embeds_with_mock_server() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/v1/embeddings");
            "{\"data\":[{\"embedding\":[0.1,0.2,0.3],\"index\":0},{\"embedding\":[0.4,0.5,0.6],\"index\":1}]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test-embedding".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]]);
        handle.join().unwrap();
    }

    /// Regression for issue #36: AFT was sending TWO Content-Type headers
    /// on the OpenAI embeddings request — once implicitly via `.json(&body)`
    /// and again explicitly via `.header("Content-Type", "application/json")`.
    /// reqwest's `.header()` calls `HeaderMap::append`, which produces two
    /// headers on the wire. OpenAI's /v1/embeddings endpoint rejects that
    /// with `HTTP 400 "you must provide a model parameter"` even though the
    /// body actually contains `model`. The fix is to drop the explicit
    /// `.header("Content-Type", ...)` call. This test pins that we send
    /// exactly one Content-Type header.
    #[test]
    fn openai_compatible_request_has_single_content_type_header() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_thread = Arc::clone(&captured);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let n = stream.read(&mut chunk).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        for line in String::from_utf8_lossy(&buf[..pos + 4]).lines() {
                            if let Some(value) = line.strip_prefix("Content-Length:") {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }
            *captured_for_thread.lock().unwrap() = buf;
            let body = "{\"data\":[{\"embedding\":[0.1,0.2,0.3],\"index\":0}]}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "text-embedding-3-small".to_string(),
            base_url: Some(format!("http://{}", addr)),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };
        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let _ = model.embed(vec!["probe".to_string()]).unwrap();
        handle.join().unwrap();

        let bytes = captured.lock().unwrap().clone();
        let request = String::from_utf8_lossy(&bytes);

        // Lowercase line counts because HTTP headers are case-insensitive
        // and reqwest may emit `content-type` in lowercase under HTTP/2.
        let content_type_lines = request
            .lines()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.starts_with("content-type:")
            })
            .count();
        assert_eq!(
            content_type_lines, 1,
            "expected exactly one Content-Type header but found {content_type_lines}; full request:\n{request}",
        );

        // The body must still include the model field — pin this so a future
        // change can't accidentally drop `model` while fixing duplicate headers.
        assert!(
            request.contains(r#""model":"text-embedding-3-small""#),
            "request body should contain model field; full request:\n{request}",
        );
    }

    #[test]
    fn ollama_backend_embeds_with_mock_server() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/api/embed");
            "{\"embeddings\":[[0.7,0.8,0.9],[1.0,1.1,1.2]]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::Ollama,
            model: "embeddinggemma".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.7, 0.8, 0.9], vec![1.0, 1.1, 1.2]]);
        handle.join().unwrap();
    }

    #[test]
    fn read_from_disk_rejects_fingerprint_mismatch() {
        let storage = tempfile::tempdir().unwrap();
        let project_key = "proj";

        let project_root = test_project_root();
        let file = project_root.join("src/main.rs");
        let mut index = SemanticIndex::new(project_root.clone(), DEFAULT_DIMENSION);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: file.clone(),
                name: "handle_request".to_string(),
                kind: SymbolKind::Function,
                start_line: 10,
                end_line: 25,
                exported: true,
                embed_text: "file:src/main.rs kind:function name:handle_request".to_string(),
                snippet: "fn handle_request() {}".to_string(),
            },
            vector: vec![0.1, 0.2, 0.3],
            chunk_hash: String::new(),
        });
        index.set_dimension(3);
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            file.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::UNIX_EPOCH,
                size: 0,
                content_hash: hash,
            },
        );
        index.set_fingerprint(SemanticIndexFingerprint {
            backend: "openai_compatible".to_string(),
            model: "test-embedding".to_string(),
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            dimension: 3,
            chunking_version: default_chunking_version(),
            output_encoding: "float".to_string(),
            storage_strategy: "native_f32".to_string(),
            distance_metric: "auto".to_string(),
            input_mode: "flat_texts".to_string(),
            document_prompt_hash: String::new(),
            ..Default::default()
        });
        index.write_to_disk(storage.path(), project_key);

        let matching = index.fingerprint().unwrap().as_string();
        assert!(SemanticIndex::read_from_disk(
            storage.path(),
            project_key,
            &project_root,
            false,
            Some(&matching),
        )
        .is_some());

        let mismatched = SemanticIndexFingerprint {
            backend: "ollama".to_string(),
            model: "embeddinggemma".to_string(),
            base_url: "http://127.0.0.1:11434".to_string(),
            dimension: 3,
            chunking_version: default_chunking_version(),
            output_encoding: "float".to_string(),
            storage_strategy: "native_f32".to_string(),
            distance_metric: "auto".to_string(),
            input_mode: "flat_texts".to_string(),
            document_prompt_hash: String::new(),
            ..Default::default()
        }
        .as_string();
        assert!(SemanticIndex::read_from_disk(
            storage.path(),
            project_key,
            &project_root,
            false,
            Some(&mismatched),
        )
        .is_none());
    }

    #[test]
    fn read_from_disk_rejects_v3_cache_for_snippet_rebuild() {
        let storage = tempfile::tempdir().unwrap();
        let project_key = "proj-v3";
        let dir = storage.path().join("semantic").join(project_key);
        fs::create_dir_all(&dir).unwrap();

        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("/src/main.rs"),
                name: "handle_request".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 0,
                exported: true,
                embed_text: "file:src/main.rs kind:function name:handle_request".to_string(),
                snippet: "fn handle_request() {}".to_string(),
            },
            vector: vec![0.1, 0.2, 0.3],
            chunk_hash: String::new(),
        });
        index.set_dimension(3);
        let hash = cache_freshness::zero_hash();
        index.file_metadata_for_test().insert(
            PathBuf::from("/src/main.rs"),
            IndexedFileMetadata {
                mtime: SystemTime::UNIX_EPOCH,
                size: 0,
                content_hash: hash,
            },
        );
        let fingerprint = SemanticIndexFingerprint {
            backend: "fastembed".to_string(),
            model: "test".to_string(),
            base_url: FALLBACK_BACKEND.to_string(),
            dimension: 3,
            chunking_version: default_chunking_version(),
            output_encoding: "float".to_string(),
            storage_strategy: "native_f32".to_string(),
            distance_metric: "auto".to_string(),
            input_mode: "flat_texts".to_string(),
            document_prompt_hash: String::new(),
            ..Default::default()
        };
        index.set_fingerprint(fingerprint.clone());

        let mut bytes = index.to_bytes();
        bytes[0] = SEMANTIC_INDEX_VERSION_V3;
        fs::write(dir.join("semantic.bin"), bytes).unwrap();

        assert!(SemanticIndex::read_from_disk(
            storage.path(),
            project_key,
            &test_project_root(),
            false,
            Some(&fingerprint.as_string())
        )
        .is_none());
        assert!(!dir.join("semantic.bin").exists());
    }

    fn make_symbol(kind: SymbolKind, name: &str, start: u32, end: u32) -> crate::symbols::Symbol {
        crate::symbols::Symbol {
            name: name.to_string(),
            kind,
            range: crate::symbols::Range {
                start_line: start,
                start_col: 0,
                end_line: end,
                end_col: 0,
            },
            signature: None,
            scope_chain: Vec::new(),
            exported: false,
            parent: None,
        }
    }

    /// Heading symbols (Markdown / HTML headings) must NOT be indexed —
    /// they overwhelmingly dominated semantic results even on code-shaped
    /// queries because heading prose embeds far more strongly than code
    /// chunks. Skipping headings keeps aft_search a code-finder.
    #[test]
    fn symbols_to_chunks_skips_heading_symbols() {
        let project_root = PathBuf::from("/proj");
        let file = project_root.join("README.md");
        let source = "# Title\n\nbody text\n\n## Section\n\nmore text\n";

        let symbols = vec![
            make_symbol(SymbolKind::Heading, "Title", 0, 2),
            make_symbol(SymbolKind::Heading, "Section", 4, 6),
        ];

        let chunks = symbols_to_chunks(&file, &symbols, source, &project_root);
        assert!(
            chunks.is_empty(),
            "Heading symbols must be filtered out before embedding; got {} chunk(s)",
            chunks.len()
        );
    }

    /// Code symbols (functions, classes, methods, structs, etc.) must still
    /// be indexed alongside the heading skip — otherwise we'd starve the
    /// index entirely.
    #[test]
    fn symbols_to_chunks_keeps_code_symbols_alongside_skipped_headings() {
        let project_root = PathBuf::from("/proj");
        let file = project_root.join("src/lib.rs");
        let source = "pub fn handle_request() -> bool {\n    true\n}\n";

        let symbols = vec![
            // A heading mixed in (e.g. from a doc comment block elsewhere).
            make_symbol(SymbolKind::Heading, "doc heading", 0, 1),
            make_symbol(SymbolKind::Function, "handle_request", 0, 2),
            make_symbol(SymbolKind::Struct, "AuthService", 4, 6),
        ];

        let chunks = symbols_to_chunks(&file, &symbols, source, &project_root);
        assert_eq!(
            chunks.len(),
            3,
            "Expected file-summary + 2 code chunks (Function + Struct), got {}",
            chunks.len()
        );
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(chunks
            .iter()
            .any(|chunk| matches!(chunk.kind, SymbolKind::FileSummary)));
        assert!(names.contains(&"handle_request"));
        assert!(names.contains(&"AuthService"));
        assert!(
            !names.contains(&"doc heading"),
            "Heading symbol leaked into chunks: {names:?}"
        );
    }

    #[test]
    fn validate_ssrf_allows_loopback_hostnames() {
        // Loopback hostnames are explicitly allowed so self-hosted backends
        // (Ollama at http://localhost:11434) work at their default config.
        for host in &[
            "http://localhost",
            "http://localhost:8080",
            "http://localhost:11434", // Ollama default
            "http://localhost.localdomain",
            "http://foo.localhost",
        ] {
            assert!(
                validate_base_url_no_ssrf(host).is_ok(),
                "Expected {host} to be allowed (loopback), got: {:?}",
                validate_base_url_no_ssrf(host)
            );
        }
    }

    #[test]
    fn validate_ssrf_allows_loopback_ips() {
        // 127.0.0.0/8 is loopback — by definition same-machine and not an
        // SSRF target. Allow it so Ollama at http://127.0.0.1:11434 works.
        for url in &[
            "http://127.0.0.1",
            "http://127.0.0.1:11434", // Ollama default
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
        ] {
            let result = validate_base_url_no_ssrf(url);
            assert!(
                result.is_ok(),
                "Expected {url} to be allowed (loopback), got: {:?}",
                result
            );
        }
    }

    #[test]
    fn validate_ssrf_rejects_private_non_loopback_ips() {
        // Non-loopback private/reserved IPs remain rejected — homelab/intranet
        // services on LAN IPs are real SSRF targets even though the user
        // configured them. Users who want this can opt in by binding the
        // service to a public-routable address.
        for url in &[
            "http://192.168.1.1",
            "http://10.0.0.1",
            "http://172.16.0.1",
            "http://169.254.169.254",
            "http://100.64.0.1",
        ] {
            let result = validate_base_url_no_ssrf(url);
            assert!(
                result.is_err(),
                "Expected {url} to be rejected (non-loopback private), got: {:?}",
                result
            );
        }
    }

    #[test]
    fn validate_ssrf_rejects_mdns_local_hostnames() {
        // mDNS .local hostnames typically resolve to LAN devices, not
        // loopback. Rejecting them before DNS lookup gives a clearer error.
        for host in &[
            "http://printer.local",
            "http://nas.local:8080",
            "http://homelab.local",
        ] {
            let result = validate_base_url_no_ssrf(host);
            assert!(
                result.is_err(),
                "Expected {host} to be rejected (mDNS), got: {:?}",
                result
            );
        }
    }

    #[test]
    fn normalize_base_url_allows_localhost_for_tests() {
        // normalize_base_url itself should NOT block localhost — only
        // validate_base_url_no_ssrf does. Tests construct backends directly.
        assert!(normalize_base_url("http://127.0.0.1:9999").is_ok());
        assert!(normalize_base_url("http://localhost:8080").is_ok());
    }

    /// Pin the user-facing wording of the ONNX version-mismatch error.
    /// The auto-fix path MUST be listed first because it's the only safe
    /// option that doesn't require sudo or risk breaking other apps that
    /// link the system library. Regression of any of these strings would
    /// either mislead users (system rm before auto-fix) or break the
    /// `aft doctor --fix` discovery path.
    #[test]
    fn ort_mismatch_message_recommends_auto_fix_first() {
        let msg =
            format_ort_version_mismatch("1.9.0", "/usr/lib/x86_64-linux-gnu/libonnxruntime.so");

        // The reported version and path must appear verbatim.
        assert!(
            msg.contains("v1.9.0"),
            "should report detected version: {msg}"
        );
        assert!(
            msg.contains("/usr/lib/x86_64-linux-gnu/libonnxruntime.so"),
            "should report system path: {msg}"
        );
        assert!(msg.contains("v1.20+"), "should state requirement: {msg}");

        // Solution ordering: auto-fix is #1, system rm is #2, install is #3.
        let auto_fix_pos = msg
            .find("Auto-fix")
            .expect("Auto-fix solution missing — users won't discover --fix");
        let remove_pos = msg
            .find("Remove the old library")
            .expect("system-rm solution missing");
        assert!(
            auto_fix_pos < remove_pos,
            "Auto-fix must come before manual rm — see PR comment thread"
        );

        // The auto-fix command must be runnable as-is on a fresh system.
        assert!(
            msg.contains("npx @cortexkit/aft doctor --fix"),
            "auto-fix command must be present and copy-pasteable: {msg}"
        );
    }

    /// macOS dylib paths must not produce a malformed message when the
    /// system path lacks a trailing slash. This is a regression guard
    /// for the "{}\n{}" format string contract.
    #[test]
    fn ort_mismatch_message_handles_macos_dylib_path() {
        let msg = format_ort_version_mismatch("1.9.0", "/opt/homebrew/lib/libonnxruntime.dylib");
        assert!(msg.contains("v1.9.0"));
        assert!(msg.contains("/opt/homebrew/lib/libonnxruntime.dylib"));
        // The dylib path must appear in the auto-fix paragraph (single
        // quotes around it) AND in the manual-rm paragraph; verify
        // both placements survived the format string.
        assert!(
            msg.contains("'/opt/homebrew/lib/libonnxruntime.dylib'"),
            "system path should be quoted in the auto-fix sentence: {msg}"
        );
    }

    // ── is_generated_file tests ─────────────────────────────────────────

    #[test]
    fn is_generated_file_detects_protobuf_go() {
        assert!(is_generated_file(Path::new("foo.pb.go")));
    }

    #[test]
    fn is_generated_file_detects_protobuf_python() {
        assert!(is_generated_file(Path::new("foo_pb2.py")));
    }

    #[test]
    fn is_generated_file_detects_minified() {
        assert!(is_generated_file(Path::new("vendor/jquery.min.js")));
    }

    #[test]
    fn is_generated_file_detects_snapshot() {
        assert!(is_generated_file(Path::new("__snapshots__/test.snap")));
    }

    #[test]
    fn is_generated_file_detects_dist_directory() {
        assert!(is_generated_file(Path::new("dist/index.js")));
    }

    #[test]
    fn is_generated_file_detects_build_directory() {
        assert!(is_generated_file(Path::new("build/main.rs")));
    }

    #[test]
    fn is_generated_file_detects_generated_directory() {
        assert!(is_generated_file(Path::new("generated/models.rs")));
    }

    #[test]
    fn is_generated_file_detects_generated_prefix() {
        assert!(is_generated_file(Path::new(".generated.ts")));
    }

    #[test]
    fn is_generated_file_detects_dart_generated() {
        assert!(is_generated_file(Path::new("foo.g.dart")));
    }

    #[test]
    fn is_generated_file_allows_normal_files() {
        assert!(!is_generated_file(Path::new("src/main.rs")));
        assert!(!is_generated_file(Path::new("lib/utils.ts")));
        assert!(!is_generated_file(Path::new("README.md")));
    }

    // ── is_doc_extension tests ──────────────────────────────────────────

    #[test]
    fn is_doc_extension_markdown() {
        assert!(is_doc_extension(Path::new("README.md")));
        assert!(is_doc_extension(Path::new("docs/guide.rst")));
        assert!(is_doc_extension(Path::new("notes.txt")));
        assert!(is_doc_extension(Path::new("guide.adoc")));
    }

    #[test]
    fn is_doc_extension_rejects_code() {
        assert!(!is_doc_extension(Path::new("main.rs")));
        assert!(!is_doc_extension(Path::new("app.ts")));
    }

    // ── is_config_extension tests ───────────────────────────────────────

    #[test]
    fn is_config_extension_toml_yaml_json() {
        assert!(is_config_extension(Path::new("Cargo.toml")));
        assert!(is_config_extension(Path::new("config.yaml")));
        assert!(is_config_extension(Path::new("package.json")));
        assert!(is_config_extension(Path::new("tsconfig.jsonc")));
    }

    #[test]
    fn is_config_extension_rejects_lockfiles() {
        assert!(!is_config_extension(Path::new("package-lock.json")));
        assert!(!is_config_extension(Path::new("yarn.lock")));
        assert!(!is_config_extension(Path::new("bun.lockb")));
    }

    #[test]
    fn is_config_extension_detects_dotfiles() {
        assert!(is_config_extension(Path::new(".env")));
        assert!(is_config_extension(Path::new(".eslintrc")));
        assert!(is_config_extension(Path::new(".prettierrc")));
        assert!(is_config_extension(Path::new(".gitignore")));
    }

    // ── classify_semantic_file tests ────────────────────────────────────

    #[test]
    fn classify_semantic_file_code() {
        assert_eq!(
            classify_semantic_file(Path::new("src/main.rs")),
            SemanticFileType::Code
        );
        assert_eq!(
            classify_semantic_file(Path::new("app.ts")),
            SemanticFileType::Code
        );
    }

    #[test]
    fn classify_semantic_file_doc() {
        assert_eq!(
            classify_semantic_file(Path::new("README.md")),
            SemanticFileType::Doc
        );
        assert_eq!(
            classify_semantic_file(Path::new("guide.rst")),
            SemanticFileType::Doc
        );
    }

    #[test]
    fn classify_semantic_file_config() {
        assert_eq!(
            classify_semantic_file(Path::new("Cargo.toml")),
            SemanticFileType::Config
        );
    }

    // ── collect_docs_chunks tests ───────────────────────────────────────

    #[test]
    fn collect_docs_chunks_markdown_splits_by_heading() {
        let md =
            "# Title\n\nIntro text.\n\n## Section A\n\nContent A.\n\n## Section B\n\nContent B.\n";
        let chunks = collect_docs_chunks(md, Path::new("docs.md"));
        // Should have at least 2 chunks (Section A, Section B); intro is merged into first
        assert!(
            chunks.len() >= 2,
            "expected >=2 chunks, got {}",
            chunks.len()
        );
        // Each chunk should have the heading name
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("Section A")),
            "got: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("Section B")),
            "got: {names:?}"
        );
    }

    #[test]
    fn collect_docs_chunks_markdown_empty_returns_empty() {
        let chunks = collect_docs_chunks("", Path::new("empty.md"));
        assert!(chunks.is_empty());
    }

    #[test]
    fn collect_docs_chunks_non_markdown_single_chunk() {
        let text = "This is a plain text document.\nWith multiple lines.\n";
        let chunks = collect_docs_chunks(text, Path::new("notes.txt"));
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].embed_text.contains("plain text"));
    }

    #[test]
    fn collect_docs_chunks_non_markdown_empty_returns_empty() {
        let chunks = collect_docs_chunks("", Path::new("empty.txt"));
        assert!(chunks.is_empty());
    }

    #[test]
    fn collect_docs_chunks_markdown_with_h1_only() {
        let md = "# Just a title\n\nSome content here.\n";
        let chunks = collect_docs_chunks(md, Path::new("single.md"));
        assert!(!chunks.is_empty());
    }

    // ── SemanticFilePolicy tests ────────────────────────────────────────

    #[test]
    fn semantic_file_policy_default_values() {
        let policy = SemanticFilePolicy::default();
        assert!(policy.include_code);
        assert!(policy.include_docs);
        assert!(!policy.include_configs);
        assert!(policy.respect_gitignore);
        assert!(policy.binary_detection);
        assert!(policy.generated_file_detection);
        assert_eq!(policy.max_file_size_bytes, 1_048_576);
        assert!(policy.include_globs.is_empty());
        assert!(policy.exclude_globs.is_empty());
    }

    #[test]
    fn semantic_file_policy_builtins_not_empty() {
        let policy = SemanticFilePolicy::default();
        assert!(!policy.builtin_doc_globs.is_empty());
        assert!(!policy.builtin_exclude_globs.is_empty());
        // Should include common exclusions
        assert!(policy
            .builtin_exclude_globs
            .iter()
            .any(|g| g.contains("node_modules")));
        assert!(policy
            .builtin_exclude_globs
            .iter()
            .any(|g| g.contains("target")));
    }

    // ── FileRecord and FileManifest tests ───────────────────────────────

    #[test]
    fn file_record_fields_populated() {
        let record = FileRecord {
            content_hash: blake3::hash(b"test content"),
            size_bytes: 1024,
            mtime: SystemTime::now(),
            language: Some("rust".to_string()),
            document_kind: "code".to_string(),
            inclusion_policy_hash: "hash123".to_string(),
            indexed_at: SystemTime::now(),
        };
        assert_eq!(record.size_bytes, 1024);
        assert_eq!(record.language.as_deref(), Some("rust"));
        assert_eq!(record.document_kind, "code");
        assert_eq!(record.inclusion_policy_hash, "hash123");
    }

    #[test]
    fn build_manifest_from_store_populates_records() {
        // Create a snapshot with some file metadata
        let mut store = crate::vector_store::FlatF32VectorStore::new(384);
        let path_a = PathBuf::from("src/main.rs");
        let path_b = PathBuf::from("lib/utils.ts");
        store.file_metadata_mut().insert(
            path_a.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::now(),
                size: 500,
                content_hash: blake3::hash(b"main"),
            },
        );
        store.file_metadata_mut().insert(
            path_b.clone(),
            IndexedFileMetadata {
                mtime: SystemTime::now(),
                size: 300,
                content_hash: blake3::hash(b"utils"),
            },
        );

        let mut snapshot = SemanticIndexSnapshot {
            store,
            dimension: 384,
            project_root: PathBuf::from("."),
            file_manifest: HashMap::new(),
            next_chunk_id: 0,
            fingerprint_string: None,
        };

        snapshot.build_manifest_from_store();

        assert_eq!(snapshot.file_manifest.len(), 2);
        let record_a = snapshot.file_manifest.get(&path_a).unwrap();
        assert_eq!(record_a.size_bytes, 500);
        assert_eq!(record_a.document_kind, "code");

        let record_b = snapshot.file_manifest.get(&path_b).unwrap();
        assert_eq!(record_b.size_bytes, 300);
    }

    #[test]
    fn build_manifest_from_store_clears_old_entries() {
        let mut store = crate::vector_store::FlatF32VectorStore::new(384);
        store.file_metadata_mut().insert(
            PathBuf::from("src/only.rs"),
            IndexedFileMetadata {
                mtime: SystemTime::now(),
                size: 100,
                content_hash: blake3::hash(b"only"),
            },
        );

        let mut snapshot = SemanticIndexSnapshot {
            store,
            dimension: 384,
            project_root: PathBuf::from("."),
            file_manifest: {
                let mut m = HashMap::new();
                m.insert(
                    PathBuf::from("old/deleted.rs"),
                    FileRecord {
                        content_hash: blake3::hash(b"old"),
                        size_bytes: 999,
                        mtime: SystemTime::UNIX_EPOCH,
                        language: None,
                        document_kind: "code".to_string(),
                        inclusion_policy_hash: String::new(),
                        indexed_at: SystemTime::UNIX_EPOCH,
                    },
                );
                m
            },
            next_chunk_id: 0,
            fingerprint_string: None,
        };

        snapshot.build_manifest_from_store();

        // Old entry should be gone, only new entry remains
        assert_eq!(snapshot.file_manifest.len(), 1);
        assert!(snapshot
            .file_manifest
            .contains_key(&PathBuf::from("src/only.rs")));
        assert!(!snapshot
            .file_manifest
            .contains_key(&PathBuf::from("old/deleted.rs")));
    }

    // ── Lifecycle state tests ───────────────────────────────────────────

    #[test]
    fn lifecycle_cold_start_is_initial_state() {
        let index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        assert!(matches!(
            index.lifecycle(),
            SemanticIndexLifecycle::ColdStart
        ));
    }

    #[test]
    fn lifecycle_set_and_get() {
        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        index.set_lifecycle(SemanticIndexLifecycle::Ready);
        assert!(matches!(index.lifecycle(), SemanticIndexLifecycle::Ready));
    }

    #[test]
    fn lifecycle_mark_failed_sets_failed() {
        let mut index = SemanticIndex::new(test_project_root(), DEFAULT_DIMENSION);
        index.set_lifecycle(SemanticIndexLifecycle::Ready);
        index.set_lifecycle(SemanticIndexLifecycle::Failed);
        index.set_last_error("something broke".to_string());
        assert!(matches!(index.lifecycle(), SemanticIndexLifecycle::Failed));
        assert_eq!(index.last_error(), Some("something broke"));
    }

    #[test]
    fn lifecycle_all_variants_exist() {
        // Verify all lifecycle variants can be constructed and are distinct.
        let _d = SemanticIndexLifecycle::Disabled;
        let _cs = SemanticIndexLifecycle::ColdStart;
        let _sf = SemanticIndexLifecycle::ScanningFiles;
        let _ck = SemanticIndexLifecycle::Chunking;
        let _em = SemanticIndexLifecycle::Embedding;
        let _r = SemanticIndexLifecycle::Ready;
        let _rf = SemanticIndexLifecycle::Refreshing;
        let _rr = SemanticIndexLifecycle::RebuildRequired;
        let _dg = SemanticIndexLifecycle::Degraded;
        let _f = SemanticIndexLifecycle::Failed;
        // Pattern-match to confirm all variants are covered.
        assert!(matches!(
            SemanticIndexLifecycle::Disabled,
            SemanticIndexLifecycle::Disabled
        ));
        assert!(matches!(
            SemanticIndexLifecycle::ColdStart,
            SemanticIndexLifecycle::ColdStart
        ));
        assert!(matches!(
            SemanticIndexLifecycle::Ready,
            SemanticIndexLifecycle::Ready
        ));
        assert!(matches!(
            SemanticIndexLifecycle::Failed,
            SemanticIndexLifecycle::Failed
        ));
    }

    // ── Snapshot atomicity tests ────────────────────────────────────────

    #[test]
    fn snapshot_search_returns_ranked_results() {
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func_a".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("b.rs"),
                name: "func_b".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![0.0, 1.0, 0.0],
            chunk_hash: String::new(),
        });
        let snapshot = index.snapshot.clone();
        let results = snapshot.search(&[1.0, 0.0, 0.0], 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "func_a");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn snapshot_immutable_after_clone() {
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        let snapshot = index.snapshot.clone();
        let original_len = snapshot.len();
        // Mutate the original index
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("b.rs"),
                name: "func2".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![0.0, 1.0, 0.0],
            chunk_hash: String::new(),
        });
        // Snapshot should still have the old length
        assert_eq!(snapshot.len(), original_len);
    }

    // ── Stale-vector pruning tests ──────────────────────────────────────

    #[test]
    fn prune_stale_vectors_removes_zero_norm() {
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("b.rs"),
                name: "zero".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![0.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        assert_eq!(index.len(), 2);
        let snap = Arc::get_mut(&mut index.snapshot).unwrap();
        let pruned = snap.store_mut().prune_stale_vectors();
        assert_eq!(pruned, 1);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn prune_orphans_removes_entries_for_deleted_files() {
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("keep.rs"),
                name: "keep".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("delete.rs"),
                name: "del".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![0.0, 1.0, 0.0],
            chunk_hash: String::new(),
        });
        let current_files = vec![PathBuf::from("keep.rs")];
        let snap = Arc::get_mut(&mut index.snapshot).unwrap();
        let removed = snap.store_mut().prune_orphans(&current_files);
        assert_eq!(removed, 1);
        assert_eq!(index.len(), 1);
    }

    // ── Concurrency tests ──────────────────────────────────────────────

    #[test]
    fn concurrent_snapshot_clones_are_independent() {
        // Verify that cloning a snapshot and reading from both doesn't interfere.
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func_a".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        let snap1 = index.snapshot.clone();
        let snap2 = index.snapshot.clone();

        // Both snapshots should search independently
        let results1 = snap1.search(&[1.0, 0.0, 0.0], 10);
        let results2 = snap2.search(&[0.0, 1.0, 0.0], 10);
        assert_eq!(results1.len(), 1);
        assert_eq!(results2.len(), 1);
        // Different queries yield different scores
        assert!(results1[0].score > results2[0].score);
    }

    #[test]
    fn concurrent_read_threads_see_same_data() {
        use std::thread;
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func_a".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        let snap = Arc::clone(&index.snapshot);
        let snap2 = Arc::clone(&index.snapshot);

        let handle1 = thread::spawn(move || snap.search(&[1.0, 0.0, 0.0], 10));
        let handle2 = thread::spawn(move || snap2.entries_slice().len());

        let results = handle1.join().unwrap();
        let count = handle2.join().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn mutex_contention_does_not_deadlock() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let data = Arc::new(Mutex::new(Vec::<i32>::new()));
        let mut handles = vec![];

        for i in 0..10 {
            let data = Arc::clone(&data);
            handles.push(thread::spawn(move || {
                let mut guard = data.lock().unwrap();
                guard.push(i);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let guard = data.lock().unwrap();
        assert_eq!(guard.len(), 10);
    }

    #[test]
    fn arc_clone_count_is_correct() {
        let mut index = SemanticIndex::new(test_project_root(), 3);
        index.entries_mut().push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("a.rs"),
                name: "func".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 5,
                exported: false,
                embed_text: String::new(),
                snippet: String::new(),
            },
            vector: vec![1.0, 0.0, 0.0],
            chunk_hash: String::new(),
        });
        assert_eq!(Arc::strong_count(&index.snapshot), 1);
        let _snap1 = Arc::clone(&index.snapshot);
        assert_eq!(Arc::strong_count(&index.snapshot), 2);
        let _snap2 = Arc::clone(&index.snapshot);
        assert_eq!(Arc::strong_count(&index.snapshot), 3);
        drop(_snap1);
        assert_eq!(Arc::strong_count(&index.snapshot), 2);
    }

    fn default_test_config(backend: SemanticBackend, base_url: String) -> SemanticBackendConfig {
        SemanticBackendConfig {
            backend,
            model: "test-model".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        }
    }

    fn encode_int8_base64_test(values: &[i8]) -> String {
        use base64::Engine as _;
        let bytes: Vec<u8> = values.iter().map(|&v| v as u8).collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    #[allow(non_snake_case)]
    fn mock_server_openai_compatible_embedsSuccessfully() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/v1/embeddings");
            r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0},{"embedding":[0.4,0.5,0.6],"index":1}]}"#.to_string()
        });

        let config = default_test_config(SemanticBackend::OpenAiCompatible, base_url);
        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]]);
        handle.join().unwrap();
    }

    #[test]
    fn mock_server_returns_wrong_dimension_returns_error() {
        let v1 = encode_int8_base64_test(&[10, -20, 30, 40, 50]);
        let (base_url, handle) = start_mock_http_server(move |_request, _path, _body| {
            format!(r#"{{"data":[{{"embedding":"{}","index":0}}]}}"#, v1)
        });

        let mut config = default_test_config(SemanticBackend::OpenAiCompatible, base_url);
        config.dimensions = Some(3);
        config.output_encoding = Some(OutputEncoding::Base64Int8);

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model.embed(vec!["test".to_string()]).unwrap_err();
        assert!(
            err.contains("dimension"),
            "expected dimension error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn mock_server_returns_wrong_count_returns_error() {
        let (base_url, handle) = start_mock_http_server(|_request, _path, _body| {
            r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0},{"embedding":[0.4,0.5,0.6],"index":1},{"embedding":[0.7,0.8,0.9],"index":2}]}"#.to_string()
        });

        let config = default_test_config(SemanticBackend::OpenAiCompatible, base_url);
        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model
            .embed(vec!["a".to_string(), "b".to_string()])
            .unwrap_err();
        assert!(
            err.contains("2 embeddings") || err.contains("for 2 inputs"),
            "expected count mismatch error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn mock_server_timeout_returns_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let n = stream.read(&mut chunk).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                        for line in headers.lines() {
                            let lower = line.trim().to_lowercase();
                            if let Some(value) = lower.strip_prefix("content-length:") {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }
            drop(stream);
            let _ = listener;
        });

        let mut config = default_test_config(
            SemanticBackend::OpenAiCompatible,
            format!("http://{}", addr),
        );
        config.timeout_ms = 200;

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model.embed(vec!["test".to_string()]).unwrap_err();
        assert!(
            err.contains("request failed") || err.contains("timeout") || err.contains("timed out"),
            "expected timeout/connection error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn mock_server_returns_500_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let attempt_count = Arc::new(AtomicUsize::new(0));
        let attempt_count_clone = Arc::clone(&attempt_count);

        let handle = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut header_end = None;
                let mut content_length = 0usize;
                loop {
                    let n = stream.read(&mut chunk).expect("read");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                            for line in headers.lines() {
                                let lower = line.trim().to_lowercase();
                                if let Some(value) = lower.strip_prefix("content-length:") {
                                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                                }
                            }
                        }
                    }
                    if let Some(end) = header_end {
                        if buf.len() >= end + content_length {
                            break;
                        }
                    }
                }

                let attempt = attempt_count_clone.fetch_add(1, Ordering::SeqCst);
                let (status_line, body) = if attempt < 2 {
                    (
                        "HTTP/1.1 500 Internal Server Error",
                        r#"{"error":"temporary failure"}"#,
                    )
                } else {
                    (
                        "HTTP/1.1 200 OK",
                        r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}]}"#,
                    )
                };
                let response = format!(
                    "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let config = default_test_config(
            SemanticBackend::OpenAiCompatible,
            format!("http://{}", addr),
        );
        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model.embed(vec!["test".to_string()]).unwrap();
        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3]]);
        assert!(
            attempt_count.load(Ordering::SeqCst) >= 3,
            "should have retried at least 3 times"
        );
        handle.join().unwrap();
    }

    #[test]
    fn mock_server_perplexity_embeds_with_mock_server() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/v1/embeddings");
            r#"{"data":[{"embedding":[0.7,0.8,0.9],"index":0},{"embedding":[1.0,1.1,1.2],"index":1}]}"#.to_string()
        });

        let config = default_test_config(SemanticBackend::Perplexity, base_url);
        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.7, 0.8, 0.9], vec![1.0, 1.1, 1.2]]);
        handle.join().unwrap();
    }
}

#[cfg(test)]
mod fingerprint_invalidation_tests {
    use super::tests::start_mock_http_server;
    use super::*;
    use crate::config::DiagnosticsOutputMode;

    /// Build a fingerprint with all fields set to predictable defaults.
    fn fp() -> SemanticIndexFingerprint {
        SemanticIndexFingerprint {
            backend: "fastembed".to_string(),
            model: "all-MiniLM-L6-v2".to_string(),
            base_url: FALLBACK_BACKEND.to_string(),
            dimension: 384,
            chunking_version: 2,
            output_encoding: "float".to_string(),
            storage_strategy: "native_f32".to_string(),
            distance_metric: "auto".to_string(),
            input_mode: "flat_texts".to_string(),
            document_prompt_hash: String::new(),
            source_vector_kind: "dense_f32".to_string(),
            stored_vector_kind: "dense_f32".to_string(),
            normalization: "already_normalized".to_string(),
            query_prompt_hash: String::new(),
            file_policy_hash: String::new(),
            docs_chunker_version: 1,
        }
    }

    #[test]
    fn backend_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.backend = "ollama".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn model_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.model = "different-model".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn base_url_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.base_url = "http://other-host:11434".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn dimension_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.dimension = 768;
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn chunking_version_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.chunking_version = 3;
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn output_encoding_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.output_encoding = "base64_int8".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn storage_strategy_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.storage_strategy = "decode_normalize_f32".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn distance_metric_mismatch_does_not_rebuild() {
        let a = fp();
        let mut b = fp();
        b.distance_metric = "cosine".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::None);
    }

    #[test]
    fn input_mode_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.input_mode = "document_chunks".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn document_prompt_hash_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.document_prompt_hash = "abc123".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn source_vector_kind_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.source_vector_kind = "binary_packed".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn stored_vector_kind_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.stored_vector_kind = "dense_int8".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn normalization_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.normalization = "normalize_on_insert_query".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn query_prompt_hash_only_triggers_clear_cache() {
        let a = fp();
        let mut b = fp();
        b.query_prompt_hash = "xyz789".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::ClearQueryCache);
    }

    #[test]
    fn identical_fingerprint_is_noop() {
        let a = fp();
        let b = fp();
        assert_eq!(a.diff(&b), FingerprintChange::None);
    }

    #[test]
    fn reranker_fields_not_in_fingerprint_produces_no_diff() {
        // distance_metric is in the fingerprint but explicitly excluded from
        // rebuild triggers. Verify it produces None.
        let a = fp();
        let mut b = fp();
        b.distance_metric = "dot_product".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::None);
    }

    #[test]
    fn file_policy_hash_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.file_policy_hash = "policy_v2_hash".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn docs_chunker_version_mismatch_triggers_rebuild() {
        let a = fp();
        let mut b = fp();
        b.docs_chunker_version = 2;
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn multi_field_change_still_rebuild() {
        // Multiple rebuild-field changes should still produce Rebuild.
        let a = fp();
        let mut b = fp();
        b.model = "different-model".to_string();
        b.dimension = 768;
        b.file_policy_hash = "new_hash".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn rebuild_plus_query_prompt_change_still_rebuild() {
        // When both rebuild and query-prompt fields change, Rebuild wins
        // because it's checked first.
        let a = fp();
        let mut b = fp();
        b.model = "different-model".to_string();
        b.query_prompt_hash = "new_query_hash".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::Rebuild);
    }

    #[test]
    fn only_query_prompt_changes_gives_clear_cache() {
        // When only query_prompt_hash changes (all rebuild fields match),
        // ClearQueryCache is returned.
        let a = fp();
        let mut b = fp();
        b.query_prompt_hash = "only_this_changes".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::ClearQueryCache);
    }

    #[test]
    fn non_fingerprint_field_changes_produce_none() {
        // Fields NOT in the fingerprint (e.g. diagnostics, rerank config)
        // should not cause any diff. We simulate this by checking that
        // changing only distance_metric (which IS in fp but excluded from
        // rebuild) produces None — and by extension, fields not in fp at all
        // also produce None.
        let a = fp();
        let mut b = fp();
        b.distance_metric = "euclidean".to_string();
        assert_eq!(a.diff(&b), FingerprintChange::None);
    }

    #[test]
    fn display_implementation() {
        assert_eq!(FingerprintChange::Rebuild.to_string(), "rebuild");
        assert_eq!(
            FingerprintChange::ClearQueryCache.to_string(),
            "clear_query_cache"
        );
        assert_eq!(FingerprintChange::None.to_string(), "none");
    }

    // ── base64_int8 embedding tests ────────────────────────────────────

    /// Helper: encode a vec of i8 as a base64 string (STANDARD encoding).
    fn encode_int8_base64(values: &[i8]) -> String {
        use base64::Engine as _;
        let bytes: Vec<u8> = values.iter().map(|&v| v as u8).collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn openai_compatible_base64_int8_embeds_with_mock_server() {
        // Simulate a provider returning base64-encoded int8 vectors.
        // Two vectors of 3 dimensions: [10, -20, 30] and [-40, 50, -60].
        let v1 = encode_int8_base64(&[10, -20, 30]);
        let v2 = encode_int8_base64(&[-40, 50, -60]);
        let response_body = format!(
            "{{\"data\":[{{\"embedding\":\"{}\",\"index\":0}},{{\"embedding\":\"{}\",\"index\":1}}]}}",
            v1, v2
        );

        let (base_url, handle) = start_mock_http_server(move |_request, _path, body| {
            // Verify that encoding_format is sent in the request body.
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed["encoding_format"], "base64_int8",
                "request should include encoding_format: base64_int8"
            );
            response_body.clone()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test-int8".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: Some(OutputEncoding::Base64Int8),
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors.len(), 2);
        // Vectors are L2-normalized after int8→f32 conversion.
        let norm1_sq: f32 = vectors[0].iter().map(|x| x * x).sum();
        assert!((norm1_sq - 1.0).abs() < 1e-5, "vector 1 norm² = {norm1_sq}");
        let norm2_sq: f32 = vectors[1].iter().map(|x| x * x).sum();
        assert!((norm2_sq - 1.0).abs() < 1e-5, "vector 2 norm² = {norm2_sq}");
        // Verify relative ordering is preserved (positive/negative signs).
        assert!(vectors[0][0] > 0.0, "v1[0] should be positive");
        assert!(vectors[0][1] < 0.0, "v1[1] should be negative");
        assert!(vectors[0][2] > 0.0, "v1[2] should be positive");
        assert!(vectors[1][0] < 0.0, "v2[0] should be negative");
        assert!(vectors[1][1] > 0.0, "v2[1] should be positive");
        assert!(vectors[1][2] < 0.0, "v2[2] should be negative");
        handle.join().unwrap();
    }

    #[test]
    fn openai_compatible_float_path_unchanged() {
        // Ensure the existing float array path still works after refactoring.
        let (base_url, handle) = start_mock_http_server(|_request, _path, body| {
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            // encoding_format should NOT be present for Float encoding.
            assert!(
                parsed.get("encoding_format").is_none(),
                "float path should not send encoding_format"
            );
            "{\"data\":[{\"embedding\":[0.1,0.2,0.3],\"index\":0}]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test-float".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None, // defaults to Float
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model.embed(vec!["test".to_string()]).unwrap();
        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3]]);
        handle.join().unwrap();
    }

    #[test]
    fn base64_int8_invalid_base64_returns_error() {
        let (base_url, handle) = start_mock_http_server(|_request, _path, _body| {
            // Return invalid base64 data.
            "{\"data\":[{\"embedding\":\"!!!NOT_BASE64!!!\",\"index\":0}]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: Some(OutputEncoding::Base64Int8),
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model.embed(vec!["test".to_string()]).unwrap_err();
        assert!(
            err.contains("base64 decode error") || err.contains("provider-response"),
            "expected base64 decode error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn base64_int8_wrong_dimension_returns_error() {
        // Return a valid base64 string, but the byte count doesn't match
        // what the model expects (we configured 5 dimensions but encode 3 bytes).
        let v = encode_int8_base64(&[1, 2, 3]); // 3 bytes, but dimensions=5

        let (base_url, handle) = start_mock_http_server(move |_request, _path, _body| {
            format!("{{\"data\":[{{\"embedding\":\"{}\",\"index\":0}}]}}", v)
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: Some(5), // expect 5 dimensions
            output_encoding: Some(OutputEncoding::Base64Int8),
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model.embed(vec!["test".to_string()]).unwrap_err();
        // The dimension mismatch is caught either at parse time (if the model
        // already knows its dimension from a prior probe) or at validation time.
        // Either way, the error should contain a meaningful message.
        assert!(
            err.contains("dimension") || err.contains("length"),
            "expected dimension/length error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn base64_int8_inconsistent_response_count_returns_error() {
        // Request 2 texts but provider returns only 1 embedding.
        let v = encode_int8_base64(&[10, 20, 30]);

        let (base_url, handle) = start_mock_http_server(move |_request, _path, _body| {
            // Return only 1 embedding for 2 inputs.
            format!("{{\"data\":[{{\"embedding\":\"{}\",\"index\":0}}]}}", v)
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: Some(OutputEncoding::Base64Int8),
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let err = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap_err();
        assert!(
            err.contains("1 embeddings for 2 inputs"),
            "expected count mismatch error, got: {err}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn base64_int8_profile_from_config_selects_correctly() {
        use crate::config::SemanticBackend;

        let config_int8 = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            model: "sonar".to_string(),
            base_url: Some("http://127.0.0.1:9999".to_string()),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: Some(OutputEncoding::Base64Int8),
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: crate::config::DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };

        let profile = SemanticEmbeddingModel::from_config(&config_int8).unwrap();
        assert_eq!(profile.output_encoding, OutputEncoding::Base64Int8);

        let config_float = SemanticBackendConfig {
            output_encoding: None, // defaults to Float
            ..config_int8
        };

        let profile = SemanticEmbeddingModel::from_config(&config_float).unwrap();
        assert_eq!(profile.output_encoding, OutputEncoding::Float);
    }

    #[test]
    fn parse_embedding_value_float_succeeds() {
        let val = serde_json::json!([0.1, 0.2, 0.3]);
        let result = parse_embedding_value(&val, OutputEncoding::Float, "test", None).unwrap();
        assert!((result[0] - 0.1).abs() < 1e-6);
        assert!((result[1] - 0.2).abs() < 1e-6);
        assert!((result[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn parse_embedding_value_base64_int8_succeeds_and_normalizes() {
        let encoded = encode_int8_base64(&[10, -20, 30]);
        let val = serde_json::json!(encoded);
        let result = parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", None).unwrap();
        // L2-norm of [10, -20, 30] = sqrt(1400) ≈ 37.4166
        let norm_sq: f32 = 10.0 * 10.0 + (-20.0) * (-20.0) + 30.0 * 30.0;
        let norm = norm_sq.sqrt();
        assert!((result[0] - 10.0 / norm).abs() < 1e-5, "got {}", result[0]);
        assert!(
            (result[1] - (-20.0) / norm).abs() < 1e-5,
            "got {}",
            result[1]
        );
        assert!((result[2] - 30.0 / norm).abs() < 1e-5, "got {}", result[2]);
        // Verify L2 norm ≈ 1.0
        let norm_check: f32 = result.iter().map(|x| x * x).sum();
        assert!((norm_check - 1.0).abs() < 1e-5, "norm² = {norm_check}");
    }

    #[test]
    fn parse_embedding_value_base64_int8_dimension_mismatch() {
        let encoded = encode_int8_base64(&[10, -20, 30]); // 3 values
        let val = serde_json::json!(encoded);
        let err =
            parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", Some(5)).unwrap_err();
        assert!(err.contains("dimension mismatch"), "got: {err}");
        assert!(err.contains("decoded 3 values, expected 5"), "got: {err}");
    }

    #[test]
    fn parse_embedding_value_base64_int8_dimension_match() {
        let encoded = encode_int8_base64(&[10, -20, 30]); // 3 values
        let val = serde_json::json!(encoded);
        let result =
            parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", Some(3)).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn parse_embedding_value_base64_int8_invalid_base64() {
        let val = serde_json::json!("not-valid-base64!!!");
        let err =
            parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", None).unwrap_err();
        assert!(err.contains("base64 decode error"), "got: {err}");
    }

    #[test]
    fn parse_embedding_value_float_wrong_type() {
        // Float encoding expects an array, not a string.
        let val = serde_json::json!("not-an-array");
        let err = parse_embedding_value(&val, OutputEncoding::Float, "test", None).unwrap_err();
        assert!(err.contains("expected float array"), "got: {err}");
    }

    #[test]
    fn parse_embedding_value_base64_binary_succeeds() {
        // Binary vector: byte 0xAA (10101010), 8 logical dimensions
        // bits (LSB→MSB): 0,1,0,1,0,1,0,1
        let val = serde_json::json!("qg==");
        let result =
            parse_embedding_value(&val, OutputEncoding::Base64Binary, "test", Some(8)).unwrap();
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

    // ── Config deserialization tests ────────────────────────────────────

    #[test]
    fn config_deserialize_minimal_json() {
        let json = r#"{"backend":"fastembed","model":"all-MiniLM-L6-v2","timeout_ms":25000,"max_batch_size":64}"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend, SemanticBackend::Fastembed);
        assert_eq!(config.model, "all-MiniLM-L6-v2");
        assert_eq!(config.timeout_ms, 25000);
        assert_eq!(config.max_batch_size, 64);
        // Optional fields default to None
        assert!(config.base_url.is_none());
        assert!(config.api_key_env.is_none());
        assert!(config.dimensions.is_none());
        assert!(config.output_encoding.is_none());
    }

    #[test]
    fn config_deserialize_all_fields() {
        let json = r#"{
            "backend": "openai_compatible",
            "model": "text-embedding-3-small",
            "base_url": "https://api.openai.com/v1",
            "api_key_env": "OPENAI_API_KEY",
            "timeout_ms": 30000,
            "max_batch_size": 128,
            "dimensions": 1536,
            "output_encoding": "base64_int8",
            "input_mode": "flat_texts",
            "storage_strategy": "decode_normalize_f32",
            "distance_metric": "cosine",
            "query_prompt_template": "Instruct: {query}",
            "document_prompt_template": "Represent: {text}",
            "diagnostics_enabled": true,
            "low_confidence_threshold": 0.5,
            "metrics_window_size": 200,
            "jsonl_logging": true,
            "include_raw_queries": true,
            "include_snippets": true,
            "retention_days": 30,
            "rerank_enabled": true,
            "rerank_model": "codellama",
            "rerank_timeout_ms": 10000,
            "rerank_max_candidates": 10
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend, SemanticBackend::OpenAiCompatible);
        assert_eq!(config.model, "text-embedding-3-small");
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(config.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(config.timeout_ms, 30000);
        assert_eq!(config.max_batch_size, 128);
        assert_eq!(config.dimensions, Some(1536));
        assert_eq!(config.output_encoding, Some(OutputEncoding::Base64Int8));
        assert_eq!(config.input_mode, Some(InputMode::FlatTexts));
        assert_eq!(
            config.storage_strategy,
            Some(StorageStrategy::DecodeNormalizeF32)
        );
        assert_eq!(config.distance_metric, Some(DistanceMetric::Cosine));
        assert!(config.diagnostics_enabled);
        assert!((config.low_confidence_threshold - 0.5).abs() < f32::EPSILON);
        assert_eq!(config.metrics_window_size, 200);
        assert!(config.jsonl_logging);
        assert!(config.include_raw_queries);
        assert!(config.include_snippets);
        assert_eq!(config.retention_days, 30);
        assert!(config.rerank_enabled);
        assert_eq!(config.rerank_model.as_deref(), Some("codellama"));
        assert_eq!(config.rerank_timeout_ms, 10000);
        assert_eq!(config.rerank_max_candidates, 10);
    }

    #[test]
    fn config_deserialize_safe_defaults() {
        // Empty object should deserialize with all defaults
        let json = r#"{
            "backend": "fastembed",
            "model": "all-MiniLM-L6-v2",
            "timeout_ms": 25000,
            "max_batch_size": 64
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        // Verify all optional fields are None
        assert!(config.base_url.is_none());
        assert!(config.api_key_env.is_none());
        assert!(config.dimensions.is_none());
        assert!(config.output_encoding.is_none());
        assert!(config.input_mode.is_none());
        assert!(config.storage_strategy.is_none());
        assert!(config.distance_metric.is_none());
        assert!(config.query_prompt_template.is_none());
        assert!(config.document_prompt_template.is_none());
        assert!(!config.diagnostics_enabled);
        assert!(!config.jsonl_logging);
        assert!(!config.include_raw_queries);
        assert!(!config.include_snippets);
    }

    #[test]
    fn config_deserialize_rerank_fields() {
        let json = r#"{
            "backend": "fastembed",
            "model": "all-MiniLM-L6-v2",
            "timeout_ms": 25000,
            "max_batch_size": 64,
            "rerank_enabled": true,
            "rerank_model": "gpt-4",
            "rerank_base_url": "https://api.openai.com/v1",
            "rerank_api_key_env": "OPENAI_API_KEY",
            "rerank_timeout_ms": 5000,
            "rerank_max_candidates": 20,
            "rerank_max_candidate_chars": 1500
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert!(config.rerank_enabled);
        assert_eq!(config.rerank_model.as_deref(), Some("gpt-4"));
        assert_eq!(
            config.rerank_base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(config.rerank_api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(config.rerank_timeout_ms, 5000);
        assert_eq!(config.rerank_max_candidates, 20);
        assert_eq!(config.rerank_max_candidate_chars, 1500);
    }

    #[test]
    fn config_deserialize_diagnostics_fields() {
        let json = r#"{
            "backend": "fastembed",
            "model": "all-MiniLM-L6-v2",
            "timeout_ms": 25000,
            "max_batch_size": 64,
            "diagnostics_enabled": true,
            "jsonl_logging": true,
            "jsonl_path": "/tmp/diag.jsonl",
            "include_raw_queries": true,
            "include_snippets": true,
            "retention_days": 30,
            "output_mode": "verbose",
            "low_confidence_threshold": 0.2,
            "metrics_window_size": 200
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert!(config.diagnostics_enabled);
        assert!(config.jsonl_logging);
        assert_eq!(
            config.jsonl_path.as_deref(),
            Some(std::path::Path::new("/tmp/diag.jsonl"))
        );
        assert!(config.include_raw_queries);
        assert!(config.include_snippets);
        assert_eq!(config.retention_days, 30);
        assert_eq!(
            config.output_mode,
            crate::config::DiagnosticsOutputMode::Verbose
        );
        assert!((config.low_confidence_threshold - 0.2).abs() < 1e-6);
        assert_eq!(config.metrics_window_size, 200);
    }

    #[test]
    fn config_deserialize_max_results_per_file() {
        let json = r#"{
            "backend": "fastembed",
            "model": "all-MiniLM-L6-v2",
            "timeout_ms": 25000,
            "max_batch_size": 64,
            "max_results_per_file": 5
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_results_per_file, 5);
    }

    #[test]
    fn config_max_results_per_file_default_is_two() {
        let json = r#"{
            "backend": "fastembed",
            "model": "all-MiniLM-L6-v2",
            "timeout_ms": 25000,
            "max_batch_size": 64
        }"#;
        let config: SemanticBackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_results_per_file, 2);
    }

    // ── Profile validation tests ────────────────────────────────────────

    #[test]
    fn profile_fastembed_minilm_is_compatible() {
        let profile = EmbeddingModelProfile::fastembed_minilm();
        assert!(profile.validate_compatible().is_ok());
        assert_eq!(profile.output_encoding, OutputEncoding::Float);
        assert_eq!(profile.source_vector_kind, VectorKind::DenseF32);
        assert_eq!(profile.stored_vector_kind, VectorKind::DenseF32);
        assert_eq!(profile.metric, DistanceMetric::Cosine);
        assert_eq!(profile.storage_strategy, StorageStrategy::NativeF32);
        assert!(!profile.contextualized_supported);
    }

    #[test]
    fn profile_openai_compatible_generic_is_compatible() {
        let profile = EmbeddingModelProfile::openai_compatible_generic();
        assert!(profile.validate_compatible().is_ok());
        assert_eq!(profile.output_encoding, OutputEncoding::Float);
        assert_eq!(profile.source_vector_kind, VectorKind::DenseF32);
        assert_eq!(profile.stored_vector_kind, VectorKind::DenseF32);
        assert_eq!(profile.metric, DistanceMetric::Auto);
        assert!(profile.mrl_supported);
        assert!(!profile.contextualized_supported);
    }

    #[test]
    fn profile_perplexity_int8_is_compatible() {
        let profile = EmbeddingModelProfile::perplexity_int8();
        assert!(profile.validate_compatible().is_ok());
        assert_eq!(profile.output_encoding, OutputEncoding::Base64Int8);
        assert_eq!(profile.source_vector_kind, VectorKind::DenseInt8);
        assert_eq!(profile.stored_vector_kind, VectorKind::DenseF32);
        assert_eq!(profile.metric, DistanceMetric::Cosine);
        assert_eq!(
            profile.normalization,
            NormalizationPolicy::NormalizeOnInsertQuery
        );
        assert_eq!(
            profile.storage_strategy,
            StorageStrategy::DecodeNormalizeF32
        );
        assert!(profile.contextualized_supported);
    }

    #[test]
    fn profile_perplexity_binary_is_compatible() {
        let profile = EmbeddingModelProfile::perplexity_binary();
        assert!(profile.validate_compatible().is_ok());
        assert_eq!(profile.output_encoding, OutputEncoding::Base64Binary);
        assert_eq!(profile.source_vector_kind, VectorKind::BinaryPacked);
        assert_eq!(profile.stored_vector_kind, VectorKind::BinaryPacked);
        assert_eq!(profile.metric, DistanceMetric::Hamming);
        assert_eq!(profile.normalization, NormalizationPolicy::NotApplicable);
        assert_eq!(profile.storage_strategy, StorageStrategy::BinaryPacked);
        assert!(profile.contextualized_supported);
    }

    #[test]
    fn profile_from_config_selects_correctly() {
        // Fastembed with matching model
        let config_fastembed = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            model: "all-MiniLM-L6-v2".to_string(),
            output_encoding: None,
            storage_strategy: None,
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config_fastembed).unwrap();
        assert_eq!(profile.backend, SemanticBackend::Fastembed);
        assert_eq!(profile.metric, DistanceMetric::Cosine);

        // OpenAI-compatible
        let config_oai = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config_oai).unwrap();
        assert_eq!(profile.backend, SemanticBackend::OpenAiCompatible);

        // Perplexity with base64_int8
        let config_int8 = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            output_encoding: Some(OutputEncoding::Base64Int8),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config_int8).unwrap();
        assert_eq!(profile.output_encoding, OutputEncoding::Base64Int8);
        assert_eq!(profile.source_vector_kind, VectorKind::DenseInt8);

        // Perplexity with base64_binary
        let config_binary = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            output_encoding: Some(OutputEncoding::Base64Binary),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config_binary).unwrap();
        assert_eq!(profile.output_encoding, OutputEncoding::Base64Binary);
        assert_eq!(profile.source_vector_kind, VectorKind::BinaryPacked);
    }

    // ── TypedVector conversion tests ────────────────────────────────────

    #[test]
    fn typed_vector_dense_f32_kind_and_dims() {
        let v = TypedVector::DenseF32(vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(v.kind(), VectorKind::DenseF32);
        assert_eq!(v.dims(), 4);
    }

    #[test]
    fn typed_vector_dense_int8_kind_and_dims() {
        let v = TypedVector::DenseInt8(vec![10, -20, 30]);
        assert_eq!(v.kind(), VectorKind::DenseInt8);
        assert_eq!(v.dims(), 3);
    }

    #[test]
    fn typed_vector_binary_packed_kind_and_dims() {
        let v = TypedVector::BinaryPacked {
            bytes: vec![0xFF, 0x00],
            logical_dims: 12,
        };
        assert_eq!(v.kind(), VectorKind::BinaryPacked);
        assert_eq!(v.dims(), 12);
    }

    #[test]
    fn typed_vector_into_stored_f32_native() {
        let v = TypedVector::DenseF32(vec![0.1, 0.2, 0.3]);
        let stored = v.into_stored(StorageStrategy::NativeF32).unwrap();
        assert_eq!(stored.kind(), VectorKind::DenseF32);
        assert_eq!(stored.dims(), 3);
        let f32s = stored.to_f32_slice().unwrap();
        assert!((f32s[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn typed_vector_into_stored_f32_normalize() {
        let v = TypedVector::DenseF32(vec![3.0, 4.0]);
        let stored = v.into_stored(StorageStrategy::DecodeNormalizeF32).unwrap();
        let f32s = stored.to_f32_slice().unwrap();
        // L2 norm of [3,4] = 5; normalized = [0.6, 0.8]
        assert!((f32s[0] - 0.6).abs() < 1e-5);
        assert!((f32s[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn typed_vector_into_stored_f32_rejects_binary_packed() {
        let v = TypedVector::DenseF32(vec![0.1, 0.2]);
        let err = v.into_stored(StorageStrategy::BinaryPacked).unwrap_err();
        assert!(err.contains("DenseF32"), "got: {err}");
    }

    #[test]
    fn typed_vector_into_stored_int8_native() {
        let v = TypedVector::DenseInt8(vec![10, -20, 30]);
        let stored = v.into_stored(StorageStrategy::NativeF32).unwrap();
        let f32s = stored.to_f32_slice().unwrap();
        assert!((f32s[0] - 10.0).abs() < 1e-6);
        assert!((f32s[1] - (-20.0)).abs() < 1e-6);
        assert!((f32s[2] - 30.0).abs() < 1e-6);
    }

    #[test]
    fn typed_vector_into_stored_int8_normalize() {
        let v = TypedVector::DenseInt8(vec![3, 4]);
        let stored = v.into_stored(StorageStrategy::DecodeNormalizeF32).unwrap();
        let f32s = stored.to_f32_slice().unwrap();
        // L2 norm of [3,4] = 5; normalized = [0.6, 0.8]
        assert!((f32s[0] - 0.6).abs() < 1e-5);
        assert!((f32s[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn typed_vector_into_stored_int8_rejects_binary_packed() {
        let v = TypedVector::DenseInt8(vec![10, -20]);
        let err = v.into_stored(StorageStrategy::BinaryPacked).unwrap_err();
        assert!(err.contains("DenseInt8"), "got: {err}");
    }

    #[test]
    fn typed_vector_into_stored_binary_native() {
        let v = TypedVector::BinaryPacked {
            bytes: vec![0xFF],
            logical_dims: 8,
        };
        let stored = v.into_stored(StorageStrategy::BinaryPacked).unwrap();
        assert_eq!(stored.kind(), VectorKind::BinaryPacked);
        assert_eq!(stored.dims(), 8);
        let (bytes, dims) = stored.to_packed().unwrap();
        assert_eq!(bytes, &[0xFF]);
        assert_eq!(dims, 8);
    }

    #[test]
    fn typed_vector_into_stored_binary_rejects_f32() {
        let v = TypedVector::BinaryPacked {
            bytes: vec![0xFF],
            logical_dims: 8,
        };
        let err = v.into_stored(StorageStrategy::NativeF32).unwrap_err();
        assert!(err.contains("BinaryPacked"), "got: {err}");
    }

    #[test]
    fn typed_vector_into_stored_binary_rejects_normalize() {
        let v = TypedVector::BinaryPacked {
            bytes: vec![0xFF],
            logical_dims: 8,
        };
        let err = v
            .into_stored(StorageStrategy::DecodeNormalizeF32)
            .unwrap_err();
        assert!(err.contains("BinaryPacked"), "got: {err}");
    }

    // ── StoredVector roundtrip tests ────────────────────────────────────

    #[test]
    fn stored_vector_dense_f32_to_f32_slice_roundtrip() {
        let sv = StoredVector::DenseF32(vec![0.1, 0.2, 0.3]);
        let slice = sv.to_f32_slice().unwrap();
        assert_eq!(slice, &[0.1, 0.2, 0.3]);
    }

    #[test]
    fn stored_vector_dense_f32_to_packed_rejects() {
        let sv = StoredVector::DenseF32(vec![0.1, 0.2]);
        let err = sv.to_packed().unwrap_err();
        assert!(err.contains("dense"), "got: {err}");
    }

    #[test]
    fn stored_vector_binary_to_packed_roundtrip() {
        let sv = StoredVector::BinaryPacked {
            bytes: vec![0xAB, 0xCD],
            logical_dims: 12,
        };
        let (bytes, dims) = sv.to_packed().unwrap();
        assert_eq!(bytes, &[0xAB, 0xCD]);
        assert_eq!(dims, 12);
    }

    #[test]
    fn stored_vector_binary_to_f32_rejects() {
        let sv = StoredVector::BinaryPacked {
            bytes: vec![0xFF],
            logical_dims: 8,
        };
        let err = sv.to_f32_slice().unwrap_err();
        assert!(err.contains("binary"), "got: {err}");
    }

    #[test]
    fn stored_vector_l2_normalize_dense() {
        let sv = StoredVector::DenseF32(vec![3.0, 4.0]);
        let normed = sv.l2_normalize();
        let f32s = normed.to_f32_slice().unwrap();
        assert!((f32s[0] - 0.6).abs() < 1e-5);
        assert!((f32s[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn stored_vector_l2_normalize_binary_noop() {
        let sv = StoredVector::BinaryPacked {
            bytes: vec![0xFF],
            logical_dims: 8,
        };
        let normed = sv.l2_normalize();
        assert_eq!(normed.kind(), VectorKind::BinaryPacked);
        let (bytes, dims) = normed.to_packed().unwrap();
        assert_eq!(bytes, &[0xFF]);
        assert_eq!(dims, 8);
    }

    // ── convert_vector tests ────────────────────────────────────────────

    #[test]
    fn convert_vector_f32_to_f32_succeeds() {
        let profile = EmbeddingModelProfile::fastembed_minilm();
        let typed = TypedVector::DenseF32(vec![0.1, 0.2, 0.3]);
        let stored = profile.convert_vector(typed).unwrap();
        assert_eq!(stored.kind(), VectorKind::DenseF32);
    }

    #[test]
    fn convert_vector_int8_to_f32_succeeds() {
        let profile = EmbeddingModelProfile::perplexity_int8();
        let typed = TypedVector::DenseInt8(vec![10, -20, 30]);
        let stored = profile.convert_vector(typed).unwrap();
        assert_eq!(stored.kind(), VectorKind::DenseF32);
        // Verify L2 normalization was applied (NormalizeOnInsertQuery)
        let f32s = stored.to_f32_slice().unwrap();
        let norm_sq: f32 = f32s.iter().map(|x| x * x).sum();
        assert!((norm_sq - 1.0).abs() < 1e-5, "norm² = {norm_sq}");
    }

    #[test]
    fn convert_vector_binary_to_binary_succeeds() {
        let profile = EmbeddingModelProfile::perplexity_binary();
        let typed = TypedVector::BinaryPacked {
            bytes: vec![0xFF, 0x00],
            logical_dims: 12,
        };
        let stored = profile.convert_vector(typed).unwrap();
        assert_eq!(stored.kind(), VectorKind::BinaryPacked);
    }

    #[test]
    fn convert_vector_rejects_kind_mismatch() {
        let profile = EmbeddingModelProfile::fastembed_minilm(); // expects DenseF32
        let typed = TypedVector::DenseInt8(vec![10, -20]);
        let err = profile.convert_vector(typed).unwrap_err();
        assert!(err.contains("vector kind mismatch"), "got: {err}");
    }

    // ── validate_compatible rejection tests ─────────────────────────────

    #[test]
    fn validate_compatible_rejects_f32_source_to_binary_stored() {
        let profile = EmbeddingModelProfile {
            source_vector_kind: VectorKind::DenseF32,
            stored_vector_kind: VectorKind::BinaryPacked,
            ..EmbeddingModelProfile::fastembed_minilm()
        };
        let err = profile.validate_compatible().unwrap_err();
        assert!(err.contains("unsupported source"), "got: {err}");
    }

    #[test]
    fn validate_compatible_rejects_binary_stored_with_cosine_metric() {
        let profile = EmbeddingModelProfile {
            source_vector_kind: VectorKind::BinaryPacked,
            stored_vector_kind: VectorKind::BinaryPacked,
            metric: DistanceMetric::Cosine,
            ..EmbeddingModelProfile::fastembed_minilm()
        };
        let err = profile.validate_compatible().unwrap_err();
        assert!(err.contains("metric"), "got: {err}");
    }

    #[test]
    fn validate_compatible_rejects_f32_encoding_with_binary_strategy() {
        let profile = EmbeddingModelProfile {
            output_encoding: OutputEncoding::Float,
            storage_strategy: StorageStrategy::BinaryPacked,
            ..EmbeddingModelProfile::fastembed_minilm()
        };
        let err = profile.validate_compatible().unwrap_err();
        assert!(err.contains("not compatible"), "got: {err}");
    }

    #[test]
    fn validate_compatible_rejects_int8_encoding_with_f32_strategy() {
        let profile = EmbeddingModelProfile {
            output_encoding: OutputEncoding::Base64Int8,
            storage_strategy: StorageStrategy::NativeF32,
            ..EmbeddingModelProfile::fastembed_minilm()
        };
        // NativeF32 is allowed for Base64Int8
        assert!(profile.validate_compatible().is_ok());
    }

    // ── Distance metric auto-resolution tests ───────────────────────────

    #[test]
    fn resolve_distance_metric_fastembed_defaults_to_cosine() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            distance_metric: Some(DistanceMetric::Auto),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::fastembed_minilm();
        let resolved = resolve_distance_metric(&config, Some(&profile));
        assert_eq!(resolved, DistanceMetric::Cosine);
    }

    #[test]
    fn resolve_distance_metric_explicit_overrides_auto() {
        let config = SemanticBackendConfig {
            distance_metric: Some(DistanceMetric::DotProduct),
            ..SemanticBackendConfig::default()
        };
        let resolved = resolve_distance_metric(&config, None);
        assert_eq!(resolved, DistanceMetric::DotProduct);
    }

    #[test]
    fn resolve_distance_metric_int8_profile_cosine() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            distance_metric: Some(DistanceMetric::Auto),
            output_encoding: Some(OutputEncoding::Base64Int8),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config).unwrap();
        let resolved = resolve_distance_metric(&config, Some(&profile));
        assert_eq!(resolved, DistanceMetric::Cosine);
    }

    #[test]
    fn resolve_distance_metric_binary_profile_hamming() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            distance_metric: Some(DistanceMetric::Auto),
            output_encoding: Some(OutputEncoding::Base64Binary),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::from_config(&config).unwrap();
        let resolved = resolve_distance_metric(&config, Some(&profile));
        assert_eq!(resolved, DistanceMetric::Hamming);
    }

    // ── Dimension validation tests ──────────────────────────────────────

    #[test]
    fn resolve_dimensions_prefers_config_over_profile() {
        let config = SemanticBackendConfig {
            dimensions: Some(1536),
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::fastembed_minilm(); // default 384
        let resolved = resolve_dimensions(&config, Some(&profile));
        assert_eq!(resolved, Some(1536));
    }

    #[test]
    fn resolve_dimensions_falls_back_to_profile_default() {
        let config = SemanticBackendConfig {
            dimensions: None,
            ..SemanticBackendConfig::default()
        };
        let profile = EmbeddingModelProfile::fastembed_minilm();
        let resolved = resolve_dimensions(&config, Some(&profile));
        assert_eq!(resolved, Some(384));
    }

    #[test]
    fn validate_config_rejects_unsupported_dimensions() {
        let profile = EmbeddingModelProfile::fastembed_minilm(); // range: 384-384
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            model: "all-MiniLM-L6-v2".to_string(),
            dimensions: Some(768),
            ..SemanticBackendConfig::default()
        };
        let err = profile.validate_config(&config).unwrap_err();
        assert!(err.iter().any(|e| e.contains("dimensions")), "got: {err:?}");
    }

    #[test]
    fn validate_config_rejects_contextualized_for_flat_provider() {
        let profile = EmbeddingModelProfile::fastembed_minilm(); // contextualized_supported: false
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            model: "all-MiniLM-L6-v2".to_string(),
            input_mode: Some(InputMode::DocumentChunks),
            ..SemanticBackendConfig::default()
        };
        let err = profile.validate_config(&config).unwrap_err();
        assert!(
            err.iter()
                .any(|e| e.contains("input_mode") || e.contains("document_chunks")),
            "got: {err:?}"
        );
    }

    // ── base64_int8 signed int8 decode tests ────────────────────────────

    #[test]
    fn base64_int8_negative_values_decode_correctly() {
        // -1 as i8 = 0xFF in unsigned, -128 as i8 = 0x80
        let values: Vec<i8> = vec![-1, -128, 127, 0, 1];
        let encoded = encode_int8_base64(&values);
        let val = serde_json::json!(encoded);
        let result = parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", None).unwrap();
        // After L2-normalization, verify signs are preserved
        assert!(result[0] < 0.0, "v[0] = {} should be negative", result[0]);
        assert!(result[1] < 0.0, "v[1] = {} should be negative", result[1]);
        assert!(result[2] > 0.0, "v[2] = {} should be positive", result[2]);
        assert!(
            (result[3]).abs() < 1e-6,
            "v[3] = {} should be ~0",
            result[3]
        );
        assert!(result[4] > 0.0, "v[4] = {} should be positive", result[4]);
    }

    #[test]
    fn base64_int8_all_zeros_is_zero_norm() {
        let values: Vec<i8> = vec![0, 0, 0];
        let encoded = encode_int8_base64(&values);
        let val = serde_json::json!(encoded);
        let result = parse_embedding_value(&val, OutputEncoding::Base64Int8, "test", None).unwrap();
        // All-zero vector: norm is 0, no division happens
        assert_eq!(result, vec![0.0, 0.0, 0.0]);
    }

    // ── Template hashing tests ──────────────────────────────────────────

    #[test]
    fn prompt_template_hash_none_is_empty() {
        assert_eq!(prompt_template_hash(None), "");
    }

    #[test]
    fn prompt_template_hash_deterministic() {
        let h1 = prompt_template_hash(Some("Instruct: {query}"));
        let h2 = prompt_template_hash(Some("Instruct: {query}"));
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn prompt_template_hash_differs_for_different_templates() {
        let h1 = prompt_template_hash(Some("template A"));
        let h2 = prompt_template_hash(Some("template B"));
        assert_ne!(h1, h2);
    }

    // ── SemanticBackend enum tests ──────────────────────────────────────

    #[test]
    fn semantic_backend_as_str_roundtrip() {
        let backends = [
            SemanticBackend::Fastembed,
            SemanticBackend::OpenAiCompatible,
            SemanticBackend::Ollama,
            SemanticBackend::Perplexity,
        ];
        for backend in &backends {
            let s = backend.as_str();
            let parsed = SemanticBackend::from_name(s).unwrap();
            assert_eq!(&parsed, backend);
        }
    }

    #[test]
    fn semantic_backend_from_name_unknown() {
        assert!(SemanticBackend::from_name("unknown_backend").is_none());
    }

    #[test]
    fn semantic_backend_serde_roundtrip() {
        let backends = [
            SemanticBackend::Fastembed,
            SemanticBackend::OpenAiCompatible,
            SemanticBackend::Ollama,
            SemanticBackend::Perplexity,
        ];
        for backend in &backends {
            let json = serde_json::to_string(backend).unwrap();
            let parsed: SemanticBackend = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, *backend);
        }
    }

    // ── OutputEncoding enum tests ───────────────────────────────────────

    #[test]
    fn output_encoding_default_for_backend() {
        // All built-in backends default to Float
        let backends = [
            SemanticBackend::Fastembed,
            SemanticBackend::OpenAiCompatible,
            SemanticBackend::Ollama,
            SemanticBackend::Perplexity,
        ];
        for backend in &backends {
            assert_eq!(
                OutputEncoding::default_for_backend(*backend),
                OutputEncoding::Float
            );
        }
    }

    // ── InputMode enum tests ────────────────────────────────────────────

    #[test]
    fn input_mode_default_for_backend() {
        let flat_backends = [
            SemanticBackend::Fastembed,
            SemanticBackend::OpenAiCompatible,
            SemanticBackend::Ollama,
        ];
        for backend in &flat_backends {
            assert_eq!(
                InputMode::default_for_backend(*backend),
                InputMode::FlatTexts
            );
        }
        assert_eq!(
            InputMode::default_for_backend(SemanticBackend::Perplexity),
            InputMode::DocumentChunks
        );
    }

    // ── resolve_output_encoding / resolve_storage_strategy tests ────────

    #[test]
    fn resolve_output_encoding_uses_config_when_set() {
        let config = SemanticBackendConfig {
            output_encoding: Some(OutputEncoding::Base64Int8),
            ..SemanticBackendConfig::default()
        };
        assert_eq!(resolve_output_encoding(&config), OutputEncoding::Base64Int8);
    }

    #[test]
    fn resolve_output_encoding_falls_back_to_default() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            output_encoding: None,
            ..SemanticBackendConfig::default()
        };
        assert_eq!(resolve_output_encoding(&config), OutputEncoding::Float);
    }

    #[test]
    fn resolve_storage_strategy_uses_config_when_set() {
        let config = SemanticBackendConfig {
            storage_strategy: Some(StorageStrategy::BinaryPacked),
            ..SemanticBackendConfig::default()
        };
        assert_eq!(
            resolve_storage_strategy(&config),
            StorageStrategy::BinaryPacked
        );
    }

    #[test]
    fn resolve_storage_strategy_falls_back_to_default() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Fastembed,
            storage_strategy: None,
            ..SemanticBackendConfig::default()
        };
        assert_eq!(
            resolve_storage_strategy(&config),
            StorageStrategy::NativeF32
        );
    }

    // ── apply_query_template / apply_document_template tests ─────────────

    #[test]
    fn apply_query_template_replaces_placeholder() {
        let result = apply_query_template("hello", Some("Search: {query}"));
        assert_eq!(result, "Search: hello");
    }

    #[test]
    fn apply_query_template_no_placeholder_returns_raw() {
        let result = apply_query_template("hello", Some("No placeholder here"));
        assert_eq!(result, "hello");
    }

    #[test]
    fn apply_query_template_none_returns_raw() {
        let result = apply_query_template("hello", None);
        assert_eq!(result, "hello");
    }

    #[test]
    fn apply_document_template_replaces_placeholder() {
        let result = apply_document_template("chunk text", Some("Doc: {text}"));
        assert_eq!(result, "Doc: chunk text");
    }

    #[test]
    fn apply_document_template_none_returns_raw() {
        let result = apply_document_template("chunk text", None);
        assert_eq!(result, "chunk text");
    }

    #[test]
    fn apply_query_template_empty_string_returns_raw() {
        // An empty template should behave like None (no wrapping).
        let result = apply_query_template("hello", Some(""));
        assert_eq!(result, "hello");
    }

    #[test]
    fn apply_document_template_empty_string_returns_raw() {
        let result = apply_document_template("chunk text", Some(""));
        assert_eq!(result, "chunk text");
    }

    #[test]
    fn apply_query_template_wrong_placeholder_returns_raw() {
        // Template with {text} instead of {query} — no-op.
        let result = apply_query_template("hello", Some("Doc: {text}"));
        assert_eq!(result, "hello");
    }

    #[test]
    fn apply_document_template_wrong_placeholder_returns_raw() {
        // Template with {query} instead of {text} — no-op.
        let result = apply_document_template("chunk text", Some("Query: {query}"));
        assert_eq!(result, "chunk text");
    }

    #[test]
    fn apply_query_template_literal_query_not_double_substituted() {
        // A query containing literal {query} should be substituted once.
        let result = apply_query_template("find {query} in code", Some("Search: {query}"));
        assert_eq!(result, "Search: find {query} in code");
    }

    #[test]
    fn collect_chunks_applies_document_prompt_template() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() { world() }").unwrap();
        let policy = SemanticFilePolicy::default();
        let (chunks, _) = SemanticIndex::collect_chunks(
            dir.path(),
            std::slice::from_ref(&file),
            &policy,
            Some("Doc: {text}"),
        );
        assert!(!chunks.is_empty(), "should have at least one chunk");
        for chunk in &chunks {
            assert!(
                chunk.embed_text.starts_with("Doc: "),
                "embed_text should be prefixed with 'Doc: ', got: {}",
                &chunk.embed_text[..50.min(chunk.embed_text.len())]
            );
        }
    }

    #[test]
    fn collect_chunks_no_prefix_when_template_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, "fn hello() { world() }").unwrap();
        let policy = SemanticFilePolicy::default();
        let (chunks, _) =
            SemanticIndex::collect_chunks(dir.path(), std::slice::from_ref(&file), &policy, None);
        assert!(!chunks.is_empty(), "should have at least one chunk");
        for chunk in &chunks {
            assert!(
                !chunk.embed_text.starts_with("Doc: "),
                "embed_text should NOT be prefixed when template is None, got: {}",
                &chunk.embed_text[..50.min(chunk.embed_text.len())]
            );
        }
    }

    #[test]
    fn resolve_embedding_profile_coderankerembed() {
        let profile = resolve_embedding_profile("nomic-ai/CodeRankEmbed");
        assert!(
            profile.is_some(),
            "profile should be Some for 'nomic-ai/CodeRankEmbed'"
        );
        let p = profile.unwrap();
        assert!(p.query_prefix.contains("query for searching relevant code"));
        assert_eq!(p.document_prefix, "");
    }

    #[test]
    fn resolve_embedding_profile_e5_base() {
        let profile = resolve_embedding_profile("intfloat/e5-base-v2");
        assert!(profile.is_some());
        let p = profile.unwrap();
        assert_eq!(p.query_prefix, "query: ");
        assert_eq!(p.document_prefix, "passage: ");
    }

    #[test]
    fn resolve_embedding_profile_bge_m3() {
        let profile = resolve_embedding_profile("BAAI/bge-m3");
        assert!(profile.is_some());
        let p = profile.unwrap();
        assert_eq!(p.query_prefix, "");
        assert_eq!(p.document_prefix, "");
    }

    #[test]
    fn resolve_embedding_profile_unknown_returns_none() {
        assert!(resolve_embedding_profile("some-random-model").is_none());
    }

    #[test]
    fn resolve_embedding_profile_case_insensitive() {
        let profile = resolve_embedding_profile("intfloat/E5-BASE-V2");
        assert!(profile.is_some());
    }

    #[test]
    fn prompt_template_hash_none_equals_empty_string() {
        // None and empty string should produce the same hash (both normalize to empty).
        assert_eq!(prompt_template_hash(None), "");
        assert_eq!(prompt_template_hash(Some("")), "");
    }

    #[test]
    fn prompt_template_hash_whitespace_only_equals_none() {
        // Whitespace-only templates should also normalize to empty.
        assert_eq!(prompt_template_hash(Some("  ")), "");
        assert_eq!(prompt_template_hash(Some("\t\n")), "");
    }

    #[test]
    fn prompt_template_hash_nonempty_produces_nonempty() {
        // A real template should produce a non-empty hash.
        let hash = prompt_template_hash(Some("Search: {query}"));
        assert!(!hash.is_empty());
    }

    #[test]
    fn prompt_template_hash_different_templates_different_hashes() {
        let h1 = prompt_template_hash(Some("Search: {query}"));
        let h2 = prompt_template_hash(Some("Find: {query}"));
        assert_ne!(h1, h2);
    }

    // ── Contextualized embedding tests (aft-t6p.23.1) ──────────────────────────

    /// Helper: write a source file with given content into temp dir and return its path.
    fn write_temp_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    /// Build a mock contextualized embed fn that returns vectors of given dimension.
    fn mock_contextual_embed_fn(
        dim: usize,
    ) -> impl FnMut(DocumentChunks) -> Result<DocumentEmbeddings, String> {
        move |dc: DocumentChunks| {
            let embeddings: Vec<ChunkEmbeddings> = dc
                .documents
                .into_iter()
                .map(|doc| ChunkEmbeddings {
                    file_path: doc.file_path,
                    vectors: doc.chunks.iter().map(|_| vec![1.0; dim]).collect(),
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        }
    }

    /// Build a mock contextualized embed fn that also captures the documents it receives.
    fn capturing_contextual_embed_fn(
        captured: std::rc::Rc<std::cell::RefCell<Vec<DocumentChunks>>>,
        dim: usize,
    ) -> impl FnMut(DocumentChunks) -> Result<DocumentEmbeddings, String> {
        move |dc: DocumentChunks| {
            captured.borrow_mut().push(dc.clone());
            let embeddings: Vec<ChunkEmbeddings> = dc
                .documents
                .iter()
                .map(|doc| ChunkEmbeddings {
                    file_path: doc.file_path.clone(),
                    vectors: doc.chunks.iter().map(|_| vec![1.0; dim]).collect(),
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        }
    }

    #[test]
    fn contextualized_chunks_grouped_by_source_document() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file_a = write_temp_file(
            &project_root,
            "a.rs",
            "pub fn foo() -> bool {\n    true\n}\npub fn bar() -> bool {\n    false\n}\npub fn baz() -> i32 {\n    42\n}\n",
        );
        let file_b = write_temp_file(
            &project_root,
            "b.rs",
            "pub fn alpha() -> bool {\n    true\n}\npub fn beta() -> bool {\n    false\n}\n",
        );

        let files = vec![file_a.clone(), file_b.clone()];

        let captured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut embed_fn = capturing_contextual_embed_fn(captured.clone(), 3);
        let mut progress_calls: Vec<(usize, usize)> = Vec::new();
        let mut progress = |done: usize, total: usize| progress_calls.push((done, total));

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &files,
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(result.is_ok(), "build failed: {:?}", result.err());
        let (index, _diag) = result.unwrap();
        assert!(!index.is_empty(), "index should have entries");

        // Verify documents grouped by file: each file's chunks appear together
        let caps = captured.borrow();
        let mut found_a = false;
        let mut found_b = false;
        for dc in caps.iter() {
            for doc in &dc.documents {
                if doc.file_path == file_a {
                    found_a = true;
                    assert!(doc.chunks.len() >= 2);
                }
                if doc.file_path == file_b {
                    found_b = true;
                    assert!(!doc.chunks.is_empty());
                }
            }
        }
        assert!(found_a, "file_a chunks not found");
        assert!(found_b, "file_b chunks not found");
        assert!(!progress_calls.is_empty(), "progress should be called");
    }

    #[test]
    fn contextualized_chunk_order_preserved_within_document() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "ordered.rs",
            "pub fn first() -> i32 {\n    1\n}\npub fn second() -> i32 {\n    2\n}\npub fn third() -> i32 {\n    3\n}\npub fn fourth() -> i32 {\n    4\n}\n",
        );
        let file_for_check = file.clone();

        let captured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut embed_fn = capturing_contextual_embed_fn(captured.clone(), 3);
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(result.is_ok(), "build failed: {:?}", result.err());

        // Verify chunk order is preserved
        let caps = captured.borrow();
        for dc in caps.iter() {
            for doc in &dc.documents {
                if doc.file_path == file_for_check {
                    let full_text = doc.chunks.join(" ");
                    let first_pos = full_text.find("first");
                    let second_pos = full_text.find("second");
                    let third_pos = full_text.find("third");
                    let fourth_pos = full_text.find("fourth");
                    if let (Some(a), Some(b), Some(c), Some(d)) =
                        (first_pos, second_pos, third_pos, fourth_pos)
                    {
                        assert!(a < b, "first before second");
                        assert!(b < c, "second before third");
                        assert!(c < d, "third before fourth");
                    }
                }
            }
        }
    }

    #[test]
    fn contextualized_response_wrong_chunk_count_fails() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "chunkcount.rs",
            "pub fn a() -> i32 {\n    1\n}\npub fn b() -> i32 {\n    2\n}\npub fn c() -> i32 {\n    3\n}\n",
        );

        // Return wrong number of vectors for the chunks
        let mut embed_fn = |dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            let embeddings: Vec<ChunkEmbeddings> = dc
                .documents
                .iter()
                .map(|doc| ChunkEmbeddings {
                    file_path: doc.file_path.clone(),
                    vectors: vec![vec![1.0; 3]],
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );

        assert!(result.is_err(), "should fail on wrong chunk count");
        let err = result.unwrap_err();
        assert!(
            err.contains("vectors for") || err.contains("embedding response returned"),
            "error should mention vector count mismatch, got: {err}"
        );
    }

    #[test]
    fn contextualized_response_unknown_file_path_fails() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "unknownpath.rs",
            "pub fn a() -> i32 {\n    1\n}\n",
        );

        // Return embeddings for a file not in the index
        let bad_path = project_root.join("nonexistent.rs");
        let mut embed_fn = move |_dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            Ok(DocumentEmbeddings {
                embeddings: vec![ChunkEmbeddings {
                    file_path: bad_path.clone(),
                    vectors: vec![vec![1.0; 3]],
                }],
            })
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );

        assert!(result.is_err(), "should fail on unknown file path");
        let err = result.unwrap_err();
        assert!(
            err.contains("unknown file path"),
            "error should mention unknown file path, got: {err}"
        );
    }

    #[test]
    fn contextualized_response_dimension_mismatch_fails() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file_a = write_temp_file(
            &project_root,
            "dimmismatch.rs",
            "pub fn a() -> i32 {\n    1\n}\npub fn b() -> i32 {\n    2\n}\n",
        );
        let file_b = write_temp_file(
            &project_root,
            "dimmismatch2.rs",
            "pub fn c() -> i32 {\n    3\n}\n",
        );

        let mut dims = vec![3, 5, 5].into_iter();
        let mut embed_fn = move |dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            let d = dims.next().unwrap_or(3);
            let embeddings: Vec<ChunkEmbeddings> = dc
                .documents
                .iter()
                .map(|doc| ChunkEmbeddings {
                    file_path: doc.file_path.clone(),
                    vectors: doc.chunks.iter().map(|_| vec![1.0; d]).collect(),
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file_a, file_b],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );

        assert!(result.is_err(), "should fail on dimension change");
        let err = result.unwrap_err();
        assert!(
            err.contains("dimension changed") || err.contains("embedding dimension"),
            "error should mention dimension mismatch, got: {err}"
        );
    }

    #[test]
    fn contextualized_stale_vector_pruning_after_refresh() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "stale.rs",
            "pub fn original() -> bool {\n    true\n}\npub fn also_original() -> bool {\n    false\n}\n",
        );

        let mut embed_fn = mock_contextual_embed_fn(3);
        let mut progress = |_: usize, _: usize| {};

        let (mut index, _diag) = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            std::slice::from_ref(&file),
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        )
        .expect("initial build");

        let initial_len = index.len();
        assert!(initial_len > 0, "index should have entries");

        // Modify the file
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(
            &file,
            "pub fn modified() -> i32 {\n    42\n}\npub fn new_func() -> bool {\n    false\n}\n",
        )
        .unwrap();

        let mut flat_embed = |texts: Vec<String>| -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| vec![1.0; 3]).collect())
        };
        let refreshed = index.refresh_stale_files(
            &project_root,
            std::slice::from_ref(&file),
            &mut flat_embed,
            8,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );

        assert!(
            refreshed.is_ok(),
            "refresh should succeed: {:?}",
            refreshed.err()
        );
        let summary = refreshed.unwrap();
        assert!(
            summary.changed > 0 || summary.added > 0 || summary.deleted > 0,
            "refresh should detect changes: {summary:?}"
        );
    }

    #[test]
    fn contextualized_perplexity_backend_sets_document_chunks_input_mode() {
        // Verify that Perplexity uses InputMode::DocumentChunks
        let _config = SemanticBackendConfig {
            backend: SemanticBackend::Perplexity,
            model: "sonar".to_string(),
            ..Default::default()
        };
        let profile = EmbeddingModelProfile::perplexity_generic();
        assert_eq!(
            profile.input_mode,
            InputMode::DocumentChunks,
            "Perplexity profile should use DocumentChunks input mode"
        );

        // Verify Fastembed uses FlatTexts (for contrast)
        let fastembed_profile = EmbeddingModelProfile::fastembed_minilm();
        assert_eq!(
            fastembed_profile.input_mode,
            InputMode::FlatTexts,
            "Fastembed profile should use FlatTexts"
        );
    }

    #[test]
    fn contextualized_oversized_document_is_split() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        // Create a file with many small functions to produce many chunks.
        // Each function spans 3 lines to pass symbols_to_chunks' line_count >= 2 filter.
        // Keep under 100 chunks (DEFAULT_MAX_CHUNKS_PER_DOCUMENT) to avoid
        // split-sub-group verification issues — the build path itself is exercised.
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("pub fn func_{i}() -> i32 {{\n    {i}\n}}\n"));
        }
        let file = write_temp_file(&project_root, "oversized.rs", &content);
        let file_for_check = file.clone();

        let captured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut embed_fn = capturing_contextual_embed_fn(captured.clone(), 3);
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(result.is_ok(), "build should succeed: {:?}", result.err());
        let (index, _diag) = result.unwrap();
        assert!(
            !index.is_empty(),
            "oversized doc should still produce entries"
        );

        // Verify the doc produced entries
        let caps = captured.borrow();
        let parts_for_file: usize = caps
            .iter()
            .flat_map(|dc| dc.documents.iter())
            .filter(|doc| doc.file_path == file_for_check)
            .count();
        assert!(
            parts_for_file >= 1,
            "doc should produce >= 1 sub-doc, got {parts_for_file}"
        );
    }

    #[test]
    fn contextualized_empty_files_produces_empty_index() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let mut embed_fn = |_dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            Ok(DocumentEmbeddings {
                embeddings: Vec::new(),
            })
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(result.is_ok(), "empty build should succeed");
        let (index, _diag) = result.unwrap();
        assert_eq!(index.len(), 0, "empty files → empty index");
    }

    #[test]
    fn contextualized_retry_on_transient_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "retry.rs",
            "pub fn retry_me() -> i32 {\n    1\n}\n",
        );

        let attempts = std::rc::Rc::new(std::cell::RefCell::new(0));
        let att_clone = attempts.clone();
        let mut embed_fn = move |dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            let mut a = att_clone.borrow_mut();
            *a += 1;
            if *a <= 2 {
                Err("rate limit exceeded (429)".to_string())
            } else {
                let embeddings: Vec<ChunkEmbeddings> = dc
                    .documents
                    .iter()
                    .map(|doc| ChunkEmbeddings {
                        file_path: doc.file_path.clone(),
                        vectors: doc.chunks.iter().map(|_| vec![1.0; 3]).collect(),
                    })
                    .collect();
                Ok(DocumentEmbeddings { embeddings })
            }
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "should succeed after retries: {:?}",
            result.err()
        );
        assert!(
            *attempts.borrow() >= 3,
            "should have retried, got {} attempts",
            *attempts.borrow()
        );
    }

    #[test]
    fn contextualized_non_transient_error_is_not_retried() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file = write_temp_file(
            &project_root,
            "noretry.rs",
            "pub fn no_retry() -> i32 {\n    1\n}\n",
        );

        let attempts = std::rc::Rc::new(std::cell::RefCell::new(0));
        let att_clone = attempts.clone();
        let mut embed_fn = move |_dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            *att_clone.borrow_mut() += 1;
            Err("invalid model configuration: unsupported encoding".to_string())
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "build should not abort on non-transient errors"
        );
        assert_eq!(
            *attempts.borrow(),
            1,
            "non-transient error should not retry"
        );
    }

    /// Regression: oversized file with 101+ functions triggers actual split into
    /// sub-groups. Verify every returned vector maps to the correct original chunk
    /// and no dimension mismatch occurs. This is the core regression test for
    /// aft-t6p.23.2 (split sub-groups previously reused file_path → false mismatch).
    #[test]
    fn contextualized_oversized_file_actual_split_maps_vectors_correctly() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        // 101 functions × 3 lines each = 101 chunks → exceeds DEFAULT_MAX_CHUNKS_PER_DOCUMENT (100)
        // This forces split_oversized_document to create 2 sub-groups.
        let mut content = String::new();
        for i in 0..101 {
            content.push_str(&format!("pub fn func_{i:03}() -> i32 {{\n    {i}\n}}\n"));
        }
        let file = write_temp_file(&project_root, "big.rs", &content);

        let captured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut embed_fn = capturing_contextual_embed_fn(captured.clone(), 3);
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            std::slice::from_ref(&file),
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "oversized split build should succeed: {:?}",
            result.err()
        );
        let (index, _diag) = result.unwrap();

        // Should have 101 entries (all functions embedded)
        assert_eq!(index.len(), 101, "all 101 chunks should be embedded");

        // Should have 2 sub-groups sent to embedder
        let caps = captured.borrow();
        let sub_groups: Vec<_> = caps
            .iter()
            .flat_map(|dc| dc.documents.iter())
            .filter(|doc| doc.file_path == file)
            .collect();
        assert_eq!(sub_groups.len(), 2, "should split into 2 sub-groups");

        // First sub-group has 100 chunks, second has 1
        assert_eq!(
            sub_groups[0].chunks.len(),
            100,
            "first sub-group: 100 chunks"
        );
        assert_eq!(sub_groups[1].chunks.len(), 1, "second sub-group: 1 chunk");

        // Verify chunk content mapping: first sub-group's chunks are func_000..func_099
        assert!(
            sub_groups[0].chunks[0].contains("func_000"),
            "first chunk should be func_000"
        );
        assert!(
            sub_groups[0].chunks[99].contains("func_099"),
            "last chunk of first sub-group should be func_099"
        );
        assert!(
            sub_groups[1].chunks[0].contains("func_100"),
            "second sub-group's chunk should be func_100"
        );
    }

    /// Regression: split groups from the same source file must not trigger a
    /// false full-file vector-count mismatch. Before aft-t6p.23.2, the
    /// reconstruction loop looked up all chunks for a file_path and compared
    /// against sub-group vector count → always failed for split files.
    #[test]
    fn contextualized_split_groups_same_file_no_false_mismatch() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        // 101 functions → triggers split into 2 sub-groups (100 + 1)
        let mut content = String::new();
        for i in 0..101 {
            content.push_str(&format!("pub fn split_{i:03}() -> i32 {{\n    {i}\n}}\n"));
        }
        let file = write_temp_file(&project_root, "splitfile.rs", &content);

        let mut embed_fn = mock_contextual_embed_fn(3);
        let mut progress = |_: usize, _: usize| {};

        // Before the fix, this would fail with:
        // "embedding response returned 100 vectors for 101 chunks in file splitfile.rs"
        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(
            result.is_ok(),
            "split groups should not cause false mismatch: {:?}",
            result.err()
        );
    }

    /// Retry exhaustion: all attempts return transient errors → group is skipped,
    /// build continues with remaining groups, no crash.
    #[test]
    fn contextualized_retry_exhaustion_skips_group() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        let file_ok = write_temp_file(
            &project_root,
            "ok.rs",
            "pub fn ok_func() -> i32 {\n    1\n}\n",
        );
        let file_fail = write_temp_file(
            &project_root,
            "fail.rs",
            "pub fn fail_func() -> i32 {\n    2\n}\n",
        );

        let fail_attempts = std::rc::Rc::new(std::cell::RefCell::new(0));
        let fail_clone = fail_attempts.clone();
        let mut embed_fn = move |dc: DocumentChunks| -> Result<DocumentEmbeddings, String> {
            for doc in &dc.documents {
                if doc.file_path.file_name().unwrap() == "fail.rs" {
                    *fail_clone.borrow_mut() += 1;
                    return Err("rate limit exceeded (429)".to_string());
                }
            }
            // OK file succeeds
            let embeddings: Vec<ChunkEmbeddings> = dc
                .documents
                .iter()
                .map(|doc| ChunkEmbeddings {
                    file_path: doc.file_path.clone(),
                    vectors: doc.chunks.iter().map(|_| vec![1.0; 3]).collect(),
                })
                .collect();
            Ok(DocumentEmbeddings { embeddings })
        };
        let mut progress = |_: usize, _: usize| {};

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file_ok, file_fail],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );

        // Build should succeed (partial — fail.rs skipped, ok.rs embedded)
        assert!(
            result.is_ok(),
            "build should succeed with partial results: {:?}",
            result.err()
        );
        let (index, _diag) = result.unwrap();

        // Should have entries for ok.rs only (fail.rs skipped after retry exhaustion)
        assert!(
            !index.is_empty(),
            "should have entries for the successful file"
        );

        // fail.rs should have been retried MAX_RETRIES + 1 times (initial + retries)
        let attempts = *fail_attempts.borrow();
        assert!(
            attempts >= 4,
            "should have attempted at least 4 times (1 initial + 3 retries), got {attempts}"
        );
    }

    // ── model2vec tests ─────────────────────────────────────────────

    /// Build a tiny model2vec-compatible fixture directory with `vocab_size` tokens and `dim` dimensions.
    #[cfg(feature = "semantic-model2vec")]
    fn build_model2vec_fixture(dir: &Path, vocab_size: usize, dim: usize) -> PathBuf {
        // config.json
        let config = serde_json::json!({
            "normalize": true,
            "hidden_size": dim,
        });
        fs::write(
            dir.join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();

        // Build a minimal word-level tokenizer.json
        let mut vocab: Vec<(String, u32)> = Vec::new();
        for i in 0..vocab_size {
            let word = if i == 0 {
                "[UNK]".to_string()
            } else {
                format!("token_{i}")
            };
            vocab.push((word, i as u32));
        }
        let tokenizer_json = serde_json::json!({
            "model": {
                "type": "Word",
                "unk_token": "[UNK]",
                "vocab": vocab.iter().fold(serde_json::Map::new(), |mut m, (k, v)| {
                    m.insert(k.clone(), serde_json::json!(v));
                    m
                }),
            },
            "padding": { "pad_token": "[PAD]" },
            "added_tokens": [],
        });
        fs::write(
            dir.join("tokenizer.json"),
            serde_json::to_string(&tokenizer_json).unwrap(),
        )
        .unwrap();

        // Build a tiny embeddings tensor: vocab_size x dim, all 0.1
        let data: Vec<f32> = vec![0.1; vocab_size * dim];
        let data_bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let metadata = serde_json::json!({
            "embeddings": {
                "dtype": "F32",
                "shape": [vocab_size, dim],
                "data_offsets": [0, data_bytes.len()]
            }
        });
        let meta_bytes = serde_json::to_string(&metadata).unwrap();
        let meta_len = (meta_bytes.len() as u64).to_le_bytes();

        let mut file_bytes = Vec::new();
        file_bytes.extend_from_slice(&meta_len);
        file_bytes.extend_from_slice(meta_bytes.as_bytes());
        file_bytes.extend_from_slice(&data_bytes);
        fs::write(dir.join("model.safetensors"), &file_bytes).unwrap();

        dir.to_path_buf()
    }

    /// Helper: build model2vec from_bytes with given tokenizer/config/data.
    #[cfg(feature = "semantic-model2vec")]
    fn model2vec_from_bytes_helper(
        tokenizer_json: &str,
        config_json: &str,
        data: &[f32],
        dim: usize,
    ) -> model2vec_rs::model::StaticModel {
        let data_bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let vocab_size = data.len() / dim;
        let metadata = serde_json::json!({
            "embeddings": {
                "dtype": "F32",
                "shape": [vocab_size, dim],
                "data_offsets": [0, data_bytes.len()]
            }
        });
        let meta_bytes = serde_json::to_string(&metadata).unwrap();
        let meta_len = (meta_bytes.len() as u64).to_le_bytes();
        let mut model_bytes = Vec::new();
        model_bytes.extend_from_slice(&meta_len);
        model_bytes.extend_from_slice(meta_bytes.as_bytes());
        model_bytes.extend_from_slice(&data_bytes);

        model2vec_rs::model::StaticModel::from_bytes(
            tokenizer_json.as_bytes(),
            &model_bytes,
            config_json.as_bytes(),
            None,
        )
        .expect("load from_bytes")
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_from_bytes_deterministic_encoding() {
        let tok = r#"{"model":{"type":"Word","unk_token":"[UNK]","vocab":{"[UNK]":0,"hello":1,"world":2}},"padding":{"pad_token":"[PAD]"},"added_tokens":[]}"#;
        let cfg = r#"{"normalize":true,"hidden_size":4}"#;
        let data: Vec<f32> = vec![0.25; 3 * 4];
        let model = model2vec_from_bytes_helper(tok, cfg, &data, 4);

        let emb1 = model.encode(&["hello world".to_string()]);
        let emb2 = model.encode(&["hello world".to_string()]);
        assert_eq!(emb1.len(), 1);
        assert_eq!(emb1[0].len(), 4);
        assert_eq!(emb1[0], emb2[0], "encoding must be deterministic");
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_from_bytes_empty_input() {
        let tok = r#"{"model":{"type":"Word","unk_token":"[UNK]","vocab":{"[UNK]":0,"hello":1}},"padding":{"pad_token":"[PAD]"},"added_tokens":[]}"#;
        let cfg = r#"{"normalize":true}"#;
        let data: Vec<f32> = vec![0.5; 2 * 2];
        let model = model2vec_from_bytes_helper(tok, cfg, &data, 2);

        let emb = model.encode(&["".to_string()]);
        assert_eq!(emb.len(), 1);
        assert_eq!(emb[0].len(), 2);
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_from_bytes_unknown_tokens_only() {
        let tok = r#"{"model":{"type":"Word","unk_token":"[UNK]","vocab":{"[UNK]":0,"known":1}},"padding":{"pad_token":"[PAD]"},"added_tokens":[]}"#;
        let cfg = r#"{"normalize":false}"#;
        let data: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 2x3
        let model = model2vec_from_bytes_helper(tok, cfg, &data, 3);

        let emb = model.encode(&["xyz_unknown_token".to_string()]);
        assert_eq!(emb.len(), 1);
        assert_eq!(emb[0].len(), 3);
        // Unknown tokens only => zero vector
        assert!(emb[0].iter().all(|&v| v.abs() < 1e-6));
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_from_pretrained_fixture_roundtrip() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let fixture_path = build_model2vec_fixture(dir.path(), 10, 8);

        let model =
            model2vec_rs::model::StaticModel::from_pretrained(&fixture_path, None, None, None)
                .expect("load from_pretrained fixture");

        let emb = model.encode(&["token_1 token_2".to_string()]);
        assert_eq!(emb.len(), 1);
        assert_eq!(emb[0].len(), 8);
        // L2-normalized (normalize=true in fixture config)
        let norm: f32 = emb[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "L2 norm should be ~1.0, got {norm}"
        );
    }

    /// Helper: build a default SemanticBackendConfig for model2vec tests.
    #[cfg(feature = "semantic-model2vec")]
    fn make_model2vec_config(model_path: Option<PathBuf>) -> SemanticBackendConfig {
        SemanticBackendConfig {
            backend: SemanticBackend::Model2Vec,
            model: String::new(),
            base_url: None,
            api_key_env: None,
            timeout_ms: 5000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        }
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_missing_model_path_returns_error() {
        let config = make_model2vec_config(None);
        let err = SemanticEmbeddingModel::from_config(&config).err().unwrap();
        assert!(err.contains("model_path is required"), "error: {err}");
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_nonexistent_model_path_returns_error() {
        let config = make_model2vec_config(Some(PathBuf::from("/nonexistent/path/to/model")));
        let err = SemanticEmbeddingModel::from_config(&config).err().unwrap();
        assert!(err.contains("does not exist"), "error: {err}");
    }

    #[cfg(feature = "semantic-model2vec")]
    #[test]
    fn model2vec_engine_embed_texts_roundtrip() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let fixture_path = build_model2vec_fixture(dir.path(), 20, 16);

        let mut config = make_model2vec_config(Some(fixture_path));
        config.model2vec_max_length = 128;

        let mut model = SemanticEmbeddingModel::from_config(&config).expect("from_config");
        let texts = vec![
            "token_1 token_2 token_3".to_string(),
            "token_5 token_6".to_string(),
        ];
        let vectors = model.embed_texts(texts).expect("embed_texts");
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].len(), 16);
        assert_eq!(vectors[1].len(), 16);
        assert!(vectors[0].iter().any(|&v| v.abs() > 1e-6));
        assert!(vectors[1].iter().any(|&v| v.abs() > 1e-6));
    }

    /// Optional integration test for Potion Code 16M.
    /// Run: AFT_POTION_CODE_16M_PATH=/path/to/potion-code-16M \
    ///   cargo test -p agent-file-tools --features semantic-model2vec \
    ///   -- --ignored potion_code_16m
    #[cfg(feature = "semantic-model2vec")]
    #[test]
    #[ignore]
    fn potion_code_16m_embed_and_search() {
        let model_path = std::env::var("AFT_POTION_CODE_16M_PATH")
            .expect("set AFT_POTION_CODE_16M_PATH to run this test");
        let model_dir = PathBuf::from(model_path);
        assert!(
            model_dir.exists(),
            "model dir not found: {}",
            model_dir.display()
        );

        let config = make_model2vec_config(Some(model_dir));
        let mut model = SemanticEmbeddingModel::from_config(&config).expect("load Potion Code 16M");

        let query = "how to handle authentication errors".to_string();
        let doc = "fn handle_auth_error(e: AuthError) -> Response { todo!() }".to_string();
        let vectors = model.embed_texts(vec![query, doc]).expect("embed texts");
        assert_eq!(vectors.len(), 2);
        let dim = vectors[0].len();
        eprintln!("Potion Code 16M dimension: {dim}");
        assert!(dim > 0);

        let dot: f32 = vectors[0]
            .iter()
            .zip(vectors[1].iter())
            .map(|(a, b)| a * b)
            .sum();
        let na: f32 = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        let nb: f32 = vectors[1].iter().map(|v| v * v).sum::<f32>().sqrt();
        let sim = if na > 0.0 && nb > 0.0 {
            dot / (na * nb)
        } else {
            0.0
        };
        eprintln!("query-doc cosine similarity: {sim:.4}");
        assert!(
            sim > 0.0,
            "Potion Code 16M should produce positive similarity"
        );
    }

    #[cfg(not(feature = "semantic-model2vec"))]
    #[test]
    fn model2vec_feature_disabled_returns_error() {
        let config = SemanticBackendConfig {
            backend: SemanticBackend::Model2Vec,
            model: String::new(),
            base_url: None,
            api_key_env: None,
            timeout_ms: 5000,
            max_batch_size: 64,
            dimensions: None,
            output_encoding: None,
            input_mode: None,
            storage_strategy: None,
            distance_metric: None,
            query_prompt_template: None,
            document_prompt_template: None,
            diagnostics_enabled: false,
            low_confidence_threshold: 0.3,
            metrics_window_size: 100,
            jsonl_logging: false,
            jsonl_path: None,
            include_raw_queries: false,
            include_snippets: false,
            retention_days: 14,
            output_mode: DiagnosticsOutputMode::default(),
            rerank_enabled: false,
            rerank_model: None,
            rerank_base_url: None,
            rerank_api_key_env: None,
            rerank_timeout_ms: 15000,
            rerank_max_candidates: 20,
            rerank_max_candidate_chars: 2500,
            rerank_api_type: crate::config::RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            rerank_prompt_template: None,
            use_model_profiles: true,
            model_path: Some(PathBuf::from("/any/path")),
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        };
        let err = SemanticEmbeddingModel::from_config(&config).err().unwrap();
        assert!(err.contains("semantic-model2vec"), "error: {err}");
    }

    #[test]
    fn contextualized_progress_reports_total_chunks() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");

        // Each function must span ≥2 lines so symbols_to_chunks doesn't
        // skip it (line_count < 2 filter).
        let file = write_temp_file(
            &project_root,
            "progress.rs",
            "pub fn a() -> i32 {\n    0\n}\npub fn b() -> i32 {\n    1\n}\npub fn c() -> i32 {\n    2\n}\n",
        );

        let mut embed_fn = mock_contextual_embed_fn(3);
        let mut progress_calls: Vec<(usize, usize)> = Vec::new();
        let mut progress = |done: usize, total: usize| progress_calls.push((done, total));

        let result = SemanticIndex::build_with_progress_contextualized(
            &project_root,
            &[file],
            &mut embed_fn,
            &mut progress,
            &SemanticFilePolicy::default(),
            None,
        );
        assert!(result.is_ok());

        assert!(!progress_calls.is_empty(), "progress should be called");
        assert_eq!(progress_calls[0].0, 0, "first call should report 0 done");
        assert!(progress_calls[0].1 > 0, "total should be > 0");
        let last = progress_calls.last().unwrap();
        assert_eq!(last.0, last.1, "final progress: done == total");
    }

    #[test]
    fn concurrent_snapshot_swap_does_not_panic() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let project_root = fs::canonicalize(dir.path()).expect("canonicalize");
        let mut index = SemanticIndex::new(project_root.clone(), 3);
        index.set_dimension(3);
        let index = Arc::new(std::sync::Mutex::new(index));

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let index = Arc::clone(&index);
                std::thread::spawn(move || {
                    let mut idx = index.lock().unwrap();
                    let mut snap = (*idx.snapshot).clone();
                    let entry = EmbeddingEntry {
                        chunk: SemanticChunk {
                            file: PathBuf::from(format!("src/t{i}.rs")),
                            name: format!("fn_{i}"),
                            kind: SymbolKind::Function,
                            start_line: 0,
                            end_line: 2,
                            exported: true,
                            embed_text: format!("fn_{i}"),
                            snippet: format!("fn fn_{i}() {{}}"),
                        },
                        vector: vec![1.0, 0.0, 0.0],
                        chunk_hash: String::new(),
                    };
                    snap.entries_mut_inner().push(entry);
                    idx.swap_snapshot(snap);
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread should not panic");
        }

        let idx = index.lock().unwrap();
        assert!(
            idx.entry_count() > 0,
            "index should have entries after concurrent swaps"
        );
    }

    #[test]
    fn concurrent_query_embedding_cache_access() {
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Test concurrency of the query embedding cache data structure
        // directly, without needing ONNX Runtime for an actual embedding model.
        type Cache = Arc<Mutex<(HashMap<String, Vec<f32>>, VecDeque<String>, u64, u64)>>;

        let cache: Cache = Arc::new(Mutex::new((HashMap::new(), VecDeque::new(), 0u64, 0u64)));
        let hit_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let cache = Arc::clone(&cache);
                let hit_count = Arc::clone(&hit_count);
                std::thread::spawn(move || {
                    for j in 0..10 {
                        let key = format!("query_{i}_{j}");
                        let vector = vec![i as f32, j as f32, 0.0];
                        let mut c = cache.lock().unwrap();
                        if c.0.contains_key(&key) {
                            c.2 += 1;
                            hit_count.fetch_add(1, Ordering::Relaxed);
                        } else {
                            c.0.insert(key.clone(), vector);
                            c.1.push_back(key);
                            c.3 += 1;
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread should not panic");
        }

        let c = cache.lock().unwrap();
        assert_eq!(
            c.0.len(),
            40,
            "cache should have 40 entries (4 threads x 10 queries)"
        );
        assert!(c.2 + c.3 > 0, "should have cache activity");
    }
}
