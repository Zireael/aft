#!/usr/bin/env python3
"""Before/after query-latency A/B for the disk-backed trigram index.

BEFORE = resident HashMap index (commit dae42377, pre-rewrite).
AFTER  = disk-backed pread index (commit 0a44b734).
The two binaries differ ONLY in the search storage layer, so any latency delta
is attributable to the pread/base-delta change.

Per query: WARMUP iters (also warms OS page cache for the pread path), then ITERS
timed iters; report p50/p95/mean. Match counts are compared before vs after as a
correctness sanity check inside the latency run. RSS captured for context.
"""
import subprocess, json, time, sys, os, select, statistics

BEFORE = "/tmp/aft-before-wt/target/release/aft"
AFTER  = os.path.expanduser("~/Work/Projects/CortexKit/aft/target/release/aft")

REPOS = [
    # reth already measured clean (search-only, byte-identical): pread == resident
    # latency, lower RAM. This run isolates the chromium SCALE case.
    ("chromium", os.path.expanduser("~/Work/OSS/chromium"), True),   # 161k files: bypass lifts BEFORE's 20k cap so its resident index builds
]
QUERIES = [
    # (label, pattern, include, is_regex)
    ("literal common",        "return",            None,     False),  # extremely common trigrams -> huge posting lists
    ("literal mid",           "TODO",              None,     False),
    ("literal rare ident",    "DCHECK_EQ",         None,     False),  # rare -> tiny posting lists
    ("literal + glob",        "include",           "*.h",    False),
    ("literal fn",            "void",              "*.cc",   False),
    ("regex method",          r"void\s+\w+::On\w+", "*.cc",  True),
    ("regex derive",          r"#\[derive\(",      "*.rs",   True),
]
ITERS = 25
WARMUP = 5

class Aft:
    def __init__(self, binary, repo, bypass):
        env = dict(os.environ)
        env["AFT_STORAGE_DIR"] = f"/tmp/aft-ab-store-{os.path.basename(binary)}-{int(time.time())}"
        self.proc = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, bufsize=0, env=env)
        self.buf = b""; self.id = 0
        cfg = {"id": "cfg", "command": "configure", "project_root": repo,
               "harness": "runner",
               "config": [{"tier": "user", "doc": json.dumps({"indexes": {"trigram": True, "semantic": False, "callgraph": False}})}]}
        if bypass:
            cfg["_bypass_size_limits"] = True
        self._send(cfg); self._recv(timeout=60)

    def _send(self, o):
        self.proc.stdin.write((json.dumps(o) + "\n").encode()); self.proc.stdin.flush()

    def _recv(self, timeout=60):
        dl = time.time() + timeout
        while time.time() < dl:
            if select.select([self.proc.stdout], [], [], 0.1)[0]:
                c = os.read(self.proc.stdout.fileno(), 65536)
                if c: self.buf += c
            t = self.buf.decode(errors="replace")
            if "\n" in t:
                line, rest = t.split("\n", 1); self.buf = rest.encode()
                if line.strip():
                    try: return json.loads(line.strip())
                    except json.JSONDecodeError: continue
        return None

    def call(self, command, params=None):
        self.id += 1
        m = {"id": str(self.id), "command": command}
        if params: m.update(params)
        self._send(m); return self._recv()

    def wait_ready(self, max_wait=1800):
        """Poll grep until the index is genuinely Ready. Fallback/Building are NOT
        ready (Fallback => trigram index not engaged; would measure the ripgrep
        path, not the index). Hard-fail the run rather than silently benchmark
        the wrong path."""
        dl = time.time() + max_wait
        last = "?"
        while time.time() < dl:
            r = self.call("grep", {"pattern": "fn"})
            last = (r or {}).get("index_status", "?")
            if last in ("Ready", "indexed", "ready"):
                time.sleep(0.5)
                return last
            time.sleep(2)
        raise RuntimeError(f"index never reached Ready (last status={last}); benchmark would measure the fallback path")

    def rss_mb(self):
        try:
            r = subprocess.run(["ps", "-o", "rss=", "-p", str(self.proc.pid)],
                               capture_output=True, text=True)
            return int(r.stdout.strip()) / 1024
        except Exception:
            return 0.0

    def close(self):
        try: self.proc.terminate(); self.proc.wait(timeout=10)
        except Exception: self.proc.kill()

