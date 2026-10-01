#!/usr/bin/env python3
"""Loopback `/rerank` adapter for measuring a llama.cpp reranker through AFT.

It forwards AFT's `remote`-backend requests to `llama-server --reranking`
(`/v1/rerank`, the same request and response shape) and lets a benchmark break
the backend on purpose, to watch AFT fall back to its fused order:
`POST /control` with `{"mode": "ok" | "stall" | "error"}`. `stall` holds every
request longer than AFT's deadline, `error` answers 503.

AFT accepts only relevance scores in [0, 1]. A GGUF converted with a rank
classifier head (llama.cpp reports pooling type "rank") already returns
probabilities; one without it returns raw logits, which `--logistic` maps into
[0, 1]. The logistic function is monotonic, so it never changes the order of
one request's candidates, which is all AFT uses.

    python3 rerank_score_proxy.py --upstream http://127.0.0.1:8091 --port 8092
"""
from __future__ import annotations

import argparse
import json
import math
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = {"mode": "ok", "stall_seconds": 30.0, "requests": 0}
LOCK = threading.Lock()


def logistic(value: float) -> float:
    if value >= 0:
        return 1.0 / (1.0 + math.exp(-value))
    exp = math.exp(value)
    return exp / (1.0 + exp)


class Handler(BaseHTTPRequestHandler):
    upstream = ""
    logistic = False

    def log_message(self, *_args: object) -> None:
        return

    def _reply(self, status: int, body: object) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self) -> None:  # noqa: N802 (http.server naming)
        length = int(self.headers.get("content-length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        if self.path == "/control":
            with LOCK:
                STATE.update({key: body[key] for key in ("mode", "stall_seconds") if key in body})
                self._reply(200, dict(STATE))
            return
        if self.path.rstrip("/") != "/rerank":
            self._reply(404, {"error": "not found"})
            return
        with LOCK:
            STATE["requests"] += 1
            mode = STATE["mode"]
            stall = float(STATE["stall_seconds"])
        if mode == "error":
            self._reply(503, {"error": "injected failure"})
            return
        if mode == "stall":
            time.sleep(stall)
        request = urllib.request.Request(
            self.upstream + "/v1/rerank",
            data=json.dumps({"query": body["query"], "documents": body["documents"], "top_n": len(body["documents"])}).encode(),
            headers={"content-type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            upstream = json.loads(response.read())
        results = [
            {
                "index": row["index"],
                "relevance_score": logistic(float(row["relevance_score"])) if self.logistic else float(row["relevance_score"]),
            }
            for row in upstream["results"]
        ]
        self._reply(200, {"results": results})

    def do_GET(self) -> None:  # noqa: N802
        with LOCK:
            self._reply(200, dict(STATE))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--upstream", required=True, help="llama-server base URL")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--logistic", action="store_true", help="map raw upstream scores into [0, 1]")
    args = parser.parse_args()
    Handler.upstream = args.upstream.rstrip("/")
    Handler.logistic = args.logistic
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
