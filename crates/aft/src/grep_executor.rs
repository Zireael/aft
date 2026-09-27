use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::commands::multi_path::{
    canonical_key, dedupe_nested_paths, resolve_path_or_multi, SearchPathResolution,
};
use crate::context::AppContext;
use crate::pattern_compile::{CompiledPattern, LiteralSearch};
use crate::protocol::Response;
use crate::search_index::{
    build_path_filters, decompose_grep_pattern, has_any_project_file_from, read_searchable_text,
    resolve_search_scope, sort_grep_matches_by_mtime_desc, sort_walked_paths_by_mtime_desc,
    try_read_with_budget, GrepMatch, GrepPathExclusion, GrepQueryPhaseTimings, GrepResult,
    IndexStatus, PathFilters, RegexQuery, INTERACTIVE_ARTIFACT_READ_BUDGET,
};

/// Maximum files enumerated during grep/glob index-unavailable fallback walks.
pub(crate) const MAX_FALLBACK_WALK_FILES: usize = 50_000;
/// Wall-clock budget for grep/glob index-unavailable fallback walks on the dispatch thread.
pub(crate) const FALLBACK_WALK_BUDGET: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct FallbackWalkOutcome {
    pub files: Vec<PathBuf>,
    pub walk_truncated: bool,
    /// Foreign filesystem mounts skipped before a recursive fallback could open them.
    pub skipped_foreign_mounts: usize,
    pub entries_visited: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct FallbackWalkProgress {
    walk_truncated: bool,
    skipped_foreign_mounts: usize,
}

#[derive(Clone, Debug)]
pub struct GrepParams {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_results: usize,
    pub path_exclusion: Option<GrepPathExclusion>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GrepExecutionPhaseTimings {
    pub snapshot_acquire: Duration,
    /// Regex decomposition is paid once per grep request, not once per root.
    pub query_decomposition: Duration,
    pub query: GrepQueryPhaseTimings,
    pub indexed_scope_has_files: Option<bool>,
}

impl GrepExecutionPhaseTimings {
    fn add(&mut self, other: Self) {
        self.snapshot_acquire += other.snapshot_acquire;
        self.query.trigram_lookup += other.query.trigram_lookup;
        self.query.pread_verify += other.query.pread_verify;
        self.query.post_filter += other.query.post_filter;
        self.query.candidate_count += other.query.candidate_count;
        self.query.bytes_verified += other.query.bytes_verified;
        self.indexed_scope_has_files =
            match (self.indexed_scope_has_files, other.indexed_scope_has_files) {
                (Some(left), Some(right)) => Some(left || right),
                _ => None,
            };
    }
}

#[derive(Clone, Debug)]
pub struct GrepScope {
    pub roots: Vec<ResolvedRoot>,
    pub multi_root: bool,
    pub per_root_max: usize,
}

#[derive(Clone, Debug)]
pub struct ResolvedRoot {
    pub search_root: PathBuf,
    pub filter_root: PathBuf,
    pub use_index: bool,
    pub is_external: bool,
}

pub fn project_root(ctx: &AppContext) -> PathBuf {
    let project_root = ctx
        .config()
        .project_root
        .clone()
        .unwrap_or_else(|| env::current_dir().unwrap_or_default());
    std::fs::canonicalize(&project_root).unwrap_or(project_root)
}

pub fn resolve_grep_scope(
    ctx: &AppContext,
    paths: Option<&serde_json::Value>,
    max_results: usize,
    req_id: &str,
) -> Result<GrepScope, Response> {
    let project_root = project_root(ctx);
    let search_roots = resolve_roots(ctx, paths, &project_root, req_id)?;

    if let Some(missing_root) = search_roots.iter().find(|root| !root.exists()) {
        return Err(Response::error(
            req_id,
            "path_not_found",
            format!(
                "grep: search path does not exist: {}",
                missing_root.display()
            ),
        ));
    }

    let roots = search_roots
        .into_iter()
        .map(|search_root| {
            let scope = resolve_search_scope(&project_root, Some(&search_root.to_string_lossy()));
            let is_external = !scope.use_index;
            let filter_root =
                compute_filter_root(&project_root, &scope.root, scope.use_index, is_external);
            ResolvedRoot {
                search_root: scope.root,
                filter_root,
                use_index: scope.use_index,
                is_external,
            }
        })
        .collect::<Vec<_>>();

    let multi_root = roots.len() > 1;
    let per_root_max = if multi_root {
        max_results.saturating_mul(2).max(max_results)
    } else {
        max_results
    };

    Ok(GrepScope {
        roots,
        multi_root,
        per_root_max,
    })
}

pub fn compute_filter_root(
    project_root: &Path,
    search_root: &Path,
    use_index: bool,
    is_external: bool,
) -> PathBuf {
    if is_external && !use_index {
        search_root.to_path_buf()
    } else {
        project_root.to_path_buf()
    }
}

pub fn scope_has_files(project_root: &Path, scope: &GrepScope) -> bool {
    scope.roots.iter().any(|root| {
        // An explicitly-named existing file is always in scope (it's searched
        // directly even if gitignored / .aftignored), so don't report it as
        // "no files matched scope".
        if root.search_root.is_file() {
            return true;
        }
        let catch_all =
            build_path_filters(&["**/*".to_string()], &[]).expect("valid catch-all glob");
        has_any_project_file_from(&root.filter_root, &root.search_root, &catch_all)
            || has_any_project_file_from(project_root, &root.search_root, &catch_all)
    })
}

pub fn execute(
    ctx: &AppContext,
    pattern: &CompiledPattern,
    scope: &GrepScope,
    params: &GrepParams,
) -> GrepResult {
    execute_profiled(ctx, pattern, scope, params).0
}

pub(crate) fn execute_profiled(
    ctx: &AppContext,
    pattern: &CompiledPattern,
    scope: &GrepScope,
    params: &GrepParams,
) -> (GrepResult, GrepExecutionPhaseTimings) {
    let filters = build_path_filters(&params.include, &params.exclude).unwrap_or_default();
    execute_profiled_with_filters(ctx, pattern, scope, params, &filters)
}

pub(crate) fn execute_profiled_with_filters(
    ctx: &AppContext,
    pattern: &CompiledPattern,
    scope: &GrepScope,
    params: &GrepParams,
    filters: &PathFilters,
) -> (GrepResult, GrepExecutionPhaseTimings) {
    let project_root = project_root(ctx);
    let query_started = Instant::now();
    let query = decompose_grep_pattern(pattern);
    let query_decomposition = query_started.elapsed();
    if scope.roots.len() == 1 {
        let (result, mut phases) = execute_root_profiled(
            ctx,
            pattern,
            &query,
            &scope.roots[0],
            params,
            filters,
            params.max_results,
            &project_root,
        );
        phases.query_decomposition = query_decomposition;
        return (result, phases);
    }

    let mut results = Vec::new();
    let mut phases: Option<GrepExecutionPhaseTimings> = None;
    for root in &scope.roots {
        let (result, root_phases) = execute_root_profiled(
            ctx,
            pattern,
            &query,
            root,
            params,
            filters,
            scope.per_root_max,
            &project_root,
        );
        results.push(result);
        if let Some(phases) = phases.as_mut() {
            phases.add(root_phases);
        } else {
            phases = Some(root_phases);
        }
    }
    let mut phases = phases.unwrap_or_default();
    phases.query_decomposition = query_decomposition;
    (
        merge_grep_results(results, &project_root, params.max_results),
        phases,
    )
}

fn resolve_roots(
    ctx: &AppContext,
    paths: Option<&serde_json::Value>,
    project_root: &Path,
    req_id: &str,
) -> Result<Vec<PathBuf>, Response> {
    let Some(paths) = paths else {
        return Ok(vec![resolve_search_scope(project_root, None).root]);
    };
    if paths.is_null() {
        return Ok(vec![resolve_search_scope(project_root, None).root]);
    }
    if let Some(path) = paths.as_str() {
        return match resolve_path_or_multi(
            path,
            project_root,
            |candidate| ctx.validate_path(req_id, candidate),
            req_id,
        )? {
            SearchPathResolution::Single(root) => Ok(vec![root]),
            SearchPathResolution::Multi(roots) => Ok(roots),
        };
    }
    if let Some(items) = paths.as_array() {
        let mut roots = Vec::with_capacity(items.len());
        for item in items {
            let Some(path) = item.as_str() else {
                return Err(Response::error(
                    req_id,
                    "invalid_request",
                    "grep: path array entries must be strings",
                ));
            };
            let validated = ctx.validate_path(req_id, Path::new(path))?;
            let raw = validated.to_string_lossy();
            roots.push(resolve_search_scope(project_root, Some(raw.as_ref())).root);
        }
        let roots = dedupe_nested_paths(roots);
        if roots.is_empty() {
            Ok(vec![resolve_search_scope(project_root, None).root])
        } else {
            Ok(roots)
        }
    } else {
        Err(Response::error(
            req_id,
            "invalid_request",
            "grep: path must be a string, array of strings, or null",
        ))
    }
}

fn execute_root_profiled(
    ctx: &AppContext,
    pattern: &CompiledPattern,
    query: &RegexQuery,
    root: &ResolvedRoot,
    params: &GrepParams,
    filters: &PathFilters,
    max_results: usize,
    project_root: &Path,
) -> (GrepResult, GrepExecutionPhaseTimings) {
    // Explicit single-file scope: search the named file directly, bypassing the
    // trigram index and the gitignore/.aftignore-aware walk. Matches ripgrep,
    // where naming a file explicitly searches it even when it is gitignored,
    // .aftignored, or not yet indexed. Binary + UTF-8 guards still apply.
    if root.search_root.is_file() {
        if root.use_index {
            crate::commands::configure::trigger_search_index_reload_if_evicted(ctx);
        }
        let index_status = if root.use_index {
            current_index_status(ctx)
        } else {
            IndexStatus::Fallback
        };
        let result = if params
            .path_exclusion
            .is_some_and(|exclude| exclude(&root.search_root, project_root))
        {
            empty_grep_result(index_status, false)
        } else {
            grep_explicit_file(&root.search_root, pattern, max_results, index_status)
        };
        return (result, GrepExecutionPhaseTimings::default());
    }

    let snapshot_started = Instant::now();
    let mut snapshot_timed_out = false;
    let indexed_snapshot =
        match try_read_with_budget(ctx.search_index(), INTERACTIVE_ARTIFACT_READ_BUDGET) {
            Some(search_index) => match search_index.as_ref() {
                Some(index) if index.ready && root.use_index => Some(index.snapshot()),
                _ => None,
            },
            None => {
                snapshot_timed_out = true;
                None
            }
        };
    let snapshot_acquire = snapshot_started.elapsed();
    if let Some(snapshot) = indexed_snapshot {
        let scope_started = Instant::now();
        let indexed_scope_has_files = snapshot.has_file_in_scope(&root.search_root);
        let scope_elapsed = scope_started.elapsed();
        let (result, mut query_timings) = snapshot.search_grep_profiled_with_filters_and_query(
            pattern,
            query,
            filters,
            &root.search_root,
            max_results,
            params.path_exclusion,
        );
        query_timings.post_filter += scope_elapsed;
        return (
            result,
            GrepExecutionPhaseTimings {
                snapshot_acquire,
                query_decomposition: Duration::ZERO,
                query: query_timings,
                indexed_scope_has_files: Some(indexed_scope_has_files),
            },
        );
    }

    if root.use_index {
        crate::commands::configure::trigger_search_index_reload_if_evicted(ctx);
    }
    let index_status = if root.use_index {
        if snapshot_timed_out {
            IndexStatus::Fallback
        } else {
            current_index_status(ctx)
        }
    } else {
        IndexStatus::Fallback
    };
    (
        fallback_grep(
            project_root,
            &root.search_root,
            &root.filter_root,
            pattern,
            filters,
            max_results,
            index_status,
            params.path_exclusion,
        ),
        GrepExecutionPhaseTimings {
            snapshot_acquire,
            ..GrepExecutionPhaseTimings::default()
        },
    )
}

fn empty_grep_result(index_status: IndexStatus, fully_degraded: bool) -> GrepResult {
    GrepResult {
        matches: Vec::new(),
        total_matches: 0,
        files_searched: 0,
        files_with_matches: 0,
        index_status,
        truncated: false,
        fully_degraded,
        engine_capped: false,
        walk_truncated: false,
        skipped_foreign_mounts: 0,
        missing_on_disk: 0,
    }
}

/// Grep a single explicitly-named file directly, bypassing the trigram index
/// and the gitignore/.aftignore-aware walk. Used when the caller's `path`
/// resolves to one existing file — ripgrep semantics: an explicitly-named file
/// is searched even when it is gitignored, `.aftignore`d, or not yet indexed.
/// Binary detection and UTF-8 guards still apply (via `read_searchable_text`
/// inside `fallback_search_file`).
fn grep_explicit_file(
    file: &Path,
    pattern: &CompiledPattern,
    max_results: usize,
    index_status: IndexStatus,
) -> GrepResult {
    let total_matches = AtomicUsize::new(0);
    let files_searched = AtomicUsize::new(0);
    let files_with_matches = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    let engine_capped = AtomicBool::new(false);
    let stop_after = max_results.saturating_mul(2);
    let job_cancellation = crate::executor::current_job_cancellation();

    let matches = fallback_search_file(
        &file.to_path_buf(),
        pattern,
        max_results,
        stop_after,
        &total_matches,
        &files_searched,
        &files_with_matches,
        &truncated,
        &engine_capped,
        job_cancellation.as_ref(),
        None,
    );

    GrepResult {
        total_matches: total_matches.load(Ordering::Relaxed),
        matches,
        files_searched: files_searched.load(Ordering::Relaxed),
        files_with_matches: files_with_matches.load(Ordering::Relaxed),
        index_status,
        truncated: truncated.load(Ordering::Relaxed),
        fully_degraded: false,
        engine_capped: engine_capped.load(Ordering::Relaxed),
        walk_truncated: false,
        skipped_foreign_mounts: 0,
        missing_on_disk: 0,
    }
}

pub fn merge_grep_results(
    results: Vec<GrepResult>,
    project_root: &Path,
    max_results: usize,
) -> GrepResult {
    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut files_searched = 0usize;
    let mut files_with_matches = 0usize;
    let mut index_status = IndexStatus::Ready;
    let mut any_child_truncated = false;
    let mut fully_degraded = false;
    let mut engine_capped = false;
    let mut walk_truncated = false;
    let mut skipped_foreign_mounts = 0usize;
    let mut missing_on_disk = 0usize;
    let mut seen_match_keys = HashSet::new();

    for result in results {
        total_matches += result.total_matches;
        files_searched += result.files_searched;
        files_with_matches += result.files_with_matches;
        index_status = weakest_index_status(index_status, result.index_status);
        any_child_truncated |= result.truncated;
        fully_degraded |= result.fully_degraded;
        engine_capped |= result.engine_capped;
        walk_truncated |= result.walk_truncated;
        skipped_foreign_mounts += result.skipped_foreign_mounts;
        missing_on_disk += result.missing_on_disk;

        for grep_match in result.matches {
            let file_key = canonical_key(&grep_match.file);
            let match_key = (file_key, grep_match.line, grep_match.column);
            if seen_match_keys.insert(match_key) {
                matches.push(grep_match);
            }
        }
    }

    sort_grep_matches_by_mtime_desc(&mut matches, project_root);
    if matches.len() > max_results {
        matches.truncate(max_results);
    }

    GrepResult {
        matches,
        total_matches,
        files_searched,
        files_with_matches,
        index_status,
        truncated: any_child_truncated || total_matches > max_results,
        fully_degraded,
        engine_capped,
        walk_truncated,
        skipped_foreign_mounts,
        missing_on_disk,
    }
}

fn fallback_project_walk_builder(
    search_root: &Path,
    skipped_foreign_mounts: Arc<AtomicUsize>,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(search_root);
    let boundary = crate::walk_boundary::DeviceBoundary::for_root(search_root).ok();
    // A disappearing child mount can make ReadDir::drop panic on ENXIO and abort
    // the daemon, so never open directories outside this walk root's filesystem.
    builder
        .same_file_system(true)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".aftignore")
        .filter_entry(move |entry| {
            if entry.depth() > 0 && entry.file_type().map_or(false, |ft| ft.is_dir()) {
                match boundary
                    .as_ref()
                    .map(|boundary| boundary.should_descend(entry.path()))
                {
                    Some(Ok(false)) => {
                        skipped_foreign_mounts.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                    Some(Err(_)) => return false,
                    _ => {}
                }
            }
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                return !matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "venv"
                        | ".venv"
                        | ".git"
                        | "__pycache__"
                        | ".tox"
                        | "dist"
                        | "build"
                );
            }
            true
        });
    builder
}

