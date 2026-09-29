//! The reranker backend of one context, built off the search path.
//!
//! Building a backend can need I/O: loading a model, reading a score pack,
//! asking a server which model it serves. None of that may happen while a
//! search waits. So each context owns a [`BackendSlot`]. Whenever the inputs
//! that decide the backend change (the published config, or whether the
//! process runs under the SubC daemon), the slot starts a build on its own
//! thread and installs the result, tagged with a generation so a build for
//! superseded inputs is discarded. A search only reads the slot: while nothing
//! is installed (a build is running, failed, or reranking is off) it is not
//! reranked, and nothing is committed for its list.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{RerankSettings, SelectedBackend, FIXTURE_PACK_ENV, RECORD_PACK_ENV};
use crate::config::{Config, RerankBackendKind, SearchConfig, SemanticBackendConfig};

/// Most build attempts for one set of inputs before the slot gives up until
/// the inputs change again.
pub(crate) const MAX_BUILD_ATTEMPTS: u32 = 6;
/// Wait before the second attempt; each later wait doubles, up to
/// `MAX_BUILD_BACKOFF`.
pub(crate) const BUILD_BACKOFF: Duration = Duration::from_secs(2);
pub(crate) const MAX_BUILD_BACKOFF: Duration = Duration::from_secs(60);

/// Everything that decides which backend a context uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildInputs {
    pub(crate) search: SearchConfig,
    /// The embedding connection settings; the Synapse reranker reuses them.
    pub(crate) semantic: SemanticBackendConfig,
    /// True when the process serves requests for the SubC daemon.
    pub(crate) under_subc: bool,
}

impl BuildInputs {
    pub(crate) fn from_config(config: &Config, under_subc: bool) -> Self {
        Self {
            search: config.search.clone(),
            semantic: config.semantic.clone(),
            under_subc,
        }
    }

    /// Whether these inputs select no backend at all.
    fn reranking_off(&self) -> bool {
        RerankSettings::resolve(&self.search).backend == RerankBackendKind::Off
            && crate::environment::non_empty_os_var(FIXTURE_PACK_ENV).is_none()
    }
}

/// Builds a backend. `Ok(None)` means reranking is off for these inputs.
/// Runs off the search path and may block and do I/O.
pub(crate) type Constructor =
    Arc<dyn Fn(&BuildInputs) -> Result<Option<SelectedBackend>, String> + Send + Sync>;

/// What a search finds in the slot.
#[derive(Clone)]
pub(crate) enum Installed {
    /// Reranking is off: no rerank and no note.
    Off,
    /// No backend yet (building, or the last build failed): skip with this
    /// reason and commit nothing.
    NotReady(String),
    Ready(SelectedBackend),
}

struct SlotState {
    generation: u64,
    inputs: Option<BuildInputs>,
    installed: Installed,
}

/// A context's reranker backend, rebuilt in the background when its inputs
/// change.
pub(crate) struct BackendSlot {
    state: Arc<Mutex<SlotState>>,
    constructor: Mutex<Constructor>,
    backoff: Mutex<Duration>,
}

impl Default for BackendSlot {
    fn default() -> Self {
        Self::with_constructor(Arc::new(build_backend))
    }
}

