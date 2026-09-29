//! The family GC protocol for per-checkout (v2) stores.
//!
//! One sweep, under the family's one-sweeper lease:
//!
//! 1. bump the registry's GC epoch to `S` and raise every plane store to `S`;
//! 2. mark, for **every** registered member: its current manifest, every
//!    generation protected by a pin or read marker in its `pins/` and
//!    `readers/`, and the keys listed by its assembly and live pins;
//! 3. delete each unmarked row with `DELETE … WHERE full_key = ? AND
//!    ref_epoch < S`, each in its own IMMEDIATE transaction, then return the
//!    freed pages;
//! 4. under the registry's write lock (the handoff barrier), count sweeps for
//!    members whose checkout root is gone and that have no protection of any
//!    class; the second such consecutive sweep removes the member and its
//!    view directory.
//!
//! Why deletion is safe: any work W that relies on key K made its protection
//! durable, then touched K. If the touch committed after the epoch bump, K's
//! `ref_epoch >= S` and the conditional delete does nothing. If the delete
//! committed first, the touch reports K missing and W puts it again. If the
//! touch committed before the bump, W's protection was durable before marking
//! began, so marking saw it. The stores' SQLite write locks order these
//! events; no wall clock is involved.
//!
//! Any error reading a member's manifest, `pins/`, `readers/` or a pin's keys,
//! and any malformed pin, aborts the sweep with nothing deleted. Pins are
//! reclaimed only when their owner process is gone, never by age alone, so a
//! stopped but live owner keeps its protection.
//!
//! This module does not decide *what* to evict under disk pressure; the byte
//! budget here only bounds how much unmarked garbage a sweep collects.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::blob_store::v2::{
    plane_path, segment_path, FamilyPlane, FamilyStore, StoreError, StoreWriteAccess,
};
use crate::pins::{self, PinError};
use crate::views::registry::{FamilyRegistry, MemberRecord, RegistryError, MISSING_ROOT_SWEEPS};
use crate::views::ViewStore;

#[derive(Debug)]
pub enum FamilySweepError {
    /// Another live process holds the family's sweep lease.
    Busy,
    /// Protection state could not be read with certainty; nothing was deleted.
    Uncertain(String),
    Registry(RegistryError),
    Store(StoreError),
    Io(std::io::Error),
}

impl fmt::Display for FamilySweepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "another process is sweeping this family"),
            Self::Uncertain(reason) => {
                write!(f, "sweep aborted with nothing deleted: {reason}")
            }
            Self::Registry(error) => write!(f, "{error}"),
            Self::Store(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "family sweep I/O error: {error}"),
        }
    }
}

impl std::error::Error for FamilySweepError {}

impl From<RegistryError> for FamilySweepError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}
impl From<StoreError> for FamilySweepError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<std::io::Error> for FamilySweepError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// How much unmarked garbage one sweep collects.
#[derive(Clone, Copy, Debug)]
pub struct FamilySweepPolicy {
    /// Unmarked rows are deleted, oldest epoch first, while a plane's payload
    /// bytes exceed this budget. Zero collects every unmarked row.
    pub byte_budget: u64,
}

/// Points in a sweep where tests can pause it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweepStep {
    EpochRaised,
    Marked,
    Deleted,
}

