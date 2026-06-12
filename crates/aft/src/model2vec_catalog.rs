//! Model2Vec model catalog with compatibility metadata.
//!
//! Defines the set of known model2vec models, their expected dimensions,
//! required files, and task categories. Used by download, validation,
//! and health-check logic.

use serde::{Deserialize, Serialize};

/// Task category for a model2vec model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Model2VecTask {
    /// General-purpose text embedding (English)
    General,
    /// Code-specific embedding
    Code,
    /// Multilingual embedding
    Multilingual,
    /// Retrieval-specific embedding
    Retrieval,
}

impl std::fmt::Display for Model2VecTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::General => write!(f, "general"),
            Self::Code => write!(f, "code"),
            Self::Multilingual => write!(f, "multilingual"),
            Self::Retrieval => write!(f, "retrieval"),
        }
    }
}

/// Metadata for a known model2vec model.
#[derive(Debug, Clone)]
pub struct Model2VecModelInfo {
    /// HuggingFace repo ID (e.g., "minishlab/potion-code-16M")
    pub repo_id: &'static str,
    /// Human-readable name
    pub display_name: &'static str,
    /// Embedding dimensions
    pub dimensions: usize,
    /// Approximate parameter count
    pub param_count: &'static str,
    /// Model task category
    pub task: Model2VecTask,
    /// Required files in the model directory
    pub required_files: &'static [&'static str],
    /// Description
    pub description: &'static str,
}

/// All three files required by model2vec-rs.
const MODEL2VEC_REQUIRED_FILES: &[&str] = &["config.json", "tokenizer.json", "model.safetensors"];

/// Catalog of all official model2vec models from MinishLab.
///
/// Models are sorted by parameter count (descending) for discoverability.
/// Source: <https://huggingface.co/collections/minishlab/potion>
pub const MODEL2VEC_CATALOG: &[Model2VecModelInfo] = &[
    Model2VecModelInfo {
        repo_id: "minishlab/potion-multilingual-128M",
        display_name: "Potion Multilingual 128M",
        dimensions: 256,
        param_count: "128M",
        task: Model2VecTask::Multilingual,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "Multilingual general-purpose embedding distilled from BAAI/bge-m3",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-base-32M",
        display_name: "Potion Base 32M",
        dimensions: 256,
        param_count: "32M",
        task: Model2VecTask::General,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "English general-purpose embedding distilled from BAAI/bge-base-en-v1.5",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-retrieval-32M",
        display_name: "Potion Retrieval 32M",
        dimensions: 256,
        param_count: "32M",
        task: Model2VecTask::Retrieval,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "English retrieval-focused embedding distilled from BAAI/bge-base-en-v1.5",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-code-16M",
        display_name: "Potion Code 16M",
        dimensions: 256,
        param_count: "16M",
        task: Model2VecTask::Code,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "Code embedding distilled from nomic-ai/CodeRankEmbed, optimized for code retrieval",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-base-8M",
        display_name: "Potion Base 8M",
        dimensions: 256,
        param_count: "8M",
        task: Model2VecTask::General,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "Compact English general-purpose embedding distilled from BAAI/bge-base-en-v1.5",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-base-4M",
        display_name: "Potion Base 4M",
        dimensions: 256,
        param_count: "4M",
        task: Model2VecTask::General,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "Minimal English general-purpose embedding distilled from BAAI/bge-base-en-v1.5",
    },
    Model2VecModelInfo {
        repo_id: "minishlab/potion-base-2M",
        display_name: "Potion Base 2M",
        dimensions: 256,
        param_count: "2M",
        task: Model2VecTask::General,
        required_files: MODEL2VEC_REQUIRED_FILES,
        description: "Tiny English general-purpose embedding distilled from BAAI/bge-base-en-v1.5",
    },
];

