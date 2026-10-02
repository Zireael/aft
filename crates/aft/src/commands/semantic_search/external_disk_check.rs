//! Checking another project's saved search index against its files on disk.
//!
//! `aft_search` with a `path` in another project answers from that project's
//! saved AFT index when one exists. Nothing in this session refreshes that
//! index, and opening it only parses the saved file, so a file edited, added
//! or deleted since the index was saved would otherwise be answered from what
//! it held then, or not at all.
//!
//! Before such an index answers a query, [`check_against_disk`] walks the
//! project once, checking a deadline at every walk entry, and compares each
//! file's size and modification time with the index's record of it:
//!
//! - unchanged files keep the saved postings and are not read;
//! - changed and new files are read from disk into an in-memory copy of the
//!   index (the saved file is never written), source files first, under a
//!   second deadline;
//! - files the index lists but the finished walk did not find are dropped.
//!
//! The returned [`DiskCheck`] says how many files were examined and whether the
//! walk and the re-reading finished, so the reply can say whether its answer
//! reflects every file on disk or only part of the project.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::external_exact::classify_file;
use crate::search_index::SearchIndex;
use crate::semantic_index::SemanticIndex;

/// Time allowed for walking the project and comparing file metadata.
const WALK_BUDGET: Duration = Duration::from_secs(2);
/// Time allowed, after the walk, for reading changed and new files.
const REREAD_BUDGET: Duration = Duration::from_secs(1);

thread_local! {
    static BUDGETS_FOR_TEST: Cell<Option<(Duration, Duration)>> = const { Cell::new(None) };
}

/// The walk and re-read budgets, overridable per thread by tests.
pub(super) fn budgets() -> (Duration, Duration) {
    BUDGETS_FOR_TEST
        .with(Cell::get)
        .unwrap_or((WALK_BUDGET, REREAD_BUDGET))
}

/// Run `run` with the walk and re-read budgets replaced on this thread.
#[cfg(test)]
pub(crate) fn with_budgets_for_test<R>(
    walk: Duration,
    reread: Duration,
    run: impl FnOnce() -> R,
) -> R {
    BUDGETS_FOR_TEST.with(|slot| {
        let previous = slot.replace(Some((walk, reread)));
        let result = run();
        slot.set(previous);
        result
    })
}

/// What comparing a saved index with the files on disk found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DiskCheck {
    /// Files the walk reached and compared with the index.
    pub files_examined: usize,
    /// The walk reached every file under the root before its deadline.
    pub walk_complete: bool,
    /// Indexed files whose size or modification time differs from the index.
    pub changed: usize,
    /// Files on disk the index has no record of.
    pub added: usize,
    /// Indexed files the finished walk did not find. Only counted when the
    /// walk completed: an unfinished walk cannot tell missing from unvisited.
    pub removed: usize,
    /// Changed or new files whose current content was read into the index.
    pub reread: usize,
    /// Changed or new files found but not read before the deadline; the saved
    /// index still answers for them (or, for new files, does not know them).
    pub not_reread: usize,
    /// Examined files the saved semantic index describes with older content,
    /// plus new files it has no vectors for. Zero when no semantic index was
    /// given.
    pub semantic_outdated: usize,
}

impl DiskCheck {
    /// True when every file on disk was compared and every difference was
    /// read, so the checked index answers exactly as the files are now.
    pub(super) fn verified(&self) -> bool {
        self.walk_complete && self.not_reread == 0
    }

    pub(super) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "complete": self.verified(),
            "files_examined": self.files_examined,
            "walk_complete": self.walk_complete,
            "changed": self.changed,
            "added": self.added,
            "removed": self.removed,
            "reread": self.reread,
            "not_reread": self.not_reread,
            "semantic_outdated": self.semantic_outdated,
        })
    }
}

/// A saved index brought in line with the disk as far as the budgets allowed.
pub(super) struct CheckedIndex {
    /// The saved index itself when nothing differed, otherwise an in-memory
    /// copy with the differences applied.
    pub index: Arc<SearchIndex>,
    pub check: DiskCheck,
    /// Stable digest of the differences that were applied, `None` when none
    /// were. Two queries over the same unchanged files get the same digest, so
    /// caches keyed by index generation stay usable without mixing versions.
    pub applied_digest: Option<String>,
}

