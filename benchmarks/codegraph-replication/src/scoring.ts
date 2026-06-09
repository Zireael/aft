import type { EvalResult, EvalTestCase, PrecisionAtK, RankedResult } from "./types";
import { median, normalizePath, percentile, roundMetric, unique } from "./util";

export const PASS_THRESHOLD = 0.5;

export function scoreCase(
  testCase: EvalTestCase,
  driver: string,
  items: RankedResult[],
  latencySamplesMs: number[],
  codebasePath: string,
  error?: string,
): EvalResult {
  const normalizedItems = items.map((item, index) => ({
    ...item,
    rank: item.rank || index + 1,
    file: normalizePath(item.file, codebasePath),
  }));

  if (testCase.skip) {
    return skippedResult(testCase, driver, normalizedItems, latencySamplesMs, testCase.skipReason);
  }

  const expectedSymbols = testCase.expectedSymbols;
  const expectedFiles = unique([
    ...(testCase.expectedFiles ?? []),
    ...(testCase.groundTruth ?? []).map((g) => g.file),
  ]);
  const foundSymbols = expectedSymbols.filter((symbol) =>
    normalizedItems.some((item) => itemMatchesSymbol(item, symbol)),
  );
  const missedSymbols = expectedSymbols.filter((symbol) => !foundSymbols.includes(symbol));
  const foundFiles = expectedFiles.filter((file) =>
    normalizedItems.some((item) => itemMatchesFile(item, file)),
  );
  const missedFiles = expectedFiles.filter((file) => !foundFiles.includes(file));

  const recallDenominator =
    expectedSymbols.length > 0 ? expectedSymbols.length : expectedFiles.length;
  const recallNumerator = expectedSymbols.length > 0 ? foundSymbols.length : foundFiles.length;
  const recall = recallDenominator > 0 ? recallNumerator / recallDenominator : 0;
  const firstRelevantRank =
    normalizedItems.find((item) => itemIsRelevant(item, testCase))?.rank ?? 0;
  const precisionAtK = {
    p1: precisionAt(normalizedItems, testCase, 1),
    p5: precisionAt(normalizedItems, testCase, 5),
    p10: precisionAt(normalizedItems, testCase, 10),
  };
  const mrr = firstRelevantRank > 0 ? 1 / firstRelevantRank : 0;
  const latencies = latencySamplesMs.length > 0 ? latencySamplesMs : [0];

  return {
    caseId: testCase.id,
    api: testCase.api,
    driver,
    pass: !error && recall >= PASS_THRESHOLD,
    recall: roundMetric(recall),
    mrr: roundMetric(mrr),
    precisionAtK: roundPrecision(precisionAtK),
    foundSymbols,
    missedSymbols,
    foundFiles,
    missedFiles,
    latencyMs: roundMetric(latencies[latencies.length - 1] ?? 0, 3),
    latencySamplesMs: latencies.map((value) => roundMetric(value, 3)),
    latencyMsMedian: median(latencies),
    latencyMsP95: percentile(latencies, 95),
    resultCount: normalizedItems.length,
    results: normalizedItems,
    error,
  };
}

export function aggregateResults(results: EvalResult[]) {
  const evaluated = results.filter((result) => !result.skipReason);
  const skipped = results.length - evaluated.length;
  const latencies = evaluated.flatMap((result) => result.latencySamplesMs);
  const total = evaluated.length;
  const passed = evaluated.filter((result) => result.pass).length;
  const failed = total - passed;

  return {
    total,
    passed,
    failed,
    skipped,
    meanRecall: meanMetric(evaluated.map((result) => result.recall)),
    meanMRR: meanMetric(evaluated.map((result) => result.mrr)),
    meanPrecisionAt1: meanMetric(evaluated.map((result) => result.precisionAtK.p1)),
    meanPrecisionAt5: meanMetric(evaluated.map((result) => result.precisionAtK.p5)),
    meanPrecisionAt10: meanMetric(evaluated.map((result) => result.precisionAtK.p10)),
    latencyMsMedian: median(latencies),
    latencyMsP95: percentile(latencies, 95),
  };
}

function skippedResult(
  testCase: EvalTestCase,
  driver: string,
  items: RankedResult[],
  latencySamplesMs: number[],
  skipReason: string | undefined,
): EvalResult {
  const latencies = latencySamplesMs.length > 0 ? latencySamplesMs : [0];
  return {
    caseId: testCase.id,
    api: testCase.api,
    driver,
    pass: false,
    recall: 0,
    mrr: 0,
    precisionAtK: { p1: 0, p5: 0, p10: 0 },
    foundSymbols: [],
    missedSymbols: testCase.expectedSymbols,
    foundFiles: [],
    missedFiles: testCase.expectedFiles ?? [],
    latencyMs: roundMetric(latencies[latencies.length - 1] ?? 0, 3),
    latencySamplesMs: latencies.map((value) => roundMetric(value, 3)),
    latencyMsMedian: median(latencies),
    latencyMsP95: percentile(latencies, 95),
    resultCount: items.length,
    results: items,
    skipReason: skipReason ?? "skipped by corpus",
  };
}

function precisionAt(items: RankedResult[], testCase: EvalTestCase, k: number): number {
  if (k <= 0) return 0;
  const relevant = items.slice(0, k).filter((item) => itemIsRelevant(item, testCase)).length;
  return relevant / k;
}

function roundPrecision(value: PrecisionAtK): PrecisionAtK {
  return {
    p1: roundMetric(value.p1),
    p5: roundMetric(value.p5),
    p10: roundMetric(value.p10),
  };
}

function meanMetric(values: number[]): number {
  if (values.length === 0) return 0;
  return roundMetric(values.reduce((sum, value) => sum + value, 0) / values.length);
}

function itemIsRelevant(item: RankedResult, testCase: EvalTestCase): boolean {
  if (testCase.expectedSymbols.some((symbol) => itemMatchesSymbol(item, symbol))) return true;
  const expectedFiles = testCase.expectedFiles ?? [];
  return expectedFiles.some((file) => itemMatchesFile(item, file));
}

function itemMatchesSymbol(item: RankedResult, symbol: string): boolean {
  const needle = symbol.toLowerCase();
  if ((item.name ?? "").toLowerCase() === needle) return true;
  const text = [item.name, item.kind, item.text].filter(Boolean).join("\n").toLowerCase();
  return text.includes(needle);
}

function itemMatchesFile(item: RankedResult, expectedFile: string): boolean {
  if (!item.file) return false;
  const actual = item.file.replace(/\\/g, "/").replace(/^\.\//, "");
  const expected = expectedFile.replace(/\\/g, "/").replace(/^\.\//, "");
  return actual === expected || actual.endsWith(`/${expected}`);
}
