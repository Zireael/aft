//! The family registry: which views and registry readers exist for one
//! repository family, the family GC epoch, and the one-sweeper lease.
//!
//! It lives at `<storage>/blobs/v2/<family>/members.sqlite`. The safety of the
//! family GC rests on one ordering rule: a view is registered here, in an
//! IMMEDIATE transaction, **before** it creates any pin, put, segment, seed pin
//! or derived clone. A sweep marks every registered member, so a view that
//! registers after marking began can only create references that touch at the
//! new epoch, which the sweep's conditional delete never removes.
//!
//! Two kinds of participant register:
//!
//! - a **view** ([`ViewRegistration`]) owns a checkout's view directory and may
//!   write family stores (it is the only source of `StoreWriteAccess`);
//! - a **registry reader** ([`ReaderRegistration`]) holds no view of its own.
//!   A multi-repo parent-folder session is one: it reads its child
//!   repositories' views and never writes, builds or repairs them. Its
//!   registration is a reader marker, not a membership, and the generations it
//!   serves are protected by read markers in the member's view directory,
//!   taken with the pin-then-verify protocol of
//!   [`ReaderRegistration::protect_current`].
//!
//! Deregistering a member whose checkout root is gone happens under the
//! registry's write lock (the handoff barrier), and only when no protection of
//! any class exists for it; see `gc::family`.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use crate::blob_store::v2::{
    family_dir, FamilyPlane, FamilyStore, FamilyStoreReader, StoreError, StoreWriteAccess,
};
use crate::db::lifecycle::{SqliteStore, TrackedConnection};
use crate::pins::PinOwner;

/// Schema version of `members.sqlite`. A newer version is refused rather than
/// read; extending the registry means bumping this and migrating forward.
pub const REGISTRY_SCHEMA_VERSION: i64 = 1;
pub const REGISTRY_FILE: &str = "members.sqlite";
/// Consecutive sweeps that must find a missing-root member with no protection
/// of any class before it and its view directory are removed.
pub const MISSING_ROOT_SWEEPS: u32 = 2;
/// How often a reader retries pin-then-verify when the pointer keeps moving.
const PROTECT_ATTEMPTS: usize = 8;

const REGISTRY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS registry_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    gc_epoch INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS members (
    scope TEXT NOT NULL PRIMARY KEY,
    root BLOB,
    view_dir TEXT NOT NULL,
    registered_at_ms INTEGER NOT NULL,
    last_bind_ms INTEGER NOT NULL,
    last_publish_ms INTEGER,
    state TEXT NOT NULL,
    missing_sweeps INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS readers (
    reader_id TEXT NOT NULL PRIMARY KEY,
    label TEXT NOT NULL,
    owner_pid INTEGER NOT NULL,
    owner_start INTEGER NOT NULL,
    registered_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sweep_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_pid INTEGER NOT NULL,
    owner_start INTEGER NOT NULL,
    epoch INTEGER NOT NULL,
    acquired_at_ms INTEGER NOT NULL
);
"#;

#[derive(Debug)]
pub enum RegistryError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Store(StoreError),
    View(super::ViewError),
    InvalidScope(String),
    /// The registry file is from a newer release or is not a registry.
    Incompatible(String),
    /// The member is not registered (any more).
    NotRegistered(String),
    /// A reader could not pin a generation that stayed current long enough to
    /// verify it.
    PointerUnstable {
        scope: String,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "family registry I/O error: {error}"),
            Self::Sqlite(error) => write!(f, "family registry SQLite error: {error}"),
            Self::Store(error) => write!(f, "{error}"),
            Self::View(error) => write!(f, "{error}"),
            Self::InvalidScope(scope) => write!(f, "invalid view scope `{scope}`"),
            Self::Incompatible(message) => write!(f, "incompatible family registry: {message}"),
            Self::NotRegistered(scope) => write!(f, "view `{scope}` is not registered"),
            Self::PointerUnstable { scope } => write!(
                f,
                "the current generation of `{scope}` kept changing while a reader pinned it"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<std::io::Error> for RegistryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<rusqlite::Error> for RegistryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
impl From<StoreError> for RegistryError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<super::ViewError> for RegistryError {
    fn from(error: super::ViewError) -> Self {
        Self::View(error)
    }
}

pub type RegistryResult<T> = Result<T, RegistryError>;

/// `<storage>/views/v2/<scope>`: the view directory of a registered member.
pub fn view_dir(storage: &Path, scope: &str) -> RegistryResult<PathBuf> {
    super::validate_scope_key(scope).map_err(|_| RegistryError::InvalidScope(scope.to_owned()))?;
    Ok(storage.join("views").join("v2").join(scope))
}

/// One registered view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberRecord {
    pub scope: String,
    /// `None` when the root could not be recorded exactly; such a member is
    /// never treated as having a missing root.
    pub root: Option<PathBuf>,
    pub view_dir: PathBuf,
    pub registered_at_ms: u64,
    pub last_bind_ms: u64,
    pub last_publish_ms: Option<u64>,
    pub missing_sweeps: u32,
}

/// One registry reader that holds no view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaderRecord {
    pub reader_id: String,
    pub label: String,
    pub owner: PinOwner,
    pub registered_at_ms: u64,
}

