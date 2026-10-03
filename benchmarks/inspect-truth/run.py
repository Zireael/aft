#!/usr/bin/env python3
"""Measure aft_inspect findings against each language's authoritative tool.

For every repository in corpus.json this script:

1. clones it (outside this checkout) and checks out the pinned commit;
2. runs a release `aft` binary over the NDJSON protocol and collects the full
   dead_code, unused_exports and todos lists (see `collect_category` for how
   the 100-row drill-down cap is worked around with scoped requests);
3. runs the oracle for each category (fallow/knip, rustc, staticcheck,
   vulture, plus tsc/cargo/go vet/pyright for diagnostics);
4. normalizes both sides to (category, path, symbol, line), matches them and
   writes per-category counts, precision/recall and sampled disagreements.

Phases can be run separately (`fetch`, `aft`, `oracle`, `score`) or together
with `all`. Raw tool output is kept under <corpus_root>/_results/<repo>/ so a
re-score never has to re-run the tools; the committed summary lives in
benchmarks/inspect-truth/results/.

Only the Python standard library is used. The NDJSON client and the Tier 2
readiness check are reused from benchmarks/inspect-field-audit/run_audit.py.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import random
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

HERE = Path(__file__).resolve().parent
CHECKOUT_ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE.parent / "inspect-field-audit"))
from run_audit import NdjsonClient, pending_tier2  # noqa: E402

RESULTS_DIR = HERE / "results"
DEFAULT_AFT_BIN = CHECKOUT_ROOT / "target" / "release" / "aft"
SAMPLE_SIZE = 10
# Inspect drill-down lists are capped at 100 rows server-side; topK cannot
# raise that, so larger result sets are collected by narrowing `scope`.
PAGE_CAP = 100

# Semantic and trigram indexes are not needed for inspect and only add load.
# The callgraph index stays on: the dead-code scanner reads it.
AFT_PROJECT_CONFIG = {
    "indexes": {"semantic": False, "trigram": False},
    "inspect": {"enabled": True},
}

# Inspect computes every category on each call, whatever `sections` asks for,
# and a scoped call first has the language servers analyze the scoped files.
# During list collection that sweep only costs time (typeorm: over 30 minutes),
# so language servers are switched off there and turned back on for the
# separate diagnostics pass. `lsp.disabled` is only honoured in the user tier.
COLLECTION_USER_CONFIG = {
    "lsp": {"disabled": ["typescript", "typescript-native", "python", "rust", "go", "bash", "yaml",
                        "ty", "oxlint", "biome", "vue", "svelte", "astro", "prisma", "dockerfile",
                        "terraform"]},
}

LANG_EXTS = {
    "typescript": (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"),
    "rust": (".rs",),
    "go": (".go",),
    "python": (".py",),
}
TS_SOURCE_EXTS = (".ts", ".tsx", ".mts", ".cts")

# Mirrors AFT's own test-tree rule (crates/aft/src/inspect/job.rs,
# `is_test_tree_file`). AFT withholds dead symbols in these files by design, so
# oracle findings in them are counted separately instead of as AFT misses.
TEST_SEGMENTS = {"tests", "test", "__tests__", "__mocks__", "testdata", "testutil", "testing"}
TEST_FILE_RE = re.compile(r"(\.test\.|\.spec\.|_test\.(rs|go|py)$|^test_.*\.py$|^conftest\.py$)")


def is_test_path(path: str) -> bool:
    parts = path.split("/")
    if any(part in TEST_SEGMENTS for part in parts[:-1]):
        return True
    return bool(TEST_FILE_RE.search(parts[-1]))


# --------------------------------------------------------------------------
# Corpus and process helpers
# --------------------------------------------------------------------------


def load_corpus() -> Tuple[Path, List[Dict[str, Any]]]:
    data = json.loads((HERE / "corpus.json").read_text())
    return Path(os.path.expanduser(data["corpus_root"])), data["repos"]


def run(argv: List[str], cwd: Path, env: Optional[Dict[str, str]] = None,
        timeout: int = 3600) -> Tuple[int, str, str, float]:
    started = time.monotonic()
    merged = os.environ.copy()
    if env:
        merged.update(env)
    proc = subprocess.run(argv, cwd=str(cwd), env=merged, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=timeout)
    return proc.returncode, proc.stdout, proc.stderr, time.monotonic() - started


def log(msg: str) -> None:
    print(f"[inspect-truth {time.strftime('%H:%M:%S')}] {msg}", file=sys.stderr, flush=True)


def raw_dir(corpus_root: Path, spec: Dict[str, Any]) -> Path:
    path = corpus_root / "_results" / spec["name"]
    path.mkdir(parents=True, exist_ok=True)
    return path


def tools_dir(corpus_root: Path) -> Path:
    return corpus_root / ".tools"


def tracked_files(repo: Path) -> List[str]:
    code, out, _err, _ = run(["git", "ls-files"], cwd=repo)
    if code != 0:
        raise RuntimeError(f"git ls-files failed in {repo}")
    return [line for line in out.splitlines() if line]


def in_domain(path: str, spec: Dict[str, Any]) -> bool:
    """True when `path` is a source file of the repo's language under include_paths."""
    if not path.endswith(LANG_EXTS[spec["language"]]):
        return False
    return any(path.startswith(prefix) for prefix in spec.get("include_paths", [""]))


COMMENT_OR_ATTR_RE = re.compile(r"^\s*(//|/\*|\*|#\[|#!\[|@|$)")


def source_line(repo: Path, path: str, line: int, skip_preamble: bool = False) -> str:
    """Return the source text at `line`.

    With `skip_preamble`, doc comments, attributes and decorators starting at
    `line` are skipped so a reader sees the declaration itself: AFT reports
    some items at the first line of their doc comment.
    """
    try:
        lines = (repo / path).read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return ""
    index = line - 1
    if skip_preamble:
        limit = min(len(lines), index + 40)
        while index < limit - 1 and COMMENT_OR_ATTR_RE.match(lines[index]):
            index += 1
    return lines[index].strip()[:200] if 0 <= index < len(lines) else ""


# --------------------------------------------------------------------------
# `fetch` phase: clone and pin the corpus
# --------------------------------------------------------------------------


def fetch(corpus_root: Path, spec: Dict[str, Any]) -> None:
    repo = corpus_root / spec["name"]
    if not repo.exists():
        log(f"{spec['name']}: cloning {spec['url']}")
        code, _o, err, _ = run(["git", "clone", "--depth", "1", spec["url"], str(repo)], cwd=corpus_root)
        if code != 0:
            raise RuntimeError(err)
    code, head, _e, _ = run(["git", "rev-parse", "HEAD"], cwd=repo)
    if head.strip() != spec["commit"]:
        log(f"{spec['name']}: checking out pinned {spec['commit']}")
        run(["git", "fetch", "--depth", "1", "origin", spec["commit"]], cwd=repo)
        code, _o, err, _ = run(["git", "checkout", "--detach", spec["commit"]], cwd=repo)
        if code != 0:
            raise RuntimeError(err)
    # Keep the AFT project config and tool caches out of `git status` in the clone.
    exclude = repo / ".git" / "info" / "exclude"
    exclude.parent.mkdir(parents=True, exist_ok=True)
    existing = exclude.read_text() if exclude.exists() else ""
    for pattern in (".cortexkit/", ".fallow/", "target/"):
        if pattern not in existing.split("\n"):
            existing += ("" if existing.endswith("\n") or not existing else "\n") + pattern + "\n"
    exclude.write_text(existing)
    if spec.get("install") and not (repo / "node_modules").exists():
        log(f"{spec['name']}: installing dependencies")
        code, _o, err, _ = run(spec["install"], cwd=repo, env=spec.get("install_env"), timeout=3600)
        if code != 0:
            raise RuntimeError(f"install failed: {err[-2000:]}")


