//! Integrity pin for the ONNX Runtime library AFT loads at run time.
//!
//! AFT does not link ONNX Runtime: it loads the shared library by path
//! (`dlopen` / `LoadLibrary`) once semantic search or the reranker first needs
//! it. The usual path is the runtime AFT's installer downloaded into a
//! user-writable folder (`<storage>/onnxruntime/<version>/`).
//!
//! On macOS the supervised module binary is signed with hardened runtime and
//! the `com.apple.security.cs.disable-library-validation` entitlement. Hardened
//! runtime is what keeps other processes of the same user from attaching a
//! debugger and reading the module's launch secret; the entitlement is needed
//! because Microsoft's library carries no Team ID and library validation would
//! refuse it. With library validation off, the loader no longer checks who
//! signed the file, so without this pin any same-user process could replace
//! the downloaded library and run its code inside AFT.
//!
//! The rule enforced here, on every platform, before any load:
//! - a library whose sha256 is on [`PINNED_ORT_LIBRARIES`] is accepted;
//! - otherwise the library is accepted only when `ORT_DYLIB_PATH` named it in
//!   the environment this process was spawned with, and the path is outside
//!   AFT's own managed runtime folder. That is the operator's explicit choice
//!   (a Homebrew or distro runtime), accepted unpinned and logged once. A value
//!   AFT exported itself after resolving its managed download never counts:
//!   the spawn-time value is captured before AFT sets anything;
//! - anything else is refused with `onnx runtime library hash mismatch at
//!   <path>` and is never handed to the loader.
//!
//! With no path at all the library would be found by bare name on the loader
//! search path, where there is no file to hash first. The supervised module
//! (`aft --subc`), the only mode that holds a launch secret, refuses that;
//! a standalone process keeps it, logged once as unpinned, so users relying on
//! a system-installed runtime keep working.
//!
//! Known limit: the file is hashed, then loaded by path. A process that can
//! write the folder and wins the race between the two could still swap it.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, OnceLock};

use sha2::{Digest, Sha256};

use crate::semantic_index::ONNX_RUNTIME_MISSING_PREFIX;

/// Environment variable naming the ONNX Runtime library to load.
pub const ORT_DYLIB_PATH_ENV: &str = "ORT_DYLIB_PATH";

/// One ONNX Runtime library build AFT's installer downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedOrtLibrary {
    /// ONNX Runtime release version.
    pub version: &'static str,
    /// Microsoft's release asset the library comes from.
    pub asset: &'static str,
    /// Path of the library inside that asset.
    pub library: &'static str,
    /// Lowercase hex sha256 of the library file's bytes (symlinks followed).
    pub sha256: &'static str,
}

/// The ONNX Runtime libraries AFT accepts without an operator override.
///
/// Each entry is the main library inside the release asset that
/// `packages/aft-bridge/src/onnx-runtime.ts` downloads (`ORT_PLATFORM_MAP`,
/// `ORT_VERSION`), fetched from
/// `https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/<asset>`
/// and hashed after extraction. On Linux `lib/libonnxruntime.so` is a symlink
/// chain ending at `libonnxruntime.so.1.24.4`; the hash is of the file it
/// resolves to, which is what the installer copies and what a load maps.
/// The installer checks the same hashes (`librarySha256`) before it publishes
/// a download. Bumping `ORT_VERSION` there means adding the new builds here.
///
/// Every entry is accepted on every platform: a library built for another
/// platform cannot load anyway, and all of them are genuine Microsoft builds.
pub const PINNED_ORT_LIBRARIES: &[PinnedOrtLibrary] = &[
    PinnedOrtLibrary {
        version: "1.24.4",
        asset: "onnxruntime-osx-arm64-1.24.4.tgz",
        library: "onnxruntime-osx-arm64-1.24.4/lib/libonnxruntime.dylib",
        sha256: "872533f130f1839a5bc01788ddb4f75c83a189763441ba1178788ed965449289",
    },
    PinnedOrtLibrary {
        version: "1.24.4",
        asset: "onnxruntime-linux-x64-1.24.4.tgz",
        library: "onnxruntime-linux-x64-1.24.4/lib/libonnxruntime.so",
        sha256: "d132535d051344ff5c64c9c200004150559049a81ed330eb4422c1962fb6b7e4",
    },
    PinnedOrtLibrary {
        version: "1.24.4",
        asset: "onnxruntime-linux-aarch64-1.24.4.tgz",
        library: "onnxruntime-linux-aarch64-1.24.4/lib/libonnxruntime.so",
        sha256: "52b2a0e75e79468404284fec38ad5ee1a7a996232274f5e1b84f3e793fb07554",
    },
    PinnedOrtLibrary {
        version: "1.24.4",
        asset: "onnxruntime-win-x64-1.24.4.zip",
        library: "onnxruntime-win-x64-1.24.4/lib/onnxruntime.dll",
        sha256: "b95efb2113b603bbbf3f191061c5516a871ed546893c820e4f3b7b6c358dbf2a",
    },
    PinnedOrtLibrary {
        version: "1.24.4",
        asset: "onnxruntime-win-arm64-1.24.4.zip",
        library: "onnxruntime-win-arm64-1.24.4/lib/onnxruntime.dll",
        sha256: "724d81ac50b11bfaa01ab3ce01b99fb6734a4b762259a8be4395ca662ce99fe6",
    },
];

