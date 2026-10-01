//! Disk limits for one repository family's per-checkout views, and
//! least-recently-used eviction of whole views to stay under them.
//!
//! Two resource domains are accounted separately and never added together:
//!
//! - the **view store**: every member's own view directory (pointer,
//!   manifests, derived callgraph databases, trigram overlays, pins and
//!   markers). Its limits are 4 GiB soft and 6 GiB hard per family. Above
//!   the soft limit, views without a live session are evicted, least
//!   recently bound first, until the store is back under it.
//! - the **family store**: the shared plane databases, trigram segments and
//!   the registry under `blobs/v2/<family>/`. Each key and segment lives
//!   there once however many views use it, so it is counted once. Its limits
//!   are 3 GiB soft and 4 GiB hard; above the soft limit the family sweep
//!   collects every unreferenced row and returns the space to the filesystem.
//!
//! Nothing that is in use is deleted to meet a number. A view is never
//! evicted while a process holds a session on it, while any pin or protected
//! read or residency marker exists in it, while this process has one of its
//! SQLite files open, or while any of that cannot be read with certainty.
//! When what remains over a hard limit is all held that way, new work is not
//! admitted; the refusal names the domain and the held bytes.
//!
//! Evicting a view removes its member row and its view directory. Its next
//! registration starts it as a new view: the first load seeds from a sibling
//! or walks the checkout, exactly as for a checkout that was never bound.
//!
//! Every walk here is bounded at its iterator and checks the deadline inside
//! its loop; a walk cut short reports `complete: false` rather than a number
//! that looks exact.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::params;

use crate::blob_store::v2::{family_dir, FamilyPlane, FamilyStoreReader};
use crate::gc::family::{self, FamilySweepPolicy, FamilySweepReport, SweepBounds};

use super::registry::{
    has_live_session, remove_view_dir, FamilyRegistry, MemberRecord, RegistryError, RegistryResult,
    STATE_EVICTING,
};

const GIB: u64 = 1024 * 1024 * 1024;

/// View-store limits per repository family, as decided for this design: a
/// soft limit that starts eviction and a hard limit that stops admission.
pub const VIEW_STORE_SOFT_BYTES: u64 = 4 * GIB;
pub const VIEW_STORE_HARD_BYTES: u64 = 6 * GIB;
/// Family-store limits on physical file bytes (plane databases with their
/// write-ahead logs, segments and the registry).
pub const FAMILY_STORE_SOFT_BYTES: u64 = 3 * GIB;
pub const FAMILY_STORE_HARD_BYTES: u64 = 4 * GIB;
/// Payload bytes the family sweep keeps unreferenced rows under while the
/// family store is below its soft limit. Unreferenced rows are often the
/// content of a branch a checkout will switch back to, so they are kept
/// while there is room.
pub const FAMILY_PAYLOAD_BUDGET_BYTES: u64 = 2 * GIB;

/// How often the runtime checks a bound family's limits.
pub const ENFORCEMENT_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How long one background enforcement pass may spend before it stops.
pub const ENFORCEMENT_DEADLINE: Duration = Duration::from_secs(60);

/// Entries one measurement walk reads in a view or family directory.
const MAX_WALK_ENTRIES: usize = 200_000;
/// How deep a view directory walk goes: the view directory itself, then
/// `pins/` and `readers/`, then the generation directories under `readers/`.
const MAX_WALK_DEPTH: usize = 3;

/// The limits applied to one family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskBudget {
    pub view_store_soft: u64,
    pub view_store_hard: u64,
    pub family_store_soft: u64,
    pub family_store_hard: u64,
    pub family_payload: u64,
}

impl Default for DiskBudget {
    fn default() -> Self {
        Self {
            view_store_soft: VIEW_STORE_SOFT_BYTES,
            view_store_hard: VIEW_STORE_HARD_BYTES,
            family_store_soft: FAMILY_STORE_SOFT_BYTES,
            family_store_hard: FAMILY_STORE_HARD_BYTES,
            family_payload: FAMILY_PAYLOAD_BUDGET_BYTES,
        }
    }
}

/// Why a view cannot be evicted now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hold {
    /// A live process holds a session on the view.
    ActiveSession,
    /// A pin with a live owner, or a protected read or residency marker,
    /// protects one of its generations.
    Protected,
    /// This process has one of the view's SQLite files open.
    OpenInProcess,
    /// Its sessions, protection or files could not be read with certainty.
    Uncertain(String),
}

impl fmt::Display for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveSession => write!(f, "active session"),
            Self::Protected => write!(f, "protected generation"),
            Self::OpenInProcess => write!(f, "open in this process"),
            Self::Uncertain(reason) => write!(f, "uncertain: {reason}"),
        }
    }
}

/// One member's view directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewUsage {
    pub scope: String,
    pub view_dir: PathBuf,
    /// File bytes under the view directory, each file once.
    pub bytes: u64,
    /// The member's most recent bind, or the end of its most recent session.
    pub last_bind_ms: u64,
    /// `None` when the view may be evicted.
    pub hold: Option<Hold>,
    /// An earlier eviction of this view began and did not finish.
    pub evicting: bool,
    /// False when the walk stopped at a bound or the deadline.
    pub complete: bool,
}

/// The family store's files, each counted once.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FamilyStoreUsage {
    /// Bytes of every file under `blobs/v2/<family>/`: plane databases with
    /// their write-ahead logs, segment files and the registry.
    pub file_bytes: u64,
    /// `SUM(length(payload))` over every plane, as SQLite reports it.
    pub payload_bytes: u64,
    pub complete: bool,
}

/// Disk use of one family, per domain.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiskUsage {
    pub views: Vec<ViewUsage>,
    /// Sum of every member's view directory bytes.
    pub view_store_bytes: u64,
    /// The part of `view_store_bytes` in views that cannot be evicted now.
    pub held_view_bytes: u64,
    pub family_store: FamilyStoreUsage,
    pub complete: bool,
}

impl DiskUsage {
    fn largest_view(&self) -> u64 {
        self.views.iter().map(|view| view.bytes).max().unwrap_or(0)
    }
}

/// How the family store's keys divide among the views that rely on them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeyAccounting {
    /// Bytes of keys and segments that at least one view relies on, each
    /// counted once however many views rely on it.
    pub referenced_bytes: u64,
    /// The part of `referenced_bytes` that two or more views rely on.
    pub shared_bytes: u64,
    /// Per view, the bytes of keys no other view relies on: what a sweep
    /// could reclaim after that view alone is evicted.
    pub exclusive_bytes: BTreeMap<String, u64>,
    /// Bytes of stored keys no view relies on, which a sweep may collect.
    pub unreferenced_bytes: u64,
    pub complete: bool,
}

#[derive(Debug)]
pub enum EvictionError {
    Registry(RegistryError),
    Io(std::io::Error),
}

impl fmt::Display for EvictionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "view eviction I/O error: {error}"),
        }
    }
}

impl std::error::Error for EvictionError {}

impl From<RegistryError> for EvictionError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<std::io::Error> for EvictionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::blob_store::v2::StoreError> for EvictionError {
    fn from(error: crate::blob_store::v2::StoreError) -> Self {
        Self::Registry(RegistryError::Store(error))
    }
}

/// Points in an eviction where tests can pause or kill the process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvictionStep {
    /// The member is marked `evicting` and nothing is deleted yet.
    Begun,
    /// The pointer is deleted; the rest of the view directory is not, and
    /// the transaction that removes the member has not committed.
    PointerRemoved,
}

