//! In-process cross-encoder reranking through ONNX Runtime.
//!
//! The session is raw `ort`, built the way the local embedder builds its own:
//! the runtime library is pre-validated with a plain `dlopen`, bound late if the
//! plugin adopted it after startup, and every native section (session creation,
//! inference, release) runs under the ORT exit gate. The managed-runtime
//! resolver is never called from here: it mutates process-global environment
//! and is only safe at startup.
//!
//! Resource rules:
//! - Its own single-flight worker thread and busy flag, separate from the
//!   embedding worker and its model mutex, so a slow rerank can never make a
//!   query embedding fail as busy.
//! - Intra-op threads come from a budget shared with the embedder (see
//!   [`rerank_intra_threads`]).
//! - Requests carry their deadline to the worker; a request whose deadline has
//!   passed when the worker picks it up is dropped without inference, and a
//!   batch stops between sub-batches once the deadline passes.
//! - Model download and session creation never run on the request path. The
//!   first request starts them on the worker thread and is told the backend is
//!   unavailable.
//! - Memory: the model file must fit `MAX_MODEL_FILE_BYTES`, inputs are cut to
//!   `MAX_PAIR_TOKENS`, and one inference never exceeds
//!   `MAX_RERANK_ATTENTION_UNITS` (`batch × tokens²`).

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::{RerankBackend, RerankDoc, RerankError, RerankFingerprint};

/// A reranker model the ONNX backend may load. Only these can be selected, so a
/// config value can never point the process at an arbitrary download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AllowedModel {
    pub(crate) name: &'static str,
    pub(crate) repo: &'static str,
    pub(crate) model_file: &'static str,
    /// Files the ONNX graph loads beside itself (external weights).
    pub(crate) extra_files: &'static [&'static str],
    pub(crate) tokenizer_file: &'static str,
}

pub(crate) const DEFAULT_MODEL: &str = "bge-reranker-base";

pub(crate) const ALLOWED_MODELS: &[AllowedModel] = &[
    AllowedModel {
        name: "bge-reranker-base",
        repo: "BAAI/bge-reranker-base",
        model_file: "onnx/model.onnx",
        extra_files: &[],
        tokenizer_file: "tokenizer.json",
    },
    AllowedModel {
        name: "bge-reranker-v2-m3",
        repo: "rozgo/bge-reranker-v2-m3",
        model_file: "model.onnx",
        extra_files: &["model.onnx.data"],
        tokenizer_file: "tokenizer.json",
    },
    AllowedModel {
        name: "jina-reranker-v1-turbo",
        repo: "jinaai/jina-reranker-v1-turbo-en",
        model_file: "onnx/model.onnx",
        extra_files: &[],
        tokenizer_file: "tokenizer.json",
    },
];

/// Most tokens per pair (query plus candidate). All three allowed models were
/// trained with a 512-token input; longer pairs are cut from the longer side
/// first.
pub(crate) const MAX_PAIR_TOKENS: usize = 512;
/// Largest `batch × tokens²` one inference may use. The attention tensors of a
/// base-size cross-encoder scale with it; 2M units is about 7 full-length
/// pairs or 20 pairs of 316 tokens per inference, which keeps the temporary
/// memory of one inference to a few hundred MB.
pub(crate) const MAX_RERANK_ATTENTION_UNITS: usize = 2_000_000;
/// Largest model (graph plus external weights) the backend will load. The
/// largest allowed model, bge-reranker-v2-m3 in fp32, is about 2.3 GB.
pub(crate) const MAX_MODEL_FILE_BYTES: u64 = 2_500 * 1024 * 1024;
/// Most pairs accepted in one call.
pub(crate) const MAX_PAIRS_PER_CALL: usize = 64;
/// After a failed download or load, wait this long before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(300);

/// Intra-op threads for the reranker session, from a budget shared with the
/// embedder. The embedder keeps the count it derives for itself (half the
/// cores, the container CPU quota, at most eight). The reranker takes at most
/// half of that again, and never more than the cores the embedder leaves
/// unused, with a floor of one thread. The two sessions together therefore
/// stay within the available cores whenever there are at least two; on a
/// single core the floor makes it two threads.
pub(crate) fn rerank_intra_threads() -> usize {
    let derivation = crate::local_embed::intra_thread_derivation();
    joint_rerank_threads(
        derivation.threads,
        derivation.available_parallelism,
        derivation.quota_threads,
    )
}

