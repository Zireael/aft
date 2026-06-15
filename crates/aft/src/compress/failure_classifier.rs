//! Bash failure classifier, raw log retention, structured passthrough, and output dedupe.
//!
//! Classifies bash command failures into actionable categories, preserves raw log paths,
//! passes through structured JSON output, and deduplicates repeated identical lines.

/// Failure classification for bash command output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FailureClass {
    /// Test failure (assertion, panic, test harness output).
    Test,
    /// Build/typecheck failure (compiler error, type error).
    Build,
    /// Lint failure (clippy, eslint, ruff, etc.).
    Lint,
    /// Dependency failure (missing package, version conflict).
    Dependency,
    /// Command not found.
    CommandNotFound,
    /// Permission denied.
    PermissionDenied,
    /// Timeout.
    Timeout,
    /// Network failure (connection refused, DNS, SSL).
    Network,
    /// Out of memory.
    Oom,
    /// Unknown failure.
    Unknown,
}

impl FailureClass {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Build => "build",
            Self::Lint => "lint",
            Self::Dependency => "dependency",
            Self::CommandNotFound => "command_not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Oom => "oom",
            Self::Unknown => "unknown",
        }
    }

    /// Suggest a next action based on failure class.
    pub fn next_action(&self) -> &'static str {
        match self {
            Self::Test => "run the specific failing test to verify the fix",
            Self::Build => "check the compiler/type errors and fix them",
            Self::Lint => "review the lint violations and apply fixes",
            Self::Dependency => "check package.json/Cargo.toml for version conflicts",
            Self::CommandNotFound => "install the missing tool or check PATH",
            Self::PermissionDenied => "check file permissions or run with appropriate user",
            Self::Timeout => "increase timeout or optimize the command",
            Self::Network => "check network connectivity and proxy settings",
            Self::Oom => "reduce memory usage or increase available memory",
            Self::Unknown => "review the full log for details",
        }
    }
}

/// Classified failure information extracted from command output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailureInfo {
    /// Classified failure type.
    pub class: FailureClass,
    /// Extracted file:line evidence (if any).
    pub evidence: Vec<FileLineEvidence>,
    /// Raw log file path (when output was compressed).
    pub raw_log_path: Option<String>,
    /// Whether the output was compressed.
    pub was_compressed: bool,
    /// Lines removed by deduplication.
    pub deduped_lines: u32,
    /// Next action suggestion.
    pub next_action: &'static str,
}

/// File:line evidence extracted from failure output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileLineEvidence {
    /// File path.
    pub file: String,
    /// Line number (1-based, if present).
    pub line: Option<u32>,
    /// Column number (1-based, if present).
    pub column: Option<u32>,
    /// Context text around the evidence.
    pub context: Option<String>,
}

/// Classify bash command output into a failure class.
///
/// Analyzes the output text for patterns indicating specific failure types.
pub fn classify_failure(output: &str) -> FailureClass {
    let lower = output.to_lowercase();

    // Test failures
    if lower.contains("test result:")
        || lower.contains("FAILED")
        || lower.contains("assertion")
        || lower.contains("panicked at")
        || lower.contains("thread .* panicked")
        || lower.contains("assertion `left == right` failed")
    {
        return FailureClass::Test;
    }

    // Build/typecheck failures
    if lower.contains("error[")
        || lower.contains("error:")
        || lower.contains("compilation failed")
        || lower.contains("type error")
        || lower.contains("cannot find")
        || lower.contains("no such file or directory")
    {
        return FailureClass::Build;
    }

    // Lint failures
    if lower.contains("warning:")
        || lower.contains("lint error")
        || lower.contains("clippy")
        || lower.contains("eslint")
        || lower.contains("ruff")
        || lower.contains("pylint")
    {
        return FailureClass::Lint;
    }

    // Dependency failures
    if lower.contains("could not resolve")
        || lower.contains("version conflict")
        || lower.contains("no matching version")
        || lower.contains("package not found")
        || lower.contains("module not found")
    {
        return FailureClass::Dependency;
    }

    // Command not found
    if lower.contains("command not found")
        || lower.contains("not found in PATH")
        || lower.contains("no such command")
    {
        return FailureClass::CommandNotFound;
    }

    // Permission denied
    if lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("eacces")
    {
        return FailureClass::PermissionDenied;
    }

    // Timeout
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline exceeded")
    {
        return FailureClass::Timeout;
    }

    // Network failures
    if lower.contains("connection refused")
        || lower.contains("network unreachable")
        || lower.contains("dns resolution")
        || lower.contains("ssl")
        || lower.contains("certificate")
    {
        return FailureClass::Network;
    }

    // OOM
    if lower.contains("out of memory")
        || lower.contains("oom")
        || lower.contains("memory allocation failed")
    {
        return FailureClass::Oom;
    }

    FailureClass::Unknown
}

