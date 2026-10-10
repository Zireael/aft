# aft_inspect field-audit harness

This harness measures `aft_inspect` correctness against real repositories. It is
intended to be rerun after each fix wave and keeps cloned repositories and raw
results outside this repository.

## Data root

Use the task-standard workdir:

```bash
export AFT_AUDIT_ROOT="$HOME/Work/OSS/AFT_TESTS"
```

The harness writes:

- `$AFT_AUDIT_ROOT/repos/<slug>/` — shallow clones
- `$AFT_AUDIT_ROOT/storage/<slug>/` — AFT cache/storage, isolated per repo
- `$AFT_AUDIT_ROOT/_results/<slug>/inspect.json` — raw NDJSON response for the
  full `aft_inspect` call
- `$AFT_AUDIT_ROOT/_results/<slug>/summary.json` — runtime, counts, and status
- `$AFT_AUDIT_ROOT/_results/REPORT.md` — human-authored consolidated report

Do **not** commit `AFT_TESTS`, `repos`, `storage`, `_results`, raw inspect JSON,
or cloned repositories. The local `.gitignore` also ignores those names in case a
rerun is pointed at this directory by mistake.

## Build AFT once

From the AFT repository root:

```bash
cargo build --release
```

Then run the audit sequentially:

```bash
python3 benchmarks/inspect-field-audit/run_audit.py \
  --aft-bin ./target/release/aft \
  --matrix benchmarks/inspect-field-audit/repos.json \
  --workdir "$AFT_AUDIT_ROOT"
```

The runner processes one repository at a time. `aft warmup` can be CPU-heavy, so
parallelizing repos contaminates timings and can starve the machine.

## Semantic indexing is disabled

For every cloned repository the runner writes `.cortexkit/aft.jsonc` with:

```jsonc
{
  "indexes": {
    "semantic": false,
    "trigram": false,
    "callgraph": true
  },
  "inspect": { "enabled": true }
}
```

The index settings disable semantic/embedding and trigram indexing while keeping
the callgraph available; embeddings are irrelevant to inspect correctness.

## NDJSON protocol note

The standalone AFT binary exposes plugin tools through a bare subc tool name.
The OpenCode/Pi user-facing tool is `aft_inspect`, and its bare core name is
`inspect`. The runner therefore sends:

```json
{"id":"inspect-1","command":"tool_call","name":"inspect","arguments":{"sections":"all","topK":100}}
```

This exercises the same `tool_call` translation/formatting path that the plugin
uses for `aft_inspect`.

## Verification workflow

After `run_audit.py`, create sample candidate files:

```bash
python3 benchmarks/inspect-field-audit/verify_samples.py \
  --workdir "$AFT_AUDIT_ROOT" \
  --repo dub
```

`verify_samples.py` deliberately defaults verdicts to `UNCERTAIN`. It extracts
sampled findings, searches the clone for candidate references, and writes
`$AFT_AUDIT_ROOT/_results/<slug>/verify.json`. A human auditor must inspect the
candidate references and set each sample verdict to `TP`, `FP`, or `UNCERTAIN`,
recording the exact missed `file:line` and code form for every false positive.

Generate a report skeleton after samples are reviewed:

```bash
python3 benchmarks/inspect-field-audit/report_from_results.py \
  --workdir "$AFT_AUDIT_ROOT" \
  --matrix benchmarks/inspect-field-audit/repos.json \
  --output "$AFT_AUDIT_ROOT/_results/REPORT.md"
```

The report generator preserves an existing `REPORT.md` by default. Pass
`--force` to overwrite a generated skeleton.

## Optional ground-truth tools

The harness records availability for cross-check tools but does not install them
for you:

- TS/JS: `knip`, `fallow`
- Rust: `cargo check`/rustc private `dead_code` warnings
- Go: `go vet`, `staticcheck`

Record unavailable or skipped tools in the per-repo `summary.json` / report;
do not silently treat missing ground truth as agreement.