pub trait SweepObserver {
    fn reached(&self, step: SweepStep);
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FamilySweepReport {
    pub epoch: u64,
    pub marked_keys: usize,
    pub deleted_blobs: usize,
    pub deleted_bytes: u64,
    pub deleted_segments: usize,
    pub reclaimed_pins: usize,
    pub reclaimed_readers: usize,
    /// Members whose root is gone but that are kept this sweep.
    pub missing_root_retained: Vec<String>,
    /// Members removed with their view directories.
    pub deregistered: Vec<String>,
}

/// What one member contributes to marking.
#[derive(Debug, Default)]
struct MemberMarks {
    keys: BTreeSet<[u8; 32]>,
    reclaimed_pins: usize,
}

/// Runs one family sweep. `requested_by` names the view on whose behalf the
/// sweep runs; it does not narrow marking, which always covers every member.
pub fn sweep_family(
    registry: &FamilyRegistry,
    requested_by: Option<&str>,
    policy: FamilySweepPolicy,
    observer: Option<&dyn SweepObserver>,
) -> Result<FamilySweepReport, FamilySweepError> {
    let _ = requested_by;
    let lease = registry.begin_sweep()?.ok_or(FamilySweepError::Busy)?;
    let epoch = lease.epoch();
    let mut report = FamilySweepReport {
        epoch,
        ..FamilySweepReport::default()
    };
    let access = StoreWriteAccess::for_registered_view(registry.storage(), registry.family());
    let mut stores = Vec::new();
    for plane in FamilyPlane::ALL {
        if plane_path(registry.storage(), registry.family(), plane)?.is_file() {
            let store = FamilyStore::open(&access, plane)?;
            store.raise_epoch(epoch)?;
            stores.push(store);
        }
    }
    observe(observer, SweepStep::EpochRaised);

    report.reclaimed_readers = registry.reclaim_dead_readers()?;
    let members = registry.members()?;
    let mut marked = BTreeSet::new();
    for member in &members {
        let marks = mark_member(member, registry.family())
            .map_err(|reason| FamilySweepError::Uncertain(format!("{}: {reason}", member.scope)))?;
        report.reclaimed_pins += marks.reclaimed_pins;
        marked.extend(marks.keys);
    }
    report.marked_keys = marked.len();
    observe(observer, SweepStep::Marked);

    for store in &stores {
        delete_unmarked(registry, store, &marked, epoch, policy, &mut report)?;
    }
    observe(observer, SweepStep::Deleted);

    for member in &members {
        deregister_if_missing(registry, member, &mut report)?;
    }
    lease.release();
    Ok(report)
}

fn observe(observer: Option<&dyn SweepObserver>, step: SweepStep) {
    if let Some(observer) = observer {
        observer.reached(step);
    }
}

fn delete_unmarked(
    registry: &FamilyRegistry,
    store: &FamilyStore,
    marked: &BTreeSet<[u8; 32]>,
    epoch: u64,
    policy: FamilySweepPolicy,
    report: &mut FamilySweepReport,
) -> Result<(), FamilySweepError> {
    let rows = store.rows()?;
    let segments = store.segments()?;
    let mut total = rows.iter().map(|row| row.payload_bytes).sum::<u64>()
        + segments.iter().map(|row| row.byte_len).sum::<u64>();
    let mut deleted_any = false;
    for row in rows {
        if total <= policy.byte_budget {
            break;
        }
        // The epoch check happens in the DELETE itself, not here: a touch can
        // land between this listing and the delete.
        if marked.contains(row.key.as_bytes()) {
            continue;
        }
        if store.delete_if_unreferenced_since(&row.key, epoch)? {
            total = total.saturating_sub(row.payload_bytes);
            report.deleted_blobs += 1;
            report.deleted_bytes += row.payload_bytes;
            deleted_any = true;
        }
    }
    for segment in segments {
        if total <= policy.byte_budget {
            break;
        }
        if marked.contains(&segment.segment_id) {
            continue;
        }
        let file = segment_path(registry.storage(), registry.family(), &segment.segment_id)?;
        if store.delete_segment_if_unreferenced_since(&segment.segment_id, epoch, &file)?
            == crate::blob_store::v2::SegmentDeletion::Deleted
        {
            total = total.saturating_sub(segment.byte_len);
            report.deleted_segments += 1;
            report.deleted_bytes += segment.byte_len;
        }
    }
    if deleted_any {
        store.incremental_vacuum()?;
    }
    Ok(())
}

/// Marks everything one member relies on. Any uncertainty is an error.
fn mark_member(member: &MemberRecord, family: &str) -> Result<MemberMarks, String> {
    let mut marks = MemberMarks::default();
    let view_dir = &member.view_dir;
    match fs::symlink_metadata(view_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(marks),
        Err(error) => return Err(format!("view directory unreadable: {error}")),
    }
    let store = ViewStore::existing_dir(view_dir.clone());
    if let Some(store) = &store {
        let current = store
            .current_generation()
            .map_err(|error| format!("pointer unreadable: {error}"))?;
        if let Some(generation) = current {
            mark_generation(store, &generation, &mut marks)?;
        }
    }
    mark_pins(view_dir, family, store.as_ref(), &mut marks)?;
    mark_readers(view_dir, store.as_ref(), &mut marks)?;
    Ok(marks)
}

fn mark_generation(
    store: &ViewStore,
    generation: &str,
    marks: &mut MemberMarks,
) -> Result<(), String> {
    let manifest = store
        .load_manifest_v2(generation)
        .map_err(|error| format!("manifest {generation} unreadable: {error}"))?;
    marks
        .keys
        .extend(manifest.ready_keys().map(|key| *key.as_bytes()));
    if let Some(segment) = manifest.segment_id() {
        marks.keys.insert(segment);
    }
    Ok(())
}

fn mark_pins(
    view_dir: &Path,
    family: &str,
    store: Option<&ViewStore>,
    marks: &mut MemberMarks,
) -> Result<(), String> {
    let pins_dir = view_dir.join("pins");
    let entries = match fs::read_dir(&pins_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("pins unreadable: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("pins unreadable: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = pins::read_metadata_strict(&path)
            .map_err(|error| format!("malformed pin {}: {error}", path.display()))?;
        let (metadata_path, keys_path) = pins::pin_paths(view_dir, &metadata.generation);
        if metadata_path != path || metadata.family != family {
            return Err(format!(
                "pin {} does not belong to this view and family",
                path.display()
            ));
        }
        if !pins::owner_is_live(&metadata.owner) {
            let _ = fs::remove_file(&metadata_path);
            let _ = fs::remove_file(&keys_path);
            crate::fs_lock::sync_parent(&metadata_path);
            marks.reclaimed_pins += 1;
            continue;
        }
        let keys = pins::read_keys(&keys_path).map_err(|error: PinError| {
            format!("pin keys {} unreadable: {error}", keys_path.display())
        })?;
        marks.keys.extend(keys);
        if let Some(store) = store {
            mark_generation_if_present(store, &metadata.generation, marks)?;
        }
    }
    Ok(())
}

fn mark_readers(
    view_dir: &Path,
    store: Option<&ViewStore>,
    marks: &mut MemberMarks,
) -> Result<(), String> {
    let readers = view_dir.join("readers");
    let entries = match fs::read_dir(&readers) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("readers unreadable: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("readers unreadable: {error}"))?;
        let is_dir = entry
            .file_type()
            .map_err(|error| format!("reader entry unreadable: {error}"))?
            .is_dir();
        if !is_dir {
            continue;
        }
        let Some(generation) = entry.file_name().to_str().map(str::to_owned) else {
            return Err("reader directory with a non-UTF-8 name".to_string());
        };
        // Unreadable markers count as protected here, so uncertainty retains.
        if crate::root_cache::sweep_read_markers(view_dir, &generation).protected {
            if let Some(store) = store {
                mark_generation_if_present(store, &generation, marks)?;
            }
        }
    }
    Ok(())
}

fn mark_generation_if_present(
    store: &ViewStore,
    generation: &str,
    marks: &mut MemberMarks,
) -> Result<(), String> {
    let path = store
        .manifest_path(generation)
        .map_err(|error| error.to_string())?;
    match fs::symlink_metadata(&path) {
        Ok(_) => mark_generation(store, generation, marks),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("manifest {generation} unreadable: {error}")),
    }
}

/// True when the root is certainly gone. An unreadable parent is not "gone".
fn root_is_missing(member: &MemberRecord) -> bool {
    match &member.root {
        Some(root) => matches!(
            fs::symlink_metadata(root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ),
        None => false,
    }
}

/// Protection of any class for a member, checked inside the barrier: live
/// pins (assembly, live or seed), and protected read or residency markers.
/// An error is uncertainty and keeps the member.
fn member_has_protection(view_dir: &Path) -> Result<bool, String> {
    let pins_dir = view_dir.join("pins");
    match fs::read_dir(&pins_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let metadata =
                    pins::read_metadata_strict(&path).map_err(|error| error.to_string())?;
                if pins::owner_is_live(&metadata.owner) {
                    return Ok(true);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let readers = view_dir.join("readers");
    match fs::read_dir(&readers) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    continue;
                }
                let generation = entry
                    .file_name()
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "non-UTF-8 reader directory".to_string())?;
                if crate::root_cache::sweep_read_markers(view_dir, &generation).protected {
                    return Ok(true);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    Ok(false)
}

fn deregister_if_missing(
    registry: &FamilyRegistry,
    member: &MemberRecord,
    report: &mut FamilySweepReport,
) -> Result<(), FamilySweepError> {
    let missing = root_is_missing(member);
    let outcome = registry.with_barrier(|tx| {
        let recorded: Option<i64> = tx
            .query_row(
                "SELECT missing_sweeps FROM members WHERE scope = ?1",
                params![member.scope],
                |row| row.get(0),
            )
            .optional()?;
        let Some(recorded) = recorded else {
            return Ok(Deregistration::Gone);
        };
        if !missing {
            if recorded != 0 {
                tx.execute(
                    "UPDATE members SET missing_sweeps = 0 WHERE scope = ?1",
                    params![member.scope],
                )?;
            }
            return Ok(Deregistration::Present);
        }
        match member_has_protection(&member.view_dir) {
            Ok(false) => {}
            Ok(true) => {
                tx.execute(
                    "UPDATE members SET missing_sweeps = 0 WHERE scope = ?1",
                    params![member.scope],
                )?;
                return Ok(Deregistration::Retained);
            }
            // Uncertainty keeps the member and does not advance its count.
            Err(_) => return Ok(Deregistration::Retained),
        }
        let count = recorded.max(0) as u32 + 1;
        if count < MISSING_ROOT_SWEEPS {
            tx.execute(
                "UPDATE members SET missing_sweeps = ?2 WHERE scope = ?1",
                params![member.scope, i64::from(count)],
            )?;
            return Ok(Deregistration::Retained);
        }
        // Remove the directory while the barrier is held, so no registration
        // or reader confirmation can interleave with the removal.
        match fs::remove_dir_all(&member.view_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Ok(Deregistration::Retained),
        }
        tx.execute(
            "DELETE FROM members WHERE scope = ?1",
            params![member.scope],
        )?;
        Ok(Deregistration::Removed)
    })?;
    match outcome {
        Deregistration::Retained => report.missing_root_retained.push(member.scope.clone()),
        Deregistration::Removed => report.deregistered.push(member.scope.clone()),
        Deregistration::Present | Deregistration::Gone => {}
    }
    Ok(())
}

enum Deregistration {
    Present,
    Retained,
    Removed,
    Gone,
}
