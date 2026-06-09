#!/usr/bin/env bun
/**
 * Semble annotation importer for AFT benchmarks.
 *
 * Reads Semble's repos.json + annotations/*.json and produces
 * an AFT-compatible fixture file preserving all provenance fields.
 *
 * Usage:
 *   bun run benchmarks/semble/import.ts [options]
 *
 * Options:
 *   --repo <name>        Filter to specific repo(s) (repeatable)
 *   --language <lang>    Filter by language (repeatable)
 *   --category <cat>     Filter by category: symbol|semantic|architecture (repeatable)
 *   --limit <n>          Max annotations to include (default: all)
 *   --pilot              Use the pilot 5-repo subset instead of full 63-repo set
 *   --input <dir>        Input directory (default: ./benchmarks/semble)
 *   --output <file>      Output file (default: ./benchmarks/semble/fixtures.json)
 */

import { readFileSync, readdirSync, writeFileSync, mkdirSync } from "fs";
import { join, resolve } from "path";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface SembleRepo {
  name: string;
  language: string;
  url: string;
  revision: string;
  benchmark_root: string | null;
}

interface SembleTarget {
  path?: string;
  start_line?: number;
  end_line?: number;
}

interface SembleAnnotation {
  query: string;
  relevant: (string | SembleTarget)[];
  secondary?: (string | SembleTarget)[];
  category: "symbol" | "semantic" | "architecture";
  seed?: { path: string; line: number };
  related?: string[];
}

interface AftRepo {
  name: string;
  language: string;
  url: string;
  revision: string;
  benchmark_root: string | null;
}

interface AftTarget {
  path: string;
  start_line?: number;
  end_line?: number;
}

interface AftAnnotation {
  query: string;
  relevant: AftTarget[];
  secondary: AftTarget[];
  category: "symbol" | "semantic" | "architecture";
  repo_name: string;
  tags: string[];
}

interface AftFixture {
  schema_version: 1;
  source: {
    name: string;
    upstream: string;
    imported_at: string;
    importer_version: string;
  };
  repos: AftRepo[];
  annotations: AftAnnotation[];
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

interface CliOptions {
  repos: string[];
  languages: string[];
  categories: string[];
  limit: number;
  pilot: boolean;
  inputDir: string;
  outputFile: string;
}

function parseCliArgs(): CliOptions {
  const args = process.argv.slice(2);
  const opts: CliOptions = {
    repos: [],
    languages: [],
    categories: [],
    limit: Infinity,
    pilot: false,
    inputDir: resolve(import.meta.dir),
    outputFile: resolve(import.meta.dir, "fixtures.json"),
  };

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--repo":
        opts.repos.push(args[++i]);
        break;
      case "--language":
        opts.languages.push(args[++i]);
        break;
      case "--category":
        opts.categories.push(args[++i]);
        break;
      case "--limit":
        opts.limit = parseInt(args[++i], 10);
        break;
      case "--pilot":
        opts.pilot = true;
        break;
      case "--input":
        opts.inputDir = resolve(args[++i]);
        break;
      case "--output":
        opts.outputFile = resolve(args[++i]);
        break;
    }
  }

  return opts;
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

function normalizeTarget(t: string | SembleTarget): AftTarget {
  if (typeof t === "string") {
    return { path: t };
  }
  const target: AftTarget = { path: t.path! };
  if (t.start_line !== undefined) target.start_line = t.start_line;
  if (t.end_line !== undefined) target.end_line = t.end_line;
  return target;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const opts = parseCliArgs();

  // Load repos manifest
  const reposFile = opts.pilot
    ? join(opts.inputDir, "repos-pilot.json")
    : join(opts.inputDir, "repos.json");
  const allRepos: SembleRepo[] = JSON.parse(readFileSync(reposFile, "utf-8"));

  // Filter repos
  let repos = allRepos;
  if (opts.repos.length > 0) {
    const repoSet = new Set(opts.repos);
    repos = repos.filter((r) => repoSet.has(r.name));
  }
  if (opts.languages.length > 0) {
    const langSet = new Set(opts.languages);
    repos = repos.filter((r) => langSet.has(r.language));
  }

  // Build repo lookup
  const repoMap = new Map<string, SembleRepo>();
  for (const r of repos) repoMap.set(r.name, r);

  // Load annotations
  const annDir = join(opts.inputDir, "annotations");
  const annFiles = readdirSync(annDir).filter((f) => f.endsWith(".json"));

  const aftRepos: AftRepo[] = repos.map((r) => ({
    name: r.name,
    language: r.language,
    url: r.url,
    revision: r.revision,
    benchmark_root: r.benchmark_root,
  }));

  const aftAnnotations: AftAnnotation[] = [];

  for (const annFile of annFiles) {
    const repoName = annFile.replace(/\.json$/, "");
    if (!repoMap.has(repoName)) continue;

    const annotations: SembleAnnotation[] = JSON.parse(
      readFileSync(join(annDir, annFile), "utf-8")
    );

    for (const ann of annotations) {
      // Filter by category
      if (
        opts.categories.length > 0 &&
        !opts.categories.includes(ann.category)
      ) {
        continue;
      }

      // Filter out seed/related (Semble-specific, not needed for AFT)
      const tags: string[] = [];
      if (ann.seed) tags.push("has-seed");
      if (ann.related) tags.push("has-related");

      aftAnnotations.push({
        query: ann.query,
        relevant: ann.relevant.map(normalizeTarget),
        secondary: (ann.secondary ?? []).map(normalizeTarget),
        category: ann.category,
        repo_name: repoName,
        tags,
      });

      if (aftAnnotations.length >= opts.limit) break;
    }
    if (aftAnnotations.length >= opts.limit) break;
  }

  // Build fixture
  const fixture: AftFixture = {
    schema_version: 1,
    source: {
      name: opts.pilot ? "semble-pilot" : "semble-full",
      upstream:
        "https://github.com/MinishLab/semble/tree/main/benchmarks",
      imported_at: new Date().toISOString(),
      importer_version: "0.1.0",
    },
    repos: aftRepos,
    annotations: aftAnnotations,
  };

  // Write output
  mkdirSync(resolve(opts.outputFile, ".."), { recursive: true });
  writeFileSync(opts.outputFile, JSON.stringify(fixture, null, 2) + "\n");

  // Summary
  const langCounts = new Map<string, number>();
  const catCounts = new Map<string, number>();
  for (const a of aftAnnotations) {
    const r = repoMap.get(a.repo_name);
    if (r) langCounts.set(r.language, (langCounts.get(r.language) ?? 0) + 1);
    catCounts.set(a.category, (catCounts.get(a.category) ?? 0) + 1);
  }

  console.log(`Imported ${aftAnnotations.length} annotations from ${repos.length} repos`);
  console.log(`Languages: ${Object.fromEntries(langCounts)}`);
  console.log(`Categories: ${Object.fromEntries(catCounts)}`);
  console.log(`Output: ${opts.outputFile}`);
}

main();
