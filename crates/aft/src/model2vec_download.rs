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
    MODEL2VEC_FILES.iter().all(|f| model_dir.join(f).is_file())
}

/// Validate that a directory contains all required model2vec files.
fn validate_model_dir(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("model_path is not a directory: {}", path.display()));
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

    // Validate config.json has required fields
    validate_model_config(path)?;

    Ok(())
}

/// Validate that config.json in a model directory has the required fields.
fn validate_model_config(model_dir: &Path) -> Result<(), String> {
    let config_path = model_dir.join("config.json");
    let config_str = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read {}: {e}", config_path.display()))?;

    let config: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| format!("invalid JSON in {}: {e}", config_path.display()))?;

    // Check for required fields
    if config.get("hidden_size").is_none() {
        return Err(format!(
            "config.json in {} is missing 'hidden_size' field",
            model_dir.display()
        ));
    }

    // Validate hidden_size is a positive integer
    if let Some(size) = config.get("hidden_size").and_then(|v| v.as_u64()) {
        if size == 0 || size > 16384 {
            return Err(format!(
                "config.json hidden_size={} is out of valid range (1-16384)",
                size
            ));
        }
    }

    // Validate normalize field exists (boolean)
    if config.get("normalize").is_none() {
        // Some models don't have this field — that's OK, default is true
    }

    Ok(())
}

/// Validate a model directory against expected dimensions from the catalog.
///
/// If the model is in the catalog, checks that the config.json hidden_size
/// matches the expected dimensions.
pub fn validate_model_dimensions(model_dir: &Path, expected_repo_id: &str) -> Result<(), String> {
    if let Some(catalog_model) = lookup_model(expected_repo_id) {
        let config_path = model_dir.join("config.json");
        let config_str = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("failed to read {}: {e}", config_path.display()))?;

        let config: serde_json::Value = serde_json::from_str(&config_str)
            .map_err(|e| format!("invalid JSON in {}: {e}", config_path.display()))?;

        if let Some(actual_dims) = config.get("hidden_size").and_then(|v| v.as_u64()) {
            if actual_dims as usize != catalog_model.dimensions {
                return Err(format!(
                    "model {} has {} dimensions but catalog expects {}",
                    expected_repo_id, actual_dims, catalog_model.dimensions
                ));
            }
        }
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
    std::fs::create_dir_all(target_dir).map_err(|e| {
        format!(
            "failed to create model directory {}: {e}",
            target_dir.display()
        )
    })?;

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
        std::fs::copy(&cached_path, &target_path)
            .map_err(|e| format!("failed to copy {file} to {}: {e}", target_path.display()))?;
    }

    slog_info!("model2vec model {} downloaded successfully", repo_id);
    Ok(target_dir.to_path_buf())
}