/// Bounded project walk used when the trigram index is unavailable (grep/glob fallback).
pub(crate) fn bounded_fallback_walk_files(
    filter_root: &Path,
    search_root: &Path,
    filters: &PathFilters,
) -> FallbackWalkOutcome {
    bounded_fallback_walk_files_with_limits(
        filter_root,
        search_root,
        filters,
        MAX_FALLBACK_WALK_FILES,
        FALLBACK_WALK_BUDGET,
    )
}

fn bounded_fallback_walk_files_with_limits(
    filter_root: &Path,
    search_root: &Path,
    filters: &PathFilters,
    max_files: usize,
    budget: Duration,
) -> FallbackWalkOutcome {
    let started = Instant::now();
    let mut files = Vec::new();
    let mut walk_truncated = false;
    let mut entries_visited = 0usize;
    let skipped_foreign_mounts = Arc::new(AtomicUsize::new(0));
    let builder = fallback_project_walk_builder(search_root, Arc::clone(&skipped_foreign_mounts));

    for entry in builder.build().filter_map(|entry| entry.ok()) {
        entries_visited += 1;
        if started.elapsed() >= budget {
            walk_truncated = true;
            break;
        }
        if !entry
            .file_type()
            .map_or(false, |file_type| file_type.is_file())
        {
            continue;
        }
        let path = entry.into_path();
        if filters.matches(filter_root, &path) {
            files.push(path);
            if files.len() > max_files {
                walk_truncated = true;
                files.truncate(max_files);
                break;
            }
        }
    }

    sort_walked_paths_by_mtime_desc(&mut files, filter_root);
    FallbackWalkOutcome {
        files,
        walk_truncated,
        skipped_foreign_mounts: skipped_foreign_mounts.load(Ordering::Relaxed),
        entries_visited,
    }
}

