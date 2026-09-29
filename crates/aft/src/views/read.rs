//! The common reader for an already selected, pinned publication.

use std::path::PathBuf;
use std::sync::Arc;

use crate::callgraph_store::{ReadonlyCallGraphStore, Result};
use crate::pins::QueryPin;

/// Open exactly the selected generation. The caller decides whether its snapshot
/// is acceptable; opening a reader must never replace a pinned generation with a
/// newer pointer or silently fall back to the mutable legacy store.
pub(crate) fn open_published_callgraph(
    project_root: PathBuf,
    family: String,
    view_dir: PathBuf,
    generation: &str,
    pin: Option<Arc<QueryPin>>,
) -> Result<ReadonlyCallGraphStore> {
    ReadonlyCallGraphStore::open_manifest_view(project_root, family, view_dir, generation, pin)
}

/// Watcher edits do not change HEAD. Refuse to project an older published plane
/// when any requested tracked source still has a different content key.
pub(crate) fn callgraph_paths_match(
    manifest: &super::Manifest,
    root: &std::path::Path,
    paths: &[PathBuf],
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
            let current = crate::blob_store::CallgraphKey::for_current(&source, language)
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