/// Compare `index` with the files under `root` and apply what differs.
///
/// `semantic`, when given, is the project's saved semantic index; files it
/// describes with older content are counted in
/// [`DiskCheck::semantic_outdated`] (vectors cannot be recomputed here).
pub(super) fn check_against_disk(
    index: &Arc<SearchIndex>,
    root: &Path,
    semantic: Option<&SemanticIndex>,
) -> CheckedIndex {
    let (walk_budget, reread_budget) = budgets();
    let walk_deadline = Instant::now() + walk_budget;
    let stop_requested =
        |deadline: Instant| Instant::now() >= deadline || crate::executor::current_job_cancelled();

    let mut check = DiskCheck::default();
    let mut seen = vec![false; index.files.len()];
    let mut differing: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut walk_complete = true;
    // The index was built from this same walk, so a file the walk skips (an
    // ignored path, a build directory) is one the index never held either.
    for entry in crate::search_index::project_walk_builder(root).build() {
        if stop_requested(walk_deadline) {
            walk_complete = false;
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        check.files_examined += 1;
        let size = metadata.len();
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let path = entry.path();
        let added = match index.path_to_id.get(path) {
            Some(&file_id) => {
                if let Some(slot) = seen.get_mut(file_id as usize) {
                    *slot = true;
                }
                let unchanged = index
                    .files
                    .get(file_id as usize)
                    .is_some_and(|recorded| recorded.size == size && recorded.modified == modified);
                if !unchanged {
                    check.changed += 1;
                    differing.push((path.to_path_buf(), size, modified));
                }
                false
            }
            None => {
                check.added += 1;
                differing.push((path.to_path_buf(), size, modified));
                true
            }
        };
        if let Some(semantic) = semantic {
            let outdated = match semantic.recorded_stat(path) {
                Some((recorded_mtime, recorded_size)) => {
                    recorded_mtime != modified
                        || recorded_size.is_some_and(|recorded| recorded != size)
                }
                None => added,
            };
            if outdated {
                check.semantic_outdated += 1;
            }
        }
    }
    check.walk_complete = walk_complete;

    let removed: Vec<PathBuf> = if walk_complete {
        index
            .files
            .iter()
            .zip(&seen)
            .filter(|(entry, seen)| !**seen && !entry.path.as_os_str().is_empty())
            .map(|(entry, _)| entry.path.clone())
            .collect()
    } else {
        Vec::new()
    };
    check.removed = removed.len();

    if differing.is_empty() && removed.is_empty() {
        return CheckedIndex {
            index: Arc::clone(index),
            check,
            applied_digest: None,
        };
    }

    // Source first, so a re-read cut short by its deadline has covered code.
    differing.sort_by(|(left, ..), (right, ..)| {
        classify_file(left)
            .cmp(&classify_file(right))
            .then_with(|| left.cmp(right))
    });
    let mut overlay = SearchIndex::clone(index);
    let mut digest = blake3::Hasher::new();
    for path in &removed {
        overlay.remove_file(path);
        digest.update(b"-");
        digest.update(path.as_os_str().as_encoded_bytes());
    }
    let reread_deadline = Instant::now() + reread_budget;
    for (path, size, modified) in &differing {
        if stop_requested(reread_deadline) {
            break;
        }
        overlay.update_file(path);
        check.reread += 1;
        digest.update(b"+");
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update(&size.to_le_bytes());
        let since_epoch = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        digest.update(&since_epoch.as_nanos().to_le_bytes());
    }
    check.not_reread = differing.len() - check.reread;

    CheckedIndex {
        index: Arc::new(overlay),
        check,
        applied_digest: Some(digest.finalize().to_hex()[..16].to_string()),
    }
}

fn files(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    }
}

fn was_were(count: usize) -> &'static str {
    if count == 1 {
        "was"
    } else {
        "were"
    }
}

