#!/usr/bin/env bun
/**
 * Corpus clone/cache tooling for AFT Semble benchmarks.
 *
 * Clones repos into a controlled cache, checks out pinned commits,
 * validates benchmark_root, and emits a state report.
 *
 * Usage:
 *   bun run benchmarks/semble/corpus.ts <command> [options]
 *
 * Commands:
 *   sync      Clone/fetch missing repos, checkout pinned commits
 *   check     Verify all repos are cloned at correct revisions
 *   status    Emit machine-readable corpus state report
 *   clean     Remove cached repos
 *
 * Options:
 *   --pilot              Use the pilot 5-repo subset
 *   --repo <name>        Operate on specific repo(s) (repeatable)
 *   --cache-dir <dir>    Cache directory (default: .bench-cache)
 *   --input <file>       Repo manifest (default: repos-pilot.json or repos.json)
 *   --format <fmt>       Output format: text|json (default: text)
 */

import {
  readFileSync,
  writeFileSync,
  existsSync,
  mkdirSync,
  rmSync,
  readdirSync,
} from "fs";
import { join, resolve } from "path";
import { execSync } from "child_process";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Repo {
  name: string;
  language: string;
  url: string;
  revision: string;
  benchmark_root: string | null;
}

interface RepoState {
  name: string;
  language: string;
  url: string;
  expected_revision: string;
  actual_revision: string | null;
  cloned: boolean;
  correct_revision: boolean;
  benchmark_root_valid: boolean;
  file_count: number;
  size_mb: number;
}

interface CorpusReport {
  timestamp: string;
  command: string;
  cache_dir: string;
  repos: RepoState[];
  summary: {
    total: number;
    cloned: number;
    correct_revision: number;
    valid_benchmark_root: number;
  };
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

interface CliOptions {
  command: "sync" | "check" | "status" | "clean";
  pilot: boolean;
  repos: string[];
  cacheDir: string;
  inputFile: string | null;
  format: "text" | "json";
}

function parseCliArgs(): CliOptions {
  const args = process.argv.slice(2);
  const opts: CliOptions = {
    command: "status",
    pilot: false,
    repos: [],
    cacheDir: ".bench-cache",
    inputFile: null,
    format: "text",
  };

  const commands = ["sync", "check", "status", "clean"];
  if (args.length > 0 && commands.includes(args[0])) {
    opts.command = args.shift() as CliOptions["command"];
  }

  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--pilot":
        opts.pilot = true;
        break;
      case "--repo":
        opts.repos.push(args[++i]);
        break;
      case "--cache-dir":
        opts.cacheDir = args[++i];
        break;
      case "--input":
        opts.inputFile = args[++i];
        break;
      case "--format":
        opts.format = args[++i] as "text" | "json";
        break;
    }
  }

  return opts;
}

// ---------------------------------------------------------------------------
// Git operations
// ---------------------------------------------------------------------------

function git(args: string, cwd: string): string {
  return execSync(`git ${args}`, { cwd, encoding: "utf-8", stdio: "pipe" })
    .trim();
}

