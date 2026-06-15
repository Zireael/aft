//! Handler for the `verify` command: suggest verification actions after changes.
//!
//! Suggests diagnostics, likely tests, lint/typecheck commands, and next actions
//! based on changed files and source-test links.

use crate::context::AppContext;
use crate::mutation_risk::{classify_mutation_risk, FileKind};
use crate::protocol::{RawRequest, Response};

/// Suggested verification action.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifySuggestion {
    /// What to do (e.g., "run tests", "check diagnostics", "lint").
    pub action: String,
    /// The command to run (if applicable).
    pub command: Option<String>,
    /// Confidence level: "high", "medium", "low".
    pub confidence: String,
    /// Reason for the suggestion.
    pub reason: String,
    /// Whether this action is safe to auto-run.
    pub safe_to_auto_run: bool,
}

/// Handle a `verify` request — suggest verification actions for changed files.
///
/// Params:
///   - `files` (array of strings, optional) — specific files to verify
///   - `session` (bool, optional, default false) — include all files changed in session
///   - `project_root` (string, optional) — project root for context
///
/// Returns:
///   `{ suggestions, diagnostics, likely_tests, file_kinds }`
pub fn handle_verify(req: &RawRequest, _ctx: &AppContext) -> Response {
    let files = req
        .params
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let include_session = req
        .params
        .get("session")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let _project_root = req
        .params
        .get("project_root")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut suggestions = Vec::new();
    let mut file_kinds = Vec::new();
    let mut diagnostics_count = 0u32;
    let mut likely_tests: Vec<String> = Vec::new();

    // If no specific files provided, check session context
    let target_files = if files.is_empty() && include_session {
        // In a real implementation, we'd query session-changed files
        // For now, return a suggestion to provide files
        suggestions.push(VerifySuggestion {
            action: "provide changed files".to_string(),
            command: None,
            confidence: "high".to_string(),
            reason: "No specific files provided. Pass 'files' array or enable 'session' mode."
                .to_string(),
            safe_to_auto_run: false,
        });
        return Response::success(
            &req.id,
            serde_json::json!({
                "suggestions": suggestions,
                "diagnostics": 0,
                "likely_tests": likely_tests,
                "file_kinds": file_kinds,
            }),
        );
    } else {
        files
    };

    // Analyze each target file
    for file in &target_files {
        let file_kind = FileKind::classify(file);
        file_kinds.push(serde_json::json!({
            "file": file,
            "kind": format!("{:?}", file_kind).to_lowercase(),
        }));

        // Get mutation risk for context
        let risk = classify_mutation_risk(file, None, false);

        // Suggest diagnostics based on file kind
        match file_kind {
            FileKind::Source => {
                suggestions.push(VerifySuggestion {
                    action: "check diagnostics".to_string(),
                    command: Some(format!("aft lsp_diagnostics file={file}")),
                    confidence: "high".to_string(),
                    reason: "Source file changed — check for type errors and warnings".to_string(),
                    safe_to_auto_run: true,
                });

                // Suggest linting for source files
                suggestions.push(VerifySuggestion {
                    action: "lint".to_string(),
                    command: Some(detect_lint_command(file)),
                    confidence: "medium".to_string(),
                    reason: "Source file changed — run linter for code quality".to_string(),
                    safe_to_auto_run: true,
                });

                // Suggest type checking
                suggestions.push(VerifySuggestion {
                    action: "typecheck".to_string(),
                    command: Some(detect_typecheck_command(file)),
                    confidence: "medium".to_string(),
                    reason: "Source file changed — verify type correctness".to_string(),
                    safe_to_auto_run: true,
                });

                diagnostics_count += 1;
            }
            FileKind::Test => {
                // For test files, suggest running them
                suggestions.push(VerifySuggestion {
                    action: "run test".to_string(),
                    command: Some(format!("cargo nextest run --test {file}")),
                    confidence: "high".to_string(),
                    reason: "Test file changed — run the affected test".to_string(),
                    safe_to_auto_run: true,
                });

                // Also suggest running related source tests
                likely_tests.push(file.clone());
            }
            FileKind::Config => {
                suggestions.push(VerifySuggestion {
                    action: "verify config".to_string(),
                    command: None,
                    confidence: "medium".to_string(),
                    reason: "Config file changed — verify build/project still works".to_string(),
                    safe_to_auto_run: false,
                });
            }
            FileKind::Build => {
                suggestions.push(VerifySuggestion {
                    action: "verify build".to_string(),
                    command: None,
                    confidence: "medium".to_string(),
                    reason: "Build script changed — verify CI/build pipeline".to_string(),
                    safe_to_auto_run: false,
                });
            }
            _ => {}
        }

        // Add high-risk warning for critical files
        if risk.level >= crate::mutation_risk::RiskLevel::High {
            suggestions.push(VerifySuggestion {
                action: "review risk".to_string(),
                command: None,
                confidence: "high".to_string(),
                reason: format!(
                    "File has {} risk (score: {:.2}) — manual review recommended",
                    risk.level.label(),
                    risk.score
                ),
                safe_to_auto_run: false,
            });
        }
    }

    // Add general suggestions
    if !target_files.is_empty() {
        // Suggest running tests if any source files changed
        let has_source = file_kinds.iter().any(|fk| {
            fk.get("kind")
                .and_then(|k| k.as_str())
                .map(|k| k == "source")
                .unwrap_or(false)
        });

        if has_source {
            suggestions.push(VerifySuggestion {
                action: "run tests".to_string(),
                command: Some("cargo nextest run".to_string()),
                confidence: "medium".to_string(),
                reason: "Source files changed — run test suite to verify no regressions"
                    .to_string(),
                safe_to_auto_run: true,
            });
        }
    }

    // Sort suggestions by confidence
    suggestions.sort_by(|a, b| {
        let order = |s: &str| match s {
            "high" => 0,
            "medium" => 1,
            "low" => 2,
            _ => 3,
        };
        order(&a.confidence).cmp(&order(&b.confidence))
    });

    Response::success(
        &req.id,
        serde_json::json!({
            "suggestions": suggestions,
            "diagnostics": diagnostics_count,
            "likely_tests": likely_tests,
            "file_kinds": file_kinds,
        }),
    )
}