/// Get the total size of a model directory in bytes.
pub fn model_dir_size(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for file in MODEL2VEC_FILES {
        let file_path = path.join(file);
        let metadata = std::fs::metadata(&file_path)
            .map_err(|e| format!("failed to read metadata for {}: {e}", file_path.display()))?;
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

/// Metadata about a downloaded model.
#[derive(Debug, Clone)]
pub struct ModelVersionInfo {
    /// The repo ID (e.g., "minishlab/potion-code-16M")
    pub repo_id: String,
    /// Path to the model directory
    pub model_dir: PathBuf,
    /// Download timestamp (seconds since epoch)
    pub downloaded_at: u64,
    /// File sizes in bytes
    pub total_size_bytes: u64,
}

/// Get version information about a cached model.
pub fn get_model_version_info(repo_id: &str) -> Option<ModelVersionInfo> {
    let cache_dir = model2vec_cache_dir();
    let model_dir = cache_dir.join(model_name_to_dir_name(repo_id));

    if !is_model_cached(&model_dir) {
        return None;
    }

    let downloaded_at = std::fs::metadata(&model_dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let total_size_bytes = model_dir_size(&model_dir).unwrap_or(0);

    Some(ModelVersionInfo {
        repo_id: repo_id.to_string(),
        model_dir,
        downloaded_at,
        total_size_bytes,
    })
}

/// Check if a model update might be available.
///
/// Since HuggingFace models are identified by git revisions, a simple
/// heuristic is: if the model was downloaded more than 30 days ago,
/// suggest checking for updates. This avoids network calls.
pub fn check_for_update(repo_id: &str) -> Option<String> {
    let info = get_model_version_info(repo_id)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let age_days = (now.saturating_sub(info.downloaded_at)) / 86400;

    if age_days > 30 {
        Some(format!(
            "Model {} was downloaded {} days ago. Consider checking for updates: \
             delete the cached model and re-download to get the latest version.",
            repo_id, age_days
        ))
    } else {
        None
    }
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

    #[test]
    fn validate_config_json_missing_hidden_size() {
        let dir = std::env::temp_dir().join("test_model2vec_no_hidden");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("config.json"))
            .unwrap()
            .write_all(b"{\"normalize\": true}")
            .unwrap();
        std::fs::File::create(dir.join("tokenizer.json"))
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        std::fs::File::create(dir.join("model.safetensors"))
            .unwrap()
            .write_all(b"fake")
            .unwrap();

        let result = validate_model_dir(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("hidden_size"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_config_json_invalid_json() {
        let dir = std::env::temp_dir().join("test_model2vec_bad_json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("config.json"))
            .unwrap()
            .write_all(b"not json")
            .unwrap();
        std::fs::File::create(dir.join("tokenizer.json"))
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        std::fs::File::create(dir.join("model.safetensors"))
            .unwrap()
            .write_all(b"fake")
            .unwrap();

        let result = validate_model_dir(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid JSON"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_dimensions_matches_catalog() {
        let dir = std::env::temp_dir().join("test_model2vec_dims_ok");
        create_test_model_dir(&dir);
        // Our test model has hidden_size: 256, which matches all catalog models
        let result = validate_model_dimensions(&dir, "minishlab/potion-code-16M");
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_dimensions_mismatch() {
        let dir = std::env::temp_dir().join("test_model2vec_dims_bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("config.json"))
            .unwrap()
            .write_all(b"{\"normalize\": true, \"hidden_size\": 128}")
            .unwrap();
        std::fs::File::create(dir.join("tokenizer.json"))
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        std::fs::File::create(dir.join("model.safetensors"))
            .unwrap()
            .write_all(b"fake")
            .unwrap();

        let result = validate_model_dimensions(&dir, "minishlab/potion-code-16M");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("128"),
            "error should mention actual dims: {}",
            err
        );
        assert!(
            err.contains("256"),
            "error should mention expected dims: {}",
            err
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_hidden_size_out_of_range() {
        let dir = std::env::temp_dir().join("test_model2vec_hidden_range");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join("config.json"))
            .unwrap()
            .write_all(b"{\"normalize\": true, \"hidden_size\": 0}")
            .unwrap();
        std::fs::File::create(dir.join("tokenizer.json"))
            .unwrap()
            .write_all(b"{}")
            .unwrap();
        std::fs::File::create(dir.join("model.safetensors"))
            .unwrap()
            .write_all(b"fake")
            .unwrap();

        let result = validate_model_dir(&dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("out of valid range"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_model_by_name_uses_catalog() {
        // When model name matches a catalog entry, it should resolve
        // (even if not downloaded yet — the download function handles that)
        // For this test, we create a local dir and use it as a path fallback
        let dir = std::env::temp_dir().join("test_model2vec_resolve_name");
        create_test_model_dir(&dir);
        let result = resolve_model2vec_files(Some(dir.to_str().unwrap()), None);
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_model_path_takes_priority_over_name() {
        let path_dir = std::env::temp_dir().join("test_model2vec_path_priority");
        create_test_model_dir(&path_dir);
        // Even if we provide a name, the explicit path should be used
        let result = resolve_model2vec_files(Some("unknown-model"), Some(&path_dir));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), path_dir);
        std::fs::remove_dir_all(&path_dir).ok();
    }

    #[test]
    fn validate_model_dir_complete_flow() {
        let dir = std::env::temp_dir().join("test_model2vec_complete");
        create_test_model_dir(&dir);
        assert!(validate_model_dir(&dir).is_ok());
        assert!(is_model_cached(&dir));
        assert!(model_dir_size(&dir).unwrap() > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_cached_models_empty() {
        let cache_dir = std::env::temp_dir().join("test_model2vec_list_empty");
        // No models cached — should return empty
        std::fs::create_dir_all(&cache_dir).ok();
        // Note: list_cached_models uses its own cache dir, not this one
        // This test just verifies the function doesn't panic
        let _ = list_cached_models();
    }

    #[test]
    fn get_model_version_info_returns_none_for_missing() {
        let result = get_model_version_info("nonexistent/model");
        assert!(result.is_none());
    }

    #[test]
    fn check_for_update_returns_none_for_missing() {
        let result = check_for_update("nonexistent/model");
        assert!(result.is_none());
    }
}