function gitSafe(args: string, cwd: string): string | null {
  try {
    return git(args, cwd);
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Repo operations
// ---------------------------------------------------------------------------

function repoDir(cacheDir: string, name: string): string {
  return join(cacheDir, name);
}

function getRepoState(cacheDir: string, repo: Repo): RepoState {
  const dir = repoDir(cacheDir, repo.name);
  const cloned = existsSync(join(dir, ".git"));

  let actualRevision: string | null = null;
  let correctRevision = false;
  let benchmarkRootValid = false;
  let fileCount = 0;
  let sizeMb = 0;

  if (cloned) {
    actualRevision = gitSafe("rev-parse HEAD", dir);
    correctRevision = actualRevision === repo.revision;

    if (repo.benchmark_root) {
      const rootPath = join(dir, repo.benchmark_root);
      benchmarkRootValid = existsSync(rootPath);
    } else {
      benchmarkRootValid = true;
    }

    // Count files under benchmark_root
    const scanRoot = repo.benchmark_root
      ? join(dir, repo.benchmark_root)
      : dir;
    if (existsSync(scanRoot)) {
      try {
        const output = execSync(
          `find . -type f | wc -l`,
          { cwd: scanRoot, encoding: "utf-8", stdio: "pipe" }
        ).trim();
        fileCount = parseInt(output, 10) || 0;
      } catch {
        fileCount = -1;
      }
    }

    // Size estimate
    try {
      const output = execSync(
        `du -sm . 2>/dev/null | cut -f1`,
        { cwd: dir, encoding: "utf-8", stdio: "pipe" }
      ).trim();
      sizeMb = parseInt(output, 10) || 0;
    } catch {
      sizeMb = -1;
    }
  }

  return {
    name: repo.name,
    language: repo.language,
    url: repo.url,
    expected_revision: repo.revision,
    actual_revision: actualRevision,
    cloned,
    correct_revision: correctRevision,
    benchmark_root_valid: benchmarkRootValid,
    file_count: fileCount,
    size_mb: sizeMb,
  };
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

function loadRepos(opts: CliOptions): Repo[] {
  const inputPath = opts.inputFile
    ? resolve(opts.inputFile)
    : resolve(
        import.meta.dir,
        opts.pilot ? "repos-pilot.json" : "repos.json"
      );
  const allRepos: Repo[] = JSON.parse(readFileSync(inputPath, "utf-8"));

  if (opts.repos.length > 0) {
    const repoSet = new Set(opts.repos);
    return allRepos.filter((r) => repoSet.has(r.name));
  }
  return allRepos;
}

function cmdSync(repos: Repo[], cacheDir: string): CorpusReport {
  mkdirSync(cacheDir, { recursive: true });

  const states: RepoState[] = [];
  for (const repo of repos) {
    const dir = repoDir(cacheDir, repo.name);
    const state = getRepoState(cacheDir, repo);

    if (!state.cloned) {
      console.log(`Cloning ${repo.name}...`);
      try {
        execSync(`git clone --quiet ${repo.url} "${dir}"`, {
          stdio: "pipe",
        });
      } catch (e) {
        console.error(`Failed to clone ${repo.name}: ${e}`);
        states.push(state);
        continue;
      }
    }

    if (!state.correct_revision) {
      console.log(`Checking out ${repo.name}@${repo.revision.slice(0, 8)}...`);
      gitSafe(`fetch --quiet origin`, dir);
      gitSafe(`checkout --quiet ${repo.revision}`, dir);
    }

    states.push(getRepoState(cacheDir, repo));
  }

  return buildReport("sync", cacheDir, states);
}

function cmdCheck(repos: Repo[], cacheDir: string): CorpusReport {
  const states = repos.map((r) => getRepoState(cacheDir, r));
  return buildReport("check", cacheDir, states);
}

function cmdStatus(repos: Repo[], cacheDir: string): CorpusReport {
  const states = repos.map((r) => getRepoState(cacheDir, r));
  return buildReport("status", cacheDir, states);
}

function cmdClean(repos: Repo[], cacheDir: string): CorpusReport {
  const states: RepoState[] = [];
  for (const repo of repos) {
    const dir = repoDir(cacheDir, repo.name);
    if (existsSync(dir)) {
      console.log(`Removing ${repo.name}...`);
      rmSync(dir, { recursive: true, force: true });
    }
    states.push({
      ...getRepoState(cacheDir, repo),
      cloned: false,
      actual_revision: null,
    });
  }
  return buildReport("clean", cacheDir, states);
}

function buildReport(
  command: string,
  cacheDir: string,
  states: RepoState[]
): CorpusReport {
  return {
    timestamp: new Date().toISOString(),
    command,
    cache_dir: resolve(cacheDir),
    repos: states,
    summary: {
      total: states.length,
      cloned: states.filter((s) => s.cloned).length,
      correct_revision: states.filter((s) => s.correct_revision).length,
      valid_benchmark_root: states.filter((s) => s.benchmark_root_valid).length,
    },
  };
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

function printText(report: CorpusReport) {
  console.log(`\n=== Corpus ${report.command} ===`);
  console.log(`Cache: ${report.cache_dir}`);
  console.log(
    `Repos: ${report.summary.cloned}/${report.summary.total} cloned, ` +
      `${report.summary.correct_revision}/${report.summary.total} at correct revision, ` +
      `${report.summary.valid_benchmark_root}/${report.summary.total} valid benchmark_root`
  );
  console.log();

  for (const r of report.repos) {
    const status = !r.cloned
      ? "MISSING"
      : !r.correct_revision
        ? "WRONG REVISION"
        : !r.benchmark_root_valid
          ? "INVALID ROOT"
          : "OK";
    console.log(
      `  ${r.name.padEnd(20)} ${r.language.padEnd(12)} ${status.padEnd(16)} ` +
        `${r.file_count} files, ${r.size_mb}MB`
    );
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const opts = parseCliArgs();
  const repos = loadRepos(opts);
  const cacheDir = resolve(opts.cacheDir);

  let report: CorpusReport;
  switch (opts.command) {
    case "sync":
      report = cmdSync(repos, cacheDir);
      break;
    case "check":
      report = cmdCheck(repos, cacheDir);
      break;
    case "status":
      report = cmdStatus(repos, cacheDir);
      break;
    case "clean":
      report = cmdClean(repos, cacheDir);
      break;
  }

  if (opts.format === "json") {
    console.log(JSON.stringify(report, null, 2));
  } else {
    printText(report);
  }
}

main();
