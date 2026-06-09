#!/usr/bin/env bun
import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { loadCorpus } from "./corpus";
import { createDriver } from "./drivers";
import { printReport, writeReports } from "./reporter";
import { scoreCase, summarize } from "./scoring";
import type { RetrievalDriverName, RetrievalReport } from "./types";
import { ensureGitTarget, gitRev, HARNESS_DIR, REPO_ROOT } from "./util";

interface CliOptions {
  driver: RetrievalDriverName;
  corpus: string;
  target?: string;
  codebasePath?: string;
  aftBinary: string;
  outDir: string;
  topK: number;
  timeoutMs: number;
  prepareTarget: boolean;
}

async function main(): Promise<void> {
  const options = parseCliArgs();
  const { path: corpusPath, corpus } = loadCorpus(options.corpus);
  const codebasePath = await resolveCodebase(options, corpus);
  const driver = createDriver(options.driver, {
    codebasePath,
    topK: options.topK,
    timeoutMs: options.timeoutMs,
    aftBinary: options.aftBinary,
  });

  const results = [];
  try {
    await driver.prepare?.();
    for (const testCase of corpus.cases) {
      const start = performance.now();
      try {
        const run = await driver.run(testCase);
        results.push(scoreCase(testCase, driver.name, run.items, performance.now() - start, codebasePath));
      } catch (err) {
        const error = err instanceof Error ? err.message : String(err);
        results.push(scoreCase(testCase, driver.name, [], performance.now() - start, codebasePath, error));
      }
    }
  } finally {
    await driver.close?.();
  }

  const report: RetrievalReport = {
    benchmark: "codegraph-vs-aft-retrieval",
    driver: driver.name,
    timestamp: new Date().toISOString(),
    corpus: corpus.name || options.corpus,
    corpusPath,
    codebasePath,
    targetSha: gitRev(codebasePath),
    topK: options.topK,
    summary: summarize(results),
    results,
    metadata: {
      aftBinary: options.aftBinary,
      target: corpus.target,
      methodology:
        "Identical retrieval cases scored by ground-truth symbol/file matches with Recall, MRR, and Precision@k. No LLM is used.",
    },
  };
  printReport(report);
  const written = writeReports(report, options.outDir);
  console.log(`\nJSON report: ${written.jsonPath}`);
  console.log(`Markdown summary: ${written.markdownPath}`);
}

async function resolveCodebase(options: CliOptions, corpus: { target?: { name: string; kind: "local" | "git"; path?: string; url?: string; commit?: string } }): Promise<string> {
  if (options.codebasePath) return resolve(options.codebasePath);
  const envCodebase = process.env.CODEBASE_PATH;
  if (envCodebase) return resolve(envCodebase);
  const target = corpus.target;
  if (target?.kind === "git") {
    if (!target.url || !target.commit) throw new Error(`Git corpus target ${target.name} missing url/commit`);
    if (!options.prepareTarget) {
      throw new Error(`Corpus ${corpus.target?.name} is a git target; pass --prepare-target or CODEBASE_PATH`);
    }
    return ensureGitTarget(target.name, target.url, target.commit);
  }
  if (target?.path) return target.path;
  return process.env.IN_DOCKER === "1" ? "/workspace" : REPO_ROOT;
}

function parseCliArgs(): CliOptions {
  const { values } = parseArgs({
    options: {
      driver: { type: "string", default: process.env.RETRIEVAL_DRIVER ?? "aft" },
      corpus: { type: "string", default: process.env.RETRIEVAL_CORPUS ?? "opencode-aft" },
      target: { type: "string", default: process.env.TARGET },
      codebase: { type: "string", default: process.env.CODEBASE_PATH },
      binary: { type: "string", default: process.env.AFT_BINARY ?? `${REPO_ROOT}/target/release/aft` },
      "out-dir": { type: "string", default: process.env.RETRIEVAL_OUT_DIR ?? `${HARNESS_DIR}/results` },
      topK: { type: "string", default: process.env.TOP_K ?? "10" },
      "timeout-ms": { type: "string", default: process.env.RETRIEVAL_TIMEOUT_MS ?? "600000" },
      "prepare-target": { type: "boolean", default: process.env.PREPARE_TARGET === "1" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
  });

  if (values.help) {
    console.log(`
AFT vs CodeGraph retrieval benchmark (no LLM)

Usage:
  bun run src/cli.ts --driver aft --corpus opencode-aft --codebase /path/to/aft
  bun run src/cli.ts --driver codegraph --corpus opencode-aft --codebase /path/to/aft
  bun run src/cli.ts --driver aft --corpus ripgrep --prepare-target

Docker:
  docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm aft
  docker compose -f benchmarks/codegraph-vs-aft-retrieval/docker-compose.yml run --rm codegraph
`);
    process.exit(0);
  }

  const driver = String(values.driver) as RetrievalDriverName;
  if (!["aft", "codegraph"].includes(driver)) throw new Error(`Invalid --driver: ${values.driver}`);
  return {
    driver,
    corpus: String(values.target ?? values.corpus),
    target: values.target ? String(values.target) : undefined,
    codebasePath: values.codebase ? String(values.codebase) : undefined,
    aftBinary: resolve(String(values.binary)),
    outDir: String(values["out-dir"]),
    topK: positiveInt(String(values.topK), "--topK"),
    timeoutMs: positiveInt(String(values["timeout-ms"]), "--timeout-ms"),
    prepareTarget: values["prepare-target"] === true,
  };
}

function positiveInt(raw: string, name: string): number {
  const value = Number.parseInt(raw, 10);
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}

main().catch((err) => {
  console.error(err instanceof Error ? err.stack ?? err.message : err);
  process.exit(1);
});