pub(crate) fn joint_rerank_threads(
    embedder_threads: usize,
    available_parallelism: usize,
    quota_threads: Option<usize>,
) -> usize {
    let cores = quota_threads
        .unwrap_or(available_parallelism)
        .min(available_parallelism)
        .max(1);
    embedder_threads
        .div_ceil(2)
        .min(cores.saturating_sub(embedder_threads))
        .max(1)
}

pub(crate) fn find_allowed_model(name: &str) -> Option<&'static AllowedModel> {
    let name = name.trim();
    ALLOWED_MODELS
        .iter()
        .find(|model| model.name.eq_ignore_ascii_case(name))
}

/// Scores query/document pairs. The worker owns one; tests substitute a fake.
pub(crate) trait PairScorer: Send {
    fn score_pairs(
        &mut self,
        query: &str,
        docs: &[String],
        deadline: Instant,
    ) -> Result<Vec<f32>, RerankError>;
}

/// Builds the scorer on the worker thread (download, then session creation).
pub(crate) type ScorerLoader = Arc<dyn Fn() -> Result<Box<dyn PairScorer>, String> + Send + Sync>;

struct Job {
    query: String,
    docs: Vec<String>,
    deadline: Instant,
    reply: Sender<Result<Vec<f32>, RerankError>>,
}

enum WorkerState {
    NotStarted,
    Starting,
    Ready(Sender<Job>),
    Failed { reason: String, at: Instant },
}

struct Shared {
    state: Mutex<WorkerState>,
    /// Set while this reranker has a job queued or running. Another request to
    /// the same reranker is refused as busy instead of queuing behind it.
    busy: AtomicBool,
}

/// The ONNX backend for one allowed model.
pub(crate) struct OnnxReranker {
    fingerprint: RerankFingerprint,
    shared: Arc<Shared>,
    loader: ScorerLoader,
}

impl OnnxReranker {
    pub(crate) fn with_loader(fingerprint: RerankFingerprint, loader: ScorerLoader) -> Self {
        Self {
            fingerprint,
            shared: Arc::new(Shared {
                state: Mutex::new(WorkerState::NotStarted),
                busy: AtomicBool::new(false),
            }),
            loader,
        }
    }

    fn for_model(model: &'static AllowedModel) -> Self {
        Self::with_loader(
            fingerprint_for(model),
            Arc::new(move || {
                let files = provision_model_files(model)?;
                let scorer = OnnxPairScorer::load(model.name, &files, rerank_intra_threads())?;
                Ok(Box::new(scorer) as Box<dyn PairScorer>)
            }),
        )
    }

    /// The worker's queue when it is ready; otherwise start it (once, or again
    /// after `RETRY_AFTER` following a failure) and say why it cannot serve.
    fn ready_sender(&self) -> Result<Sender<Job>, RerankError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            WorkerState::Ready(sender) => return Ok(sender.clone()),
            WorkerState::Starting => {
                return Err(RerankError::Unavailable("model loading".to_string()))
            }
            WorkerState::Failed { reason, at } if at.elapsed() < RETRY_AFTER => {
                return Err(RerankError::Unavailable(reason.clone()));
            }
            WorkerState::NotStarted | WorkerState::Failed { .. } => {}
        }
        *state = WorkerState::Starting;
        drop(state);
        let shared = self.shared.clone();
        let loader = self.loader.clone();
        let spawned = std::thread::Builder::new()
            .name("aft-rerank-onnx".to_string())
            .spawn(move || worker_main(shared, loader));
        if let Err(error) = spawned {
            let reason = format!("worker not started: {error}");
            *self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = WorkerState::Failed {
                reason: reason.clone(),
                at: Instant::now(),
            };
            return Err(RerankError::Unavailable(reason));
        }
        Err(RerankError::Unavailable("model loading".to_string()))
    }
}

fn worker_main(shared: Arc<Shared>, loader: ScorerLoader) {
    let mut scorer = match loader() {
        Ok(scorer) => scorer,
        Err(reason) => {
            crate::slog_warn!("rerank model unavailable: {reason}");
            *shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = WorkerState::Failed {
                reason: format!("model unavailable: {reason}"),
                at: Instant::now(),
            };
            return;
        }
    };
    let (sender, receiver): (Sender<Job>, Receiver<Job>) = crossbeam_channel::unbounded();
    *shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = WorkerState::Ready(sender);
    for job in receiver {
        // A request whose caller has already given up is dropped, not computed.
        let result = if Instant::now() >= job.deadline {
            Err(RerankError::Timeout)
        } else {
            scorer.score_pairs(&job.query, &job.docs, job.deadline)
        };
        shared.busy.store(false, Ordering::Release);
        let _ = job.reply.send(result);
    }
}

