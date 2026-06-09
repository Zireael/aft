export type CodeGraphApi = "searchNodes" | "findRelevantContext";

export type AftTool =
  | "aft_search"
  | "aft_grep"
  | "aft_outline"
  | "aft_zoom"
  | "aft_navigate"
  | "ripgrep"
  | "list_files";

export type DriverName = "aft" | "aft-grep" | "ripgrep" | "list-files";

export interface GroundTruthLocation {
  file: string;
  startLine?: number;
  endLine?: number;
  relevance?: number;
}

export interface EvalTestCase {
  id: string;
  query: string;
  api: CodeGraphApi;
  expectedSymbols: string[];
  expectedFiles?: string[];
  groundTruth?: GroundTruthLocation[];
  kinds?: string[];
  category?: string;
  tool?: AftTool;
  options?: Record<string, unknown>;
  sourceCaseId?: string;
  notes?: string;
  skip?: boolean;
  skipReason?: string;
}

export interface CorpusFile {
  name?: string;
  description?: string;
  source?: string;
  attribution?: string;
  cases?: EvalTestCase[];
  testCases?: EvalTestCase[];
}

export interface RankedResult {
  rank: number;
  file?: string;
  name?: string;
  kind?: string;
  line?: number;
  endLine?: number;
  score?: number;
  source?: string;
  text?: string;
}

export interface DriverRunResult {
  items: RankedResult[];
  raw?: unknown;
  status?: string;
}

export interface PrecisionAtK {
  p1: number;
  p5: number;
  p10: number;
}

export interface EvalResult {
  caseId: string;
  api: CodeGraphApi;
  driver: string;
  pass: boolean;
  recall: number;
  mrr: number;
  precisionAtK: PrecisionAtK;
  foundSymbols: string[];
  missedSymbols: string[];
  foundFiles: string[];
  missedFiles: string[];
  latencyMs: number;
  latencySamplesMs: number[];
  latencyMsMedian: number;
  latencyMsP95: number;
  resultCount: number;
  results: RankedResult[];
  skipReason?: string;
  error?: string;
}

export interface EvalSummary {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
  meanRecall: number;
  meanMRR: number;
  meanPrecisionAt1: number;
  meanPrecisionAt5: number;
  meanPrecisionAt10: number;
  latencyMsMedian: number;
  latencyMsP95: number;
}

export interface EvalReport {
  timestamp: string;
  benchmark: "codegraph-replication";
  codebasePath: string;
  codegraphSha: string;
  aftSha?: string;
  driver: string;
  corpus: string;
  corpusPath: string;
  topK: number;
  runs: number;
  summary: EvalSummary;
  results: EvalResult[];
  metadata: Record<string, unknown>;
}

export interface CliOptions {
  driver: DriverName;
  corpus: string;
  codebasePath: string;
  binaryPath: string;
  outDir: string;
  topK: number;
  runs: number;
  readyTimeoutMs: number;
  includeSkipped: boolean;
  verbose: boolean;
}

export interface DriverContext {
  codebasePath: string;
  binaryPath: string;
  topK: number;
  readyTimeoutMs: number;
  verbose: boolean;
}

export interface EvalDriver {
  name: string;
  prepare?(): Promise<void>;
  run(testCase: EvalTestCase): Promise<DriverRunResult>;
  close?(): Promise<void>;
}
