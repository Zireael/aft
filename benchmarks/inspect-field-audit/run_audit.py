#!/usr/bin/env python3
"""Run a sequential real-repo aft_inspect field audit.

The runner intentionally uses only Python's standard library. It clones each repo
once, writes a project-level AFT config with semantic indexing disabled, warms
AFT, calls the standalone NDJSON protocol through `tool_call`, and stores raw
responses under ~/Work/OSS/AFT_TESTS/_results.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

TIER2_CATEGORIES = {"dead_code", "unused_exports", "duplicates", "cycles"}
DEFAULT_WORKDIR = Path.home() / "Work" / "OSS" / "AFT_TESTS"
PROJECT_CONFIG = {
    "indexes": {"semantic": False, "trigram": False, "callgraph": True},
    "inspect": {"enabled": True},
}


@dataclass(frozen=True)
class RepoSpec:
    slug: str
    category: str
    url: str
    notes: str = ""

    @staticmethod
    def from_json(value: dict[str, Any]) -> "RepoSpec":
        return RepoSpec(
            slug=str(value["slug"]),
            category=str(value["category"]),
            url=str(value["url"]),
            notes=str(value.get("notes", "")),
        )


class NdjsonClient:
    """Small request/response client for the standalone AFT stdin/stdout API."""

    def __init__(self, aft_bin: Path, repo_root: Path, storage_dir: Path):
        env = os.environ.copy()
        env.setdefault("RUST_LOG", "warn")
        env["AFT_STORAGE_DIR"] = str(storage_dir)
        # Semantic indexing is disabled, but keep any accidental model downloads
        # inside the audit storage tree rather than the user's global cache.
        env.setdefault("FASTEMBED_CACHE_DIR", str(storage_dir / "semantic" / "models"))
        self.proc = subprocess.Popen(
            [str(aft_bin)],
            cwd=str(repo_root),
            env=env,
            text=True,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=1,
        )
        self._next_id = 1

    def close(self) -> None:
        if self.proc.stdin and not self.proc.stdin.closed:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)

    def request(self, command: str, timeout_s: float = 120.0, **params: Any) -> dict[str, Any]:
        if not self.proc.stdin or not self.proc.stdout:
            raise RuntimeError("AFT process pipes are not available")
        request_id = str(self._next_id)
        self._next_id += 1
        payload = {"id": request_id, "command": command, **params}
        self.proc.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self.proc.stdin.flush()

        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if line == "":
                stderr = self.proc.stderr.read() if self.proc.stderr else ""
                raise RuntimeError(f"AFT exited before response {request_id}; stderr:\n{stderr}")
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            # Ignore push frames such as configure_warnings/status_changed.
            if str(frame.get("id")) == request_id:
                return frame
        raise TimeoutError(f"Timed out waiting for AFT response {request_id}: {payload}")


def load_matrix(path: Path) -> list[RepoSpec]:
    with path.open("r", encoding="utf-8") as handle:
        return [RepoSpec.from_json(item) for item in json.load(handle)]


def run_command(
    argv: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: int | None = None,
) -> tuple[int, float, str, str]:
    started = time.monotonic()
    completed = subprocess.run(
        argv,
        cwd=str(cwd) if cwd else None,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    return completed.returncode, time.monotonic() - started, completed.stdout, completed.stderr


def clone_repo(spec: RepoSpec, repo_dir: Path) -> dict[str, Any]:
    if repo_dir.exists():
        return {"skipped": True, "reason": "already_exists"}
    repo_dir.parent.mkdir(parents=True, exist_ok=True)
    code, elapsed, stdout, stderr = run_command(
        ["git", "clone", "--depth", "1", spec.url, str(repo_dir)], timeout=1800
    )
    return {
        "skipped": False,
        "returncode": code,
        "elapsed_s": elapsed,
        "stdout_tail": tail(stdout),
        "stderr_tail": tail(stderr),
    }


def current_head(repo_dir: Path) -> str | None:
    code, _elapsed, stdout, _stderr = run_command(["git", "rev-parse", "HEAD"], cwd=repo_dir)
    return stdout.strip() if code == 0 else None


def file_count(repo_dir: Path) -> int | None:
    code, _elapsed, stdout, _stderr = run_command(["git", "ls-files"], cwd=repo_dir)
    if code != 0:
        return None
    return len([line for line in stdout.splitlines() if line.strip()])


def write_project_config(repo_dir: Path) -> Path:
    config_dir = repo_dir / ".cortexkit"
    config_dir.mkdir(exist_ok=True)
    config_path = config_dir / "aft.jsonc"
    config_path.write_text(json.dumps(PROJECT_CONFIG, indent=2) + "\n", encoding="utf-8")
    return config_path


def warmup(aft_bin: Path, repo_dir: Path, storage_dir: Path, timeout_ms: int) -> dict[str, Any]:
    env = os.environ.copy()
    env.setdefault("RUST_LOG", "warn")
    env["AFT_STORAGE_DIR"] = str(storage_dir)
    env.setdefault("FASTEMBED_CACHE_DIR", str(storage_dir / "semantic" / "models"))
    code, elapsed, stdout, stderr = run_command(
        [str(aft_bin), "warmup", "--root", str(repo_dir.resolve()), "--timeout", str(timeout_ms)],
        cwd=repo_dir,
        env=env,
        timeout=max(60, int(timeout_ms / 1000) + 60),
    )
    return {
        "returncode": code,
        "elapsed_s": elapsed,
        "stdout_tail": tail(stdout),
        "stderr_tail": tail(stderr),
    }


def configure(client: NdjsonClient, repo_dir: Path, storage_dir: Path) -> dict[str, Any]:
    return client.request(
        "configure",
        timeout_s=120,
        project_root=str(repo_dir.resolve()),
        harness="runner",
        storage_dir=str(storage_dir.resolve()),
    )


def inspect(client: NdjsonClient, timeout_s: float = 240.0) -> dict[str, Any]:
    # Plugin-facing tool name is aft_inspect; the standalone core tool_call name
    # is the bare subc name "inspect".
    return client.request(
        "tool_call",
        timeout_s=timeout_s,
        name="inspect",
        arguments={"sections": "all", "topK": 100},
    )


def pending_tier2(response: dict[str, Any]) -> set[str]:
    state = response.get("scanner_state") or {}
    pending = state.get("pending_categories") or []
    result = {str(item) for item in pending if str(item) in TIER2_CATEGORIES}
    summary = response.get("summary") or {}
    for category in TIER2_CATEGORIES:
        category_summary = summary.get(category)
        if not isinstance(category_summary, dict):
            continue
        status = str(category_summary.get("status") or "")
        reason = str(category_summary.get("reason") or "")
        if status in {"pending", "stale"}:
            result.add(category)
        # The dead-code scanner reports a warming callgraph as an unavailable
        # status rather than a pending category. Treat that state as not ready;
        # otherwise the audit would preserve a startup transient as a finding.
        if status == "unavailable" and "building" in reason:
            result.add(category)
    return result


def poll_inspect_ready(
    client: NdjsonClient,
    *,
    attempts: int,
    sleep_s: float,
    result_dir: Path,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    polls: list[dict[str, Any]] = []
    last: dict[str, Any] | None = None
    for attempt in range(1, attempts + 1):
        started = time.monotonic()
        last = inspect(client)
        elapsed = time.monotonic() - started
        pending = sorted(pending_tier2(last))
        polls.append({"attempt": attempt, "elapsed_s": elapsed, "pending_tier2": pending})
        (result_dir / f"inspect_poll_{attempt:02d}.json").write_text(
            json.dumps(last, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        if not pending:
            return last, polls
        time.sleep(sleep_s)
    assert last is not None
    return last, polls


def plant_cycle_probe(client: NdjsonClient, repo_dir: Path, result_dir: Path) -> dict[str, Any]:
    # Use a normal source-looking directory rather than a dot directory; AFT's
    # project walker intentionally ignores hidden paths, and the probe is meant
    # to test cycle detection rather than ignore-file behavior.
    scratch = repo_dir / "aft_audit_cycle_probe"
    scratch.mkdir(exist_ok=True)
    a = scratch / "a.ts"
    b = scratch / "b.ts"
    a.write_text('import { b } from "./b";\nexport const a = () => b();\n', encoding="utf-8")
    b.write_text('import { a } from "./a";\nexport const b = () => a();\n', encoding="utf-8")
    try:
        response = client.request(
            "tool_call",
            timeout_s=180,
            name="inspect",
            arguments={"sections": "cycles", "scope": str(scratch.resolve()), "topK": 10},
        )
        result = {
            "scratch_dir": str(scratch.relative_to(repo_dir)),
            "response": response,
            "detected": bool((response.get("details") or {}).get("cycles")),
        }
        (result_dir / "cycle_probe.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        return result
    finally:
        for path in (a, b):
            try:
                path.unlink()
            except FileNotFoundError:
                pass
        try:
            scratch.rmdir()
        except OSError:
            pass


def summarize_counts(response: dict[str, Any]) -> dict[str, Any]:
    summary = response.get("summary") or {}
    details = response.get("details") or {}
    counts: dict[str, Any] = {}
    for category, value in summary.items():
        if isinstance(value, dict):
            counts[category] = {
                "count": value.get("count"),
                "status": value.get("status"),
                "total_groups": value.get("total_groups"),
                "by_language": value.get("by_language"),
            }
    detail_counts = {}
    for category, value in details.items():
        if isinstance(value, list):
            detail_counts[category] = len(value)
        elif isinstance(value, dict):
            detail_counts[category] = value
    return {"summary": counts, "detail_item_counts": detail_counts}


def tool_availability() -> dict[str, bool]:
    tools = ["node", "npx", "knip", "fallow", "cargo", "go", "staticcheck", "python3", "java"]
    return {tool: shutil.which(tool) is not None for tool in tools}


def tail(text: str, limit: int = 4000) -> str:
    if len(text) <= limit:
        return text
    return text[-limit:]


def run_repo(spec: RepoSpec, args: argparse.Namespace) -> dict[str, Any]:
    workdir = Path(args.workdir).expanduser().resolve()
    repo_dir = workdir / "repos" / spec.slug
    result_dir = workdir / "_results" / spec.slug
    storage_dir = workdir / "storage" / spec.slug
    result_dir.mkdir(parents=True, exist_ok=True)
    storage_dir.mkdir(parents=True, exist_ok=True)

    metadata: dict[str, Any] = {
        "slug": spec.slug,
        "category": spec.category,
        "url": spec.url,
        "notes": spec.notes,
        "started_at_unix": time.time(),
        "tool_availability": tool_availability(),
    }

    clone = clone_repo(spec, repo_dir)
    metadata["clone"] = clone
    if clone.get("returncode") not in (None, 0):
        metadata["status"] = "clone_failed"
        write_json(result_dir / "summary.json", metadata)
        return metadata

    metadata["head"] = current_head(repo_dir)
    metadata["tracked_file_count"] = file_count(repo_dir)
    metadata["project_config"] = str(write_project_config(repo_dir).relative_to(repo_dir))

    metadata["warmup"] = warmup(Path(args.aft_bin).resolve(), repo_dir, storage_dir, args.warmup_timeout_ms)

    client = NdjsonClient(Path(args.aft_bin).resolve(), repo_dir, storage_dir)
    try:
        config_response = configure(client, repo_dir, storage_dir)
        metadata["configure"] = config_response
        if not config_response.get("success"):
            metadata["status"] = "configure_failed"
            write_json(result_dir / "summary.json", metadata)
            return metadata

        inspect_response, polls = poll_inspect_ready(
            client,
            attempts=args.inspect_poll_attempts,
            sleep_s=args.inspect_poll_sleep_s,
            result_dir=result_dir,
        )
        metadata["inspect_polls"] = polls
        metadata["inspect_counts"] = summarize_counts(inspect_response)
        metadata["inspect_complete"] = bool(inspect_response.get("complete"))
        metadata["scanner_state"] = inspect_response.get("scanner_state")
        write_json(result_dir / "inspect.json", inspect_response)

        if args.plant_cycle_probe and spec.slug == args.plant_cycle_probe:
            metadata["cycle_probe"] = plant_cycle_probe(client, repo_dir, result_dir)

        metadata["status"] = "completed"
        return metadata
    except Exception as exc:  # noqa: BLE001 - field-audit runs must capture degraded states.
        metadata["status"] = "failed"
        metadata["error"] = f"{type(exc).__name__}: {exc}"
        return metadata
    finally:
        client.close()
        metadata["finished_at_unix"] = time.time()
        write_json(result_dir / "summary.json", metadata)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def select_repos(matrix: Iterable[RepoSpec], selected: list[str] | None) -> list[RepoSpec]:
    repos = list(matrix)
    if not selected:
        return repos
    wanted = set(selected)
    return [repo for repo in repos if repo.slug in wanted or repo.category in wanted]


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aft-bin", default="./target/release/aft", help="Path to release AFT binary")
    parser.add_argument("--matrix", default=str(Path(__file__).with_name("repos.json")))
    parser.add_argument("--workdir", default=os.environ.get("AFT_AUDIT_ROOT", str(DEFAULT_WORKDIR)))
    parser.add_argument("--repo", action="append", help="Slug or category to run; may be repeated")
    parser.add_argument("--warmup-timeout-ms", type=int, default=600_000)
    parser.add_argument("--inspect-poll-attempts", type=int, default=4)
    parser.add_argument("--inspect-poll-sleep-s", type=float, default=5.0)
    parser.add_argument(
        "--plant-cycle-probe",
        default="dub",
        help="Repo slug where a temporary two-file TS cycle is planted and removed; empty disables.",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    aft_bin = Path(args.aft_bin).expanduser()
    if not aft_bin.exists():
        print(f"AFT binary not found: {aft_bin}", file=sys.stderr)
        return 2
    matrix = select_repos(load_matrix(Path(args.matrix)), args.repo)
    random.seed(0)
    for index, spec in enumerate(matrix, start=1):
        print(f"[{index}/{len(matrix)}] {spec.slug} ({spec.category})", flush=True)
        metadata = run_repo(spec, args)
        print(f"  status={metadata.get('status')} warmup={metadata.get('warmup', {}).get('elapsed_s')}s", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