impl RerankBackend for OnnxReranker {
    fn fingerprint(&self) -> RerankFingerprint {
        self.fingerprint.clone()
    }

    fn max_batch(&self) -> usize {
        MAX_PAIRS_PER_CALL
    }

    fn score(
        &self,
        query: &str,
        docs: &[RerankDoc<'_>],
        deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        if docs.len() > MAX_PAIRS_PER_CALL {
            return Err(RerankError::Refused(format!(
                "{} pairs exceed the limit of {MAX_PAIRS_PER_CALL}",
                docs.len()
            )));
        }
        let sender = self.ready_sender()?;
        if self
            .shared
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RerankError::Unavailable("busy".to_string()));
        }
        let (reply, response) = crossbeam_channel::bounded(1);
        let job = Job {
            query: query.to_string(),
            docs: docs.iter().map(|doc| doc.text.to_string()).collect(),
            deadline,
            reply,
        };
        if sender.send(job).is_err() {
            self.shared.busy.store(false, Ordering::Release);
            return Err(RerankError::Unavailable("worker stopped".to_string()));
        }
        match response.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(RerankError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(RerankError::Failed("worker stopped".to_string()))
            }
        }
    }
}

pub(crate) fn fingerprint_for(model: &AllowedModel) -> RerankFingerprint {
    RerankFingerprint {
        backend: "onnx",
        model: model.name.to_string(),
        revision: format!("{}:{}", model.repo, model.model_file),
    }
}

/// The process-wide ONNX backend for a configured model name (default when
/// absent). Cheap: it neither downloads nor loads anything.
pub(crate) fn shared_backend(model: Option<&str>) -> Result<Arc<dyn RerankBackend>, String> {
    static BACKENDS: OnceLock<Mutex<HashMap<&'static str, Arc<OnnxReranker>>>> = OnceLock::new();
    let requested = model.unwrap_or(DEFAULT_MODEL);
    let allowed = find_allowed_model(requested)
        .ok_or_else(|| format!("unsupported rerank model {requested:?}"))?;
    let mut backends = BACKENDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(backends
        .entry(allowed.name)
        .or_insert_with(|| Arc::new(OnnxReranker::for_model(allowed)))
        .clone())
}

/// Local paths of one model's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelFiles {
    pub(crate) model: PathBuf,
    pub(crate) tokenizer: PathBuf,
}

/// Find the model in the shared model cache, downloading it when absent. Runs
/// only on the worker thread.
fn provision_model_files(model: &AllowedModel) -> Result<ModelFiles, String> {
    let cache_dir = crate::local_embed::embedding_cache_dir()?;
    if let Some(found) = scan_local_snapshot(&cache_dir, model) {
        return Ok(found);
    }
    use hf_hub::api::sync::ApiBuilder;
    crate::slog_info!(
        "downloading rerank model {} ({}) to {}",
        model.name,
        model.repo,
        cache_dir.display()
    );
    let api = ApiBuilder::new()
        .with_progress(false)
        .with_cache_dir(cache_dir.clone())
        .build()
        .map_err(|error| format!("init model download: {error}"))?;
    let repo = api.model(model.repo.to_string());
    let model_path = repo
        .get(model.model_file)
        .map_err(|error| format!("download {}: {error}", model.model_file))?;
    for extra in model.extra_files {
        repo.get(extra)
            .map_err(|error| format!("download {extra}: {error}"))?;
    }
    let tokenizer = repo
        .get(model.tokenizer_file)
        .map_err(|error| format!("download {}: {error}", model.tokenizer_file))?;
    Ok(ModelFiles {
        model: model_path,
        tokenizer,
    })
}

