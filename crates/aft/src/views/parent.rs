//! Multi-repo parent folders.
//!
//! A views-on session opened in a folder that is not itself a git checkout
//! but holds git repositories (its children) builds no index of its own. It
//! serves grep, glob, `aft_search`, the call graph tools and inspect from the
//! children's own published indexes, read-only:
//!
//! - **trigram:** the child's persisted `cache.bin`, opened through the
//!   borrowed read-only loader and reconciled with the child's files in RAM.
//!   With views on, a child's trigram plane is still this legacy artifact
//!   (the content-addressed view's own trigram file is empty), so the parent
//!   reads the artifact the child actually publishes.
//! - **semantic:** the child's per-checkout (v2) view, opened through a
//!   registry *reader* registration (a reader marker, never a view) and a read
//!   marker on the generation it serves.
//! - **callgraph:** the child's content-addressed view generation, protected
//!   by a query pin (a read marker in the child's view directory).
//!
//! Every child plane is loaded once by the session's background worker and
//! kept warm; the worker follows the child's publications and reconciles its
//! trigram snapshot with disk between queries. A request never loads, builds,
//! repairs or waits for a child index: a child still loading, or without a
//! published index, is a named gap for that child, and files outside every
//! child are a named gap too. Gaps use the existing `complete: false` and
//! `gaps` response fields.
//!
//! The session registry is process-wide and keyed by the canonical parent
//! root, so every context bound to the same parent folder shares one warm set
//! of child readers.

mod inspect;
mod query;
#[cfg(test)]
mod tests;

pub use query::route;
pub(crate) use query::{attach_gaps, glob_fan_out, grep_fan_out};

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

use crate::readonly_artifacts::{BorrowedArtifactGeneration, ReadOnlyArtifact};
use crate::search_index::SearchIndex;

use super::contracts::{PlaneAdapter, ViewAccess};
use super::manifest_v2::Producers;
use super::registry::{FamilyRegistry, ReaderRegistration};
use super::semantic::{SemanticPlane, SemanticProducer};
use super::snapshot::{LiveDelta, OpenGeneration, Snapshot};

/// Most child repositories one parent folder serves. The operator's own
/// projects folder holds 39 repositories, so the cap leaves room above that.
/// Children past the cap are reported as a named gap, never silently dropped.
pub const DEFAULT_MAX_CHILD_REPOS: usize = 64;

/// How deep discovery looks for repositories below the parent folder.
const MAX_DISCOVERY_DEPTH: usize = 3;

/// Upper bound on directory entries discovery examines, so a huge non-project
/// folder cannot stall configure.
const MAX_DISCOVERY_ENTRIES: usize = 20_000;

/// At most this many paths outside every child are remembered for gap reports.
const MAX_OUTSIDE_PATHS: usize = 64;

/// Directory names discovery never descends into: build output and package
/// caches hold no repositories of interest and can be very large.
const SKIPPED_DIRECTORY_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "venv",
    "__pycache__",
    "dist",
    "build",
];

/// Default pause between two refresh rounds of the session worker.
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// The child repositories found below a parent folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Discovery {
    /// Canonical parent folder.
    pub root: PathBuf,
    /// Served children, canonical, in byte order of their paths.
    pub children: Vec<PathBuf>,
    /// Children past the cap, in the same order. They are not served.
    pub skipped: Vec<PathBuf>,
    /// Files, and directories whose contents discovery did not examine, that
    /// lie outside every child (at most [`MAX_OUTSIDE_PATHS`]).
    pub outside: Vec<PathBuf>,
}

fn is_repository(dir: &Path) -> bool {
    // A `.git` directory is a primary checkout; a `.git` file is a linked
    // worktree or submodule checkout. Both own their own indexes.
    dir.join(".git").exists()
}

fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    Some(std::fs::canonicalize(&home).unwrap_or(home))
}

