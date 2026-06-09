# CodeGraph replication benchmark — ripgrep

- Corpus: `codegraph`
- Codebase: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/d4d1353f0512127b/bg_1e2cc588`
- Timestamp: 2026-05-26T09:41:56.635Z
- Top K: 10
- Runs/query: 1

## Summary

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | median ms | p95 ms | skipped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | 12 | 6 | 0.521 | 0.583 | 0.583 | 0.400 | 0.367 | 33.2 | 36.4 | 0 |

## Per case

| case | api | status | recall | MRR | P@5 | latency ms | found | missed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| search-class-exact | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 31.3 | BinaryBridge |  |
| search-method-qualified | searchNodes | PASS | 1.000 | 1.000 | 0.400 | 36.4 | send |  |
| search-interface | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 34.4 | BridgeOptions |  |
| search-enum | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 34.1 | SemanticIndexStatus |  |
| search-exception | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 33.1 | HomeProjectRootError |  |
| search-nested-class | searchNodes | PASS | 1.000 | 1.000 | 0.200 | 32.8 | BridgeReplacedDuringVersionCheck |  |
| explore-plugin-bridge | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 33.0 |  | callBridge, bridgeFor, BinaryBridge, BridgePool |
| explore-semantic-search | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 34.3 |  | semanticTools, handle_semantic_search, SemanticIndex, SearchIndex |
| explore-background-bash | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 30.2 |  | BgTaskRegistry, PersistedTask, replay_session, write_task |
| explore-lsp-diagnostics | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 33.2 |  | LspManager, wait_for_diagnostics, pull_file_diagnostics, PostEditWaitOutcome |
| explore-bridge-search | findRelevantContext | FAIL | 0.250 | 1.000 | 0.200 | 33.2 | BinaryBridge | send, semanticTools, handle_semantic_search |
| explore-import-organization | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 33.6 |  | handle_organize_imports, parse_imports, classify_group, generate_import_line |

