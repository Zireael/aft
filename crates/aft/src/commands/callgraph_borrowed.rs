//! Disclosure for callgraph answers served from another checkout's graph.
//!
//! A linked worktree, or a second clone of the same repository, does not build
//! its own callgraph store: it reads the one the owning checkout publishes
//! (borrow-only mode, see `artifact_owner`). That graph describes the owner's
//! files. When this checkout sits on another commit or has edits of its own,
//! callers, line numbers and even whether a symbol exists can differ from the
//! files here, and an unmarked answer would look complete and current.
//!
//! Until each checkout gets its own graph view, every callgraph query answered
//! from a borrowed store checks whether the borrowed graph can differ from this
//! checkout and, when it can, marks the answer `complete: false` with a
//! one-line explanation. The check uses only signals already at hand: the two
//! checkouts' HEAD commits, read straight from the git metadata files, and
//! the per-file content hashes the callgraph store recorded, compared with
//! the hashes the search index's RAM overlay holds for this checkout's files.
//! It never walks the working tree; when the two checkouts share a commit and
//! some files differ, git is asked which of them this checkout has edited.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::artifact_owner::ArtifactOwnerMode;
use crate::cache_freshness;
use crate::context::AppContext;
use crate::protocol::Response;

/// Where the queried symbol of an operation is looked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolLookup {
    /// The symbol is resolved in the callgraph store, so a not-found answer
    /// can mean only that the borrowed graph lacks it.
    Graph,
    /// The symbol is resolved by parsing this checkout's own file, so a
    /// not-found answer is already about this checkout.
    Checkout,
}

/// Error codes that report a symbol missing from the graph.
const GRAPH_NOT_FOUND_CODES: [&str; 2] = ["symbol_not_found", "target_symbol_not_found"];

/// How a borrowed callgraph relates to this checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorrowedCallgraph {
    /// The checkout whose graph is being read; `None` when it is unknown.
    pub owner_checkout: Option<PathBuf>,
    /// The owner checkout's current HEAD commit.
    pub owner_head: Option<String>,
    /// This checkout's HEAD commit.
    pub checkout_head: Option<String>,
    /// Files whose content differs between the source snapshot the borrowed
    /// graph describes and this checkout's disk; `None` when that is not
    /// known.
    pub changed_files: Option<usize>,
    /// Which checkout holds the edits behind those differences, when it is
    /// known.
    pub edits_in: Option<EditsIn>,
}

/// The checkout whose uncommitted edits make the borrowed graph differ from
/// this checkout. Known only when both checkouts are on the same commit, so
/// that every difference is an edit on one side or the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditsIn {
    /// The checkout that owns the graph: this checkout's differing files all
    /// match its commit.
    BorrowedCheckout,
    /// This checkout: every differing file has edits here.
    ThisCheckout,
    /// Some differing files have edits here and some do not.
    BothCheckouts,
}

impl EditsIn {
    fn label(self) -> &'static str {
        match self {
            EditsIn::BorrowedCheckout => "borrowed_checkout",
            EditsIn::ThisCheckout => "this_checkout",
            EditsIn::BothCheckouts => "both_checkouts",
        }
    }

    fn clause(self) -> &'static str {
        match self {
            EditsIn::BorrowedCheckout => " (edits in the borrowed checkout)",
            EditsIn::ThisCheckout => " (edits in this checkout)",
            EditsIn::BothCheckouts => " (edits in both checkouts)",
        }
    }
}

impl BorrowedCallgraph {
    /// True unless the borrowed graph provably describes this checkout: both
    /// checkouts are on the same known commit and no file here is known to
    /// differ from the files the graph was built from. Anything unknown
    /// counts as a possible difference.
    pub fn can_differ(&self) -> bool {
        let same_head = matches!(
            (&self.owner_head, &self.checkout_head),
            (Some(owner), Some(checkout)) if owner == checkout
        );
        !(same_head && self.changed_files == Some(0))
    }