/// Finds the child repositories of `root`, keeping at most `cap`.
///
/// `None` means `root` is not a parent folder: it is inside a git checkout
/// (which keeps its own indexes), it is the home folder (which never indexes
/// anything), or it holds no repository within [`MAX_DISCOVERY_DEPTH`].
pub fn discover(root: &Path, cap: usize) -> Option<Discovery> {
    let root = std::fs::canonicalize(root).ok()?;
    if root.ancestors().any(is_repository) {
        return None;
    }
    if home_dir().is_some_and(|home| home == root) {
        return None;
    }
    let mut repositories = Vec::new();
    let mut outside = Vec::new();
    let mut examined = 0usize;
    let mut pending = vec![(root.clone(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        let mut subdirectories = Vec::new();
        for entry in entries {
            examined += 1;
            if examined > MAX_DISCOVERY_ENTRIES {
                break;
            }
            let name = entry.file_name();
            let hidden = name.to_str().is_none_or(|name| name.starts_with('.'));
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            // Symlinks are not followed: a link to a repository elsewhere
            // would make the parent serve a checkout outside its folder.
            if hidden || kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                if name
                    .to_str()
                    .is_some_and(|name| SKIPPED_DIRECTORY_NAMES.contains(&name))
                {
                    continue;
                }
                if is_repository(&path) {
                    repositories.push(path);
                } else if depth + 1 < MAX_DISCOVERY_DEPTH {
                    subdirectories.push(path);
                } else if outside.len() < MAX_OUTSIDE_PATHS {
                    outside.push(path);
                }
            } else if outside.len() < MAX_OUTSIDE_PATHS {
                outside.push(path);
            }
        }
        // Depth-first in reverse so directories are visited in name order.
        for subdirectory in subdirectories.into_iter().rev() {
            pending.push((subdirectory, depth + 1));
        }
    }
    if repositories.is_empty() {
        return None;
    }
    let mut repositories = repositories
        .into_iter()
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .collect::<Vec<_>>();
    repositories.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    repositories.dedup();
    let skipped = repositories.split_off(repositories.len().min(cap));
    outside.sort();
    Some(Discovery {
        root,
        children: repositories,
        skipped,
        outside,
    })
}

// ---------------------------------------------------------------------------
// Session registry
// ---------------------------------------------------------------------------

/// The index planes the user asked for before the parent session turned its
/// own indexes off. A plane that is off is not served from the children either.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestedPlanes {
    pub trigram: bool,
    pub semantic: bool,
    pub callgraph: bool,
}

struct Prepared {
    discovery: Discovery,
    planes: RequestedPlanes,
}

#[derive(Default)]
struct Registry {
    prepared: HashMap<PathBuf, Prepared>,
    sessions: HashMap<PathBuf, Arc<ParentSession>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Configure's first step for a views-on root: decides whether `root` is a
/// parent folder and, when it is, remembers its children for [`activate`].
/// True means the root must build no index of its own.
pub fn prepare(root: &Path, planes: RequestedPlanes) -> bool {
    prepare_with_cap(root, planes, DEFAULT_MAX_CHILD_REPOS)
}

pub fn prepare_with_cap(root: &Path, planes: RequestedPlanes, cap: usize) -> bool {
    let Some(discovery) = discover(root, cap) else {
        return false;
    };
    let key = discovery.root.clone();
    lock(registry())
        .prepared
        .insert(key, Prepared { discovery, planes });
    true
}

/// Configure's maintenance step: starts (or keeps) the parent session that
/// [`prepare`] decided on. Returns false, and drops any session left for this
/// root, when the root was not prepared as a parent folder.
///
/// Starting a session only spawns its worker; no child index is read here.
pub fn activate(
    root: &Path,
    storage: &Path,
    semantic: &crate::config::SemanticBackendConfig,
) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut registry = lock(registry());
    let Some(prepared) = registry.prepared.remove(&root) else {
        if let Some(session) = registry.sessions.remove(&root) {
            session.stop();
        }
        return false;
    };
    if let Some(existing) = registry.sessions.get(&root) {
        // A rebind with the same children, planes and storage keeps the warm
        // readers instead of loading every child again.
        if existing.discovery == prepared.discovery
            && existing.planes == prepared.planes
            && existing.storage == storage
        {
            return true;
        }
    }
    let session = ParentSession::start(
        prepared.discovery,
        prepared.planes,
        storage.to_path_buf(),
        semantic.clone(),
        refresh_interval(),
    );
    if let Some(previous) = registry.sessions.insert(root, session) {
        previous.stop();
    }
    true
}

