# AFT vs CodeGraph retrieval — codegraph

- Corpus: `opencode-aft`
- Codebase: `/workspace`
- Target SHA: `unknown`
- Timestamp: 2026-05-27T05:09:27.778Z
- Top K: 10

## Summary

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | p50 ms | p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| codegraph | 6 | 3 | 0.500 | 0.500 | 0.500 | 0.100 | 0.050 | 72.9 | 137.6 |

## Per case

| case | mode | status | recall | MRR | P@5 | latency ms | found | missed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| symbol-binary-bridge | search | PASS | 1.000 | 1.000 | 0.200 | 70.8 | BinaryBridge |  |
| symbol-bridge-options | search | PASS | 1.000 | 1.000 | 0.200 | 72.9 | BridgeOptions |  |
| symbol-semantic-index-status | search | PASS | 1.000 | 1.000 | 0.200 | 69.8 | SemanticIndexStatus |  |
| context-plugin-bridge-dispatch | context | FAIL | 0.000 | 0.000 | 0.000 | 97.1 |  | callBridge, bridgeFor, BinaryBridge |
| context-semantic-search-path | context | FAIL | 0.000 | 0.000 | 0.000 | 114.7 |  | semanticTools, SemanticIndex, SearchIndex |
| context-import-organization | context | FAIL | 0.000 | 0.000 | 0.000 | 137.6 |  | handle_organize_imports, parse_imports, classify_group, generate_import_line |
