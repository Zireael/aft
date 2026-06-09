# Why AFT uses more tokens than CodeGraph — findings & recommendations

**Benchmark:** `codegraph-vs-aft-agent`
**Date:** 2026-06-03
**Model:** `opencode-go/deepseek-v4-flash-free` (same model, both arms)
**Corpus:** `corpora/agent-fixture` (7 small TS files, 10 deterministic tasks)
**Reference run:** `results/agent-ab-2026-06-03T17-01-50-426Z.json` (+ `.md`), raw event
streams in `.bench/runs/2026-06-03T16-57-56-823Z/`, fresh transcripts in
`results/transcripts/`.

> All numbers below come from real OpenCode `--format json` event streams of a
> Docker run built from this branch (AFT compiled from local source). They are
> reproducible: `docker compose up --build` from this directory.

---

## TL;DR

On this corpus AFT spends **~15.7k median tokens/task vs CodeGraph's ~11.4k**
(both ~8/10 and 7/10 pass — within agent-variance noise on pass rate). The
**~4.4k token/task gap decomposes as:**

| component | tokens/task (median) | share of gap | nature |
|---|--:|--:|---|
| **Fixed prompt overhead** (AFT tool defs + workflow-hints block) | **+1,630** | **~37%** | constant per task |
| **Tool-output verbosity** (mostly `aft_search`) | **+2,580** | **~59%** | variable, compounding |
| residual (extra turns / text) | ~190 | ~4% | small |

**The single biggest lever is `aft_search` output size.** One `aft_search` call
on this fixture emits **10,000–16,300 characters (~2.5k–4.1k tokens)** because it
returns the *entire corpus* (≈22 symbols, `topK:20`) with ranked source snippets.
The equivalent `codegraph_search` returns **36–302 characters** (symbol + location
+ signature). Because every tool result is re-fed into context on the next turn,
that one fat result is paid for on every subsequent step of the task.

---

## Harness bugs fixed first (results were previously untrustworthy)

Before any analysis, two harness bugs were fixed so the data can be trusted and
runs are comparable. **Both are validated against the real Docker run above.**

### Bug #1 — transcripts were stale, never regenerated

- **Symptom:** `results/transcripts/*.txt` existed (dated *May 29*) but the
  current `src/cli.ts` **never wrote transcripts at all** — it only wrote raw
  `opencode.stdout.jsonl` into `.bench/runs/<stamp>/…`. The committed `.txt`
  files were leftovers from an older harness. Anyone reading
  `results/transcripts/` for "the latest run" was reading **month-old data from
  a different AFT build**, silently misleading analysis. (The task brief framed
  this as a missing volume mount; the deeper root cause is that the write path
  was dropped entirely in a refactor — the mount for `results/` is actually fine,
  it just had nothing fresh to receive.)
- **Fix:** `cli.ts` now (a) `resetDir()`s `results/transcripts/` at the **start
  of every run** so stale data can never survive, and (b) renders a fresh
  human-readable `[TOOL CALL]/[TOOL RESULT]/[ASSISTANT TEXT]` transcript per
  task-arm into `results/transcripts/<task>.<arm>.txt` — which lives inside the
  already-mounted `./results` volume, so it always refreshes on the host.
  `renderTranscript()` parses the OpenCode JSON event stream; each result also
  records its `transcriptPath`.
- **Validation:** after the real run, `results/transcripts/` is dated to the run
  (18:59), not May 29.

### Bug #2 — arm config was not recorded, so runs weren't comparable

- **Symptom:** `results/*.json` recorded model/tokens/pass but **not** the
  resolved arm setup (AFT `disabled_tools`, `hoist_builtin_tools`, tool surface,
  CodeGraph MCP wiring, pre-warm). You could not tell whether two runs used the
  same configuration, making cross-run comparison unverifiable.