    fn provenance(&self) -> String {
        let owner = self
            .owner_checkout
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "another checkout".to_string());
        let changed = match self.changed_files {
            // The count is measured against the files the borrowed graph was
            // built from, not against this checkout's own HEAD, so a clean
            // checkout on another commit still has files that differ; the
            // phrase names the graph to make that clear.
            Some(1) => "1 file that differs from the borrowed graph".to_string(),
            Some(count) => format!("{count} files that differ from the borrowed graph"),
            None => "an unknown number of files that differ from the borrowed graph".to_string(),
        };
        let edits_in = match (self.changed_files, self.edits_in) {
            (Some(count), Some(edits_in)) if count > 0 => edits_in.clause(),
            _ => "",
        };
        format!(
            "borrowed from {owner} at {}; this checkout is at {} with {changed}{edits_in}",
            short_commit(self.owner_head.as_deref()),
            short_commit(self.checkout_head.as_deref()),
        )
    }

    /// The single line added to a successful answer.
    pub fn summary_line(&self) -> String {
        format!(
            "callgraph: {}, so callers and line numbers may not match this tree",
            self.provenance()
        )
    }

    /// The clause appended to a not-found error from the borrowed graph.
    pub fn not_found_hint(&self) -> String {
        format!(
            "the callgraph is {}, so the symbol may exist in this checkout but not in the borrowed graph; use grep or aft_zoom on this checkout to confirm",
            self.provenance()
        )
    }

    fn to_json(&self) -> Value {
        json!({
            "owner_checkout": self.owner_checkout.as_ref().map(|path| path.display().to_string()),
            "owner_head": self.owner_head,
            "checkout_head": self.checkout_head,
            "changed_files": self.changed_files,
            "edits_in": self.edits_in.map(EditsIn::label),
            "message": self.summary_line(),
        })
    }
}

fn short_commit(commit: Option<&str>) -> String {
    match commit {
        // Object ids are ASCII hex, so the first seven bytes are the usual
        // abbreviated commit.
        Some(commit) => commit.get(..7).unwrap_or(commit).to_string(),
        None => "an unknown commit".to_string(),
    }
}

/// Annotate a callgraph response that was answered from a borrowed store
/// whose graph can differ from this checkout. Successful answers get
/// `complete: false` and a `borrowed_callgraph` object; not-found errors for a
/// symbol looked up in the graph get a hint appended to their message. Every
/// other response, and every response on the owner checkout or on a
/// per-checkout view, is left exactly as it was.
pub fn disclose_borrowed_answer(ctx: &AppContext, response: &mut Response, lookup: SymbolLookup) {
    let graph_not_found = lookup == SymbolLookup::Graph
        && !response.success
        && response
            .data
            .get("code")
            .and_then(Value::as_str)
            .is_some_and(|code| GRAPH_NOT_FOUND_CODES.contains(&code));
    if !response.success && !graph_not_found {
        return;
    }
    let Some(borrowed) = borrowed_callgraph(ctx).filter(BorrowedCallgraph::can_differ) else {
        return;
    };
    let Some(data) = response.data.as_object_mut() else {
        return;
    };
    if response.success {
        data.insert("complete".to_string(), Value::Bool(false));
        data.insert("borrowed_callgraph".to_string(), borrowed.to_json());
        return;
    }
    let message = data
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_string();
    data.insert(
        "message".to_string(),
        Value::String(format!("{message}; {}", borrowed.not_found_hint())),
    );
}

/// Describe the borrowed callgraph this context answers from, or `None` when
/// callgraph answers come from this checkout's own data: the owner checkout,
/// a per-checkout query runtime, or a pinned per-HEAD view.
pub fn borrowed_callgraph(ctx: &AppContext) -> Option<BorrowedCallgraph> {
    if !ctx.shared_artifacts_read_only() || ctx.checkout_query_runtime_active() {
        return None;
    }
    if ctx.config().views.enabled
        && ctx
            .pinned_view_runtime()
            .is_some_and(|view| view.manifest.is_some())
    {
        return None;
    }
    let checkout_root = ctx.config().project_root.clone()?;
    let checkout_root = canonical(&checkout_root);
    let owner_checkout = match owner_checkout(ctx, &checkout_root) {
        Owner::Other(path) => Some(path),
        Owner::Unknown => None,
        Owner::ThisCheckout => return None,
    };
    let owner_head = owner_checkout.as_deref().and_then(checkout_head);
    let checkout_head = checkout_head(&checkout_root);
    let differing =
        files_differing_from_borrowed_graph(ctx, &checkout_root, owner_checkout.as_deref());
    let edits_in = differing.as_deref().and_then(|differing| {
        let same_head = matches!(
            (&owner_head, &checkout_head),
            (Some(owner), Some(checkout)) if owner == checkout
        );
        (same_head && !differing.is_empty())
            .then(|| locate_edits(&checkout_root, differing))
            .flatten()
    });
    Some(BorrowedCallgraph {
        owner_head,
        owner_checkout,
        checkout_head,
        changed_files: differing.as_ref().map(Vec::len),
        edits_in,
    })
}

