use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::harness::Harness;

/// Runtime configuration for the aft process.
///
/// Holds project-scoped settings and tuning knobs. Values are set at startup
/// and remain immutable for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticBackend {
    Fastembed,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    Ollama,
    /// Perplexity contextualized embeddings — sends nested document/chunk
    /// arrays and returns one embedding per chunk using surrounding context.
    #[serde(rename = "perplexity")]
    Perplexity,
    /// Local model2vec static embeddings (e.g. Potion Code 16M).
    /// Requires the `semantic-model2vec` Cargo feature to be enabled.
    #[serde(rename = "model2vec")]
    Model2Vec,
}

impl SemanticBackend {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Fastembed => "fastembed",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Ollama => "ollama",
            Self::Perplexity => "perplexity",
            Self::Model2Vec => "model2vec",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fastembed" => Some(Self::Fastembed),
            "openai_compatible" => Some(Self::OpenAiCompatible),
            "ollama" => Some(Self::Ollama),
            "perplexity" => Some(Self::Perplexity),
            "model2vec" => Some(Self::Model2Vec),
            _ => None,
        }
    }
}

/// The encoding format returned by the embedding provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputEncoding {
    /// Standard float32 embeddings (default for most providers).
    Float,
    /// Base64-encoded signed int8 embeddings (e.g. Perplexity, some OpenAI-compatible).
    #[serde(rename = "base64_int8")]
    Base64Int8,
    /// Base64-encoded binary packed embeddings (e.g. Perplexity binary).
    #[serde(rename = "base64_binary")]
    Base64Binary,
}

impl OutputEncoding {
    /// Default encoding for a given backend.
    pub fn default_for_backend(backend: SemanticBackend) -> Self {
        match backend {
            SemanticBackend::Fastembed => Self::Float,
            SemanticBackend::OpenAiCompatible => Self::Float,
            SemanticBackend::Ollama => Self::Float,
            SemanticBackend::Perplexity => Self::Float,
            SemanticBackend::Model2Vec => Self::Float,
        }
    }
}

/// How embedding inputs are structured for the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// Simple array of text strings.
    #[serde(rename = "flat_texts")]
    FlatTexts,
    /// Grouped document-chunk inputs (e.g. Perplexity contextualized).
    #[serde(rename = "document_chunks")]
    DocumentChunks,
}

impl InputMode {
    pub fn default_for_backend(backend: SemanticBackend) -> Self {
        match backend {
            SemanticBackend::Fastembed => Self::FlatTexts,
            SemanticBackend::OpenAiCompatible => Self::FlatTexts,
            SemanticBackend::Ollama => Self::FlatTexts,
            SemanticBackend::Perplexity => Self::DocumentChunks,
            SemanticBackend::Model2Vec => Self::FlatTexts,
        }
    }
}

/// How vectors are stored in the local index after retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageStrategy {
    /// Native f32 vectors stored as-is (default for Float output encoding).
    #[serde(rename = "native_f32")]
    NativeF32,
    /// Decode int8 to f32 and L2-normalize before storage (compatibility path for base64_int8).
    #[serde(rename = "decode_normalize_f32")]
    DecodeNormalizeF32,
    /// Store as packed binary (bit) vectors for Hamming distance search.
    #[serde(rename = "binary_packed")]
    BinaryPacked,
}

impl StorageStrategy {
    pub fn default_for_backend(backend: SemanticBackend) -> Self {
        match backend {
            SemanticBackend::Fastembed => Self::NativeF32,
            SemanticBackend::OpenAiCompatible => Self::NativeF32,
            SemanticBackend::Ollama => Self::NativeF32,
            SemanticBackend::Perplexity => Self::NativeF32,
            SemanticBackend::Model2Vec => Self::NativeF32,
        }
    }
}

