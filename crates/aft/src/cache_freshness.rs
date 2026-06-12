use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CONTENT_HASH_SIZE_CAP: u64 = 4 * 1024 * 1024;

#[cfg(debug_assertions)]
static STRICT_VERIFY_FILE_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFreshness {
    pub mtime: SystemTime,
    pub size: u64,
    pub content_hash: blake3::Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessVerdict {
    HotFresh,
    ContentFresh {
        new_mtime: SystemTime,
        new_size: u64,
    },
    Stale,
    Deleted,
}

pub fn hash_bytes(bytes: &[u8]) -> blake3::Hash {
    blake3::hash(bytes)
}

pub fn hash_file_if_small(path: &Path, size: u64) -> std::io::Result<Option<blake3::Hash>> {
    if size > CONTENT_HASH_SIZE_CAP {
        return Ok(None);
    }
    fs::read(path).map(|bytes| Some(hash_bytes(&bytes)))
}

pub fn zero_hash() -> blake3::Hash {
    blake3::Hash::from_bytes([0u8; 32])
}

pub fn collect(path: &Path) -> std::io::Result<FileFreshness> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
    let size = metadata.len();
    let content_hash = hash_file_if_small(path, size)?.unwrap_or_else(zero_hash);
    Ok(FileFreshness {
        mtime,
        size,
        content_hash,
    })
}

pub fn verify_file(path: &Path, cached: &FileFreshness) -> FreshnessVerdict {
    verify_file_inner(path, cached, false)
}

pub fn verify_file_strict(path: &Path, cached: &FileFreshness) -> FreshnessVerdict {
    #[cfg(debug_assertions)]
    STRICT_VERIFY_FILE_CALLS.fetch_add(1, Ordering::Relaxed);
    verify_file_inner(path, cached, true)
}

/// Verify semantic cache file freshness in a private bounded Rayon pool.
///
/// Do not use the global pool here: load-time strict verification can hash every
/// indexed file, and the semantic load/build already runs beside the bridge's
/// single dispatch thread. Match the half-cores/cap-8 policy used by the search
/// and callgraph cold-build pools.
#[allow(dead_code)]
pub(crate) fn verify_files_strict_bounded<K: Send>(
    files: Vec<(K, PathBuf, FileFreshness)>,
) -> Vec<(K, PathBuf, FreshnessVerdict)> {
    fn verify_one<K>(
        (key, path, cached): (K, PathBuf, FileFreshness),
    ) -> (K, PathBuf, FreshnessVerdict) {
        let verdict = verify_file_strict(&path, &cached);
        (key, path, verdict)
    }

    if files.len() <= 1 {
        return files.into_iter().map(verify_one::<K>).collect();
    }

    match rayon::ThreadPoolBuilder::new()
        .num_threads(strict_verify_pool_size())
        .thread_name(|index| format!("aft-semantic-verify-{index}"))
        .build()
    {
        Ok(pool) => pool.install(|| files.into_par_iter().map(verify_one::<K>).collect()),
        Err(_) => files.into_iter().map(verify_one::<K>).collect(),
    }
}

fn strict_verify_pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1)
        .div_ceil(2)
        .clamp(1, 8)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn reset_verify_file_strict_count_for_debug() {
    STRICT_VERIFY_FILE_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn verify_file_strict_count_for_debug() -> usize {
    STRICT_VERIFY_FILE_CALLS.load(Ordering::Relaxed)
}

fn verify_file_inner(
    path: &Path,
    cached: &FileFreshness,
    hash_matching_metadata: bool,
) -> FreshnessVerdict {
    let Ok(metadata) = fs::metadata(path) else {
        return FreshnessVerdict::Deleted;
    };
    let new_size = metadata.len();
    let new_mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
    if new_size == cached.size && new_mtime == cached.mtime {
        if hash_matching_metadata {
            if new_size > CONTENT_HASH_SIZE_CAP || cached.content_hash == zero_hash() {
                return FreshnessVerdict::Stale;
            }
            return match hash_file_if_small(path, new_size) {
                Ok(Some(hash)) if hash == cached.content_hash => FreshnessVerdict::HotFresh,
                _ => FreshnessVerdict::Stale,
            };
        }
        return FreshnessVerdict::HotFresh;
    }
    if new_size != cached.size || new_size > CONTENT_HASH_SIZE_CAP {
        return FreshnessVerdict::Stale;
    }
    match hash_file_if_small(path, new_size) {
        Ok(Some(hash)) if hash == cached.content_hash => FreshnessVerdict::ContentFresh {
            new_mtime,
            new_size,
        },
        _ => FreshnessVerdict::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn hot_fresh_when_mtime_size_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write(&path, b"same");
        let fresh = collect(&path).unwrap();
        assert_eq!(verify_file(&path, &fresh), FreshnessVerdict::HotFresh);
    }

    #[test]
    fn strict_hashes_small_file_when_metadata_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let original_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        write(&path, b"alpha");
        filetime::set_file_mtime(&path, original_mtime).unwrap();
        let fresh = collect(&path).unwrap();

        assert_eq!(
            verify_file_strict(&path, &fresh),
            FreshnessVerdict::HotFresh
        );

        write(&path, b"bravo");
        filetime::set_file_mtime(&path, original_mtime).unwrap();

        assert_eq!(verify_file(&path, &fresh), FreshnessVerdict::HotFresh);
        assert_eq!(verify_file_strict(&path, &fresh), FreshnessVerdict::Stale);
    }

    #[test]
    fn strict_stale_when_large_file_hash_was_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let original_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        let file = fs::File::create(&path).unwrap();
        file.set_len(CONTENT_HASH_SIZE_CAP + 1).unwrap();
        filetime::set_file_mtime(&path, original_mtime).unwrap();
        let fresh = collect(&path).unwrap();

        assert_eq!(fresh.size, CONTENT_HASH_SIZE_CAP + 1);
        assert_eq!(fresh.content_hash, zero_hash());
        assert_eq!(verify_file(&path, &fresh), FreshnessVerdict::HotFresh);
        assert_eq!(verify_file_strict(&path, &fresh), FreshnessVerdict::Stale);
    }

    #[test]
    fn content_fresh_when_only_mtime_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write(&path, b"same");
        let fresh = collect(&path).unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"").unwrap();
        file.sync_all().unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        assert!(matches!(
            verify_file(&path, &fresh),
            FreshnessVerdict::ContentFresh { .. }
        ));
    }

    #[test]
    fn stale_when_size_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write(&path, b"same");
        let fresh = collect(&path).unwrap();
        write(&path, b"different");
        assert_eq!(verify_file(&path, &fresh), FreshnessVerdict::Stale);
    }

    #[test]
    fn deleted_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write(&path, b"same");
        let fresh = collect(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(verify_file(&path, &fresh), FreshnessVerdict::Deleted);
    }

    #[test]
    fn over_cap_hash_is_not_computed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        fs::write(&path, vec![0u8; CONTENT_HASH_SIZE_CAP as usize + 1]).unwrap();
        assert!(hash_file_if_small(&path, CONTENT_HASH_SIZE_CAP + 1)
            .unwrap()
            .is_none());
    }
}