fn for_each_bounded_fallback_walk_file<F>(
    filter_root: &Path,
    search_root: &Path,
    filters: &PathFilters,
    project_root: &Path,
    path_exclusion: Option<GrepPathExclusion>,
    mut on_file: F,
) -> FallbackWalkProgress
where
    F: FnMut(&PathBuf),
{
    for_each_bounded_fallback_walk_file_with_limits(
        filter_root,
        search_root,
        filters,
        project_root,
        path_exclusion,
        MAX_FALLBACK_WALK_FILES,
        FALLBACK_WALK_BUDGET,
        &mut on_file,
    )
}

fn for_each_bounded_fallback_walk_file_with_limits<F>(
    filter_root: &Path,
    search_root: &Path,
    filters: &PathFilters,
    project_root: &Path,
    path_exclusion: Option<GrepPathExclusion>,
    max_files: usize,
    budget: Duration,
    on_file: &mut F,
) -> FallbackWalkProgress
where
    F: FnMut(&PathBuf),
{
    let started = Instant::now();
    let mut files_seen = 0usize;
    let skipped_foreign_mounts = Arc::new(AtomicUsize::new(0));
    let builder = fallback_project_walk_builder(search_root, Arc::clone(&skipped_foreign_mounts));

    for entry in builder.build().filter_map(|entry| entry.ok()) {
        if crate::executor::current_job_cancelled() {
            return FallbackWalkProgress {
                walk_truncated: true,
                skipped_foreign_mounts: skipped_foreign_mounts.load(Ordering::Relaxed),
            };
        }
        if started.elapsed() >= budget {
            return FallbackWalkProgress {
                walk_truncated: true,
                skipped_foreign_mounts: skipped_foreign_mounts.load(Ordering::Relaxed),
            };
        }
        if !entry
            .file_type()
            .map_or(false, |file_type| file_type.is_file())
        {
            continue;
        }
        let path = entry.into_path();
        if path_exclusion.is_some_and(|exclude| exclude(&path, project_root)) {
            continue;
        }
        if filters.matches(filter_root, &path) {
            files_seen += 1;
            if files_seen > max_files {
                return FallbackWalkProgress {
                    walk_truncated: true,
                    skipped_foreign_mounts: skipped_foreign_mounts.load(Ordering::Relaxed),
                };
            }
            on_file(&path);
        }
    }
    FallbackWalkProgress {
        walk_truncated: false,
        skipped_foreign_mounts: skipped_foreign_mounts.load(Ordering::Relaxed),
    }
}