# --------------------------------------------------------------------------
# `aft` phase: collect AFT findings
# --------------------------------------------------------------------------


AFT_CATEGORIES = ["dead_code", "unused_exports", "todos", "duplicates"]
LISTED_CATEGORIES = ["dead_code", "unused_exports", "todos"]


def summary_count(response: Dict[str, Any], category: str) -> Optional[int]:
    entry = (response.get("summary") or {}).get(category)
    if not isinstance(entry, dict):
        return None
    for key in ("count", "total_groups"):
        if isinstance(entry.get(key), int):
            return entry[key]
    return None


def detail_items(response: Dict[str, Any], category: str) -> List[Dict[str, Any]]:
    items = (response.get("details") or {}).get(category)
    return items if isinstance(items, list) else []


class AftSession:
    def __init__(self, aft_bin: Path, repo: Path, storage: Path):
        storage.mkdir(parents=True, exist_ok=True)
        self.client = NdjsonClient(aft_bin, repo, storage)
        self.repo = repo
        self.storage = storage
        self.calls = 0

    def configure(self, user_config: Optional[Path] = None) -> Dict[str, Any]:
        extra: Dict[str, Any] = {}
        if user_config is not None:
            extra["cortexkit_user_config_path"] = str(user_config.resolve())
        return self.client.request(
            "configure", timeout_s=600, project_root=str(self.repo.resolve()),
            harness="runner", storage_dir=str(self.storage.resolve()), **extra)

    def inspect(self, sections: List[str], scope: Optional[List[str]] = None,
                timeout_s: float = 1800) -> Dict[str, Any]:
        params: Dict[str, Any] = {"sections": sections, "topK": PAGE_CAP}
        if scope:
            params["scope"] = scope
        self.calls += 1
        return self.client.request("inspect", timeout_s=timeout_s, **params)

    def close(self) -> None:
        self.client.close()


def children_index(files: Iterable[str]) -> Dict[str, List[str]]:
    """Map each directory ("" for the root) to its immediate child paths."""
    index: Dict[str, set] = collections.defaultdict(set)
    for path in files:
        parts = path.split("/")
        for depth in range(len(parts)):
            parent = "/".join(parts[:depth])
            child = "/".join(parts[: depth + 1])
            index[parent].add(child)
    return {key: sorted(value) for key, value in index.items()}


