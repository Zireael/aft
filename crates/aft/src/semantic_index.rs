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
use crate::{slog_info, slog_warn};

use fastembed::{EmbeddingModel as FastembedEmbeddingModel, InitOptions, TextEmbedding};
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::SystemTime;
use tree_sitter::Parser;
use url::Url;

const DEFAULT_DIMENSION: usize = 384;
const MAX_ENTRIES: usize = 1_000_000;
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

/// Compute a stable hash for a prompt template. Returns empty string when None.
pub fn prompt_template_hash(template: Option<&str>) -> String {
    template.map_or(String::new(), |t| {
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
    Fastembed(TextEmbedding),
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

fn is_retryable_embedding_error(error: &reqwest::Error) -> bool {
    error.is_connect()
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
                return Err(format!("{backend_label} request failed: {error}"));
            }
        };

        let status = response.status();
        let raw = match response.text() {
            Ok(raw) => raw,
            Err(error) => {
                if !last_attempt && is_retryable_embedding_error(&error) {
                    sleep_before_embedding_retry(attempt_index);
                    continue;
                }
                return Err(format!("{backend_label} response read failed: {error}"));
            }
        };

        if status.is_success() {
            return Ok(raw);
        }

        if !last_attempt && is_retryable_embedding_status(status) {
            sleep_before_embedding_retry(attempt_index);
            continue;
        }

        return Err(format!(
            "{backend_label} request failed (HTTP {}): {}",
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
                SemanticEmbeddingEngine::Fastembed(initialize_text_embedding(&model)?)
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
            SemanticEmbeddingEngine::Fastembed(model) => {
                let vectors = model
                    .embed(vec!["semantic index fingerprint probe".to_string()], None)
                    .map_err(|error| format_embedding_init_error(error.to_string()))?;
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
    ) -> Result<Vec<f32>, String> {
        let prompt_hash = prompt_template_hash(query_prompt_template);
        let cache_key = if prompt_hash.is_empty() {
            query.to_string()
        } else {
            format!("{prompt_hash}:{query}")
        };

        if let Some(vector) = self.query_embedding_cache.get(&cache_key) {
            self.query_embedding_cache_hits += 1;
            return Ok(vector.clone());
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

        Ok(vector)
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
            SemanticEmbeddingEngine::Fastembed(model) => model
                .embed(texts, None::<usize>)
                .map_err(|error| format_embedding_init_error(error.to_string()))
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

pub fn initialize_text_embedding(model: &str) -> Result<TextEmbedding, String> {
    // Pre-validate before ort can panic on a bad library
    pre_validate_onnx_runtime()?;

    let selected_model = match model {
        "all-MiniLM-L6-v2" | "all-minilm-l6-v2" => FastembedEmbeddingModel::AllMiniLML6V2,
        _ => {
            return Err(format!(
                "unsupported fastembed model '{}'. Supported: all-MiniLM-L6-v2",
                model
            ))
        }
    };

    TextEmbedding::try_new(InitOptions::new(selected_model)).map_err(format_embedding_init_error)
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

fn format_embedding_init_error(error: impl Display) -> String {
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
#[derive(Debug)]
pub struct SemanticIndex {
    snapshot: Arc<SemanticIndexSnapshot>,
    lifecycle: SemanticIndexLifecycle,
    last_error: Option<String>,
    fingerprint: Option<SemanticIndexFingerprint>,
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
                    // Skip expected/normal skip reasons silently
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

        (chunks, file_metadata)
    }

    fn build_from_chunks<F, P>(
        project_root: &Path,
        chunks: Vec<SemanticChunk>,
        file_metadata: HashMap<PathBuf, IndexedFileMetadata>,
        embed_fn: &mut F,
        max_batch_size: usize,
        mut progress: Option<&mut P>,
    ) -> Result<SemanticIndexSnapshot, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        debug_assert!(project_root.is_absolute());
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
            Self::collect_chunks(project_root, files, &SemanticFilePolicy::default());
        let snapshot = Self::build_from_chunks(
            project_root,
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Option::<&mut fn(usize, usize)>::None,
        )?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint: None,
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
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        let mut files = files.to_vec();
        Self::sort_files_by_priority(&mut files);
        let (chunks, file_mtimes) = Self::collect_chunks(project_root, &files, file_policy);
        let total_chunks = chunks.len();
        progress(0, total_chunks);
        let snapshot = Self::build_from_chunks(
            project_root,
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Some(progress),
        )?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint: None,
        })
    }

    /// Build the semantic index using a contextualized document-chunk embedding
    /// function. Groups chunks by source document so the embedding provider can
    /// use surrounding chunks as context.
    pub fn build_with_progress_contextualized<F, P>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        progress: &mut P,
        file_policy: &SemanticFilePolicy,
    ) -> Result<Self, String>
    where
        F: FnMut(DocumentChunks) -> Result<DocumentEmbeddings, String>,
        P: FnMut(usize, usize),
    {
        let mut files = files.to_vec();
        Self::sort_files_by_priority(&mut files);
        let (chunks, file_metadata) = Self::collect_chunks(project_root, &files, file_policy);
        let total_chunks = chunks.len();
        progress(0, total_chunks);

        if chunks.is_empty() {
            return Ok(Self {
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
            });
        }

        // Group chunks by file path
        let mut docs_map: HashMap<PathBuf, Vec<SemanticChunk>> = HashMap::new();
        for chunk in chunks {
            docs_map.entry(chunk.file.clone()).or_default().push(chunk);
        }

        let mut documents: Vec<PerDocumentChunks> = Vec::with_capacity(docs_map.len());
        for (path, chunks) in &docs_map {
            let title = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let chunk_texts: Vec<String> = chunks.iter().map(|c| c.embed_text.clone()).collect();
            documents.push(PerDocumentChunks {
                file_path: path.clone(),
                title,
                chunks: chunk_texts,
            });
        }

        let doc_embeddings = embed_fn(DocumentChunks { documents })?;

        let mut entries: Vec<EmbeddingEntry> = Vec::with_capacity(total_chunks);
        let mut expected_dimension: Option<usize> = None;
        let mut done = 0;

        for emb in doc_embeddings.embeddings.into_iter() {
            let file_chunks = docs_map.get(&emb.file_path).ok_or_else(|| {
                format!(
                    "embedding response returned unknown file path: {}",
                    emb.file_path.display()
                )
            })?;

            if emb.vectors.len() != file_chunks.len() {
                return Err(format!(
                    "embedding response returned {} vectors for {} chunks in file {}",
                    emb.vectors.len(),
                    file_chunks.len(),
                    emb.file_path.display()
                ));
            }

            for (chunk, vector) in file_chunks.iter().zip(emb.vectors) {
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
                    chunk_hash: compute_chunk_hash(&chunk),
                });
                done += 1;
                progress(done, total_chunks);
            }
        }

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
        Ok(Self {
            snapshot: Arc::new(new_snapshot),
            lifecycle: SemanticIndexLifecycle::Ready,
            last_error: None,
            fingerprint: None,
        })
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

        let (chunks, fresh_metadata) = Self::collect_chunks(project_root, &to_embed, file_policy);

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
    let mut text = format!(
        "name:{name} file:{} kind:{} name:{name}",
        relative, kind_label
    );

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
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
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

fn split_large_chunk(chunk: &SemanticChunk) -> Vec<SemanticChunk> {
    let mut result = Vec::new();
    let mut current_body = String::new();
    let mut chunk_start = chunk.start_line;
    let mut current_lines: u32 = 0;
    let mut total_lines: u32 = 0;

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
        total_lines += para.lines().count() as u32;
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
            end_line: chunk.start_line + total_lines,
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
}

#[cfg(test)]
mod fingerprint_invalidation_tests {
    use super::tests::start_mock_http_server;
    use super::*;

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
}
