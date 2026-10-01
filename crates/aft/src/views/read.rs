//! The common reader for an already selected, pinned publication.

use std::path::PathBuf;
use std::sync::Arc;

use crate::callgraph_store::{ReadonlyCallGraphStore, Result};
use crate::pins::QueryPin;

/// Why a view generation published with the call graph off is not served.
pub(crate) const CALLGRAPH_DISABLED: &str = "call graph is disabled (indexes.callgraph=false): this view generation was published without call graph data";

/// Open exactly the selected generation. The caller decides whether its snapshot
/// is acceptable; opening a reader must never replace a pinned generation with a
/// newer pointer or silently fall back to the mutable legacy store.
///
/// A generation published with the call graph off has an empty derived
/// database; serving it would answer "no callers" and "no dead code" for a
/// graph that was never built. Every reader of a view's call graph opens it
/// here, so it is refused here, as unavailable with [`CALLGRAPH_DISABLED`].
/// A manifest that cannot be read is refused too, rather than assumed to
/// carry a call graph.
pub(crate) fn open_published_callgraph(
    project_root: PathBuf,
    family: String,
    view_dir: PathBuf,
    generation: &str,
    pin: Option<Arc<QueryPin>>,
) -> Result<ReadonlyCallGraphStore> {
    if !generation_has_callgraph(&view_dir, generation)? {
        return Err(crate::callgraph_store::CallGraphStoreError::Unavailable(
            CALLGRAPH_DISABLED.to_string(),
        ));
    }
    ReadonlyCallGraphStore::open_manifest_view(project_root, family, view_dir, generation, pin)
}

/// True when the checkout's current view generation (the v1 view under
/// `<storage>/views/<scope>`) exists and was published without call graph
/// data. Read-only: nothing is created when the view is absent.
pub(crate) fn current_generation_lacks_callgraph(
    storage: &std::path::Path,
    root: &std::path::Path,
) -> bool {
    let view_dir = storage
        .join("views")
        .join(crate::path_identity::project_scope_key(root));
    let Some(store) = super::ViewStore::existing_dir(view_dir.clone()) else {
        return false;
    };
    let Ok(Some(generation)) = store.current_generation_read_only() else {
        return false;
    };
    matches!(generation_has_callgraph(&view_dir, &generation), Ok(false))
}

/// Whether `generation` was published with call graph data. Generations are
/// immutable, so the answer is cached per view directory and generation; the
/// manifest is read once, not on every call graph query.
fn generation_has_callgraph(view_dir: &std::path::Path, generation: &str) -> Result<bool> {
    type Cache = std::collections::HashMap<(PathBuf, String), bool>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (view_dir.to_path_buf(), generation.to_owned());
    if let Some(known) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Ok(*known);
    }
    let unavailable = |reason: String| {
        crate::callgraph_store::CallGraphStoreError::Unavailable(format!(
            "view generation {generation} manifest unreadable: {reason}"
        ))
    };
    let store = super::ViewStore::existing_dir(view_dir.to_path_buf())
        .ok_or_else(|| unavailable("view pointer missing".into()))?;
    let manifest = store
        .load_manifest(generation)
        .map_err(|error| unavailable(error.to_string()))?;
    let has = !super::assembly::manifest_lacks_callgraph(&manifest);
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= 1024 {
        cache.clear();
    }
    cache.insert(key, has);
    Ok(has)
}

/// Watcher edits do not change HEAD. Refuse to project an older published plane
/// when any requested tracked source still has a different content key.
///
/// Checkout assembly keys every published callgraph plane with the ruled
/// producer, so the current source must be keyed the same way. Keying it with
/// the legacy producer would never match and would leave every published
/// generation looking stale to inspect.
pub(crate) fn callgraph_paths_match(
    manifest: &super::Manifest,
    root: &std::path::Path,
    paths: &[PathBuf],
) -> super::Result<bool> {
    callgraph_paths_match_with_producer(manifest, root, paths, super::callgraph::PRODUCER)
}

/// Content verification for an opted-in ruled callgraph generation. Legacy
/// payload keys are intentionally incompatible with its dispatch-hint producer.
pub fn callgraph_paths_match_v2(
    manifest: &super::manifest_v2::ManifestV2,
    root: &std::path::Path,
    paths: &[PathBuf],
) -> super::Result<bool> {
    let projected = super::callgraph::project_manifest(manifest)
        .map_err(|error| super::ViewError::InvalidManifest(error.to_string()))?;
    callgraph_paths_match_with_producer(&projected, root, paths, super::callgraph::PRODUCER)
}

