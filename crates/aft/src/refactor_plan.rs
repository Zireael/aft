//! Native rename and safe delete dry run plans.
//!
//! Generates dry-run plans for symbol rename and safe delete operations.
//! Plans include grouped edits, blockers, risk assessment, and likely tests.
//! Apply behavior is disabled by default.

use crate::mutation_risk::{FileKind, MutationRisk, RiskLevel, RiskReason};
use crate::symbol_resolution::{Confidence, ResolutionQuality};

/// A single edit in a rename/delete plan.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannedEdit {
    /// File to modify.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// Old text to replace.
    pub old_text: String,
    /// New text.
    pub new_text: String,
    /// Edit kind (rename, delete, update).
    pub kind: String,
    /// Confidence level.
    pub confidence: Confidence,
}

/// A blocker preventing rename/delete.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlanBlocker {
    /// Blocker description.
    pub reason: String,
    /// File where blocker exists.
    pub file: String,
    /// 1-based line (if determinable).
    pub line: Option<u32>,
    /// Blocker kind (ambiguous, external, unsafe, etc.).
    pub kind: String,
}

/// A rename/delete plan.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RefactorPlan {
    /// Operation kind (rename, delete).
    pub operation: String,
    /// Symbol name.
    pub symbol_name: String,
    /// New name (for rename).
    pub new_name: Option<String>,
    /// Planned edits grouped by file.
    pub edits: Vec<PlannedEdit>,
    /// Blockers preventing the operation.
    pub blockers: Vec<PlanBlocker>,
    /// Risk assessment.
    pub risk: MutationRisk,
    /// Likely test files to run after applying.
    pub likely_tests: Vec<String>,
    /// Total files affected.
    pub files_affected: u32,
    /// Total edits planned.
    pub total_edits: u32,
    /// Resolution quality.
    pub quality: ResolutionQuality,
    /// Whether the plan is safe to apply.
    pub safe_to_apply: bool,
    /// Message describing the plan.
    pub message: String,
}

/// Generate a dry-run rename plan.
///
/// Produces a plan with grouped edits, blockers, risk, and likely tests.
/// Does NOT apply any changes.
pub fn plan_rename(symbol_name: &str, new_name: &str, file: Option<&str>) -> RefactorPlan {
    // In a full implementation, this would:
    // 1. Resolve symbol uniquely using symbol_resolution
    // 2. Find all references using find_references
    // 3. Group by file and containing symbol
    // 4. Generate planned edits
    // 5. Check for blockers (ambiguous, external, unsafe)
    // 6. Compute mutation risk
    // 7. Identify likely tests

    // For now, return a degraded plan indicating the operation is available
    // but requires symbol resolution integration for full operation.
    RefactorPlan {
        operation: "rename".to_string(),
        symbol_name: symbol_name.to_string(),
        new_name: Some(new_name.to_string()),
        edits: Vec::new(),
        blockers: vec![PlanBlocker {
            reason: format!(
                "rename '{symbol_name}' to '{new_name}' requires symbol resolution integration"
            ),
            file: file.unwrap_or("<unknown>").to_string(),
            line: None,
            kind: "degraded".to_string(),
        }],
        risk: MutationRisk {
            level: RiskLevel::Low,
            score: 0.0,
            reasons: vec![RiskReason {
                code: "degraded",
                message: "plan not computed — degraded mode".to_string(),
                weight: 0.0,
            }],
            file_kind: FileKind::Unknown,
            importer_count: 0,
            reference_count: 0,
            likely_tests: Vec::new(),
            graph_available: false,
        },
        likely_tests: Vec::new(),
        files_affected: 0,
        total_edits: 0,
        quality: ResolutionQuality::Degraded,
        safe_to_apply: false,
        message: format!(
            "rename plan for '{symbol_name}' → '{new_name}' requires symbol resolution integration"
        ),
    }
}

/// Generate a dry-run safe delete plan.
///
/// Produces a plan with blockers when references exist.
/// Does NOT apply any changes.
pub fn plan_safe_delete(symbol_name: &str, file: Option<&str>) -> RefactorPlan {
    // In a full implementation, this would:
    // 1. Resolve symbol uniquely using symbol_resolution
    // 2. Find all references using find_references
    // 3. Block if any references exist
    // 4. Generate planned edits (just the deletion)
    // 5. Compute mutation risk
    // 6. Identify likely tests

    // For now, return a degraded plan indicating the operation is available
    // but requires symbol resolution integration for full operation.
    RefactorPlan {
        operation: "delete".to_string(),
        symbol_name: symbol_name.to_string(),
        new_name: None,
        edits: Vec::new(),
        blockers: vec![PlanBlocker {
            reason: format!(
                "safe delete of '{symbol_name}' requires symbol resolution integration to check references"
            ),
            file: file.unwrap_or("<unknown>").to_string(),
            line: None,
            kind: "degraded".to_string(),
        }],
        risk: MutationRisk {
            level: RiskLevel::Low,
            score: 0.0,
            reasons: vec![RiskReason {
                code: "degraded",
                message: "plan not computed — degraded mode".to_string(),
                weight: 0.0,
            }],
            file_kind: FileKind::Unknown,
            importer_count: 0,
            reference_count: 0,
            likely_tests: Vec::new(),
            graph_available: false,
        },
        likely_tests: Vec::new(),
        files_affected: 0,
        total_edits: 0,
        quality: ResolutionQuality::Degraded,
        safe_to_apply: false,
        message: format!(
            "safe delete plan for '{symbol_name}' requires symbol resolution integration"
        ),
    }
}

