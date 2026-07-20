# AFT Semble Lexical Canon Package

Generated: 2026-06-16T21:22:45Z

This package adds a reviewable lexical/search canon for the current AFT Semble benchmark work.

It is intentionally **not** generated from a benchmark-time ripgrep pass. Runtime truth generation should stay forbidden: the benchmark should load checked-in JSON and score every requested mode against that oracle.

## Contents

```text
benchmarks/semble/canon/
  repos.json
  lexical-canon.schema.json
  mode-matrix.json
  identifier-exact.json
  identifier-prefix.json
  path-lookup.json
  structural.json
  unverified-seeds.json
  README.md
  validation-checklist.md
benchmarks/semble/tools/
  validate-lexical-canon.ts
INTEGRATION_PROMPT.md
```

## Review status

All rows are marked `review_status: "seed"`.

This is deliberate. The package is meant to remove the flawed benchmark-time `rg` oracle and give the coding agent a strong starting canon. Before using it for hard regression gates, validate path existence and line/symbol spans at the pinned revisions.

## Included repo groups

1. Pinned fixture repos:
   - axum
   - express
   - pydantic
   - serde
   - gin

2. Current `pilot.ts` hardcoded lexical repos:
   - opencode-aft
   - reth

The second group is placed in `unverified-seeds.json` because the current script does not pin revisions for those repos.

## Scoring rule

Every requested mode should emit exactly one attempt per query:

- `status: ok`
- `status: empty`
- `status: error`
- `status: unavailable`

Empty/error/unavailable attempts count as zero for recall/MRR/nDCG. Do not drop them from denominators.

## Do not do this

Do not generate `allRelevant` by calling `rgSearch()` during a benchmark run. That makes ripgrep both the oracle and a contestant.