/// Why a library load was allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrtLoadAuthorization {
    /// The file's sha256 is a pinned build.
    Pinned { path: PathBuf, sha256: String },
    /// Not a pinned build, but named by `ORT_DYLIB_PATH` at spawn and outside
    /// AFT's managed runtime folder.
    OperatorUnpinned { path: PathBuf, sha256: String },
    /// No path configured; the loader finds the library by bare name.
    /// Only allowed outside the supervised module.
    LoaderSearchUnpinned,
}

/// The value `ORT_DYLIB_PATH` had when this process was spawned.
///
/// The first capture wins for the life of the process, so a value AFT exports
/// later (after resolving its own managed download) can never be mistaken for
/// the operator's. Empty values count as unset, matching how the resolver
/// treats them.
pub(crate) struct SpawnEnvValue(OnceLock<Option<OsString>>);

impl SpawnEnvValue {
    pub(crate) const fn new() -> Self {
        Self(OnceLock::new())
    }

    pub(crate) fn capture_with(&self, lookup: impl FnOnce() -> Option<OsString>) -> Option<&OsStr> {
        self.0
            .get_or_init(|| lookup().filter(|value| !value.is_empty()))
            .as_deref()
    }
}

static SPAWN_ORT_DYLIB_PATH: SpawnEnvValue = SpawnEnvValue::new();

/// Record `ORT_DYLIB_PATH` as this process received it. Call it before
/// anything in the process may set the variable: `main` does so first thing,
/// and the managed-runtime resolver does so again right before it exports its
/// own path (the call is idempotent, the first one wins).
pub fn capture_spawn_ort_dylib_path() -> Option<&'static OsStr> {
    SPAWN_ORT_DYLIB_PATH.capture_with(|| std::env::var_os(ORT_DYLIB_PATH_ENV))
}

static PINNED_RUNTIME_REQUIRED: AtomicBool = AtomicBool::new(false);

/// Refuse bare-name loads from the loader search path for the rest of the
/// process. Called when AFT runs as the supervised module (`--subc`), which
/// holds a launch secret; a standalone process keeps the bare-name fallback.
pub fn require_pinned_onnx_runtime() {
    PINNED_RUNTIME_REQUIRED.store(true, Ordering::SeqCst);
}

fn pinned_runtime_required() -> bool {
    PINNED_RUNTIME_REQUIRED.load(Ordering::SeqCst)
}

static MANAGED_STORAGE_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Remember a storage directory whose `onnxruntime/` folder holds AFT's own
/// managed runtime, so a spawn-time `ORT_DYLIB_PATH` pointing into it is held
/// to the pin instead of being treated as an operator choice. The bridge
/// passes the managed path it resolved at spawn; that is AFT's own download,
/// not the operator's decision.
pub fn register_managed_storage_dir(storage_dir: &Path) {
    let mut dirs = MANAGED_STORAGE_DIRS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !dirs.iter().any(|known| known == storage_dir) {
        dirs.push(storage_dir.to_path_buf());
    }
}

