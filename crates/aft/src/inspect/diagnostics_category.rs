use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::job::{InspectSnapshot, JobOutcome, JobScope};
use crate::config::{
    Config, MAX_INSPECT_DIAGNOSTICS_TIMEOUT_MS, MIN_INSPECT_DIAGNOSTICS_TIMEOUT_MS,
};
use crate::context::AppContext;
use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};
use crate::lsp::manager::{ApplicableServerFailure, NotApplicableServer};
use crate::lsp::registry::servers_for_file;
use crate::lsp::roots::ServerKey;
use crate::lsp::tsconfig_membership::TsconfigMembershipCache;

/// Whole-request server budget for blocking inspect. Every phase shares one
/// absolute deadline derived from this value; client transport adds separate
/// headroom so the server always answers before the client gives up.
pub(crate) fn inspect_request_timeout(config: &Config) -> Duration {
    Duration::from_millis(config.inspect.diagnostics_timeout_ms.clamp(
        MIN_INSPECT_DIAGNOSTICS_TIMEOUT_MS,
        MAX_INSPECT_DIAGNOSTICS_TIMEOUT_MS,
    ))
}

#[derive(Debug, Clone)]
struct CollectedDiagnostic {
    diagnostic: StoredDiagnostic,
    provisional: bool,
}

/// A scoped file that no producer has authoritatively analyzed. Warm
/// collection cannot prove per-file cleanliness from the global "some server
/// reported" signal, so scoped payloads name these files instead of rendering
/// a confident empty answer.
#[derive(Debug)]
struct ScopedCoverageGap {
    file: PathBuf,
    reason: &'static str,
}

#[derive(Default)]
struct DiagnosticsCollection {
    diagnostics: Vec<CollectedDiagnostic>,
    server_ran: bool,
    applicability_is_empty: bool,
    servers_pending: BTreeSet<String>,
    producer_failures: BTreeMap<String, String>,
    /// Servers with a root marker but no file to analyze, keyed by server id.
    /// Informational only: an inapplicable server is neither a failure nor a
    /// gap, so it never makes the payload incomplete.
    not_applicable: BTreeMap<String, String>,
    scope_coverage_gaps: Vec<ScopedCoverageGap>,
    /// True when the producer set this collection is responsible for has all
    /// settled (authoritative report or no longer warming). Distinct from
    /// `server_ran`: a quiesced producer may never publish, and that empty
    /// store is still a complete answer.
    producers_settled: bool,
}

/// Collect diagnostics for the explicit inspect path.
///
/// Collection never depends on the request scope: scoped and unscoped
/// requests both read the warm working set, so a scope cannot switch the
/// category onto a more expensive collection strategy. Scope only filters
/// which findings the payload renders.
///
/// The authority halves differ by design. An unscoped request makes a
/// full-root claim, so producer settlement over the started set decides
/// freshness — the same predicate the blocking wait uses. A scoped request
/// makes per-file claims: every scoped file must either carry an
/// authoritative producer report or appear as a named gap, because a
/// settled producer cannot prove that a specific file nothing ever analyzed
/// is clean. A collection becomes Fresh after every expected producer has
/// settled (authoritative report or no longer warming) or reached a
/// terminal failure. Terminal producer failures remain named gaps in the
/// payload; producers still warming without an authoritative report still
/// prevent a fresh response.
pub(crate) fn run_diagnostics_category(
    ctx: &AppContext,
    snapshot: &InspectSnapshot,
    scope: &JobScope,
    scope_was_provided: bool,
    applicability_is_empty: bool,
    producer_failures: &[ApplicableServerFailure],
    not_applicable: &[NotApplicableServer],
    expected_producers: &[ServerKey],
) -> JobOutcome {
    let mut collection = if applicability_is_empty {
        // No applicable producer means there is no diagnostic artifact to wait
        // for; the empty category is authoritative for this applicability snapshot.
        DiagnosticsCollection {
            applicability_is_empty: true,
            ..DiagnosticsCollection::default()
        }
    } else {
        collect_warm_working_set(ctx, snapshot, expected_producers)
    };
    collection.record_producer_failures(producer_failures);
    collection.not_applicable = not_applicable
        .iter()
        .map(|server| (server.server_id.clone(), server.reason()))
        .collect();

    if scope_was_provided {
        collection.apply_scope(scope);
        collection.record_scope_coverage_gaps(ctx, snapshot, scope);
        // Per-file coverage gaps make the scoped verdict self-certifying:
        // every scoped file is either covered by an authoritative report or
        // named as a gap, so the payload is honest without waiting on global
        // quiescence signals that may describe files outside the scope.
        return JobOutcome::Fresh {
            payload: collection.into_payload(snapshot),
        };
    }

    if collection.is_reportable() {
        JobOutcome::Fresh {
            payload: collection.into_payload(snapshot),
        }
    } else {
        JobOutcome::pending(true)
    }
}

