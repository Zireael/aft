//! Sibling-seeded loading contracts. These hooks are opt-in until all planes
//! can build checkout generations; registering a driver does not change legacy routing.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::blob_store::v2::FamilyPlane;

use super::contracts::{PlaneError, ViewAccess};
use super::manifest_v2::Producers;
use super::snapshot::{LiveEntry, OpenGeneration, Snapshot};
use super::RelPath;

/// A complete strict content walk under the checkout's current ignore rules.
/// The revision must change for every delivered edit, write intent, watcher gap,
/// or membership change; a partial walk must return an error, not this value.
#[derive(Clone, Debug)]
pub struct ReconciledCheckout {
    pub revision: u64,
    pub entries: BTreeMap<RelPath, LiveEntry>,
}

/// The runtime owner supplies source reads and generation construction. Plane
/// implementations attach their data here rather than writing shared runtime code.
pub trait FirstLoadDriver: Send + Sync {
    /// The loader calls this before choosing a seed. It must not write or block
    /// on model work; every selected manifest must match these producer identities.
    fn producers(&self, access: &ViewAccess) -> Producers;

    /// The loader calls this for seed preference only. No HEAD is valid for a
    /// copied or non-git tree; it must not replace the strict membership walk.
    fn head_tree(&self, access: &ViewAccess) -> Option<String>;

    /// The loader calls this on owners only. It may block while walking and
    /// hashing every current member, attaching plane data from the same bytes.
    /// It must not publish or write another checkout's artifacts.
    fn reconcile(&self, access: &ViewAccess) -> Result<ReconciledCheckout, PlaneError>;

    /// Called after the walk, and again before installation, to detect edits
    /// made during loading. This must not block or write any filesystem state.
    fn revision(&self, access: &ViewAccess) -> u64;

    /// Called on owners after strict reconciliation. It may block to build and
    /// publish the owner's generation under the core protection and compare-and-swap protocol.
    /// It must return a pinned, verified generation, never merely a pointer.
    /// A foreign derived graph may be copied only when `seed_derived` is true.
    fn build_own_generation(
        &self,
        access: &ViewAccess,
        snapshot: &Snapshot,
        seed_derived: bool,
    ) -> Result<Arc<OpenGeneration>, PlaneError>;

    /// Called with a reconciled seed snapshot, then with the rebased own snapshot.
    /// It must atomically install resident data and snapshot under the root lock,
    /// rejecting an obsolete revision. It may block on that lock, never build,
    /// publish, repair another checkout's state, or report success before data is installed.
    fn install(
        &self,
        access: &ViewAccess,
        snapshot: &Snapshot,
        revision: u64,
    ) -> Result<(), PlaneError>;
}

/// The query router supplies an atomic installed snapshot and its actual gaps.
/// Pointer publication alone must not satisfy this interface.
pub trait QueryState: Send + Sync {
    /// Called repeatedly during a bounded wait, including once after timeout.
    /// It may briefly block on the root lock, but must never build, repair, or
    /// write any artifacts. Gaps include pending, failed, dirty and intent paths
    /// for this plane even when another plane is ready.
    fn installed_state(
        &self,
        access: &ViewAccess,
        plane: FamilyPlane,
    ) -> (Snapshot, Vec<std::path::PathBuf>);
}