pub fn weakest_index_status(left: IndexStatus, right: IndexStatus) -> IndexStatus {
    match (left, right) {
        (IndexStatus::Disabled, _) | (_, IndexStatus::Disabled) => IndexStatus::Disabled,
        (IndexStatus::Fallback, _) | (_, IndexStatus::Fallback) => IndexStatus::Fallback,
        (IndexStatus::Building, _) | (_, IndexStatus::Building) => IndexStatus::Building,
        (IndexStatus::Ready, IndexStatus::Ready) => IndexStatus::Ready,
    }
}

/// Hidden entry for `search_startup_bench` timing (fallback grep path).
#[doc(hidden)]
pub fn fallback_grep_bench(
    project_root: &Path,
    search_root: &Path,
    filter_root: &Path,
    pattern: &CompiledPattern,
    include: &[String],
    exclude: &[String],
    max_results: usize,
) -> GrepResult {
    let filters = build_path_filters(include, exclude).unwrap_or_default();
    fallback_grep(
        project_root,
        search_root,
        filter_root,
        pattern,
        &filters,
        max_results,
        IndexStatus::Fallback,
        None,
    )
}

fn fallback_grep(
    project_root: &Path,
    search_root: &Path,
    filter_root: &Path,
    pattern: &CompiledPattern,
    filters: &PathFilters,
    max_results: usize,
    index_status: IndexStatus,
    path_exclusion: Option<GrepPathExclusion>,
) -> GrepResult {
    let total_matches = AtomicUsize::new(0);
    let files_searched = AtomicUsize::new(0);
    let files_with_matches = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    let engine_capped = AtomicBool::new(false);
    let stop_after = max_results.saturating_mul(2);
    let stop_scan = Arc::new(AtomicBool::new(false));
    let scan_deadline = Instant::now() + FALLBACK_WALK_BUDGET;
    let job_cancellation = crate::executor::current_job_cancellation();

    let mut matches = Vec::new();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(256);

    let flush_batch = |batch: &mut Vec<PathBuf>, matches: &mut Vec<GrepMatch>| {
        if batch.is_empty() {
            return;
        }
        let chunk = std::mem::take(batch);
        let partial: Vec<GrepMatch> = chunk
            .par_iter()
            .filter_map(|file| {
                if stop_scan.load(Ordering::Relaxed)
                    || Instant::now() >= scan_deadline
                    || job_cancellation
                        .as_ref()
                        .is_some_and(|token| token.cancel_requested_before_commit())
                {
                    return None;
                }
                let file_matches = fallback_search_file(
                    file,
                    pattern,
                    max_results,
                    stop_after,
                    &total_matches,
                    &files_searched,
                    &files_with_matches,
                    &truncated,
                    &engine_capped,
                    job_cancellation.as_ref(),
                    Some(scan_deadline),
                );
                if truncated.load(Ordering::Relaxed)
                    && total_matches.load(Ordering::Relaxed) >= stop_after
                {
                    stop_scan.store(true, Ordering::Relaxed);
                }
                (!file_matches.is_empty()).then_some(file_matches)
            })
            .flatten()
            .collect();
        matches.extend(partial);
    };

    let progress = for_each_bounded_fallback_walk_file(
        filter_root,
        search_root,
        filters,
        project_root,
        path_exclusion,
        |path| {
            if stop_scan.load(Ordering::Relaxed) {
                return;
            }
            batch.push(path.clone());
            if batch.len() >= 256 {
                flush_batch(&mut batch, &mut matches);
            }
        },
    );
    flush_batch(&mut batch, &mut matches);
    let mut walk_truncated = progress.walk_truncated;
    if Instant::now() >= scan_deadline {
        walk_truncated = true;
        engine_capped.store(true, Ordering::Relaxed);
    }

    sort_grep_matches_by_mtime_desc(&mut matches, project_root);

    GrepResult {
        total_matches: total_matches.load(Ordering::Relaxed),
        matches,
        files_searched: files_searched.load(Ordering::Relaxed),
        files_with_matches: files_with_matches.load(Ordering::Relaxed),
        index_status,
        truncated: truncated.load(Ordering::Relaxed),
        fully_degraded: true,
        engine_capped: engine_capped.load(Ordering::Relaxed),
        walk_truncated,
        skipped_foreign_mounts: progress.skipped_foreign_mounts,
        missing_on_disk: 0,
    }
}