fn callgraph_paths_match_with_producer(
    manifest: &super::Manifest,
    root: &std::path::Path,
    paths: &[PathBuf],
    producer: &str,
) -> super::Result<bool> {
    for path in paths {
        let Ok(relative) = path.strip_prefix(root) else {
            return Ok(false);
        };
        let key = super::RelPath::from_os_path(relative)?;
        let language = if super::assembly::is_resolution_input(key.as_bytes()) {
            Some("config".to_string())
        } else {
            crate::parser::detect_language(path)
                .map(|language| format!("{language:?}").to_lowercase())
        };
        let Some(language) = language else { continue };
        let entry = manifest.get(&key);
        let source = match std::fs::read(path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if entry.is_some() {
                    return Ok(false);
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        // Untracked paths are not members of the view; their contributions can
        // still be scanned, but they cannot change this published graph.
        let Some(entry) = entry else { continue };
        if let super::ManifestEntry::Regular { planes, .. } = entry {
            let current = crate::blob_store::CallgraphKey::from_bytes(&source, language, producer)
                .full_key()
                .to_hex();
            if planes.callgraph.as_deref() != Some(current.as_str()) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Opens only the generation protected by the reader's verified marker.
/// No pointer, manifest or plane artifact is created or repaired on this path.
pub fn open_foreign_generation(
    reader: &super::registry::ReaderRegistration,
    scope: &str,
    producers: &super::manifest_v2::Producers,
) -> super::Result<Option<std::sync::Arc<super::snapshot::OpenGeneration>>> {
    let protected = reader.protect_current(scope).map_err(|error| {
        super::ViewError::InvalidManifest(format!("foreign view {scope} unavailable: {error}"))
    })?;
    let Some(protected) = protected else {
        return Ok(None);
    };
    let store =
        super::ViewStore::existing_dir(protected.view_dir().to_path_buf()).ok_or_else(|| {
            super::ViewError::InvalidManifest(format!(
                "foreign view {scope} unavailable: pointer missing"
            ))
        })?;
    let manifest = store.load_manifest_v2(protected.generation())?;
    manifest.ensure_producers(producers)?;
    Ok(Some(std::sync::Arc::new(
        super::snapshot::OpenGeneration::new(
            protected.generation().to_owned(),
            manifest,
            Some(super::snapshot::Residency::Protected(protected)),
        ),
    )))
}

#[cfg(test)]
mod producer_tests {
    use super::*;
    #[test]
    fn ruled_callgraph_content_match_accepts_unchanged_and_rejects_edit() {
        let root = tempfile::tempdir().unwrap();
        let absolute = root.path().join("file.rs");
        let bytes = b"fn target() {}";
        std::fs::write(&absolute, bytes).unwrap();
        let mut entry = super::super::snapshot::LiveEntry::new(
            super::super::snapshot::DiskState::of_bytes(bytes),
            0,
        );
        let attachment = super::super::callgraph::attach(&mut entry, bytes, "rust").unwrap();
        let mut manifest =
            super::super::manifest_v2::ManifestV2::new(super::super::manifest_v2::ManifestHeader {
                producers: super::super::manifest_v2::Producers {
                    trigram: "test".into(),
                    semantic: None,
                    callgraph: super::super::callgraph::PRODUCER.into(),
                },
                head_tree: None,
                ignore_fingerprint: None,
                segment: None,
            });
        manifest
            .insert(
                super::super::RelPath::new(b"file.rs".to_vec()).unwrap(),
                super::super::manifest_v2::EntryV2::regular(
                    crate::blob_store::v2::ContentHash::of(bytes),
                    bytes.len() as u64,
                    super::super::manifest_v2::EntryPlanes {
                        callgraph: Some(super::super::readiness::PlaneState::ready(
                            &attachment.key,
                        )),
                        ..Default::default()
                    },
                ),
            )
            .unwrap();
        assert!(callgraph_paths_match_v2(&manifest, root.path(), &[absolute.clone()]).unwrap());
        std::fs::write(&absolute, b"fn changed() {}").unwrap();
        assert!(!callgraph_paths_match_v2(&manifest, root.path(), &[absolute]).unwrap());
    }
}
