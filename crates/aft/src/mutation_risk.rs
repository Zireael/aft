//! Mutation Risk Classifier.
//!
//! Computes pre-mutation risk based on graph facts, symbol facts, and file classification.
//! Used by exact tools to warn about high-risk edits before they happen.

use crate::ril_indexer::{GraphHealth, RilIndexer};

/// Risk level for a mutation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum RiskLevel {
    /// Safe to proceed without confirmation.
    Low,
    /// Proceed with awareness — reasons provided.
    Medium,
    /// Proceed with caution — reasons provided, may require confirmation.
    High,
    /// Requires explicit confirmation before proceeding.
    Critical,
}

impl RiskLevel {
    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }

    /// Whether this risk level requires explicit confirmation.
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, RiskLevel::Critical)
    }
}

/// A single risk reason with context.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RiskReason {
    /// Short identifier for the risk factor.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
    /// Numeric weight (0.0–1.0) contributing to overall risk.
    pub weight: f64,
}

/// Mutation risk assessment result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MutationRisk {
    /// Overall risk level.
    pub level: RiskLevel,
    /// Composite risk score (0.0–1.0).
    pub score: f64,
    /// Specific reasons for the risk assessment.
    pub reasons: Vec<RiskReason>,
    /// File kind classification.
    pub file_kind: FileKind,
    /// Number of direct importers.
    pub importer_count: usize,
    /// Number of references.
    pub reference_count: usize,
    /// Likely test files for the affected file.
    pub likely_tests: Vec<String>,
    /// Whether the graph was available for assessment.
    pub graph_available: bool,
}

/// File kind classification for risk assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum FileKind {
    /// Regular source file.
    Source,
    /// Test file.
    Test,
    /// Configuration file (Cargo.toml, package.json, etc.).
    Config,
    /// Generated code.
    Generated,
    /// Vendor/third-party code.
    Vendor,
    /// Documentation.
    Documentation,
    /// Build script or CI config.
    Build,
    /// Unknown file type.
    Unknown,
}

impl FileKind {
    /// Classify a file path into a kind.
    pub fn classify(path: &str) -> Self {
        let lower = path.to_lowercase();

        // Test files
        if lower.contains("test")
            || lower.contains("spec")
            || lower.contains("__tests__")
            || lower.contains("tests/")
        {
            return FileKind::Test;
        }

        // Generated files
        if lower.contains(".generated.")
            || lower.contains("_generated.")
            || lower.starts_with("generated/")
            || lower.contains("/generated/")
            || lower.starts_with("gen/")
            || lower.contains("/gen/")
        {
            return FileKind::Generated;
        }

        // Vendor
        if lower.contains("vendor/")
            || lower.contains("node_modules/")
            || lower.contains("third_party/")
            || lower.contains("third-party/")
        {
            return FileKind::Vendor;
        }

        // Config files
        let config_names = [
            "cargo.toml",
            "package.json",
            "tsconfig.json",
            "pyproject.toml",
            "go.mod",
            "go.sum",
            "package-lock.json",
            "yarn.lock",
            "bun.lockb",
            ".gitignore",
            ".eslintrc",
            ".prettierrc",
            "clippy.toml",
            "rustfmt.toml",
            "dockerfile",
            "docker-compose",
            "makefile",
            "cmakelists",
            "justfile",
            "taskfile",
            "earthfile",
        ];
        for name in &config_names {
            if lower.ends_with(name) || lower.contains(&format!("/{name}")) {
                return FileKind::Config;
            }
        }

        // Documentation
        if lower.ends_with(".md")
            || lower.ends_with(".txt")
            || lower.ends_with(".rst")
            || lower.contains("docs/")
            || lower.contains("doc/")
        {
            return FileKind::Documentation;
        }

        // Build scripts
        if lower.contains("build.rs")
            || lower.contains("build.zig")
            || lower.contains(".github/workflows/")
            || lower.contains("scripts/")
            || lower.contains(".ci/")
        {
            return FileKind::Build;
        }

        // Source files by extension
        let source_exts = [
            ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".c", ".cpp", ".h", ".hpp",
            ".cs", ".rb", ".php", ".swift", ".kt", ".scala", ".lua", ".zig", ".nim", ".ex", ".exs",
        ];
        for ext in &source_exts {
            if lower.ends_with(ext) {
                return FileKind::Source;
            }
        }

        FileKind::Unknown
    }
}