/// Look up a model by HuggingFace repo ID (e.g., "minishlab/potion-code-16M").
///
/// Returns `None` if the model is not in the catalog.
pub fn lookup_model(repo_id: &str) -> Option<&'static Model2VecModelInfo> {
    MODEL2VEC_CATALOG
        .iter()
        .find(|m| m.repo_id == repo_id)
}

/// Check if a string looks like a known model2vec model repo ID.
///
/// Returns `true` if the string starts with "minishlab/" and matches a catalog entry.
pub fn is_known_model(repo_id: &str) -> bool {
    lookup_model(repo_id).is_some()
}

/// Get all models matching a task category.
pub fn models_by_task(task: Model2VecTask) -> Vec<&'static Model2VecModelInfo> {
    MODEL2VEC_CATALOG
        .iter()
        .filter(|m| m.task == task)
        .collect()
}

/// Get the recommended default model for a given task.
///
/// Returns the smallest model for the task (fastest inference).
pub fn default_model_for_task(task: Model2VecTask) -> Option<&'static Model2VecModelInfo> {
    models_by_task(task).last().copied() // Models sorted by param count descending, so last is smallest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_models() {
        assert!(MODEL2VEC_CATALOG.len() >= 7, "expected at least 7 models");
    }

    #[test]
    fn all_models_have_required_files() {
        for model in MODEL2VEC_CATALOG {
            assert!(
                model.required_files.contains(&"config.json"),
                "{} missing config.json",
                model.repo_id
            );
            assert!(
                model.required_files.contains(&"tokenizer.json"),
                "{} missing tokenizer.json",
                model.repo_id
            );
            assert!(
                model.required_files.contains(&"model.safetensors"),
                "{} missing model.safetensors",
                model.repo_id
            );
        }
    }

    #[test]
    fn all_models_have_256_dimensions() {
        for model in MODEL2VEC_CATALOG {
            assert_eq!(
                model.dimensions, 256,
                "{} should have 256 dimensions",
                model.repo_id
            );
        }
    }

    #[test]
    fn lookup_known_model() {
        let model = lookup_model("minishlab/potion-code-16M").unwrap();
        assert_eq!(model.dimensions, 256);
        assert_eq!(model.task, Model2VecTask::Code);
        assert_eq!(model.param_count, "16M");
    }

    #[test]
    fn lookup_unknown_model_returns_none() {
        assert!(lookup_model("unknown/model-xyz").is_none());
    }

    #[test]
    fn is_known_model_works() {
        assert!(is_known_model("minishlab/potion-code-16M"));
        assert!(is_known_model("minishlab/potion-base-2M"));
        assert!(!is_known_model("some-other/model"));
    }

    #[test]
    fn models_by_task_code() {
        let code_models = models_by_task(Model2VecTask::Code);
        assert_eq!(code_models.len(), 1);
        assert_eq!(code_models[0].repo_id, "minishlab/potion-code-16M");
    }

    #[test]
    fn models_by_task_general() {
        let general = models_by_task(Model2VecTask::General);
        assert!(general.len() >= 4, "expected at least 4 general models");
    }

    #[test]
    fn default_model_for_task_works() {
        let default_code = default_model_for_task(Model2VecTask::Code).unwrap();
        assert_eq!(default_code.repo_id, "minishlab/potion-code-16M");

        let default_general = default_model_for_task(Model2VecTask::General).unwrap();
        // Should be the smallest general model (2M)
        assert_eq!(default_general.param_count, "2M");
    }

    #[test]
    fn task_display() {
        assert_eq!(Model2VecTask::General.to_string(), "general");
        assert_eq!(Model2VecTask::Code.to_string(), "code");
        assert_eq!(Model2VecTask::Multilingual.to_string(), "multilingual");
        assert_eq!(Model2VecTask::Retrieval.to_string(), "retrieval");
    }

    #[test]
    fn task_serde_roundtrip() {
        let task = Model2VecTask::Code;
        let json = serde_json::to_string(&task).unwrap();
        assert_eq!(json, "\"code\"");
        let decoded: Model2VecTask = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, task);
    }
}
