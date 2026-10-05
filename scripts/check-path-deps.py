#!/usr/bin/env python3
"""Refuse Cargo and npm dependencies that escape the repository tree."""

from __future__ import annotations

import argparse
import glob
import json
import os
from pathlib import Path, PureWindowsPath
import sys
import tempfile
from typing import Any

try:
    import tomllib
except ImportError:  # Python 3.9 and 3.10 are still common on developer machines.
    tomllib = None

SKIP_DIRS = {".git", "node_modules", "target"}
CARGO_DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
NPM_DEPENDENCY_TABLES = ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies")


def _strip_toml_comment(line: str) -> str:
    quote = ""
    escaped = False
    for index, character in enumerate(line):
        if quote == '"':
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = ""
        elif quote:
            if character == quote:
                quote = ""
        elif character in ('"', "'"):
            quote = character
        elif character == "#":
            return line[:index]
    return line


def _toml_key_parts(source: str) -> list[str]:
    parts: list[str] = []
    start = 0
    quote = ""
    escaped = False
    for index, character in enumerate(source):
        if quote == '"':
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = ""
        elif quote:
            if character == quote:
                quote = ""
        elif character in ('"', "'"):
            quote = character
        elif character == ".":
            raw = source[start:index].strip()
            parts.append(_toml_string(raw) if raw.startswith(('"', "'")) else raw)
            start = index + 1
    raw = source[start:].strip()
    if raw:
        parts.append(_toml_string(raw) if raw.startswith(('"', "'")) else raw)
    return parts


def _toml_string(source: str) -> str:
    source = source.strip()
    if source.startswith('"') and source.endswith('"'):
        try:
            return json.loads(source)
        except json.JSONDecodeError:
            return source[1:-1]
    if source.startswith("'") and source.endswith("'"):
        return source[1:-1]
    return source


def _top_level_split(source: str, separator: str) -> list[str]:
    pieces: list[str] = []
    start = 0
    quote = ""
    escaped = False
    depth = 0
    for index, character in enumerate(source):
        if quote == '"':
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = ""
        elif quote:
            if character == quote:
                quote = ""
        elif character in ('"', "'"):
            quote = character
        elif character in "[{(":
            depth += 1
        elif character in "]})":
            depth -= 1
        elif character == separator and depth == 0:
            pieces.append(source[start:index])
            start = index + 1
    pieces.append(source[start:])
    return pieces


def _inline_path(value: str) -> str | None:
    value = value.strip()
    if not (value.startswith("{") and value.endswith("}")):
        return None
    for field in _top_level_split(value[1:-1], ","):
        assignment = _top_level_split(field, "=")
        if len(assignment) == 2 and _toml_key_parts(assignment[0]) == ["path"]:
            raw = assignment[1].strip()
            if raw.startswith(('"', "'")):
                return _toml_string(raw)
            return raw
    return None


def _scope_for_table(table: list[str]) -> tuple[list[str], str] | None:
    if len(table) == 1 and table[0] in CARGO_DEPENDENCY_TABLES:
        return table, f"[{table[0]}]"
    if len(table) == 2 and table[0] == "workspace" and table[1] == "dependencies":
        return table, "[workspace.dependencies]"
    if len(table) == 3 and table[0] == "target" and table[2] in CARGO_DEPENDENCY_TABLES:
        return table, f"[target.{table[1]}.{table[2]}]"
    if len(table) == 2 and table[0] == "patch":
        return table, f"[patch.{table[1]}]"
    if table == ["replace"]:
        return table, "[replace]"
    return None


def _toml_string_array(source: str) -> list[str]:
    source = source.strip()
    if not (source.startswith("[") and source.endswith("]")):
        return []
    return [
        _toml_string(value.strip())
        for value in _top_level_split(source[1:-1], ",")
        if value.strip().startswith(('"', "'"))
    ]