/// The sentence a reply ends with when `check` covered every file.
pub(super) fn verified_line(check: &DiskCheck, root: &Path) -> String {
    let mut differences = Vec::new();
    if check.changed > 0 {
        differences.push(format!("{} changed", files(check.changed)));
    }
    if check.added > 0 {
        differences.push(format!(
            "{} {} added",
            files(check.added),
            was_were(check.added)
        ));
    }
    if check.removed > 0 {
        differences.push(format!(
            "{} {} deleted",
            files(check.removed),
            was_were(check.removed)
        ));
    }
    let head = format!(
        "Checked the saved AFT index of {} against all {} on disk",
        root.display(),
        files(check.files_examined)
    );
    if differences.is_empty() {
        format!("{head}; none changed since it was saved.")
    } else {
        format!(
            "{head}; since it was saved {}, and the current files were searched.",
            differences.join(", ")
        )
    }
}

/// What `check` left unverified, for a reply that could not cover every file.
pub(super) fn unverified_detail(check: &DiskCheck) -> String {
    let read = if check.reread > 0 {
        format!(
            ", and {} that changed or {} added among them {} read from disk",
            files(check.reread),
            was_were(check.reread),
            was_were(check.reread)
        )
    } else {
        String::new()
    };
    if check.walk_complete {
        format!(
            "all {} on disk were compared with it{read}, but {} that changed or {} added could not be read before the time limit",
            files(check.files_examined),
            files(check.not_reread),
            was_were(check.not_reread)
        )
    } else {
        let unread = if check.not_reread > 0 {
            format!(
                "; {} more that changed or {} added could not be read in time",
                files(check.not_reread),
                was_were(check.not_reread)
            )
        } else {
            String::new()
        };
        format!(
            "only {} on disk were compared with it before the time limit{read}{unread}",
            files(check.files_examined)
        )
    }
}