/// Distance metric for similarity search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceMetric {
    /// Resolve from provider/model profile and storage strategy.
    #[serde(rename = "auto")]
    Auto,
    /// Cosine similarity (default for normalized dense vectors).
    Cosine,
    /// Dot product.
    #[serde(rename = "dot_product")]
    DotProduct,
    /// Euclidean distance.
    Euclidean,
    /// Hamming distance (for binary vectors).
    Hamming,
}

impl DistanceMetric {
    pub fn default_for_backend(backend: SemanticBackend) -> Self {
        match backend {
            SemanticBackend::Fastembed => Self::Auto,
            SemanticBackend::OpenAiCompatible => Self::Auto,
            SemanticBackend::Ollama => Self::Auto,
            SemanticBackend::Perplexity => Self::Cosine,
            SemanticBackend::Model2Vec => Self::Cosine,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticBackendConfig {
    pub backend: SemanticBackend,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    pub max_batch_size: usize,
    /// Optional user-requested embedding dimensions. When set, the provider
    /// is asked to return vectors of this dimension (if supported).
    /// When unset, the provider's default dimension is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    /// Optional output encoding format from the provider.
    /// Defaults to `float` for all built-in backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_encoding: Option<OutputEncoding>,
    /// Optional input mode for the provider.
    /// Defaults to `flat_texts` for all built-in backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<InputMode>,
    /// Optional storage strategy for how vectors are stored locally.
    /// Defaults to `native_f32` for all built-in backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_strategy: Option<StorageStrategy>,
    /// Optional distance metric for similarity search.
    /// Defaults to `auto` which resolves from provider/model profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_metric: Option<DistanceMetric>,
    /// Optional template applied to user queries before embedding.
    /// Use `{query}` as the placeholder for the raw query text.
    /// Example: "Instruct: Given a code search query, retrieve relevant code snippet that answer the query\nQuery: {query}"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_prompt_template: Option<String>,
    /// Optional template applied to document/chunk text before embedding.
    /// Use `{text}` as the placeholder for the raw chunk text.
    /// Example: "Represent this code snippet for retrieval: {text}"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_prompt_template: Option<String>,
    /// Enable per-query search diagnostics collection (default: false).
    #[serde(default)]
    pub diagnostics_enabled: bool,
    /// Score threshold below which results are flagged as low-confidence (default: 0.3).
    #[serde(default = "default_low_confidence_threshold")]
    pub low_confidence_threshold: f32,
    /// Number of recent queries to retain for aggregate metrics (default: 100).
    #[serde(default = "default_metrics_window_size")]
    pub metrics_window_size: usize,
    /// Write per-query diagnostics as JSONL to a local file (default: false).
    #[serde(default)]
    pub jsonl_logging: bool,
    /// Override path for the JSONL diagnostics log.
    /// Defaults to `<AFT_CACHE_DIR>/semantic_diagnostics.jsonl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonl_path: Option<PathBuf>,
    /// Include the raw query text in JSONL diagnostics (default: false).
    /// When false, only the query hash is recorded.
    #[serde(default)]
    pub include_raw_queries: bool,
    /// Include code snippets in JSONL diagnostics (default: false).
    #[serde(default)]
    pub include_snippets: bool,
    /// Number of days to retain JSONL diagnostics before cleanup (default: 14).
    #[serde(default = "default_jsonl_retention_days")]
    pub retention_days: u32,
    /// How much diagnostic detail to include in `aft_search` tool output (default: minimal).
    #[serde(default)]
    pub output_mode: DiagnosticsOutputMode,
    /// Enable optional reranking via an OpenAI-compatible chat endpoint (default: false).
    /// When enabled, `aft_search` overfetches candidates and reranks them.
    /// Falls back to original order on failure.
    #[serde(default)]
    pub rerank_enabled: bool,
    /// Override model for reranking. Defaults to `codellama/codellama:7b-instruct` if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_model: Option<String>,
    /// Base URL for reranker (OpenAI-compatible /v1/chat/completions endpoint).
    /// Falls back to `base_url` if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_base_url: Option<String>,
    /// Env var name for reranker API key. Falls back to `api_key_env` if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_api_key_env: Option<String>,
    /// Timeout in ms for reranker requests (default: 15000).
    #[serde(default = "default_rerank_timeout_ms")]
    pub rerank_timeout_ms: u64,
    /// Max number of candidates to send to the reranker per query (default: 20).
    #[serde(default = "default_rerank_max_candidates")]
    pub rerank_max_candidates: usize,
    /// Max characters per candidate snippet sent to reranker (default: 2500).
    #[serde(default = "default_rerank_max_candidate_chars")]
    pub rerank_max_candidate_chars: usize,
    /// Reranker API format: `"chat"` for LLM-based chat completions (default),
    /// `"rerank"` for cross-encoder `/v1/rerank` endpoints (e.g. GTE-Reranker-Modernbert).
    #[serde(default = "default_rerank_api_type")]
    pub rerank_api_type: RerankApiType,
    /// Max characters per candidate snippet for cross-encoder rerankers (default: 512).
    /// Cross-encoders have tighter context windows than chat models.
    #[serde(default = "default_rerank_max_candidate_chars_cross_encoder")]
    pub rerank_max_candidate_chars_cross_encoder: usize,
    /// Local filesystem path to a model2vec model directory (e.g. `minishlab/potion-code-16M`).
    /// Required when `backend = "model2vec"`. Must contain `config.json`, `tokenizer.json`,
    /// and `model.safetensors`. No remote downloads are performed.
    /// **USER-ONLY trust boundary** — project-level config cannot set this field;
    /// the OpenCode plugin strips it from project config before merging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
    /// Max token length for model2vec truncation (default: 512).
    /// **USER-ONLY trust boundary** — project-level config cannot set this field.
    #[serde(default = "default_model2vec_max_length")]
    pub model2vec_max_length: usize,
    /// Maximum number of results returned per file after hybrid fusion (default: 2).
    /// Prevents a single dense module from dominating search results.
    #[serde(default = "default_max_results_per_file")]
    pub max_results_per_file: usize,
    /// Maximum number of project files to semantically index. Guards local
    /// embedding memory on huge project roots; remote backends can raise it.
    #[serde(default = "default_max_semantic_files")]
    pub max_files: usize,
    /// Max tokens per embedding request for remote backends (default: 512).
    /// When a symbol exceeds this limit, it is chunked before embedding.
    /// Local backends (Fastembed, Model2Vec) already truncate internally.
    /// Per-model defaults: text-embedding-3-small: 8191, text-embedding-ada-002: 8191,
    /// BAAI/bge-large-en: 512. Set to 0 to disable chunking.
    #[serde(default = "default_max_embed_tokens")]
    pub max_embed_tokens: usize,
    /// Number of overlapping tokens between chunks when splitting large symbols (default: 100).
    /// Overlap preserves boundary context across chunks.
    #[serde(default = "default_chunk_overlap_tokens")]
    pub chunk_overlap_tokens: usize,
}

/// How much diagnostic detail to include in the tool output text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsOutputMode {
    /// No diagnostics in tool output.
    Off,
    /// Only warnings that change result interpretation (default).
    #[default]
    Minimal,
    /// Include full diagnostics (scores, latency, warnings) in tool output.
    Verbose,
}

fn default_low_confidence_threshold() -> f32 {
    0.3
}

fn default_jsonl_retention_days() -> u32 {
    14
}

fn default_metrics_window_size() -> usize {
    100
}

fn default_rerank_timeout_ms() -> u64 {
    15000
}

fn default_rerank_max_candidates() -> usize {
    20
}

fn default_rerank_max_candidate_chars() -> usize {
    2500
}

/// Reranker API format type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankApiType {
    /// LLM-based chat completions endpoint (`/v1/chat/completions`).
    /// Expects JSON array of indices in `choices[0].message.content`.
    Chat,
    /// Cross-encoder rerank endpoint (`/v1/rerank`).
    /// Expects `{results: [{index, relevance_score}]}` or provider variants.
    Rerank,
}

impl Default for RerankApiType {
    fn default() -> Self {
        Self::Chat
    }
}

fn default_rerank_api_type() -> RerankApiType {
    RerankApiType::Chat
}

fn default_rerank_max_candidate_chars_cross_encoder() -> usize {
    512
}

fn default_model2vec_max_length() -> usize {
    512
}

fn default_max_results_per_file() -> usize {
    2
}

fn default_max_embed_tokens() -> usize {
    512
}

fn default_chunk_overlap_tokens() -> usize {
    100
}

fn default_max_semantic_files() -> usize {
    20_000
}

impl SemanticBackendConfig {
    /// Returns true if either in-memory metrics or JSONL logging is enabled.
    pub fn diagnostics_enabled(&self) -> bool {
        self.diagnostics_enabled || self.jsonl_logging
    }