def fallback_cargo_data(
    manifest: Path,
) -> tuple[list[tuple[str, str, str]], list[tuple[str, str]]]:
    """Read dependency path fields on Python versions without stdlib tomllib."""
    references: list[tuple[str, str, str]] = []
    workspace_patterns: list[tuple[str, str]] = []
    lines = manifest.read_text(encoding="utf-8").splitlines()
    table: list[str] = []
    index = 0
    while index < len(lines):
        line = _strip_toml_comment(lines[index]).strip()
        index += 1
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            header = line[2:-2] if line.startswith("[[") else line[1:-1]
            table = _toml_key_parts(header)
            continue
        assignment = _top_level_split(line, "=")
        if len(assignment) != 2:
            continue
        key_parts = _toml_key_parts(assignment[0])
        value = assignment[1].strip()
        # TOML permits whitespace and comments inside multiline arrays/inline tables.
        balance = 0
        quote = ""
        escaped = False
        for character in value:
            if quote == '"':
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == quote:
                    quote = ""
            elif quote:
                if character == quote:
                    quote = ""
            elif character in ('"', "'"):
                quote = character
            elif character in "[{":
                balance += 1
            elif character in "]}":
                balance -= 1
        while balance > 0 and index < len(lines):
            continuation = _strip_toml_comment(lines[index]).strip()
            index += 1
            value += " " + continuation
            for character in continuation:
                if quote == '"':
                    if escaped:
                        escaped = False
                    elif character == "\\":
                        escaped = True
                    elif character == quote:
                        quote = ""
                elif quote:
                    if character == quote:
                        quote = ""
                elif character in ('"', "'"):
                    quote = character
                elif character in "[{":
                    balance += 1
                elif character in "]}":
                    balance -= 1

        scope = _scope_for_table(table)
        if scope is not None:
            path_value = _inline_path(value)
            if path_value is not None and key_parts:
                references.append((scope[1], ".".join(key_parts), path_value))
            elif len(key_parts) > 1 and key_parts[-1] == "path":
                references.append((scope[1], ".".join(key_parts[:-1]), _toml_string(value)))
            continue
        if table == ["workspace"] and key_parts in (["members"], ["exclude"]):
            workspace_table = f"[workspace.{key_parts[0]}]"
            workspace_patterns.extend(
                (workspace_table, pattern) for pattern in _toml_string_array(value)
            )
            continue
        dependency_scope = _scope_for_table(table[:-1]) if table else None
        if dependency_scope is not None:
            if key_parts == ["path"]:
                raw = value.strip()
                references.append((dependency_scope[1], table[-1], _toml_string(raw)))
    return references, workspace_patterns


def repository_manifests(root: Path) -> list[Path]:
    manifests: list[Path] = []
    for current, directories, files in os.walk(root, topdown=True, followlinks=False):
        directories[:] = sorted(name for name in directories if name not in SKIP_DIRS)
        for name in ("Cargo.toml", "package.json"):
            if name in files:
                manifests.append(Path(current) / name)
    return sorted(manifests)


def inside_repository(path: Path, root: Path) -> bool:
    try:
        path.resolve(strict=False).relative_to(root.resolve(strict=False))
        return True
    except (OSError, ValueError, RuntimeError):
        return False


