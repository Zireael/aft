//! Model2Vec model download and cache management.
//!
//! Downloads model2vec model files from HuggingFace Hub using `hf-hub`,
//! following the same cache layout pattern as the fastembed backend.
//! Supports automatic download on first use and local path override.

use std::path::{Path, PathBuf};

use crate::model2vec_catalog::lookup_model;
use crate::slog_info;

/// Required files for a model2vec model.
const MODEL2VEC_FILES: &[&str] = &["config.json", "tokenizer.json", "model.safetensors"];

/// Resolve the cache directory for model2vec models.
///
/// Checks `MODEL2VEC_CACHE_DIR` env var first, then falls back to
/// `~/.cache/model2vec`.
fn model2vec_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MODEL2VEC_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".cache").join("model2vec")
}

/// Resolve model files for a model2vec model.
///
/// If `model_path` is `Some`, validates and returns files from that directory.
/// If `model_path` is `None` and `model_name` is a known catalog model,
/// downloads from HuggingFace Hub (or returns cached files).
///
/// Returns the path to the directory containing the three required files.
pub fn resolve_model2vec_files(
    model_name: Option<&str>,
    model_path: Option<&Path>,
) -> Result<PathBuf, String> {
    // If explicit path is given, validate it
    if let Some(path) = model_path {
        validate_model_dir(path)?;
        return Ok(path.to_path_buf());
    }

    // Otherwise, resolve by model name
    let name = model_name.ok_or_else(|| {
        "model2vec backend requires either model_path or model name (e.g., \"minishlab/potion-code-16M\")"
            .to_string()
    })?;

    // Check if it's a known catalog model
    if lookup_model(name).is_some() {
        let cache_dir = model2vec_cache_dir();
        let model_dir = cache_dir.join(model_name_to_dir_name(name));

        // Check if already cached
        if is_model_cached(&model_dir) {
            return Ok(model_dir);
        }

        // Download from HuggingFace
        return download_model(name, &model_dir);
    }

    // Unknown model — try to treat as a local path
    let path = Path::new(name);
    if path.exists() && path.is_dir() {
        validate_model_dir(path)?;
        return Ok(path.to_path_buf());
    }

    Err(format!(
        "unknown model2vec model '{}'. Use a known model from the catalog \
         (e.g., minishlab/potion-code-16M) or provide a local model_path.",
        name
    ))
}

/// Convert a model name like "minishlab/potion-code-16M" to a directory name
/// like "minishlab--potion-code-16M".
fn model_name_to_dir_name(name: &str) -> String {
    name.replace('/', "--")
}

/// Check if a model directory has all required files cached.
fn is_model_cached(model_dir: &Path) -> bool {
    MODEL2VEC_FILES
        .iter()
        .all(|f| model_dir.join(f).is_file())
}

/// Validate that a directory contains all required model2vec files.
fn validate_model_dir(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!(
            "model_path is not a directory: {}",
            path.display()
        ));
    }

    let mut missing = Vec::new();
    for file in MODEL2VEC_FILES {
        if !path.join(file).is_file() {
            missing.push(*file);
        }
    }

    if !missing.is_empty() {
        return Err(format!(
            "model directory {} is missing required files: {}",
            path.display(),
            missing.join(", ")
        ));
    }

    Ok(())
}

/// Download a model from HuggingFace Hub.
fn download_model(repo_id: &str, target_dir: &Path) -> Result<PathBuf, String> {
    use hf_hub::api::sync::ApiBuilder;

    slog_info!(
        "downloading model2vec model {} to {}",
        repo_id,
        target_dir.display()
    );

    // Create target directory
    std::fs::create_dir_all(target_dir)
        .map_err(|e| format!("failed to create model directory {}: {e}", target_dir.display()))?;

    let cache_dir = model2vec_cache_dir();
    let api = ApiBuilder::new()
        .with_progress(false)
        .with_cache_dir(cache_dir)
        .build()
        .map_err(|e| format!("failed to init hf-hub api: {e}"))?;

    let repo = api.model(repo_id.to_string());

    for file in MODEL2VEC_FILES {
        let cached_path = repo
            .get(file)
            .map_err(|e| format!("failed to download {file} from {repo_id}: {e}"))?;

        // Copy from HF cache to our target directory
        let target_path = target_dir.join(file);
        std::fs::copy(&cached_path, &target_path).map_err(|e| {
            format!(
                "failed to copy {file} to {}: {e}",
                target_path.display()
            )
        })?;
    }

    slog_info!("model2vec model {} downloaded successfully", repo_id);
    Ok(target_dir.to_path_buf())
}

