#!/usr/bin/env python3
"""Plan committed Rust slices and transport them to the single Windows builder."""
import argparse
import base64
import json
import os
from pathlib import Path
import queue
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = "C:/build/aft"
READY = "AFT_WINDOWS_GATE_READY "
DEV_SHELL = "$ProgressPreference='SilentlyContinue'; . C:\\build\\provision\\dev-shell.ps1; "


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def select_tests(paths, full=False, test_filter=None):
    """Use broad source-module slices, retaining independently touched test targets."""
    jobs = set()
    shared = full
    for path in paths:
        if (path in ("Cargo.toml", "Cargo.lock", "build.rs", "crates/aft/Cargo.toml",
                     "crates/aft/build.rs", "crates/aft/src/lib.rs", "crates/aft/src/context.rs",
                     "crates/aft/src/db.rs", "crates/aft/src/executor.rs")
                or re.match(r"crates/aft/src/(config[^/]*\.rs|subc_config\.rs|executor/|db/)", path)
                or path.startswith("crates/aft-tokenizer/")):
            shared = True
        elif path == "crates/aft/src/main.rs":
            jobs.add(("bin", "aft", ""))
        elif path.startswith("crates/aft/src/") and path.endswith(".rs"):
            module = path[len("crates/aft/src/"):].split("/")[0].removesuffix(".rs")
            jobs.add(("lib", "", module + "::"))
        elif path.startswith("crates/aft/tests/") and path.endswith(".rs"):
            tail = path[len("crates/aft/tests/"):].split("/")
            if tail[0] == "helpers" or tail[0] == "fixtures":
                jobs.add(("test", "integration", ""))
            elif len(tail) == 1:
                jobs.add(("test", tail[0].removesuffix(".rs"), ""))
            elif tail[-1] == "main.rs" or tail[1] == "helpers" or len(tail) > 2:
                jobs.add(("test", tail[0], ""))
            else:
                jobs.add(("test", tail[0], tail[-1].removesuffix(".rs") + "::"))
    if shared:
        jobs = {job for job in jobs if job[0] != "lib"}
        jobs.add(("lib", "", ""))
    if full:
        jobs.add(("test", "integration", ""))
    # An explicit ad-hoc filter overrides the diff, unless --full requests both harnesses.
    if test_filter is not None:
        jobs = {("lib", "", test_filter)}
        if full:
            jobs.add(("test", "integration", test_filter))
    # A whole harness subsumes its filtered slices.
    jobs = {job for job in jobs if not job[2] or (job[0], job[1], "") not in jobs}
    return [{"kind": kind, "target": target, "filter": filt}
            for kind, target, filt in sorted(jobs)]


def cargo_args(job):
    args = ["test", "--locked", "-j", "4", "-p", "agent-file-tools", "--" + job["kind"]]
    if job["target"]:
        args.append(job["target"])
    if job["filter"]:
        args.append(job["filter"])
    return args + ["--", "--test-threads", "4"]


def failure_summary(lines):
    """Repeat libtest's named failure blocks and panics, not just the exit status."""
    selected = []
    in_block = False
    for line in lines:
        if line.startswith("failures:"):
            in_block = True
        if in_block or re.search(r"^test .* \.\.\. FAILED$|panicked at|^error:|GATE ERROR|TIMEOUT|Zero tests", line):
            selected.append(line)
        if in_block and line.startswith("test result:"):
            in_block = False
    return "\n".join(selected) or "No libtest failure block (transport, setup, compile or timeout failure); see output above."


def gate_exit_code(returncode, lines):
    # A lost PowerShell exit code must never turn a failed worker into a green gate.
    if returncode == 0 and ("GATE PASSED" not in lines or "GATE FAILED" in lines):
        return 1
    return returncode


