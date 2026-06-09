# CodeGraph replication benchmark — list-files

- Corpus: `codegraph`
- Codebase: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/d4d1353f0512127b/bg_1e2cc588`
- Timestamp: 2026-05-26T09:42:18.550Z
- Top K: 10
- Runs/query: 1

## Summary

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | median ms | p95 ms | skipped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| list-files | 12 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0.000 | 0.1 | 0.3 | 0 |

## Per case

| case | api | status | recall | MRR | P@5 | latency ms | found | missed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| search-class-exact | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.3 |  | BinaryBridge |
| search-method-qualified | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | send |
| search-interface | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | BridgeOptions |
| search-enum | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | SemanticIndexStatus |
| search-exception | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | HomeProjectRootError |
| search-nested-class | searchNodes | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | BridgeReplacedDuringVersionCheck |
| explore-plugin-bridge | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | callBridge, bridgeFor, BinaryBridge, BridgePool |
| explore-semantic-search | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | semanticTools, handle_semantic_search, SemanticIndex, SearchIndex |
| explore-background-bash | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | BgTaskRegistry, PersistedTask, replay_session, write_task |
| explore-lsp-diagnostics | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | LspManager, wait_for_diagnostics, pull_file_diagnostics, PostEditWaitOutcome |
| explore-bridge-search | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | BinaryBridge, send, semanticTools, handle_semantic_search |
| explore-import-organization | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 0.1 |  | handle_organize_imports, parse_imports, classify_group, generate_import_line |