fn collect_warm_working_set(
    ctx: &AppContext,
    snapshot: &InspectSnapshot,
    expected_producers: &[ServerKey],
) -> DiagnosticsCollection {
    let mut collection = DiagnosticsCollection::default();
    let mut tsconfig_membership = TsconfigMembershipCache::new();
    {
        let mut lsp = ctx.lsp();
        // The only diagnostics collection path, for scoped and unscoped
        // requests alike: drain already queued LSP events, then read only the
        // warm diagnostics store. It does not open files or spawn servers.
        lsp.drain_events();
        collection.server_ran = lsp.has_any_diagnostic_reports();
        // Pending producers are those that have not settled. The blocking wait
        // uses the same `producer_has_settled` check, so a quiesced producer
        // with no published report is complete rather than forever pending.
        // When the caller started a specific set, that set is the obligation;
        // otherwise the currently running clients are.
        let producers = if expected_producers.is_empty() {
            lsp.active_server_keys()
        } else {
            expected_producers.to_vec()
        };
        for server in &producers {
            if !lsp.producer_has_settled(server) {
                collection.servers_pending.insert(server_id(server));
            }
        }
        collection.producers_settled =
            !producers.is_empty() && collection.servers_pending.is_empty();
        collection.diagnostics = lsp
            .get_all_diagnostics_with_provisional()
            .into_iter()
            .map(|(diagnostic, provisional)| CollectedDiagnostic {
                diagnostic: diagnostic.clone(),
                provisional,
            })
            .collect();
    }

    collection.diagnostics.retain(|diagnostic| {
        diagnostic
            .diagnostic
            .file
            .starts_with(&snapshot.project_root)
            && !tsconfig_membership.should_skip_diagnostics(&diagnostic.diagnostic.file)
    });
    collection.sort_and_dedup();
    collection
}

/// Per-file diagnostic coverage verdict for scoped requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopedFileCoverage {
    /// A registered producer holds an authoritative (neither stale nor
    /// warming) report for the file, including an empty checked-clean report.
    Covered,
    /// No LSP producer is registered for this file type.
    NoProducer,
    /// Producers are registered but none has a current report for the file.
    NoReport,
    /// Only warming (provisional) reports exist: the reporting server has not
    /// reached quiescence yet.
    Warming,
}

/// Test hook: force every scoped file to read as authoritatively covered.
/// Mutation control for the per-file authority check — with this forced, a
/// scoped request for a file nothing ever analyzed returns a confident empty
/// payload instead of a named gap, exactly the regression the coverage gap
/// exists to prevent.
pub fn force_scoped_diagnostic_coverage_for_test(forced: bool) {
    FORCE_SCOPED_DIAGNOSTIC_COVERAGE.store(forced, Ordering::SeqCst);
}

static FORCE_SCOPED_DIAGNOSTIC_COVERAGE: AtomicBool = AtomicBool::new(false);