pub trait EvictionObserver {
    fn reached(&self, step: EvictionStep);
}

/// What happened to one eviction candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvictionOutcome {
    Evicted {
        bytes: u64,
    },
    Held(Hold),
    /// The member was no longer registered.
    Gone,
    /// The member row is gone, or still `evicting`, but some files could not
    /// be removed (an open file on Windows); the next pass retries.
    Incomplete(String),
}

/// Options of one enforcement pass.
#[derive(Clone, Copy)]
pub struct EnforceOptions<'a> {
    pub deadline: Option<Instant>,
    pub observer: Option<&'a dyn EvictionObserver>,
}

impl Default for EnforceOptions<'_> {
    fn default() -> Self {
        Self {
            deadline: None,
            observer: None,
        }
    }
}

/// What one enforcement pass did.
#[derive(Clone, Debug, Default)]
pub struct EnforcementReport {
    pub before: DiskUsage,
    pub after: DiskUsage,
    /// Evicted views with the view-store bytes each one freed, in order.
    pub evicted: Vec<(String, u64)>,
    /// Candidates the pass looked at and kept, with the reason.
    pub held: Vec<(String, Hold)>,
    pub sweep: Option<FamilySweepReport>,
    /// Why the family sweep did not run or deleted nothing.
    pub sweep_error: Option<String>,
    /// The named reasons new work would be queued after this pass, if any.
    pub over_hard: Vec<String>,
    /// True when the pass stopped at its deadline.
    pub stopped_early: bool,
}

fn walk_bounds(deadline: Option<Instant>) -> SweepBounds {
    SweepBounds {
        deadline,
        ..SweepBounds::default()
    }
}

fn past(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Total file bytes under `dir`, each file once, up to `MAX_WALK_DEPTH`
/// levels. Symlinks are not followed and count nothing.
fn tree_bytes(dir: &Path, deadline: Option<Instant>) -> std::io::Result<(u64, bool)> {
    let mut bytes = 0u64;
    let mut read = 0usize;
    let mut complete = true;
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = pending.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let remaining = MAX_WALK_ENTRIES.saturating_sub(read);
        for entry in entries.take(remaining.saturating_add(1)) {
            if read >= MAX_WALK_ENTRIES || past(deadline) {
                complete = false;
                break;
            }
            read += 1;
            let entry = match entry {
                Ok(entry) => entry,
                // Removed while listed: nothing to count.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                if depth + 1 < MAX_WALK_DEPTH {
                    pending.push((entry.path(), depth + 1));
                } else {
                    complete = false;
                }
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
        if !complete {
            break;
        }
    }
    Ok((bytes, complete))
}

/// The SQLite files of a view directory (main files only; their `-wal` and
/// `-shm` belong to the same open connections).
fn sqlite_files(view_dir: &Path, deadline: Option<Instant>) -> std::io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(view_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries.take(MAX_WALK_ENTRIES.saturating_add(1)) {
        if files.len() >= MAX_WALK_ENTRIES || past(deadline) {
            return Err(std::io::Error::other(
                "view directory listing stopped at its bound",
            ));
        }
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("sqlite") {
            files.push(path);
        }
    }
    Ok(files)
}

/// True when this process has any of the view's SQLite files open. Deleting
/// a file set under a live connection detaches it from its locks.
fn open_in_process(view_dir: &Path, deadline: Option<Instant>) -> Result<bool, String> {
    Ok(sqlite_files(view_dir, deadline)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|path| crate::db::file_identity::open_connections(path) != 0))
}

/// Why `member` cannot be evicted, read inside the registry barrier `tx` so
/// that no registration can land between this check and the deletion.
fn hold_inside_barrier(
    tx: &rusqlite::Connection,
    member: &MemberRecord,
    deadline: Option<Instant>,
) -> RegistryResult<Option<Hold>> {
    if has_live_session(tx, &member.scope)? {
        return Ok(Some(Hold::ActiveSession));
    }
    match family::view_has_protection(&member.view_dir) {
        Ok(false) => {}
        Ok(true) => return Ok(Some(Hold::Protected)),
        Err(reason) => return Ok(Some(Hold::Uncertain(reason))),
    }
    match open_in_process(&member.view_dir, deadline) {
        Ok(false) => Ok(None),
        Ok(true) => Ok(Some(Hold::OpenInProcess)),
        Err(reason) => Ok(Some(Hold::Uncertain(reason))),
    }
}

fn hold_of(
    registry: &FamilyRegistry,
    member: &MemberRecord,
    deadline: Option<Instant>,
) -> Option<Hold> {
    match registry.with_barrier(|tx| hold_inside_barrier(tx, member, deadline)) {
        Ok(hold) => hold,
        Err(error) => Some(Hold::Uncertain(error.to_string())),
    }
}

/// Measures both domains of a family. Nothing is changed on disk.
pub fn measure(registry: &FamilyRegistry, deadline: Option<Instant>) -> RegistryResult<DiskUsage> {
    let mut usage = DiskUsage {
        complete: true,
        ..DiskUsage::default()
    };
    for member in registry.members()? {
        let (bytes, complete) = match tree_bytes(&member.view_dir, deadline) {
            Ok(measured) => measured,
            Err(_) => (0, false),
        };
        let hold = hold_of(registry, &member, deadline);
        usage.complete &= complete;
        usage.view_store_bytes = usage.view_store_bytes.saturating_add(bytes);
        if hold.is_some() {
            usage.held_view_bytes = usage.held_view_bytes.saturating_add(bytes);
        }
        usage.views.push(ViewUsage {
            scope: member.scope.clone(),
            view_dir: member.view_dir.clone(),
            bytes,
            last_bind_ms: member.last_bind_ms,
            hold,
            evicting: member.evicting,
            complete,
        });
    }
    usage.family_store = measure_family_store(registry, deadline)?;
    usage.complete &= usage.family_store.complete;
    Ok(usage)
}

fn measure_family_store(
    registry: &FamilyRegistry,
    deadline: Option<Instant>,
) -> RegistryResult<FamilyStoreUsage> {
    let dir = family_dir(registry.storage(), registry.family())?;
    let (file_bytes, complete) = tree_bytes(&dir, deadline)?;
    let mut payload_bytes = 0u64;
    for plane in FamilyPlane::ALL {
        if let Some(store) =
            FamilyStoreReader::open_existing(registry.storage(), registry.family(), plane)?
        {
            payload_bytes = payload_bytes.saturating_add(store.usage()?.payload_bytes);
        }
    }
    Ok(FamilyStoreUsage {
        file_bytes,
        payload_bytes,
        complete,
    })
}

