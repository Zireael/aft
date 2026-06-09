import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { EvalReport, EvalResult } from "./types";

export function printReport(report: EvalReport): void {
  const { summary } = report;
  console.log(`\nCodeGraph replication benchmark — ${report.driver}`);
  console.log(`  corpus: ${report.corpus}`);
  console.log(`  codebase: ${report.codebasePath}`);
  console.log(
    `  summary: ${summary.passed}/${summary.total} passed (${summary.skipped} skipped) | recall=${summary.meanRecall.toFixed(
      3,
    )} | mrr=${summary.meanMRR.toFixed(3)} | p95=${summary.latencyMsP95.toFixed(1)}ms`,
  );
  console.log("\nCase results");
  for (const result of report.results) {
    if (result.skipReason) {
      console.log(`  SKIP ${result.caseId} — ${result.skipReason}`);
      continue;
    }
    const status = result.pass ? "PASS" : "FAIL";
    const missed =
      result.missedSymbols.length > 0 ? ` missed=${result.missedSymbols.join(",")}` : "";
    console.log(
      `  ${status} ${result.caseId} recall=${result.recall.toFixed(2)} mrr=${result.mrr.toFixed(
        2,
      )} p@5=${result.precisionAtK.p5.toFixed(2)} ${result.latencyMs.toFixed(1)}ms${missed}`,
    );
  }
}

export function writeReports(
  report: EvalReport,
  outDir: string,
): { jsonPath: string; markdownPath: string } {
  mkdirSync(outDir, { recursive: true });
  const stamp = new Date(report.timestamp).toISOString().replace(/[:.]/g, "-");
  const stem = `${report.driver}-${report.corpus}-${stamp}`;
  const jsonPath = join(outDir, `${stem}.json`);
  const markdownPath = join(outDir, `${stem}.md`);
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  writeFileSync(markdownPath, markdownSummary(report));
  return { jsonPath, markdownPath };
}

export function markdownSummary(report: EvalReport): string {
  const s = report.summary;
  const lines = [
    `# CodeGraph replication benchmark — ${report.driver}`,
    "",
    `- Corpus: \`${report.corpus}\``,
    `- Codebase: \`${report.codebasePath}\``,
    `- Timestamp: ${report.timestamp}`,
    `- Top K: ${report.topK}`,
    `- Runs/query: ${report.runs}`,
    "",
    "## Summary",
    "",
    "| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | median ms | p95 ms | skipped |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    `| ${report.driver} | ${s.total} | ${s.passed} | ${fixed(s.meanRecall)} | ${fixed(
      s.meanMRR,
    )} | ${fixed(s.meanPrecisionAt1)} | ${fixed(s.meanPrecisionAt5)} | ${fixed(
      s.meanPrecisionAt10,
    )} | ${fixed(s.latencyMsMedian, 1)} | ${fixed(s.latencyMsP95, 1)} | ${s.skipped} |`,
    "",
    "## Per case",
    "",
    "| case | api | status | recall | MRR | P@5 | latency ms | found | missed |",
    "| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |",
    ...report.results.map(caseRow),
    "",
  ];
  return `${lines.join("\n")}\n`;
}

function caseRow(result: EvalResult): string {
  if (result.skipReason) {
    return `| ${result.caseId} | ${result.api} | SKIP | 0.000 | 0.000 | 0.000 | ${fixed(
      result.latencyMs,
      1,
    )} |  | ${escapeCell(result.skipReason)} |`;
  }
  const found =
    result.foundSymbols.length > 0 ? result.foundSymbols.join(", ") : result.foundFiles.join(", ");
  const missed =
    result.foundSymbols.length > 0
      ? result.missedSymbols.join(", ")
      : result.missedSymbols.length > 0
        ? result.missedSymbols.join(", ")
        : result.missedFiles.join(", ");
  return `| ${result.caseId} | ${result.api} | ${result.pass ? "PASS" : "FAIL"} | ${fixed(
    result.recall,
  )} | ${fixed(result.mrr)} | ${fixed(result.precisionAtK.p5)} | ${fixed(
    result.latencyMs,
    1,
  )} | ${escapeCell(found)} | ${escapeCell(missed)} |`;
}

function fixed(value: number, digits = 3): string {
  return value.toFixed(digits);
}

function escapeCell(value: string): string {
  return value.replace(/\|/g, "\\|");
}