/// Extract file:line evidence from failure output.
///
/// Looks for common patterns:
/// - `path/to/file.rs:42:10`
/// - `at path/to/file.rs:42`
/// - `error in path/to/file.rs line 42`
pub fn extract_file_line_evidence(output: &str) -> Vec<FileLineEvidence> {
    let mut evidence = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in output.lines() {
        // Pattern 1: file:line:col (e.g. " --> src/main.rs:10:5")
        let has_extension = line.contains(".rs:")
            || line.contains(".ts:")
            || line.contains(".js:")
            || line.contains(".py:")
            || line.contains(".go:");

        if has_extension {
            // Split on whitespace and look for tokens containing .ext:NN[:col]
            // For "src/main.rs:10:5", split on ':' gives ["src/main.rs", "10", "5"]
            for word in line.split_whitespace() {
                // Strip leading punctuation like "--> " or "at "
                let clean = word.trim_start_matches(|c: char| {
                    !c.is_ascii_alphanumeric() && c != '_' && c != '/' && c != '.' && c != '-'
                });
                let parts: Vec<&str> = clean.split(':').collect();
                if parts.len() >= 2 {
                    // Walk backwards from second-to-last to find the file path
                    for i in (1..parts.len()).rev() {
                        let candidate = parts[..i].join(":");
                        if (candidate.ends_with(".rs")
                            || candidate.ends_with(".ts")
                            || candidate.ends_with(".js")
                            || candidate.ends_with(".py")
                            || candidate.ends_with(".go"))
                            && seen.insert(candidate.to_string())
                        {
                            let line_num = parts.get(i).and_then(|s| s.parse::<u32>().ok());
                            let col_num = parts.get(i + 1).and_then(|s| s.parse::<u32>().ok());
                            evidence.push(FileLineEvidence {
                                file: candidate,
                                line: line_num,
                                column: col_num,
                                context: Some(line.trim().to_string()),
                            });
                            break;
                        }
                    }
                }
            }
        }

        // Pattern 2: at path/to/file:line
        if let Some(at_pos) = line.find(" at ") {
            let after_at = &line[at_pos + 4..];
            if let Some(colon_pos) = after_at.find(':') {
                let file_path = &after_at[..colon_pos];
                let rest = &after_at[colon_pos + 1..];
                let line_num: Option<u32> = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|s| s.parse().ok());

                if !file_path.is_empty() && seen.insert(file_path.to_string()) {
                    evidence.push(FileLineEvidence {
                        file: file_path.to_string(),
                        line: line_num,
                        column: None,
                        context: Some(line.trim().to_string()),
                    });
                }
            }
        }

        // Pattern 3: simple file.rs:line
        for word in line.split_whitespace() {
            if let Some(colon_pos) = word.rfind(':') {
                let path = &word[..colon_pos];
                let num_str = &word[colon_pos + 1..];
                if let Ok(num) = num_str.parse::<u32>() {
                    if (path.ends_with(".rs")
                        || path.ends_with(".ts")
                        || path.ends_with(".js")
                        || path.ends_with(".py")
                        || path.ends_with(".go"))
                        && seen.insert(path.to_string())
                    {
                        evidence.push(FileLineEvidence {
                            file: path.to_string(),
                            line: Some(num),
                            column: None,
                            context: Some(line.trim().to_string()),
                        });
                    }
                }
            }
        }
    }

    evidence
}

/// Deduplicate repeated identical lines in output, keeping count.
///
/// Returns (deduped_output, lines_removed).
pub fn dedupe_output(output: &str) -> (String, u32) {
    let lines: Vec<&str> = output.lines().collect();
    let mut deduped: Vec<String> = Vec::new();
    let mut removed = 0u32;
    let mut prev: Option<&str> = None;
    let mut repeat_count = 0u32;

    for line in &lines {
        if Some(*line) == prev {
            repeat_count += 1;
        } else {
            if repeat_count > 1 {
                // Replace repeated lines with a summary
                deduped.push(format!("  ... (repeated {} times)", repeat_count));
                removed += repeat_count - 1;
            }
            repeat_count = 1;
            prev = Some(line);
        }
    }

    // Handle final repeat
    if repeat_count > 1 {
        deduped.push(format!("  ... (repeated {} times)", repeat_count));
        removed += repeat_count - 1;
    }

    (deduped.join("\n"), removed)
}

