//! Which TypeScript a project has installed, and which language server can
//! serve it.
//!
//! TypeScript 6 and earlier ship `lib/tsserver.js`, which
//! `typescript-language-server` loads. TypeScript 7 and later are the native
//! (Go) compiler: they ship no `tsserver.js` at all, but the compiler binary
//! serves LSP itself with `tsc --lsp --stdio`. The binary lives in a
//! per-platform optional dependency, `@typescript/typescript-<os>-<cpu>`, next
//! to the `typescript` package.
//!
//! The decision is always made from what is installed in `node_modules`, never
//! from a lockfile: after a branch switch without an install, the lockfile
//! names what should be installed while `node_modules` holds what actually
//! runs, and starting the server that matches the lockfile would fail against
//! the installed compiler.

use std::path::{Path, PathBuf};

use super::environmental::TS_NATIVE_NO_TSSERVER;

/// The `serverInfo.name` the native TypeScript language server reports in its
/// `initialize` response.
pub(crate) const NATIVE_SERVER_INFO_NAME: &str = "typescript-go";

/// Arguments that put the native compiler binary into language-server mode.
/// The mode is not listed in `tsc --help`, but `tsc --lsp --help` documents it.
pub(crate) const NATIVE_SERVER_ARGS: [&str; 2] = ["--lsp", "--stdio"];

/// The TypeScript package a project has installed, read from the nearest
/// `node_modules/typescript/package.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectTypeScript {
    /// The package.json `version`, or "unknown" when it has none.
    pub(crate) version: String,
    /// The `node_modules/typescript` directory.
    pub(crate) package_dir: PathBuf,
}

impl ProjectTypeScript {
    /// Leading integer of the version ("7.0.2" and "7.1.0-dev.1" are both 7).
    pub(crate) fn major(&self) -> Option<u64> {
        let digits: String = self
            .version
            .trim()
            .trim_start_matches('v')
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    }

    /// TypeScript 7 and later are the native (Go) compiler, which ships no
    /// tsserver.js for typescript-language-server to load.
    pub(crate) fn is_native_compiler(&self) -> bool {
        self.major().is_some_and(|major| major >= 7)
    }

    /// Whether the package ships the `lib/tsserver.js` that
    /// typescript-language-server loads.
    pub(crate) fn has_tsserver(&self) -> bool {
        self.package_dir.join("lib").join("tsserver.js").is_file()
    }

    /// Read the package at `package_dir` (a `node_modules/typescript`
    /// directory). `None` when it has no package.json.
    pub(crate) fn read(package_dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(package_dir.join("package.json")).ok()?;
        let version = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|json| {
                json.get("version")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown".into());
        Some(Self {
            version,
            package_dir: package_dir.to_path_buf(),
        })
    }
}

/// Find the project's own TypeScript package for `source_file`, walking up to
/// `project_root`. It looks for `package.json` rather than `lib/*.js` because
/// TypeScript 7 ships none of the JavaScript files the SDK probe expects.
pub(crate) fn find_project_typescript_package(
    source_file: &Path,
    project_root: &Path,
) -> Option<ProjectTypeScript> {
    let mut directory = source_file.parent()?;
    loop {
        if let Some(found) =
            ProjectTypeScript::read(&directory.join("node_modules").join("typescript"))
        {
            return Some(found);
        }
        if directory == project_root {
            return None;
        }
        let parent = directory.parent()?;
        if !parent.starts_with(project_root) {
            return None;
        }
        directory = parent;
    }
}

/// The project's TypeScript for `source_file`, served from `server_root`.
///
/// The walk is bounded by the configured project root when the file is inside
/// it, and by the server root otherwise, the same boundary the SDK lookup for
/// `typescript-language-server` uses. Paths are canonicalized first so a
/// symlinked temp or checkout path still compares equal to its root.
pub(crate) fn project_typescript_for(
    source_file: &Path,
    server_root: &Path,
    project_root: Option<&Path>,
) -> Option<ProjectTypeScript> {
    let source_file = crate::inspect::job::canonicalize_normalized(source_file);
    let server_root = crate::inspect::job::canonicalize_normalized(server_root);
    let project_root = project_root.map(crate::inspect::job::canonicalize_normalized);
    let boundary = project_root
        .as_deref()
        .filter(|root| source_file.starts_with(root))
        .unwrap_or(&server_root);
    find_project_typescript_package(&source_file, boundary)
}

/// The npm name of the platform package that carries the native compiler for
/// the running machine, in Node's `process.platform`/`process.arch` spelling
/// (`@typescript/typescript-darwin-arm64`). `None` on a platform Node has no
/// name for, where no such package can exist.
pub(crate) fn native_platform_package() -> Option<String> {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "solaris" | "illumos" => "sunos",
        os @ ("linux" | "freebsd" | "netbsd" | "openbsd" | "aix") => os,
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        "loongarch64" => "loong64",
        "powerpc64" => "ppc64",
        "mips64" if cfg!(target_endian = "little") => "mips64el",
        arch @ ("arm" | "riscv64" | "s390x") => arch,
        _ => return None,
    };
    Some(format!("@typescript/typescript-{platform}-{arch}"))
}

