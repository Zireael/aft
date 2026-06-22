#!/usr/bin/env node

// CI Recall Gate — checks benchmark intent_metrics against baseline thresholds.
//
// Usage: node scripts/ci-recall-gate.mjs <baseline.json> <current.json>

import { readFileSync } from "fs";

const THRESHOLD = 5; // percent drop allowed

const baselineFile = process.argv[2];
const currentFile = process.argv[3];

if (!baselineFile || !currentFile) {
  console.error("Usage: ci-recall-gate.mjs <baseline.json> <current.json>");
  process.exit(1);
}

const baseline = JSON.parse(readFileSync(baselineFile, "utf8"));
const current = JSON.parse(readFileSync(currentFile, "utf8"));

const regressions = [];
const passed = [];
const schemaFailures = [];

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

if (current.status === "incomplete") {
  schemaFailures.push({
    reason: "current benchmark report is incomplete",
    detail: Array.isArray(current.incomplete_reasons)
      ? current.incomplete_reasons.join("; ")
      : "no incomplete_reasons provided",
  });
}

if (!isObject(baseline.intent_metrics) || Object.keys(baseline.intent_metrics).length === 0) {
  schemaFailures.push({
    reason: "baseline is missing non-empty intent_metrics",
    detail: baselineFile,
  });
}

if (!isObject(current.intent_metrics) || Object.keys(current.intent_metrics).length === 0) {
  schemaFailures.push({
    reason: "current run is missing non-empty intent_metrics",
    detail: currentFile,
  });
}

if (!isObject(current.context_quality)) {
  schemaFailures.push({
    reason: "current run is missing context_quality",
    detail: currentFile,
  });
}

if (current.rerank_context !== "aft_output") {
  schemaFailures.push({
    reason: "current run must use rerank_context=aft_output",
    detail: `actual: ${current.rerank_context ?? "(missing)"}`,
  });
}

const baselineMetrics = isObject(baseline.intent_metrics) ? baseline.intent_metrics : {};
const currentMetrics = isObject(current.intent_metrics) ? current.intent_metrics : {};

for (const [intent, bMetrics] of Object.entries(baselineMetrics)) {
  const cMetrics = currentMetrics[intent];
  if (!cMetrics) {
    regressions.push({
      intent,
      reason: "category missing from current run",
      baseline_recall: bMetrics.recall_at_10,
      current_recall: null,
    });
    continue;
  }

  const bRecall = bMetrics.recall_at_10 || 0;
  const cRecall = cMetrics.recall_at_10 || 0;

  if (bRecall > 0) {
    const drop = ((bRecall - cRecall) / bRecall) * 100;
    if (drop > THRESHOLD) {
      regressions.push({
        intent,
        reason: `recall_at_10 dropped ${drop.toFixed(1)}% (>${THRESHOLD}%)`,
        baseline_recall: bRecall,
        current_recall: cRecall,
        drop_pct: drop,
      });
    } else {
      passed.push({
        intent,
        baseline_recall: bRecall,
        current_recall: cRecall,
        drop_pct: drop,
      });
    }
  } else {
    passed.push({
      intent,
      baseline_recall: bRecall,
      current_recall: cRecall,
      drop_pct: 0,
    });
  }
}

if (schemaFailures.length > 0) {
  console.log("SCHEMA FAILURES:");
  for (const failure of schemaFailures) {
    console.log(`  ${failure.reason}`);
    if (failure.detail) console.log(`    ${failure.detail}`);
  }
  console.log("");
}

if (passed.length > 0) {
  console.log("PASSED:");
  for (const p of passed) {
    console.log(
      `  ${p.intent}: recall@10 = ${p.current_recall.toFixed(3)} (baseline: ${p.baseline_recall.toFixed(3)}, drop: ${p.drop_pct.toFixed(1)}%)`,
    );
  }
  console.log("");
}

if (schemaFailures.length > 0 || regressions.length > 0) {
  console.log("REGRESSIONS:");
  for (const r of regressions) {
    console.log(`  ${r.intent}: ${r.reason}`);
    if (r.current_recall !== null) {
      console.log(
        `    baseline: ${r.baseline_recall.toFixed(3)}, current: ${r.current_recall.toFixed(3)}`,
      );
    }
  }
  console.log("");
  console.log(schemaFailures.length > 0 ? "FAIL: benchmark report invalid" : "FAIL: regression detected");
  process.exit(1);
} else {
  console.log("OK: no regressions detected");
  process.exit(0);
}
