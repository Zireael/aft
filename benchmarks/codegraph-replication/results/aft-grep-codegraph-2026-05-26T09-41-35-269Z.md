# CodeGraph replication benchmark — aft-grep

- Corpus: `codegraph`
- Codebase: `/Users/ufukaltinok/.local/share/cortexkit/alfonso/worktrees/d4d1353f0512127b/bg_1e2cc588`
- Timestamp: 2026-05-26T09:41:35.269Z
- Top K: 10
- Runs/query: 1

## Summary

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | median ms | p95 ms | skipped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| aft-grep | 12 | 6 | 0.521 | 0.583 | 0.583 | 0.400 | 0.367 | 13.2 | 21.0 | 0 |

## Per case

| case | api | status | recall | MRR | P@5 | latency ms | found | missed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| search-class-exact | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 16.0 | BinaryBridge |  |
| search-method-qualified | searchNodes | PASS | 1.000 | 1.000 | 0.400 | 16.5 | send |  |
| search-interface | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 11.3 | BridgeOptions |  |
| search-enum | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 13.7 | SemanticIndexStatus |  |
| search-exception | searchNodes | PASS | 1.000 | 1.000 | 1.000 | 13.5 | HomeProjectRootError |  |
| search-nested-class | searchNodes | PASS | 1.000 | 1.000 | 0.200 | 11.6 | BridgeReplacedDuringVersionCheck |  |
| explore-plugin-bridge | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 10.4 |  | callBridge, bridgeFor, BinaryBridge, BridgePool |
| explore-semantic-search | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 13.2 |  | semanticTools, handle_semantic_search, SemanticIndex, SearchIndex |
| explore-background-bash | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 17.4 |  | BgTaskRegistry, PersistedTask, replay_session, write_task |
| explore-lsp-diagnostics | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 11.5 |  | LspManager, wait_for_diagnostics, pull_file_diagnostics, PostEditWaitOutcome |
| explore-bridge-search | findRelevantContext | FAIL | 0.250 | 1.000 | 0.200 | 12.5 | BinaryBridge | send, semanticTools, handle_semantic_search |
| explore-import-organization | findRelevantContext | FAIL | 0.000 | 0.000 | 0.000 | 21.0 |  | handle_organize_imports, parse_imports, classify_group, generate_import_line |