struct RegistryInner {
    storage: PathBuf,
    family: String,
    path: PathBuf,
    connection: Mutex<TrackedConnection>,
}

impl fmt::Debug for RegistryInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FamilyRegistry")
            .field("family", &self.family)
            .field("path", &self.path)
            .finish()
    }
}

fn open_registries() -> &'static Mutex<HashMap<PathBuf, Weak<RegistryInner>>> {
    static REGISTRIES: OnceLock<Mutex<HashMap<PathBuf, Weak<RegistryInner>>>> = OnceLock::new();
    REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A handle on one family's registry. Clones share one SQLite connection per
/// process, like the family stores.
#[derive(Clone, Debug)]
pub struct FamilyRegistry {
    inner: Arc<RegistryInner>,
}

impl FamilyRegistry {
    /// Opens the registry, creating it when absent.
    pub fn open(storage: &Path, family: &str) -> RegistryResult<Self> {
        let dir = family_dir(storage, family)?;
        fs::create_dir_all(&dir)?;
        Self::open_path(storage, family, dir.join(REGISTRY_FILE), true)
    }

    /// Opens an existing registry without creating anything; `None` when the
    /// family has no v2 registry yet.
    pub fn open_existing(storage: &Path, family: &str) -> RegistryResult<Option<Self>> {
        let path = family_dir(storage, family)?.join(REGISTRY_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        Self::open_path(storage, family, path, false).map(Some)
    }

    fn open_path(
        storage: &Path,
        family: &str,
        path: PathBuf,
        create: bool,
    ) -> RegistryResult<Self> {
        let mut registries = lock(open_registries());
        registries.retain(|_, registry| registry.strong_count() > 0);
        if let Some(inner) = registries.get(&path).and_then(Weak::upgrade) {
            return Ok(Self { inner });
        }
        let connection = if create {
            TrackedConnection::open(&path, SqliteStore::BlobStore)?
        } else {
            TrackedConnection::open_path_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
                SqliteStore::BlobStore,
            )?
        };
        connection.busy_timeout(Duration::from_millis(crate::blob_store::BUSY_TIMEOUT_MS))?;
        let inner = Arc::new(RegistryInner {
            storage: storage.to_path_buf(),
            family: family.to_owned(),
            path: path.clone(),
            connection: Mutex::new(connection),
        });
        {
            let mut connection = lock(&inner.connection);
            if create {
                crate::blob_store::retry_while_busy(
                    Duration::from_millis(crate::blob_store::BUSY_TIMEOUT_MS),
                    || connection.pragma_update(None, "journal_mode", "WAL"),
                )?;
                connection.pragma_update(None, "synchronous", "FULL")?;
                let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                tx.execute_batch(REGISTRY_SCHEMA)?;
                tx.execute(
                    "INSERT OR IGNORE INTO registry_meta (singleton, schema_version, gc_epoch)
                     VALUES (1, ?1, 0)",
                    params![REGISTRY_SCHEMA_VERSION],
                )?;
                check_schema(&tx)?;
                tx.commit()?;
            } else {
                check_schema(&connection)?;
            }
        }
        registries.insert(path, Arc::downgrade(&inner));
        Ok(Self { inner })
    }