/// Drops the parent session for `root`, if any: its worker stops and every
/// reader registration and read marker it holds is released.
pub fn deactivate(root: &Path) {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut registry = lock(registry());
    registry.prepared.remove(&root);
    if let Some(session) = registry.sessions.remove(&root) {
        session.stop();
    }
}

/// The active parent session for `root`.
pub fn session_for_root(root: &Path) -> Option<Arc<ParentSession>> {
    let registry = lock(registry());
    if registry.sessions.is_empty() {
        return None;
    }
    if let Some(session) = registry.sessions.get(root) {
        return Some(Arc::clone(session));
    }
    let canonical = std::fs::canonicalize(root).ok()?;
    registry.sessions.get(&canonical).cloned()
}

/// The parent session serving `ctx`'s root, when views are on.
pub fn session_for(ctx: &crate::context::AppContext) -> Option<Arc<ParentSession>> {
    let config = ctx.config();
    if !config.views.enabled {
        return None;
    }
    session_for_root(config.project_root.as_deref()?)
}

fn refresh_interval() -> Duration {
    if let Some(interval) = std::env::var("AFT_PARENT_REFRESH_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Duration::from_millis(interval);
    }
    match DEFAULT_REFRESH_OVERRIDE_MS.load(Ordering::SeqCst) {
        0 => DEFAULT_REFRESH_INTERVAL,
        millis => Duration::from_millis(millis),
    }
}

static DEFAULT_REFRESH_OVERRIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Replaces the default refresh interval for sessions started afterwards in
/// this process, so tests can observe child edits without waiting seconds.
#[doc(hidden)]
pub fn set_default_refresh_interval(interval: Duration) {
    DEFAULT_REFRESH_OVERRIDE_MS.store(
        u64::try_from(interval.as_millis())
            .unwrap_or(u64::MAX)
            .max(1),
        Ordering::SeqCst,
    );
}

// ---------------------------------------------------------------------------
// Session and children
// ---------------------------------------------------------------------------

/// One child plane as seen by queries.
pub(crate) enum Plane<T> {
    /// The worker has not finished its first load of this plane.
    Loading,
    Ready(Arc<T>),
    /// The child has nothing this session can serve; the reason is shown to
    /// the user as the gap's reason.
    Gap(String),
}

impl<T> Clone for Plane<T> {
    // Manual so a plane clones its `Arc`, without requiring `T: Clone`.
    fn clone(&self) -> Self {
        match self {
            Self::Loading => Self::Loading,
            Self::Ready(value) => Self::Ready(Arc::clone(value)),
            Self::Gap(reason) => Self::Gap(reason.clone()),
        }
    }
}

impl<T> Plane<T> {
    pub(crate) fn ready(&self) -> Option<&Arc<T>> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// The reason this plane cannot answer, for a named gap.
    pub(crate) fn gap_reason(&self, plane: &str) -> Option<String> {
        match self {
            Self::Loading => Some(format!("{plane} index of this repository is still loading")),
            Self::Ready(_) => None,
            Self::Gap(reason) => Some(format!("{plane}: {reason}")),
        }
    }
}

pub(crate) struct TrigramChild {
    pub index: Arc<SearchIndex>,
    artifact: BorrowedArtifactGeneration,
}

pub(crate) struct SemanticChild {
    pub access: ViewAccess,
    pub generation: Arc<OpenGeneration>,
    pub snapshot: Snapshot,
}

pub(crate) struct CallgraphChild {
    pub store: Arc<crate::callgraph_store::ReadonlyCallGraphStore>,
    generation: String,
}