/// Divides the family store's keys among the views that rely on them. A key
/// several views rely on is counted once, in `shared_bytes`, and in no
/// view's exclusive share.
pub fn account_keys(
    registry: &FamilyRegistry,
    deadline: Option<Instant>,
) -> RegistryResult<KeyAccounting> {
    let bounds = walk_bounds(deadline);
    let mut accounting = KeyAccounting {
        complete: true,
        ..KeyAccounting::default()
    };
    let mut holders: BTreeMap<[u8; 32], BTreeSet<String>> = BTreeMap::new();
    for member in registry.members()? {
        accounting.exclusive_bytes.insert(member.scope.clone(), 0);
        match family::member_references(&member, registry.family(), &bounds) {
            Ok(keys) => {
                for key in keys {
                    holders.entry(key).or_default().insert(member.scope.clone());
                }
            }
            Err(_) => accounting.complete = false,
        }
    }
    let mut sizes: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    let access = crate::blob_store::v2::StoreWriteAccess::for_registered_view(
        registry.storage(),
        registry.family(),
    );
    for plane in FamilyPlane::ALL {
        let Some(reader) =
            FamilyStoreReader::open_existing(registry.storage(), registry.family(), plane)?
        else {
            continue;
        };
        drop(reader);
        // `FamilyStore` handles share one SQLite connection per store file in
        // this process, so listing through one never opens a second
        // descriptor on the live file set.
        let store = crate::blob_store::v2::FamilyStore::open(&access, plane)?;
        let rows = store.rows_oldest(bounds.max_rows_per_store)?;
        if rows.len() >= bounds.max_rows_per_store {
            accounting.complete = false;
        }
        for row in rows {
            if past(deadline) {
                accounting.complete = false;
                break;
            }
            sizes.insert(*row.key.as_bytes(), row.payload_bytes);
        }
        for segment in store
            .segments()?
            .into_iter()
            .take(bounds.max_rows_per_store)
        {
            sizes.insert(segment.segment_id, segment.byte_len);
        }
    }
    for (key, bytes) in sizes {
        match holders.get(&key) {
            None => accounting.unreferenced_bytes += bytes,
            Some(scopes) => {
                accounting.referenced_bytes += bytes;
                if scopes.len() == 1 {
                    let scope = scopes.iter().next().expect("one holder");
                    *accounting.exclusive_bytes.entry(scope.clone()).or_default() += bytes;
                } else {
                    accounting.shared_bytes += bytes;
                }
            }
        }
    }
    Ok(accounting)
}

/// Evicts one member: its view directory and its member row. Runs in two
/// steps under the registry barrier. The first marks the member `evicting`
/// and commits, so an interrupted eviction is visible; the second checks
/// every hold again, deletes the pointer first (so no half-deleted
/// generation is ever published), then the rest of the directory, then the
/// member row.
pub fn evict_view(
    registry: &FamilyRegistry,
    scope: &str,
    deadline: Option<Instant>,
    observer: Option<&dyn EvictionObserver>,
) -> RegistryResult<EvictionOutcome> {
    let begun = registry.with_barrier(|tx| {
        let Some(member) = member_in(tx, scope)? else {
            return Ok(Err(EvictionOutcome::Gone));
        };
        if let Some(hold) = hold_inside_barrier(tx, &member, deadline)? {
            return Ok(Err(EvictionOutcome::Held(hold)));
        }
        tx.execute(
            "UPDATE members SET state = ?2 WHERE scope = ?1",
            params![scope, STATE_EVICTING],
        )?;
        Ok(Ok(member))
    })?;
    let member = match begun {
        Ok(member) => member,
        Err(outcome) => return Ok(outcome),
    };
    if let Some(observer) = observer {
        observer.reached(EvictionStep::Begun);
    }
    let (bytes, _) = tree_bytes(&member.view_dir, deadline).unwrap_or((0, false));
    registry.with_barrier(|tx| {
        let Some(current) = member_in(tx, scope)? else {
            return Ok(EvictionOutcome::Gone);
        };
        // A registration after the member was marked `evicting` set it back
        // to `active`: that view is in use again, so this eviction stops.
        if !current.evicting {
            return Ok(EvictionOutcome::Held(Hold::ActiveSession));
        }
        if let Some(hold) = hold_inside_barrier(tx, &current, deadline)? {
            tx.execute(
                "UPDATE members SET state = 'active' WHERE scope = ?1",
                params![scope],
            )?;
            return Ok(EvictionOutcome::Held(hold));
        }
        // No connection may open a file of this set between the check above
        // and the deletion below.
        let _files = crate::db::file_identity::filesystem_guard();
        if open_in_process(&current.view_dir, deadline).unwrap_or(true) {
            tx.execute(
                "UPDATE members SET state = 'active' WHERE scope = ?1",
                params![scope],
            )?;
            return Ok(EvictionOutcome::Held(Hold::OpenInProcess));
        }
        if let Err(error) = remove_pointer(&current.view_dir) {
            tx.execute(
                "UPDATE members SET state = 'active' WHERE scope = ?1",
                params![scope],
            )?;
            return Ok(EvictionOutcome::Held(Hold::Uncertain(format!(
                "pointer not removable: {error}"
            ))));
        }
        if let Some(observer) = observer {
            observer.reached(EvictionStep::PointerRemoved);
        }
        if let Err(error) = remove_view_dir(&current.view_dir) {
            // The pointer is gone, so the view publishes nothing; leave the
            // member `evicting` for the next pass or registration to finish.
            return Ok(EvictionOutcome::Incomplete(error.to_string()));
        }
        tx.execute("DELETE FROM members WHERE scope = ?1", params![scope])?;
        // Dead sessions of the evicted view, if the table exists at all.
        let _ = tx.execute("DELETE FROM view_sessions WHERE scope = ?1", params![scope]);
        Ok(EvictionOutcome::Evicted { bytes })
    })
}

fn member_in(tx: &rusqlite::Connection, scope: &str) -> RegistryResult<Option<MemberRecord>> {
    Ok(super::registry::read_members(tx)?
        .into_iter()
        .find(|member| member.scope == scope))
}

