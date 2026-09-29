//! The reusable parity harness for per-checkout views.
//!
//! Every plane proves that what a view answers equals an **independent cold
//! rebuild** of the same frozen checkout. The oracle starts from an empty,
//! isolated store and rebuilds from source; it never reuses the tested view's
//! blobs, seeds or derived files. Planes supply the observations (membership,
//! matched lines, semantic identities and scores, logical callgraph rows and
//! bound query results); timestamps and SQLite file bytes are never compared.
//!
//! The harness also drives the shared edit schedules every plane must pass:
//! edits, delete and recreate, rename, revert, add-then-delete, A→B→A
//! switches, untracked and ignored membership, and ignore-file changes.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::segment_store::rel_path_to_os;
use super::snapshot::DiskState;
use super::RelPath;

/// Walker membership of a checkout: tracked and untracked files that are not
/// ignored, with their content, using the same walker and ignore rules as the
/// trigram build.
pub fn walker_membership(root: &Path) -> io::Result<BTreeMap<RelPath, DiskState>> {
    let mut members = BTreeMap::new();
    let filters = crate::search_index::PathFilters::default();
    for path in crate::search_index::walk_project_files(root, &filters) {
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "walker left the checkout root")
        })?;
        let rel_path = RelPath::from_os_path(relative)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        match fs::read(&path) {
            Ok(bytes) => {
                members.insert(rel_path, DiskState::of_bytes(&bytes));
            }
            // A file removed between the walk and the read is not a member.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(members)
}

/// A frozen copy of a checkout's walker membership. Oracles rebuild from this
/// copy, so later edits to the live checkout cannot leak into the oracle.
/// The caller owns the (empty) directory the copy is written into.
pub struct FrozenCheckout {
    dir: PathBuf,
    membership: BTreeMap<RelPath, DiskState>,
}

impl fmt::Debug for FrozenCheckout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrozenCheckout")
            .field("root", &self.dir)
            .field("members", &self.membership.len())
            .finish()
    }
}

impl FrozenCheckout {
    /// Copies `root`'s walker membership into the empty directory `into`.
    pub fn capture(root: &Path, into: &Path) -> io::Result<Self> {
        if fs::read_dir(into)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a frozen checkout must be captured into an empty directory",
            ));
        }
        let dir = into.to_path_buf();
        let membership = walker_membership(root)?;
        for rel_path in membership.keys() {
            let relative = rel_path_to_os(rel_path)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            let destination = dir.join(&relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(root.join(&relative), &destination)?;
        }
        // The copy's bytes may have moved on since the walk; the membership
        // recorded is the copy's, so the oracle and its source agree.
        let membership = membership
            .into_keys()
            .map(|rel_path| {
                let relative = rel_path_to_os(&rel_path).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                })?;
                let bytes = fs::read(dir.join(relative))?;
                Ok((rel_path, DiskState::of_bytes(&bytes)))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { dir, membership })
    }

    pub fn root(&self) -> &Path {
        &self.dir
    }

    pub fn membership(&self) -> &BTreeMap<RelPath, DiskState> {
        &self.membership
    }

    pub fn read(&self, rel_path: &RelPath) -> io::Result<Vec<u8>> {
        let relative = rel_path_to_os(rel_path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        fs::read(self.dir.join(relative))
    }
}

/// A fresh, empty AFT storage directory for a cold rebuild. It refuses a
/// directory that already holds anything, so an oracle can never start from
/// the tested view's blobs, seeds or derived files.
pub struct IsolatedStore {
    dir: PathBuf,
}

impl IsolatedStore {
    pub fn empty(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        if fs::read_dir(dir)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an isolated store must start empty",
            ));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

/// One plane's parity check: what the view under test answers, and what an
/// independent cold rebuild of the frozen checkout answers.
pub trait PlaneOracle {
    type Observation: fmt::Debug + Eq;

    fn name(&self) -> &str;

    /// The answer of the view under test for the live checkout at `root`.
    fn observe_view(&self, root: &Path) -> Self::Observation;

    /// The answer of a cold rebuild from `frozen` into the empty `store`.
    fn rebuild_cold(&self, frozen: &FrozenCheckout, store: &IsolatedStore) -> Self::Observation;
}

/// A parity failure with enough context to reproduce it.
#[derive(Debug)]
pub struct ParityMismatch {
    pub oracle: String,
    pub step: String,
    pub view: String,
    pub cold: String,
}

impl fmt::Display for ParityMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} parity failed after {}:\n view: {}\n cold: {}",
            self.oracle, self.step, self.view, self.cold
        )
    }
}