    pub fn family(&self) -> &str {
        &self.inner.family
    }

    pub fn storage(&self) -> &Path {
        &self.inner.storage
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Registers (or re-binds) a view in one IMMEDIATE transaction. This must
    /// be the first durable step of any view work; the returned registration
    /// is the only way to obtain write access to the family's stores.
    pub fn register_view(&self, scope: &str, root: &Path) -> RegistryResult<ViewRegistration> {
        let view_dir = view_dir(&self.inner.storage, scope)?;
        let now = now_ms();
        let mut connection = lock(&self.inner.connection);
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO members
                 (scope, root, view_dir, registered_at_ms, last_bind_ms, state, missing_sweeps)
             VALUES (?1, ?2, ?3, ?4, ?4, 'active', 0)
             ON CONFLICT(scope) DO UPDATE SET
                 root = excluded.root,
                 view_dir = excluded.view_dir,
                 last_bind_ms = excluded.last_bind_ms,
                 state = 'active',
                 missing_sweeps = 0",
            params![scope, encode_root(root), path_text(&view_dir), now as i64],
        )?;
        tx.commit()?;
        drop(connection);
        fs::create_dir_all(&view_dir)?;
        Ok(ViewRegistration {
            registry: self.clone(),
            scope: scope.to_owned(),
            root: root.to_path_buf(),
            view_dir,
        })
    }

    /// Registers a reader that holds no view. The row is removed when the
    /// registration is dropped; a crashed reader's row is reclaimed by the
    /// next sweep once its owner process is gone.
    pub fn register_reader(&self, label: &str) -> RegistryResult<ReaderRegistration> {
        let owner = current_owner();
        let reader_id = format!(
            "{}-{}-{}",
            owner.pid,
            owner.start_time,
            READER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        lock(&self.inner.connection).execute(
            "INSERT INTO readers (reader_id, label, owner_pid, owner_start, registered_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                reader_id,
                label,
                i64::from(owner.pid),
                owner.start_time as i64,
                now_ms() as i64
            ],
        )?;
        Ok(ReaderRegistration {
            registry: self.clone(),
            reader_id,
            label: label.to_owned(),
        })
    }

    pub fn members(&self) -> RegistryResult<Vec<MemberRecord>> {
        read_members(&lock(&self.inner.connection))
    }

    pub fn member(&self, scope: &str) -> RegistryResult<Option<MemberRecord>> {
        Ok(read_members(&lock(&self.inner.connection))?
            .into_iter()
            .find(|member| member.scope == scope))
    }