/// The sentence added when the semantic lane answered from a saved semantic
/// index that predates some of the files.
pub(super) fn semantic_outdated_line(check: &DiskCheck, root: &Path) -> Option<String> {
    (check.semantic_outdated > 0).then(|| {
        format!(
            "The saved semantic index of {} predates {} that changed or {} added since: their ranking by meaning reflects older content or is missing, though their current text was searched.",
            root.display(),
            files(check.semantic_outdated),
            was_were(check.semantic_outdated)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("project dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical root");
        for (relative, content) in files {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
            std::fs::write(path, content).expect("write file");
        }
        (dir, root)
    }

    fn postings_find(index: &SearchIndex, root: &Path, needle: &str) -> Vec<PathBuf> {
        let compiled = match crate::pattern_compile::compile(
            needle,
            crate::pattern_compile::CompileOpts {
                literal: true,
                ..crate::pattern_compile::CompileOpts::default()
            },
        ) {
            crate::pattern_compile::CompileResult::Ok(compiled) => compiled,
            other => panic!("literal compile failed: {other:?}"),
        };
        let mut files: Vec<PathBuf> = index
            .snapshot()
            .search_grep_bounded(
                &compiled,
                &[],
                &[],
                root,
                100,
                None,
                1_000,
                Duration::from_secs(5),
            )
            .matches
            .into_iter()
            .map(|grep_match| grep_match.file)
            .collect();
        files.sort();
        files.dedup();
        files
    }

    fn backdate(path: &Path) {
        // Equal sizes with different mtimes must still count as a change, so
        // give the original file an mtime the rewrite cannot share.
        let old = filetime::FileTime::from_unix_time(1_000_000_000, 0);
        filetime::set_file_mtime(path, old).expect("backdate file");
    }

    #[test]
    fn unchanged_project_is_verified_and_keeps_the_saved_index() {
        let (_dir, root) = project(&[("src/lib.rs", "pub fn kept_needle() {}\n")]);
        let index = Arc::new(SearchIndex::build(&root));

        let checked = check_against_disk(&index, &root, None);

        assert!(checked.check.verified(), "{:?}", checked.check);
        assert_eq!(checked.check.files_examined, 1);
        assert_eq!(checked.check.changed + checked.check.added, 0);
        assert!(Arc::ptr_eq(&checked.index, &index));
        assert!(checked.applied_digest.is_none());
    }

    #[test]
    fn added_changed_and_deleted_files_are_answered_from_disk() {
        let (_dir, root) = project(&[
            ("src/edited.rs", "pub fn before_edit_needle() {}\n"),
            ("src/deleted.rs", "pub fn deleted_needle() {}\n"),
            ("src/same.rs", "pub fn same_needle() {}\n"),
        ]);
        backdate(&root.join("src/edited.rs"));
        let index = Arc::new(SearchIndex::build(&root));
        std::fs::write(
            root.join("src/edited.rs"),
            "pub fn after_edit_needle() {}\n",
        )
        .unwrap();
        std::fs::remove_file(root.join("src/deleted.rs")).unwrap();
        std::fs::write(root.join("src/added.rs"), "pub fn added_needle() {}\n").unwrap();
        assert!(postings_find(&index, &root, "added_needle").is_empty());

        let checked = check_against_disk(&index, &root, None);

        assert_eq!(
            checked.check,
            DiskCheck {
                files_examined: 3,
                walk_complete: true,
                changed: 1,
                added: 1,
                removed: 1,
                reread: 2,
                not_reread: 0,
                semantic_outdated: 0,
            }
        );
        assert!(checked.check.verified());
        assert_eq!(
            postings_find(&checked.index, &root, "added_needle"),
            vec![root.join("src/added.rs")]
        );
        assert_eq!(
            postings_find(&checked.index, &root, "after_edit_needle"),
            vec![root.join("src/edited.rs")]
        );
        assert!(postings_find(&checked.index, &root, "deleted_needle").is_empty());
        assert_eq!(
            postings_find(&checked.index, &root, "same_needle"),
            vec![root.join("src/same.rs")]
        );
        // The saved index itself is left exactly as it was loaded.
        assert!(postings_find(&index, &root, "added_needle").is_empty());
        let again = check_against_disk(&index, &root, None);
        assert_eq!(again.applied_digest, checked.applied_digest);
    }

    #[test]
    fn a_walk_stopped_by_its_deadline_is_not_verified_and_drops_nothing() {
        let (_dir, root) = project(&[
            ("src/a.rs", "pub fn a_needle() {}\n"),
            ("src/b.rs", "pub fn b_needle() {}\n"),
        ]);
        let index = Arc::new(SearchIndex::build(&root));
        std::fs::write(root.join("src/c.rs"), "pub fn c_needle() {}\n").unwrap();

        let checked = with_budgets_for_test(Duration::ZERO, Duration::from_secs(5), || {
            check_against_disk(&index, &root, None)
        });

        assert!(!checked.check.walk_complete);
        assert!(!checked.check.verified());
        assert_eq!(checked.check.files_examined, 0);
        assert_eq!(checked.check.removed, 0, "unvisited files are not deleted");
        assert!(Arc::ptr_eq(&checked.index, &index));
        assert!(unverified_detail(&checked.check).starts_with("only 0 files on disk were compared"));
    }

    #[test]
    fn differences_left_unread_by_the_deadline_are_not_verified() {
        let (_dir, root) = project(&[("src/a.rs", "pub fn a_needle() {}\n")]);
        let index = Arc::new(SearchIndex::build(&root));
        std::fs::write(root.join("src/new.rs"), "pub fn new_needle() {}\n").unwrap();

        let checked = with_budgets_for_test(Duration::from_secs(5), Duration::ZERO, || {
            check_against_disk(&index, &root, None)
        });

        assert!(checked.check.walk_complete);
        assert_eq!(checked.check.added, 1);
        assert_eq!(checked.check.not_reread, 1);
        assert!(!checked.check.verified());
        assert!(postings_find(&checked.index, &root, "new_needle").is_empty());
        assert!(
            unverified_detail(&checked.check).ends_with(
                "but 1 file that changed or was added could not be read before the time limit"
            ),
            "{}",
            unverified_detail(&checked.check)
        );
    }

    #[test]
    fn verified_line_names_each_kind_of_difference() {
        let root = Path::new("/p");
        let mut check = DiskCheck {
            files_examined: 12,
            walk_complete: true,
            ..DiskCheck::default()
        };
        assert_eq!(
            verified_line(&check, root),
            "Checked the saved AFT index of /p against all 12 files on disk; none changed since it was saved."
        );
        check.changed = 2;
        check.added = 1;
        check.removed = 3;
        assert_eq!(
            verified_line(&check, root),
            "Checked the saved AFT index of /p against all 12 files on disk; since it was saved 2 files changed, 1 file was added, 3 files were deleted, and the current files were searched."
        );
    }
}