/// One child repository of a parent folder.
pub struct Child {
    /// Canonical checkout root.
    pub root: PathBuf,
    /// Path relative to the parent folder, used as the answer's path prefix.
    pub relative: PathBuf,
    family: OnceLock<String>,
    scope: String,
    pub(crate) trigram: RwLock<Plane<TrigramChild>>,
    pub(crate) semantic: RwLock<Plane<SemanticChild>>,
    pub(crate) callgraph: RwLock<Plane<CallgraphChild>>,
    /// Read-only handle on the child's persisted inspect aggregates, holding
    /// a read marker on the inspect generation it reads.
    pub(crate) inspect: RwLock<Option<Arc<crate::inspect::cache::ReadonlyInspectCache>>>,
    /// The family registry reader registration, created once on first use.
    reader: Mutex<Option<Arc<ReaderRegistration>>>,
    /// HEAD fingerprint, cached against the `.git/HEAD` bytes it came from.
    head: Mutex<Option<(Vec<u8>, String)>>,
    loads: AtomicUsize,
}

impl Child {
    fn new(parent: &Path, root: PathBuf) -> Self {
        let relative = root
            .strip_prefix(parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| root.clone());
        let scope = crate::path_identity::project_scope_key(&root);
        Self {
            root,
            relative,
            family: OnceLock::new(),
            scope,
            trigram: RwLock::new(Plane::Loading),
            semantic: RwLock::new(Plane::Loading),
            callgraph: RwLock::new(Plane::Loading),
            inspect: RwLock::new(None),
            reader: Mutex::new(None),
            head: Mutex::new(None),
            loads: AtomicUsize::new(0),
        }
    }

    /// The child's artifact family, the same key its own sessions use.
    fn family(&self) -> &str {
        self.family
            .get_or_init(|| crate::search_index::artifact_cache_key(&self.root))
    }

    /// The child's path relative to the parent, slash-separated, for gaps.
    pub fn display(&self) -> String {
        self.relative.to_string_lossy().replace('\\', "/")
    }

    /// How many index artifacts the worker has loaded for this child. Queries
    /// never add to it.
    pub fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    pub fn trigram_ready(&self) -> bool {
        read(&self.trigram).ready().is_some()
    }

    pub fn semantic_ready(&self) -> bool {
        read(&self.semantic).ready().is_some()
    }

    pub fn callgraph_ready(&self) -> bool {
        read(&self.callgraph).ready().is_some()
    }

    /// The callgraph generation this session holds a read marker on.
    pub fn callgraph_generation(&self) -> Option<String> {
        read(&self.callgraph)
            .ready()
            .map(|callgraph| callgraph.generation.clone())
    }

    /// The semantic generation this session holds a read marker on.
    pub fn semantic_generation(&self) -> Option<String> {
        read(&self.semantic)
            .ready()
            .map(|semantic| semantic.generation.name().to_owned())
    }

    fn reader(&self, storage: &Path) -> Result<Option<Arc<ReaderRegistration>>, String> {
        let mut reader = lock(&self.reader);
        if let Some(reader) = reader.as_ref() {
            return Ok(Some(Arc::clone(reader)));
        }
        // Readers never create a family registry: a child whose own sessions
        // never published a per-checkout view has nothing to read yet.
        let Some(registry) =
            FamilyRegistry::open_existing(storage, self.family()).map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let registration = Arc::new(
            registry
                .register_reader("parent-folder")
                .map_err(|error| error.to_string())?,
        );
        *reader = Some(Arc::clone(&registration));
        Ok(Some(registration))
    }

    /// The fingerprint the child's view generations are named after for its
    /// current HEAD. `git ls-tree` runs only when `.git/HEAD` or the ref it
    /// names changed since the last call.
    fn head_fingerprint(&self) -> Result<String, String> {
        let probe = head_probe(&self.root);
        if let Some((cached_probe, fingerprint)) = lock(&self.head).as_ref() {
            if *cached_probe == probe {
                return Ok(fingerprint.clone());
            }
        }
        let entries =
            crate::alias::head_tree_entries(&self.root).map_err(|error| error.to_string())?;
        let fingerprint = super::assembly::head_tree_fingerprint(&entries);
        *lock(&self.head) = Some((probe, fingerprint.clone()));
        Ok(fingerprint)
    }
}