/// hf-hub keeps a repo at `<cache>/models--<org>--<repo>/snapshots/<rev>/`.
/// Take the newest snapshot holding every required file.
fn scan_local_snapshot(cache_dir: &Path, model: &AllowedModel) -> Option<ModelFiles> {
    let snapshots = cache_dir
        .join(format!("models--{}", model.repo.replace('/', "--")))
        .join("snapshots");
    let mut candidates = std::fs::read_dir(snapshots)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    candidates.into_iter().rev().find_map(|snapshot| {
        let files = ModelFiles {
            model: snapshot.join(model.model_file),
            tokenizer: snapshot.join(model.tokenizer_file),
        };
        let complete = files.model.is_file()
            && files.tokenizer.is_file()
            && model
                .extra_files
                .iter()
                .all(|extra| snapshot.join(extra).is_file());
        complete.then_some(files)
    })
}

/// Total size of the graph file and every file beside it that it may load.
fn model_bytes(model_path: &Path) -> u64 {
    let graph = std::fs::metadata(model_path).map_or(0, |metadata| metadata.len());
    let data = std::fs::metadata(model_path.with_extension("onnx.data"))
        .map_or(0, |metadata| metadata.len());
    graph + data
}

/// A loaded cross-encoder session and its tokenizer.
pub(crate) struct OnnxPairScorer {
    /// Released explicitly in `Drop` under the ORT exit gate.
    session: ManuallyDrop<Session>,
    tokenizer: Tokenizer,
    wants_token_type_ids: bool,
    pad_id: i64,
}

impl OnnxPairScorer {
    pub(crate) fn load(name: &str, files: &ModelFiles, threads: usize) -> Result<Self, String> {
        let bytes = model_bytes(&files.model);
        if bytes > MAX_MODEL_FILE_BYTES {
            return Err(format!(
                "model {name} is {bytes} bytes, above the {MAX_MODEL_FILE_BYTES}-byte limit"
            ));
        }
        crate::semantic_index::pre_validate_onnx_runtime()?;
        crate::semantic_index::bind_late_onnx_runtime()?;
        let _ort_section = crate::ort_lifecycle::enter()
            .ok_or_else(|| "rerank model not loaded: the process is shutting down".to_string())?;
        let session = Session::builder()
            .map_err(|error| format!("create ONNX session builder: {error}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|error| format!("set ONNX optimization level: {error}"))?
            .with_intra_threads(threads)
            .map_err(|error| format!("set ONNX intra-op threads: {error}"))?
            .commit_from_file(&files.model)
            .map_err(crate::semantic_index::format_embedding_init_error)?;
        let mut tokenizer = Tokenizer::from_file(&files.tokenizer)
            .map_err(|error| format!("load tokenizer {}: {error}", files.tokenizer.display()))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_PAIR_TOKENS,
                strategy: tokenizers::TruncationStrategy::LongestFirst,
                ..Default::default()
            }))
            .map_err(|error| format!("set tokenizer truncation: {error}"))?;
        tokenizer.with_padding(None);
        let wants_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        let pad_id = ["<pad>", "[PAD]"]
            .iter()
            .find_map(|token| tokenizer.token_to_id(token))
            .map_or(0, i64::from);
        crate::slog_info!(
            "rerank model ready: model={name} intra_threads={threads} token_type_ids={wants_token_type_ids}"
        );
        Ok(Self {
            session: ManuallyDrop::new(session),
            tokenizer,
            wants_token_type_ids,
            pad_id,
        })
    }

    fn run_inference(&mut self, encodings: &[tokenizers::Encoding]) -> Result<Vec<f32>, String> {
        let _ort_section = crate::ort_lifecycle::enter()
            .ok_or_else(|| "rerank stopped: the process is shutting down".to_string())?;
        let batch = encodings.len();
        let max_len = encodings
            .iter()
            .map(|encoding| encoding.get_ids().len())
            .max()
            .unwrap_or(1)
            .max(1);
        let mut ids = vec![self.pad_id; batch * max_len];
        let mut mask = vec![0i64; batch * max_len];
        let mut types = vec![0i64; batch * max_len];
        for (row, encoding) in encodings.iter().enumerate() {
            let base = row * max_len;
            for (column, ((id, attention), kind)) in encoding
                .get_ids()
                .iter()
                .zip(encoding.get_attention_mask())
                .zip(encoding.get_type_ids())
                .enumerate()
            {
                ids[base + column] = i64::from(*id);
                mask[base + column] = i64::from(*attention);
                types[base + column] = i64::from(*kind);
            }
        }
        let shape = (batch, max_len);
        let input_ids = ndarray::Array2::from_shape_vec(shape, ids)
            .map_err(|error| format!("build input_ids: {error}"))?;
        let attention_mask = ndarray::Array2::from_shape_vec(shape, mask)
            .map_err(|error| format!("build attention_mask: {error}"))?;
        let mut inputs = ort::inputs![
            "input_ids" => Tensor::from_array(input_ids).map_err(|error| format!("input_ids: {error}"))?,
            "attention_mask" => Tensor::from_array(attention_mask).map_err(|error| format!("attention_mask: {error}"))?,
        ];
        if self.wants_token_type_ids {
            let token_type_ids = ndarray::Array2::from_shape_vec(shape, types)
                .map_err(|error| format!("build token_type_ids: {error}"))?;
            inputs.push((
                "token_type_ids".into(),
                Tensor::from_array(token_type_ids)
                    .map_err(|error| format!("token_type_ids: {error}"))?
                    .into(),
            ));
        }
        let outputs = self
            .session
            .run(inputs)
            .map_err(|error| format!("rerank inference failed: {error}"))?;
        let output = outputs
            .values()
            .next()
            .ok_or_else(|| "rerank model produced no output".to_string())?;
        let (shape, data): (Vec<i64>, Vec<f32>) = match output.try_extract_tensor::<f32>() {
            Ok((shape, data)) => (shape.to_vec(), data.to_vec()),
            Err(_) => {
                let (shape, data) = output
                    .try_extract_tensor::<half::f16>()
                    .map_err(|error| format!("extract rerank logits: {error}"))?;
                (
                    shape.to_vec(),
                    data.iter().map(|value| value.to_f32()).collect(),
                )
            }
        };
        // Logits are [batch, 1] (or [batch]); the first column is the
        // relevance logit. Its sigmoid is monotonic, so the raw logit orders
        // the same way.
        let width = match shape.as_slice() {
            [rows] if *rows as usize == batch => 1,
            [rows, columns] if *rows as usize == batch && *columns >= 1 => *columns as usize,
            other => return Err(format!("unexpected rerank output shape {other:?}")),
        };
        Ok((0..batch).map(|row| data[row * width]).collect())
    }
}