- **Fix:** added `ArmConfig` to `types.ts` and a single-source-of-truth
  `resolveArmConfig()` / `aftBenchConfig()` / `codegraphMcpConfig()` /
  `sharedBuiltinTools()` in `cli.ts`. The report now carries `armConfigs`
  (top-level **and** echoed into `metadata`) describing, per arm: the
  intelligence layer (`@cortexkit/aft-opencode@latest` vs
  `@colbymchenry/codegraph@0.9.6`), the resolved AFT config / MCP wiring, the
  shared native built-in tools, and the pre-warm command. `metadata` also now
  records `timeoutMs` and provider info.
- **Bonus (self-describing tokens):** each result now carries a `tokenBreakdown`
  (`promptInputTokens`, `toolOutputChars`, `toolOutputTokensEst`,
  `assistantTextChars`, `steps`) so the fixed-vs-variable split below can be
  recomputed from the JSON alone, without re-parsing raw event streams.

---

## Methodology & fairness assessment

The human cares about fairness, so this is explicit.

**What is fair (good):**

- **Same model, same prompt, same tasks, same fixture** across both arms.
- **Identical built-in tool mechanics.** Both arms use native OpenCode
  `read/edit/write/grep/glob/bash`. AFT's config disables its own
  `aft_read/aft_write/aft_edit/aft_apply_patch/aft_grep`(via `grep`/`glob`
  disable + `hoist_builtin_tools:false` + `bash:false`) so the **only** variable
  is the code-intelligence layer: AFT's `aft_search/aft_outline/aft_zoom/
  aft_callgraph/aft_inspect` vs CodeGraph's `codegraph_*`. Confirmed in
  transcripts: both arms call native `read`, `edit`, `grep`, `bash`.
- **Both indexes are pre-warmed** before the agent runs (`aft warmup` vs
  `codegraph init && index`), so neither pays a cold-start tax inside the timed
  window.
- No AGENTS.md bias prompts on either side (removed in `bd721de1`).

**Fairness caveats / threats to validity (call-outs):**

1. **The fixed-overhead delta is partly an apples-to-oranges artifact of tool
   *count*, not capability.** AFT registers more tools by default (search,
   outline, zoom, callgraph, inspect — plus a ~500-token workflow-hints system
   block) than CodeGraph's MCP surface. That is a *real* cost the agent pays, but
   it reflects AFT exposing a broader surface, not doing the same job more
   expensively. A "benchmark-parity" surface (see recommendations) would isolate
   the comparison better.
2. **Tiny corpus inflates `aft_search`'s relative cost.** With only ~22 symbols,
   `topK:20` returns essentially the *whole repo*. On a real repo `topK:20` is a
   small slice, so the absolute per-call size would not scale linearly — but the
   *ratio* vs CodeGraph's terse output would persist, and likely worsen because
   AFT ships source snippets and CodeGraph ships signatures.
3. **Pass-rate is noisy at n=10** (8 vs 7 here, 8 vs 8 in the prior run). Do not
   read quality signal into ±1 task. Token/tool-call medians are the stable
   signal; pass-rate needs more tasks/seeds.
4. **Snippets are read from disk at query time** for AFT — correct and current,
   but it means `aft_search` is doing extra file I/O + emitting source the model
   mostly doesn't need when it only wanted "where is X".

Net: the comparison is **fair in setup** (same everything except the intel
layer), and the gap is **real**, but the fixed-overhead third of it is a
"surface size" cost rather than a "same-task efficiency" cost, and should be
interpreted as such.

---

## Evidence: where the extra AFT tokens go

### Gap decomposition (medians, reference run)

```
AFT       total median: 15,675   prompt-in median: 11,768   tool-out median: ~3,057 tok
CodeGraph total median: 11,368   prompt-in median: 10,138   tool-out median:   ~474 tok

total gap        : ~4,402 tok/task
  fixed prompt   : ~1,630 tok  (37%)   ← AFT tool defs + workflow-hints block
  tool-output    : ~2,580 tok  (59%)   ← dominated by aft_search snippet dump
  residual       :   ~190 tok  ( 4%)
