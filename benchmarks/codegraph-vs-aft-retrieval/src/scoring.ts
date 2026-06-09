import type { CaseResult, GroundTruth, RankedResult, RetrievalCase, RetrievalSummary } from "./types";
import { mean, normalizePath, percentile, round } from "./util";

const PASS_RECALL = 0.5;

export function scoreCase(
  testCase: RetrievalCase,
  driver: "aft" | "codegraph",
  items: RankedResult[],
  latencyMs: number,
  codebasePath: string,
  error?: string,
): CaseResult {
  const normalized = items.map((item, index) => ({
    ...item,
    rank: item.rank || index + 1,
    file: normalizePath(item.file, codebasePath),
  }));
  const found = testCase.groundTruth.filter((truth) =>
    normalized.some((item) => itemMatchesTruth(item, truth)),
  );
  const missed = testCase.groundTruth.filter((truth) => !found.includes(truth));
  const recall = testCase.groundTruth.length > 0 ? found.length / testCase.groundTruth.length : 0;
  const firstRelevantRank = normalized.find((item) =>
    testCase.groundTruth.some((truth) => itemMatchesTruth(item, truth)),
  )?.rank ?? 0;
  return {
    caseId: testCase.id,
    query: testCase.query,
    mode: testCase.mode,
    driver,
    pass: !error && recall >= PASS_RECALL,
    recall: round(recall),
    mrr: round(firstRelevantRank > 0 ? 1 / firstRelevantRank : 0),
    precisionAt1: round(precisionAt(normalized, testCase.groundTruth, 1)),
    precisionAt5: round(precisionAt(normalized, testCase.groundTruth, 5)),
    precisionAt10: round(precisionAt(normalized, testCase.groundTruth, 10)),
    found,
    missed,
    latencyMs: round(latencyMs, 3),
    resultCount: normalized.length,
    results: normalized,
    error,
  };
}

export function summarize(results: CaseResult[]): RetrievalSummary {
  const latencies = results.map((result) => result.latencyMs);
  const passed = results.filter((result) => result.pass).length;
  return {
    cases: results.length,
    passed,
    failed: results.length - passed,
    meanRecall: mean(results.map((result) => result.recall)),
    meanMRR: mean(results.map((result) => result.mrr)),
    meanPrecisionAt1: mean(results.map((result) => result.precisionAt1)),
    meanPrecisionAt5: mean(results.map((result) => result.precisionAt5)),
    meanPrecisionAt10: mean(results.map((result) => result.precisionAt10)),
    latencyMsP50: percentile(latencies, 50),
    latencyMsP95: percentile(latencies, 95),
  };
}

function precisionAt(items: RankedResult[], truths: GroundTruth[], k: number): number {
  if (k <= 0) return 0;
  const relevant = items.slice(0, k).filter((item) => truths.some((truth) => itemMatchesTruth(item, truth))).length;
  return relevant / k;
}

function itemMatchesTruth(item: RankedResult, truth: GroundTruth): boolean {
  const symbolMatch = truth.symbol ? itemMatchesSymbol(item, truth.symbol) : false;
  const fileMatch = itemMatchesFile(item, truth.file);
  if (symbolMatch && (!truth.file || fileMatch || !item.file)) return true;
  if (!fileMatch) return false;
  if (truth.startLine && item.startLine) return rangesOverlap(item.startLine, item.endLine ?? item.startLine, truth.startLine, truth.endLine ?? truth.startLine);
  return true;
}

function itemMatchesSymbol(item: RankedResult, symbol: string): boolean {
  const needle = symbol.toLowerCase();
  if ((item.symbol ?? "").toLowerCase() === needle) return true;
  const haystack = [item.symbol, item.kind, item.text].filter(Boolean).join("\n").toLowerCase();
  return haystack.includes(needle);
}

function itemMatchesFile(item: RankedResult, expectedFile: string): boolean {
  if (!item.file) return false;
  const actual = item.file.replace(/\\/g, "/").replace(/^\.\//, "");
  const expected = expectedFile.replace(/\\/g, "/").replace(/^\.\//, "");
  return actual === expected || actual.endsWith(`/${expected}`);
}

function rangesOverlap(aStart: number, aEnd: number, bStart: number, bEnd: number): boolean {
  return aStart <= bEnd && bStart <= aEnd;
}