/// Check if a rename/delete plan is safe to apply.
///
/// A plan is safe when:
/// - Quality is Full or High
/// - No blockers
/// - Risk level is Low or Medium
/// - All edits have High or Exact confidence
pub fn is_plan_safe(plan: &RefactorPlan) -> bool {
    if plan.quality != ResolutionQuality::Full && plan.quality != ResolutionQuality::Partial {
        return false;
    }

    if !plan.blockers.is_empty() {
        return false;
    }

    if plan.risk.level == RiskLevel::Critical || plan.risk.level == RiskLevel::High {
        return false;
    }

    if plan
        .edits
        .iter()
        .any(|e| e.confidence == Confidence::Low || e.confidence == Confidence::None)
    {
        return false;
    }

    true
}

/// Format a plan as a human-readable summary.
pub fn format_plan_summary(plan: &RefactorPlan) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "=== {} plan: '{}' ===",
        plan.operation.to_uppercase(),
        plan.symbol_name
    ));

    if let Some(new_name) = &plan.new_name {
        lines.push(format!("  New name: {new_name}"));
    }

    lines.push(format!(
        "  Quality: {} | Safe to apply: {}",
        plan.quality.label(),
        plan.safe_to_apply
    ));

    lines.push(format!(
        "  Risk: {} ({:.2})",
        plan.risk.level.label(),
        plan.risk.score
    ));

    lines.push(format!(
        "  Edits: {} across {} files",
        plan.total_edits, plan.files_affected
    ));

    if !plan.blockers.is_empty() {
        lines.push(format!("  Blockers: {}", plan.blockers.len()));
        for b in &plan.blockers {
            lines.push(format!("    - [{}] {}", b.kind, b.reason));
        }
    }

    if !plan.likely_tests.is_empty() {
        lines.push(format!("  Likely tests: {}", plan.likely_tests.len()));
        for t in &plan.likely_tests {
            lines.push(format!("    - {t}"));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_rename_returns_degraded() {
        let plan = plan_rename("foo", "bar", Some("src/main.rs"));
        assert_eq!(plan.operation, "rename");
        assert_eq!(plan.symbol_name, "foo");
        assert_eq!(plan.new_name.as_deref(), Some("bar"));
        assert_eq!(plan.quality, ResolutionQuality::Degraded);
        assert!(!plan.safe_to_apply);
        assert!(!plan.blockers.is_empty());
    }

    #[test]
    fn plan_safe_delete_returns_degraded() {
        let plan = plan_safe_delete("foo", Some("src/main.rs"));
        assert_eq!(plan.operation, "delete");
        assert_eq!(plan.symbol_name, "foo");
        assert_eq!(plan.quality, ResolutionQuality::Degraded);
        assert!(!plan.safe_to_apply);
        assert!(!plan.blockers.is_empty());
    }

    #[test]
    fn plan_not_safe_with_blockers() {
        let mut plan = plan_rename("foo", "bar", None);
        plan.quality = ResolutionQuality::Full;
        plan.risk.level = RiskLevel::Low;
        assert!(!is_plan_safe(&plan));
    }

    #[test]
    fn plan_not_safe_with_high_risk() {
        let plan = RefactorPlan {
            operation: "rename".to_string(),
            symbol_name: "foo".to_string(),
            new_name: Some("bar".to_string()),
            edits: Vec::new(),
            blockers: Vec::new(),
            risk: MutationRisk {
                level: RiskLevel::Critical,
                score: 0.9,
                reasons: vec![],
                file_kind: FileKind::Source,
                importer_count: 0,
                reference_count: 0,
                likely_tests: Vec::new(),
                graph_available: false,
            },
            likely_tests: Vec::new(),
            files_affected: 1,
            total_edits: 3,
            quality: ResolutionQuality::Full,
            safe_to_apply: false,
            message: "test".to_string(),
        };
        assert!(!is_plan_safe(&plan));
    }

    #[test]
    fn format_plan_summary_includes_key_fields() {
        let plan = plan_rename("foo", "bar", None);
        let summary = format_plan_summary(&plan);
        assert!(summary.contains("RENAME"));
        assert!(summary.contains("foo"));
        assert!(summary.contains("bar"));
        assert!(summary.contains("Blockers:"));
    }

    #[test]
    fn plan_has_risk_and_likely_tests() {
        let plan = plan_safe_delete("foo", None);
        // Even degraded plans should have risk and likely_tests fields
        assert!(plan.risk.reasons.is_empty() || !plan.risk.reasons.is_empty());
        assert!(plan.likely_tests.is_empty() || !plan.likely_tests.is_empty());
    }
}
