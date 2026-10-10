//! A resumable directory walk: the budget applies to iterator advancement,
//! including invalid entries and session directories, not just deletions.
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, ReadDir};
use std::path::{Path, PathBuf};

use super::persistence::validate_task_id;

pub(crate) const GC_ENTRY_BUDGET: usize = 128;

pub(crate) struct Batch {
    pub session: PathBuf,
    pub ids: Vec<String>,
    pub invalid: Vec<OsString>,
    pub names: Vec<OsString>,
}

pub(crate) struct Cursor {
    sessions: Entries,
    tasks: Option<(PathBuf, Entries)>,
    pub finished: bool,
}

impl Cursor {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            sessions: Entries::open(root)?,
            tasks: None,
            finished: false,
        })
    }

    pub fn next_batch(&mut self) -> Vec<Batch> {
        let mut batches: Vec<Batch> = Vec::new();
        let mut ids = BTreeSet::new();
        for _ in 0..GC_ENTRY_BUDGET {
            if let Some((session, entries)) = &mut self.tasks {
                if batches.last().is_none_or(|batch| &batch.session != session) {
                    ids.clear();
                    batches.push(Batch {
                        session: session.clone(),
                        ids: Vec::new(),
                        invalid: Vec::new(),
                        names: Vec::new(),
                    });
                }
                let Some(entry) = entries.next() else {
                    self.tasks = None;
                    continue;
                };
                let Ok(entry) = entry else {
                    continue;
                };
                let name = entry.file_name();
                let batch = batches.last_mut().expect("session batch");
                batch.names.push(name.clone());
                let Some(text) = name.to_str() else {
                    batch.invalid.push(name);
                    continue;
                };
                let id = text.split_once('.').map_or(text, |(id, _)| id);
                if validate_task_id(id).is_ok() {
                    if ids.insert(id.to_string()) {
                        batch.ids.push(id.to_string());
                    }
                } else if text.starts_with("bash-") {
                    batch.invalid.push(name);
                }
            } else {
                let Some(entry) = self.sessions.next() else {
                    self.finished = true;
                    break;
                };
                let Ok(entry) = entry else {
                    continue;
                };
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                if let Ok(entries) = Entries::open(&entry.path()) {
                    self.tasks = Some((entry.path(), entries));
                }
            }
        }
        batches
    }
}

/// Count advancement at the actual iterator boundary, so eagerly collecting
/// names before applying a task budget also trips the work-count regression.
struct Entries(ReadDir);

impl Entries {
    fn open(path: &Path) -> std::io::Result<Self> {
        #[cfg(test)]
        super::persistence::work_counts::record_open();
        fs::read_dir(path).map(Self)
    }
}

impl Iterator for Entries {
    type Item = std::io::Result<fs::DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        #[cfg(test)]
        super::persistence::work_counts::record_gc_entry();
        self.0.next()
    }
}