enum Owner {
    Other(PathBuf),
    Unknown,
    ThisCheckout,
}

fn owner_checkout(ctx: &AppContext, checkout_root: &Path) -> Owner {
    let recorded = ctx
        .artifact_owner_status()
        .filter(|status| status.mode == ArtifactOwnerMode::ReadOnly)
        .map(|status| canonical(Path::new(&status.owner_checkout_path)));
    if let Some(owner) = recorded.filter(|owner| owner != checkout_root) {
        return Owner::Other(owner);
    }
    // A read-only checkout that is not a linked worktree and names itself as
    // the owner (for example, artifacts written by a newer build) reads its
    // own data, so there is nothing to disclose.
    if !ctx.is_worktree_bridge() {
        return Owner::ThisCheckout;
    }
    // A linked worktree whose owner manifest was unreadable falls back to its
    // own path. The graph still comes from elsewhere; the main worktree, the
    // parent of the shared `.git` directory, is the usual owner.
    ctx.git_common_dir()
        .filter(|dir| dir.file_name().is_some_and(|name| name == ".git"))
        .and_then(|dir| dir.parent().map(canonical))
        .filter(|main| main != checkout_root)
        .map_or(Owner::Unknown, Owner::Other)
}

/// The most files read from disk to decide one disclosure. Nearly every file
/// is compared from hashes already in memory; this bounds the rest (files the
/// search index never hashed, or holds no record of) so a pathological tree
/// costs a bounded amount per answer and is reported as unknown instead.
const MAX_DISK_HASHES: usize = 256;

/// Root-relative paths of the files whose content differs between the source
/// snapshot the borrowed graph describes and this checkout's disk.
///
/// The graph's side is the per-file size and content hash the callgraph store
/// recorded when it parsed each file. This checkout's side comes from the
/// search index's RAM overlay, which has already compared the borrowed search
/// snapshot with this checkout's files and keeps itself current from watcher
/// events, so it holds this checkout's hashes without walking the tree. The
/// overlay's own delta is not the answer: it is measured against the shared
/// search snapshot, which the owner rewrites only on shutdown or a rebuild,
/// so it also counts every file the owner changed since then, and files it
/// never hashed (binary or oversized ones), even when this checkout matches
/// the graph exactly.
///
/// `None` when this is not known: without the overlay, before the store or
/// the index is ready, or when deciding would read too many files.
fn files_differing_from_borrowed_graph(
    ctx: &AppContext,
    checkout_root: &Path,
    owner_root: Option<&Path>,
) -> Option<Vec<String>> {
    if !ctx.ram_overlay_active() {
        return None;
    }
    let checkout = {
        let guard = ctx
            .search_index()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = guard.as_ref()?;
        if !index.ready || index.build_denied {
            return None;
        }
        index.live_file_identities()
    };
    let store = ctx
        .callgraph_store()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    let graph = store.file_identities().ok()?;
    compare_with_graph(&graph, checkout, checkout_root, owner_root, MAX_DISK_HASHES)
}

/// Size and content hash of one file; a zero hash means it was never hashed.
type FileIdentity = (u64, blake3::Hash);

/// The disk-read budget of one comparison ran out.
struct BudgetSpent;

struct DiskHasher {
    remaining: usize,
}