class Guest:
    def __init__(self, config, deadline):
        self.options = ["-F", str(config), "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                        "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15",
                        "-o", "ServerAliveCountMax=3"]
        self.deadline = deadline

    def remaining(self):
        return max(1, self.deadline - time.monotonic())

    def command(self, code):
        encoded = base64.b64encode((DEV_SHELL + code).encode("utf-16le")).decode("ascii")
        return ["ssh", *self.options, "windows-build-vm",
                "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand " + encoded]

    def run(self, code):
        subprocess.run(self.command(code), check=True, timeout=self.remaining())

    def copy(self, path, destination):
        subprocess.run(["scp", *self.options, str(path), "windows-build-vm:" + destination],
                       check=True, timeout=self.remaining())


def stream_output(process, lines, events):
    for line in process.stdout:
        line = line.rstrip("\r\n")
        lines.append(line)
        print(line, flush=True)
        if line.startswith(READY):
            events.put(line[len(READY):].strip())
    events.put(None)


def run_gate(guest, sha, base, jobs, cap):
    run_id = uuid.uuid4().hex
    remote = ROOT + "/runs/" + run_id
    holder = socket.gethostname() + ":" + str(os.getpid()) + ":" + sha[:12]
    helper = Path(__file__).with_name("windows-gate.ps1")
    process = None
    lines = []
    start = time.monotonic()
    try:
        guest.run("$ErrorActionPreference='Stop'; New-Item -ItemType Directory -Force -Path '" + remote + "' | Out-Null")
        guest.copy(helper, remote + "/gate.ps1")
        process = subprocess.Popen(guest.command("& '" + remote + "/gate.ps1' -Mode Supervisor -RunId '" + run_id
                                  + "' -CapSeconds " + str(cap) + " -Holder '" + holder.replace("'", "''") + "'; exit $LASTEXITCODE"),
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        events = queue.Queue()
        reader = threading.Thread(target=stream_output, args=(process, lines, events), daemon=True)
        reader.start()
        prerequisite = events.get(timeout=guest.remaining())
        if prerequisite is None:
            return process.wait(timeout=guest.remaining()) or 1
        with tempfile.TemporaryDirectory(prefix="aft-windows-gate-") as local:
            bundle = Path(local) / "commit.bundle"
            # Exclude only an ancestor that the LOCKED guest checkout actually has.
            incremental = (re.fullmatch(r"[0-9a-f]{40}", prerequisite or "") and prerequisite != sha
                           and subprocess.run(["git", "merge-base", "--is-ancestor", prerequisite, sha],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0)
            revisions = [sha, "^" + prerequisite] if incremental else [sha]
            print("Bundle: " + ("incremental from " + prerequisite if incremental else "full history") , flush=True)
            # HEAD is a ref, not a raw SHA: Git bundles need a named tip to advertise.
            # Confirm it has not moved since the plan was made before transporting it.
            if git("rev-parse", "HEAD") != sha:
                raise RuntimeError("HEAD moved while planning; rerun for the new commit")
            subprocess.run(["git", "bundle", "create", str(bundle), "HEAD", *revisions[1:]],
                           check=True, timeout=guest.remaining())
            advertised = subprocess.check_output(["git", "bundle", "list-heads", str(bundle)], text=True)
            if advertised.strip() != sha + " HEAD":
                raise RuntimeError("HEAD moved during bundle creation; refusing to ship a different commit")
            guest.copy(bundle, remote + "/commit.bundle")
            plan = Path(local) / "plan.json"
            plan.write_text(json.dumps({"sha": sha, "base": base, "jobs": jobs}), encoding="utf-8")
            guest.copy(plan, remote + "/plan.incoming")
            guest.run("$ErrorActionPreference='Stop'; Move-Item '" + remote + "/plan.incoming' '" + remote + "/plan.json'")
        rc = process.wait(timeout=guest.remaining() + 10)
        reader.join(timeout=5)
        return gate_exit_code(rc, lines)
    finally:
        # Cancellation is a guest-side request: the supervisor kills the worker's
        # whole process tree before releasing its lock or deleting the test homes.
        if process is not None and process.poll() is None:
            try:
                cancel = Guest(Path(guest.options[1]), time.monotonic() + 30)
                cancel.run("if (Test-Path '" + remote + "') { New-Item -ItemType File -Force '" + remote + "/cancel' | Out-Null }")
                process.wait(timeout=30)
            except (subprocess.SubprocessError, OSError):
                process.kill()
                process.wait()
                print("Guest supervisor retains the lock until its wall-clock cap; do not remove it manually.", file=sys.stderr)
        try:
            cleanup = Guest(Path(guest.options[1]), time.monotonic() + 30)
            # The supervisor owns cleanup once launched; never remove a live run.
            cleanup.run("if ((Test-Path '" + remote + "') -and -not (Test-Path '" + ROOT + "/gate.lock')) { Remove-Item -Recurse -Force '" + remote + "' }")
        except (subprocess.SubprocessError, OSError):
            print("Remote cleanup unavailable; guest cap/next run will reclaim abandoned run directories.", file=sys.stderr)
        print("\nWindows failure summary:\n" + failure_summary(lines) if not (process is not None and
              gate_exit_code(process.returncode, lines) == 0) else "\nWindows gate passed.", flush=True)
        print("Windows gate wall time: {:.2f}s".format(time.monotonic() - start), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="origin/main", help="diff base (default: origin/main)")
    parser.add_argument("--full", action="store_true", help="run lib + integration")
    parser.add_argument("--filter", help="ad-hoc lib filter (both harnesses with --full)")
    parser.add_argument("--timeout-minutes", type=int, default=60, help="wall-clock cap, including transfer (default: 60)")
    parser.add_argument("--status", action="store_true", help="guest disk free and persistent target size")
    parser.add_argument("--plan", action="store_true", help="print the selected commands without contacting the VM")
    args = parser.parse_args()
    if args.timeout_minutes <= 0 or args.filter == "":
        parser.error("timeout must be positive and filter must not be empty")
    config = Path(os.environ.get("AFT_WINDOWS_GATE_SSH_CONFIG",
                                "~/Work/Projects/CortexKit/prefrontal/script/windows-vm/ssh-config")).expanduser()
    guest = Guest(config, time.monotonic() + args.timeout_minutes * 60)
    if args.status:
        guest.run("$ErrorActionPreference='Stop'; $disk=Get-PSDrive C; Write-Output ('C: free {0:N2} GiB' -f ($disk.Free/1GB)); "
                  "$size=(Get-ChildItem '" + ROOT + "/target' -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum; "
                  "Write-Output ('Target {0}: {1:N2} GiB' -f '" + ROOT + "/target', ($size/1GB)); "
                  "if (Test-Path '" + ROOT + "/gate.lock') { Get-Content '" + ROOT + "/gate.lock' }")
        return 0
    os.chdir(git("rev-parse", "--show-toplevel"))
    sha = git("rev-parse", "--verify", "HEAD^{commit}")
    base = git("rev-parse", "--verify", args.base + "^{commit}")
    paths = subprocess.check_output(["git", "diff", "--name-only", "-z", base + ".." + sha]).decode().split("\0")
    jobs = select_tests(paths, args.full, args.filter)
    print("Windows x64 plan: {}..{}; cap {} min; jobs/test threads 4".format(base, sha, args.timeout_minutes), flush=True)
    if jobs:
        print("  storage preflight: cargo " + subprocess.list2cmdline(cargo_args({
            "kind": "lib", "target": "", "filter": "gate_hermeticity_tests::gate_resolves_every_user_config_and_state_path_under_the_gate_homes"
        })), flush=True)
    for job in jobs:
        print("  cargo " + subprocess.list2cmdline(cargo_args(job)), flush=True)
    if not jobs:
        print("No Rust tests selected; use --full or --filter for an explicit gate.")
        return 0
    if args.plan:
        return 0
    if not config.is_file():
        parser.error("SSH config not found: " + str(config))
    if git("status", "--porcelain"):
        print("Note: working-tree/untracked changes are NOT under test; shipping committed HEAD only.", flush=True)
    return run_gate(guest, sha, base, jobs, args.timeout_minutes * 60)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.SubprocessError, OSError, RuntimeError, queue.Empty) as error:
        print("WINDOWS GATE FAILED: " + str(error), file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print("WINDOWS GATE CANCELLED", file=sys.stderr)
        sys.exit(130)