fn scoped_file_coverage(ctx: &AppContext, config: &Config, file: &Path) -> ScopedFileCoverage {
    if FORCE_SCOPED_DIAGNOSTIC_COVERAGE.load(Ordering::SeqCst) {
        return ScopedFileCoverage::Covered;
    }
    if servers_for_file(file, config).is_empty() {
        return ScopedFileCoverage::NoProducer;
    }
    let lsp = ctx.lsp();
    if lsp.has_authoritative_report_for_file(file) {
        return ScopedFileCoverage::Covered;
    }
    if lsp.has_diagnostic_report_for_file(file) {
        // Reports exist but every one is warming (provisional): the server
        // published before reaching quiescence, so its view is not
        // authoritative yet.
        return ScopedFileCoverage::Warming;
    }
    ScopedFileCoverage::NoReport
}

/// Enumerate the files a scoped diagnostics verdict must cover.
///
/// Explicit file roots are always candidates: naming a file is a direct claim
/// on its diagnostics, including the "no producer applies" case. Directory
/// roots are walked with the same filters applicability resolution uses;
/// only files with a registered producer are candidates there, because
/// directories inevitably contain non-code files nobody expects diagnostics
/// for. Files a tsconfig excludes from diagnostics are skipped in both cases.
fn scoped_coverage_candidates(
    snapshot: &InspectSnapshot,
    scope: &JobScope,
    config: &Config,
    tsconfig_membership: &mut TsconfigMembershipCache,
) -> Vec<PathBuf> {
    let roots = if scope.roots().is_empty() {
        vec![snapshot.project_root.clone()]
    } else {
        scope.roots().to_vec()
    };

    let mut candidates = BTreeSet::new();
    for root in roots {
        if root.is_file() {
            if tsconfig_membership.should_skip_diagnostics(&root) {
                continue;
            }
            candidates.insert(crate::inspect::job::canonicalize_normalized(&root));
            continue;
        }

        // Prevent a disappearing child mount from making ReadDir::drop abort on ENXIO.
        let walker = ignore::WalkBuilder::new(&root)
            .same_file_system(true)
            .standard_filters(true)
            .add_custom_ignore_filename(".aftignore")
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | ".turbo"
                )
            })
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }
            let path = entry.path();
            if tsconfig_membership.should_skip_diagnostics(path)
                || servers_for_file(path, config).is_empty()
            {
                continue;
            }
            candidates.insert(crate::inspect::job::canonicalize_normalized(path));
        }
    }

    candidates.into_iter().collect()
}

impl DiagnosticsCollection {
    fn record_producer_failures(&mut self, failures: &[ApplicableServerFailure]) {
        for failure in failures {
            self.producer_failures
                .entry(server_id(&failure.server_key))
                .or_insert_with(|| failure.reason());
        }
    }

    /// Render-time scope filter over findings. The warm collection is
    /// full-root; a scoped payload keeps only in-scope findings. Warming
    /// (provisional) rows are dropped here because the per-file coverage
    /// check reports their files as named gaps until the responsible server
    /// settles, and a warming row must not read as an authoritative finding.
    fn apply_scope(&mut self, scope: &JobScope) {
        self.diagnostics.retain(|diagnostic| {
            !diagnostic.provisional && scope.contains(&diagnostic.diagnostic.file)
        });
    }

    /// Name every scoped file that no producer has authoritatively analyzed.
    /// The global `server_ran` signal is per-root, not per-file, so without
    /// this check a scoped request for a file nothing ever analyzed would
    /// render as a confident empty answer. Each named file becomes a gap row
    /// (`complete: false`) instead.
    fn record_scope_coverage_gaps(
        &mut self,
        ctx: &AppContext,
        snapshot: &InspectSnapshot,
        scope: &JobScope,
    ) {
        let mut tsconfig_membership = TsconfigMembershipCache::new();
        let candidates =
            scoped_coverage_candidates(snapshot, scope, &snapshot.config, &mut tsconfig_membership);
        for file in candidates {
            let reason = match scoped_file_coverage(ctx, &snapshot.config, &file) {
                ScopedFileCoverage::Covered => continue,
                ScopedFileCoverage::NoProducer => {
                    "no LSP producer is registered for this file type"
                }
                ScopedFileCoverage::NoReport => {
                    "no LSP producer has a current diagnostic report for this file"
                }
                ScopedFileCoverage::Warming => {
                    "the reporting LSP server has not reached quiescence yet"
                }
            };
            self.scope_coverage_gaps
                .push(ScopedCoverageGap { file, reason });
        }
    }