impl BackendSlot {
    pub(crate) fn with_constructor(constructor: Constructor) -> Self {
        Self {
            state: Arc::new(Mutex::new(SlotState {
                generation: 0,
                inputs: None,
                installed: Installed::Off,
            })),
            constructor: Mutex::new(constructor),
            backoff: Mutex::new(BUILD_BACKOFF),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_constructor_for_test(&self, constructor: Constructor, backoff: Duration) {
        *lock(&self.constructor) = constructor;
        *lock(&self.backoff) = backoff;
    }

    /// Start building for `inputs` unless they are the inputs already built or
    /// being built. Returns at once: the build runs on its own thread. The one
    /// exception is the benchmark fixture pack, which is loaded before this
    /// returns so the first benchmark search already has it.
    pub(crate) fn reconcile(&self, inputs: BuildInputs) {
        let mut state = lock(&self.state);
        if state.inputs.as_ref() == Some(&inputs) {
            return;
        }
        state.generation += 1;
        let generation = state.generation;
        if inputs.reranking_off() {
            state.inputs = Some(inputs);
            state.installed = Installed::Off;
            return;
        }
        state.inputs = Some(inputs.clone());
        state.installed = Installed::NotReady("backend not ready".to_string());
        drop(state);

        let constructor = lock(&self.constructor).clone();
        let backoff = *lock(&self.backoff);
        let shared = self.state.clone();
        if crate::environment::non_empty_os_var(FIXTURE_PACK_ENV).is_some() {
            build_with_retries(shared, generation, inputs, constructor, backoff);
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("aft-rerank-build".to_string())
            .spawn(move || build_with_retries(shared, generation, inputs, constructor, backoff));
        if let Err(error) = spawned {
            let mut state = lock(&self.state);
            if state.generation == generation {
                state.installed =
                    Installed::NotReady(format!("backend build not started: {error}"));
            }
        }
    }

    /// The installed backend for `inputs`, without I/O and without waiting.
    /// Inputs the slot has not seen (a config published before the slot was
    /// told, or a daemon-mode change) start a build and read as not ready.
    pub(crate) fn read(&self, inputs: BuildInputs) -> Installed {
        {
            let state = lock(&self.state);
            if state.inputs.as_ref() == Some(&inputs) {
                return state.installed.clone();
            }
        }
        self.reconcile(inputs);
        lock(&self.state).installed.clone()
    }
}

fn build_with_retries(
    state: Arc<Mutex<SlotState>>,
    generation: u64,
    inputs: BuildInputs,
    constructor: Constructor,
    backoff: Duration,
) {
    let mut wait = backoff;
    for attempt in 1..=MAX_BUILD_ATTEMPTS {
        let built = constructor(&inputs);
        let mut current = lock(&state);
        if current.generation != generation {
            // The inputs changed while this build ran; a newer build owns the
            // slot.
            return;
        }
        match built {
            Ok(None) => {
                current.installed = Installed::Off;
                return;
            }
            Ok(Some(backend)) => {
                current.installed = Installed::Ready(backend);
                return;
            }
            Err(reason) => {
                crate::slog_warn!(
                    "rerank backend build failed (attempt {attempt}/{MAX_BUILD_ATTEMPTS}): {reason}"
                );
                current.installed = Installed::NotReady(reason);
            }
        }
        drop(current);
        if attempt < MAX_BUILD_ATTEMPTS {
            std::thread::sleep(wait);
            wait = (wait * 2).min(MAX_BUILD_BACKOFF);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The production constructor. Runs on the slot's build thread.
pub(crate) fn build_backend(inputs: &BuildInputs) -> Result<Option<SelectedBackend>, String> {
    if let Some(path) = crate::environment::non_empty_os_var(FIXTURE_PACK_ENV) {
        return super::fixture::shared_fixture_backend(std::path::Path::new(&path)).map(
            |backend| {
                Some(SelectedBackend {
                    backend,
                    fail_closed: true,
                })
            },
        );
    }
    let settings = RerankSettings::resolve(&inputs.search);
    match settings.backend {
        RerankBackendKind::Off => Ok(None),
        RerankBackendKind::Onnx => {
            let backend = super::onnx::shared_onnx_reranker(settings.model.as_deref())?;
            // Download and load now, so the backend is installed only once it
            // can score.
            backend.ensure_loaded()?;
            let backend: Arc<dyn super::RerankBackend> = backend;
            let backend = match crate::environment::non_empty_os_var(RECORD_PACK_ENV) {
                Some(path) => super::fixture::recording(backend, path.into()),
                None => backend,
            };
            Ok(Some(SelectedBackend {
                backend,
                fail_closed: false,
            }))
        }
        RerankBackendKind::Remote => build_remote(inputs),
        RerankBackendKind::Synapse => build_synapse(inputs),
    }
}

/// Builds the HTTP rerank backend from `inputs.search.rerank` (endpoint,
/// api_key_env, model). Not part of this build yet.
fn build_remote(_inputs: &BuildInputs) -> Result<Option<SelectedBackend>, String> {
    Err("remote backend unavailable".to_string())
}

/// Builds the Synapse rerank backend from the embedding connection settings
/// in `inputs.semantic` when `inputs.under_subc` is set; this is where it asks
/// Synapse which model it serves. Not part of this build yet.
fn build_synapse(_inputs: &BuildInputs) -> Result<Option<SelectedBackend>, String> {
    Err("synapse backend unavailable".to_string())
}