def matches(r):
    return (r or {}).get("total_matches", -1) if (r and r.get("success")) else -1

def bench_query(client, pattern, include, is_regex):
    # Default match cap = realistic agent query. We measure index-lookup +
    # modest result build, NOT 200k-match materialization (which would swamp the
    # pread-vs-resident signal with shared response-building code). Match-count
    # correctness is established separately by search-parity.test.ts (byte-
    # identical); the +/-1 match diffs here are mtime-tie clipping at the cap.
    params = {"pattern": pattern, "max_results": 500}
    if include: params["include"] = [include]
    # cold (first touch after warm index — page cache may still be cold for pread)
    t0 = time.perf_counter(); first = client.call("grep", params); cold = (time.perf_counter()-t0)*1000
    for _ in range(WARMUP): client.call("grep", params)
    samples = []
    for _ in range(ITERS):
        t = time.perf_counter(); r = client.call("grep", params); samples.append((time.perf_counter()-t)*1000)
    samples.sort()
    p50 = statistics.median(samples)
    p95 = samples[min(len(samples)-1, int(round(0.95*(len(samples)-1))))]
    return {"cold_ms": cold, "p50_ms": p50, "p95_ms": p95, "mean_ms": statistics.mean(samples),
            "matches": matches(r), "status": (r or {}).get("index_status","?")}

def run_repo(name, repo, bypass):
    if not os.path.isdir(repo):
        print(f"  SKIP {name}: {repo} not found"); return None
    out = {"repo": name, "before": {}, "after": {}}
    for tag, binary in (("before", BEFORE), ("after", AFTER)):
        print(f"\n  [{name}] {tag}: {binary}")
        c = Aft(binary, repo, bypass)
        st = c.wait_ready()
        rss = c.rss_mb()
        print(f"    index status={st}  RSS={rss:.0f} MB")
        out[tag]["rss_mb"] = rss; out[tag]["ready"] = st; out[tag]["q"] = {}
        for label, pat, inc, rgx in QUERIES:
            res = bench_query(c, pat, inc, rgx)
            out[tag]["q"][label] = res
            print(f"    {label:<20} cold={res['cold_ms']:7.2f}  p50={res['p50_ms']:7.3f}  p95={res['p95_ms']:7.3f}  m={res['matches']}")
        c.close()
        time.sleep(1)
    return out

def main():
    for b in (BEFORE, AFTER):
        if not os.path.exists(b):
            print(f"MISSING binary: {b}"); sys.exit(1)
    print(f"BEFORE={BEFORE}\nAFTER ={AFTER}\nITERS={ITERS} WARMUP={WARMUP}")
    results = []
    for name, repo, bypass in REPOS:
        r = run_repo(name, repo, bypass)
        if r: results.append(r)
    # delta table
    print("\n" + "="*92)
    print(f"  {'repo/query':<32} {'before p50':>11} {'after p50':>11} {'delta':>9} {'b p95':>9} {'a p95':>9} {'match':>7}")
    print("  " + "-"*90)
    for r in results:
        print(f"  {r['repo']+' RSS':<32} {r['before'].get('rss_mb',0):>10.0f}M {r['after'].get('rss_mb',0):>10.0f}M")
        for label, *_ in QUERIES:
            b = r["before"]["q"].get(label, {}); a = r["after"]["q"].get(label, {})
            bp = b.get("p50_ms", 0); ap = a.get("p50_ms", 0)
            delta = f"{((ap-bp)/bp*100):+.1f}%" if bp > 0 else "n/a"
            mok = "ok" if b.get("matches") == a.get("matches") else f"B{b.get('matches')}!=A{a.get('matches')}"
            print(f"  {r['repo']+': '+label:<32} {bp:>10.3f}m {ap:>10.3f}m {delta:>9} {b.get('p95_ms',0):>8.2f}m {a.get('p95_ms',0):>8.2f}m {mok:>7}")
    path = os.path.join(os.path.dirname(__file__), "trigram-ab-results.json")
    with open(path, "w") as f: json.dump(results, f, indent=2)
    print(f"\n  saved {path}")

if __name__ == "__main__":
    main()
