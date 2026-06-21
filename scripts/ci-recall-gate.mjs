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

const baselineMetrics = baseline.intent_metrics || {};
const currentMetrics = current.intent_metrics || {};

const regressions = [];
const passed = [];

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

if (passed.length > 0) {
  console.log("PASSED:");
  for (const p of passed) {
    console.log(
      `  ${p.intent}: recall@10 = ${p.current_recall.toFixed(3)} (baseline: ${p.baseline_recall.toFixed(3)}, drop: ${p.drop_pct.toFixed(1)}%)`,
    );
  }
  console.log("");
}

if (regressions.length > 0) {
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
  console.log("FAIL: regression detected");
  process.exit(1);
} else {
  console.log("OK: no regressions detected");
  process.exit(0);
}