def collect_category(session: AftSession, category: str, scope: List[str],
                     tree: Dict[str, List[str]], truncated: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Return every finding of `category` under `scope`.

    A scoped inspect rolls up the whole project, filters to the scope and only
    then applies the 100-row cap, while `summary.<category>.count` keeps the
    full in-scope count. So when count exceeds the rows returned, the scope is
    split (a list of paths in halves, a directory into its children) until
    every piece fits under the cap.
    """
    response = session.inspect([category], scope=scope)
    for _attempt in range(20):
        if not unavailable_categories(response, [category]):
            break
        time.sleep(15)
        response = session.inspect([category], scope=scope)
    if not response.get("success", False):
        raise RuntimeError(f"inspect {category} scope={scope[:3]} failed: {response}")
    count = summary_count(response, category)
    items = detail_items(response, category)
    if count is None:
        raise RuntimeError(f"inspect {category} returned no count for scope={scope[:3]}: "
                           f"{json.dumps(response.get('summary'))[:500]}")
    if count <= len(items):
        return items
    if len(scope) > 1:
        middle = len(scope) // 2
        return (collect_category(session, category, scope[:middle], tree, truncated)
                + collect_category(session, category, scope[middle:], tree, truncated))
    children = tree.get(scope[0], [])
    if children:
        return collect_category(session, category, children, tree, truncated)
    truncated.append({"category": category, "path": scope[0], "count": count, "returned": len(items)})
    return items


def unavailable_categories(response: Dict[str, Any], categories: Iterable[str]) -> set:
    """Categories whose summary says the Tier 2 result is not ready yet.

    On a large repo the dead-code aggregate can outlive inspect's phase wait
    budget; the summary then carries `unavailable: true` and no count.
    """
    summary = response.get("summary") or {}
    return {c for c in categories
            if isinstance(summary.get(c), dict) and summary[c].get("unavailable")}


def wait_until_ready(session: AftSession, deadline_s: float) -> Dict[str, Any]:
    started = time.monotonic()
    while True:
        response = session.inspect(AFT_CATEGORIES)
        pending = (pending_tier2(response) | unavailable_categories(response, AFT_CATEGORIES)) & set(AFT_CATEGORIES)
        if not pending:
            return response
        if time.monotonic() - started > deadline_s:
            log(f"Tier 2 still pending after {deadline_s}s: {sorted(pending)}")
            return response
        time.sleep(5)


def collect_aft(corpus_root: Path, spec: Dict[str, Any], aft_bin: Path,
                diagnostics_sample: int) -> None:
    repo = corpus_root / spec["name"]
    out = raw_dir(corpus_root, spec)
    config_dir = repo / ".cortexkit"
    config_dir.mkdir(exist_ok=True)
    (config_dir / "aft.jsonc").write_text(json.dumps(AFT_PROJECT_CONFIG, indent=2) + "\n")

    files = tracked_files(repo)
    tree = children_index(files)
    user_config = out / "collection-user-config.jsonc"
    user_config.write_text(json.dumps(COLLECTION_USER_CONFIG, indent=2) + "\n")
    session = AftSession(aft_bin, repo, out / "aft-storage")
    record: Dict[str, Any] = {"aft_bin": str(aft_bin), "commit": spec["commit"]}
    try:
        started = time.monotonic()
        configured = session.configure(user_config)
        if not configured.get("success", False) or configured.get("config_dropped_keys"):
            raise RuntimeError(f"configure failed or dropped keys: {json.dumps(configured)[:1000]}")
        project = wait_until_ready(session, deadline_s=3600)
        record["ready_s"] = round(time.monotonic() - started, 1)
        record["project_summary"] = project.get("summary")
        record["scanner_state"] = project.get("scanner_state")
        # test-only and generated rows are reported beside the headline list and
        # are not narrowed by scope, so only the capped project-wide copy exists.
        record["side_lists"] = {
            key: value for key, value in (project.get("details") or {}).items()
            if key not in LISTED_CATEGORIES
        }
        record["duplicates"] = detail_items(project, "duplicates")
        truncated: List[Dict[str, Any]] = []
        record["items"] = {}
        for category in LISTED_CATEGORIES:
            total = summary_count(project, category)
            first = detail_items(project, category)
            if total is not None and total <= len(first):
                items = first
            else:
                items = collect_category(session, category, tree.get("", []), tree, truncated)
            unique = {(i.get("file"), i.get("symbol") or i.get("text"), i.get("line")): i for i in items}
            record["items"][category] = list(unique.values())
            record.setdefault("project_counts", {})[category] = total
            log(f"{spec['name']}: {category} project count={total} collected={len(unique)}")
        record["truncated_scopes"] = truncated
        record["inspect_calls"] = session.calls
        record["elapsed_s"] = round(time.monotonic() - started, 1)
    finally:
        session.close()
    if diagnostics_sample > 0:
        session = AftSession(aft_bin, repo, out / "aft-storage")
        try:
            configured = session.configure()
            if not configured.get("success", False):
                raise RuntimeError(f"configure failed: {configured}")
            record["diagnostics"] = collect_aft_diagnostics(session, spec, files, diagnostics_sample)
        finally:
            session.close()
    (out / "aft.json").write_text(json.dumps(record, indent=1))


def diagnostics_sample_files(spec: Dict[str, Any], files: List[str], size: int) -> List[str]:
    candidates = [f for f in files if in_domain(f, spec) and not is_test_path(f)
                  and not f.endswith(".d.ts")]
    if spec["language"] == "typescript":
        candidates = [f for f in candidates if f.endswith(TS_SOURCE_EXTS)]
    rng = random.Random(f"{spec['name']}:diagnostics")
    return sorted(rng.sample(candidates, min(size, len(candidates))))


def collect_aft_diagnostics(session: AftSession, spec: Dict[str, Any], files: List[str],
                            size: int) -> Dict[str, Any]:
    """Ask AFT for diagnostics on a seeded sample of files.

    A project-wide inspect only reports files the language servers already
    have open; a scoped request opens the scoped files first. The same sample
    is compared against the compiler's output for those files only.
    """
    sample = diagnostics_sample_files(spec, files, size)
    started = time.monotonic()
    items: List[Dict[str, Any]] = []
    responses = []
    # Diagnostics pages are capped too; keep each request small enough that a
    # page rarely overflows, and record the count so overflow is visible.
    for offset in range(0, len(sample), 20):
        chunk = sample[offset: offset + 20]
        response = session.inspect(["diagnostics"], scope=chunk, timeout_s=900)
        summary = (response.get("summary") or {}).get("diagnostics")
        got = detail_items(response, "diagnostics")
        items.extend(got)
        responses.append({"files": chunk, "summary": summary, "returned": len(got),
                          "success": response.get("success")})
    return {"sample": sample, "items": items, "pages": responses,
            "elapsed_s": round(time.monotonic() - started, 1)}


# --------------------------------------------------------------------------
# `oracle` phase: run the reference tools
# --------------------------------------------------------------------------


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=1))


def run_oracles(corpus_root: Path, spec: Dict[str, Any], diagnostics: bool) -> None:
    repo = corpus_root / spec["name"]
    out = raw_dir(corpus_root, spec)
    tools = tools_dir(corpus_root)
    meta: Dict[str, Any] = {}
    lang = spec["language"]
    if lang == "typescript":
        fallow = "/opt/homebrew/bin/fallow"
        for label, extra in (("fallow-default", []), ("fallow-entry-exports", ["--include-entry-exports"])):
            code, stdout, stderr, elapsed = run([fallow, "dead-code", "--format", "json", *extra], cwd=repo)
            (out / f"{label}.json").write_text(stdout)
            meta[label] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-500:]}
        code, stdout, stderr, _ = run([fallow, "list", "--entry-points", "--format", "json"], cwd=repo)
        (out / "fallow-entry-points.json").write_text(stdout)
        knip = tools / "node" / "node_modules" / ".bin" / "knip"
        code, stdout, stderr, elapsed = run([str(knip), "--reporter", "json", "--no-exit-code",
                                             "--no-progress"], cwd=repo, timeout=3600)
        (out / "knip.json").write_text(stdout)
        meta["knip"] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-1500:]}
        if diagnostics:
            texts = []
            for project in spec.get("tsc_projects", ["tsconfig.json"]):
                # Use the TypeScript the project itself pins: in a pnpm
                # workspace it lives in the package's node_modules, and a newer
                # tsc rejects options the project still uses.
                tsc = tools / "node" / "node_modules" / ".bin" / "tsc"
                directory = (repo / project).parent
                while True:
                    if (directory / "node_modules" / ".bin" / "tsc").exists():
                        tsc = directory / "node_modules" / ".bin" / "tsc"
                        break
                    if directory == repo:
                        break
                    directory = directory.parent
                code, stdout, stderr, elapsed = run([str(tsc), "--noEmit", "--pretty", "false", "-p", project],
                                                    cwd=repo, timeout=3600)
                texts.append(stdout + stderr)
                meta.setdefault("tsc", []).append({"project": project, "tsc": str(tsc), "exit": code,
                                                   "elapsed_s": round(elapsed, 1)})
            (out / "tsc.txt").write_text("\n".join(texts))
    elif lang == "rust":
        env = {"RUSTC_WRAPPER": "", "CARGO_BUILD_RUSTC_WRAPPER": ""}
        code, stdout, stderr, elapsed = run(spec["cargo_check"], cwd=repo, env=env, timeout=7200)
        (out / "cargo-check.jsonl").write_text(stdout)
        meta["cargo_check"] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-1500:]}
        code, stdout, stderr, _ = run(["cargo", "+nightly", "udeps", "--version"], cwd=repo)
        meta["cargo_udeps"] = "installed" if code == 0 else "not installed (skipped)"
    elif lang == "go":
        staticcheck = tools / "go-bin" / "staticcheck"
        code, stdout, stderr, elapsed = run([str(staticcheck), "-checks", "U1000", "-f", "json", "./..."],
                                            cwd=repo, timeout=3600)
        (out / "staticcheck-u1000.jsonl").write_text(stdout)
        meta["staticcheck"] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-1500:]}
        # U1000 treats every exported identifier as used, while AFT only ever
        # reports exported symbols. golang.org/x/tools/cmd/deadcode does a
        # whole-program reachability walk from main packages and does report
        # exported functions, so it is the oracle for AFT's half of the space.
        deadcode = tools / "go-bin" / "deadcode"
        for label, extra in (("deadcode", []), ("deadcode-test", ["-test"])):
            code, stdout, stderr, elapsed = run([str(deadcode), "-json", *extra, "./..."], cwd=repo, timeout=3600)
            (out / f"{label}.json").write_text(stdout)
            meta[label] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-1500:]}
        code, stdout, stderr, elapsed = run(["go", "vet", "./..."], cwd=repo, timeout=3600)
        (out / "go-vet.txt").write_text(stdout + stderr)
        meta["go_vet"] = {"exit": code, "elapsed_s": round(elapsed, 1)}
    elif lang == "python":
        vulture = tools / "py" / "bin" / "vulture"
        code, stdout, stderr, elapsed = run([str(vulture), spec["python_package"], "--min-confidence", "60"],
                                            cwd=repo)
        (out / "vulture.txt").write_text(stdout)
        meta["vulture"] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-500:]}
        if diagnostics:
            pyright_args = ["pyright", "--outputjson"]
            venv = repo / ".venv"
            if venv.exists():
                # Without --pythonpath pointing at the venv's interpreter, pyright
                # falls back to the system interpreter and reports version and
                # missing-import errors that are not real.
                pyright_args += ["--pythonpath", str(venv / "bin" / "python")]
            code, stdout, stderr, elapsed = run([*pyright_args, spec["python_package"]],
                                                cwd=repo, timeout=3600)
            (out / "pyright.json").write_text(stdout)
            meta["pyright"] = {"exit": code, "elapsed_s": round(elapsed, 1), "stderr_tail": stderr[-500:]}
    write_json(out / "oracle-meta.json", meta)


# --------------------------------------------------------------------------
# `score` phase: normalize, match and report
# --------------------------------------------------------------------------


def finding(category: str, path: str, symbol: str, line: int, **extra: Any) -> Dict[str, Any]:
    item = {"category": category, "path": path, "symbol": symbol, "line": int(line or 0)}
    item.update(extra)
    return item


def aft_findings(corpus_root: Path, spec: Dict[str, Any]) -> Dict[str, List[Dict[str, Any]]]:
    record = json.loads((raw_dir(corpus_root, spec) / "aft.json").read_text())
    result: Dict[str, List[Dict[str, Any]]] = {}
    for category, items in record["items"].items():
        rows = []
        for item in items:
            path = item.get("file") or ""
            symbol = item.get("symbol") or item.get("marker") or ""
            rows.append(finding(category, path, symbol, item.get("line") or 0, kind=item.get("kind")))
        result[category] = rows
    return result


def fallow_oracle(out: Path) -> Tuple[List[Dict[str, Any]], set, set]:
    """Unused exports/types per fallow, excluding fallow's own entry files.

    fallow's default run auto-marks every export of an entry file as used, and
    its plugins can turn most of an app's files into entries (Outline: 2,228
    of 2,231). `--include-entry-exports` reports those exports too; the files
    `fallow list --entry-points` names as real entries (configs, tests,
    scripts, package.json targets) are then removed so their exports stay used.
    """
    report = json.loads((out / "fallow-entry-exports.json").read_text())
    entries_doc = json.loads((out / "fallow-entry-points.json").read_text() or "{}")
    entry_files = {e.get("path") for e in entries_doc.get("entry_points", []) if isinstance(e, dict)}
    rows = []
    for key in ("unused_exports", "unused_types"):
        for item in report.get(key, []):
            if item.get("path") in entry_files:
                continue
            rows.append(finding("unused_exports", item["path"], item["export_name"], item.get("line", 0),
                                type_only=bool(item.get("is_type_only")),
                                re_export=bool(item.get("is_re_export"))))
    unused_files = {item["path"] for item in report.get("unused_files", [])}
    return rows, unused_files, entry_files


def knip_verdicts(out: Path) -> Optional[Dict[str, Any]]:
    path = out / "knip.json"
    if not path.exists() or not path.read_text().strip():
        return None
    try:
        doc = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None
    exports = set()
    files = set(doc.get("files") or [])
    for issue in doc.get("issues", []):
        file = issue.get("file")
        if issue.get("files"):
            files.add(file)
        for key in ("exports", "types", "nsExports", "nsTypes", "enumMembers"):
            for entry in issue.get(key) or []:
                name = entry.get("name") if isinstance(entry, dict) else None
                if name:
                    exports.add((file, name))
    return {"exports": exports, "files": files}


RUSTC_ITEM_KINDS = [
    ("associated function", "associated_function"), ("associated items", "associated_item"),
    ("associated constant", "associated_constant"), ("associated type", "associated_type"),
    ("type alias", "type_alias"), ("function", "function"), ("method", "method"),
    ("struct", "struct"), ("enum", "enum"), ("constant", "constant"), ("static", "static"),
    ("trait", "trait"), ("union", "union"), ("macro", "macro"), ("field", "field"),
    ("variant", "variant"), ("module", "module"), ("constructor", "constructor"),
]
# Kinds AFT's dead-code scanner can report (it tracks items, not fields or variants).
RUST_ITEM_DOMAIN = {"function", "method", "associated_function", "associated_item", "struct", "enum",
                    "constant", "static", "trait", "type_alias", "union", "macro", "associated_constant"}


def rustc_kind(message: str) -> str:
    lowered = message.lower()
    for needle, kind in RUSTC_ITEM_KINDS:
        if needle in lowered:
            return kind
    return "other"


def rust_oracle(repo: Path, out: Path) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    """dead_code findings and every warning/error from `cargo check` JSON."""
    dead: Dict[Tuple[str, int, str], Dict[str, Any]] = {}
    diags: Dict[Tuple[str, int, str, str], Dict[str, Any]] = {}
    for raw in (out / "cargo-check.jsonl").read_text().splitlines():
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if frame.get("reason") != "compiler-message":
            continue
        message = frame.get("message") or {}
        level = message.get("level")
        code = (message.get("code") or {}).get("code") or ""
        is_test_build = bool((frame.get("target") or {}).get("test"))
        primaries = [s for s in message.get("spans") or [] if s.get("is_primary")]
        for span in primaries:
            path = span.get("file_name", "")
            if path.startswith("/") or path.startswith(".."):
                continue  # dependency or registry source
            if level in ("warning", "error"):
                key = (path, span["line_start"], level, message.get("message", ""))
                diags[key] = finding("diagnostics", path, code or level, span["line_start"],
                                     severity=level, message=message.get("message", ""))
            if code != "dead_code":
                continue
            text = (span.get("text") or [{}])[0]
            symbol = text.get("text", "")[text.get("highlight_start", 1) - 1: text.get("highlight_end", 1) - 1]
            key = (path, span["line_start"], symbol)
            entry = dead.get(key)
            if entry is None:
                entry = finding("dead_code", path, symbol, span["line_start"],
                                kind=rustc_kind(message.get("message", "")), builds=[])
                dead[key] = entry
            entry["builds"].append("test" if is_test_build else "normal")
    rows = list(dead.values())
    for row in rows:
        source = source_line(repo, row["path"], row["line"])
        row["pub"] = bool(re.match(r"^(#\[.*\]\s*)?pub\b", source))
        builds = set(row.pop("builds"))
        # Only flagged when tests are not compiled: the item is used from tests.
        row["test_only_use"] = builds == {"normal"}
    return rows, list(diags.values())


STATICCHECK_RE = re.compile(r"^(func|field|type|const|var|method)\s+(\S+)\s+is unused")


def deadcode_oracle(out: Path) -> List[Dict[str, Any]]:
    """Functions unreachable from any main package, per x/tools deadcode."""
    def load(label: str) -> List[Dict[str, Any]]:
        path = out / f"{label}.json"
        text = path.read_text().strip() if path.exists() else ""
        rows = []
        for package in (json.loads(text) if text else None) or []:
            for func in package.get("Funcs") or []:
                if func.get("Generated"):
                    continue
                pos = func.get("Position") or {}
                symbol = func["Name"].split(".")[-1]
                rows.append(finding("dead_code", pos.get("File", ""), symbol, pos.get("Line", 0),
                                    kind="method" if "." in func["Name"] else "func",
                                    exported=symbol[:1].isupper()))
        return rows

    production = load("deadcode")
    with_tests = {(r["path"], r["line"]) for r in load("deadcode-test")}
    for row in production:
        # Unreachable from main but reachable once tests are roots.
        row["test_only_use"] = (row["path"], row["line"]) not in with_tests
    return production


def go_oracle(repo: Path, out: Path) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    rows = []
    for raw in (out / "staticcheck-u1000.jsonl").read_text().splitlines():
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            continue
        loc = frame.get("location") or {}
        path = os.path.relpath(loc.get("file", ""), repo)
        match = STATICCHECK_RE.match(frame.get("message", ""))
        if not match:
            continue
        kind, name = match.groups()
        symbol = re.split(r"[.)]", name)[-1].lstrip("(*")
        rows.append(finding("dead_code", path, symbol, loc.get("line", 0), kind=kind,
                            exported=symbol[:1].isupper()))
    diags = []
    for raw in (out / "go-vet.txt").read_text().splitlines():
        match = re.match(r"^(?:vet: )?(\.?/?[^:\s]+\.go):(\d+):(?:\d+:)?\s*(.*)$", raw)
        if match:
            diags.append(finding("diagnostics", match.group(1).lstrip("./"), "vet", int(match.group(2)),
                                 severity="warning", message=match.group(3)))
    return rows, diags


VULTURE_RE = re.compile(r"^(.+?\.py):(\d+): unused (\w+(?: \w+)?) '([^']+)' \((\d+)% confidence")


def python_oracle(repo: Path, out: Path) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    rows = []
    for raw in (out / "vulture.txt").read_text().splitlines():
        match = VULTURE_RE.match(raw)
        if match:
            path, line, kind, name, confidence = match.groups()
            rows.append(finding("dead_code", path, name, int(line), kind=kind, confidence=int(confidence)))
    diags = []
    pyright_path = out / "pyright.json"
    if pyright_path.exists() and pyright_path.read_text().strip():
        doc = json.loads(pyright_path.read_text())
        for diag in doc.get("generalDiagnostics", []):
            if diag.get("severity") not in ("error", "warning"):
                continue
            diags.append(finding("diagnostics", os.path.relpath(diag["file"], repo), diag.get("rule") or "",
                                 diag["range"]["start"]["line"] + 1, severity=diag["severity"],
                                 message=diag.get("message", "")))
    return rows, diags


TSC_RE = re.compile(r"^(.+?)\((\d+),(\d+)\): (error|warning) (TS\d+): (.*)$")


def tsc_diagnostics(spec: Dict[str, Any], out: Path) -> List[Dict[str, Any]]:
    path = out / "tsc.txt"
    if not path.exists():
        return []
    rows = []
    base = ""
    for raw in path.read_text().splitlines():
        match = TSC_RE.match(raw)
        if match:
            file, line, _col, severity, code, message = match.groups()
            rows.append(finding("diagnostics", os.path.normpath(os.path.join(base, file)), code, int(line),
                                severity=severity, message=message))
    return rows


TODO_RE = re.compile(r"(?://|/\*|^\s*\*|#|<!--|--)\s*(TODO|FIXME|HACK|XXX|BUG)\b")


def todo_oracle(repo: Path, spec: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Regex cross-check for TODO markers. Not authoritative: it has no parser,
    so it also matches markers inside string literals and URLs."""
    rows = []
    for path in tracked_files(repo):
        if not in_domain(path, spec):
            continue
        try:
            text = (repo / path).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            match = TODO_RE.search(line)
            if match:
                rows.append(finding("todos", path, match.group(1), number))
    return rows