/// Bytes that change whenever the checkout's HEAD commit can have changed: the
/// HEAD file plus the ref file it names (and `packed-refs`), with their mtimes.
fn head_probe(root: &Path) -> Vec<u8> {
    let git = root.join(".git");
    let git_dir = if git.is_file() {
        std::fs::read_to_string(&git)
            .ok()
            .and_then(|text| {
                text.trim()
                    .strip_prefix("gitdir:")
                    .map(|dir| root.join(dir.trim()))
            })
            .unwrap_or(git)
    } else {
        git
    };
    let mut probe = Vec::new();
    let mut add = |path: &Path| {
        if let Ok(bytes) = std::fs::read(path) {
            probe.extend_from_slice(&bytes);
        }
        if let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) {
            probe.extend_from_slice(format!("{modified:?}").as_bytes());
        }
        probe.push(0);
    };
    let head = git_dir.join("HEAD");
    add(&head);
    let head_text = std::fs::read_to_string(&head).unwrap_or_default();
    if let Some(reference) = head_text.trim().strip_prefix("ref:") {
        let reference = reference.trim();
        add(&git_dir.join(reference));
        // A linked worktree keeps branch refs in the common directory.
        if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
            let common = git_dir.join(common.trim());
            add(&common.join(reference));
            add(&common.join("packed-refs"));
        }
    }
    add(&git_dir.join("packed-refs"));
    probe
}

/// The semantic model this session embeds queries with, and the shared plane
/// whose arena holds the children's resident vectors.
pub(crate) struct ParentSemantic {
    pub model: Mutex<crate::semantic_index::EmbeddingModel>,
    pub plane: Arc<SemanticPlane>,
    pub producer_id: String,
    pub config: crate::config::SemanticBackendConfig,
}

/// A parent folder's session state, shared by every context bound to it.
pub struct ParentSession {
    pub(crate) discovery: Discovery,
    pub(crate) planes: RequestedPlanes,
    storage: PathBuf,
    pub(crate) children: Vec<Arc<Child>>,
    pub(crate) semantic: RwLock<Plane<ParentSemantic>>,
    stop: Arc<AtomicBool>,
    /// While set, the worker skips its refresh rounds, so a test can hold the
    /// generations the session currently serves.
    paused: AtomicBool,
    /// Completed refresh rounds, including the first load.
    rounds: AtomicUsize,
}

impl ParentSession {
    fn start(
        discovery: Discovery,
        planes: RequestedPlanes,
        storage: PathBuf,
        semantic: crate::config::SemanticBackendConfig,
        interval: Duration,
    ) -> Arc<Self> {
        let children = discovery
            .children
            .iter()
            .map(|child| Arc::new(Child::new(&discovery.root, child.clone())))
            .collect();
        let session = Arc::new(Self {
            discovery,
            planes,
            storage,
            children,
            semantic: RwLock::new(if planes.semantic {
                Plane::Loading
            } else {
                Plane::Gap("semantic search is off".into())
            }),
            stop: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            rounds: AtomicUsize::new(0),
        });
        if !planes.trigram {
            for child in &session.children {
                *write(&child.trigram) = Plane::Gap("the trigram index is off".into());
            }
        }
        if !planes.callgraph {
            for child in &session.children {
                *write(&child.callgraph) = Plane::Gap("the call graph is off".into());
            }
        }
        if !planes.semantic {
            for child in &session.children {
                *write(&child.semantic) = Plane::Gap("semantic search is off".into());
            }
        }
        let weak = Arc::downgrade(&session);
        let stop = Arc::clone(&session.stop);
        let spawned = std::thread::Builder::new()
            .name("aft-parent-folder".into())
            .spawn(move || worker(weak, stop, semantic, interval));
        if let Err(error) = spawned {
            crate::slog_warn!("parent folder worker could not start: {}", error);
            for child in &session.children {
                *write(&child.trigram) = Plane::Gap(format!("worker unavailable: {error}"));
                *write(&child.callgraph) = Plane::Gap(format!("worker unavailable: {error}"));
                *write(&child.semantic) = Plane::Gap(format!("worker unavailable: {error}"));
            }
        }
        session
    }

    pub fn root(&self) -> &Path {
        &self.discovery.root
    }

    pub fn children(&self) -> &[Arc<Child>] {
        &self.children
    }

    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// Completed worker rounds; the first one is the initial load.
    pub fn rounds(&self) -> usize {
        self.rounds.load(Ordering::SeqCst)
    }