impl DiskHasher {
    /// Hash the file at `path`; `Ok(None)` when it cannot be read.
    fn hash(&mut self, path: &Path) -> Result<Option<blake3::Hash>, BudgetSpent> {
        self.remaining = self.remaining.checked_sub(1).ok_or(BudgetSpent)?;
        Ok(fs::read(path)
            .ok()
            .map(|bytes| cache_freshness::hash_bytes(&bytes)))
    }

    /// Whether the file at `path`, whose size is `size` and whose hash is
    /// `hash` when it was hashed, holds the content `expected` describes.
    /// When `expected` has no hash (the graph does not hash very large
    /// files) the sizes are all there is to compare.
    fn same_content(
        &mut self,
        expected: FileIdentity,
        size: u64,
        hash: blake3::Hash,
        path: &Path,
    ) -> Result<bool, BudgetSpent> {
        let zero = cache_freshness::zero_hash();
        let (expected_size, expected_hash) = expected;
        if size != expected_size {
            return Ok(false);
        }
        if expected_hash == zero {
            return Ok(true);
        }
        if hash != zero {
            return Ok(hash == expected_hash);
        }
        Ok(self.hash(path)? == Some(expected_hash))
    }
}

/// Compare the graph's recorded files with this checkout's files, both keyed
/// by root-relative path with `/` separators, and return the differing paths
/// sorted.
///
/// A graph file is compared with this checkout's record of the same path or,
/// when this checkout's index has none (its walk can skip what the graph's
/// walk keeps), with the file on disk; a graph file missing here differs. A
/// file here that the graph lacks differs only when the graph's walk would
/// include it and the borrowed checkout lacks it or holds other content: a
/// file both checkouts hold identically is one the graph equally leaves out
/// here, for example a source file its parser rejected.
fn compare_with_graph(
    graph: &HashMap<String, FileIdentity>,
    mut checkout: HashMap<String, FileIdentity>,
    checkout_root: &Path,
    owner_root: Option<&Path>,
    max_disk_hashes: usize,
) -> Option<Vec<String>> {
    let zero = cache_freshness::zero_hash();
    let mut hasher = DiskHasher {
        remaining: max_disk_hashes,
    };
    let mut differing = Vec::new();
    for (relative, &expected) in graph {
        let path = checkout_root.join(relative);
        let same = match checkout.remove(relative) {
            Some((size, hash)) => hasher.same_content(expected, size, hash, &path).ok()?,
            None => match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => hasher
                    .same_content(expected, metadata.len(), zero, &path)
                    .ok()?,
                _ => false,
            },
        };
        if !same {
            differing.push(relative.clone());
        }
    }
    for (relative, (size, hash)) in checkout {
        if !crate::callgraph::walk_could_include(&relative) {
            continue;
        }
        let same_as_owner = match owner_root {
            Some(owner_root) => {
                let owner_path = owner_root.join(&relative);
                match fs::metadata(&owner_path) {
                    Ok(metadata) if metadata.is_file() && metadata.len() == size => {
                        let owner_hash = hasher.hash(&owner_path).ok()?;
                        let here = if hash == zero {
                            hasher.hash(&checkout_root.join(&relative)).ok()?
                        } else {
                            Some(hash)
                        };
                        owner_hash.is_some() && owner_hash == here
                    }
                    _ => false,
                }
            }
            None => false,
        };
        if !same_as_owner {
            differing.push(relative);
        }
    }
    differing.sort();
    Some(differing)
}