def match_findings(aft: List[Dict[str, Any]], oracle: List[Dict[str, Any]],
                   by_line: bool = False) -> Tuple[List[Tuple[Dict, Dict]], List[Dict], List[Dict]]:
    """Pair findings that name the same symbol in the same file.

    When a file declares the same name more than once (several `new` methods,
    overloads) the pair with the nearest line wins. `by_line` matches on
    (path, line) instead, for categories without a stable symbol name.
    """
    buckets: Dict[Tuple[str, Any], List[Dict]] = collections.defaultdict(list)
    for item in oracle:
        buckets[(item["path"], item["line"] if by_line else item["symbol"])].append(item)
    agreed, aft_only = [], []
    for item in aft:
        candidates = buckets.get((item["path"], item["line"] if by_line else item["symbol"]))
        if not candidates:
            aft_only.append(item)
            continue
        best = min(candidates, key=lambda c: abs(c["line"] - item["line"]))
        candidates.remove(best)
        agreed.append((item, best))
    oracle_only = [item for bucket in buckets.values() for item in bucket]
    return agreed, aft_only, oracle_only


def used_in_own_file(repo: Path, item: Dict[str, Any], cache: Dict[str, str]) -> bool:
    """True when the exported name also appears on another line of its file.

    A textual check: it can be fooled by a comment or a string that repeats the
    name, which errs toward treating the export as used.
    """
    if item["symbol"] == "default":
        return False
    if item["path"] not in cache:
        try:
            cache[item["path"]] = (repo / item["path"]).read_text(encoding="utf-8", errors="replace")
        except OSError:
            cache[item["path"]] = ""
    pattern = re.compile(r"\b" + re.escape(item["symbol"]) + r"\b")
    for number, line in enumerate(cache[item["path"]].splitlines(), start=1):
        if number != item["line"] and pattern.search(line):
            return True
    return False


