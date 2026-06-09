export type RetrievalDriverName = "aft" | "codegraph";
export type RetrievalMode = "search" | "context";

export interface GroundTruth {
  file: string;
  symbol?: string;
  startLine?: number;
  endLine?: number;
  relevance?: number;
}

export interface RetrievalCase {
  id: string;
  query: string;
  mode: RetrievalMode;
  hint?: "auto" | "semantic" | "literal" | "regex";
  kind?: string;
  expectedSymbols?: string[];
  groundTruth: GroundTruth[];
  notes?: string;
}

export interface RetrievalCorpus {
  name: string;
  description?: string;
  target?: {
    name: string;
    kind: "local" | "git";
    path?: string;
    url?: string;
    commit?: string;
  };
  cases: RetrievalCase[];
}

export interface RankedResult {
  rank: number;
  file?: string;
  symbol?: string;
  kind?: string;
  startLine?: number;
  endLine?: number;
  score?: number;
  text?: string;
}

export interface CaseResult {
  caseId: string;
  query: string;
  mode: RetrievalMode;
  driver: RetrievalDriverName;
  pass: boolean;
  recall: number;
  mrr: number;
  precisionAt1: number;
  precisionAt5: number;
  precisionAt10: number;
  found: GroundTruth[];
  missed: GroundTruth[];
  latencyMs: number;
  resultCount: number;
  results: RankedResult[];
  error?: string;
}

export interface RetrievalSummary {
  cases: number;
  passed: number;
  failed: number;
  meanRecall: number;
  meanMRR: number;
  meanPrecisionAt1: number;
  meanPrecisionAt5: number;
  meanPrecisionAt10: number;
  latencyMsP50: number;
  latencyMsP95: number;
}

export interface RetrievalReport {
  benchmark: "codegraph-vs-aft-retrieval";
  driver: RetrievalDriverName;
  timestamp: string;
  corpus: string;
  corpusPath: string;
  codebasePath: string;
  targetSha: string;
  topK: number;
  summary: RetrievalSummary;
  results: CaseResult[];
  metadata: Record<string, unknown>;
}

export interface DriverContext {
  codebasePath: string;
  topK: number;
  timeoutMs: number;
  aftBinary: string;
}

export interface RetrievalDriver {
  name: RetrievalDriverName;
  prepare?(): Promise<void>;
  run(testCase: RetrievalCase): Promise<{ items: RankedResult[] }>;
  close?(): Promise<void>;
}
