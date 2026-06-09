# Spike: Full Semble Corpus CI Workflow

> **Bead:** `aft-t6p.bench.full-ci.1`  
> **Parent epic:** `aft-t6p`  
> **Date:** 2026-06-09  
> **Scope constraint:** `aft-t6p.scope.1` — full 63-repo benchmark must **NOT** be required PR CI.

## 1. Corpus Profile

| Metric | Value |
|--------|-------|
| Total repos | **63** |
| Languages | 18 (Python, JavaScript, Go, Java, PHP, Ruby, Rust, TypeScript, C#, Kotlin, Scala, Swift, C++, Elixir, C, Bash, Haskell, Lua, Zig) |
| Annotated repos | **5** (pilot subset: axum, express, pydantic, serde, gin) |
| Pilot queries | **50** (10/repo, 3 categories: symbol, semantic, architecture) |
| Unannotated repos | **58** — no query annotations exist |
| Benchmark root defined | **55/63** repos (8 have `null` root — whole-repo scan) |

> **Critical finding:** Only 5 of 63 repos have human-authored annotations. A full-corpus evaluation against ground-truth relevance judgments is impossible without annotating the other 58 repos. The upstream [MinishLab/semble](https://github.com/MinishLab/semble) may have additional annotations — those should be imported before any workflow buildout.

---

## 2. Resource Estimates

### 2.1 Disk

Based on pilot data (5 repos averaged ~8-12MB each in their `benchmark_root` with pinned commits):

| Scenario | Estimated Disk | Source |
|----------|---------------|--------|
| `.bench-cache/` — full git history (no `--depth`) | **8–15 GB** | 63 repos × typical 150MB `.git/` for popular OSS |
| `.bench-cache/` — shallow clone (`--depth 1`) | **2–4 GB** | Working-tree only, 63 repos × ~30-60MB |
| AFT semantic index per repo | **~200 MB** | 63 repos × ~3MB index each |
| Reports + artifacts | **~5 MB** | JSON reports negligible |

**GitHub Actions `ubuntu-22.04` runner disk:** 14 GB SSD.

- **Full-history clones BARELY fit.** Risk of `no space left on device` when combined with OS/tooling overhead (~4 GB). Not recommended.
- **Shallow clones fit comfortably.** 2-4 GB for cache + 200 MB for indices + ~4 GB OS overhead ≈ 6-8 GB used. Leaves ~6 GB headroom.

### 2.2 Network

| Phase | Data transferred | Notes |
|-------|-----------------|-------|
| Initial clone (shallow, `--depth 1`) | **~1–2 GB** | 63 repos × ~15-30 MB wire |
| Subsequent sync (`git fetch`) | **~50–200 MB** | Only new objects since last fetch; `actions/cache` makes this the common case |
| AFT binary download | **~15–30 MB** | Per runner cold start; extract from GH release |
| ONNX Runtime install | **~22 MB** | `all-MiniLM-L6-v2` model; one-time per runner |

GitHub Actions network bandwidth: typically 100–200 Mbps. Shallow clone of all 63 repos: ~2–4 minutes sequential, ~30–60 seconds if parallelized in batches.

### 2.3 Indexing Time (AFT semantic search, first-time cold)

| Repo class | Count | Est. per repo | Total |
|------------|-------|---------------|-------|
| Small (< 500 files) | ~30 | 2–5 s | 1–2.5 min |
| Medium (500–2000 files) | ~20 | 5–30 s | 1.5–10 min |
| Large (> 2000 files, e.g. rails, abseil-cpp, zig, curl) | ~13 | 30–120 s | 6.5–26 min |
| **Total** | **63** | | **~10–40 min** |

AFT uses fastembed (ONNX, local, all-MiniLM-L6-v2). Index builds in a single-threaded pass over source files. File-count from the README benchmarks: 3,953 C++ files (Chromium/base) took ~2 minutes. Most repos are under 2K files where indexing takes ~2 s.

**Cached index (subsequent runs):** near-zero. AFT persists indices to disk. Restoring from `actions/cache` takes seconds.

### 2.4 Query Execution Time

Assuming all 63 repos had ~10 queries each (630 queries total), run across 4 modes:

| Mode | Per query | Total (630 queries) | Notes |
|------|-----------|---------------------|-------|
| **Lexical** (ripgrep) | ~50 ms | **~32 s** | Sequential per-repo; rg is fast |
| **Semantic** (AFT fastembed) | ~2 ms (warm) | **~1.3 s** | Sub-millisecond once index is built |
| **Hybrid** (AFT) | ~3 ms | **~2 s** | Semantic + lexical fusion |
| **Reranked** (AFT + LLM) | ~500–1500 ms | **5–16 min** | LLM call per query; dominant cost |
| **Total (all modes)** | | **~6–17 min** | Dominated by reranked mode if enabled |

Without reranked mode (the most expensive by far): **~40 seconds** total query execution.

### 2.5 Total Estimated Workflow Wall Time

| Scenario | Cold cache | Warm cache |
|----------|------------|------------|
| **Lexical only** (no AFT binary needed) | ~7–15 min (clone dominated) | ~2–5 min (fetch + rg scan) |
| **All 4 modes, shallow clone** | ~60–90 min | ~10–20 min |
| **Lexical + semantic + hybrid, shallow clone** | ~30–55 min | ~5–10 min |

**All scenarios fit within GitHub's 6-hour runner timeout.** The main cost is first-time index build.

---

## 3. GitHub Actions Runner Feasibility

### Standard `ubuntu-22.04` runner specs

| Resource | Spec | Full-clone fit | Shallow-clone fit |
|----------|------|----------------|-------------------|
| **vCPU** | 2 | Barely adequate for sequential ops | ✓ — mostly sequential work anyway |
| **RAM** | 7 GB | ✓ — AFT semantic search needs ~500 MB | ✓ |
| **SSD** | 14 GB | ⚠️ TIGHT — 8–15 GB estimate = risk of OOD | ✓ — 2–4 GB leaves headroom |
| **Max runtime** | 6 hrs | ✓ — estimated 60–90 min | ✓ — estimated 10–55 min |
| **Max artifact retention** | 90 days (default 30) | ✓ | ✓ |

### Verdict: Feasible with shallow clones

**Shallow clones are mandatory.** Without them, the 14 GB disk limit is a real risk for several repos (rails: ~200 MB `.git`, zig: ~300 MB, abseil-cpp: ~150 MB). Use `git clone --depth 1 --single-branch` for initial clone. For subsequent sync runs, `git fetch --depth 1` tops up the shallow boundary.

### Cache strategy

```
┌─────────────────────────────────────────────────┐
│ actions/cache key: bench-cache-${{ hashFiles(    │
│   'benchmarks/semble/repos.json') }}             │
│ Restores on: restore-keys: bench-cache-          │
│ Path: .bench-cache/                              │
├─────────────────────────────────────────────────┤
│ actions/cache key: aft-index-${{ hashFiles(      │
│   'benchmarks/semble/repos.json') }}             │
│ Path: ~/.cache/aft/semantic/                     │
└─────────────────────────────────────────────────┘
```

- **Cache hit rate expected high:** `repos.json` changes only when pinned revisions are bumped.
- **Cache miss:** Only on the first run or after a revision roll. On miss, full clone + index build.
- **AFT binary:** Use `actions/cache` for `~/.cache/aft/bin/` to avoid re-download on each run.

---

## 4. Annotation Gap — The Real Bottleneck

### Current state

Only **5/63 repos** have human-authored annotations (50 queries). This means:

- A full-corpus benchmark evaluating retrieval **quality** (recall, MRR, NDCG) is **impossible today** for 58 repos.
- You CAN run the full corpus for: clone times, index build times, file counts, disk usage, and query latency — but not retrieval accuracy.
- The upstream Semble project may have additional annotations. Importing them via `benchmarks/semble/import.ts` should be step one.

### Options to close the gap

| Approach | Effort | Quality | Notes |
|----------|--------|---------|-------|
| **Import upstream Semble** | Low (automated) | Medium | If upstream has annotations; unknown coverage |
| **Auto-generate annotations** | Low | Low | Use `import` statements or symbol definitions as automatic relevance — noisy but fills gaps |
| **Human-annotate per repo** | High (~2–4 hrs/repo) | High | Gold standard; 58 × 3 hrs ≈ 174 hrs — not viable in one sprint |
| **Sampling** (annotate 5 more diverse repos) | Medium (~15 hrs) | Medium | Strategic expansion to 10 repos across more languages |
| **Skip quality eval on unannotated repos** | None | N/A | Run benchmark as infra stress test only (timings, disk, reliability) |

### Recommendation

**Phase 1**: Import upstream Semble annotations, auto-generate basic symbol-based annotations for the remaining repos, and run a "full corpus stress test" (clone + index + latency measurement, not accuracy). This validates the pipeline works at scale.

**Phase 2**: Annotate 5–10 additional strategically chosen repos from underrepresented languages (C++, Haskell, Lua, Zig, Scala, Swift). Target 10 annotated repos minimum before running accuracy-focused full corpus benchmarks.

---

## 5. Workflow Shape (Recommended)

### 5.1 Trigger: Manual (`workflow_dispatch`) + optional scheduled

```yaml
on:
  workflow_dispatch:
    inputs:
      scope:
        description: "Benchmark scope: pilot (5 repos) or full (63 repos)"
        required: true
        default: pilot
        type: choice
        options: [pilot, full]
      modes:
        description: "Search modes to run"
        default: "lexical"
        type: choice
        options: [lexical, all]  # "all" = lexical + semantic + hybrid
  schedule:
    - cron: "0 2 * * 0"  # Weekly Sunday 02:00 UTC — optional, decide after manual validation
```

### 5.2 Pipeline

```
1. Checkout repo
       │
2. Restore .bench-cache/ from actions/cache
       │
3. Install deps (bun, AFT binary, ONNX)
       │
4. corpus.ts sync [--pilot | full]    ← shallow clone, fetches missing only
       │
5. corpus.ts check                     ← verify all repos at pinned revisions
       │
6. Build AFT semantic index            ← or restore from cache
       │
7. Run fixtures.json through all modes
   ├── pilot.ts / ablation.ts          ← lexical (rg), semantic (AFT), hybrid
   └── speed.ts                        ← index build time, query latency
       │
8. ci.ts --baseline --current          ← regression detection against stored baseline
       │
9. Upload report artifacts:
   ├── pilot-report.json / ablation-report.json
   ├── speed-report.json
   ├── ci-comparison.json
   └── summary.txt                     ← human-readable digest
       │
10. Save .bench-cache/ + .cache/aft/ to actions/cache
```

### 5.3 Key design decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Clone strategy | **Shallow** (`--depth 1`) | Disk constraint on standard runners |
| Parallel clones | **Batches of 5** via `corpus.ts --repo` | Avoid network saturation; implemented via simple Bash loop |
| Index caching | **`actions/cache`** | 10–40 min cold build → seconds restore |
| Report format | **JSON** (existing ci.ts) + **summary text** | Machine-readable for comparison, human-readable for quick review |
| Artifact retention | **90 days** | Trend tracking across releases |
| Blocking / gate | **Never blocks PR merge** | Per `aft-t6p.scope.1` |
| Baseline storage | **Git-tracked** (`benchmarks/semble/baseline/`) | Version-controlled, auditable; updated manually or after intentional improvements |

---

## 6. Reproducibility Metadata

Every report artifact must embed:

```json
{
  "timestamp": "2026-06-09T12:00:00Z",
  "workflow_run_id": 12345678,
  "workflow_ref": "refs/heads/main@abc123def",

  "corpus": {
    "repos_file": "benchmarks/semble/repos.json",
    "revision_hash": "sha256-of-repos.json",
    "repo_count": 63,
    "clone_depth": 1
  },

  "fixture": {
    "schema_version": 1,
    "annotation_count": 50,
    "source": "semble-pilot"
  },

  "tooling": {
    "aft_version": "0.x.y",
    "bun_version": "1.x.y",
    "ripgrep_version": "14.x.y",
    "onnx_version": "1.x.y",
    "semantic_model": "all-MiniLM-L6-v2"
  },

  "modes_ran": ["lexical", "semantic", "hybrid"],
  "k": 10,

  "environment": {
    "runner": "github-actions-ubuntu-22.04",
    "cpu": "x86_64",
    "ram_gb": 7
  }
}
```

This enables:
- **Cross-run comparison**: exact same corpus, same tool versions, comparable results
- **Regression attribution**: knowing which AFT version introduced a regression
- **Cache debugging**: knowing whether cache was warm or cold

---

## 7. Cost Assessment

| Item | Cost |
|------|------|
| **GitHub Actions compute** | **Free for public repos** (aft is MIT, public). If private: ~$0.008/min × 60 min = ~$0.48/run |
| **Storage (artifacts, 90 days)** | Free tier: 500 MB included. Each report is ~50 KB. Negligible. |
| **Cache storage** | Free tier: 10 GB. `.bench-cache` at 2-4 GB is ~$0.60/month if over free tier. |
| **Developer time (annotations)** | The real cost. Phase 2 annotations: ~15-20 hrs for 5-10 repos. |
| **AFT API calls (embeddings)** | Zero — local fastembed ONNX, no API cost. |
| **Reranker LLM cost** | Zero by default — reranked mode optional, not included in recommended default pipeline. |

**Total recurring CI cost:** ~$0/run (public repo). CapEx: annotation effort.

---

## 8. Flake Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Git clone transient failure** | Medium (5% of runs) | High — blocks the entire run | Retry with backoff; `actions/cache` makes this rare on subsequent runs |
| **GitHub rate limiting** | Low | Medium | Batch clones with delay; use token for auth |
| **Disk space exhaustion** | Low (shallow) / Medium (full) | High | Enforce `--depth 1`; monitor via `df -h` step |
| **AFT binary compatibility** | Low | High | Pin AFT version; test in pilot mode first |
| **ONNX Runtime missing/version mismatch** | Low | Medium | Install via `actions/cache` + prebuilt binary |
| **Network timeout on large repo** | Low | Medium | Retry individual repos; skip and report |
| **Memory exhaustion** | Very low | Medium | 7 GB RAM is ample for AFT indexing |
| **Flaky query results** | Low (deterministic) | Low | Pinned commits + same search binary = deterministic output |
| **Cache corruption** | Low | Medium | Cache key includes `repos.json` hash; invalidate on structural change |

**Overall flake rating: LOW.** The pipeline is deterministic (pinned commits, same tool versions, local execution). The main flake vector is network-dependent clone failures, mitigated by `actions/cache`.

---

## 9. Recommendations Summary

### Do now (Phase 1 — infrastructure validation)

1. **Keep shallow clones** (`--depth 1`) mandatory in CI. Add `--single-branch` to reduce history.
2. **Add `workflow_dispatch` trigger** to the existing `tests.yml` (or a new `benchmark.yml`) with a `scope` input (`pilot`/`full`). This costs no runner minutes until triggered.
3. **Add `actions/cache` for `.bench-cache/`** and `~/.cache/aft/`. Reuses downloaded repos and indexes across runs.
4. **Import upstream Semble annotations** for any repos not yet covered. Run `import.ts --input` against the upstream source.
5. **Validate with pilot scope first** — prove the pipeline works end-to-end with the known 5-repo, 50-query set.

### Do next (Phase 2 — accuracy coverage)

6. **Annotate 5-10 additional repos** from underrepresented language families (C++, Haskell, Lua, Zig, Swift, Scala). This raises confidence in cross-language conclusions.
7. **Run full corpus as a stress test** (timing + disk + reliability), even if only 5-10 repos have accuracy annotations.
8. **Establish baseline reports** in `benchmarks/semble/baseline/` after the first clean full run.

### Do later (Phase 3 — automation)

9. **Add optional `schedule` trigger** (weekly) after the pipeline has been stable for 4+ weeks.
10. **Consider larger runners** (`ubuntu-22.04-4core`) if full-history clones become desirable for bisection.
11. **Add trend dashboard** (simple chart from accumulated CI reports) if the benchmark runs weekly for 3+ months.

### Explicitly NOT recommended (deferred/rejected)

| Proposal | Reason |
|----------|--------|
| Make full corpus benchmark a **required PR check** | Per `aft-t6p.scope.1` — requires too much time and flakes |
| Run full corpus on **every PR** even as optional | Runner minute cost + long wait; `workflow_dispatch` is sufficient |
| **Full-history clones** in CI | Exceeds 14 GB disk limit on standard runners |
| **Reranked mode** in automated CI | LLM cost + latency (adds ~5-16 min); keep as optional local-only for now |
| **Publishable comparison claims** against other tools | Not the goal of this benchmark — this is for AFT regression detection |

---

## 10. Appendices

### A. Language distribution across 63 repos

| Language | Count | Repos |
|----------|-------|-------|
| Python | 8 | aiohttp, fastapi, flask, httpx, model2vec, pydantic, requests, starlette, click |
| JavaScript | 3 | axios, express, redux |
| Go | 3 | gin, cobra, chi |
| Java | 3 | gson, commons-lang, jackson-databind |
| PHP | 3 | guzzle, monolog, laravel-framework |
| Ruby | 3 | sinatra, rack, rails |
| Rust | 4 | tokio, serde, axum, axtum |
| TypeScript | 4 | trpc, zod, vitest, (shared with JS) |
| C# | 3 | messagepack-csharp, newtonsoft-json, dapper |
| Kotlin | 3 | ktor, kotlinx-coroutines, exposed |
| Scala | 3 | cats, circe, http4s |
| Swift | 3 | alamofire, vapor, snapkit |
| Elixir | 3 | phoenix, plug, ecto |
| C++ | 3 | nlohmann-json, abseil-cpp, fmtlib |
| C | 3 | curl, redis, libuv |
| Bash | 3 | bats-core, nvm, bash-it |
| Haskell | 3 | aeson, pandoc, xmonad |
| Lua | 3 | telescope.nvim, lazy.nvim, mini.nvim |
| Zig | 3 | zig, zls, zig-clap |

### B. Existing tooling overview

| File | Purpose | Works on full corpus? |
|------|---------|-----------------------|
| `corpus.ts` | Clone/fetch/checkout to pinned commit | ✓ (63 repos) |
| `pilot.ts` | Run lexical search with scored metrics | ~ (needs fixtures; only 5 annotated) |
| `ablation.ts` | Mode comparison (lexical only currently) | ~ (same limitation) |
| `speed.ts` | Cold-start index + query latency | ✓ (no annotations needed) |
| `ci.ts` | Baseline comparison with threshold | ✓ (works on any reports) |
| `baseline-rg.ts` | Ripgrep lexical baseline | ~ (needs fixtures) |
| `token-efficiency.ts` | Recall@token_budget curves | ~ (needs fixtures + AFT binary) |

### C. Key files referenced

- `benchmarks/semble/repos.json` — 63-repo manifest with pinned commits
- `benchmarks/semble/repos-pilot.json` — 5-repo pilot subset
- `benchmarks/semble/corpus.ts` — clone/cache/checkout tooling
- `benchmarks/semble/pilot.ts` — multi-mode query runner
- `benchmarks/semble/ci.ts` — regression detection
- `benchmarks/semble/fixtures.json` — 50-query pilot fixture
- `benchmarks/semble/annotations/` — 5 human-authored annotation files
- `benchmarks/semble/schema.json` — fixture JSON schema
- `.gitignore` — `.bench-cache/` already gitignored
- `.github/workflows/tests.yml` — current CI (no benchmarks)
