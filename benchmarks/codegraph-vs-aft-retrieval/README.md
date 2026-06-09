# AFT vs CodeGraph retrieval benchmark (Path A)

No LLM is involved. The harness runs the same retrieval cases against two tool surfaces and scores ranked results against shared ground truth:

- AFT image: builds `target/release/aft` from this checkout and queries `semantic_search` through `BinaryBridge`.
- CodeGraph image: installs `@colbymchenry/codegraph@0.9.6`, indexes the target, and queries `codegraph query` / `codegraph context`.

Metrics: Recall, MRR, P@1/P@5/P@10, result count, and wall-clock latency around each dispatch.

## Run

```bash
# Docker, both arms
bun run bench:codegraph-vs-aft-retrieval

# Individual arms
docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm aft
docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm codegraph

# Optional pinned external target
RETRIEVAL_CORPUS=ripgrep PREPARE_TARGET=1 bun run bench:codegraph-vs-aft-retrieval
```

Native development is also supported after building AFT and installing CodeGraph on PATH:

```bash
cargo build --release -p agent-file-tools
cd benchmarks/codegraph-vs-aft-retrieval
bun run src/cli.ts --driver aft --corpus opencode-aft --codebase ../..
bun run src/cli.ts --driver codegraph --corpus opencode-aft --codebase ../..
```

## Corpora

- `corpora/opencode-aft.json` (default): symbol and context cases over this checkout.
- `corpora/ripgrep.json`: optional pinned `BurntSushi/ripgrep` corpus cloned on demand.
- `corpora/elasticsearch.json`: optional pinned `elastic/elasticsearch` corpus cloned on demand.

The CodeGraph competitor used here is the published package from <https://github.com/colbymchenry/codegraph>, pinned in the Dockerfile.

The harness is honest about limitations: AFT and CodeGraph do not expose identical ranking APIs, so the benchmark normalizes their public outputs to file/symbol ranked items and scores only shared observable quality.