# --------------------------------------------------------------------------
# Cause hints
#
# Every disagreement gets a textual hint so causes can be counted over the
# whole bucket, not only the ten sampled rows. Hints are grep heuristics, not
# verdicts; REPORT.md records the hand-checked verdict for sampled rows.
# --------------------------------------------------------------------------


class Grepper:
    def __init__(self, repo: Path, spec: Dict[str, Any]):
        self.repo = repo
        self.globs = ["*" + ext for ext in LANG_EXTS[spec["language"]]]
        self.cache: Dict[str, List[Tuple[str, int, str]]] = {}
        self.lines: Dict[str, List[str]] = {}

    def refs(self, symbol: str) -> List[Tuple[str, int, str]]:
        if symbol not in self.cache:
            code, out, _e, _ = run(["git", "grep", "-n", "-w", "-I", "-e", symbol, "--", *self.globs],
                                   cwd=self.repo)
            rows = []
            for raw in out.splitlines():
                path, line, text = raw.split(":", 2)
                rows.append((path, int(line), text))
            self.cache[symbol] = rows
        return self.cache[symbol]

    def file_lines(self, path: str) -> List[str]:
        if path not in self.lines:
            try:
                self.lines[path] = (self.repo / path).read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                self.lines[path] = []
        return self.lines[path]


def enclosing_rust_impl(lines: List[str], line: int) -> str:
    """The `impl` header around a 1-based line, or "" at module level."""
    for index in range(min(line, len(lines)) - 1, -1, -1):
        text = lines[index]
        if re.match(r"^\s*(unsafe\s+)?impl\b", text):
            return text.strip()
        if re.match(r"^(macro_rules!|\S.*macro_rules!)", text):
            return "macro_rules!"
        if index < line - 1 and re.match(r"^}", text):
            return ""
    return ""