/// `<storage>/onnxruntime` for every storage directory this process knows,
/// plus the default one (the bridge's storage when no override is set).
fn managed_runtime_roots() -> Vec<PathBuf> {
    let mut storage_dirs = MANAGED_STORAGE_DIRS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let default = crate::bash_background::storage_dir(None);
    if !storage_dirs.contains(&default) {
        storage_dirs.push(default);
    }
    storage_dirs
        .into_iter()
        .map(|dir| dir.join("onnxruntime"))
        .collect()
}

/// Inputs to one authorization decision. Production builds it from process
/// state in [`authorize_onnx_runtime_load`]; tests build it directly.
pub(crate) struct OrtPinPolicy<'a> {
    /// `ORT_DYLIB_PATH` as captured at spawn.
    pub spawn_value: Option<&'a OsStr>,
    /// Refuse bare-name loads (supervised module).
    pub require_pinned: bool,
    /// Folders holding AFT's managed runtime (`<storage>/onnxruntime`).
    pub managed_roots: &'a [PathBuf],
    /// Accepted library hashes, lowercase hex.
    pub pinned_sha256: &'a [&'a str],
}

/// Decide whether the library at `candidate` (or the bare-name load when
/// `None`) may be loaded. Reads and hashes the file; never loads it.
pub(crate) fn authorize_with(
    policy: &OrtPinPolicy<'_>,
    candidate: Option<&OsStr>,
) -> Result<OrtLoadAuthorization, String> {
    let Some(candidate) = candidate.filter(|value| !value.is_empty()) else {
        if policy.require_pinned {
            // Keeps the missing-runtime prefix: the semantic build treats that
            // prefix as "wait for the managed download and retry", and the
            // download is exactly what makes a pinned runtime available here.
            return Err(format!(
                "{ONNX_RUNTIME_MISSING_PREFIX} No managed ONNX Runtime was found and \
                 ORT_DYLIB_PATH was not set when this module was spawned. The supervised \
                 module loads only a pinned runtime or one named by ORT_DYLIB_PATH at \
                 spawn, never a bare library name from the loader search path. Run \
                 `npx @cortexkit/aft doctor --fix` to install it."
            ));
        }
        return Ok(OrtLoadAuthorization::LoaderSearchUnpinned);
    };

    let path = Path::new(candidate);
    let sha256 = sha256_file(path).map_err(|error| {
        format!(
            "{ONNX_RUNTIME_MISSING_PREFIX} cannot read '{}' to check its sha256 before \
             loading it: {error}. Run `npx @cortexkit/aft doctor --fix` to install it.",
            path.display()
        )
    })?;

    if policy
        .pinned_sha256
        .iter()
        .any(|pinned| pinned.eq_ignore_ascii_case(&sha256))
    {
        return Ok(OrtLoadAuthorization::Pinned {
            path: path.to_path_buf(),
            sha256,
        });
    }

    let named_at_spawn = policy.spawn_value == Some(candidate);
    if named_at_spawn && !is_inside_any(path, policy.managed_roots) {
        return Ok(OrtLoadAuthorization::OperatorUnpinned {
            path: path.to_path_buf(),
            sha256,
        });
    }

    // Worded so `semantic_index::is_onnx_runtime_unavailable` does not match it
    // (that check looks for "dlopen", "not found" and similar): a match would
    // report a missing runtime and suggest `doctor --fix`, which cannot help
    // while a tampered file still sits in the folder.
    Err(format!(
        "onnx runtime library hash mismatch at {}: sha256 {sha256} is not a pinned ONNX \
         Runtime build, so semantic search is unavailable. Remove the file and reinstall \
         with `npx @cortexkit/aft doctor --clear`, or name a runtime you trust with \
         ORT_DYLIB_PATH in the environment AFT is started with.",
        path.display()
    ))
}

/// True when `path` lies inside one of `roots`, comparing both the spelling
/// given and the symlink-resolved forms, so neither a symlink into the managed
/// folder nor a symlinked storage directory escapes the check.
fn is_inside_any(path: &Path, roots: &[PathBuf]) -> bool {
    let resolved_path = std::fs::canonicalize(path).ok();
    roots.iter().any(|root| {
        if path.starts_with(root) {
            return true;
        }
        let Some(resolved_path) = resolved_path.as_deref() else {
            return false;
        };
        resolved_path.starts_with(root)
            || std::fs::canonicalize(root)
                .map(|resolved_root| resolved_path.starts_with(resolved_root))
                .unwrap_or(false)
    })
}

