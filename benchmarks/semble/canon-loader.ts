/**
 * Canon loader for the AFT Semble benchmark.
 *
 * Loads checked-in canon JSON files and filters them by profile settings.
 * This is the only source of relevance truth for lexical/path/structural suites.
 */

import { readFileSync, readdirSync } from "fs";
import { join } from "path";
import type { BenchmarkProfile, ReviewStatus } from "./bench-profiles";

export interface RelevantEntry {
  path: string;
  grade?: string;
  confidence?: string;
  review_status?: ReviewStatus;
  symbol?: string;
  kind?: string;
}

export interface CanonQuery {
  id: string;
  repo_name: string;
  language: string;
  query: string;
  intent: string;
  eligible_modes: string[];
  relevant: RelevantEntry[];
  secondary?: RelevantEntry[];
  review_status?: ReviewStatus;
  ast_pattern?: string;
  ast_language?: string;
  candidate_source?: string;
}

export interface CanonSuite {
  schema_version: number;
  suite: string;
  generated_at: string;
  description?: string;
  queries: CanonQuery[];
}

export interface RepoEntry {
  name: string;
  language: string;
  url: string;
  revision: string | null;
  benchmark_root: string | null;
  pin_status: string;
  review_required?: boolean;
}

export interface CanonRepos {
  schema_version: number;
  generated_at: string;
  source?: Record<string, unknown>;
  repos: RepoEntry[];
}

export interface ModeMatrixSuite {
  primary_modes: string[];
  control_modes: string[];
  do_not_mix_with?: string[];
}

export interface ModeMatrix {
  schema_version: number;
  generated_at: string;
  suites: Record<string, ModeMatrixSuite>;
}

const SUITE_FILES: Record<string, string> = {
  identifier_exact: "identifier-exact.json",
  identifier_prefix: "identifier-prefix.json",
  path_lookup: "path-lookup.json",
  structural: "structural.json",
  semantic_nl: "semantic_nl.json", // may not exist yet
};

/**
 * Load a canon suite from disk.
 */
export function loadCanonSuite(canonDir: string, suite: string): CanonSuite | null {
  const filename = SUITE_FILES[suite];
  if (!filename) return null;
  const filepath = join(canonDir, filename);
  try {
    const raw = readFileSync(filepath, "utf-8");
    return JSON.parse(raw) as CanonSuite;
  } catch {
    return null;
  }
}

/**
 * Load all canon suites from a directory.
 */
export function loadAllCanonSuites(canonDir: string): Record<string, CanonSuite> {
  const result: Record<string, CanonSuite> = {};
  for (const suite of Object.keys(SUITE_FILES)) {
    const loaded = loadCanonSuite(canonDir, suite);
    if (loaded) {
      result[suite] = loaded;
    }
  }
  return result;
}

/**
 * Load repo metadata from canon repos.json.
 */
export function loadCanonRepos(canonDir: string): CanonRepos {
  const filepath = join(canonDir, "repos.json");
  const raw = readFileSync(filepath, "utf-8");
  return JSON.parse(raw) as CanonRepos;
}

/**
 * Load mode matrix from canon mode-matrix.json.
 */
export function loadModeMatrix(canonDir: string): ModeMatrix {
  const filepath = join(canonDir, "mode-matrix.json");
  const raw = readFileSync(filepath, "utf-8");
  return JSON.parse(raw) as ModeMatrix;
}

/**
 * Filter canon queries by profile settings.
 *
 * Returns queries that pass all filters:
 * - suite is in the profile's suite_filters or matches default_filter
 * - repo is in the profile's allowed repos (if specified)
 * - review_status is in the profile's allowed statuses (if specified)
 * - max_queries_per_suite limit is respected
 */
export function filterQueriesByProfile(
  queries: CanonQuery[],
  suite: string,
  profile: BenchmarkProfile,
): CanonQuery[] {
  const filter = profile.suite_filters[suite] ?? profile.default_filter;
  let filtered = [...queries];

  // Filter by repo
  if (filter.repos && filter.repos.length > 0) {
    filtered = filtered.filter((q) => filter.repos!.includes(q.repo_name));
  }

  // Filter by review status
  if (filter.allowed_review_statuses && filter.allowed_review_statuses.length > 0) {
    filtered = filtered.filter((q) => {
      const status = q.review_status ?? "seed";
      return filter.allowed_review_statuses!.includes(status);
    });
  }

  // Exclude seed rows if profile says so
  if (filter.exclude_seed) {
    filtered = filtered.filter((q) => q.review_status !== "seed");
  }

  // Apply max queries limit
  if (filter.max_queries_per_suite && filter.max_queries_per_suite > 0) {
    filtered = filtered.slice(0, filter.max_queries_per_suite);
  }

  return filtered;
}

/**
 * Validate that a mode is known to the mode matrix for a given suite.
 */
export function isModeKnown(mode: string, suite: string, matrix: ModeMatrix): boolean {
  const suiteEntry = matrix.suites[suite];
  if (!suiteEntry) return false;
  return suiteEntry.primary_modes.includes(mode) || suiteEntry.control_modes.includes(mode);
}