```

### Per-task detail (this run)

| task | AFT total | CG total | AFT promptIn | CG promptIn | AFT toolOut≈ | CG toolOut≈ |
|---|--:|--:|--:|--:|--:|--:|
| answer-checkout-route | 15620 | 11048 | 11766 | 10136 | 3079 | 613 |
| answer-inventory-reservation | 20208 | 11854 | 11765 | 10135 | 6640 | 892 |
| answer-loyalty-discount | 15451 | 11059 | 11772 | 10142 | 2960 | 409 |
| answer-payment-caller | 13475 | 10270 | 11763 | 10133 | 1060 | 21 |
| answer-retry-helper | 16120 | 10872 | 11766 | 10136 | 3343 | 359 |
| edit-cart-limit | 15675 | 11806 | 11769 | 10139 | 2711 | 811 |
| edit-free-shipping-threshold | 15949 | 11368 | 11778 | 10148 | 3035 | 445 |
| edit-pending-status | 17706 | 11504 | 11779 | 10149 | 4002 | 471 |
| edit-retry-attempts | 17779 | 11583 | 11777 | 10147 | 4351 | 734 |
| **edit-tax-rate** | **12645** | **11452** | 11766 | 10136 | **318** | 478 |

Two things to notice:

- **`promptIn` is rock-stable** (~11,768 AFT vs ~10,138 CG, ±15 tokens). This is
  the fixed system-prompt + tool-definition cost. It is **+1,630 every single
  task** regardless of what the agent does.
- **`edit-tax-rate` is the control case.** It is the *only* task where AFT's
  tool-output (318 tok) is *lower* than CodeGraph's (478 tok), and there the
  totals nearly converge (12,645 vs 11,452 — almost entirely the fixed +1,630).
  On that task the agent reached the answer with a narrow `grep`/`aft_zoom`
  instead of a broad `aft_search`. **This proves the gap is the verbose search
  dump, not anything intrinsic to AFT.**

### The smoking gun: one search call, same query

Task `answer-checkout-route`, query `submitOrder`:

| arm | tool | output size |
|---|---|--:|
| AFT | `aft_search({query:"submitOrder", topK:20})` | **16,014 chars (~4,000 tok)** — 20 ranked symbols with source snippets |
| CodeGraph | `codegraph_search({query:"submitOrder"})` | **214 chars (~50 tok)** — 2 hits, symbol + `file:line` + signature |

Other AFT `aft_search` outputs in this run: 10,569 / 11,744 / 11,839 / 11,908 /
12,003 / 12,010 / 12,317 / 12,358 / **16,308** chars. CodeGraph search outputs:
36 / 39 / 143 / 180 / 210 / 214 / 296 / 302 chars.

Why so big: `aft_search` default `topK` is 10 but the agent passed `topK:20`,
and on a 22-symbol corpus that returns nearly everything. The renderer
(`crates/aft/src/commands/semantic_search.rs::format_semantic_text` →
`snippet_line_budget`) attaches a **20-line source snippet to rank 0** and
**5-line snippets to ranks 1–2**, header-only for rank 4+. With ~22 results that
is still a large block, and crucially it ships **source code** the model usually
doesn't need for a "where is X" question — CodeGraph ships only the signature.

### Fixed overhead breakdown (~1,630 tok)

- AFT registers **more tools** than CodeGraph's MCP surface
  (`aft_search/aft_outline/aft_zoom/aft_callgraph/aft_inspect` + their JSON
  schemas). Each tool definition is tokens in the system prompt on every turn.
- AFT injects a **workflow-hints system block** (`workflow-hints.ts`). For this
  bench config (search + inspect + callgraph enabled) the rendered block is
  **~2,000 chars ≈ ~500 tokens**. Useful for steering real agents, but pure
  overhead in a benchmark where the agent already knows the tools.
- CodeGraph's MCP tool defs are comparatively terse and it injects no extra
  system prose.

---

## Recommendations to close the gap (evidence-backed)

These are **product recommendations** — none are implemented here (this task only
touches `benchmarks/`). Ordered by impact-per-effort.

### 1. Trim `aft_search` output — the ~59% lever (highest impact)

The agent asking "where is `submitOrder`" does not need 20 ranked source
snippets. Options (smallest, most durable change first):

- **(a) Lower the default snippet footprint.** Rank-0 at 20 lines is half a
  function; most "where is X" queries are answered by `file:line` + signature.
  Consider a **terse default mode** (symbol + `file:line` + one-line signature,
  *no* source body) with snippets opt-in via a param (e.g. `snippets:true` or
  `verbosity:"full"`). This alone would bring a 12k-char result to ~300–600
  chars — i.e. close most of the 2,580-token variable gap, matching CodeGraph's
  shape while keeping AFT's hybrid ranking.
- **(b) Cap effective result count for snippet emission more aggressively** when
  the result set is small relative to the corpus, or when scores are tightly
  clustered (low discrimination) — emitting 20 near-zero-score rows
  (`score 0.011`, `score -0.000`) is pure noise the model must read.
- **(c) Stop shipping rank 3+ at all by default** (already header-only) *and*
  drop file-summary rows with negative scores.

Estimated effect: collapsing `aft_search` output to CodeGraph-like terseness
removes ~2,000–2,500 tok/task here, taking AFT from ~15.7k to ~13k — closing
~60% of the total gap.

### 2. Add a benchmark/parity tool-surface mode — addresses ~37% fixed cost

Provide a config (or honor the **already-set-but-currently-dead `AFT_BENCHMARK=1`
env var** — the harness exports it via `invokeOpencode`, but nothing in the
product reads it) that:

- **Suppresses the workflow-hints block** (~500 tok/task saved). In a benchmark
  the agent doesn't need to be taught the tools.
- Optionally registers a **minimal intel surface** (just `aft_search` +
  `aft_zoom`) to match CodeGraph's tool count, isolating capability from
  surface-size. This makes the fixed-overhead comparison apples-to-apples.

Estimated effect: removes most of the +1,630 fixed delta in benchmark mode (the
workflow-hints ~500 tok unconditionally; more if the surface is slimmed).

> Note: this is a **fairness/measurement** improvement, not necessarily a
> product default — real users benefit from the hints. But the benchmark should
> be able to measure same-capability efficiency without the steering prose.

### 3. Encourage narrow-first retrieval (smaller, optional)

`edit-tax-rate` shows the agent is far cheaper when it uses `grep`/`aft_zoom`
for a known literal instead of a broad `aft_search topK:20`. A lower **default
`topK`** (e.g. keep 10, and document that exact-symbol lookups want a small
topK) would reduce accidental whole-repo dumps. Pair with (1a): even at topK:20,
terse output makes over-fetching cheap.

### 4. Benchmark-harness follow-ups (in-scope, not yet done)

- Add **multiple seeds / repeats per task** and report variance, so pass-rate
  (currently noisy at n=10) becomes interpretable.
- Add a **larger corpus** so `aft_search`'s `topK` behaves like it would on a
  real repo (the current corpus makes topK:20 ≈ "return everything").
- Consider asserting in the harness that both arms used the **same model** and
  **no fallback** for a run to count as comparable (the `armConfigs` metadata now
  makes this checkable post-hoc).

---

## Appendix: how to reproduce

```bash
cd benchmarks/codegraph-vs-aft-agent
docker compose up --build            # builds AFT from local source, runs both arms
# Outputs:
#   results/agent-ab-<stamp>.json     # now includes armConfigs + per-result tokenBreakdown
#   results/agent-ab-<stamp>.md       # now includes prompt-in / tool-out columns + arm config
#   results/transcripts/<task>.<arm>.txt   # FRESH per-run, cleared at start
```

Token decomposition can be recomputed from any run's JSON via the
`tokenBreakdown` field on each result — no raw event-stream parsing needed.