/// Lowercase hex sha256 of a file's bytes, following symlinks.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn pinned_hashes() -> Vec<&'static str> {
    PINNED_ORT_LIBRARIES
        .iter()
        .map(|library| library.sha256)
        .collect()
}

/// Authorize the next ONNX Runtime load against this process's state. Every
/// path that loads the library calls this first, and loads only on `Ok`.
pub fn authorize_onnx_runtime_load(
    candidate: Option<&OsStr>,
) -> Result<OrtLoadAuthorization, String> {
    let roots = managed_runtime_roots();
    let pinned = pinned_hashes();
    let policy = OrtPinPolicy {
        spawn_value: capture_spawn_ort_dylib_path(),
        require_pinned: pinned_runtime_required(),
        managed_roots: &roots,
        pinned_sha256: &pinned,
    };
    let authorization = authorize_with(&policy, candidate)?;
    log_unpinned_once(&authorization);
    Ok(authorization)
}

fn log_unpinned_once(authorization: &OrtLoadAuthorization) {
    static OPERATOR: Once = Once::new();
    static LOADER_SEARCH: Once = Once::new();
    match authorization {
        OrtLoadAuthorization::Pinned { .. } => {}
        OrtLoadAuthorization::OperatorUnpinned { path, sha256 } => OPERATOR.call_once(|| {
            crate::slog_warn!(
                "ONNX Runtime at {} (sha256 {sha256}) is not a pinned build; accepted unpinned \
                 because ORT_DYLIB_PATH named it when this process was spawned",
                path.display()
            );
        }),
        OrtLoadAuthorization::LoaderSearchUnpinned => LOADER_SEARCH.call_once(|| {
            crate::slog_warn!(
                "no ONNX Runtime path configured; loading it by name from the loader search \
                 path, unpinned"
            );
        }),
    }
}

