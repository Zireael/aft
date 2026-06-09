# Semantic Search Benchmark — Development & Testing Guide

> **Purpose:** How to build, configure, and test AFT's semantic search capabilities
> across multiple embedding backends and reranking configurations.
>
> **Last updated:** 2026-06-09

## Prerequisites

| Requirement | Purpose | How to verify |
|---|---|---|
| Docker | Running existing test suite | `docker --version` |
| Bun | Running benchmark scripts | `bun --version` |
| llama.cpp server | Local embedding + reranking models | `curl http://127.0.0.1:10002/v1/models` |
| GitHub account | Triggering build workflow | `gh auth status` |

**Note:** You do NOT need Rust/MSVC Build Tools installed locally. The binary is
built via GitHub Actions and downloaded as an artifact.

## Step 1: Build the Binary

### Option A — GitHub Actions (Recommended)

A manually-triggered workflow builds the binary with all feature flags enabled:

```bash
# Trigger the workflow
gh workflow run build-aft.yml --ref semantic-search-enhancement

# Wait for completion, then download the artifact
gh run list --workflow=build-aft.yml --limit 1
gh run download <run-id> -n aft-windows-x64 -D ./aft-build
```

The binary lands at `./aft-build/aft.exe`.

### Option B — Local build (requires MSVC Build Tools)

If you have `link.exe` (Visual Studio Build Tools with C++ workload):

```bash
cargo build --release --target x86_64-pc-windows-msvc -p agent-file-tools \
  --features semantic-model2vec,semantic-fts5
# Output: target/x86_64-pc-windows-msvc/release/aft.exe
```

### Option C — Docker (Linux binary only, not usable as Windows plugin)

Docker on this system runs Linux containers via WSL2. The `Dockerfile.build-linux`
produces a Linux ELF binary. This is useful for running the test suite but NOT for
the OpenCode plugin on Windows.

```bash
docker build -t aft-build -f tests/docker/Dockerfile.build-linux .
docker cp $(docker create aft-build):/build/target/release/aft ./aft-linux
```

## Step 2: Install the Binary

The AFT bridge resolver checks these locations in order:
1. Versioned cache: `%LOCALAPPDATA%/aft/bin/v<version>/aft.exe`
2. npm platform package
3. PATH
4. `~/.cargo/bin/aft.exe`
5. Auto-download from GitHub releases

**To use your custom build**, copy it to the versioned cache:

```bash
# Create the cache directory (version must match crate version)
mkdir -p "$LOCALAPPDATA/aft/bin/v0.29.1"
cp ./aft-build/aft.exe "$LOCALAPPDATA/aft/bin/v0.29.1/aft.exe"

# Verify
"$LOCALAPPDATA/aft/bin/v0.29.1/aft.exe" --version
```

## Step 3: Configure OpenCode Plugin

The plugin is already declared in `opencode.jsonc`. Verify it's present:

```jsonc
// In ~/.config/opencode/opencode.jsonc — plugin array should include:
"@cortexkit/aft-opencode@latest"
```

The TUI component is declared in `tui.jsonc`:

```jsonc
// In ~/.config/opencode/tui.jsonc — plugin array should include:
"@cortexkit/aft-opencode@latest"
```

**Restart OpenCode** after changing the binary to pick up the new version.

## Step 4: Configure Semantic Search Backends

Edit `~/.config/opencode/aft.jsonc` to switch between backends. Only one backend
is active at a time. Restart OpenCode after switching.

### 4a. Legacy ONNX (fastembed) — Default

```jsonc
{
  "semantic_search": true,
  "semantic": {
    "backend": "fastembed"
    // model defaults to all-MiniLM-L6-v2 (384 dims)
    // ONNX Runtime auto-downloaded on first use
  }
}
```

### 4b. model2vec Potion Code 16M

Requires binary built with `--features semantic-model2vec`.

```bash
# Download the model (one-time)
mkdir -p ~/models/potion-code-16M
cd ~/models/potion-code-16M
curl -LO https://huggingface.co/minishlab/potion-code-16M/resolve/main/config.json
curl -LO https://huggingface.co/minishlab/potion-code-16M/resolve/main/tokenizer.json
curl -LO https://huggingface.co/minishlab/potion-code-16M/resolve/main/model.safetensors
```

```jsonc
{
  "semantic_search": true,
  "semantic": {
    "backend": "model2vec",
    "model_path": "C:/Users/zir/models/potion-code-16M"
    // 256 dims, ~65MB, pure CPU, no ONNX runtime
  }
}
```

### 4c. OASIS Endpoint (llama.cpp)

```jsonc
{
  "semantic_search": true,
  "semantic": {
    "backend": "openai_compatible",
    "model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "base_url": "http://127.0.0.1:10002/v1",
    "max_batch_size": 16,
    "timeout_ms": 60000
  }
}
```

### 4d. Any Backend + Reranking

Add reranking fields to any backend configuration:

```jsonc
{
  "semantic_search": true,
  "semantic": {
    "backend": "openai_compatible",  // or fastembed, model2vec, etc.
    "model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "base_url": "http://127.0.0.1:10002/v1",
    "rerank_enabled": true,
    "rerank_model": "CodeRankLLM.Q4_K_M",
    "rerank_base_url": "http://127.0.0.1:10002/v1"
  }
}
```

## Step 5: Capture Benchmark Metrics

### Method A: Diagnostics Mode (per-query detail)

Add to `aft.jsonc`:

