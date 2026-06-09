import { existsSync, readFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import type { CorpusFile, EvalTestCase } from "./types";
import { HARNESS_DIR } from "./util";

const NAMED_CORPORA: Record<string, string> = {
  codegraph: "corpora/codegraph.json",
  "codegraph-original": "corpora/codegraph-original.json",
  aft: "corpora/aft.json",
};

export interface LoadedCorpus {
  name: string;
  path: string;
  description?: string;
  source?: string;
  attribution?: string;
  cases: EvalTestCase[];
}

export function resolveCorpusPath(corpus: string): string {
  const named = NAMED_CORPORA[corpus];
  if (named) return resolve(HARNESS_DIR, named);
  return resolve(corpus);
}

export function loadCorpus(corpus: string): LoadedCorpus {
  const corpusPath = resolveCorpusPath(corpus);
  if (!existsSync(corpusPath)) {
    const names = Object.keys(NAMED_CORPORA).join(", ");
    throw new Error(`Corpus not found: ${corpus}. Use one of ${names} or pass a JSON path.`);
  }

  const parsed = JSON.parse(readFileSync(corpusPath, "utf-8")) as CorpusFile | EvalTestCase[];
  if (Array.isArray(parsed)) {
    return {
      name: corpus,
      path: corpusPath,
      cases: normalizeCases(parsed, corpusPath),
    };
  }

  const cases = parsed.cases ?? parsed.testCases ?? [];
  return {
    name: parsed.name ?? corpusNameFromPath(corpus, corpusPath),
    path: corpusPath,
    description: parsed.description,
    source: parsed.source,
    attribution: parsed.attribution,
    cases: normalizeCases(cases, corpusPath),
  };
}

function corpusNameFromPath(corpus: string, corpusPath: string): string {
  return corpus in NAMED_CORPORA ? corpus : basename(corpusPath).replace(/\.json$/, "");
}

function normalizeCases(cases: EvalTestCase[], corpusPath: string): EvalTestCase[] {
  if (!Array.isArray(cases) || cases.length === 0) {
    throw new Error(`Corpus has no cases: ${corpusPath}`);
  }

  const seen = new Set<string>();
  return cases.map((testCase, index) => {
    for (const key of ["id", "query", "api", "expectedSymbols"] as const) {
      if (!(key in testCase)) {
        throw new Error(`Case ${index + 1} in ${corpusPath} is missing ${key}`);
      }
    }
    if (seen.has(testCase.id)) {
      throw new Error(`Duplicate case id in ${corpusPath}: ${testCase.id}`);
    }
    seen.add(testCase.id);
    if (!Array.isArray(testCase.expectedSymbols)) {
      throw new Error(`Case ${testCase.id} expectedSymbols must be an array`);
    }
    return {
      ...testCase,
      expectedFiles: testCase.expectedFiles ?? testCase.groundTruth?.map((truth) => truth.file),
    };
  });
}