/// Deletes the pointer database set of a view directory, main file first.
fn remove_pointer(view_dir: &Path) -> std::io::Result<()> {
    let pointer = view_dir.join(super::POINTER_DATABASE);
    for suffix in ["", "-wal", "-shm"] {
        let mut name = pointer.as_os_str().to_owned();
        name.push(suffix);
        match fs::remove_file(PathBuf::from(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// One enforcement pass: finish interrupted evictions, evict views without a
/// hold (least recently bound first) while the view store is over its soft
/// limit, then sweep the family store so that keys only the evicted views
/// relied on are reclaimed and the freed space returns to the filesystem.
pub fn enforce(
    registry: &FamilyRegistry,
    options: EnforceOptions<'_>,
) -> Result<EnforcementReport, EvictionError> {
    let budget = registry.disk_budget();
    let deadline = options.deadline;
    let _ = registry.reclaim_dead_sessions();
    let before = measure(registry, deadline)?;
    let mut report = EnforcementReport {
        before: before.clone(),
        ..EnforcementReport::default()
    };
    let mut total = before.view_store_bytes;
    let mut candidates = before
        .views
        .iter()
        .filter(|view| view.hold.is_none() || view.evicting)
        .collect::<Vec<_>>();
    // Interrupted evictions first, then least recently bound first; the
    // scope breaks ties so the order never depends on listing order.
    candidates.sort_by(|a, b| {
        (!a.evicting, a.last_bind_ms, &a.scope).cmp(&(!b.evicting, b.last_bind_ms, &b.scope))
    });
    for view in candidates {
        if !view.evicting && total <= budget.view_store_soft {
            break;
        }
        if past(deadline) {
            report.stopped_early = true;
            break;
        }
        match evict_view(registry, &view.scope, deadline, options.observer)? {
            EvictionOutcome::Evicted { bytes } => {
                total = total.saturating_sub(bytes);
                report.evicted.push((view.scope.clone(), bytes));
            }
            EvictionOutcome::Held(hold) => report.held.push((view.scope.clone(), hold)),
            EvictionOutcome::Gone => {}
            EvictionOutcome::Incomplete(reason) => report
                .held
                .push((view.scope.clone(), Hold::Uncertain(reason))),
        }
    }
    for view in &before.views {
        if let Some(hold) = &view.hold {
            if !view.evicting {
                report.held.push((view.scope.clone(), hold.clone()));
            }
        }
    }

    let collect_all =
        !report.evicted.is_empty() || before.family_store.file_bytes > budget.family_store_soft;
    let policy = FamilySweepPolicy {
        byte_budget: if collect_all {
            0
        } else {
            budget.family_payload
        },
    };
    if !past(deadline) {
        match family::sweep_family_bounded(registry, None, policy, walk_bounds(deadline), None) {
            Ok(sweep) => {
                report.stopped_early |= sweep.stopped_early;
                report.sweep = Some(sweep);
            }
            Err(error) => report.sweep_error = Some(error.to_string()),
        }
    } else {
        report.stopped_early = true;
    }

    report.after = measure(registry, deadline)?;
    report.over_hard = over_hard(&report.after, budget, 0);
    Ok(report)
}

/// The named reasons new work that adds `reserve` view-store bytes would be
/// queued under `budget`, given `usage`.
fn over_hard(usage: &DiskUsage, budget: DiskBudget, reserve: u64) -> Vec<String> {
    let mut reasons = Vec::new();
    if usage.view_store_bytes.saturating_add(reserve) > budget.view_store_hard {
        reasons.push(format!(
            "queued: view store disk: {} of {} bytes are held by active sessions or protected \
             generations, hard limit {} bytes{}",
            usage.held_view_bytes,
            usage.view_store_bytes,
            budget.view_store_hard,
            if reserve > 0 {
                format!(", a new view reserves {reserve} bytes")
            } else {
                String::new()
            }
        ));
    }
    if usage.family_store.file_bytes > budget.family_store_hard {
        reasons.push(format!(
            "queued: family store disk: {} bytes in use after collecting unreferenced keys, \
             hard limit {} bytes",
            usage.family_store.file_bytes, budget.family_store_hard
        ));
    }
    reasons
}

/// Admission of a new view under the family's hard limits. A new view's
/// first generation will add about as many bytes as the largest view the
/// family has, so that much is reserved. When the reserve does not fit, one
/// enforcement pass runs first; when it still does not fit because what
/// remains is held, the view is refused with the named reason and the caller
/// retries later. Views already registered are never refused.
pub fn admit_new_view(registry: &FamilyRegistry) -> RegistryResult<()> {
    let budget = registry.disk_budget();
    let usage = measure(registry, None)?;
    let reserve = usage.largest_view();
    if over_hard(&usage, budget, reserve).is_empty() {
        return Ok(());
    }
    let report = enforce(registry, EnforceOptions::default()).map_err(|error| match error {
        EvictionError::Registry(error) => error,
        EvictionError::Io(error) => RegistryError::Io(error),
    })?;
    let reasons = over_hard(&report.after, budget, report.after.largest_view());
    if reasons.is_empty() {
        return Ok(());
    }
    crate::slog_warn!(
        "view admission refused family={} {}",
        registry.family(),
        reasons.join("; ")
    );
    Err(RegistryError::AdmissionQueued(reasons.join("; ")))
}

fn last_runs() -> &'static Mutex<BTreeMap<PathBuf, Instant>> {
    static RUNS: OnceLock<Mutex<BTreeMap<PathBuf, Instant>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn in_flight() -> &'static Mutex<BTreeSet<PathBuf>> {
    static RUNNING: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// True when this process has not checked `registry`'s limits within
/// [`ENFORCEMENT_INTERVAL`] and no check is running.
pub fn enforcement_due(registry: &FamilyRegistry) -> bool {
    let key = registry.path().to_path_buf();
    if in_flight()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&key)
    {
        return false;
    }
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .is_none_or(|last| last.elapsed() >= ENFORCEMENT_INTERVAL)
}

/// Starts one background enforcement pass for `registry` when one is due.
/// Returns whether a pass was started.
pub fn spawn_enforcement_if_due(registry: FamilyRegistry) -> bool {
    if !enforcement_due(&registry) {
        return false;
    }
    let key = registry.path().to_path_buf();
    if !in_flight()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key.clone())
    {
        return false;
    }
    last_runs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key.clone(), Instant::now());
    let spawned = std::thread::Builder::new()
        .name("aft-view-disk-limits".to_string())
        .spawn({
            let key = key.clone();
            move || {
                let options = EnforceOptions {
                    deadline: Some(Instant::now() + ENFORCEMENT_DEADLINE),
                    observer: None,
                };
                match enforce(&registry, options) {
                    Ok(report) => log_report(&registry, &report),
                    Err(error) => crate::slog_warn!(
                        "view disk limits check failed family={} error={}",
                        registry.family(),
                        error
                    ),
                }
                in_flight()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&key);
            }
        });
    if let Err(error) = spawned {
        in_flight()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        crate::slog_warn!("view disk limits thread unavailable: {error}");
        return false;
    }
    true
}