```jsonc
{
  "semantic": {
    "diagnostics_enabled": true,
    "low_confidence_threshold": 0.3
  }
}
```

Then run `aft_search` — results include per-query latency, score distribution,
and confidence metrics.

### Method B: Semble Benchmark Suite (structured comparison)

```bash
# Sync the pilot corpus (5 repos, 50 queries)
cd D:/Coding/_tools/aft-src
bun run benchmarks/semble/corpus.ts sync --pilot

# Run the baseline (ripgrep lexical)
bun run benchmarks/semble/baseline-rg.ts --pilot --k 10

# Run multi-mode pilot (requires aft binary in PATH or cache)
bun run benchmarks/semble/pilot.ts --cache-dir .bench-cache --k 10

# Compare against baseline
bun run benchmarks/semble/ci.ts --baseline baseline.json --current pilot-report.json
```

### Method C: Quick Manual Test

```bash
# Start llama.cpp with both models
& "D:\Program Files\llama.cpp\start-llama.ps1"

# Use aft_search via OpenCode to test queries
# The tool returns recall@k, latency, and score distribution
```

## Step 6: Run Semble Benchmark

Semble is available as an MCP tool:

```bash
# Via MCP (in OpenCode)
# Use mcp__semble__search with semantic queries

# Via CLI
uvx --from "semble[mcp]" semble search "function that handles HTTP routing" ./your-project
```

### Semble Benchmark Scripts

| Script | Purpose | Command |
|---|---|---|
| `corpus.ts` | Clone/cache benchmark repos | `bun run benchmarks/semble/corpus.ts sync --pilot` |
| `import.ts` | Import Semble annotations | `bun run benchmarks/semble/import.ts --pilot` |
| `baseline-rg.ts` | Ripgrep lexical baseline | `bun run benchmarks/semble/baseline-rg.ts --pilot` |
| `pilot.ts` | Multi-mode pilot runner | `bun run benchmarks/semble/pilot.ts --k 10` |
| `speed.ts` | Cold-start + query latency | `bun run benchmarks/semble/speed.ts --pilot` |
| `token-efficiency.ts` | Recall@token_budget | `bun run benchmarks/semble/token-efficiency.ts --pilot` |
| `ci.ts` | Regression detection | `bun run benchmarks/semble/ci.ts --baseline b.json --current c.json` |

## Step 7: Run colgrep Benchmark

```bash
# Basic semantic search
colgrep "authentication middleware" --results 10

# With file type filter
colgrep "error handling" --include="*.rs" --results 20

# Hybrid text + semantic
colgrep -e "async fn" "concurrent request handling" --results 15

# Pattern-only (no semantic)
colgrep -e "TODO|FIXME" --include="*.ts" --results 50
```

## Step 8: Couple with Reranking

After configuring reranking in `aft.jsonc` (see Step 4d), the `aft_search` tool
automatically applies reranking to search results. Compare:

1. **Without reranking:** Run `aft_search` with a query → note results
2. **With reranking:** Add `rerank_enabled: true`, restart, run same query → compare

Reranking adds ~500-1500ms per query (LLM call). The reranker re-scores the top
candidates from the initial search to improve precision.

## Test Matrix

| Backend | Reranking | Semble | colgrep | Metrics |
|---|---|---|---|---|
| fastembed (ONNX) | ❌ | ✅ | ✅ | recall@10, MRR, latency |
| fastembed + rerank | ✅ | ✅ | ✅ | recall@10, MRR, latency, rerank delta |
| model2vec Potion | ❌ | ✅ | ✅ | recall@10, MRR, latency |
| model2vec + rerank | ✅ | ✅ | ✅ | recall@10, MRR, latency, rerank delta |
| OASIS endpoint | ❌ | ✅ | ✅ | recall@10, MRR, latency |
| OASIS + rerank | ✅ | ✅ | ✅ | recall@10, MRR, latency, rerank delta |

## Troubleshooting

### "Could not find the `aft` binary"

- Check cache: `ls "$LOCALAPPDATA/aft/bin/"`
- Check PATH: `where aft`
- The resolver logs the attempted sources — check OpenCode logs

### "compiled without semantic-model2vec feature"

- Rebuild with `--features semantic-model2vec`
- Copy the new binary to the cache directory

### Semantic search returns empty results

- Check backend configuration in `aft.jsonc`
- Verify llama.cpp is running: `curl http://127.0.0.1:10002/v1/models`
- Check OpenCode logs for AFT errors

### Index rebuild takes too long

- First launch with a new backend triggers a full rebuild (~2s per 1K files)
- Subsequent sessions use the cached index
- Check status via `/aft-status` command

## File Locations

| File | Purpose |
|---|---|
| `~/.config/opencode/opencode.jsonc` | Plugin declaration |
| `~/.config/opencode/tui.jsonc` | TUI plugin declaration |
| `~/.config/opencode/aft.jsonc` | AFT config (semantic backend, tools) |
| `%LOCALAPPDATA%/aft/bin/v<ver>/aft.exe` | Cached binary |
| `D:/Coding/_tools/aft-src/benchmarks/semble/` | Semble benchmark suite |
| `D:/Coding/_tools/aft-src/crates/aft/src/FTS5-SPIKE.md` | FTS5 comparison spike |
| `D:/Coding/_tools/aft-src/benchmarks/semble/FULL-CORPUS-CI-SPIKE.md` | Full corpus CI spike |