    /// Waits until the worker finished `rounds` rounds, for tests and tools
    /// that need a settled session. Queries never call this.
    pub fn wait_rounds(&self, rounds: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.rounds() < rounds {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Holds (or releases) the worker's refresh rounds. Queries keep being
    /// answered from what is already loaded.
    #[doc(hidden)]
    pub fn pause_refresh(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// The child whose checkout contains `path`.
    pub fn child_for(&self, path: &Path) -> Option<&Arc<Child>> {
        self.children
            .iter()
            .filter(|child| path.starts_with(&child.root))
            .max_by_key(|child| child.root.components().count())
    }
}

impl Drop for ParentSession {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Worker: load once, keep warm
// ---------------------------------------------------------------------------

fn worker(
    session: std::sync::Weak<ParentSession>,
    stop: Arc<AtomicBool>,
    semantic: crate::config::SemanticBackendConfig,
    interval: Duration,
) {
    let mut semantic_config = Some(semantic);
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(session) = session.upgrade() else {
            return;
        };
        if session.paused.load(Ordering::SeqCst) {
            drop(session);
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        // The embedding model starts after the first lexical round, so grep
        // and glob are served as early as possible.
        let first_round = session.rounds() == 0;
        refresh_round(&session, PlaneKind::Trigram);
        refresh_round(&session, PlaneKind::Callgraph);
        if session.planes.semantic {
            if let Some(config) = semantic_config.take() {
                start_semantic(&session, config);
            }
            refresh_round(&session, PlaneKind::Semantic);
        }
        session.rounds.fetch_add(1, Ordering::SeqCst);
        if first_round {
            crate::slog_info!(
                "parent folder loaded root={} children={} skipped={}",
                session.root().display(),
                session.children.len(),
                session.discovery.skipped.len()
            );
        }
        drop(session);
        let deadline = Instant::now() + interval;
        while Instant::now() < deadline {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20).min(interval));
        }
    }
}

/// Which child plane a refresh round works on.
#[derive(Clone, Copy)]
enum PlaneKind {
    Trigram,
    Semantic,
    Callgraph,
}

fn refresh_round(session: &ParentSession, plane: PlaneKind) {
    for child in &session.children {
        if session.stopped() {
            return;
        }
        match plane {
            PlaneKind::Trigram if session.planes.trigram => refresh_trigram(session, child),
            PlaneKind::Callgraph => {
                if session.planes.callgraph {
                    refresh_callgraph(session, child);
                }
                refresh_inspect(session, child);
            }
            PlaneKind::Semantic if session.planes.semantic => refresh_semantic(session, child),
            _ => {}
        }
    }
}

fn refresh_trigram(session: &ParentSession, child: &Child) {
    let keep_going = || !session.stopped();
    let artifact = crate::readonly_artifacts::search_index_artifact_generation_with_key(
        child.family(),
        Some(&session.storage),
    );
    let Some(artifact) = artifact else {
        *write(&child.trigram) =
            Plane::Gap("no trigram index has been built for this repository yet".into());
        return;
    };
    let current = read(&child.trigram).ready().cloned();
    if let Some(current) = current.filter(|current| current.artifact == artifact) {
        // Same artifact: bring the warm snapshot up to date with the files in
        // RAM. Nothing is loaded; unchanged files cost a stat.
        let mut index = (*current.index).clone();
        match index.reconcile_borrowed_snapshot_with_disk(&keep_going) {
            Some(summary) if summary.reindexed + summary.added + summary.removed > 0 => {
                *write(&child.trigram) = Plane::Ready(Arc::new(TrigramChild {
                    index: Arc::new(index),
                    artifact,
                }));
            }
            _ => {}
        }
        return;
    }
    let cache_dir =
        crate::search_index::resolve_cache_dir_with_key(child.family(), Some(&session.storage));
    child.loads.fetch_add(1, Ordering::SeqCst);
    let opened = crate::readonly_artifacts::open_search_index_background(
        &child.root,
        cache_dir,
        &keep_going,
    );
    let mut index = match opened {
        ReadOnlyArtifact::Fresh(index) => index,
        ReadOnlyArtifact::Stale(stale) => stale.index,
        ReadOnlyArtifact::Degraded(degradation) => {
            *write(&child.trigram) = Plane::Gap(format!(
                "the trigram index could not be read ({})",
                degradation.reason
            ));
            return;
        }
        ReadOnlyArtifact::Absent => {
            *write(&child.trigram) =
                Plane::Gap("no readable trigram index for this repository".into());
            return;
        }
        ReadOnlyArtifact::Cancelled => return,
    };
    // The artifact describes the child as its owner last saved it; edits made
    // since then go into this reader's RAM copy, never into the artifact.
    if index
        .reconcile_borrowed_snapshot_with_disk(&keep_going)
        .is_none()
    {
        return;
    }
    *write(&child.trigram) = Plane::Ready(Arc::new(TrigramChild {
        index: Arc::new(index),
        artifact,
    }));
}

fn refresh_callgraph(session: &ParentSession, child: &Child) {
    let view_dir = session.storage.join("views").join(&child.scope);
    let Some(store) = super::ViewStore::existing_dir(view_dir.clone()) else {
        *write(&child.callgraph) =
            Plane::Gap("no call graph view has been published for this repository yet".into());
        return;
    };
    let generation = match store.current_generation_read_only() {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            *write(&child.callgraph) =
                Plane::Gap("no call graph view has been published for this repository yet".into());
            return;
        }
        Err(error) => {
            *write(&child.callgraph) = Plane::Gap(format!("call graph view unreadable: {error}"));
            return;
        }
    };
    let head = match child.head_fingerprint() {
        Ok(head) => head,
        Err(error) => {
            *write(&child.callgraph) = Plane::Gap(format!("HEAD unreadable: {error}"));
            return;
        }
    };
    if !super::generation_matches_head(&generation, &head) {
        // The child's sessions have not yet published a view for its current
        // HEAD. Serving the older graph would answer for another commit.
        *write(&child.callgraph) =
            Plane::Gap("the call graph view for the current HEAD is still pending".into());
        return;
    }
    if read(&child.callgraph)
        .ready()
        .is_some_and(|current| current.generation == generation)
    {
        return;
    }
    // Protect before touching: the read marker exists before the generation's
    // database is opened, and the pointer is re-read afterwards so a
    // generation the child replaced in between is never adopted.
    let pin = match crate::pins::QueryPin::acquire(&view_dir, &generation) {
        Ok(pin) => Arc::new(pin),
        Err(error) => {
            *write(&child.callgraph) = Plane::Gap(format!("call graph view not pinned: {error}"));
            return;
        }
    };
    if store
        .current_generation_read_only()
        .ok()
        .flatten()
        .as_deref()
        != Some(generation.as_str())
    {
        return;
    }
    child.loads.fetch_add(1, Ordering::SeqCst);
    match super::read::open_published_callgraph(
        child.root.clone(),
        child.family().to_owned(),
        view_dir,
        &generation,
        Some(pin),
    ) {
        Ok(opened) => {
            *write(&child.callgraph) = Plane::Ready(Arc::new(CallgraphChild {
                store: Arc::new(opened),
                generation,
            }));
        }
        Err(error) => {
            *write(&child.callgraph) = Plane::Gap(format!("call graph view unreadable: {error}"));
        }
    }
}

fn refresh_inspect(session: &ParentSession, child: &Child) {
    if read(&child.inspect).is_some() {
        return;
    }
    // `open_readonly` creates nothing but its read marker; `None` means the
    // child's sessions never ran a project-wide inspect.
    if let Ok(Some(cache)) = crate::inspect::cache::InspectCache::open_readonly(
        session.storage.join("inspect"),
        child.root.clone(),
    ) {
        child.loads.fetch_add(1, Ordering::SeqCst);
        *write(&child.inspect) = Some(Arc::new(cache));
    }
}

fn start_semantic(session: &ParentSession, config: crate::config::SemanticBackendConfig) {
    let started = (|| -> Result<ParentSemantic, String> {
        let mut model = crate::semantic_index::EmbeddingModel::from_config(&config)?;
        let fingerprint = model.fingerprint(&config)?;
        let producer =
            SemanticProducer::current(fingerprint.as_string(), fingerprint.embed_text_caps);
        let producer_id = producer.id();
        let plane = super::semantic_runtime::shared_plane(&session.storage, producer);
        Ok(ParentSemantic {
            model: Mutex::new(model),
            plane,
            producer_id,
            config,
        })
    })();
    *write(&session.semantic) = match started {
        Ok(semantic) => Plane::Ready(Arc::new(semantic)),
        Err(error) => Plane::Gap(format!("embedding model unavailable: {error}")),
    };
}

fn refresh_semantic(session: &ParentSession, child: &Child) {
    let semantic = match &*read(&session.semantic) {
        Plane::Ready(semantic) => Arc::clone(semantic),
        Plane::Loading => return,
        Plane::Gap(reason) => {
            *write(&child.semantic) = Plane::Gap(reason.clone());
            return;
        }
    };
    let reader = match child.reader(&session.storage) {
        Ok(Some(reader)) => reader,
        Ok(None) => {
            *write(&child.semantic) =
                Plane::Gap("no semantic view has been published for this repository yet".into());
            return;
        }
        Err(error) => {
            *write(&child.semantic) = Plane::Gap(format!("semantic view unreadable: {error}"));
            return;
        }
    };
    let members = reader.members().unwrap_or_default();
    if !members.iter().any(|member| member.scope == child.scope) {
        *write(&child.semantic) =
            Plane::Gap("no semantic view has been published for this repository yet".into());
        return;
    }
    let producers = Producers {
        trigram: super::semantic_runtime::UNREGISTERED_PRODUCER.into(),
        semantic: Some(semantic.producer_id.clone()),
        callgraph: super::semantic_runtime::UNREGISTERED_PRODUCER.into(),
    };
    // `open_foreign_generation` reads the pointer before protecting it, so
    // check the cheap pointer first and skip a generation already served.
    let current_name = super::registry::view_dir(&session.storage, &child.scope)
        .ok()
        .and_then(super::ViewStore::existing_dir)
        .and_then(|store| store.current_generation_read_only().ok().flatten());
    if current_name.is_some()
        && read(&child.semantic)
            .ready()
            .is_some_and(|current| Some(current.generation.name()) == current_name.as_deref())
    {
        return;
    }
    let generation = match super::read::open_foreign_generation(&reader, &child.scope, &producers) {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            *write(&child.semantic) =
                Plane::Gap("no semantic view has been published for this repository yet".into());
            return;
        }
        Err(error) => {
            *write(&child.semantic) = Plane::Gap(format!("semantic view unavailable: {error}"));
            return;
        }
    };
    let access = ViewAccess::Reader {
        registration: Arc::clone(&reader),
        scope: child.scope.clone(),
    };
    child.loads.fetch_add(1, Ordering::SeqCst);
    // Admission decodes the generation's vectors once into the family arena.
    // It has no size cap: a large child is served like a small one.
    if let Err(error) = semantic.plane.open_generation(&access, &generation) {
        *write(&child.semantic) = Plane::Gap(format!("semantic view unavailable: {error}"));
        return;
    }
    let snapshot = LiveDelta::new(Arc::clone(&generation)).snapshot();
    let previous = std::mem::replace(
        &mut *write(&child.semantic),
        Plane::Ready(Arc::new(SemanticChild {
            access: access.clone(),
            generation,
            snapshot,
        })),
    );
    if let Plane::Ready(previous) = previous {
        // Release the replaced generation's residents unless a session of the
        // child in this same process still serves them.
        if super::semantic_runtime::live_holders(&semantic.plane, &access) == 0 {
            semantic
                .plane
                .release_generation(&access, previous.generation.name());
        }
    }
}

/// Paths in `paths` relative to the parent root, for gap reports.
pub(crate) fn display_relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        ".".into()
    } else {
        text
    }
}

/// The set of child roots, for tests.
pub fn child_roots(session: &ParentSession) -> BTreeSet<PathBuf> {
    session
        .children
        .iter()
        .map(|child| child.root.clone())
        .collect()
}