fn log_report(registry: &FamilyRegistry, report: &EnforcementReport) {
    crate::slog_info!(
        "view disk limits family={} view_store_bytes={}->{} held_bytes={} family_store_bytes={}->{} \
         evicted={} held={} sweep_deleted_bytes={} sweep_error={} stopped_early={}",
        registry.family(),
        report.before.view_store_bytes,
        report.after.view_store_bytes,
        report.after.held_view_bytes,
        report.before.family_store.file_bytes,
        report.after.family_store.file_bytes,
        report.evicted.len(),
        report.held.len(),
        report.sweep.as_ref().map_or(0, |sweep| sweep.deleted_bytes),
        report.sweep_error.as_deref().unwrap_or("none"),
        report.stopped_early
    );
    for reason in &report.over_hard {
        crate::slog_warn!("view disk limits family={} {}", registry.family(), reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::v2::{ContentHash, FamilyKey, TrigramKey, TrigramPolicy};
    use crate::pins::{AssemblyPin, LivePin, QueryPin};
    use crate::views::manifest_v2::{
        EntryPlanes, EntryV2, GenerationName, ManifestHeader, ManifestV2, Producers, PublishV2,
    };
    use crate::views::readiness::PlaneState;
    use crate::views::registry::ViewRegistration;
    use crate::views::segment_store::{self, SegmentMember, TrigramPayload};
    use crate::views::RelPath;

    const FAMILY: &str = "family";

    fn policy() -> TrigramPolicy {
        TrigramPolicy {
            max_file_size: 1 << 20,
        }
    }

    fn trigram_key(bytes: &[u8]) -> FamilyKey {
        TrigramKey {
            content: ContentHash::of(bytes),
            policy: policy(),
        }
        .family_key()
    }

    fn rel(value: &str) -> RelPath {
        RelPath::new(value.as_bytes().to_vec()).unwrap()
    }

    /// Text with many distinct trigrams, so its trigram payload has real size.
    fn text(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (0..len)
            .map(|index| {
                if index % 61 == 60 {
                    return b'\n';
                }
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                b'a' + ((state >> 33) % 26) as u8
            })
            .collect()
    }

    /// Publishes `files` as the view's next generation through the protection
    /// protocol (live pin, put-or-touch, segment, manifest, pointer), then
    /// writes a `derived_kib` KiB database as that generation's derived
    /// file, standing in for the callgraph database a real generation has.
    fn publish(view: &ViewRegistration, files: &[(&str, Vec<u8>)], derived_kib: usize) -> String {
        let storage = view.registry().storage().to_path_buf();
        let store = view.open_store(FamilyPlane::Trigram).unwrap();
        let mut live = LivePin::create(view).unwrap();
        let keys = files
            .iter()
            .map(|(_, bytes)| trigram_key(bytes))
            .collect::<Vec<_>>();
        live.protect(&keys).unwrap();
        for (_, bytes) in files {
            store
                .put_or_touch(
                    &trigram_key(bytes),
                    &TrigramPayload::extract(bytes, &policy()).encode(),
                )
                .unwrap();
        }
        let members = files
            .iter()
            .map(|(path, bytes)| SegmentMember {
                rel_path: rel(path),
                content: ContentHash::of(bytes),
                size: bytes.len() as u64,
            })
            .collect::<Vec<_>>();
        let segment = segment_store::build_from_blobs(&store, &members, &policy()).unwrap();
        live.protect_segment(&segment.id).unwrap();
        segment_store::write_segment(&store, &storage, &segment, None).unwrap();
        let mut manifest = ManifestV2::new(ManifestHeader {
            producers: Producers {
                trigram: policy().fingerprint_hex(),
                semantic: None,
                callgraph: "callgraph-v1".to_string(),
            },
            head_tree: None,
            ignore_fingerprint: None,
            segment: Some(crate::blob_store::v2::to_hex(&segment.id)),
        });
        for (path, bytes) in files {
            manifest
                .insert(
                    rel(path),
                    EntryV2::regular(
                        ContentHash::of(bytes),
                        bytes.len() as u64,
                        EntryPlanes {
                            trigram: Some(PlaneState::ready(&trigram_key(bytes))),
                            semantic: None,
                            callgraph: None,
                        },
                    ),
                )
                .unwrap();
        }
        let store_view = view.view_store().unwrap();
        let base = store_view.current_generation().unwrap();
        let name = GenerationName::for_manifest(&manifest).unwrap();
        let prepared = store_view
            .prepare_v2(&name, base.as_deref(), &manifest, None)
            .unwrap();
        assert_eq!(
            store_view.commit_v2(prepared, None).unwrap(),
            PublishV2::Published
        );
        drop(live);
        let generation = name.to_string();
        if derived_kib > 0 {
            let path = store_view.derived_path(&generation).unwrap();
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE pad (value BLOB NOT NULL);
                     WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {derived_kib})
                     INSERT INTO pad SELECT zeroblob(1024) FROM n;"
                ))
                .unwrap();
        }
        generation
    }

    fn set_last_bind(registry: &FamilyRegistry, scope: &str, ms: u64) {
        registry
            .with_barrier(|tx| {
                tx.execute(
                    "UPDATE members SET last_bind_ms = ?2 WHERE scope = ?1",
                    params![scope, ms as i64],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn budget(soft: u64, hard: u64) -> DiskBudget {
        DiskBudget {
            view_store_soft: soft,
            view_store_hard: hard,
            ..DiskBudget::default()
        }
    }

    fn view_bytes(usage: &DiskUsage, scope: &str) -> u64 {
        usage
            .views
            .iter()
            .find(|view| view.scope == scope)
            .unwrap_or_else(|| panic!("{scope} not measured"))
            .bytes
    }

    fn evicted(report: &EnforcementReport) -> Vec<&str> {
        report
            .evicted
            .iter()
            .map(|(scope, _)| scope.as_str())
            .collect()
    }

    fn registered(registry: &FamilyRegistry, scope: &str) -> bool {
        registry.member(scope).unwrap().is_some()
    }

    /// Registers `scope`, publishes one generation of its own content, and
    /// ends its session.
    fn idle_view(registry: &FamilyRegistry, scope: &str, seed: u64, derived_kib: usize) {
        let root = registry.storage().join("roots").join(scope);
        fs::create_dir_all(&root).unwrap();
        let view = registry.register_view(scope, &root).unwrap();
        publish(&view, &[("file.txt", text(seed, 2_000))], derived_kib);
    }

    /// Over the soft limit, idle views go least recently bound first, and
    /// eviction stops as soon as the view store is back under the limit.
    #[test]
    fn eviction_takes_the_least_recently_bound_idle_views_first() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        for (seed, scope) in ["a", "b", "c"].into_iter().enumerate() {
            idle_view(&registry, scope, seed as u64, 64);
        }
        set_last_bind(&registry, "a", 3_000);
        set_last_bind(&registry, "b", 1_000);
        set_last_bind(&registry, "c", 2_000);
        let usage = measure(&registry, None).unwrap();
        assert!(usage.views.iter().all(|view| view.hold.is_none()));
        // Room for exactly one view: the two oldest must go, oldest first.
        registry.set_disk_budget(budget(view_bytes(&usage, "a"), u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert_eq!(evicted(&report), ["b", "c"]);
        assert!(registered(&registry, "a"));
        assert!(!registered(&registry, "b") && !registered(&registry, "c"));
        assert!(report.after.view_store_bytes <= view_bytes(&usage, "a"));
        assert!(report.over_hard.is_empty());
    }

    /// A view whose session is alive stays even when it is the least
    /// recently bound and the limit is not met without it.
    #[test]
    fn an_active_session_is_never_evicted() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let root = storage.path().join("root-active");
        fs::create_dir_all(&root).unwrap();
        let active = registry.register_view("active", &root).unwrap();
        publish(&active, &[("file.txt", text(1, 2_000))], 64);
        idle_view(&registry, "idle", 2, 64);
        set_last_bind(&registry, "active", 1);
        set_last_bind(&registry, "idle", 2);
        registry.set_disk_budget(budget(0, u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert_eq!(evicted(&report), ["idle"]);
        assert!(report
            .held
            .contains(&("active".to_string(), Hold::ActiveSession)));
        assert!(registered(&registry, "active"));
        let store = active.view_store().unwrap();
        let generation = store.current_generation().unwrap().unwrap();
        assert!(store.manifest_path(&generation).unwrap().is_file());
        assert!(store.derived_path(&generation).unwrap().is_file());
        drop(active);
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert_eq!(
            evicted(&report),
            ["active"],
            "a session that ended frees it"
        );
    }

    /// An old generation pinned by a reader keeps its whole view, though the
    /// view has no session and is over the limit.
    #[test]
    fn a_pinned_old_generation_keeps_its_view() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let root = storage.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let view = registry.register_view("pinned", &root).unwrap();
        let old = publish(&view, &[("file.txt", text(1, 2_000))], 32);
        let current = publish(&view, &[("file.txt", text(2, 2_000))], 32);
        assert_ne!(old, current);
        let view_dir = view.view_dir().to_path_buf();
        let pin = QueryPin::acquire(&view_dir, &old).unwrap();
        drop(view);
        registry.set_disk_budget(budget(0, u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert!(evicted(&report).is_empty(), "{report:?}");
        assert!(report
            .held
            .contains(&("pinned".to_string(), Hold::Protected)));
        let store = crate::views::ViewStore::existing_dir(view_dir.clone()).unwrap();
        assert!(store.manifest_path(&old).unwrap().is_file());
        assert!(store.derived_path(&old).unwrap().is_file());
        drop(pin);
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert_eq!(evicted(&report), ["pinned"]);
        assert!(!view_dir.exists());
    }

    /// An assembly in progress keeps a view, and so does a pin file that
    /// cannot be parsed: an unreadable pin might be protecting a generation,
    /// so it is never guessed away to meet the limit.
    #[test]
    fn assemblies_and_unreadable_protection_keep_their_views() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let root = storage.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let assembling = registry.register_view("assembling", &root).unwrap();
        publish(&assembling, &[("file.txt", text(1, 2_000))], 16);
        let assembly = AssemblyPin::create_v2(&assembling, "next-generation", &[]).unwrap();
        drop(assembling);
        idle_view(&registry, "malformed", 2, 16);
        let malformed_dir = registry.member("malformed").unwrap().unwrap().view_dir;
        fs::create_dir_all(malformed_dir.join("pins")).unwrap();
        fs::write(malformed_dir.join("pins").join("broken.json"), b"{not json").unwrap();
        registry.set_disk_budget(budget(0, u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert!(evicted(&report).is_empty(), "{report:?}");
        assert!(report
            .held
            .contains(&("assembling".to_string(), Hold::Protected)));
        assert!(report
            .held
            .iter()
            .any(|(scope, hold)| scope == "malformed" && matches!(hold, Hold::Uncertain(_))));
        drop(assembly);
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert_eq!(evicted(&report), ["assembling"]);
        assert!(registered(&registry, "malformed"));
    }

    /// When everything over the hard limit is held, nothing is deleted and a
    /// new view is refused with the named reason; an existing view still
    /// re-binds. Once a session ends, the new view is admitted by evicting it.
    #[test]
    fn held_bytes_over_the_hard_limit_refuse_new_views_by_name() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let mut sessions = Vec::new();
        for (seed, scope) in ["a", "b"].into_iter().enumerate() {
            let root = storage.path().join(scope);
            fs::create_dir_all(&root).unwrap();
            let view = registry.register_view(scope, &root).unwrap();
            publish(&view, &[("file.txt", text(seed as u64, 2_000))], 64);
            sessions.push(view);
        }
        let usage = measure(&registry, None).unwrap();
        let smallest = usage.views.iter().map(|view| view.bytes).min().unwrap();
        // A new view reserves the largest view's bytes: that does not fit
        // beside both held views, but fits beside one of them.
        registry.set_disk_budget(budget(0, usage.view_store_bytes + smallest / 2));
        let new_root = storage.path().join("c");
        fs::create_dir_all(&new_root).unwrap();

        let refused = registry.register_view("c", &new_root).unwrap_err();

        let RegistryError::AdmissionQueued(reason) = &refused else {
            panic!("unexpected error: {refused}");
        };
        assert!(reason.starts_with("queued: view store disk"), "{reason}");
        assert!(reason.contains("held by active sessions"), "{reason}");
        assert!(!registered(&registry, "c"));
        let after = measure(&registry, None).unwrap();
        assert_eq!(after.view_store_bytes, usage.view_store_bytes);
        let rebound = registry
            .register_view("a", &storage.path().join("a"))
            .expect("an existing view re-binds over the limit");
        drop(rebound);

        let b = sessions.pop().unwrap();
        drop(b);
        let admitted = registry.register_view("c", &new_root).unwrap();
        assert!(registered(&registry, "c"));
        assert!(!registered(&registry, "b"), "b was evicted to admit c");
        drop(admitted);
    }

    fn plane_file_bytes(storage: &Path, plane: FamilyPlane) -> u64 {
        let path = crate::blob_store::v2::plane_path(storage, FAMILY, plane).unwrap();
        ["", "-wal"]
            .iter()
            .map(|suffix| {
                let mut name = path.as_os_str().to_owned();
                name.push(suffix);
                fs::metadata(PathBuf::from(name)).map_or(0, |metadata| metadata.len())
            })
            .sum()
    }

    /// A key two views rely on is counted once and belongs to neither
    /// view's exclusive share. Evicting one view then reclaims exactly its
    /// exclusive keys, and the store files really get smaller.
    #[test]
    fn shared_keys_count_once_and_eviction_shrinks_the_files() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let shared = text(100, 60_000);
        let only_a = text(101, 60_000);
        let only_b = text(102, 60_000);
        let root_a = storage.path().join("a");
        let root_b = storage.path().join("b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        let a = registry.register_view("a", &root_a).unwrap();
        publish(
            &a,
            &[("shared.txt", shared.clone()), ("a.txt", only_a.clone())],
            16,
        );
        let b = registry.register_view("b", &root_b).unwrap();
        publish(
            &b,
            &[("shared.txt", shared.clone()), ("b.txt", only_b.clone())],
            16,
        );
        let store = b.open_store(FamilyPlane::Trigram).unwrap();
        let sizes = store
            .rows()
            .unwrap()
            .into_iter()
            .map(|row| (*row.key.as_bytes(), row.payload_bytes))
            .collect::<BTreeMap<_, _>>();
        let segments = store.segments().unwrap();
        assert_eq!(segments.len(), 2, "one segment per view");
        let segment_bytes = segments.iter().map(|row| row.byte_len).sum::<u64>();
        let shared_key = sizes[trigram_key(&shared).as_bytes()];
        let a_key = sizes[trigram_key(&only_a).as_bytes()];
        let b_key = sizes[trigram_key(&only_b).as_bytes()];

        let accounting = account_keys(&registry, None).unwrap();

        assert!(accounting.complete);
        assert_eq!(accounting.shared_bytes, shared_key);
        assert_eq!(
            accounting.referenced_bytes,
            shared_key + a_key + b_key + segment_bytes
        );
        let a_segment = accounting.exclusive_bytes["a"] - a_key;
        let b_segment = accounting.exclusive_bytes["b"] - b_key;
        assert_eq!(a_segment + b_segment, segment_bytes);
        assert_eq!(accounting.unreferenced_bytes, 0);

        drop(a);
        registry.set_disk_budget(budget(0, u64::MAX));
        let plane_before = plane_file_bytes(storage.path(), FamilyPlane::Trigram);
        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert_eq!(evicted(&report), ["a"]);
        let sweep = report.sweep.as_ref().expect("the sweep ran");
        assert_eq!(sweep.deleted_blobs, 1);
        assert_eq!(sweep.deleted_segments, 1);
        assert_eq!(sweep.deleted_bytes, a_key + a_segment);
        assert!(store.get(&trigram_key(&shared)).unwrap().is_some());
        assert!(store.get(&trigram_key(&only_b)).unwrap().is_some());
        assert!(store.get(&trigram_key(&only_a)).unwrap().is_none());
        assert!(report.after.view_store_bytes < report.before.view_store_bytes);
        assert!(
            report.after.family_store.file_bytes < report.before.family_store.file_bytes,
            "{} -> {}",
            report.before.family_store.file_bytes,
            report.after.family_store.file_bytes
        );
        let plane_after = plane_file_bytes(storage.path(), FamilyPlane::Trigram);
        assert!(
            plane_after < plane_before,
            "the trigram database itself shrinks: {plane_before} -> {plane_after}"
        );
        let after = account_keys(&registry, None).unwrap();
        assert_eq!(after.shared_bytes, 0);
        assert_eq!(after.referenced_bytes, shared_key + b_key + b_segment);
        drop(b);
    }

    /// An eviction interrupted after it removed the pointer leaves the
    /// member `evicting`; the next pass finishes it, and a registration in
    /// between clears the leftovers instead of reusing them.
    #[test]
    fn an_interrupted_eviction_is_finished_by_the_next_pass_or_registration() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        for scope in ["next-pass", "rebind"] {
            idle_view(&registry, scope, 7, 16);
            let member = registry.member(scope).unwrap().unwrap();
            // What a killed eviction leaves: state committed, pointer gone,
            // the rest of the directory still there.
            registry
                .with_barrier(|tx| {
                    tx.execute(
                        "UPDATE members SET state = ?2 WHERE scope = ?1",
                        params![scope, STATE_EVICTING],
                    )?;
                    Ok(())
                })
                .unwrap();
            remove_pointer(&member.view_dir).unwrap();
            assert!(registry.member(scope).unwrap().unwrap().evicting);
        }
        let rebind_dir = registry.member("rebind").unwrap().unwrap().view_dir;
        let rebound = registry
            .register_view("rebind", &storage.path().join("roots").join("rebind"))
            .unwrap();
        let leftovers = fs::read_dir(&rebind_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("derived-") || name.starts_with("manifest-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert!(!registry.member("rebind").unwrap().unwrap().evicting);

        // The soft limit is not exceeded; the pass finishes the eviction
        // because it began, not to meet a number.
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert_eq!(evicted(&report), ["next-pass"]);
        assert!(registered(&registry, "rebind"));
        drop(rebound);
    }

    // -----------------------------------------------------------------------
    // Rebuilding after eviction, against cold oracles

    use crate::views::contracts::{PlaneLoader, ViewAccess};
    use crate::views::first_load::{
        CallgraphBridge, CheckoutDriver, ConfiguredMembershipWalker, MembershipWalker,
        SiblingLoader, TrigramBridge,
    };
    use std::sync::Arc;

    const SOURCES: &[(&str, &str)] = &[
        (
            "src/alpha.rs",
            "pub fn alpha_total(values: &[u32]) -> u32 {\n    beta_sum(values)\n}\n\npub fn alpha_label() -> &'static str {\n    \"alpha\"\n}\n",
        ),
        (
            "src/beta.rs",
            "pub fn beta_sum(values: &[u32]) -> u32 {\n    values.iter().copied().sum()\n}\n\npub struct BetaCache {\n    entries: Vec<String>,\n}\n\nimpl BetaCache {\n    pub fn insert(&mut self, value: String) {\n        self.entries.push(value);\n    }\n}\n",
        ),
        (
            "src/gamma.rs",
            "fn gamma_parse(input: &str) -> Option<u64> {\n    input.trim().parse().ok()\n}\n",
        ),
        ("notes.txt", "alpha notes, not a source file\n"),
    ];

    fn write_tree(root: &Path, extra: &str) {
        for (path, text) in SOURCES {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fs::write(
            root.join("src").join("local.rs"),
            format!("pub fn {extra}_local() {{ crate::alpha::alpha_label(); }}\n"),
        )
        .unwrap();
    }

    /// Every observable of the trigram and callgraph planes for one loaded
    /// checkout: membership, matched lines for a few literals, and the
    /// derived database's logical rows.
    #[derive(Debug, PartialEq, Eq)]
    struct PlaneObservation {
        membership: Vec<String>,
        matches: Vec<String>,
        callgraph: crate::views::materialization::parity::LogicalSnapshot,
    }

    /// Registers and loads `scope` with the real trigram and callgraph
    /// planes, observes it, and drops every handle, which ends its session.
    fn load_and_observe(registry: &FamilyRegistry, scope: &str, root: &Path) -> PlaneObservation {
        let owner = registry.register_view(scope, root).unwrap();
        let trigram = Arc::new(TrigramBridge::new(
            registry.storage().to_path_buf(),
            policy(),
        ));
        let callgraph = Arc::new(CallgraphBridge::default());
        let driver = Arc::new(
            CheckoutDriver::new(
                owner.clone(),
                Producers {
                    trigram: policy().fingerprint_hex(),
                    semantic: None,
                    callgraph: crate::views::callgraph::PRODUCER.into(),
                },
                None,
                Arc::new(ConfiguredMembershipWalker),
                vec![trigram.clone(), callgraph.clone()],
            )
            .with_adapters(vec![trigram.adapter.clone(), callgraph.adapter.clone()]),
        );
        let loaded = SiblingLoader::new(
            driver,
            vec![trigram.adapter.clone(), callgraph.adapter.clone()],
        )
        .load(&ViewAccess::Owner(owner.clone()))
        .unwrap();
        assert!(
            loaded.pending_planes.is_empty(),
            "{scope}: {:?}",
            loaded.pending_planes
        );
        let generation = loaded.snapshot.generation().name().to_owned();
        let membership = loaded
            .snapshot
            .membership()
            .into_keys()
            .map(|path| String::from_utf8_lossy(path.as_bytes()).into_owned())
            .collect();
        let index = trigram.adapter.resident(scope, &generation).unwrap();
        let mut matches = Vec::new();
        for literal in ["alpha", "beta_sum", "_local", "entries"] {
            let answer = index.query(root, &loaded.snapshot, literal);
            assert!(
                answer.gaps.is_empty(),
                "{scope} {literal}: {:?}",
                answer.gaps
            );
            matches.push(format!("{literal}: {:?}", answer.matches));
        }
        let derived = owner
            .view_store()
            .unwrap()
            .derived_path(&generation)
            .unwrap();
        drop(index);
        drop(loaded);
        drop(trigram);
        drop(callgraph);
        drop(owner);
        PlaneObservation {
            membership,
            matches,
            callgraph: crate::views::materialization::parity::logical_snapshot(&derived),
        }
    }

    /// An evicted view re-binds as a new view: it seeds from its sibling,
    /// strictly reconciles its checkout and rebuilds, and every trigram and
    /// callgraph observable equals a cold build of the same checkout in an
    /// empty, isolated store.
    #[test]
    fn a_view_rebuilt_after_eviction_matches_a_cold_build_in_every_plane() {
        let storage = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let root_a = roots.path().join("a");
        let root_b = roots.path().join("b");
        write_tree(&root_a, "a");
        write_tree(&root_b, "b");
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let b_session = registry.register_view("b", &root_b).unwrap();
        load_and_observe(&registry, "b", &root_b);
        let first = load_and_observe(&registry, "a", &root_a);
        registry.set_disk_budget(budget(0, u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert_eq!(evicted(&report), ["a"], "{:?}", report.held);
        let a_dir = crate::views::registry::view_dir(storage.path(), "a").unwrap();
        assert!(!a_dir.exists());
        // The checkout changes while its view is evicted.
        fs::write(
            root_a.join("src").join("delta.rs"),
            "pub fn delta_entries() {}\n",
        )
        .unwrap();
        fs::remove_file(root_a.join("src").join("gamma.rs")).unwrap();

        let rebuilt = load_and_observe(&registry, "a", &root_a);

        let cold_storage = tempfile::tempdir().unwrap();
        let cold_registry = FamilyRegistry::open(cold_storage.path(), FAMILY).unwrap();
        let cold = load_and_observe(&cold_registry, "a", &root_a);
        assert_eq!(rebuilt, cold);
        assert_ne!(
            first.membership, rebuilt.membership,
            "the edit is reflected"
        );
        drop(b_session);
    }

    fn vector(text: &str) -> Vec<f32> {
        blake3::hash(text.as_bytes()).as_bytes()[..16]
            .iter()
            .map(|byte| (f32::from(*byte) - 127.5) / 127.5)
            .collect()
    }

    type SemanticRow = (String, String, u32, u32);

    fn semantic_rows(
        root: &Path,
        results: &[crate::semantic_index::SemanticResult],
    ) -> Vec<SemanticRow> {
        results
            .iter()
            .map(|result| {
                (
                    result
                        .file
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    result.name.clone(),
                    result.start_line,
                    result.score.to_bits(),
                )
            })
            .collect()
    }

    fn semantic_view(
        storage: &Path,
        scope: &str,
        root: &Path,
    ) -> crate::views::semantic_runtime::CheckoutSemantic {
        let runtime = crate::views::semantic_runtime::CheckoutSemantic::new(
            storage,
            FAMILY,
            scope,
            root,
            crate::views::semantic::SemanticProducer::current(
                "eviction-test-model",
                crate::semantic_index::EmbedTextCaps::default(),
            ),
            std::sync::Weak::new(),
        )
        .unwrap();
        runtime.load().unwrap();
        runtime
            .refresh(
                crate::views::semantic::FillBudget::default(),
                &mut |texts: Vec<String>| Ok(texts.iter().map(|text| vector(text)).collect()),
            )
            .unwrap();
        runtime
    }

    /// The semantic plane of an evicted view, rebuilt on its next bind,
    /// ranks exactly like a full build of the checkout.
    #[test]
    fn a_semantic_view_rebuilt_after_eviction_ranks_like_a_cold_build() {
        let storage = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let root_a = roots.path().join("a");
        let root_b = roots.path().join("b");
        write_tree(&root_a, "a");
        write_tree(&root_b, "b");
        let b = semantic_view(storage.path(), "b", &root_b);
        drop(semantic_view(storage.path(), "a", &root_a));
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        registry.set_disk_budget(budget(0, u64::MAX));

        let report = enforce(&registry, EnforceOptions::default()).unwrap();

        assert_eq!(evicted(&report), ["a"], "{:?}", report.held);
        fs::write(
            root_a.join("src").join("delta.rs"),
            "pub fn delta_cache_insert() {}\n",
        )
        .unwrap();
        let a = semantic_view(storage.path(), "a", &root_a);
        let query = "beta cache insert";
        let answer = a.search(&vector(query), 100, &|_| true).unwrap();
        assert!(answer.complete(), "{answer:?}");

        let files = ConfiguredMembershipWalker.files(&root_a).unwrap();
        let cold = crate::semantic_index::SemanticIndex::build(
            &root_a,
            &files,
            &mut |batch: Vec<String>| Ok(batch.iter().map(|text| vector(text)).collect()),
            64,
        )
        .unwrap();
        assert_eq!(
            semantic_rows(&root_a, &answer.results),
            semantic_rows(&root_a, &cold.search(&vector(query), 100))
        );
        drop(a);
        drop(b);
    }

    /// A view whose checkout root is gone is removed only after two passes
    /// find it with no protection of any kind. Disk limits do not shorten
    /// that: protected only by a reader's read marker, it stays, with its
    /// protected generation, through two passes and through a pass that is
    /// over the limit; after the reader leaves, two further passes remove it.
    #[test]
    fn a_missing_root_view_held_only_by_a_reader_marker_survives_enforcement() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let root = storage.path().join("removed-root");
        fs::create_dir_all(&root).unwrap();
        let view = registry.register_view("removed", &root).unwrap();
        let generation = publish(&view, &[("file.txt", text(5, 2_000))], 16);
        let view_dir = view.view_dir().to_path_buf();
        drop(view);
        fs::remove_dir_all(&root).unwrap();
        let reader = registry.register_reader("parent-folder").unwrap();
        let marker = reader.protect_current("removed").unwrap().unwrap();
        assert_eq!(marker.generation(), generation);

        for pass in 0..2 {
            let report = enforce(&registry, EnforceOptions::default()).unwrap();
            assert!(evicted(&report).is_empty(), "pass {pass}");
            let sweep = report.sweep.expect("the sweep ran");
            assert!(sweep.deregistered.is_empty(), "pass {pass}");
        }
        registry.set_disk_budget(budget(0, u64::MAX));
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert!(evicted(&report).is_empty());
        assert!(report
            .held
            .contains(&("removed".to_string(), Hold::Protected)));
        let store = crate::views::ViewStore::existing_dir(view_dir.clone()).unwrap();
        assert!(store.manifest_path(&generation).unwrap().is_file());
        assert!(registered(&registry, "removed"));

        drop(marker);
        drop(reader);
        registry.set_disk_budget(DiskBudget::default());
        let first = enforce(&registry, EnforceOptions::default()).unwrap();
        assert!(first.sweep.unwrap().deregistered.is_empty());
        assert!(registered(&registry, "removed") && view_dir.is_dir());
        let second = enforce(&registry, EnforceOptions::default()).unwrap();
        assert_eq!(second.sweep.unwrap().deregistered, ["removed"]);
        assert!(!registered(&registry, "removed") && !view_dir.exists());
    }

    /// Occupancy of a small family of real views (trigram and callgraph
    /// planes) against the configured default limits, printed for the
    /// record. At the defaults nothing is evicted.
    #[test]
    fn reports_occupancy_at_the_default_limits() {
        assert_eq!(VIEW_STORE_SOFT_BYTES, 4 * 1024 * 1024 * 1024);
        assert_eq!(VIEW_STORE_HARD_BYTES, 6 * 1024 * 1024 * 1024);
        let storage = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), FAMILY).unwrap();
        let mut sessions = Vec::new();
        for scope in ["main", "feature", "review"] {
            let root = roots.path().join(scope);
            write_tree(&root, scope);
            sessions.push(registry.register_view(scope, &root).unwrap());
            load_and_observe(&registry, scope, &root);
        }
        let usage = measure(&registry, None).unwrap();
        let keys = account_keys(&registry, None).unwrap();
        let percent = |bytes: u64, of: u64| bytes as f64 * 100.0 / of as f64;
        for view in &usage.views {
            eprintln!(
                "occupancy view={} bytes={} exclusive_family_bytes={} hold={:?}",
                view.scope,
                view.bytes,
                keys.exclusive_bytes.get(&view.scope).copied().unwrap_or(0),
                view.hold
            );
        }
        eprintln!(
            "occupancy view_store_bytes={} ({:.6}% of the {} byte soft limit) family_store_file_bytes={} \
             ({:.6}% of the {} byte soft limit) payload_bytes={} referenced_bytes={} shared_bytes={}",
            usage.view_store_bytes,
            percent(usage.view_store_bytes, VIEW_STORE_SOFT_BYTES),
            VIEW_STORE_SOFT_BYTES,
            usage.family_store.file_bytes,
            percent(usage.family_store.file_bytes, FAMILY_STORE_SOFT_BYTES),
            FAMILY_STORE_SOFT_BYTES,
            usage.family_store.payload_bytes,
            keys.referenced_bytes,
            keys.shared_bytes
        );
        assert!(usage.complete && keys.complete);
        assert!(keys.shared_bytes > 0, "the three checkouts share content");
        drop(sessions);
        let report = enforce(&registry, EnforceOptions::default()).unwrap();
        assert!(evicted(&report).is_empty());
        assert!(report.over_hard.is_empty());
    }
}