/// Authorize `path`, then hand it to `load`. A refused library never reaches
/// the loader. `load` is `ort::init_from` in production.
pub(crate) fn load_authorized_with<T>(
    authorize: impl FnOnce(Option<&OsStr>) -> Result<OrtLoadAuthorization, String>,
    path: &Path,
    load: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    authorize(Some(path.as_os_str()))?;
    load(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn write_library(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write fake library");
        path
    }

    fn hash_of(path: &Path) -> String {
        sha256_file(path).expect("hash fake library")
    }

    fn policy<'a>(
        spawn_value: Option<&'a OsStr>,
        require_pinned: bool,
        managed_roots: &'a [PathBuf],
        pinned_sha256: &'a [&'a str],
    ) -> OrtPinPolicy<'a> {
        OrtPinPolicy {
            spawn_value,
            require_pinned,
            managed_roots,
            pinned_sha256,
        }
    }

    #[test]
    fn sha256_file_matches_a_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_library(dir.path(), "lib", b"abc");
        assert_eq!(
            hash_of(&path),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pinned_library_is_authorized_and_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_library(dir.path(), "libonnxruntime.dylib", b"genuine build");
        let hash = hash_of(&path);
        let pinned = [hash.as_str()];
        let roots = [dir.path().to_path_buf()];
        // Inside the managed folder and not named by ORT_DYLIB_PATH at spawn,
        // so only a matching pinned hash can admit it.
        let policy = policy(None, true, &roots, &pinned);

        let loads = Cell::new(0);
        let result = load_authorized_with(
            |candidate| authorize_with(&policy, candidate),
            &path,
            |loaded| {
                assert_eq!(loaded, path.as_path());
                loads.set(loads.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(loads.get(), 1);
        assert_eq!(
            authorize_with(&policy, Some(path.as_os_str())),
            Ok(OrtLoadAuthorization::Pinned {
                path: path.clone(),
                sha256: hash
            })
        );
    }

    #[test]
    fn mismatched_library_is_refused_and_never_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_library(dir.path(), "libonnxruntime.dylib", b"tampered build");
        let pinned = ["872533f130f1839a5bc01788ddb4f75c83a189763441ba1178788ed965449289"];
        let roots = [dir.path().to_path_buf()];
        let policy = policy(None, false, &roots, &pinned);

        let loads = Cell::new(0);
        let error = load_authorized_with(
            |candidate| authorize_with(&policy, candidate),
            &path,
            |_| {
                loads.set(loads.get() + 1);
                Ok(())
            },
        )
        .expect_err("a library off the pinned list must be refused");
        assert_eq!(
            loads.get(),
            0,
            "a refused library must never reach the loader"
        );
        assert!(
            error.starts_with(&format!(
                "onnx runtime library hash mismatch at {}",
                path.display()
            )),
            "{error}"
        );
        assert!(
            !crate::semantic_index::is_onnx_runtime_unavailable(&error),
            "a tampered library is not a missing runtime: {error}"
        );
    }

    #[test]
    fn spawn_time_operator_path_outside_the_managed_folder_is_accepted_unpinned() {
        let operator_dir = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let path = write_library(operator_dir.path(), "libonnxruntime.dylib", b"brew build");
        let roots = [storage.path().join("onnxruntime")];
        let pinned = ["872533f130f1839a5bc01788ddb4f75c83a189763441ba1178788ed965449289"];
        for require_pinned in [false, true] {
            let policy = policy(Some(path.as_os_str()), require_pinned, &roots, &pinned);
            assert_eq!(
                authorize_with(&policy, Some(path.as_os_str())),
                Ok(OrtLoadAuthorization::OperatorUnpinned {
                    path: path.clone(),
                    sha256: hash_of(&path)
                }),
                "require_pinned={require_pinned}"
            );
        }
    }

    #[test]
    fn spawn_time_path_inside_the_managed_folder_must_still_match_the_pin() {
        let storage = tempfile::tempdir().unwrap();
        let version_dir = storage.path().join("onnxruntime").join("1.24.4");
        std::fs::create_dir_all(&version_dir).unwrap();
        let path = write_library(&version_dir, "libonnxruntime.dylib", b"swapped build");
        let roots = [storage.path().join("onnxruntime")];
        let pinned = ["872533f130f1839a5bc01788ddb4f75c83a189763441ba1178788ed965449289"];
        // The bridge passes its managed path at spawn; that is AFT's own
        // download, so a bad hash is refused whether or not the process is
        // the supervised module (require_pinned true or false).
        for require_pinned in [false, true] {
            let policy = policy(Some(path.as_os_str()), require_pinned, &roots, &pinned);
            let error = authorize_with(&policy, Some(path.as_os_str()))
                .expect_err("a managed library with a bad hash must be refused");
            assert!(
                error.starts_with("onnx runtime library hash mismatch at"),
                "require_pinned={require_pinned}: {error}"
            );
        }
    }

    #[test]
    fn managed_folder_reached_through_a_symlink_is_still_managed() {
        #[cfg(unix)]
        {
            let storage = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let version_dir = storage.path().join("onnxruntime").join("1.24.4");
            std::fs::create_dir_all(&version_dir).unwrap();
            let real = write_library(&version_dir, "libonnxruntime.dylib", b"swapped build");
            let link = outside.path().join("libonnxruntime.dylib");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let roots = [storage.path().join("onnxruntime")];
            let pinned: [&str; 0] = [];
            let policy = policy(Some(link.as_os_str()), false, &roots, &pinned);
            assert!(authorize_with(&policy, Some(link.as_os_str())).is_err());
        }
    }

    #[test]
    fn a_value_aft_exported_itself_is_not_an_operator_override() {
        // The process starts with no ORT_DYLIB_PATH; AFT later resolves its
        // managed download and exports it. The spawn capture must still read
        // "unset", so the exported path is held to the pin.
        let env: RefCell<Option<OsString>> = RefCell::new(None);
        let spawn = SpawnEnvValue::new();
        assert_eq!(spawn.capture_with(|| env.borrow().clone()), None);

        let dir = tempfile::tempdir().unwrap();
        let path = write_library(dir.path(), "libonnxruntime.dylib", b"swapped build");
        *env.borrow_mut() = Some(path.as_os_str().to_os_string());

        let spawn_value = spawn.capture_with(|| env.borrow().clone());
        assert_eq!(spawn_value, None, "the first capture must win");
        let roots: [PathBuf; 0] = [];
        let pinned = ["872533f130f1839a5bc01788ddb4f75c83a189763441ba1178788ed965449289"];
        let policy = policy(spawn_value, false, &roots, &pinned);
        let error = authorize_with(&policy, Some(path.as_os_str()))
            .expect_err("AFT's own export is not the operator's choice");
        assert!(error.starts_with("onnx runtime library hash mismatch at"));
    }

    #[test]
    fn spawn_capture_keeps_a_value_present_at_spawn() {
        let spawn = SpawnEnvValue::new();
        let value = OsString::from("/opt/onnxruntime/libonnxruntime.so");
        assert_eq!(
            spawn.capture_with(|| Some(value.clone())),
            Some(value.as_os_str())
        );
        assert_eq!(
            spawn.capture_with(|| None),
            Some(value.as_os_str()),
            "a later unset must not erase the spawn value"
        );
        let empty = SpawnEnvValue::new();
        assert_eq!(empty.capture_with(|| Some(OsString::new())), None);
    }

    #[test]
    fn bare_name_load_is_refused_under_the_supervised_module() {
        let roots: [PathBuf; 0] = [];
        let pinned: [&str; 0] = [];
        let error = authorize_with(&policy(None, true, &roots, &pinned), None)
            .expect_err("the supervised module must not load by bare name");
        assert!(
            error.starts_with(ONNX_RUNTIME_MISSING_PREFIX),
            "the wait-for-download flow keys on this prefix: {error}"
        );
    }

    #[test]
    fn bare_name_load_is_allowed_standalone() {
        let roots: [PathBuf; 0] = [];
        let pinned: [&str; 0] = [];
        assert_eq!(
            authorize_with(&policy(None, false, &roots, &pinned), None),
            Ok(OrtLoadAuthorization::LoaderSearchUnpinned)
        );
        assert_eq!(
            authorize_with(&policy(None, false, &roots, &pinned), Some(OsStr::new(""))),
            Ok(OrtLoadAuthorization::LoaderSearchUnpinned),
            "an empty ORT_DYLIB_PATH is unset"
        );
    }

    #[test]
    fn unreadable_library_reports_a_missing_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join("libonnxruntime.dylib");
        let roots: [PathBuf; 0] = [];
        let pinned: [&str; 0] = [];
        let error = authorize_with(
            &policy(Some(path.as_os_str()), false, &roots, &pinned),
            Some(path.as_os_str()),
        )
        .expect_err("a file that cannot be hashed cannot be loaded");
        assert!(error.starts_with(ONNX_RUNTIME_MISSING_PREFIX), "{error}");
        assert!(error.contains("doctor --fix"), "{error}");
    }

    #[test]
    fn installer_pins_the_same_library_hashes() {
        // The bridge installer (onnx-runtime.ts, `librarySha256`) checks a
        // download against its own copy of PINNED_ORT_LIBRARIES before
        // publishing it. If the two lists drift apart, the installer publishes
        // builds AFT then refuses, or refuses builds AFT would load.
        let installer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/aft-bridge/src/onnx-runtime.ts");
        let Ok(source) = std::fs::read_to_string(&installer) else {
            eprintln!("skipping: {} not present", installer.display());
            return;
        };
        let installer_pins: Vec<&str> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("librarySha256: \""))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        let mut rust_pins: Vec<&str> = PINNED_ORT_LIBRARIES
            .iter()
            .map(|library| library.sha256)
            .collect();
        let mut installer_sorted = installer_pins.clone();
        rust_pins.sort_unstable();
        installer_sorted.sort_unstable();
        assert_eq!(installer_sorted, rust_pins);
    }

    #[test]
    fn pinned_list_covers_every_platform_the_installer_downloads() {
        for asset in [
            "onnxruntime-osx-arm64-1.24.4.tgz",
            "onnxruntime-linux-x64-1.24.4.tgz",
            "onnxruntime-linux-aarch64-1.24.4.tgz",
            "onnxruntime-win-x64-1.24.4.zip",
            "onnxruntime-win-arm64-1.24.4.zip",
        ] {
            let entry = PINNED_ORT_LIBRARIES
                .iter()
                .find(|library| library.asset == asset)
                .unwrap_or_else(|| panic!("no pin for {asset}"));
            assert_eq!(entry.sha256.len(), 64, "{asset}");
            assert!(
                entry
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{asset}: pins are lowercase hex"
            );
        }
    }
}
