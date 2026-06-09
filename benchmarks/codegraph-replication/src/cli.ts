#!/usr/bin/env bun
import { existsSync } from "node:fs";
import { parseArgs } from "node:util";
import { loadCorpus } from "./corpus";
import { createDriver } from "./drivers";
import { markdownSummary, printReport, writeReports } from "./reporter";
import { aggregateResults, scoreCase } from "./scoring";
import type { CliOptions, DriverName, EvalReport, EvalResult, RankedResult } from "./types";
import { gitRev, HARNESS_DIR, REPO_ROOT } from "./util";

const DEFAULT_CODEGRAPH_REPO = "/Users/ufukaltinok/Work/OSS/codegraph";

async function main(): Promise<void> {
  const options = parseCliArgs();
  const corpus = loadCorpus(options.corpus);
  const driver = createDriver(options.driver, {
    codebasePath: options.codebasePath,
    binaryPath: options.binaryPath,
    topK: options.topK,
    readyTimeoutMs: options.readyTimeoutMs,
    verbose: options.verbose,
  });

  const cases = options.includeSkipped
    ? corpus.cases
    : corpus.cases.filter((testCase) => !testCase.skip);
  const results: EvalResult[] = [];

  try {
    await driver.prepare?.();
    for (const testCase of cases) {
      const samples: number[] = [];
      let items: RankedResult[] = [];
      let error: string | undefined;

      for (let run = 0; run < options.runs; run++) {
        const start = performance.now();
        try {
          const runResult = testCase.skip ? { items: [] } : await driver.run(testCase);
          const latencyMs = performance.now() - start;
          samples.push(latencyMs);
          items = runResult.items;
        } catch (err) {
          const latencyMs = performance.now() - start;
          samples.push(latencyMs);
          error = err instanceof Error ? err.message : String(err);
          if (options.verbose) console.error(`${testCase.id}: ${error}`);
          break;
        }
      }

      results.push(scoreCase(testCase, driver.name, items, samples, options.codebasePath, error));
    }
  } finally {
    await driver.close?.();
  }

  const report = buildReport(options, corpus.path, driver.name, results);
  printReport(report);
  const written = writeReports(report, options.outDir);
  console.log(`\nJSON report: ${written.jsonPath}`);
  console.log(`Markdown summary: ${written.markdownPath}`);

  if (options.verbose) {
    console.log("\nMarkdown summary:\n");
    console.log(markdownSummary(report));
  }
}

function buildReport(
  options: CliOptions,
  corpusPath: string,
  driverName: string,
  results: EvalResult[],
): EvalReport {
  const codegraphSha = existsSync(DEFAULT_CODEGRAPH_REPO)
    ? gitRev(DEFAULT_CODEGRAPH_REPO)
    : "unknown";
  return {
    timestamp: new Date().toISOString(),
    benchmark: "codegraph-replication",
    codebasePath: options.codebasePath,
    codegraphSha,
    aftSha: gitRev(options.codebasePath),
    driver: driverName,
    corpus: options.corpus,
    corpusPath,
    topK: options.topK,
    runs: options.runs,
    summary: aggregateResults(results),
    results,
    metadata: {
      binaryPath: options.binaryPath,
      latencyNote:
        "latencySamplesMs are wall-clock measurements around each actual driver dispatch",
      harnessDir: HARNESS_DIR,
      methodology:
        "AFT replication of codegraph/__tests__/evaluation structured retrieval-quality eval",
    },
  };
}

function parseCliArgs(): CliOptions {
  const { values } = parseArgs({
    options: {
      driver: { type: "string", default: "aft" },
      corpus: { type: "string", default: "codegraph" },
      codebase: { type: "string", default: REPO_ROOT },
      binary: { type: "string", default: `${REPO_ROOT}/target/release/aft` },
      "out-dir": { type: "string", default: `${HARNESS_DIR}/results` },
      topK: { type: "string", default: "10" },
      runs: { type: "string", default: "1" },
      "ready-timeout-ms": { type: "string", default: "600000" },
      "include-skipped": { type: "boolean", default: true },
      verbose: { type: "boolean", short: "v" },
      help: { type: "boolean", short: "h" },
    },
    strict: true,
  });

  if (values.help) {
    console.log(`
CodeGraph replication benchmark for AFT

Usage:
  bun run bench:codegraph-replication --driver aft --corpus codegraph
  bun run bench:codegraph-replication --driver ripgrep --corpus codegraph --codebase /path/to/repo

Drivers:
  aft          AFT hybrid semantic+lexical search via BinaryBridge semantic_search
  aft-grep     AFT indexed grep via BinaryBridge grep
  ripgrep      rg -F lexical baseline
  list-files   naive file listing sanity baseline

Corpora:
  codegraph            AFT translation of CodeGraph's 12 structured eval shapes
  codegraph-original   Exact CodeGraph upstream structured corpus, for compatible codebases
  aft                  Supplemental AFT tool-surface cases
  /path/to/file.json   Custom corpus using the same schema
`);
    process.exit(0);
  }

  const driver = String(values.driver) as DriverName;
  if (!["aft", "aft-grep", "ripgrep", "list-files"].includes(driver)) {
    throw new Error(`Invalid --driver: ${values.driver}`);
  }

  const topK = positiveInt(String(values.topK), "--topK");
  const runs = positiveInt(String(values.runs), "--runs");
  const readyTimeoutMs = positiveInt(String(values["ready-timeout-ms"]), "--ready-timeout-ms");

  return {
    driver,
    corpus: String(values.corpus),
    codebasePath: String(values.codebase),
    binaryPath: String(values.binary),
    outDir: String(values["out-dir"]),
    topK,
    runs,
    readyTimeoutMs,
    includeSkipped: values["include-skipped"] !== false,
    verbose: values.verbose === true,
  };
}

function positiveInt(raw: string, name: string): number {
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