/// Classify mutation risk for a file edit.
///
/// # Arguments
/// * `path` - Path to the file being edited
/// * `indexer` - Optional RIL indexer for graph-based risk assessment
/// * `graph_enabled` - Whether the graph feature is enabled
///
/// # Returns
/// A `MutationRisk` assessment with level, score, reasons, and likely tests.
pub fn classify_mutation_risk(
    path: &str,
    indexer: Option<&RilIndexer>,
    graph_enabled: bool,
) -> MutationRisk {
    let file_kind = FileKind::classify(path);
    let mut reasons = Vec::new();
    let mut score = 0.0f64;

    // Base risk from file kind
    match file_kind {
        FileKind::Config => {
            score += 0.3;
            reasons.push(RiskReason {
                code: "config_file",
                message: "Configuration files affect build and runtime behavior".to_string(),
                weight: 0.3,
            });
        }
        FileKind::Build => {
            score += 0.25;
            reasons.push(RiskReason {
                code: "build_file",
                message: "Build scripts and CI configs affect the build pipeline".to_string(),
                weight: 0.25,
            });
        }
        FileKind::Generated => {
            score += 0.1;
            reasons.push(RiskReason {
                code: "generated_file",
                message: "Generated code may be overwritten on next build".to_string(),
                weight: 0.1,
            });
        }
        FileKind::Vendor => {
            score += 0.15;
            reasons.push(RiskReason {
                code: "vendor_file",
                message: "Vendor/third-party code changes may be lost on update".to_string(),
                weight: 0.15,
            });
        }
        FileKind::Test => {
            // Tests are lower risk — failures are isolated
            score += 0.05;
        }
        FileKind::Documentation => {
            score += 0.02;
        }
        _ => {}
    }

    // Graph-based risk assessment
    let mut graph_available = false;
    let importer_count: usize = 0;
    let reference_count: usize = 0;
    let likely_tests: Vec<String> = Vec::new();

    if let Some(idx) = indexer {
        let health = idx.health(graph_enabled);
        graph_available = matches!(
            health,
            GraphHealth::Healthy | GraphHealth::Stale | GraphHealth::Degraded
        );

        if graph_available {
            // Check graph stats for this file
            if let Ok(stats) = idx.stats() {
                if stats.unresolved_count as f64 / (stats.edge_count as f64 + 1.0) > 0.3 {
                    score += 0.1;
                    reasons.push(RiskReason {
                        code: "graph_partial",
                        message: "Graph has significant unresolved references — risk assessment may be incomplete".to_string(),
                        weight: 0.1,
                    });
                }
            }
        } else if graph_enabled {
            // Graph is enabled but not healthy
            match health {
                GraphHealth::Cold => {
                    score += 0.15;
                    reasons.push(RiskReason {
                        code: "graph_cold",
                        message: "Graph not yet indexed — risk assessment unavailable".to_string(),
                        weight: 0.15,
                    });
                }
                GraphHealth::Indexing | GraphHealth::Rebuilding => {
                    score += 0.1;
                    reasons.push(RiskReason {
                        code: "graph_indexing",
                        message: "Graph is currently indexing — risk assessment partial"
                            .to_string(),
                        weight: 0.1,
                    });
                }
                GraphHealth::Corrupt => {
                    score += 0.2;
                    reasons.push(RiskReason {
                        code: "graph_corrupt",
                        message: "Graph database is corrupt — risk assessment unavailable"
                            .to_string(),
                        weight: 0.2,
                    });
                }
                GraphHealth::Degraded => {
                    score += 0.15;
                    reasons.push(RiskReason {
                        code: "graph_degraded",
                        message: "Graph is degraded — risk assessment may be incomplete"
                            .to_string(),
                        weight: 0.15,
                    });
                }
                _ => {}
            }
        }
    } else if !graph_enabled {
        score += 0.1;
        reasons.push(RiskReason {
            code: "no_graph",
            message: "Graph is disabled — risk assessment based on file kind only".to_string(),
            weight: 0.1,
        });
    }

    // Cap score at 1.0
    let score = score.min(1.0);

    // Determine risk level from score
    let level = if score >= 0.7 {
        RiskLevel::Critical
    } else if score >= 0.4 {
        RiskLevel::High
    } else if score >= 0.2 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    MutationRisk {
        level,
        score,
        reasons,
        file_kind,
        importer_count,
        reference_count,
        likely_tests,
        graph_available,
    }
}

/// Create a compact risk sidecar for inclusion in mutation tool responses.
///
/// Returns a JSON object with:
/// - `level`: "low" | "medium" | "high" | "critical"
/// - `score`: 0.0–1.0
/// - `reasons`: array of {code, message}
/// - `file_kind`: "source" | "test" | "config" | ...
/// - `requires_confirmation`: bool
pub fn risk_sidecar(risk: &MutationRisk) -> serde_json::Value {
    let reasons: Vec<serde_json::Value> = risk
        .reasons
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code,
                "message": r.message,
            })
        })
        .collect();

    serde_json::json!({
        "level": risk.level.label(),
        "score": risk.score,
        "reasons": reasons,
        "file_kind": format!("{:?}", risk.file_kind).to_lowercase(),
        "requires_confirmation": risk.level.requires_confirmation(),
        "graph_available": risk.graph_available,
    })
}

/// Compute mutation risk and return it as a sidecar value.
///
/// Convenience wrapper for command handlers that don't need the full MutationRisk.
pub fn compute_risk_sidecar(path: &str, graph_enabled: bool) -> serde_json::Value {
    let risk = classify_mutation_risk(path, None, graph_enabled);
    risk_sidecar(&risk)
}