/// Get the total size of a model directory in bytes.
pub fn model_dir_size(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for file in MODEL2VEC_FILES {
        let file_path = path.join(file);
        let metadata = std::fs::metadata(&file_path).map_err(|e| {
            format!(
                "failed to read metadata for {}: {e}",
                file_path.display()
            )
        })?;
        total += metadata.len();
    }
    Ok(total)
}

/// List all cached model2vec models.
pub fn list_cached_models() -> Vec<(String, PathBuf, u64)> {
    let cache_dir = model2vec_cache_dir();
    let mut models = Vec::new();

    if !cache_dir.is_dir() {
        return models;
    }

    for entry in std::fs::read_dir(&cache_dir).into_iter().flatten() {
        let entry = entry.ok();
        if let Some(entry) = entry {
            let path = entry.path();
            if path.is_dir() && is_model_cached(&path) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().replace("--", "/"))
                    .unwrap_or_default();
                let size = model_dir_size(&path).unwrap_or(0);
                models.push((name, path, size));
            }
        }
    }

    models.sort_by(|a, b| a.0.cmp(&b.0));
    models
}

/// Remove a cached model from disk.
pub fn remove_cached_model(repo_id: &str) -> Result<(), String> {
    let cache_dir = model2vec_cache_dir();
    let model_dir = cache_dir.join(model_name_to_dir_name(repo_id));

    if !model_dir.is_dir() {
        return Err(format!("no cached model found for {}", repo_id));
    }

    std::fs::remove_dir_all(&model_dir)
        .map_err(|e| format!("failed to remove {}: {e}", model_dir.display()))?;

    slog_info!("removed cached model2vec model {}", repo_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_test_model_dir(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();

        // config.json
        let mut f = std::fs::File::create(dir.join("config.json")).unwrap();
        writeln!(f, "{{\"normalize\": true, \"hidden_size\": 256}}").unwrap();

        // tokenizer.json
        let mut f = std::fs::File::create(dir.join("tokenizer.json")).unwrap();
        writeln!(f, "{{\"version\": \"1.0\"}}").unwrap();

        // model.safetensors — minimal valid file (header + one tensor)
        let mut f = std::fs::File::create(dir.join("model.safetensors")).unwrap();
        writeln!(f, "fake safetensors content").unwrap();
    }

    #[test]
    fn validate_model_dir_success() {
        let dir = std::env::temp_dir().join("test_model2vec_validate");
        create_test_model_dir(&dir);
        assert!(validate_model_dir(&dir).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_model_dir_missing_files() {
        let dir = std::env::temp_dir().join("test_model2vec_missing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("config.json")).unwrap();
        // Missing tokenizer.json and model.safetensors

        let err = validate_model_dir(&dir).unwrap_err();
        assert!(err.contains("tokenizer.json"));
        assert!(err.contains("model.safetensors"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_model_dir_not_a_dir() {
        let dir = std::env::temp_dir().join("test_model2vec_notdir");
        std::fs::File::create(&dir).unwrap();
        let err = validate_model_dir(&dir).unwrap_err();
        assert!(err.contains("not a directory"));
        std::fs::remove_file(&dir).ok();
    }

    #[test]
    fn is_model_cached_true() {
        let dir = std::env::temp_dir().join("test_model2vec_cached");
        create_test_model_dir(&dir);
        assert!(is_model_cached(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_model_cached_false() {
        let dir = std::env::temp_dir().join("test_model2vec_notcached");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_model_cached(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn model_name_to_dir_name_conversion() {
        assert_eq!(
            model_name_to_dir_name("minishlab/potion-code-16M"),
            "minishlab--potion-code-16M"
        );
        assert_eq!(
            model_name_to_dir_name("minishlab/potion-base-2M"),
            "minishlab--potion-base-2M"
        );
    }

    #[test]
    fn model_dir_size_calculation() {
        let dir = std::env::temp_dir().join("test_model2vec_size");
        create_test_model_dir(&dir);
        let size = model_dir_size(&dir).unwrap();
        assert!(size > 0, "model dir size should be > 0");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_model_path_valid() {
        let dir = std::env::temp_dir().join("test_model2vec_resolve_path");
        create_test_model_dir(&dir);
        let result = resolve_model2vec_files(None, Some(&dir));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_model_no_name_no_path() {
        let result = resolve_model2vec_files(None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("requires either"));
    }

    #[test]
    fn resolve_model_unknown_name() {
        let result = resolve_model2vec_files(Some("unknown/model"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown model2vec model"));
    }

    #[test]
    fn resolve_model_local_path_as_name() {
        let dir = std::env::temp_dir().join("test_model2vec_local_name");
        create_test_model_dir(&dir);
        let result = resolve_model2vec_files(Some(dir.to_str().unwrap()), None);
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