    #[cfg(test)]
    fn is_complete(&self) -> bool {
        self.is_reportable()
            && self.producer_failures.is_empty()
            && self.scope_coverage_gaps.is_empty()
    }

    /// Full-root authority conjunction for the no-scope path. Completeness is
    /// producer settlement — the same predicate the blocking wait uses — not
    /// "some server published a report". A quiesced producer with no reports
    /// is complete; a still-warming producer without an authoritative report
    /// is not. Scoped requests use per-file authority instead (see
    /// `record_scope_coverage_gaps`).
    fn is_reportable(&self) -> bool {
        self.servers_pending.is_empty()
            && (self.server_ran
                || self.applicability_is_empty
                || !self.producer_failures.is_empty()
                || self.producers_settled)
            && (self.producers_settled
                || self
                    .diagnostics
                    .iter()
                    .all(|diagnostic| !diagnostic.provisional))
    }

    fn into_payload(mut self, snapshot: &InspectSnapshot) -> Value {
        // Warming rows are not findings. After producers settle, leftover
        // provisional entries are dropped rather than blocking the payload;
        // the wait already treated those producers as complete.
        self.diagnostics
            .retain(|diagnostic| !diagnostic.provisional);
        self.sort_and_dedup();
        let (errors, warnings, info, hints) = severity_counts(&self.diagnostics);
        let items = self
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic_item(snapshot, diagnostic))
            .collect::<Vec<_>>();

        let mut payload = serde_json::json!({
            "errors": errors,
            "warnings": warnings,
            "info": info,
            "hints": hints,
            "items": items,
        });

        let mut gaps: Vec<Value> = self
            .producer_failures
            .into_iter()
            .map(|(producer, reason)| {
                serde_json::json!({
                    "kind": "failed_producer",
                    "producer": producer,
                    "reason": reason,
                })
            })
            .collect();
        gaps.extend(self.scope_coverage_gaps.iter().map(|gap| {
            serde_json::json!({
                "kind": "uncovered_file",
                "file": display_path(snapshot, &gap.file),
                "reason": gap.reason,
            })
        }));
        if !gaps.is_empty() {
            payload["complete"] = Value::Bool(false);
            payload["gaps"] = Value::Array(gaps);
        }
        if !self.not_applicable.is_empty() {
            payload["not_applicable"] = Value::Array(
                self.not_applicable
                    .iter()
                    .map(|(producer, reason)| {
                        serde_json::json!({ "producer": producer, "reason": reason })
                    })
                    .collect(),
            );
        }
        payload
    }

    fn sort_and_dedup(&mut self) {
        self.diagnostics.sort_by(|left, right| {
            left.diagnostic
                .file
                .cmp(&right.diagnostic.file)
                .then(left.diagnostic.line.cmp(&right.diagnostic.line))
                .then(left.diagnostic.column.cmp(&right.diagnostic.column))
                .then(left.diagnostic.end_line.cmp(&right.diagnostic.end_line))
                .then(left.diagnostic.end_column.cmp(&right.diagnostic.end_column))
                .then(left.diagnostic.severity.as_str().cmp(right.diagnostic.severity.as_str()))
                .then(left.diagnostic.message.cmp(&right.diagnostic.message))
                .then(left.diagnostic.source.cmp(&right.diagnostic.source))
                // Prefer an authoritative copy when multiple servers report the
                // same diagnostic, so detail rows do not retain a warming tag.
                .then(left.provisional.cmp(&right.provisional))
        });
        self.diagnostics.dedup_by(|left, right| {
            left.diagnostic.file == right.diagnostic.file
                && left.diagnostic.line == right.diagnostic.line
                && left.diagnostic.column == right.diagnostic.column
                && left.diagnostic.end_line == right.diagnostic.end_line
                && left.diagnostic.end_column == right.diagnostic.end_column
                && left.diagnostic.severity == right.diagnostic.severity
                && left.diagnostic.message == right.diagnostic.message
                && left.diagnostic.source == right.diagnostic.source
        });
    }
}