def inside_macro_invocation(lines: List[str], line: int) -> bool:
    """True when a 1-based line sits inside a `name! { ... }` block."""
    for index in range(min(line, len(lines)) - 2, -1, -1):
        text = lines[index]
        if re.match(r"^\s*[\w:]+!\s*[{(\[]\s*$", text) and not text.lstrip().startswith("macro_rules"):
            return True
        if re.match(r"^(}|\S.*\{\s*$)", text):
            return False
    return False


def in_library_crate(repo: Path, path: str) -> bool:
    """True when the file belongs to a Cargo package with a library target."""
    directory = (repo / path).parent
    while True:
        manifest = directory / "Cargo.toml"
        if manifest.exists():
            text = manifest.read_text(errors="replace")
            return (directory / "src" / "lib.rs").exists() or "[lib]" in text
        if directory == repo or directory == directory.parent:
            return False
        directory = directory.parent


def cause_hint(item: Dict[str, Any], side: str, lang: str, grep: Grepper) -> str:
    symbol, path = item["symbol"], item["path"]
    if item["category"] in ("todos", "diagnostics"):
        return ""
    if lang == "python":
        return "aft_has_no_python_dead_code" if side == "oracle_only" else ""
    refs = [] if symbol == "default" else grep.refs(symbol)
    elsewhere = [r for r in refs if r[0] != path]
    own = [r for r in refs if r[0] == path and abs(r[1] - item["line"]) > 3]
    if lang == "typescript":
        if side == "aft_only":
            stem = re.sub(r"\.(tsx?|jsx?|mts|cts)$", "", path.split("/")[-1])
            if stem == "index":
                stem = path.split("/")[-2] if "/" in path else stem
            if symbol == "default":
                _c, out, _e, _ = run(["git", "grep", "-l", "-E", r"import\([^)]*" + re.escape(stem) + r"['\"]"],
                                     cwd=grep.repo)
                return "dynamic_import_default" if out.strip() else "default_export_no_dynamic_import"
            if any(re.search(r"\b\w+\." + re.escape(symbol) + r"\b", text) for _p, _l, text in elsewhere):
                return "namespace_member_reference"
            if any(re.search(r"\bimport\b|\bfrom\b|^\s*" + re.escape(symbol) + r",?\s*$", text)
                   for _p, _l, text in elsewhere):
                return "imported_by_name_elsewhere"
            if elsewhere:
                return "referenced_elsewhere"
            return "used_in_own_file_only" if own else "no_textual_reference"
        if item.get("type_only"):
            return "type_only_export"
        if item.get("re_export"):
            return "re_export"
        if any(re.search(r"\bimport\s+" + re.escape(symbol) + r"\b", text) for _p, _l, text in elsewhere):
            return "default_import_with_same_name"
        if symbol == "default":
            return "default_export"
        if own:
            return "used_in_own_file_only"
        return "other"
    if lang == "go":
        if side == "aft_only":
            if item.get("kind") == "method" and any(re.match(r"^\s+" + re.escape(symbol) + r"\(", text)
                                                       for _p, _l, text in refs):
                return "interface_method"
            if any(re.search(r"\b" + re.escape(symbol) + r"\s*\(", text) for _p, _l, text in elsewhere + own
                   if not text.lstrip().startswith(("//", "func "))):
                return "called_elsewhere"
            if any(not text.lstrip().startswith("//") for _p, _l, text in elsewhere + own):
                return "function_value_reference"
            return "no_textual_reference"
        return "exported" if item.get("exported") else "unexported_out_of_aft_scope"
    if lang == "rust":
        impl = enclosing_rust_impl(grep.file_lines(path), item["line"])
        if side == "aft_only":
            if impl == "macro_rules!":
                return "inside_macro_rules"
            if impl and " for " in impl:
                return "trait_impl_member"
            lines = grep.file_lines(path)
            if inside_macro_invocation(lines, item["line"]):
                return "defined_inside_macro_invocation"
            decl = source_line(grep.repo, path, item["line"], skip_preamble=True)
            if re.match(r"^pub\s", decl) and in_library_crate(grep.repo, path):
                return "public_api_of_library_crate"
            if any(re.search(r"\bpub(\([^)]*\))?\s+use\b", text) for _p, _l, text in refs):
                return "reexported_with_pub_use"
            if any(re.search(r"(::|\.)" + re.escape(symbol) + r"\b|\b" + re.escape(symbol) + r"\s*[({!<]", text)
                   for _p, _l, text in elsewhere + own if not text.lstrip().startswith("//")):
                return "referenced_in_code"
            return "no_textual_reference"
        if item.get("kind") in ("field", "variant"):
            return item["kind"] + "_out_of_aft_scope"
        if not item.get("pub"):
            return "private_item_out_of_aft_scope"
        if item.get("test_only_use"):
            return "pub_item_used_only_by_tests"
        return "pub_item_missed"
    return ""


def ratio(numerator: int, denominator: int) -> Optional[float]:
    return round(numerator / denominator, 3) if denominator else None


def sample(items: List[Dict[str, Any]], seed: str, repo: Path) -> List[Dict[str, Any]]:
    rng = random.Random(seed)
    chosen = rng.sample(items, min(SAMPLE_SIZE, len(items)))
    rows = []
    for item in sorted(chosen, key=lambda i: (i["path"], i["line"])):
        row = dict(item)
        row["source"] = source_line(repo, item["path"], item["line"],
                                    skip_preamble=item["category"] in ("dead_code", "unused_exports"))
        rows.append(row)
    return rows


def score_bucket(name: str, category: str, language: str, aft: List[Dict], oracle: List[Dict],
                 repo: Path, oracle_files: Optional[set] = None, by_line: bool = False,
                 knip: Optional[Dict[str, Any]] = None, note: str = "",
                 grep: Optional[Grepper] = None) -> Dict[str, Any]:
    agreed, aft_only, oracle_only = match_findings(aft, oracle, by_line=by_line)
    if grep is not None:
        for side, rows in (("aft_only", aft_only), ("oracle_only", oracle_only)):
            for row in rows:
                row["hint"] = cause_hint(row, side, language, grep)
    file_level = []
    if oracle_files:
        # The oracle reports the whole file unused instead of each export in it.
        file_level = [item for item in aft_only if item["path"] in oracle_files]
        aft_only = [item for item in aft_only if item["path"] not in oracle_files]
    entry = {
        "repo": name, "category": category, "language": language, "note": note,
        "aft_count": len(aft), "oracle_count": len(oracle), "agreed": len(agreed),
        "agreed_file_level": len(file_level), "aft_only": len(aft_only), "oracle_only": len(oracle_only),
        "precision": ratio(len(agreed) + len(file_level), len(aft)),
        "recall": ratio(len(agreed), len(oracle)),
        "cause_hints": {
            "aft_only": dict(collections.Counter(i.get("hint") or "" for i in aft_only).most_common()),
            "oracle_only": dict(collections.Counter(i.get("hint") or "" for i in oracle_only).most_common()),
        } if grep is not None else None,
        "samples": {
            "aft_only": sample(aft_only, f"{name}:{category}:aft_only", repo),
            "oracle_only": sample(oracle_only, f"{name}:{category}:oracle_only", repo),
        },
    }
    if knip is not None:
        for bucket in entry["samples"].values():
            for row in bucket:
                row["knip_unused"] = ((row["path"], row["symbol"]) in knip["exports"]
                                      or row["path"] in knip["files"])
        entry["knip_agrees_aft_only"] = sum(
            1 for item in aft_only
            if (item["path"], item["symbol"]) in knip["exports"] or item["path"] in knip["files"])
        entry["knip_agrees_oracle_only"] = sum(
            1 for item in oracle_only
            if (item["path"], item["symbol"]) in knip["exports"] or item["path"] in knip["files"])
    # Full lists go to the raw results directory (see score_repo), not the
    # committed summary, which keeps only the sampled rows.
    entry["_lists"] = {"aft_only": aft_only, "oracle_only": oracle_only}
    return entry


