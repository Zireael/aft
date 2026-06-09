import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { RetrievalCorpus } from "./types";
import { HARNESS_DIR } from "./util";

export function loadCorpus(nameOrPath: string): { path: string; corpus: RetrievalCorpus } {
  const candidates = [
    nameOrPath,
    resolve(HARNESS_DIR, "corpora", `${nameOrPath}.json`),
    resolve(HARNESS_DIR, "corpora", nameOrPath),
  ];
  const corpusPath = candidates.find((candidate) => existsSync(candidate));
  if (!corpusPath) throw new Error(`Corpus not found: ${nameOrPath}`);
  const corpus = JSON.parse(readFileSync(corpusPath, "utf8")) as RetrievalCorpus;
  if (!Array.isArray(corpus.cases) || corpus.cases.length === 0) {
    throw new Error(`Corpus has no cases: ${corpusPath}`);
  }
  for (const testCase of corpus.cases) {
    if (!testCase.id || !testCase.query || !testCase.mode) {
      throw new Error(`Invalid corpus case in ${corpusPath}: ${JSON.stringify(testCase)}`);
    }
    if (!Array.isArray(testCase.groundTruth) || testCase.groundTruth.length === 0) {
      throw new Error(`Case ${testCase.id} has no groundTruth`);
    }
  }
  return { path: corpusPath, corpus };
}
