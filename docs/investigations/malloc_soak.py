#!/usr/bin/env python3
"""Run the isolated Rust broker with deterministic 768-float HTTP embeddings.

Usage: python3 docs/investigations/malloc_soak.py EXPERIMENT_DIR
Build aft and the malloc_soak example with --profile stage first. Use a fresh
directory outside any target/ or node_modules/ ancestor, then clone the
repository into EXPERIMENT_DIR/repo. All profiling targets the child PID written
by that broker, never a discovered or production daemon.
"""
import hashlib
import http.server
import json
from pathlib import Path
import os
import subprocess
import sys
import threading
import time


class Embeddings(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        texts = body.get("input", [])
        if isinstance(texts, str):
            texts = [texts]
        data = []
        for i, text in enumerate(texts):
            digest = hashlib.sha256(text.encode()).digest()
            vector = [(digest[j % 32] - 127.5) / 127.5 for j in range(768)]
            data.append({"index": i, "embedding": vector})
        payload = json.dumps({"data": data}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


def main():
    out = Path(sys.argv[1]).resolve()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Embeddings)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    log = (out / "broker.log").open("w")
    broker = subprocess.Popen([
        "target/stage/examples/malloc_soak", os.environ.get("AFT_SOAK_BINARY", "target/stage/aft"), str(out),
        f"http://127.0.0.1:{server.server_port}",
    ], stdout=log, stderr=subprocess.STDOUT)
    start = time.monotonic()
    while not (out / "pid").exists():
        if broker.poll() is not None or time.monotonic() - start > 120:
            raise RuntimeError("broker did not publish its child PID")
        time.sleep(1)
    pid = int((out / "pid").read_text())
    assert pid not in {11567, 96581}, "production process is forbidden"
    samples = (out / "samples.jsonl").open("w")
    n = 0
    while broker.poll() is None:
        cycle_start = time.monotonic()
        commands = {
            "footprint": ["footprint", "-p", str(pid)],
            "heap": ["heap", "-s", str(pid)],
        }
        if n % 5 == 0 or (out / "finished").exists():
            commands["history"] = ["malloc_history", str(pid), "-allBySize"]
        if (out / "finished").exists():
            commands["leaks"] = ["leaks", "--noContent", str(pid)]
        for name, command in commands.items():
            path = out / f"sample-{n:03}-{name}.txt"
            with path.open("w") as output:
                try:
                    result = subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, timeout=150)
                    status = result.returncode
                except subprocess.TimeoutExpired:
                    status = "timeout"
            samples.write(json.dumps({"sample": n, "elapsed_s": round(time.monotonic()-start, 1),
                                      "time": time.time(), "pid": pid, "tool": name, "status": status,
                                      "path": path.name, "unbound": (out / "unbound").exists()}) + "\n")
            samples.flush()
        n += 1
        if (out / "finished").exists():
            break
        remaining = max(0, 180 - (time.monotonic() - cycle_start))
        try:
            broker.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            pass
    status = broker.wait()
    server.shutdown()
    samples.close()
    log.close()
    if status:
        raise SystemExit(status)


if __name__ == "__main__":
    main()