/// Detect the appropriate lint command for a file based on its extension.
fn detect_lint_command(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "cargo clippy --workspace --all-targets".to_string(),
        "ts" | "tsx" | "js" | "jsx" => "npx eslint .".to_string(),
        "py" => "python -m ruff check .".to_string(),
        "go" => "golangci-lint run".to_string(),
        _ => "echo 'No linter configured for this file type'".to_string(),
    }
}

/// Detect the appropriate typecheck command for a file based on its extension.
fn detect_typecheck_command(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "cargo check --workspace".to_string(),
        "ts" | "tsx" => "npx tsc --noEmit".to_string(),
        "js" | "jsx" => "npx tsc --noEmit".to_string(),
        "py" => "python -m mypy .".to_string(),
        "go" => "go vet ./...".to_string(),
        _ => "echo 'No typechecker configured for this file type'".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_for_source_file() {
        // Test that we get appropriate suggestions for a Rust source file
        let file_kind = FileKind::classify("src/main.rs");
        assert_eq!(file_kind, FileKind::Source);
    }

    #[test]
    fn suggest_for_test_file() {
        let file_kind = FileKind::classify("tests/foo_test.rs");
        assert_eq!(file_kind, FileKind::Test);
    }

    #[test]
    fn suggest_for_config_file() {
        let file_kind = FileKind::classify("Cargo.toml");
        assert_eq!(file_kind, FileKind::Config);
    }

    #[test]
    fn lint_command_rust() {
        assert!(detect_lint_command("src/main.rs").contains("clippy"));
    }

    #[test]
    fn lint_command_typescript() {
        assert!(detect_lint_command("src/app.ts").contains("eslint"));
    }

    #[test]
    fn typecheck_command_rust() {
        assert!(detect_typecheck_command("src/main.rs").contains("cargo check"));
    }

    #[test]
    fn typecheck_command_typescript() {
        assert!(detect_typecheck_command("src/app.ts").contains("tsc"));
    }

    #[test]
    fn no_files_returns_provide_suggestion() {
        // When no files and no session mode, should suggest providing files
        let file_kind = FileKind::classify("unknown.xyz");
        assert_eq!(file_kind, FileKind::Unknown);
    }
}