/// Which checkout holds the edits behind `differing`, for two checkouts on
/// the same commit: a differing file this checkout has not changed from that
/// commit must have been changed in the borrowed checkout. Asks git for this
/// checkout's changed and untracked files; `None` when git cannot say.
fn locate_edits(checkout_root: &Path, differing: &[String]) -> Option<EditsIn> {
    let mut edited_here = git_paths(
        checkout_root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--relative",
            "-z",
            "HEAD",
        ],
    )?;
    edited_here.extend(git_paths(
        checkout_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    let here = differing
        .iter()
        .filter(|path| edited_here.contains(*path))
        .count();
    Some(match here {
        0 => EditsIn::BorrowedCheckout,
        here if here == differing.len() => EditsIn::ThisCheckout,
        _ => EditsIn::BothCheckouts,
    })
}

/// Run a git command in `root` that prints NUL-separated paths relative to
/// it, with a short deadline so a slow repository cannot hold up an answer.
fn git_paths(root: &Path, args: &[&str]) -> Option<HashSet<String>> {
    const GIT_DEADLINE: Duration = Duration::from_secs(2);

    if !crate::developer_tools::git_usable() {
        return None;
    }
    let mut child = crate::effective_path::new_command("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout while waiting: a long path list would otherwise fill the
    // pipe and stall git until the deadline.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + GIT_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()?.ok()?;
    if !status?.success() {
        return None;
    }
    Some(
        bytes
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
            .collect(),
    )
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve a checkout's HEAD commit from its git metadata files without
/// running git: `.git` (a directory, or a `gitdir:` file in a linked
/// worktree), `HEAD`, loose refs, and `packed-refs`. Returns `None` for
/// anything it cannot resolve, such as a reftable repository.
pub fn checkout_head(checkout: &Path) -> Option<String> {
    let dot_git = checkout.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let marker = fs::read_to_string(&dot_git).ok()?;
        let target = PathBuf::from(marker.trim().strip_prefix("gitdir:")?.trim());
        if target.is_absolute() {
            target
        } else {
            checkout.join(target)
        }
    };
    // A linked worktree keeps HEAD in its own git dir and shares branch refs
    // through the directory named by `commondir`.
    let common_dir = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => {
            let path = PathBuf::from(text.trim());
            if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }
        }
        Err(_) => git_dir.clone(),
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    resolve_ref_value(&git_dir, &common_dir, head.trim(), 0)
}

/// Resolve the text of a ref file: an object id, or `ref: <name>` pointing at
/// another ref. Symbolic chains are followed a few levels at most.
fn resolve_ref_value(git_dir: &Path, common_dir: &Path, value: &str, depth: u8) -> Option<String> {
    let Some(reference) = value.strip_prefix("ref:").map(str::trim) else {
        return is_object_id(value).then(|| value.to_string());
    };
    if depth >= 5 || reference.is_empty() {
        return None;
    }
    for dir in [git_dir, common_dir] {
        if let Ok(text) = fs::read_to_string(dir.join(reference)) {
            return resolve_ref_value(git_dir, common_dir, text.trim(), depth + 1);
        }
    }
    let packed = fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .filter_map(|line| line.split_once(' '))
        .find(|(_, name)| name.trim() == reference)
        .map(|(id, _)| id.trim())
        .filter(|id| is_object_id(id))
        .map(str::to_string)
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn borrowed(
        owner_head: Option<&str>,
        checkout_head: Option<&str>,
        changed_files: Option<usize>,
    ) -> BorrowedCallgraph {
        BorrowedCallgraph {
            owner_checkout: Some(PathBuf::from("/work/owner")),
            owner_head: owner_head.map(str::to_string),
            checkout_head: checkout_head.map(str::to_string),
            changed_files,
            edits_in: None,
        }
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn only_a_known_same_head_with_no_changed_files_proves_the_graph_fits() {
        assert!(!borrowed(Some(A), Some(A), Some(0)).can_differ());
        assert!(borrowed(Some(A), Some(B), Some(0)).can_differ());
        assert!(borrowed(Some(A), Some(A), Some(2)).can_differ());
        assert!(borrowed(Some(A), Some(A), None).can_differ());
        assert!(borrowed(None, Some(A), Some(0)).can_differ());
        assert!(borrowed(None, None, Some(0)).can_differ());
    }

    #[test]
    fn disclosure_lines_are_single_lines_naming_both_commits_and_the_change_count() {
        let line = borrowed(Some(A), Some(B), Some(3)).summary_line();
        assert_eq!(
            line,
            "callgraph: borrowed from /work/owner at aaaaaaa; this checkout is at bbbbbbb with 3 files that differ from the borrowed graph, so callers and line numbers may not match this tree"
        );
        let unknown = BorrowedCallgraph {
            owner_checkout: None,
            owner_head: None,
            checkout_head: Some(B.to_string()),
            changed_files: None,
            edits_in: None,
        }
        .not_found_hint();
        assert_eq!(
            unknown,
            "the callgraph is borrowed from another checkout at an unknown commit; this checkout is at bbbbbbb with an unknown number of files that differ from the borrowed graph, so the symbol may exist in this checkout but not in the borrowed graph; use grep or aft_zoom on this checkout to confirm"
        );
        assert!(!line.contains('\n') && !unknown.contains('\n'));
        assert!(borrowed(Some(A), Some(B), Some(1))
            .summary_line()
            .contains("with 1 file that differs from the borrowed graph,"));
    }

    #[test]
    fn disclosure_names_the_checkout_holding_the_edits_when_known() {
        let located = |changed_files, edits_in| BorrowedCallgraph {
            edits_in,
            ..borrowed(Some(A), Some(A), changed_files)
        };
        assert_eq!(
            located(Some(3), Some(EditsIn::BorrowedCheckout)).summary_line(),
            "callgraph: borrowed from /work/owner at aaaaaaa; this checkout is at aaaaaaa with 3 files that differ from the borrowed graph (edits in the borrowed checkout), so callers and line numbers may not match this tree"
        );
        assert!(located(Some(2), Some(EditsIn::ThisCheckout))
            .not_found_hint()
            .contains("with 2 files that differ from the borrowed graph (edits in this checkout), so the symbol"));
        assert!(located(Some(2), Some(EditsIn::BothCheckouts))
            .summary_line()
            .contains("(edits in both checkouts), so callers"));
        // Without a count there is nothing to locate.
        assert!(!located(None, Some(EditsIn::ThisCheckout))
            .summary_line()
            .contains("(edits"));
        assert_eq!(
            located(Some(3), Some(EditsIn::BorrowedCheckout)).to_json()["edits_in"],
            json!("borrowed_checkout")
        );
    }

    fn identity(content: &str) -> FileIdentity {
        (
            content.len() as u64,
            cache_freshness::hash_bytes(content.as_bytes()),
        )
    }

    fn unhashed(content: &str) -> FileIdentity {
        (content.len() as u64, cache_freshness::zero_hash())
    }

    fn files(entries: &[(&str, FileIdentity)]) -> HashMap<String, FileIdentity> {
        entries
            .iter()
            .map(|(path, identity)| (path.to_string(), *identity))
            .collect()
    }

    /// A checkout and an owner directory holding the given files.
    fn trees(
        checkout_files: &[(&str, &str)],
        owner_files: &[(&str, &str)],
    ) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let owner = temp.path().join("owner");
        for (root, entries) in [(&checkout, checkout_files), (&owner, owner_files)] {
            fs::create_dir_all(root).unwrap();
            for (path, content) in entries {
                let path = root.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
        }
        (temp, checkout, owner)
    }

    #[test]
    fn identical_files_never_count_even_when_one_side_never_hashed_them() {
        let big = "x".repeat(64);
        let (_temp, checkout, owner) = trees(
            &[
                ("src/a.ts", "a"),
                ("src/big.ts", &big),
                ("src/huge.ts", "huge"),
            ],
            &[],
        );
        // The graph hashed big.ts but the search index held it unindexed
        // (too large for it), so its content is read from disk; huge.ts is
        // past the graph's hash cap, so only sizes are compared.
        let graph = files(&[
            ("src/a.ts", identity("a")),
            ("src/big.ts", identity(&big)),
            ("src/huge.ts", unhashed("huge")),
        ]);
        let here = files(&[
            ("src/a.ts", identity("a")),
            ("src/big.ts", unhashed(&big)),
            ("src/huge.ts", identity("huge")),
            // Files the graph's walk never includes: no supported source
            // type, a hidden path, the `.git` file that points a linked
            // worktree at its git directory, or an always-skipped directory.
            ("assets/blob.bin", unhashed("\0\x01")),
            ("notes.txt", identity("notes")),
            (".github/ci.ts", identity("ci")),
            (".git", identity("gitdir: elsewhere")),
            ("dist/out.ts", identity("built")),
        ]);
        assert_eq!(
            compare_with_graph(&graph, here, &checkout, Some(&owner), 8),
            Some(Vec::new())
        );
    }

    #[test]
    fn edited_missing_and_new_source_files_each_count_once() {
        let (_temp, checkout, owner) = trees(
            &[
                ("src/edited.ts", "new body"),
                ("src/unhashed.ts", "other!"),
                ("src/unindexed.ts", "on disk only"),
                ("src/added.ts", "added here"),
                ("src/rejected.ts", "same in both"),
            ],
            &[("src/rejected.ts", "same in both")],
        );
        let graph = files(&[
            ("src/edited.ts", identity("old body")),
            ("src/unhashed.ts", identity("origin")),
            ("src/unindexed.ts", identity("on disk only")),
            ("src/gone.ts", identity("deleted here")),
            ("src/gone_unhashed.ts", unhashed("too big")),
        ]);
        let here = files(&[
            ("src/edited.ts", identity("new body")),
            // Same size as the graph's copy, different bytes on disk.
            ("src/unhashed.ts", unhashed("other!")),
            ("src/added.ts", identity("added here")),
            // The graph lacks it, but the borrowed checkout holds the same
            // content: the graph leaves it out there just as it would here.
            ("src/rejected.ts", identity("same in both")),
        ]);
        // unindexed.ts is in the graph but has no record in this checkout's
        // index; its file on disk is hashed instead and matches the graph.
        assert_eq!(
            compare_with_graph(&graph, here, &checkout, Some(&owner), 8),
            Some(vec![
                "src/added.ts".to_string(),
                "src/edited.ts".to_string(),
                "src/gone.ts".to_string(),
                "src/gone_unhashed.ts".to_string(),
                "src/unhashed.ts".to_string(),
            ])
        );
    }

    #[test]
    fn a_new_file_counts_when_the_owner_is_unknown_and_reading_past_the_budget_is_unknown() {
        let (_temp, checkout, owner) = trees(
            &[("src/a.ts", "aa"), ("src/b.ts", "bb")],
            &[("src/a.ts", "aa")],
        );
        let here = files(&[("src/a.ts", identity("aa"))]);
        assert_eq!(
            compare_with_graph(&HashMap::new(), here, &checkout, None, 8),
            Some(vec!["src/a.ts".to_string()])
        );
        let graph = files(&[("src/a.ts", identity("aa")), ("src/b.ts", identity("bb"))]);
        let here = files(&[("src/a.ts", unhashed("aa")), ("src/b.ts", unhashed("bb"))]);
        assert_eq!(
            compare_with_graph(&graph, here.clone(), &checkout, Some(&owner), 2),
            Some(Vec::new())
        );
        assert_eq!(
            compare_with_graph(&graph, here, &checkout, Some(&owner), 1),
            None
        );
    }

    #[test]
    fn head_resolves_detached_loose_packed_and_linked_worktree_refs() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        let git = main.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();

        fs::write(git.join("HEAD"), format!("{A}\n")).unwrap();
        assert_eq!(checkout_head(&main).as_deref(), Some(A));

        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git.join("refs/heads/main"), format!("{B}\n")).unwrap();
        assert_eq!(checkout_head(&main).as_deref(), Some(B));

        fs::remove_file(git.join("refs/heads/main")).unwrap();
        fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled sorted\n{A} refs/heads/main\n^{B}\n"),
        )
        .unwrap();
        assert_eq!(checkout_head(&main).as_deref(), Some(A));

        let linked_git = git.join("worktrees/linked");
        fs::create_dir_all(&linked_git).unwrap();
        fs::write(linked_git.join("commondir"), "../..\n").unwrap();
        fs::write(linked_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let linked = temp.path().join("linked");
        fs::create_dir_all(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", linked_git.display()),
        )
        .unwrap();
        assert_eq!(checkout_head(&linked).as_deref(), Some(A));

        fs::write(linked_git.join("HEAD"), "ref: refs/heads/missing\n").unwrap();
        assert_eq!(checkout_head(&linked), None);
        assert_eq!(checkout_head(&temp.path().join("absent")), None);
    }
}
