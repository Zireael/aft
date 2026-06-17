#!/usr/bin/env bun
/**
 * Lightweight lexical canon validator.
 *
 * This intentionally validates schema shape and duplicate IDs only.
 * Repo checkout/path/symbol validation belongs in the benchmark runner or a
 * separate command that has local checkouts available.
 */
import { readdirSync, readFileSync } from "fs";
import { join, resolve } from "path";

const canonDir = resolve(process.argv[2] || "benchmarks/semble/canon");
const files = readdirSync(canonDir).filter((f) => f.endsWith(".json") && !["repos.json", "mode-matrix.json", "lexical-canon.schema.json"].includes(f));

const seen = new Set<string>();
let errors = 0;

for (const file of files) {
  const full = join(canonDir, file);
  const doc = JSON.parse(readFileSync(full, "utf-8"));

  if (doc.schema_version !== 1) {
    console.error(`${file}: schema_version must be 1`);
    errors++;
  }
  if (!Array.isArray(doc.queries)) {
    console.error(`${file}: queries must be an array`);
    errors++;
    continue;
  }

  for (const [idx, q] of doc.queries.entries()) {
    const prefix = `${file}:queries[${idx}]`;
    if (!q.id) { console.error(`${prefix}: missing id`); errors++; continue; }
    if (seen.has(q.id)) { console.error(`${prefix}: duplicate id ${q.id}`); errors++; }
    seen.add(q.id);

    for (const key of ["repo_name", "language", "query"]) {
      if (!q[key]) { console.error(`${prefix}: missing ${key}`); errors++; }
    }

    if (file !== "unverified-seeds.json") {
      if (!q.intent) { console.error(`${prefix}: missing intent`); errors++; }
      if (!Array.isArray(q.eligible_modes)) { console.error(`${prefix}: missing eligible_modes`); errors++; }
      if (!Array.isArray(q.relevant)) { console.error(`${prefix}: missing relevant[]`); errors++; }
      for (const [ridx, r] of (q.relevant || []).entries()) {
        if (!r.path) { console.error(`${prefix}.relevant[${ridx}]: missing path`); errors++; }
      }
    }
  }
}

if (errors) {
  console.error(`\n${errors} validation error(s).`);
  process.exit(1);
}
console.log(`OK: ${seen.size} canon query rows across ${files.length} files.`);