/// Freezes the checkout, rebuilds cold into an isolated store, and compares.
/// `scratch` must be an empty directory the caller owns.
pub fn check_parity<O: PlaneOracle>(
    oracle: &O,
    root: &Path,
    scratch: &Path,
    step: &str,
) -> io::Result<Result<(), ParityMismatch>> {
    let frozen_dir = scratch.join("frozen");
    fs::create_dir_all(&frozen_dir)?;
    let frozen = FrozenCheckout::capture(root, &frozen_dir)?;
    let store = IsolatedStore::empty(&scratch.join("store"))?;
    let view = oracle.observe_view(root);
    let cold = oracle.rebuild_cold(&frozen, &store);
    Ok(if view == cold {
        Ok(())
    } else {
        Err(ParityMismatch {
            oracle: oracle.name().to_owned(),
            step: step.to_owned(),
            view: format!("{view:?}"),
            cold: format!("{cold:?}"),
        })
    })
}

/// One filesystem edit of a schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditStep {
    Write { path: String, bytes: Vec<u8> },
    Delete { path: String },
    Rename { from: String, to: String },
}

impl EditStep {
    pub fn write(path: &str, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Write {
            path: path.to_owned(),
            bytes: bytes.into(),
        }
    }

    pub fn delete(path: &str) -> Self {
        Self::Delete {
            path: path.to_owned(),
        }
    }

    pub fn rename(from: &str, to: &str) -> Self {
        Self::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        }
    }

    pub fn apply(&self, root: &Path) -> io::Result<()> {
        match self {
            Self::Write { path, bytes } => {
                let path = root.join(path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, bytes)
            }
            Self::Delete { path } => match fs::remove_file(root.join(path)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            },
            Self::Rename { from, to } => {
                let to = root.join(to);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(root.join(from), to)
            }
        }
    }

    /// The paths this step changes.
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Write { path, .. } | Self::Delete { path } => vec![PathBuf::from(path)],
            Self::Rename { from, to } => vec![PathBuf::from(from), PathBuf::from(to)],
        }
    }
}

/// A named sequence of edits.
#[derive(Clone, Debug)]
pub struct EditSchedule {
    pub name: &'static str,
    pub steps: Vec<EditStep>,
}

/// The ignore file the standard schedules edit. `.aftignore` applies with or
/// without a git repository, so fixtures need no `git init`.
pub const FIXTURE_IGNORE_FILE: &str = ".aftignore";

/// The shared schedules every plane runs. `base` names a file that exists in
/// the fixture with `base_bytes`; `ignored` is a path the fixture's
/// [`FIXTURE_IGNORE_FILE`] excludes.
pub fn standard_schedules(base: &str, base_bytes: &[u8], ignored: &str) -> Vec<EditSchedule> {
    let edited = [base_bytes, b"\n// edited\n".as_slice()].concat();
    vec![
        EditSchedule {
            name: "edit",
            steps: vec![EditStep::write(base, edited.clone())],
        },
        EditSchedule {
            name: "delete then recreate",
            steps: vec![
                EditStep::delete(base),
                EditStep::write(base, edited.clone()),
            ],
        },
        EditSchedule {
            name: "rename",
            steps: vec![EditStep::rename(base, "renamed/moved.txt")],
        },
        EditSchedule {
            name: "revert",
            steps: vec![
                EditStep::write(base, edited.clone()),
                EditStep::write(base, base_bytes.to_vec()),
            ],
        },
        EditSchedule {
            name: "add then delete",
            steps: vec![
                EditStep::write("added.txt", b"transient".to_vec()),
                EditStep::delete("added.txt"),
            ],
        },
        EditSchedule {
            name: "A to B to A",
            steps: vec![
                EditStep::write(base, b"branch b".to_vec()),
                EditStep::write("only-on-b.txt", b"b".to_vec()),
                EditStep::write(base, base_bytes.to_vec()),
                EditStep::delete("only-on-b.txt"),
            ],
        },
        EditSchedule {
            name: "untracked and ignored",
            steps: vec![
                EditStep::write("untracked.txt", b"untracked".to_vec()),
                EditStep::write(ignored, b"ignored".to_vec()),
            ],
        },
        EditSchedule {
            name: "ignore file change",
            steps: vec![
                EditStep::write(ignored, b"ignored".to_vec()),
                EditStep::write(FIXTURE_IGNORE_FILE, b"".to_vec()),
            ],
        },
    ]
}