/// Locate the native language-server binary for a TypeScript 7+ package.
///
/// This mirrors the package's own `lib/getExePath.js`: resolve the platform
/// package the way Node resolves a bare specifier from inside the
/// `typescript` package, then use its `lib/tsc` (`tsc.exe` on Windows).
/// Resolution starts from the package's real path because Bun's isolated
/// linker and pnpm install `node_modules/typescript` as a symlink into a
/// store, and the platform package sits next to the real directory, not the
/// link. AFT starts the binary directly rather than through
/// `node_modules/.bin/tsc`, a `#!/usr/bin/env node` wrapper that machines
/// with only Bun cannot run.
///
/// The error names the missing package and where it was looked for.
pub(crate) fn resolve_native_binary(project: &ProjectTypeScript) -> Result<PathBuf, String> {
    let Some(platform_package) = native_platform_package() else {
        return Err(format!(
            "no native TypeScript binary is published for this platform ({}-{})",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    };
    let real_dir = crate::inspect::job::canonicalize_normalized(&project.package_dir);
    let binary_name = if cfg!(windows) { "tsc.exe" } else { "tsc" };
    for ancestor in real_dir.ancestors() {
        // Node looks in `<dir>/node_modules` for every ancestor, except that
        // a directory that is itself named node_modules is searched directly.
        let modules = if ancestor
            .file_name()
            .is_some_and(|name| name == "node_modules")
        {
            ancestor.to_path_buf()
        } else {
            ancestor.join("node_modules")
        };
        let package = modules.join(&platform_package);
        if package.join("package.json").is_file() {
            let binary = package.join("lib").join(binary_name);
            if binary.is_file() {
                return Ok(binary);
            }
            return Err(format!(
                "{platform_package} is installed at {} but has no lib/{binary_name}",
                package.display()
            ));
        }
    }
    Err(format!(
        "{platform_package} (the optional dependency that carries the native compiler) was not found from {}; the install skipped optional dependencies or this platform is unsupported",
        real_dir.display()
    ))
}

/// Named gap for a TypeScript 7+ project whose native language server could
/// not be started. Nothing was spawned: `typescript-language-server` is known
/// to fail on the native compiler, and installing dependencies with LSP
/// auto-install cannot add a tsserver.js that TypeScript 7 never ships.
pub(crate) fn native_server_unavailable_reason(project: &ProjectTypeScript, cause: &str) -> String {
    format!(
        "TypeScript unavailable: this project uses TypeScript {} (native compiler) at {}, {TS_NATIVE_NO_TSSERVER}, and AFT could not start its native language server: {cause}. Run the project's tsc --noEmit for type errors.",
        project.version,
        project.package_dir.display(),
    )
}

/// Named gap for a native server binary whose `initialize` response did not
/// identify it as `typescript-go`. AFT shuts it down rather than trust
/// diagnostics from a program it cannot vouch for.
pub(crate) fn native_server_misidentified_reason(
    project: Option<&ProjectTypeScript>,
    package_dir: &Path,
    binary: &Path,
    reported: Option<&str>,
) -> String {
    let version = project.map_or("unknown", |project| project.version.as_str());
    let reported = reported.map_or_else(
        || "reported no serverInfo name".to_string(),
        |name| format!("identified itself as \"{name}\""),
    );
    format!(
        "TypeScript unavailable: the native language server {} for TypeScript {version} at {} {reported} rather than {NATIVE_SERVER_INFO_NAME}, so AFT stopped it instead of trusting its diagnostics. Run the project's tsc --noEmit for type errors.",
        binary.display(),
        package_dir.display(),
    )
}

/// Named gap for a project TypeScript that neither server can serve: its
/// version is unreadable, or it is TypeScript 6 or earlier without the
/// `lib/tsserver.js` typescript-language-server needs. Nothing was spawned.
pub(crate) fn unservable_typescript_reason(project: &ProjectTypeScript) -> String {
    if project.major().is_none() {
        return format!(
            "TypeScript unavailable: the project's TypeScript at {} reports version \"{}\", which AFT cannot read, so it cannot tell whether typescript-language-server (TypeScript 6 and earlier) or the native language server (TypeScript 7 and later) serves it. Check the installed package; run the project's tsc --noEmit for type errors.",
            project.package_dir.display(),
            project.version,
        );
    }
    format!(
        "TypeScript unavailable: the project's TypeScript {} at {} has no lib/tsserver.js, so typescript-language-server can't serve it, and TypeScript before 7 has no native language server. The installation looks incomplete: reinstall the project's dependencies, or run the project's tsc --noEmit for type errors.",
        project.version,
        project.package_dir.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn major_version_is_the_leading_integer() {
        let ts = |version: &str| ProjectTypeScript {
            version: version.into(),
            package_dir: PathBuf::from("/repo/node_modules/typescript"),
        };
        assert_eq!(ts("7.0.2").major(), Some(7));
        assert_eq!(ts("7.1.0-dev.20260929.1").major(), Some(7));
        assert_eq!(ts("5.9.3").major(), Some(5));
        assert_eq!(ts("unknown").major(), None);
        assert!(ts("10.0.0").is_native_compiler());
        assert!(!ts("6.0.1").is_native_compiler());
        assert!(!ts("next").is_native_compiler());
    }

    #[test]
    fn platform_package_uses_node_spelling() {
        let name = native_platform_package().expect("test hosts have a published platform");
        assert!(name.starts_with("@typescript/typescript-"), "{name}");
        assert!(
            !name.contains("macos") && !name.contains("aarch64"),
            "{name}"
        );
        assert!(
            !name.contains("x86_64") && !name.contains("windows"),
            "{name}"
        );
    }

    fn native_install(modules: &Path, version: &str) -> PathBuf {
        let package_dir = modules.join("typescript");
        write(
            &package_dir.join("package.json"),
            &format!(r#"{{"name":"typescript","version":"{version}"}}"#),
        );
        let platform = modules.join(native_platform_package().unwrap());
        write(&platform.join("package.json"), "{}");
        let binary = platform
            .join("lib")
            .join(if cfg!(windows) { "tsc.exe" } else { "tsc" });
        write(&binary, "");
        package_dir
    }

    #[test]
    fn native_binary_is_found_next_to_a_hoisted_package() {
        let temp = tempfile::tempdir().unwrap();
        let modules = temp.path().join("node_modules");
        let package_dir = native_install(&modules, "7.0.2");
        let project = ProjectTypeScript::read(&package_dir).unwrap();
        let binary = resolve_native_binary(&project).unwrap();
        assert!(binary.ends_with(
            Path::new(&native_platform_package().unwrap())
                .join("lib")
                .join(if cfg!(windows) { "tsc.exe" } else { "tsc" })
        ));
    }

    /// Bun's isolated linker and pnpm link `node_modules/typescript` into a
    /// store; the platform package is a sibling of the real directory only.
    #[cfg(unix)]
    #[test]
    fn native_binary_is_resolved_from_the_real_path_of_a_linked_package() {
        let temp = tempfile::tempdir().unwrap();
        let store_modules = temp
            .path()
            .join("node_modules")
            .join(".bun")
            .join("typescript@7.0.2")
            .join("node_modules");
        let real_package = native_install(&store_modules, "7.0.2");
        let project_modules = temp.path().join("app").join("node_modules");
        std::fs::create_dir_all(&project_modules).unwrap();
        std::os::unix::fs::symlink(&real_package, project_modules.join("typescript")).unwrap();

        let project = ProjectTypeScript::read(&project_modules.join("typescript")).unwrap();
        let binary = resolve_native_binary(&project).unwrap();
        assert!(
            binary.starts_with(crate::inspect::job::canonicalize_normalized(&store_modules)),
            "{}",
            binary.display()
        );
    }

    #[test]
    fn missing_platform_package_names_the_package_and_where_it_looked() {
        let temp = tempfile::tempdir().unwrap();
        let package_dir = temp.path().join("node_modules").join("typescript");
        write(
            &package_dir.join("package.json"),
            r#"{"name":"typescript","version":"7.0.2"}"#,
        );
        let project = ProjectTypeScript::read(&package_dir).unwrap();
        let cause = resolve_native_binary(&project).unwrap_err();
        assert!(
            cause.contains(&native_platform_package().unwrap()),
            "{cause}"
        );
        assert!(cause.contains("was not found from"), "{cause}");
        let reason = native_server_unavailable_reason(&project, &cause);
        assert!(
            reason.starts_with(
                "TypeScript unavailable: this project uses TypeScript 7.0.2 (native compiler)"
            ),
            "{reason}"
        );
        assert!(!reason.contains("bun install"), "{reason}");
        assert!(!reason.contains("auto-install"), "{reason}");
    }

    #[test]
    fn unservable_reasons_name_the_version_and_path() {
        let unreadable = ProjectTypeScript {
            version: "unknown".into(),
            package_dir: PathBuf::from("/repo/node_modules/typescript"),
        };
        let reason = unservable_typescript_reason(&unreadable);
        assert!(reason.contains("reports version \"unknown\""), "{reason}");
        assert!(reason.contains("/repo/node_modules/typescript"), "{reason}");

        let broken = ProjectTypeScript {
            version: "5.9.3".into(),
            ..unreadable
        };
        let reason = unservable_typescript_reason(&broken);
        assert!(
            reason.contains(
                "TypeScript 5.9.3 at /repo/node_modules/typescript has no lib/tsserver.js"
            ),
            "{reason}"
        );
    }

    #[test]
    fn detection_walks_from_the_file_and_stops_at_the_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        native_install(&root.join("node_modules"), "7.0.2");
        let file = root
            .join("packages")
            .join("app")
            .join("src")
            .join("index.ts");
        write(&file, "");
        let found = project_typescript_for(&file, &root.join("packages").join("app"), Some(&root))
            .expect("hoisted TypeScript above the server root but inside the project");
        assert_eq!(found.version, "7.0.2");

        // A nearer install wins over the hoisted one.
        let nearer = root
            .join("packages")
            .join("app")
            .join("node_modules")
            .join("typescript");
        write(&nearer.join("package.json"), r#"{"version":"5.9.3"}"#);
        let found =
            project_typescript_for(&file, &root.join("packages").join("app"), Some(&root)).unwrap();
        assert_eq!(found.version, "5.9.3");

        // Nothing above the project root is considered.
        native_install(&temp.path().join("node_modules"), "7.0.2");
        let outside = temp.path().join("other").join("src").join("index.ts");
        write(&outside, "");
        let project = temp.path().join("other");
        assert_eq!(
            project_typescript_for(&outside, &project, Some(&project)),
            None
        );
    }
}
