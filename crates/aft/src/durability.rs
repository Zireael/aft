//! Durable file commits use the platform's full sync, including F_FULLFSYNC on
//! macOS. Rebuildable and process-lifetime stores deliberately do not call these
//! helpers. The test ledger observes the real calls, never substitutes for them.

use std::fs::File;
use std::io;
use std::path::Path;

pub(crate) fn sync_file(file: &File, path: &Path) -> io::Result<()> {
    file.sync_all()?;
    record(EventKind::FileSync, path);
    Ok(())
}

pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
        record(EventKind::DirectorySync, path);
    }
    // Windows cannot open a directory as a regular File. Its rename uses
    // MoveFileExW write-through semantics; data files are synced before rename.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    FileSync,
    #[cfg_attr(not(unix), allow(dead_code))]
    DirectorySync,
    DirectoryCreated,
}

#[cfg(test)]
thread_local! {
    static EVENTS: std::cell::RefCell<Vec<(EventKind, std::path::PathBuf)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[inline]
pub(crate) fn record(kind: EventKind, path: &Path) {
    #[cfg(test)]
    EVENTS.with(|events| events.borrow_mut().push((kind, path.to_path_buf())));
    #[cfg(not(test))]
    let _ = (kind, path);
}

#[cfg(test)]
pub(crate) fn take() -> Vec<(EventKind, std::path::PathBuf)> {
    EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[cfg(test)]
pub(crate) fn sync_count(events: &[(EventKind, std::path::PathBuf)]) -> usize {
    events
        .iter()
        .filter(|(kind, _)| *kind != EventKind::DirectoryCreated)
        .count()
}
