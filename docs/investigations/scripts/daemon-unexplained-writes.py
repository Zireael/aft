#!/usr/bin/env python3
"""macOS-only isolated daemon workload; never connects to the operator's daemon."""
import argparse
import concurrent.futures
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("aft-bin", "subc-bin", "probe-bin", "interposer", "output-dir"):
        parser.add_argument("--" + arg, required=True, type=Path)
    args = parser.parse_args()
    home = args.output_dir.resolve()
    home.mkdir(parents=True, exist_ok=False)
    aft, subc, probe_bin, interposer = (
        getattr(args, name).resolve()
        for name in ("aft_bin", "subc_bin", "probe_bin", "interposer")
    )
    for directory in ("home", "runtime", "config/cortexkit", "data", "state", "cache", "tmp"):
        (home / directory).mkdir(parents=True, exist_ok=True)
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home / "home"),
           "TMPDIR": str(home / "tmp"), "SUBC_PORT": "0"}
    for name, directory in (("RUNTIME_DIR", "runtime"), ("CONFIG_HOME", "config"),
                            ("DATA_HOME", "data"), ("STATE_HOME", "state"), ("CACHE_HOME", "cache")):
        env["XDG_" + name] = str(home / directory)
    connection = home / "runtime/subc-connection.json"
    (home / "config/cortexkit/subc.jsonc").write_text(json.dumps({
        "version": 1, "storage": {"backend": "sqlite", "data_home": str(home / "data")},
        "modules": {"aft": {"program": str(aft), "args": [], "enabled": True,
                            "env": {"DYLD_INSERT_LIBRARIES": str(interposer),
                                    "AFT_IO_PROBE_LOG": str(home / "io-probe.tsv")}}}}))
    (home / "config/cortexkit/aft.jsonc").write_text(json.dumps({
        "indexes": {"search": True, "semantic": False, "callgraph": True},
        "subc": {"connection_file": str(connection)}}))
    roots = []
    for number in range(3):
        root = home / f"project{number}"
        root.mkdir()
        roots.append(root)
        for i in range(200):
            (root / f"file{i}.py").write_text("".join(
                f'def operation_{i}_{k}(value):\n    """Compute transformed value for operation {i} {k}."""\n'
                f"    return value + {k}\n\n" for k in range(20)))
        subprocess.run(["git", "init", "-q", str(root)], check=True, env=env)

    def probe(number, tool, params):
        result = subprocess.run([
            str(probe_bin), "--subc", str(connection), "--module-id", "aft",
            "--harness", "runner", "--session", f"io-session-{number}",
            "--root", str(roots[number]), "--tool", tool, "--args", json.dumps(params)],
            capture_output=True, text=True, timeout=120, env=env)
        with (home / f"calls-{number}.jsonl").open("a") as log:
            log.write(json.dumps({"tool": tool, "args": params, "code": result.returncode,
                                  "stdout": result.stdout, "stderr": result.stderr}) + "\n")
        if result.returncode:
            raise RuntimeError(result.stderr)
        response = json.loads(result.stdout)
        if response.get("isError"):
            raise RuntimeError(result.stdout)
        return response

    def census(label):
        result = subprocess.run([str(aft), "profile", "--writes", "--json"], env=env,
                                capture_output=True, text=True, timeout=60)
        (home / f"{label}-census.json").write_text(result.stdout)
        (home / f"{label}-census.err").write_text(result.stderr)
        return result.returncode

    lib = ctypes.CDLL("/usr/lib/libproc.dylib")

    def io(pid):
        # rusage_info_v4: UUID occupies two u64 slots; disk I/O is slots 18/19,
        # logical writes slot 29 (Darwin sys/resource.h). Reserve a larger buffer.
        buffer = (ctypes.c_uint64 * 128)()
        if lib.proc_pid_rusage(pid, 4, ctypes.byref(buffer)) != 0:
            raise RuntimeError("proc_pid_rusage failed")
        return {"physical": buffer[19], "read": buffer[18], "logical": buffer[29]}

    def load(number):
        probe(number, "bash", {"command": "python3 -c \"print('output record\\n' * 10000)\"", "wait": True})
        probe(number, "search", {"query": "operation_42_3"})
        probe(number, "inspect", {"scope": str(roots[number])})
        for k in range(4):
            probe(number, "edit", {"path": str(roots[number] / f"file{k}.py"), "edits": [{
                "oldString": f"return value + {k}\n", "newString": f"return value + {k+1000}\n"}]})
            probe(number, "search", {"query": f"operation_{k}_3"})

    with (home / "subc.stdout").open("w") as out, (home / "subc.stderr").open("w") as err:
        child = subprocess.Popen([str(subc)], env=env, start_new_session=True, stdout=out, stderr=err)
        try:
            deadline = time.monotonic() + 60
            while not connection.exists():
                if time.monotonic() > deadline:
                    raise RuntimeError("isolated daemon did not publish connection")
                time.sleep(.2)
            for _ in range(60):
                try:
                    probe(0, "status", {})
                    if census("ready") == 0:
                        break
                except RuntimeError:
                    pass
                time.sleep(1)
            else:
                raise RuntimeError("isolated module not ready")
            candidates = subprocess.check_output(["pgrep", "-f", str(connection)], text=True).split()
            pid = next(int(p) for p in candidates if str(aft) in subprocess.check_output(
                ["ps", "-p", p, "-o", "command="], text=True))
            time.sleep(3)
            (home / "before-io-probe.tsv").write_text((home / "io-probe.tsv").read_text())
            start, before = time.monotonic(), io(pid)
            if census("before"):
                raise RuntimeError("initial census failed")
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                list(pool.map(load, range(3)))
            time.sleep(65)
            after = io(pid)
            if census("after"):
                raise RuntimeError("final census failed")
            (home / "after-io-probe.tsv").write_text((home / "io-probe.tsv").read_text())
            result = {"pid": pid, "elapsed_s": time.monotonic() - start, "before": before, "after": after}
            (home / "measurement.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result))
        finally:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()


if __name__ == "__main__":
    main()