fn severity_counts(diagnostics: &[CollectedDiagnostic]) -> (usize, usize, usize, usize) {
    severity_counts_filtered(diagnostics, |diagnostic| !diagnostic.provisional)
}

fn severity_counts_filtered(
    diagnostics: &[CollectedDiagnostic],
    include: impl Fn(&CollectedDiagnostic) -> bool,
) -> (usize, usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut info = 0;
    let mut hints = 0;

    for diagnostic in diagnostics {
        if !include(diagnostic)
            || crate::lsp::environmental::is_environmental_diagnostic(&diagnostic.diagnostic)
        {
            continue;
        }
        match diagnostic.diagnostic.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warning => warnings += 1,
            DiagnosticSeverity::Information => info += 1,
            DiagnosticSeverity::Hint => hints += 1,
        }
    }

    (errors, warnings, info, hints)
}

/// Detail-row message for `aft_inspect` items (file:line:col severity message).
/// Environmental and warming diagnostics are tagged so summary counts and
/// listed rows explain why a row is excluded from authoritative totals.
fn diagnostic_detail_message(diagnostic: &CollectedDiagnostic) -> String {
    let mut message = diagnostic.diagnostic.message.clone();
    if crate::lsp::environmental::is_environmental_diagnostic(&diagnostic.diagnostic) {
        message.push_str(" [environmental]");
    }
    if diagnostic.provisional {
        message.push_str(" (analyzer warming)");
    }
    message
}

fn diagnostic_item(snapshot: &InspectSnapshot, diagnostic: &CollectedDiagnostic) -> Value {
    serde_json::json!({
        "file": display_path(snapshot, &diagnostic.diagnostic.file),
        "line": diagnostic.diagnostic.line,
        "column": diagnostic.diagnostic.column,
        "severity": diagnostic.diagnostic.severity.as_str(),
        "message": diagnostic_detail_message(diagnostic),
        "source": diagnostic.diagnostic.source.as_deref().unwrap_or("lsp"),
    })
}