fn fallback_search_file(
    file: &PathBuf,
    pattern: &CompiledPattern,
    max_results: usize,
    stop_after: usize,
    total_matches: &AtomicUsize,
    files_searched: &AtomicUsize,
    files_with_matches: &AtomicUsize,
    truncated: &AtomicBool,
    engine_capped: &AtomicBool,
    job_cancellation: Option<&crate::executor::JobCancellation>,
    deadline: Option<Instant>,
) -> Vec<GrepMatch> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline)
        || should_stop_fallback_search(truncated, total_matches, stop_after, job_cancellation)
    {
        engine_capped.store(true, Ordering::Relaxed);
        return Vec::new();
    }

    let Some(content) = read_searchable_text(file) else {
        return Vec::new();
    };
    files_searched.fetch_add(1, Ordering::Relaxed);

    let line_starts = line_starts(&content);
    let mut seen_lines = HashSet::new();
    let mut matched_this_file = false;
    let mut matches = Vec::new();

    match pattern {
        CompiledPattern::Literal(literal) => search_literal_in_text(
            file,
            &content,
            &line_starts,
            literal,
            max_results,
            stop_after,
            total_matches,
            &mut seen_lines,
            truncated,
            engine_capped,
            &mut matched_this_file,
            &mut matches,
            job_cancellation,
            deadline,
        ),
        CompiledPattern::Regex { compiled, .. } => {
            for matched in compiled.find_iter(content.as_bytes()) {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline)
                    || should_stop_fallback_search(
                        truncated,
                        total_matches,
                        stop_after,
                        job_cancellation,
                    )
                {
                    engine_capped.store(true, Ordering::Relaxed);
                    break;
                }

                let (line, column, line_text) =
                    line_details(&content, &line_starts, matched.start());
                if !seen_lines.insert(line) {
                    continue;
                }

                matched_this_file = true;
                let match_number = total_matches.fetch_add(1, Ordering::Relaxed) + 1;
                if match_number > max_results {
                    truncated.store(true, Ordering::Relaxed);
                    break;
                }

                matches.push(GrepMatch {
                    file: file.clone(),
                    line,
                    column,
                    line_text,
                    match_text: String::from_utf8_lossy(matched.as_bytes()).into_owned(),
                });
            }
        }
    }

    if matched_this_file {
        files_with_matches.fetch_add(1, Ordering::Relaxed);
    }

    matches
}

fn search_literal_in_text(
    file: &Path,
    content: &str,
    line_starts: &[usize],
    literal: &LiteralSearch,
    max_results: usize,
    stop_after: usize,
    total_matches: &AtomicUsize,
    seen_lines: &mut HashSet<u32>,
    truncated: &AtomicBool,
    engine_capped: &AtomicBool,
    matched_this_file: &mut bool,
    matches: &mut Vec<GrepMatch>,
    job_cancellation: Option<&crate::executor::JobCancellation>,
    deadline: Option<Instant>,
) {
    let content_bytes = content.as_bytes();
    let search_content;
    let haystack = if literal.case_insensitive_ascii {
        search_content = content_bytes.to_ascii_lowercase();
        search_content.as_slice()
    } else {
        content_bytes
    };
    let finder = memchr::memmem::Finder::new(&literal.needle);
    let mut start = 0usize;

    while let Some(position) = finder.find(&haystack[start..]) {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || should_stop_fallback_search(truncated, total_matches, stop_after, job_cancellation)
        {
            engine_capped.store(true, Ordering::Relaxed);
            break;
        }

        let offset = start + position;
        start = offset + 1;
        let (line, column, line_text) = line_details(content, line_starts, offset);
        if !seen_lines.insert(line) {
            continue;
        }

        *matched_this_file = true;
        let match_number = total_matches.fetch_add(1, Ordering::Relaxed) + 1;
        if match_number > max_results {
            truncated.store(true, Ordering::Relaxed);
            break;
        }

        let end = offset + literal.needle.len();
        matches.push(GrepMatch {
            file: file.to_path_buf(),
            line,
            column,
            line_text,
            match_text: String::from_utf8_lossy(&content_bytes[offset..end]).into_owned(),
        });
    }
}

fn should_stop_fallback_search(
    truncated: &AtomicBool,
    total_matches: &AtomicUsize,
    stop_after: usize,
    job_cancellation: Option<&crate::executor::JobCancellation>,
) -> bool {
    job_cancellation.is_some_and(|token| token.cancel_requested_before_commit())
        || (truncated.load(Ordering::Relaxed)
            && total_matches.load(Ordering::Relaxed) >= stop_after)
}