def check_root(root: Path) -> tuple[list[str], int, int]:
    root = root.resolve(strict=True)
    manifests = repository_manifests(root)
    violations: list[str] = []
    checked_paths = 0

    def check_reference(manifest: Path, table: str, dependency: str, raw_path: Any) -> None:
        nonlocal checked_paths
        checked_paths += 1
        if not isinstance(raw_path, str):
            violations.append(
                f"{manifest.relative_to(root)}: table={table} dependency={dependency} "
                f"has a non-string path value {raw_path!r}"
            )
            return
        if os.name != "nt":
            windows_path = PureWindowsPath(raw_path)
            if windows_path.drive or windows_path.root:
                violations.append(
                    f"{manifest.relative_to(root)}: table={table} dependency={dependency} "
                    f"resolved=<Windows absolute path {raw_path}>"
                )
                return
        raw_paths = [raw_path]
        if os.name != "nt" and "\\" in raw_path:
            raw_paths.append(raw_path.replace("\\", "/"))
        try:
            resolved_paths = [(manifest.parent / path).resolve(strict=False) for path in raw_paths]
        except (OSError, RuntimeError) as error:
            violations.append(
                f"{manifest.relative_to(root)}: table={table} dependency={dependency} "
                f"cannot resolve path {raw_path!r}: {error}"
            )
            return
        resolved = next((path for path in resolved_paths if not inside_repository(path, root)), None)
        if resolved is not None:
            violations.append(
                f"{manifest.relative_to(root)}: table={table} dependency={dependency} "
                f"resolved={resolved}"
            )

    def check_workspace_patterns(manifest: Path, table: str, patterns: Any) -> None:
        nonlocal checked_paths
        if not isinstance(patterns, list):
            return
        for pattern in patterns:
            if not isinstance(pattern, str) or pattern.startswith("!"):
                continue
            checked_paths += 1
            if os.name != "nt":
                windows_pattern = PureWindowsPath(pattern)
                if windows_pattern.drive or windows_pattern.root:
                    violations.append(
                        f"{manifest.relative_to(root)}: table={table} dependency={pattern} "
                        f"resolved=<Windows absolute path {pattern}>"
                    )
                    continue
            patterns_to_check = [pattern]
            if os.name != "nt" and "\\" in pattern:
                patterns_to_check.append(pattern.replace("\\", "/"))
            bases = [manifest.parent / item for item in patterns_to_check]
            candidates = [
                Path(candidate)
                for item in patterns_to_check
                for candidate in glob.glob(str(manifest.parent / item), recursive=True)
            ]
            # Check the pattern even when it matches no package, and each match
            # so a symlink cannot hide an escape.
            outside_path: Path | None = None
            resolution_error: str | None = None
            for candidate in [*bases, *candidates]:
                try:
                    resolved = candidate.resolve(strict=False)
                except (OSError, RuntimeError) as error:
                    resolution_error = str(error)
                    break
                if not inside_repository(resolved, root):
                    outside_path = resolved
                    break
            if resolution_error is not None:
                violations.append(
                    f"{manifest.relative_to(root)}: table={table} dependency={pattern} "
                    f"cannot resolve path: {resolution_error}"
                )
            elif outside_path is not None:
                violations.append(
                    f"{manifest.relative_to(root)}: table={table} dependency={pattern} "
                    f"resolved={outside_path}"
                )

    def check_cargo(manifest: Path) -> None:
        if tomllib is None:
            try:
                references, workspace_patterns = fallback_cargo_data(manifest)
            except OSError as error:
                violations.append(f"{manifest.relative_to(root)}: cannot read Cargo.toml: {error}")
                return
            for table, dependency, path_value in references:
                check_reference(manifest, table, dependency, path_value)
            for table, pattern in workspace_patterns:
                check_workspace_patterns(manifest, table, [pattern])
            return
        try:
            document = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            violations.append(f"{manifest.relative_to(root)}: cannot read Cargo.toml: {error}")
            return

        def check_dependency_table(dependencies: Any, table: str) -> None:
            if not isinstance(dependencies, dict):
                return
            for dependency, specification in dependencies.items():
                if isinstance(specification, dict) and "path" in specification:
                    check_reference(manifest, table, str(dependency), specification["path"])

        for table_name in CARGO_DEPENDENCY_TABLES:
            check_dependency_table(document.get(table_name), f"[{table_name}]")

        workspace = document.get("workspace", {})
        if isinstance(workspace, dict):
            check_dependency_table(workspace.get("dependencies"), "[workspace.dependencies]")
            check_workspace_patterns(manifest, "[workspace.members]", workspace.get("members"))
            check_workspace_patterns(manifest, "[workspace.exclude]", workspace.get("exclude"))

        targets = document.get("target", {})
        if isinstance(targets, dict):
            for target_name, target_tables in targets.items():
                if isinstance(target_tables, dict):
                    for table_name in CARGO_DEPENDENCY_TABLES:
                        check_dependency_table(
                            target_tables.get(table_name),
                            f"[target.{target_name}.{table_name}]",
                        )

        # Cargo's [replace] table uses dependency specifications just like
        # dependency tables, but keys are package IDs rather than crate names.
        check_dependency_table(document.get("replace"), "[replace]")

        patch_tables = document.get("patch", {})
        if isinstance(patch_tables, dict):
            for registry, dependencies in patch_tables.items():
                check_dependency_table(dependencies, f"[patch.{registry}]")

    def check_package(manifest: Path) -> None:
        try:
            document = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            violations.append(f"{manifest.relative_to(root)}: cannot read package.json: {error}")
            return
        if not isinstance(document, dict):
            violations.append(f"{manifest.relative_to(root)}: package.json must contain an object")
            return

        for table_name in NPM_DEPENDENCY_TABLES:
            dependencies = document.get(table_name, {})
            if not isinstance(dependencies, dict):
                continue
            for dependency, specification in dependencies.items():
                if isinstance(specification, str):
                    for prefix in ("file:", "link:"):
                        if specification.startswith(prefix):
                            check_reference(
                                manifest,
                                f"[{table_name}]",
                                str(dependency),
                                specification[len(prefix) :],
                            )
                            break

        workspaces = document.get("workspaces", [])
        if isinstance(workspaces, dict):
            workspaces = workspaces.get("packages", [])
        check_workspace_patterns(manifest, "[workspaces]", workspaces)

    for manifest in manifests:
        if manifest.name == "Cargo.toml":
            check_cargo(manifest)
        else:
            check_package(manifest)

    return violations, len(manifests), checked_paths


