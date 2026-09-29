//! Synchronous write intent for opt-in checkout views.
//!
//! An intent is deliberately not cleared when a command fails: formatting,
//! partial writes and rollback can all change bytes before the error returns.
//! Only a strict reconciliation under the `Mutex<LiveDelta>` may resolve it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::snapshot::LiveDelta;
use super::RelPath;

#[derive(Clone)]
struct Registration {
    root: PathBuf,
    delta: Weak<Mutex<LiveDelta>>,
    active: Arc<std::sync::atomic::AtomicUsize>,
}

fn registrations() -> &'static Mutex<Vec<Registration>> {
    static ROOTS: OnceLock<Mutex<Vec<Registration>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register only roots routed to the new view plane. Legacy command behavior
/// and indexing remain unchanged when no view is registered.
pub fn register(root: &Path, delta: &Arc<Mutex<LiveDelta>>) {
    let mut roots = registrations().lock().unwrap_or_else(|e| e.into_inner());
    roots.retain(|entry| entry.delta.strong_count() != 0);
    roots.push(Registration {
        root: root.to_path_buf(),
        delta: Arc::downgrade(delta),
        active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
}

/// Called before the first possible filesystem mutation, not after success.
/// Directory intents invalidate the whole watcher epoch: their descendants
/// (including newly created names) cannot safely be enumerated from an index.
pub fn record_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> IntentGuard {
    let paths = paths.into_iter().collect::<Vec<_>>();
    let mut roots = registrations().lock().unwrap_or_else(|e| e.into_inner());
    roots.retain(|entry| entry.delta.strong_count() != 0);
    let entries = roots.clone();
    drop(roots);
    let mut guard = IntentGuard {
        entries: Vec::new(),
    };
    for entry in entries {
        let Some(delta) = entry.delta.upgrade() else {
            continue;
        };
        let relevant = paths
            .iter()
            .filter(|path| path.starts_with(&entry.root))
            .map(|path| path.to_path_buf())
            .collect::<Vec<_>>();
        if relevant.is_empty() {
            continue;
        }
        entry
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        guard.entries.push((entry.clone(), relevant));
        let mut delta = delta.lock().unwrap_or_else(|e| e.into_inner());
        for path in &paths {
            let Ok(relative) = path.strip_prefix(&entry.root) else {
                continue;
            };
            if relative.as_os_str().is_empty() || path.is_dir() {
                delta.bump_epoch();
                delta.set_watcher(super::snapshot::WatcherState::Reconciling);
            } else if let Ok(relative) = RelPath::from_os_path(relative) {
                delta.record_intent(relative);
            }
        }
    }
    guard
}

/// Keeps concurrent reconciliation from resolving pending intent or marking
/// the watcher healthy while a filesystem mutation is still running.
#[must_use]
pub struct IntentGuard {
    entries: Vec<(Registration, Vec<PathBuf>)>,
}

pub fn active(root: &Path) -> bool {
    registrations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|entry| {
            entry.root == root && entry.active.load(std::sync::atomic::Ordering::SeqCst) != 0
        })
}

impl Drop for IntentGuard {
    fn drop(&mut self) {
        for (entry, paths) in &self.entries {
            if let Some(delta) = entry.delta.upgrade() {
                let mut delta = delta.lock().unwrap_or_else(|e| e.into_inner());
                for path in paths {
                    if let Ok(relative) = path.strip_prefix(&entry.root) {
                        if let Ok(relative) = RelPath::from_os_path(relative) {
                            delta.record_intent(relative);
                        }
                    }
                }
                // A command may have removed or created an entire directory.
                // Mark current membership for a later rewalk rather than
                // trusting the directory entries recorded before the command.
                delta.bump_epoch();
                delta.set_watcher(super::snapshot::WatcherState::Reconciling);
            }
            entry
                .active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
