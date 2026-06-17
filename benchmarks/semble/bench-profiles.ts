/**
 * Benchmark profile definitions for the AFT Semble quick benchmark package.
 *
 * Profiles control which suites, repos, review statuses, and mode sets are
 * included in a benchmark run. They are loaded by the runner and used to
 * filter canon queries before execution.
 */

export type ReviewStatus = "seed" | "reviewed" | "rejected" | "needs_update";

export interface ProfileSuiteFilter {
  /** Suites to include. Empty = all suites. */
  suites?: string[];
  /** Repos to include. Empty = all repos. */
  repos?: string[];
  /** Maximum queries per suite. 0 = unlimited. */
  max_queries_per_suite?: number;
  /** Allowed review statuses. Empty = all. */
  allowed_review_statuses?: ReviewStatus[];
  /** If true, seed rows are excluded unless explicitly allowed. */
  exclude_seed?: boolean;
}

export interface BenchmarkProfile {
  name: string;
  description: string;
  /** Per-suite filters. Key = suite name, value = filter overrides. */
  suite_filters: Record<string, ProfileSuiteFilter>;
  /** Global overrides applied when no suite-specific filter matches. */
  default_filter: ProfileSuiteFilter;
  /** Modes to run. Empty = all eligible modes per suite. */
  modes?: string[];
  /** Number of repetitions per query. */
  repetitions: number;
  /** Number of warmup queries before measurement. */
  warmups: number;
  /** If true, allow seed canon rows. If false, seed rows cause a preflight error. */
  allow_seed_canon: boolean;
  /** If true, emit status:unavailable rows for modes not built/enabled. */
  emit_unavailable: boolean;
}

export const PROFILES: Record<string, BenchmarkProfile> = {
  smoke: {
    name: "smoke",
    description: "Fastest validation: 2 queries per suite, reviewed rows only, no repetitions.",
    suite_filters: {
      identifier_exact: { max_queries_per_suite: 2, allowed_review_statuses: ["reviewed"] },
      identifier_prefix: { max_queries_per_suite: 2, allowed_review_statuses: ["reviewed"] },
      path_lookup: { max_queries_per_suite: 2, allowed_review_statuses: ["reviewed"] },
      structural: { max_queries_per_suite: 2, allowed_review_statuses: ["reviewed"] },
      semantic_nl: { max_queries_per_suite: 2 },
    },
    default_filter: { max_queries_per_suite: 2, allowed_review_statuses: ["reviewed"] },
    modes: ["aft-grep", "rg"],
    repetitions: 1,
    warmups: 0,
    allow_seed_canon: false,
    emit_unavailable: true,
  },

  quick: {
    name: "quick",
    description: "Decision-grade: all reviewed queries, seed rows allowed with warning, 1 repetition.",
    suite_filters: {
      identifier_exact: { allowed_review_statuses: ["reviewed", "seed"] },
      identifier_prefix: { allowed_review_statuses: ["reviewed", "seed"] },
      path_lookup: { allowed_review_statuses: ["reviewed", "seed"] },
      structural: { allowed_review_statuses: ["reviewed", "seed"] },
      semantic_nl: {},
    },
    default_filter: { allowed_review_statuses: ["reviewed", "seed"] },
    repetitions: 1,
    warmups: 1,
    allow_seed_canon: true,
    emit_unavailable: true,
  },

  extended: {
    name: "extended",
    description: "All canon queries, all modes, 3 repetitions for latency stability.",
    suite_filters: {},
    default_filter: {},
    repetitions: 3,
    warmups: 1,
    allow_seed_canon: true,
    emit_unavailable: true,
  },

  "manual-full": {
    name: "manual-full",
    description: "Full corpus, all modes, 5 repetitions. Manual invocation only.",
    suite_filters: {},
    default_filter: {},
    repetitions: 5,
    warmups: 2,
    allow_seed_canon: true,
    emit_unavailable: true,
  },
};

export function getProfile(name: string): BenchmarkProfile | undefined {
  return PROFILES[name];
}

export function listProfiles(): string[] {
  return Object.keys(PROFILES);
}