/// Check if output is structured (JSON, YAML, TOML).
pub fn is_structured_output(output: &str) -> bool {
    let trimmed = output.trim_start();
    trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.starts_with("---")
        || trimmed.starts_with("```")
}

/// Process bash output: classify failure, extract evidence, dedupe.
///
/// Returns failure info if the output contains a failure, or None for success.
pub fn process_bash_output(
    output: &str,
    _command: Option<&str>,
    raw_log_path: Option<String>,
) -> Option<FailureInfo> {
    let class = classify_failure(output);

    // If unknown and no obvious failure patterns, return None (success).
    // Use `is_unknown_failure_pattern` to avoid false positives on benign
    // text that happens to contain substrings like "error".
    if class == FailureClass::Unknown && !is_unknown_failure_pattern(output) {
        return None;
    }

    let evidence = extract_file_line_evidence(output);
    let (_deduped, deduped_lines) = dedupe_output(output);

    Some(FailureInfo {
        class,
        evidence,
        raw_log_path,
        was_compressed: deduped_lines > 0,
        deduped_lines,
        next_action: class.next_action(),
    })
}

/// Check if output matches a pattern that indicates an unknown-class failure.
fn is_unknown_failure_pattern(output: &str) -> bool {
    let lower = output.to_lowercase();
    // These patterns are strong enough to indicate failure even without
    // matching a specific class.
    lower.contains("exit code")
        || lower.contains("fatal:")
        || lower.contains("error:")
        || lower.contains(" failed")
        || lower.starts_with("error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_test_failure() {
        let output = "test result: FAILED. 0 passed; 1 failed; 0 ignored";
        assert_eq!(classify_failure(output), FailureClass::Test);
    }

    #[test]
    fn classify_build_failure() {
        let output = "error[E0308]: mismatched types\n --> src/main.rs:10:5";
        assert_eq!(classify_failure(output), FailureClass::Build);
    }

    #[test]
    fn classify_lint_failure() {
        let output = "warning: unused variable `x`\n --> src/main.rs:10:5";
        assert_eq!(classify_failure(output), FailureClass::Lint);
    }

    #[test]
    fn classify_command_not_found() {
        let output = "bash: foo: command not found";
        assert_eq!(classify_failure(output), FailureClass::CommandNotFound);
    }

    #[test]
    fn classify_permission_denied() {
        let output = "Permission denied: '/etc/shadow'";
        assert_eq!(classify_failure(output), FailureClass::PermissionDenied);
    }

    #[test]
    fn classify_timeout() {
        let output = "Command timed out after 30s";
        assert_eq!(classify_failure(output), FailureClass::Timeout);
    }

    #[test]
    fn classify_oom() {
        let output = "Out of memory: Cannot allocate memory";
        assert_eq!(classify_failure(output), FailureClass::Oom);
    }

    #[test]
    fn extract_evidence_from_rust_error() {
        let output = "error[E0308]: mismatched types\n --> src/main.rs:10:5";
        let evidence = extract_file_line_evidence(output);
        assert!(!evidence.is_empty());
        assert!(evidence[0].file.contains("main.rs"));
    }

    #[test]
    fn dedupe_removes_repeated_lines() {
        let output = "line1\nline2\nline2\nline2\nline3";
        let (deduped, removed) = dedupe_output(output);
        assert!(removed >= 1);
        assert!(deduped.contains("repeated"));
    }

    #[test]
    fn structured_output_detected() {
        assert!(is_structured_output(r#"{"key": "value"}"#));
        assert!(is_structured_output(r#"[1, 2, 3]"#));
        assert!(!is_structured_output("just a plain text output"));
    }

    #[test]
    fn next_actions_provided() {
        assert!(!FailureClass::Test.next_action().is_empty());
        assert!(!FailureClass::Build.next_action().is_empty());
        assert!(!FailureClass::Unknown.next_action().is_empty());
    }

    #[test]
    fn unknown_failure_with_no_error_returns_none() {
        let output = "Everything looks good, no errors here.";
        assert!(process_bash_output(output, None, None).is_none());
    }

    #[test]
    fn failure_with_evidence_and_dedup() {
        let output = "error: something failed\n --> src/main.rs:10:5\nerror: something failed\n --> src/main.rs:10:5";
        let info = process_bash_output(output, None, Some("/tmp/log.txt".to_string()));
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.class, FailureClass::Build);
        assert!(!info.evidence.is_empty());
        assert!(info.raw_log_path.is_some());
    }
}