/// Enrich a response JSON value with mutation risk data.
///
/// Adds a "risk" field to the response if `mutation_risk` param is true (default: true).
/// Graph state is determined from `graph_enabled` (default: false).
/// The mutation always proceeds (fail-open); risk is informational.
pub fn enrich_response_with_risk(
    response_data: serde_json::Value,
    file_path: &str,
    params: &serde_json::Value,
) -> serde_json::Value {
    let show_risk = params
        .get("mutation_risk")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if !show_risk {
        return response_data;
    }

    let graph_enabled = params
        .get("graph_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let risk = compute_risk_sidecar(file_path, graph_enabled);

    let mut enriched = response_data;
    enriched["risk"] = risk;
    enriched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_source_file() {
        let risk = classify_mutation_risk("src/main.rs", None, false);
        assert_eq!(risk.file_kind, FileKind::Source);
        assert_eq!(risk.level, RiskLevel::Low);
        assert!(!risk.graph_available);
    }

    #[test]
    fn classify_test_file() {
        let risk = classify_mutation_risk("tests/foo_test.rs", None, false);
        assert_eq!(risk.file_kind, FileKind::Test);
        assert_eq!(risk.level, RiskLevel::Low);
    }

    #[test]
    fn classify_config_file() {
        let risk = classify_mutation_risk("Cargo.toml", None, false);
        assert_eq!(risk.file_kind, FileKind::Config);
        assert!(risk.level >= RiskLevel::Medium);
    }

    #[test]
    fn classify_generated_file() {
        let risk = classify_mutation_risk("src/gen/parser.rs", None, false);
        assert_eq!(risk.file_kind, FileKind::Generated);
    }

    #[test]
    fn classify_vendor_file() {
        let risk = classify_mutation_risk("vendor/lib.rs", None, false);
        assert_eq!(risk.file_kind, FileKind::Vendor);
    }

    #[test]
    fn classify_documentation() {
        let risk = classify_mutation_risk("docs/README.md", None, false);
        assert_eq!(risk.file_kind, FileKind::Documentation);
        assert_eq!(risk.level, RiskLevel::Low);
    }

    #[test]
    fn classify_build_script() {
        let risk = classify_mutation_risk(".github/workflows/ci.yml", None, false);
        assert_eq!(risk.file_kind, FileKind::Build);
        assert!(risk.level >= RiskLevel::Medium);
    }

    #[test]
    fn graph_disabled_increases_risk() {
        let risk_disabled = classify_mutation_risk("src/main.rs", None, false);
        let _risk_enabled = classify_mutation_risk("src/main.rs", None, true);
        // Both should be Low for source, but disabled has no_graph reason
        assert!(risk_disabled.reasons.iter().any(|r| r.code == "no_graph"));
    }

    #[test]
    fn risk_score_clamped() {
        // Config + no graph should still be <= 1.0
        let risk = classify_mutation_risk("Cargo.toml", None, false);
        assert!(risk.score <= 1.0);
        assert!(risk.score >= 0.0);
    }

    #[test]
    fn risk_level_boundaries() {
        // Source file with no graph → low
        let r1 = classify_mutation_risk("src/main.rs", None, true);
        assert_eq!(r1.level, RiskLevel::Low);

        // Config file → at least medium
        let r2 = classify_mutation_risk("package.json", None, false);
        assert!(r2.level >= RiskLevel::Medium);
    }

    #[test]
    fn critical_requires_confirmation() {
        assert!(RiskLevel::Critical.requires_confirmation());
        assert!(!RiskLevel::High.requires_confirmation());
        assert!(!RiskLevel::Medium.requires_confirmation());
        assert!(!RiskLevel::Low.requires_confirmation());
    }

    #[test]
    fn file_kind_extensive_classification() {
        assert_eq!(FileKind::classify("src/app.ts"), FileKind::Source);
        assert_eq!(FileKind::classify("src/app.py"), FileKind::Source);
        assert_eq!(FileKind::classify("src/main.go"), FileKind::Source);
        assert_eq!(FileKind::classify("src/app.jsx"), FileKind::Source);
        assert_eq!(FileKind::classify("tests/unit.py"), FileKind::Test);
        assert_eq!(FileKind::classify("spec/foo.spec.ts"), FileKind::Test);
        assert_eq!(FileKind::classify("__tests__/bar.test.js"), FileKind::Test);
        assert_eq!(FileKind::classify("Cargo.toml"), FileKind::Config);
        assert_eq!(FileKind::classify("package.json"), FileKind::Config);
        assert_eq!(FileKind::classify("tsconfig.json"), FileKind::Config);
        assert_eq!(FileKind::classify("CHANGELOG.md"), FileKind::Documentation);
        assert_eq!(FileKind::classify("build.rs"), FileKind::Build);
        assert_eq!(FileKind::classify("Makefile"), FileKind::Config);
        assert_eq!(
            FileKind::classify("node_modules/foo/bar.js"),
            FileKind::Vendor
        );
        assert_eq!(FileKind::classify("vendor/lib.rs"), FileKind::Vendor);
        assert_eq!(
            FileKind::classify("generated/parser.rs"),
            FileKind::Generated
        );
        assert_eq!(FileKind::classify("src/gen/lexer.rs"), FileKind::Generated);
    }
}