def score_repo(corpus_root: Path, spec: Dict[str, Any]) -> Dict[str, Any]:
    repo = corpus_root / spec["name"]
    out = raw_dir(corpus_root, spec)
    name, lang = spec["name"], spec["language"]
    aft_record = json.loads((out / "aft.json").read_text())
    aft = aft_findings(corpus_root, spec)
    excluded: Dict[str, Dict[str, int]] = {}

    def keep(items: List[Dict[str, Any]], label: str = "") -> List[Dict[str, Any]]:
        # Test files are dropped from both sides: AFT withholds dead symbols in
        # them by design, so neither side's test-file findings are comparable.
        domain = [i for i in items if in_domain(i["path"], spec)]
        kept = [i for i in domain if not is_test_path(i["path"])]
        if label:
            excluded[label] = len(domain) - len(kept)
        return kept

    result: Dict[str, Any] = {"repo": name, "language": lang, "commit": spec["commit"], "buckets": [],
                              "aft_project_counts": aft_record.get("project_counts"),
                              "aft_truncated_scopes": aft_record.get("truncated_scopes"),
                              "aft_inspect_calls": aft_record.get("inspect_calls"),
                              "aft_elapsed_s": aft_record.get("elapsed_s"),
                              "aft_ready_s": aft_record.get("ready_s"),
                              "aft_duplicate_groups": summary_count({"summary": aft_record.get("project_summary")},
                                                                    "duplicates"),
                              "test_file_findings_excluded": excluded}
    aft_dead = keep(aft["dead_code"], "aft_dead_code")
    grep = Grepper(repo, spec)
    oracle_diags: List[Dict[str, Any]] = []
    if lang == "typescript":
        oracle, unused_files, entry_files = fallow_oracle(out)
        knip = knip_verdicts(out)
        oracle_in = keep(oracle, "oracle_unused_exports")
        files_in = {f for f in unused_files if in_domain(f, spec)}
        result["oracle_dead_files"] = len(files_in)
        result["fallow_entry_files"] = len(entry_files)
        result["buckets"].append(score_bucket(
            name, "unused_exports", lang, keep(aft["unused_exports"], "aft_unused_exports"), oracle_in, repo,
            oracle_files=files_in, knip=knip, grep=grep,
            note="oracle: fallow --include-entry-exports minus fallow entry files"))
        # An export used only inside its own file is an unneeded `export`, not
        # dead code, so the dead-code comparison drops those oracle rows.
        texts: Dict[str, str] = {}
        for item in oracle_in:
            item["used_in_own_file"] = used_in_own_file(repo, item, texts)
        dead_oracle = [o for o in oracle_in if not o["used_in_own_file"]]
        bucket = score_bucket(
            name, "dead_code", lang, aft_dead, dead_oracle, repo, oracle_files=files_in, knip=knip, grep=grep,
            note="oracle: fallow unused exports not referenced in their own file, plus fallow unused files")
        bucket["oracle_used_in_own_file_dropped"] = len(oracle_in) - len(dead_oracle)
        result["buckets"].append(bucket)
        result["buckets"].append({
            "repo": name, "category": "dead_files", "language": lang, "aft_count": None,
            "oracle_count": len(files_in), "note": "AFT has no dead-file category"})
        oracle_diags = tsc_diagnostics(spec, out)
    elif lang == "rust":
        oracle, oracle_diags = rust_oracle(repo, out)
        oracle_in = keep(oracle, "oracle_dead_code")
        bucket = score_bucket(name, "dead_code", lang, aft_dead, oracle_in, repo, grep=grep,
                              note="oracle: rustc dead_code from cargo check --all-targets")
        # AFT only considers items with a visibility modifier and never fields or
        # variants; recall against that slice separates design scope from misses.
        domain = [o for o in oracle_in if o["pub"] and o["kind"] in RUST_ITEM_DOMAIN]
        agreed_domain, _a, missed_domain = match_findings(aft_dead, domain)
        bucket["oracle_in_aft_domain"] = len(domain)
        bucket["recall_in_aft_domain"] = ratio(len(agreed_domain), len(domain))
        bucket["oracle_only_by_kind"] = dict(collections.Counter(
            ("pub " if o["pub"] else "private ") + o["kind"] for o in match_findings(
                aft_dead, oracle_in)[2]))
        bucket["oracle_test_only_use"] = sum(1 for o in oracle_in if o["test_only_use"])
        bucket["samples"]["oracle_only_in_aft_domain"] = sample(missed_domain, f"{name}:domain", repo)
        result["buckets"].append(bucket)
    elif lang == "go":
        oracle, oracle_diags = go_oracle(repo, out)
        oracle_in = keep(oracle, "oracle_dead_code")
        bucket = score_bucket(name, "dead_code", lang, aft_dead, oracle_in, repo, grep=grep,
                              note="oracle: staticcheck -checks U1000")
        exported = [o for o in oracle_in if o["exported"]]
        agreed_exp, _a, _m = match_findings(aft_dead, exported)
        bucket["oracle_exported"] = len(exported)
        bucket["recall_exported"] = ratio(len(agreed_exp), len(exported))
        result["buckets"].append(bucket)
        reach = keep(deadcode_oracle(out), "oracle_deadcode")
        # deadcode reports only functions and methods, so AFT's type, const and
        # var findings are compared against it only as functions and methods.
        aft_funcs = [a for a in aft_dead if a.get("kind") in ("function", "method")]
        reach_bucket = score_bucket(
            name, "dead_code (functions)", lang, aft_funcs, reach, repo, grep=grep,
            note="oracle: x/tools deadcode (unreachable from main); AFT restricted to functions/methods")
        reach_bucket["oracle_exported"] = sum(1 for o in reach if o["exported"])
        reach_bucket["oracle_test_only_use"] = sum(1 for o in reach if o["test_only_use"])
        result["buckets"].append(reach_bucket)
    elif lang == "python":
        oracle, oracle_diags = python_oracle(repo, out)
        oracle_in = keep(oracle, "oracle_dead_code")
        result["buckets"].append(score_bucket(
            name, "dead_code", lang, aft_dead, oracle_in, repo, grep=grep,
            note="oracle: vulture --min-confidence 60; AFT skips Python dead code"))
    todo_bucket = score_bucket(name, "todos", lang, keep(aft["todos"]), keep(todo_oracle(repo, spec)), repo,
                               by_line=True, note="regex cross-check, not authoritative")
    result["buckets"].append(todo_bucket)
    diag = aft_record.get("diagnostics")
    if diag:
        result["buckets"].append(score_diagnostics(name, lang, repo, diag, oracle_diags))
    lists = {}
    for bucket in result["buckets"]:
        if "_lists" in bucket:
            lists[bucket["category"]] = bucket.pop("_lists")
    write_json(out / "disagreements.json", lists)
    return result