    pub fn low_confidence_threshold(&self) -> f32 {
        self.low_confidence_threshold
    }

    pub fn metrics_window_size(&self) -> usize {
        self.metrics_window_size
    }

    pub fn jsonl_logging(&self) -> bool {
        self.jsonl_logging
    }

    pub fn jsonl_path(&self) -> Option<&std::path::Path> {
        self.jsonl_path.as_deref()
    }

    pub fn include_raw_queries(&self) -> bool {
        self.include_raw_queries
    }

    pub fn include_snippets(&self) -> bool {
        self.include_snippets
    }

    pub fn retention_days(&self) -> u32 {
        self.retention_days
    }

    pub fn output_mode(&self) -> DiagnosticsOutputMode {
        self.output_mode
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserServerDef {
    pub id: String,
    pub extensions: Vec<String>,
    pub binary: String,
    pub args: Vec<String>,
    pub root_markers: Vec<String>,
    pub env: HashMap<String, String>,
    pub initialization_options: Option<serde_json::Value>,
    pub disabled: bool,
}

/// Configures which files are considered for semantic indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticFilePolicy {
    /// Index code files (default: true).
    pub include_code: bool,
    /// Index documentation files (default: true).
    pub include_docs: bool,
    /// Index config files (default: false).
    pub include_configs: bool,
    /// Respect .gitignore when walking files (default: true).
    pub respect_gitignore: bool,
    /// Include gitignored docs when `respect_gitignore` is true (default: true).
    pub include_gitignored_docs: bool,
    /// Extra include globs for docs/configs beyond defaults.
    #[serde(default)]
    pub include_globs: Vec<String>,
    /// Exclude globs for junk/output directories and file types.
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    /// Maximum file size in bytes to consider for indexing (default: 1 MiB).
    pub max_file_size_bytes: u64,
    /// Skip binary files by content inspection (default: true).
    pub binary_detection: bool,
    /// Skip files that look auto-generated (default: true).
    pub generated_file_detection: bool,
    /// Docs chunker version — bump when chunking logic changes.
    #[serde(default = "default_docs_chunker_version")]
    pub docs_chunker_version: u8,
    /// Globs that are always included when `include_docs` is true (baked-in, not overridable).
    #[serde(skip)]
    pub(crate) builtin_doc_globs: Vec<String>,
    /// Globs that are always excluded (baked-in, not overridable).
    #[serde(skip)]
    pub(crate) builtin_exclude_globs: Vec<String>,
}

const fn default_docs_chunker_version() -> u8 {
    1
}

impl Default for SemanticFilePolicy {
    fn default() -> Self {
        Self {
            include_code: true,
            include_docs: true,
            include_configs: false,
            respect_gitignore: true,
            include_gitignored_docs: true,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            max_file_size_bytes: 1_048_576, // 1 MiB
            binary_detection: true,
            generated_file_detection: true,
            docs_chunker_version: default_docs_chunker_version(),
            builtin_doc_globs: vec![
                "README.md".into(),
                "README.rst".into(),
                "docs/**/*.md".into(),
                "docs/**/*.rst".into(),
                "adr/**/*.md".into(),
                ".github/**/*.md".into(),
                "CONTRIBUTING.md".into(),
                "CHANGELOG.md".into(),
                "CHANGELOG*.md".into(),
            ],
            builtin_exclude_globs: vec![
                "**/node_modules/**".into(),
                "**/dist/**".into(),
                "**/build/**".into(),
                "**/target/**".into(),
                "**/.next/**".into(),
                "**/.turbo/**".into(),
                "**/.cache/**".into(),
                "**/coverage/**".into(),
                "**/vendor/**".into(),
                "**/.git/**".into(),
                "**/__pycache__/**".into(),
                "**/.tox/**".into(),
                "**/.venv/**".into(),
                "**/venv/**".into(),
                "**/*.min.js".into(),
                "**/*.min.css".into(),
                "**/*.map".into(),
                "**/*.lock".into(),
                "**/*.svg".into(),
                "**/*.png".into(),
                "**/*.jpg".into(),
                "**/*.jpeg".into(),
                "**/*.gif".into(),
                "**/*.ico".into(),
                "**/*.woff".into(),
                "**/*.woff2".into(),
                "**/*.ttf".into(),
                "**/*.eot".into(),
                "**/*.otf".into(),
                "**/*.pdf".into(),
                "**/*.zip".into(),
                "**/*.tar".into(),
                "**/*.gz".into(),
                "**/*.bz2".into(),
                "**/*.xz".into(),
                "**/*.7z".into(),
                "**/*.rar".into(),
                "**/*.wasm".into(),
                "**/*.parquet".into(),
                "**/*.onnx".into(),
                "**/*.bin".into(),
                "**/*.dll".into(),
                "**/*.dylib".into(),
                "**/*.so".into(),
                "**/*.exe".into(),
                "**/*.o".into(),
                "**/*.obj".into(),
                "**/*.a".into(),
                "**/*.lib".into(),
                "**/*.class".into(),
                "**/*.jar".into(),
                "generated/**".into(),
            ],
        }
    }
}

impl Default for SemanticBackendConfig {
    fn default() -> Self {
        Self {
            backend: SemanticBackend::Fastembed,
            model: DEFAULT_SEMANTIC_MODEL.to_string(),
            base_url: None,
            api_key_env: None,
            // Keep the default below the plugin bridge timeout to avoid bridge-killed
            // semantic_search requests when callers do not set an explicit timeout.
            timeout_ms: 25_000,
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
            rerank_api_type: RerankApiType::Chat,
            rerank_max_candidate_chars_cross_encoder: 512,
            model_path: None,
            model2vec_max_length: 512,
            max_results_per_file: 2,
            max_files: 20_000,
            max_embed_tokens: 512,
            chunk_overlap_tokens: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectConfig {
    pub enabled: bool,
}

impl Default for InspectConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

pub const DEFAULT_SEMANTIC_MODEL: &str = "all-MiniLM-L6-v2";

impl Config {
    pub fn semantic_backend_label(&self) -> &'static str {
        self.semantic.backend.as_str()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Root directory of the project being analyzed. `None` if not scoped.
    pub project_root: Option<PathBuf>,
    /// How many levels of call-graph edges to follow during validation (default: 1).
    pub validation_depth: u32,
    /// Hours before a checkpoint expires and is eligible for cleanup (default: 24).
    pub checkpoint_ttl_hours: u32,
    /// Maximum depth for recursive symbol resolution (default: 10).
    pub max_symbol_depth: u32,
    /// Seconds before killing a formatter subprocess (default: 10).
    pub formatter_timeout_secs: u32,
    /// Seconds before killing a type-checker subprocess (default: 30).
    pub type_checker_timeout_secs: u32,
    /// Whether to auto-format files after edits (default: true).
    pub format_on_edit: bool,
    /// Whether to auto-validate files after edits (default: false).
    /// When "syntax", only tree-sitter parse check. When "full", runs type checker.
    pub validate_on_edit: Option<String>,
    /// Per-language formatter overrides. Keys: "typescript", "python", "rust", "go".
    /// Values: "biome", "oxfmt", "prettier", "deno", "ruff", "black", "rustfmt", "goimports", "gofmt", "none".
    pub formatter: HashMap<String, String>,
    /// Per-language type checker overrides. Keys: "typescript", "python", "rust", "go".
    /// Values: "tsc", "tsgo", "biome", "pyright", "ruff", "cargo", "go", "staticcheck", "none".
    pub checker: HashMap<String, String>,
    /// Whether to restrict file operations to within `project_root` (default: false).
    /// When true, write-capable commands reject paths outside the project root.
    pub restrict_to_project_root: bool,
    /// Enable the trigram search index (default: false).
    pub search_index: bool,
    /// Enable semantic search (default: false).
    pub semantic_search: bool,
    /// Whether the plugin registered the `aft_search` tool for this surface.
    pub aft_search_registered: bool,
    /// Enable the persisted callgraph store substrate (default: false).
    pub callgraph_store: bool,
    /// Enable experimental bash command rewriting (default: false).
    pub experimental_bash_rewrite: bool,
    /// Enable experimental bash command compression (default: false).
    pub experimental_bash_compress: bool,
    /// Enable experimental bash background execution (default: false).
    pub experimental_bash_background: bool,
    /// Maximum number of background bash tasks allowed to run concurrently (default: 8).
    pub max_background_bash_tasks: usize,
    /// Emit reminders for long-running bash tasks (default: true).
    pub bash_long_running_reminder_enabled: bool,
    /// Milliseconds between long-running bash reminders (default: 10 minutes).
    pub bash_long_running_reminder_interval_ms: u64,
    /// Enable OpenCode-style bash permission prompts (default: false).
    pub bash_permissions: bool,
    /// Maximum file size to fully index in bytes (default: 1MB).
    pub search_index_max_file_size: u64,
    /// Maximum number of source files allowed for call-graph operations
    /// (`callers`, `trace_to`, `trace_data`, `impact`). When a project
    /// exceeds this count the reverse index is not built and those
    /// commands return a `project_too_large` error. Does not affect
    /// `grep`, `glob`, `read`, `edit`, or other non-callgraph features.
    /// Default: 5_000 (matches measured per-op cost ceilings; raise for
    /// very large projects if you accept multi-minute per-call latency).
    pub max_callgraph_files: usize,
    pub semantic: SemanticBackendConfig,
    /// File inclusion/exclusion policy for semantic indexing.
    pub semantic_files: SemanticFilePolicy,
    pub inspect: InspectConfig,
    /// Enable Astral ty as an experimental Python LSP server (default: false).
    pub experimental_lsp_ty: bool,
    /// User-defined LSP servers registered by the OpenCode plugin.
    pub lsp_servers: Vec<UserServerDef>,
    /// Lowercase LSP server IDs disabled by user config.
    pub disabled_lsp: HashSet<String>,
    /// Extra directories to search when resolving LSP binaries.
    /// The plugin populates these from its own auto-install cache (e.g.
    /// `~/.cache/aft/lsp-packages/<pkg>/node_modules/.bin/`) so a binary AFT
    /// installed itself is discoverable without needing it on PATH.
    /// Resolution order: `<project_root>/node_modules/.bin/<bin>` →
    /// `lsp_paths_extra/<bin>` (in order) → PATH via `which`.
    pub lsp_paths_extra: Vec<PathBuf>,
    /// Binary names the hosting plugin knows how to auto-install.
    ///
    /// Built-in LSPs discovered from files only emit missing-binary warnings
    /// when their binary is in this set. User-configured `lsp_servers` keep
    /// warning unconditionally.
    pub lsp_auto_install_binaries: HashSet<String>,
    /// Binary names with plugin-managed auto-installs currently in flight.
    ///
    /// Missing-binary warnings are suppressed while the install is actively
    /// running; install failure reporting is handled by the plugin after the
    /// background work settles.
    pub lsp_inflight_installs: HashSet<String>,
    /// Persistent storage directory for indexes (trigram, semantic).
    /// Set by the plugin to the XDG-compliant path (e.g. ~/.local/share/opencode/storage/plugin/aft/).
    /// Falls back to ~/.cache/aft/ if not set.
    pub storage_dir: Option<PathBuf>,
    /// Allow URL-fetch commands to access private network hosts.
    pub url_fetch_allow_private: bool,
    /// Hosting harness identity supplied by configure.
    #[serde(default)]
    pub harness: Option<Harness>,
    /// Maximum number of (server, file) entries kept in the in-memory
    /// diagnostic cache. Older entries are evicted in LRU order when the
    /// cap is exceeded. Set to 0 to disable the cap entirely.
    /// Default: 5000 (covers very large monorepos with bounded memory).
    pub diagnostic_cache_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            project_root: None,
            validation_depth: 1,
            checkpoint_ttl_hours: 24,
            max_symbol_depth: 10,
            formatter_timeout_secs: 10,
            type_checker_timeout_secs: 30,
            format_on_edit: true,
            validate_on_edit: None,
            formatter: HashMap::new(),
            checker: HashMap::new(),
            // Default to false to match OpenCode's existing permission-based model.
            // The plugin opts into root restriction explicitly when desired.
            restrict_to_project_root: false,
            search_index: false,
            semantic_search: false,
            aft_search_registered: false,
            callgraph_store: false,
            experimental_bash_rewrite: false,
            experimental_bash_compress: false,
            experimental_bash_background: false,
            max_background_bash_tasks: 8,
            bash_long_running_reminder_enabled: true,
            bash_long_running_reminder_interval_ms: 600_000,
            bash_permissions: false,
            search_index_max_file_size: 1_048_576,
            // Projects larger than this skip call-graph reverse index construction.
            //
            // The previous default (20_000) was set by hand-wave to "fits under
            // the 30 s bridge timeout" without measurement. Direct benchmarks
            // showed the cost is super-linear (tree-sitter parse + reverse-index
            // build per file): a 6.8K-file Rust project took 41 s — already past
            // the 60 s per-callgraph-op timeout. At 10 K extrapolated cost is
            // ~80–100 s; at 20 K it's 5+ minutes. So the old default routinely
            // produced "timed out, restarting bridge" rather than a clean
            // `project_too_large` rejection.
            //
            // 5_000 reflects measured reality: at this size, callgraph
            // operations on a real Rust/TS project complete in roughly 30–40 s,
            // matching the per-op timeout budget. Users with bigger projects
            // can raise this knob, but the default should not advertise
            // capabilities that fail in practice. Read/edit/grep/glob/outline/
            // semantic_search/AST/LSP all remain unaffected by this cap —
            // it only gates `aft_navigate` and `aft_refactor op="move"`.
            max_callgraph_files: 5_000,
            semantic: SemanticBackendConfig::default(),
            semantic_files: SemanticFilePolicy::default(),
            inspect: InspectConfig::default(),
            experimental_lsp_ty: false,
            lsp_servers: Vec::new(),
            disabled_lsp: HashSet::new(),
            lsp_paths_extra: Vec::new(),
            lsp_auto_install_binaries: HashSet::new(),
            lsp_inflight_installs: HashSet::new(),
            storage_dir: None,
            url_fetch_allow_private: false,
            harness: None,
            diagnostic_cache_size: 5000,
        }
    }
}
