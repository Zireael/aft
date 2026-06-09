# AFT vs CodeGraph agent benchmark (Path B)

This benchmark runs OpenCode CLI against the same deterministic task set with two tool surfaces:

- `aft`: AFT OpenCode plugin enabled.
- `codegraph`: CodeGraph MCP server enabled (`codegraph serve --mcp`).

The LLM is `opencode-go/deepseek-v4-flash-free` through `https://opencode.ai/zen/v1`. If a run fails due to rate limiting, the harness retries that task with `opencode-go/deepseek-v4-pro`. API keys are read from `OPENCODE_API_KEY`, `OPENAI_API_KEY`, or a mounted `~/.local/share/opencode/auth.json`; keys are never written to images or reports.

## Run

```bash
# Real LLM run (recommended to start small)
OPENCODE_API_KEY=... AGENT_TASK_LIMIT=3 bun run bench:codegraph-vs-aft-agent

# Harness-only Docker smoke/sample without LLM calls
AGENT_DRY_RUN=1 AGENT_TASK_LIMIT=2 bun run bench:codegraph-vs-aft-agent
```

Outputs include JSON and Markdown reports under `results/`. Each task records success, deterministic check results, token usage when OpenCode emits it, a per-task `tokenBreakdown` (prompt-input vs tool-output vs steps), wall time, tool-call count, the run directory, and stdout/stderr artifacts.

The JSON report also records `armConfigs` (top-level and echoed into `metadata`): the resolved per-arm setup — intelligence layer, AFT config / CodeGraph MCP wiring, shared native built-in tools, and pre-warm command — so two runs can be confirmed to use the same configuration before comparing them.

Fresh human-readable transcripts are written to `results/transcripts/<task>.<arm>.txt` on every run. The transcripts directory is **cleared at the start of each run**, so stale artifacts from a previous run can never be mistaken for fresh data.

See `FINDINGS.md` for a token-gap analysis (AFT vs CodeGraph), fairness assessment, and recommendations.

## Task design

`corpora/tasks.json` defines 10 tasks over `corpora/agent-fixture/`:

- answer-only code discovery tasks (find symbols, routes, and callers), scored by final answer substrings;
- edit tasks (change constants/type literals), scored by file post-conditions and `npm test`.

The fixture is intentionally small so the benchmark can run cheaply while still exercising the tool-selection patterns AFT users care about: search, navigation, focused reading, edits, and verification. Larger real-repo task sets can be added as additional corpora without changing the harness.

## Caveats

- This is an agent benchmark, so variance is expected. Use multiple runs before drawing conclusions.
- `AGENT_DRY_RUN=1` exists only for Docker/harness smoke tests and sample output shape; it is not a performance or quality measurement.
- Built-in OpenCode tools remain available in both arms; the variable under test is the added AFT plugin vs CodeGraph MCP surface and their instructions.