fn display_path(snapshot: &InspectSnapshot, path: &Path) -> String {
    path.strip_prefix(&snapshot.project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn server_id(key: &ServerKey) -> String {
    key.kind.id_str().to_string()
}

#[cfg(test)]
mod payload_count_tests {
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    use super::{
        inspect_request_timeout, CollectedDiagnostic, DiagnosticsCollection, ScopedCoverageGap,
    };
    use crate::config::Config;
    use crate::inspect::job::{InspectSnapshot, JobScope};
    use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};
    use crate::parser::SymbolCache;

    fn snapshot() -> InspectSnapshot {
        InspectSnapshot::new(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/.aft"),
            Arc::new(Config::default()),
            Arc::new(RwLock::new(SymbolCache::new())),
        )
    }

    fn collection() -> DiagnosticsCollection {
        DiagnosticsCollection {
            diagnostics: vec![CollectedDiagnostic {
                diagnostic: StoredDiagnostic {
                    file: PathBuf::from("/repo/src/main.rs"),
                    line: 1,
                    column: 1,
                    end_line: 1,
                    end_column: 2,
                    severity: DiagnosticSeverity::Error,
                    message: "verified result".into(),
                    code: None,
                    source: None,
                },
                provisional: false,
            }],
            server_ran: true,
            ..DiagnosticsCollection::default()
        }
    }

    #[test]
    fn configured_diagnostics_deadline_is_the_whole_request_budget() {
        let config = Config {
            inspect: crate::config::InspectConfig {
                diagnostics_timeout_ms: 15_000,
                ..crate::config::InspectConfig::default()
            },
            ..Config::default()
        };

        assert_eq!(
            inspect_request_timeout(&config),
            std::time::Duration::from_millis(15_000)
        );
    }

    #[test]
    fn empty_applicability_is_vacuously_complete() {
        let collection = DiagnosticsCollection {
            applicability_is_empty: true,
            ..DiagnosticsCollection::default()
        };

        assert!(collection.is_complete());
    }

    #[test]
    fn incomplete_collection_cannot_be_promoted_to_a_payload() {
        let mut collection = collection();
        collection.servers_pending.insert("rust-analyzer".into());
        assert!(!collection.is_complete());
    }

    #[test]
    fn provisional_collection_cannot_be_promoted_to_a_payload() {
        let mut collection = collection();
        collection.diagnostics[0].provisional = true;
        assert!(!collection.is_complete());
    }

    #[test]
    fn settled_producers_without_reports_are_reportable() {
        let collection = DiagnosticsCollection {
            producers_settled: true,
            ..DiagnosticsCollection::default()
        };
        // The previous full-root check required `server_ran` (any published
        // report) and treated a settled empty store as incomplete. That
        // disagreed with producer settlement, which is complete once every
        // producer holds an authoritative report or has stopped warming.
        let old_predicate = (collection.server_ran
            || collection.applicability_is_empty
            || !collection.producer_failures.is_empty())
            && collection.servers_pending.is_empty()
            && collection
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.provisional);
        assert!(
            !old_predicate,
            "the old reportable check must reject a settled empty store, otherwise this test cannot catch the wait/gate split"
        );
        assert!(collection.is_reportable());
        assert!(collection.is_complete());
        let payload = collection.into_payload(&snapshot());
        assert_eq!(payload["errors"], 0);
        assert!(payload.get("complete").is_none());
    }

    #[test]
    fn settled_producers_drop_leftover_provisional_rows_instead_of_refusing() {
        let mut collection = collection();
        collection.producers_settled = true;
        collection.diagnostics[0].provisional = true;
        assert!(collection.is_reportable());
        let payload = collection.into_payload(&snapshot());
        assert_eq!(payload["errors"], 0);
        assert!(payload["items"].as_array().is_some_and(Vec::is_empty));
    }

    #[test]
    fn fresh_payload_contains_only_authoritative_counts_and_items() {
        let payload = collection().into_payload(&snapshot());
        assert_eq!(payload["errors"], 1);
        assert_eq!(payload["warnings"], 0);
        assert!(payload.get("server_ran").is_none());
        assert!(payload.get("complete").is_none());
        assert!(payload.get("status").is_none());
        assert!(payload.get("provisional_counts").is_none());
    }

    #[test]
    fn terminal_producer_failure_is_a_reportable_named_gap() {
        let mut collection = collection();
        collection
            .producer_failures
            .insert("astro".into(), "initialize failed".into());

        assert!(collection.is_reportable());
        assert!(!collection.is_complete());
        let payload = collection.into_payload(&snapshot());
        assert_eq!(payload["complete"], false);
        assert_eq!(payload["gaps"][0]["kind"], "failed_producer");
        assert_eq!(payload["gaps"][0]["producer"], "astro");
        assert_eq!(payload["gaps"][0]["reason"], "initialize failed");
    }

    #[test]
    fn scope_filter_keeps_only_in_scope_authoritative_findings() {
        let mut collection = collection();
        collection.diagnostics.push(CollectedDiagnostic {
            diagnostic: StoredDiagnostic {
                file: PathBuf::from("/repo/other/outside.rs"),
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 2,
                severity: DiagnosticSeverity::Error,
                message: "outside scope".into(),
                code: None,
                source: None,
            },
            provisional: false,
        });
        collection.diagnostics.push(CollectedDiagnostic {
            diagnostic: StoredDiagnostic {
                file: PathBuf::from("/repo/src/main.rs"),
                line: 9,
                column: 1,
                end_line: 9,
                end_column: 2,
                severity: DiagnosticSeverity::Warning,
                message: "warming lead".into(),
                code: None,
                source: None,
            },
            provisional: true,
        });

        let scope = JobScope::from_roots(
            PathBuf::from("/repo"),
            vec![PathBuf::from("/repo/src/main.rs")],
        );
        collection.apply_scope(&scope);

        assert_eq!(
            collection.diagnostics.len(),
            1,
            "scope must drop out-of-scope rows and warming rows"
        );
        assert_eq!(
            collection.diagnostics[0].diagnostic.file,
            PathBuf::from("/repo/src/main.rs")
        );
        assert!(!collection.diagnostics[0].provisional);
    }

    #[test]
    fn scope_coverage_gap_renders_a_named_incomplete_file() {
        let mut collection = collection();
        collection.scope_coverage_gaps.push(ScopedCoverageGap {
            file: PathBuf::from("/repo/src/lib.rs"),
            reason: "no LSP producer has a current diagnostic report for this file",
        });

        let payload = collection.into_payload(&snapshot());
        assert_eq!(payload["complete"], false);
        let gap = payload["gaps"]
            .as_array()
            .and_then(|gaps| gaps.iter().find(|gap| gap["kind"] == "uncovered_file"))
            .expect("uncovered_file gap");
        assert_eq!(gap["file"], "src/lib.rs");
        assert_eq!(
            gap["reason"],
            "no LSP producer has a current diagnostic report for this file"
        );
    }
}