impl PairScorer for OnnxPairScorer {
    fn score_pairs(
        &mut self,
        query: &str,
        docs: &[String],
        deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let pairs = docs
            .iter()
            .map(|doc| (query, doc.as_str()))
            .collect::<Vec<_>>();
        let encodings = self
            .tokenizer
            .encode_batch(pairs, true)
            .map_err(|error| RerankError::Failed(format!("tokenize: {error}")))?;
        let mut scores = Vec::with_capacity(encodings.len());
        for range in attention_batches(&encodings, MAX_RERANK_ATTENTION_UNITS) {
            if Instant::now() >= deadline {
                return Err(RerankError::Timeout);
            }
            scores.extend(
                self.run_inference(&encodings[range])
                    .map_err(RerankError::Failed)?,
            );
        }
        Ok(scores)
    }
}

impl Drop for OnnxPairScorer {
    fn drop(&mut self) {
        if let Some(_ort_section) = crate::ort_lifecycle::enter() {
            // SAFETY: `session` is never used again; this is its only drop.
            unsafe { ManuallyDrop::drop(&mut self.session) };
        }
    }
}

/// Split encodings, in order, into runs whose `count × longest²` stays within
/// `budget`. A single over-budget pair still gets a run of its own.
pub(crate) fn attention_batches(
    encodings: &[tokenizers::Encoding],
    budget: usize,
) -> Vec<std::ops::Range<usize>> {
    let lengths = encodings
        .iter()
        .map(|encoding| encoding.get_ids().len().max(1))
        .collect::<Vec<_>>();
    attention_batches_for_lengths(&lengths, budget)
}

pub(crate) fn attention_batches_for_lengths(
    lengths: &[usize],
    budget: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut longest = 0usize;
    for (index, length) in lengths.iter().enumerate() {
        let count = index - start;
        let candidate = longest.max(*length);
        let cost = (count + 1)
            .saturating_mul(candidate)
            .saturating_mul(candidate);
        if count > 0 && cost > budget {
            batches.push(start..index);
            start = index;
            longest = *length;
        } else {
            longest = candidate;
        }
    }
    if start < lengths.len() {
        batches.push(start..lengths.len());
    }
    batches
}