def score_diagnostics(name: str, lang: str, repo: Path, diag: Dict[str, Any],
                      oracle_diags: List[Dict[str, Any]]) -> Dict[str, Any]:
    sample_files = set(diag["sample"])
    aft = []
    for item in diag["items"]:
        severity = item.get("severity")
        if severity not in ("error", "warning"):
            continue
        aft.append(finding("diagnostics", item.get("file", ""), severity, item.get("line") or 0,
                           severity=severity, message=item.get("message", ""), source=item.get("source")))
    oracle = [finding("diagnostics", o["path"], o["severity"], o["line"], severity=o["severity"],
                      message=o.get("message", ""), code=o["symbol"])
              for o in oracle_diags if o["path"] in sample_files]
    bucket = score_bucket(name, "diagnostics", lang, aft, oracle, repo, by_line=True,
                          note=f"{len(sample_files)} sampled files; matched on (file, line)")
    bucket["sample_files"] = len(sample_files)
    bucket["aft_pages_with_gaps"] = sum(
        1 for page in diag["pages"] if not ((page.get("summary") or {}).get("complete", True)))
    bucket["errors_aft"] = sum(1 for a in aft if a["severity"] == "error")
    bucket["errors_oracle"] = sum(1 for o in oracle if o["severity"] == "error")
    return bucket


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def fmt(value: Any) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, float):
        return f"{value:.2f}"
    return str(value)


def render_markdown(results: List[Dict[str, Any]], corpus: List[Dict[str, Any]]) -> str:
    lines = ["# inspect-truth results (generated)", "",
             "Generated by `benchmarks/inspect-truth/run.py score`. Do not edit by hand; the",
             "hand-written analysis lives in `benchmarks/inspect-truth/REPORT.md`.", "",
             "| repo | lang | category | AFT | oracle | agreed | agreed (file-level) | AFT-only | oracle-only | precision | recall |",
             "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for result in results:
        for b in result["buckets"]:
            lines.append("| {repo} | {lang} | {cat} | {a} | {o} | {ag} | {fl} | {ao} | {oo} | {p} | {r} |".format(
                repo=b["repo"], lang=b["language"], cat=b["category"], a=fmt(b.get("aft_count")),
                o=fmt(b.get("oracle_count")), ag=fmt(b.get("agreed")), fl=fmt(b.get("agreed_file_level")),
                ao=fmt(b.get("aft_only")), oo=fmt(b.get("oracle_only")), p=fmt(b.get("precision")),
                r=fmt(b.get("recall"))))
    lines.append("")
    for result in results:
        lines.append(f"## {result['repo']} ({result['language']}, {result['commit'][:12]})")
        lines.append("")
        extras = {k: v for k, v in result.items() if k not in ("buckets", "repo", "language", "commit")}
        lines.append("```json")
        lines.append(json.dumps(extras, indent=1))
        lines.append("```")
        for b in result["buckets"]:
            if "samples" not in b:
                continue
            lines.append(f"### {b['category']}: {b.get('note', '')}")
            lines.append("")
            for key in ("aft_only", "agreed", "oracle_in_aft_domain", "recall_in_aft_domain",
                        "oracle_only_by_kind", "oracle_test_only_use", "oracle_in_test_files_excluded",
                        "oracle_exported", "recall_exported", "knip_agrees_aft_only",
                        "knip_agrees_oracle_only", "sample_files", "aft_pages_with_gaps",
                        "errors_aft", "errors_oracle", "oracle_used_in_own_file_dropped", "cause_hints"):
                if key in b and b[key] is not None:
                    lines.append(f"- {key}: {fmt(b[key]) if not isinstance(b[key], dict) else json.dumps(b[key])}")
            lines.append("")
            for bucket_name, rows in b["samples"].items():
                if not rows:
                    continue
                lines.append(f"#### {bucket_name} sample")
                lines.append("")
                for row in rows:
                    tags = []
                    for key in ("hint", "kind", "pub", "test_only_use", "type_only", "re_export", "used_in_own_file",
                                "knip_unused", "severity", "confidence", "exported"):
                        if key in row and row[key] not in (None, ""):
                            tags.append(f"{key}={row[key]}")
                    message = f" — {row['message'][:120]}" if row.get("message") else ""
                    source = row.get("source", "").replace("`", "'")
                    lines.append(f"- `{row['path']}:{row['line']}` `{row['symbol']}` "
                                 f"[{', '.join(tags)}]{message}  \n  `{source}`")
                lines.append("")
    return "\n".join(lines) + "\n"


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("phase", choices=["fetch", "aft", "oracle", "score", "all"])
    parser.add_argument("--repo", action="append", help="limit to these corpus names")
    parser.add_argument("--aft-bin", type=Path, default=DEFAULT_AFT_BIN)
    parser.add_argument("--diagnostics-sample", type=int, default=60,
                        help="files per repo for the diagnostics comparison (0 disables)")
    args = parser.parse_args()

    corpus_root, corpus = load_corpus()
    selected = [spec for spec in corpus if not args.repo or spec["name"] in args.repo]
    if args.phase in ("aft", "all") and not args.aft_bin.exists():
        parser.error(f"{args.aft_bin} not found; build it with "
                     "`CARGO_BUILD_RUSTC_WRAPPER= RUSTC_WRAPPER= cargo build --release -p agent-file-tools`")
    # One repository at a time, AFT and oracles never concurrently, to keep
    # machine load predictable.
    for spec in selected:
        if args.phase in ("fetch", "all"):
            fetch(corpus_root, spec)
        if args.phase in ("aft", "all"):
            log(f"{spec['name']}: running AFT")
            collect_aft(corpus_root, spec, args.aft_bin, args.diagnostics_sample)
        if args.phase in ("oracle", "all"):
            log(f"{spec['name']}: running oracles")
            run_oracles(corpus_root, spec, diagnostics=args.diagnostics_sample > 0)
    if args.phase in ("score", "all"):
        RESULTS_DIR.mkdir(exist_ok=True)
        results = []
        for spec in corpus:
            if not (raw_dir(corpus_root, spec) / "aft.json").exists() or not (
                    raw_dir(corpus_root, spec) / "oracle-meta.json").exists():
                log(f"{spec['name']}: AFT or oracle results missing, skipping score")
                continue
            results.append(score_repo(corpus_root, spec))
        write_json(RESULTS_DIR / "results.json", {"corpus": corpus, "results": results})
        (RESULTS_DIR / "results.md").write_text(render_markdown(results, corpus))
        log(f"wrote {RESULTS_DIR / 'results.json'} and results.md")
    return 0


if __name__ == "__main__":
    sys.exit(main())