#[cfg(test)]
mod environmental_count_tests {
    use std::path::PathBuf;

    use super::{severity_counts, CollectedDiagnostic};
    use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};

    fn diag(line: u32, message: &str) -> StoredDiagnostic {
        StoredDiagnostic {
            file: PathBuf::from("/repo/src/mixed.ts"),
            line,
            column: 1,
            end_line: line,
            end_column: 2,
            severity: DiagnosticSeverity::Error,
            message: message.into(),
            code: None,
            source: None,
        }
    }

    #[test]
    fn severity_counts_exclude_environmental_on_same_file() {
        let diagnostics = vec![
            CollectedDiagnostic {
                diagnostic: diag(1, "Cannot find name 'x'."),
                provisional: false,
            },
            CollectedDiagnostic {
                diagnostic: diag(
                    2,
                    "Failed to load schema from https://example.com/schema.json",
                ),
                provisional: false,
            },
        ];
        let (errors, warnings, _, _) = severity_counts(&diagnostics);
        assert_eq!(
            errors, 1,
            "inspect summary must count only non-environmental errors"
        );
        assert_eq!(warnings, 0);
    }
}

#[cfg(test)]
mod environmental_render_tests {
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    use super::diagnostic_item;
    use crate::config::Config;
    use crate::inspect::job::InspectSnapshot;
    use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};
    use crate::parser::SymbolCache;

    fn snapshot() -> InspectSnapshot {
        InspectSnapshot::new(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/.aft"),
            Arc::new(Config::default()),
            Arc::new(RwLock::new(SymbolCache::new())),
        )
    }

    fn stored(message: &str) -> StoredDiagnostic {
        StoredDiagnostic {
            file: PathBuf::from("/repo/package.json"),
            line: 2,
            column: 5,
            end_line: 2,
            end_column: 6,
            severity: DiagnosticSeverity::Error,
            message: message.into(),
            code: None,
            source: Some("json".into()),
        }
    }

    #[test]
    fn detail_row_tags_environmental_message() {
        let item = diagnostic_item(
            &snapshot(),
            &super::CollectedDiagnostic {
                diagnostic: stored("Failed to load schema from https://example.com/schema.json"),
                provisional: false,
            },
        );
        assert_eq!(
            item["message"].as_str(),
            Some("Failed to load schema from https://example.com/schema.json [environmental]")
        );
    }

    #[test]
    fn detail_row_leaves_real_errors_untagged() {
        let item = diagnostic_item(
            &snapshot(),
            &super::CollectedDiagnostic {
                diagnostic: stored("Cannot find name 'typo'."),
                provisional: false,
            },
        );
        assert_eq!(item["message"].as_str(), Some("Cannot find name 'typo'."));
    }

    #[test]
    fn detail_row_tags_warming_diagnostics() {
        let item = diagnostic_item(
            &snapshot(),
            &super::CollectedDiagnostic {
                diagnostic: stored("temporary analyzer result"),
                provisional: true,
            },
        );
        assert_eq!(
            item["message"].as_str(),
            Some("temporary analyzer result (analyzer warming)")
        );
    }
}