    pub fn readers(&self) -> RegistryResult<Vec<ReaderRecord>> {
        let connection = lock(&self.inner.connection);
        let mut statement = connection.prepare(
            "SELECT reader_id, label, owner_pid, owner_start, registered_at_ms
             FROM readers ORDER BY reader_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ReaderRecord {
                reader_id: row.get(0)?,
                label: row.get(1)?,
                owner: PinOwner {
                    pid: row.get::<_, i64>(2)? as u32,
                    start_time: row.get::<_, i64>(3)?.max(0) as u64,
                },
                registered_at_ms: row.get::<_, i64>(4)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn gc_epoch(&self) -> RegistryResult<u64> {
        let epoch: i64 = lock(&self.inner.connection).query_row(
            "SELECT gc_epoch FROM registry_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(epoch.max(0) as u64)
    }

    /// Records a successful publication of `scope`.
    pub fn note_publish(&self, scope: &str) -> RegistryResult<()> {
        lock(&self.inner.connection).execute(
            "UPDATE members SET last_publish_ms = ?2 WHERE scope = ?1",
            params![scope, now_ms() as i64],
        )?;
        Ok(())
    }

    /// Claims the family's one-sweeper lease and bumps the GC epoch in the same
    /// transaction. `None` means a live sweeper holds the lease. A lease whose
    /// owner process is gone is taken over.
    pub(crate) fn begin_sweep(&self) -> RegistryResult<Option<SweepLease>> {
        let owner = current_owner();
        let mut connection = lock(&self.inner.connection);
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let holder: Option<(i64, i64)> = tx
            .query_row(
                "SELECT owner_pid, owner_start FROM sweep_lease WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((pid, start)) = holder {
            let holder = PinOwner {
                pid: pid as u32,
                start_time: start.max(0) as u64,
            };
            // A live holder, including another thread of this process, keeps
            // the lease; only a dead owner's lease is taken over.
            if crate::pins::owner_is_live(&holder) {
                return Ok(None);
            }
        }
        tx.execute(
            "UPDATE registry_meta SET gc_epoch = gc_epoch + 1 WHERE singleton = 1",
            [],
        )?;
        let epoch: i64 = tx.query_row(
            "SELECT gc_epoch FROM registry_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO sweep_lease (singleton, owner_pid, owner_start, epoch, acquired_at_ms)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(singleton) DO UPDATE SET owner_pid = excluded.owner_pid,
                 owner_start = excluded.owner_start, epoch = excluded.epoch,
                 acquired_at_ms = excluded.acquired_at_ms",
            params![
                i64::from(owner.pid),
                owner.start_time as i64,
                epoch,
                now_ms() as i64
            ],
        )?;
        tx.commit()?;
        Ok(Some(SweepLease {
            registry: self.clone(),
            epoch: epoch.max(0) as u64,
            owner,
            released: false,
        }))
    }

    /// Runs `body` inside one IMMEDIATE transaction on the registry. This is
    /// the handoff barrier: registration takes the same write lock, so no view
    /// can (re)register between the body's checks and its effects.
    pub(crate) fn with_barrier<T>(
        &self,
        body: impl FnOnce(&rusqlite::Transaction<'_>) -> RegistryResult<T>,
    ) -> RegistryResult<T> {
        let mut connection = lock(&self.inner.connection);
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = body(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Checks that `scope` is registered, inside the handoff barrier.
    pub(crate) fn confirm_member(&self, scope: &str) -> RegistryResult<bool> {
        self.with_barrier(|tx| {
            Ok(tx
                .query_row(
                    "SELECT 1 FROM members WHERE scope = ?1",
                    params![scope],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
    }

    /// Removes reader rows whose owner process is gone.
    pub(crate) fn reclaim_dead_readers(&self) -> RegistryResult<usize> {
        let dead = self
            .readers()?
            .into_iter()
            .filter(|reader| !crate::pins::owner_is_live(&reader.owner))
            .map(|reader| reader.reader_id)
            .collect::<Vec<_>>();
        let connection = lock(&self.inner.connection);
        for reader_id in &dead {
            connection.execute(
                "DELETE FROM readers WHERE reader_id = ?1",
                params![reader_id],
            )?;
        }
        Ok(dead.len())
    }
}

/// The one-sweeper lease of a family, holding the sweep's epoch `S`.
#[derive(Debug)]
pub struct SweepLease {
    registry: FamilyRegistry,
    epoch: u64,
    owner: PinOwner,
    released: bool,
}

impl SweepLease {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let _ = lock(&self.registry.inner.connection).execute(
            "DELETE FROM sweep_lease WHERE singleton = 1 AND owner_pid = ?1 AND owner_start = ?2",
            params![i64::from(self.owner.pid), self.owner.start_time as i64],
        );
    }
}

impl Drop for SweepLease {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// A registered view: the capability to write the family's stores and to
/// publish into its own view directory.
#[derive(Clone, Debug)]
pub struct ViewRegistration {
    registry: FamilyRegistry,
    scope: String,
    root: PathBuf,
    view_dir: PathBuf,
}

impl ViewRegistration {
    pub fn registry(&self) -> &FamilyRegistry {
        &self.registry
    }

    pub fn family(&self) -> &str {
        self.registry.family()
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn view_dir(&self) -> &Path {
        &self.view_dir
    }

    pub fn write_access(&self) -> StoreWriteAccess {
        StoreWriteAccess::for_registered_view(self.registry.storage(), self.registry.family())
    }

    pub fn open_store(&self, plane: FamilyPlane) -> RegistryResult<FamilyStore> {
        Ok(FamilyStore::open(&self.write_access(), plane)?)
    }

    /// The view's own pointer and manifests in `views/v2/<scope>`.
    pub fn view_store(&self) -> RegistryResult<super::ViewStore> {
        Ok(super::ViewStore::open_dir(self.view_dir.clone())?)
    }
}

static READER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A registered reader that holds no view. It can list the family's members,
/// read their stores and pin their current generations, and nothing else.
#[derive(Debug)]
pub struct ReaderRegistration {
    registry: FamilyRegistry,
    reader_id: String,
    label: String,
}

impl ReaderRegistration {
    pub fn reader_id(&self) -> &str {
        &self.reader_id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn family(&self) -> &str {
        self.registry.family()
    }

    pub fn members(&self) -> RegistryResult<Vec<MemberRecord>> {
        self.registry.members()
    }

    /// A read-only handle on one family store, or `None` if it was never
    /// created. Readers never create stores.
    pub fn open_store(&self, plane: FamilyPlane) -> RegistryResult<Option<FamilyStoreReader>> {
        Ok(FamilyStoreReader::open_existing(
            self.registry.storage(),
            self.registry.family(),
            plane,
        )?)
    }

    /// Pins the member's current generation with a read marker, then verifies
    /// that the pointer still names it and the member is still registered. A
    /// generation the pointer moved away from in between is released and the
    /// pin retried. `Ok(None)` means the member has no view directory or no
    /// published generation; nothing is created in that case.
    pub fn protect_current(&self, scope: &str) -> RegistryResult<Option<ProtectedGeneration>> {
        let Some(member) = self.registry.member(scope)? else {
            return Err(RegistryError::NotRegistered(scope.to_owned()));
        };
        for _ in 0..PROTECT_ATTEMPTS {
            let Some(store) = super::ViewStore::existing_dir(member.view_dir.clone()) else {
                return Ok(None);
            };
            let Some(generation) = store.current_generation()? else {
                return Ok(None);
            };
            let marker = crate::root_cache::ReadMarker::create(&member.view_dir, &generation)?;
            let still_current = store.current_generation()?.as_deref() == Some(generation.as_str());
            // Confirm membership under the registry's write lock: a missing-root
            // deregistration holds that lock while it checks for markers, so
            // either it saw this marker, or this check sees the member gone.
            let still_registered = self.registry.confirm_member(scope)?;
            if still_current && still_registered {
                return Ok(Some(ProtectedGeneration {
                    scope: scope.to_owned(),
                    view_dir: member.view_dir.clone(),
                    generation,
                    marker,
                }));
            }
            drop(marker);
            if !still_registered {
                return Err(RegistryError::NotRegistered(scope.to_owned()));
            }
        }
        Err(RegistryError::PointerUnstable {
            scope: scope.to_owned(),
        })
    }
}

impl Drop for ReaderRegistration {
    fn drop(&mut self) {
        let _ = lock(&self.registry.inner.connection).execute(
            "DELETE FROM readers WHERE reader_id = ?1",
            params![self.reader_id],
        );
    }
}

/// A generation protected by a read marker in its view directory. It stays
/// protected (from both the family sweep and the view's own generation sweep)
/// until this value is dropped.
#[derive(Debug)]
pub struct ProtectedGeneration {
    scope: String,
    view_dir: PathBuf,
    generation: String,
    marker: crate::root_cache::ReadMarker,
}

impl ProtectedGeneration {
    pub fn scope(&self) -> &str {
        &self.scope
    }

    pub fn view_dir(&self) -> &Path {
        &self.view_dir
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn marker_path(&self) -> &Path {
        self.marker.path()
    }

    /// Keeps a long-lived (resident) marker fresh for cross-host readers.
    pub fn touch_if_due(&self) -> std::io::Result<()> {
        self.marker.touch_if_due()
    }
}

pub(crate) fn current_owner() -> PinOwner {
    let pid = std::process::id();
    PinOwner {
        pid,
        start_time: crate::root_cache::process_start_time_ms(pid).unwrap_or_else(now_ms),
    }
}

pub(crate) fn read_members(connection: &rusqlite::Connection) -> RegistryResult<Vec<MemberRecord>> {
    let mut statement = connection.prepare(
        "SELECT scope, root, view_dir, registered_at_ms, last_bind_ms, last_publish_ms,
                missing_sweeps
         FROM members ORDER BY scope",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(MemberRecord {
            scope: row.get(0)?,
            root: row.get::<_, Option<Vec<u8>>>(1)?.and_then(decode_root),
            view_dir: PathBuf::from(row.get::<_, String>(2)?),
            registered_at_ms: row.get::<_, i64>(3)?.max(0) as u64,
            last_bind_ms: row.get::<_, i64>(4)?.max(0) as u64,
            last_publish_ms: row
                .get::<_, Option<i64>>(5)?
                .map(|value| value.max(0) as u64),
            missing_sweeps: row.get::<_, i64>(6)?.max(0) as u32,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn check_schema(connection: &rusqlite::Connection) -> RegistryResult<()> {
    let version: Option<i64> = connection
        .query_row(
            "SELECT schema_version FROM registry_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match version {
        Some(REGISTRY_SCHEMA_VERSION) => Ok(()),
        Some(other) => Err(RegistryError::Incompatible(format!(
            "schema {other}, this release reads schema {REGISTRY_SCHEMA_VERSION}"
        ))),
        None => Err(RegistryError::Incompatible(
            "no registry metadata row".to_string(),
        )),
    }
}

/// Roots are stored as exact OS bytes where the platform allows it. A root that
/// cannot be stored exactly is stored as NULL, which the sweep treats as
/// "never missing" rather than risk removing a live checkout's view.
fn encode_root(root: &Path) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(root.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        root.to_str().map(|text| text.as_bytes().to_vec())
    }
}

fn decode_root(bytes: Vec<u8>) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn now_ms() -> u64 {
    crate::pins::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_idempotent_and_readers_leave_on_drop() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), "family").unwrap();
        let root = storage.path().join("checkout");
        registry.register_view("scope-a", &root).unwrap();
        registry.register_view("scope-a", &root).unwrap();
        let members = registry.members().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].root.as_deref(), Some(root.as_path()));

        let reader = registry.register_reader("parent-folder").unwrap();
        assert_eq!(registry.readers().unwrap().len(), 1);
        assert!(
            registry.members().unwrap().len() == 1,
            "a reader is not a member"
        );
        drop(reader);
        assert!(registry.readers().unwrap().is_empty());
    }

    #[test]
    fn a_reader_does_not_create_a_registry_or_a_view() {
        let storage = tempfile::tempdir().unwrap();
        assert!(FamilyRegistry::open_existing(storage.path(), "family")
            .unwrap()
            .is_none());
        let registry = FamilyRegistry::open(storage.path(), "family").unwrap();
        registry
            .register_view("scope-a", &storage.path().join("root"))
            .unwrap();
        let reader = registry.register_reader("parent-folder").unwrap();
        let view = view_dir(storage.path(), "scope-a").unwrap();
        fs::remove_dir_all(&view).unwrap();
        assert!(reader.protect_current("scope-a").unwrap().is_none());
        assert!(
            !view.exists(),
            "protecting an absent view must not create it"
        );
    }

    #[test]
    fn only_one_sweeper_holds_the_lease_and_each_sweep_bumps_the_epoch() {
        let storage = tempfile::tempdir().unwrap();
        let registry = FamilyRegistry::open(storage.path(), "family").unwrap();
        let first = registry.begin_sweep().unwrap().unwrap();
        assert_eq!(first.epoch(), 1);
        first.release();
        let second = registry.begin_sweep().unwrap().unwrap();
        assert_eq!(second.epoch(), 2);
        assert_eq!(registry.gc_epoch().unwrap(), 2);
    }
}
