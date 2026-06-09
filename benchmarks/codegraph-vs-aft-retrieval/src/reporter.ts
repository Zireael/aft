import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { CaseResult, RetrievalReport } from "./types";

export function printReport(report: RetrievalReport): void {
  const s = report.summary;
  console.log(`\nAFT vs CodeGraph retrieval — ${report.driver}`);
  console.log(`  corpus: ${report.corpus}`);
  console.log(`  codebase: ${report.codebasePath}`);
  console.log(`  pass: ${s.passed}/${s.cases} | recall=${s.meanRecall.toFixed(3)} | mrr=${s.meanMRR.toFixed(3)} | p95=${s.latencyMsP95.toFixed(1)}ms`);
  for (const result of report.results) {
    const status = result.pass ? "PASS" : "FAIL";
    const missed = result.missed.map((truth) => truth.symbol ?? truth.file).join(",");
    console.log(`  ${status} ${result.caseId} recall=${result.recall.toFixed(2)} mrr=${result.mrr.toFixed(2)} ${result.latencyMs.toFixed(1)}ms${missed ? ` missed=${missed}` : ""}`);
  }
}

export function writeReports(report: RetrievalReport, outDir: string): { jsonPath: string; markdownPath: string } {
  mkdirSync(outDir, { recursive: true });
  const stamp = new Date(report.timestamp).toISOString().replace(/[:.]/g, "-");
  const stem = `${report.driver}-${report.corpus}-${stamp}`;
  const jsonPath = join(outDir, `${stem}.json`);
  const markdownPath = join(outDir, `${stem}.md`);
  writeFileSync(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  writeFileSync(markdownPath, markdownSummary(report));
  return { jsonPath, markdownPath };
}

export function markdownSummary(report: RetrievalReport): string {
  const s = report.summary;
  const lines = [
    `# AFT vs CodeGraph retrieval — ${report.driver}`,
    "",
    `- Corpus: \`${report.corpus}\``,
    `- Codebase: \`${report.codebasePath}\``,
    `- Target SHA: \`${report.targetSha}\``,
    `- Timestamp: ${report.timestamp}`,
    `- Top K: ${report.topK}`,
    "",
    "## Summary",
    "",
    "| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | p50 ms | p95 ms |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    `| ${report.driver} | ${s.cases} | ${s.passed} | ${fixed(s.meanRecall)} | ${fixed(s.meanMRR)} | ${fixed(s.meanPrecisionAt1)} | ${fixed(s.meanPrecisionAt5)} | ${fixed(s.meanPrecisionAt10)} | ${fixed(s.latencyMsP50, 1)} | ${fixed(s.latencyMsP95, 1)} |`,
    "",
    "## Per case",
    "",
    "| case | mode | status | recall | MRR | P@5 | latency ms | found | missed |",
    "| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |",
    ...report.results.map(caseRow),
    "",
  ];
  return `${lines.join("\n")}\n`;
}

function caseRow(result: CaseResult): string {
  return `| ${result.caseId} | ${result.mode} | ${result.pass ? "PASS" : "FAIL"} | ${fixed(result.recall)} | ${fixed(result.mrr)} | ${fixed(result.precisionAt5)} | ${fixed(result.latencyMs, 1)} | ${escapeCell(result.found.map((truth) => truth.symbol ?? truth.file).join(", "))} | ${escapeCell(result.missed.map((truth) => truth.symbol ?? truth.file).join(", "))} |`;
}

function fixed(value: number, digits = 3): string {
  return value.toFixed(digits);
}

function escapeCell(value: string): string {
  return value.replace(/\|/g, "\\|");
}