def _write(root: Path, relative: str, content: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def self_test() -> int:
    failures = 0

    def verify(name: str, setup: Any, expected_count: int, expected_fragments: tuple[str, ...] = ()) -> None:
        nonlocal failures
        with tempfile.TemporaryDirectory(prefix="check-path-deps-") as temporary:
            root = Path(temporary) / "repo"
            root.mkdir()
            outside = Path(temporary) / "outside"
            outside.mkdir()
            setup(root, outside)
            violations, _, _ = check_root(root)
            passed = len(violations) == expected_count and all(
                any(fragment in violation for violation in violations)
                for fragment in expected_fragments
            )
            if passed:
                print(f"check-path-deps self-test: PASS — {name}")
            else:
                failures += 1
                print(
                    f"check-path-deps self-test: FAIL — {name}: expected {expected_count} "
                    f"violation(s), found {len(violations)}; {violations}",
                    file=sys.stderr,
                )

    verify(
        "[dependencies] path outside repository is refused",
        lambda root, outside: _write(
            root, "Cargo.toml", '[dependencies]\nexternal = { path = "../outside" }\n'
        ),
        1,
        ("table=[dependencies]", "dependency=external", "resolved="),
    )
    verify(
        "[patch.crates-io] path outside repository is refused",
        lambda root, outside: _write(
            root, "Cargo.toml", '[patch.crates-io]\nexternal = { path = "../outside" }\n'
        ),
        1,
        ("table=[patch.crates-io]", "dependency=external", "resolved="),
    )
    verify(
        "package.json file: path outside repository is refused",
        lambda root, outside: _write(
            root, "package.json", '{"dependencies":{"external":"file:../outside"}}\n'
        ),
        1,
        ("table=[dependencies]", "dependency=external", "resolved="),
    )
    verify(
        "package.json link: path outside repository is refused",
        lambda root, outside: _write(
            root, "package.json", '{"devDependencies":{"external":"link:../outside"}}\n'
        ),
        1,
        ("table=[devDependencies]", "dependency=external", "resolved="),
    )

    def symlink_fixture(root: Path, outside: Path) -> None:
        (root / "linked-outside").symlink_to(outside, target_is_directory=True)
        _write(root, "Cargo.toml", '[dependencies]\nexternal = { path = "linked-outside" }\n')

    verify(
        "symlinked path dependency escaping repository is refused",
        symlink_fixture,
        1,
        ("table=[dependencies]", "dependency=external", "resolved="),
    )
    verify(
        "Windows-style path separators cannot hide an escape",
        lambda root, outside: _write(
            root,
            "Cargo.toml",
            "[dependencies]\nwindows_escape = { path = '" + r"..\outside" + "' }\n",
        ),
        1,
        ("dependency=windows_escape", "resolved="),
    )

    def other_cargo_tables(root: Path, outside: Path) -> None:
        _write(
            root,
            "Cargo.toml",
            """[build-dependencies]
build_out = { path = "../outside" }

[target.'cfg(unix)'.dev-dependencies.target_out]
path = "../outside"

[dependencies]
dotted.path = "../outside"

[workspace.dependencies]
workspace_out = { path = "../outside" }

[replace]
"replaced:1.0" = { path = "../outside" }
""",
        )

    verify(
        "build, target, workspace, and replace dependency tables are scanned",
        other_cargo_tables,
        5,
        ("table=[build-dependencies]", "dependency=build_out", "table=[target.cfg(unix).dev-dependencies]", "dependency=target_out", "table=[dependencies]", "dependency=dotted", "table=[workspace.dependencies]", "dependency=workspace_out", "table=[replace]", "dependency=replaced:1.0", "resolved="),
    )

    def inside_only(root: Path, outside: Path) -> None:
        _write(
            root,
            "Cargo.toml",
            '[dependencies]\ninside = { path = "crates/inside" }\n'
            '[workspace]\nmembers = ["crates/*"]\n',
        )
        _write(root, "crates/inside/Cargo.toml", "[package]\nname='inside'\nversion='0.1.0'\n")
        _write(root, "package.json", '{"dependencies":{"file-dep":"file:./vendor/file-dep","link-dep":"link:./vendor/link-dep"},"workspaces":["packages/*"]}\n')
        _write(root, "vendor/file-dep/package.json", "{}\n")
        _write(root, "vendor/link-dep/package.json", "{}\n")
        _write(root, "packages/app/package.json", "{}\n")
        _write(root, "target/Cargo.toml", '[dependencies]\nignored = { path = "../../outside" }\n')
        _write(root, "node_modules/ignored/package.json", '{"dependencies":{"bad":"file:../../outside"}}\n')
        _write(root, ".git/ignored/Cargo.toml", '[dependencies]\nignored = { path = "../../outside" }\n')

    verify("inside-repository paths pass and generated directories are skipped", inside_only, 0)

    verify(
        "workspace glob outside repository is refused",
        lambda root, outside: _write(
            root, "package.json", '{"workspaces":["../../outside/*"]}\n'
        ),
        1,
        ("table=[workspaces]", "dependency=../../outside/*", "resolved="),
    )
    verify(
        "Cargo workspace member glob outside repository is refused",
        lambda root, outside: _write(
            root, "Cargo.toml", '[workspace]\nmembers = ["../../outside/*"]\n'
        ),
        1,
        ("table=[workspace.members]", "dependency=../../outside/*", "resolved="),
    )

    if failures:
        print(f"check-path-deps self-test: {failures} check(s) failed", file=sys.stderr)
        return 1
    print("check-path-deps self-test: all 10 checks passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="repository root to check (defaults to this script's repository)")
    parser.add_argument("--self-test", action="store_true", help="run fixture-based self-tests")
    args = parser.parse_args()
    if args.self_test:
        if args.root is not None:
            parser.error("--root cannot be combined with --self-test")
        return self_test()

    root = args.root if args.root is not None else Path(__file__).resolve().parent.parent
    try:
        violations, manifest_count, path_count = check_root(root)
    except (OSError, RuntimeError) as error:
        print(f"check-path-deps: cannot inspect repository {root}: {error}", file=sys.stderr)
        return 2
    if violations:
        for violation in violations:
            print(f"check-path-deps: OUTSIDE {violation}", file=sys.stderr)
        return 1
    print(
        f"checked {manifest_count} manifests, {path_count} path dependencies, "
        "all inside the repository"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