pub(crate) fn ripgrep_glob(
    search_root: &Path,
    pattern: &str,
    max_results: usize,
) -> Option<FallbackWalkOutcome> {
    let filters = build_path_filters(&[pattern.to_string()], &[]).ok()?;
    let mut outcome = bounded_fallback_walk_files(search_root, search_root, &filters);
    outcome.files.truncate(max_results);
    Some(outcome)
}

fn current_index_status(ctx: &AppContext) -> IndexStatus {
    let Some(search_index) =
        try_read_with_budget(ctx.search_index(), INTERACTIVE_ARTIFACT_READ_BUDGET)
    else {
        return IndexStatus::Fallback;
    };
    if search_index.as_ref().is_some_and(|index| index.ready) {
        return IndexStatus::Ready;
    }

    let build_in_progress =
        try_read_with_budget(ctx.search_index_rx(), INTERACTIVE_ARTIFACT_READ_BUDGET)
            .is_some_and(|search_index_rx| search_index_rx.is_some());
    if build_in_progress || search_index.is_some() {
        IndexStatus::Building
    } else {
        IndexStatus::Fallback
    }
}

pub fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Floor a byte index to the nearest valid `str` char boundary (never panics).
pub fn floor_char_boundary_str(content: &str, mut index: usize) -> usize {
    index = index.min(content.len());
    while index > 0 && !content.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Prefix of `content` with at most `max_bytes` UTF-8 bytes, truncated on a char boundary.
pub fn truncate_at_char_boundary(content: &str, max_bytes: usize) -> &str {
    let end = floor_char_boundary_str(content, max_bytes);
    &content[..end]
}

pub fn line_details(content: &str, line_starts: &[usize], offset: usize) -> (u32, u32, String) {
    let offset = floor_char_boundary_str(content, offset);
    let line_index = match line_starts.binary_search(&offset) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    };
    let line_start = line_starts.get(line_index).copied().unwrap_or(0);
    let line_end = content[line_start..]
        .find('\n')
        .map(|length| line_start + length)
        .unwrap_or(content.len());
    let line_text = content[line_start..line_end]
        .trim_end_matches('\r')
        .to_string();
    let column = content[line_start..offset].chars().count() as u32 + 1;
    (line_index as u32 + 1, column, line_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grep_match(file: &Path, line: u32, column: u32) -> GrepMatch {
        GrepMatch {
            file: file.to_path_buf(),
            line,
            column,
            line_text: "needle".to_string(),
            match_text: "needle".to_string(),
        }
    }

    fn result(matches: Vec<GrepMatch>, truncated: bool, status: IndexStatus) -> GrepResult {
        GrepResult {
            total_matches: matches.len(),
            files_searched: matches.len(),
            files_with_matches: matches.len(),
            matches,
            index_status: status,
            truncated,
            fully_degraded: false,
            engine_capped: false,
            walk_truncated: false,
            skipped_foreign_mounts: 0,
            missing_on_disk: 0,
        }
    }

    #[test]
    fn optional_path_exclusion_controls_visible_totals_without_affecting_default_grep() {
        fn excludes_tests(path: &Path, root: &Path) -> bool {
            path.strip_prefix(root)
                .is_ok_and(|relative| relative.starts_with("tests"))
        }

        let project = tempfile::tempdir().expect("project");
        let test_file = project.path().join("tests/case.rs");
        let source_file = project.path().join("src/lib.rs");
        std::fs::create_dir_all(test_file.parent().expect("test parent")).expect("test dir");
        std::fs::create_dir_all(source_file.parent().expect("source parent")).expect("source dir");
        std::fs::write(&test_file, "const NEEDLE: &str = \"needle\";\n").expect("test file");
        std::fs::write(&source_file, "pub fn needle() {}\n").expect("source file");
        let pattern = match crate::pattern_compile::compile(
            "needle",
            crate::pattern_compile::CompileOpts {
                literal: true,
                ..crate::pattern_compile::CompileOpts::default()
            },
        ) {
            crate::pattern_compile::CompileResult::Ok(pattern) => pattern,
            other => panic!("compile literal: {other:?}"),
        };

        let filters = PathFilters::default();
        let unfiltered = fallback_grep(
            project.path(),
            project.path(),
            project.path(),
            &pattern,
            &filters,
            10,
            IndexStatus::Fallback,
            None,
        );
        assert_eq!(unfiltered.total_matches, 2);
        assert_eq!(unfiltered.matches.len(), 2);

        let visible = fallback_grep(
            project.path(),
            project.path(),
            project.path(),
            &pattern,
            &filters,
            10,
            IndexStatus::Fallback,
            Some(excludes_tests),
        );
        assert_eq!(visible.total_matches, 1);
        assert_eq!(visible.matches.len(), 1);
        assert_eq!(visible.files_searched, 1);
        assert_eq!(visible.files_with_matches, 1);
        assert_eq!(visible.matches[0].file, source_file);
        assert!(!visible.truncated);
        assert!(!visible.engine_capped);
    }

    #[test]
    fn single_root_uses_requested_max() {
        let scope = GrepScope {
            roots: vec![ResolvedRoot {
                search_root: PathBuf::from("/project"),
                filter_root: PathBuf::from("/project"),
                use_index: true,
                is_external: false,
            }],
            multi_root: false,
            per_root_max: 10,
        };
        assert!(!scope.multi_root);
        assert_eq!(scope.per_root_max, 10);
    }

    #[test]
    fn multi_root_uses_double_per_root_max() {
        let project = tempfile::tempdir().expect("project");
        let ctx = AppContext::new(
            Box::new(crate::parser::TreeSitterProvider::new()),
            crate::config::Config {
                project_root: Some(project.path().to_path_buf()),
                ..crate::config::Config::default()
            },
        );
        let left = project.path().join("left");
        let right = project.path().join("right");
        std::fs::create_dir_all(&left).expect("left");
        std::fs::create_dir_all(&right).expect("right");
        let paths = serde_json::json!([left.display().to_string(), right.display().to_string()]);

        let scope = resolve_grep_scope(&ctx, Some(&paths), 10, "test").expect("scope");

        assert!(scope.multi_root);
        assert_eq!(scope.per_root_max, 20);
    }

    #[test]
    fn bounded_fallback_walk_truncates_at_file_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for i in 0..25 {
            let path = root.join(format!("file_{i:03}.txt"));
            std::fs::write(path, "needle\n").expect("write");
        }
        let filters = build_path_filters(&["**/*.txt".to_string()], &[]).expect("filters");
        let outcome = bounded_fallback_walk_files_with_limits(
            root,
            root,
            &filters,
            20,
            Duration::from_secs(60),
        );
        assert!(outcome.walk_truncated);
        assert_eq!(outcome.files.len(), 20);
    }

    #[test]
    fn bounded_fallback_walk_small_tree_not_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "x\n").expect("write");
        std::fs::write(root.join("b.txt"), "x\n").expect("write");
        let filters = build_path_filters(&["**/*.txt".to_string()], &[]).expect("filters");
        let outcome = bounded_fallback_walk_files(root, root, &filters);
        assert!(!outcome.walk_truncated);
        assert_eq!(outcome.files.len(), 2);
    }

    #[test]
    fn filter_root_is_project_for_in_project_and_search_root_for_external_unindexed() {
        let project = PathBuf::from("/project");
        let in_project = compute_filter_root(&project, Path::new("/project/src"), true, false);
        let external = compute_filter_root(&project, Path::new("/tmp/external"), false, true);
        assert_eq!(in_project, project);
        assert_eq!(external, PathBuf::from("/tmp/external"));
    }

    #[test]
    fn weakest_status_orders_disabled_fallback_building_ready() {
        assert_eq!(
            weakest_index_status(IndexStatus::Ready, IndexStatus::Building),
            IndexStatus::Building
        );
        assert_eq!(
            weakest_index_status(IndexStatus::Building, IndexStatus::Fallback),
            IndexStatus::Fallback
        );
        assert_eq!(
            weakest_index_status(IndexStatus::Fallback, IndexStatus::Disabled),
            IndexStatus::Disabled
        );
    }

    #[test]
    fn merge_dedupes_by_canonical_file_line_column() {
        let temp = tempfile::tempdir().expect("temp");
        let file = temp.path().join("file.rs");
        std::fs::write(&file, "needle").expect("write");
        let symlink = temp.path().join("link.rs");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file, &symlink).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&file, &symlink).expect("symlink");

        let merged = merge_grep_results(
            vec![
                result(vec![grep_match(&file, 1, 1)], false, IndexStatus::Ready),
                result(vec![grep_match(&symlink, 1, 1)], false, IndexStatus::Ready),
            ],
            temp.path(),
            10,
        );

        assert_eq!(merged.matches.len(), 1);
    }

    #[test]
    fn merge_truncated_when_child_truncated_or_pre_merge_exceeds_max() {
        let root = Path::new("/project");
        let child = merge_grep_results(
            vec![result(
                vec![grep_match(Path::new("/project/a.rs"), 1, 1)],
                true,
                IndexStatus::Ready,
            )],
            root,
            10,
        );
        assert!(child.truncated);

        let many = merge_grep_results(
            vec![
                result(
                    vec![grep_match(Path::new("/project/a.rs"), 1, 1)],
                    false,
                    IndexStatus::Ready,
                ),
                result(
                    vec![grep_match(Path::new("/project/b.rs"), 1, 1)],
                    false,
                    IndexStatus::Ready,
                ),
            ],
            root,
            1,
        );
        assert!(many.truncated);
    }

    #[test]
    fn line_details_floors_offset_inside_multibyte_char() {
        let content = "before—after";
        let starts = line_starts(content);
        let dash_byte = content.find('—').expect("em dash");
        let mid_byte = dash_byte + 1;
        assert!(!content.is_char_boundary(mid_byte));
        let (line, column, line_text) = line_details(content, &starts, mid_byte);
        assert_eq!(line, 1);
        assert_eq!(column, content[..dash_byte].chars().count() as u32 + 1);
        assert!(line_text.contains('—'));
    }

    #[test]
    fn line_details_clamps_offset_past_end() {
        let content = "short";
        let starts = line_starts(content);
        let (line, column, _) = line_details(content, &starts, content.len() + 100);
        assert_eq!(line, 1);
        assert_eq!(column, 6);
    }

    #[test]
    fn truncate_at_char_boundary_floors_mid_multibyte_at_byte_cap() {
        let mut prefix = "a".repeat(38);
        prefix.push('—');
        prefix.push_str("tail");
        assert_eq!(prefix.len(), 45);
        assert!(!prefix.is_char_boundary(40));
        let truncated = truncate_at_char_boundary(&prefix, 40);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.ends_with('a'));
        assert!(!truncated.contains('—'));
    }

    #[test]
    fn regex_byte_match_start_mid_char_does_not_panic_in_line_details() {
        use crate::pattern_compile::{CompileOpts, CompileResult};

        let content = "xy—zz";
        let starts = line_starts(content);
        let compiled = match crate::pattern_compile::compile(
            ".",
            CompileOpts {
                multi_line: false,
                ..CompileOpts::default()
            },
        ) {
            CompileResult::Ok(compiled) => compiled,
            other => panic!("expected compiled pattern, got {other:?}"),
        };
        let crate::pattern_compile::CompiledPattern::Regex { compiled, .. } = compiled else {
            panic!("expected regex pattern");
        };
        for matched in compiled.find_iter(content.as_bytes()) {
            let _ = line_details(content, &starts, matched.start());
        }
    }

    fn compiled_regex(pattern: &str) -> CompiledPattern {
        match crate::pattern_compile::compile(
            pattern,
            crate::pattern_compile::CompileOpts::default(),
        ) {
            crate::pattern_compile::CompileResult::Ok(compiled) => compiled,
            other => panic!("compile regex {pattern:?}: {other:?}"),
        }
    }

    fn grep_result_bytes(result: &GrepResult) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "matches": result.matches.iter().map(|matched| serde_json::json!({
                "file": matched.file,
                "line": matched.line,
                "column": matched.column,
                "line_text": matched.line_text,
                "match_text": matched.match_text,
            })).collect::<Vec<_>>(),
            "total_matches": result.total_matches,
            "files_searched": result.files_searched,
            "files_with_matches": result.files_with_matches,
            "index_status": result.index_status.as_str(),
            "truncated": result.truncated,
            "fully_degraded": result.fully_degraded,
            "engine_capped": result.engine_capped,
            "walk_truncated": result.walk_truncated,
        }))
        .expect("serialize grep result projection")
    }

    #[test]
    fn multi_root_shared_query_matches_per_root_query_bytes() {
        let project = tempfile::tempdir().expect("project");
        let root_names = ["api", "cli", "daemon", "worker"];
        let roots = root_names
            .iter()
            .map(|name| {
                let root = project.path().join(name);
                std::fs::create_dir_all(&root).expect("create root");
                std::fs::write(
                    root.join("service.rs"),
                    "fn needle_alpha_12() {}\nfn needle_beta_34() {}\n",
                )
                .expect("write fixture");
                std::fs::canonicalize(root).expect("canonicalize root")
            })
            .collect::<Vec<_>>();
        let pattern = compiled_regex(r"needle_(?:alpha|beta)_\d+");
        let filters = PathFilters::default();
        let params = GrepParams {
            include: Vec::new(),
            exclude: Vec::new(),
            max_results: 100,
            path_exclusion: None,
        };
        let scope = GrepScope {
            roots: roots
                .iter()
                .map(|root| ResolvedRoot {
                    search_root: root.clone(),
                    filter_root: project.path().to_path_buf(),
                    use_index: true,
                    is_external: false,
                })
                .collect(),
            multi_root: true,
            per_root_max: 200,
        };
        let index =
            crate::search_index::SearchIndex::build_with_limit_serial(project.path(), 1_048_576);
        let snapshot = index.snapshot();
        let expected = merge_grep_results(
            scope
                .roots
                .iter()
                .map(|root| {
                    snapshot
                        .search_grep_profiled_with_filters(
                            &pattern,
                            &filters,
                            &root.search_root,
                            scope.per_root_max,
                            None,
                        )
                        .0
                })
                .collect(),
            project.path(),
            params.max_results,
        );
        let ctx = AppContext::new(
            Box::new(crate::parser::TreeSitterProvider::new()),
            crate::config::Config {
                project_root: Some(project.path().to_path_buf()),
                ..crate::config::Config::default()
            },
        );
        *ctx.search_index().write().expect("lock search index") = Some(index);

        let (actual, phases) =
            execute_profiled_with_filters(&ctx, &pattern, &scope, &params, &filters);

        assert_eq!(
            grep_result_bytes(&actual),
            grep_result_bytes(&expected),
            "shared query search must preserve the legacy per-root result bytes"
        );
        assert!(
            !phases.query_decomposition.is_zero(),
            "the indexed request must record its one-time query decomposition"
        );
    }

    /// Manual release-mode probe for a warm indexed TypeScript corpus searched across four roots.
    #[test]
    #[ignore = "manual release-mode issue #219 multi-root query performance probe"]
    fn issue_219_multi_root_query_reuse_perf_probe() {
        const ROOTS: usize = 4;
        const FILES_PER_ROOT: usize = 1_000;
        const SAMPLES: usize = 9;
        const ITERATIONS: usize = 300;

        let project_root = PathBuf::from("/tmp/aft-issue-219-multi-root");
        let mut index = crate::search_index::SearchIndex::new();
        let roots = (0..ROOTS)
            .map(|root_index| project_root.join(format!("packages/root-{root_index}")))
            .collect::<Vec<_>>();
        for root in &roots {
            for file_index in 0..FILES_PER_ROOT {
                index.index_file(
                    &root.join(format!("src/module-{file_index:04}.ts")),
                    b"export const indexed_value = 'warm corpus';\n",
                );
            }
        }
        let snapshot = index.snapshot();
        let pattern =
            compiled_regex(r"(?:(?:parse|format|validate)_[A-Za-z0-9_]+_)?issue_219_never_present");
        let filters = PathFilters::default();

        let per_root_once = || {
            for root in &roots {
                let result = snapshot
                    .search_grep_profiled_with_filters(&pattern, &filters, root, 100, None)
                    .0;
                std::hint::black_box(result.total_matches);
            }
        };
        let shared_query_once = || {
            let query = decompose_grep_pattern(&pattern);
            for root in &roots {
                let result = snapshot
                    .search_grep_profiled_with_filters_and_query(
                        &pattern, &query, &filters, root, 100, None,
                    )
                    .0;
                std::hint::black_box(result.total_matches);
            }
        };

        let mut per_root_ns = Vec::with_capacity(SAMPLES);
        let mut shared_query_ns = Vec::with_capacity(SAMPLES);
        for sample in 0..SAMPLES {
            let measure = |operation: &dyn Fn()| {
                let started = Instant::now();
                for _ in 0..ITERATIONS {
                    operation();
                }
                started.elapsed().as_nanos() / ITERATIONS as u128
            };
            if sample % 2 == 0 {
                per_root_ns.push(measure(&per_root_once));
                shared_query_ns.push(measure(&shared_query_once));
            } else {
                shared_query_ns.push(measure(&shared_query_once));
                per_root_ns.push(measure(&per_root_once));
            }
        }
        per_root_ns.sort_unstable();
        shared_query_ns.sort_unstable();
        let per_root_median = per_root_ns[SAMPLES / 2];
        let shared_query_median = shared_query_ns[SAMPLES / 2];
        let speedup = per_root_median as f64 / shared_query_median as f64;

        eprintln!(
            "issue #219 multi-root regex query: roots={ROOTS} files_per_root={FILES_PER_ROOT} samples={SAMPLES} iterations={ITERATIONS}"
        );
        eprintln!("per-root decomposition ns/op samples: {per_root_ns:?}");
        eprintln!("shared decomposition ns/op samples: {shared_query_ns:?}");
        eprintln!(
            "median: per-root={per_root_median}ns shared={shared_query_median}ns speedup={speedup:.2}x"
        );
    }
}
