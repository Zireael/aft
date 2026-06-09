# AFT vs CodeGraph agent benchmark

- Model: `opencode-go/deepseek-v4-flash-free`
- Fallback model: `opencode-go/deepseek-v4-pro`
- Corpus: `agent-fixture`
- Timestamp: 2026-05-27T03:53:18.319Z
- Tasks: 2
- Dry run: true

## Summary

| arm | runs | successes | success rate | tokens total | median tokens | median wall ms | p95 wall ms | median tool calls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| aft | 2 | 2 | 1.000 | 0 | 0 | 0 | 0 | 0 |
| codegraph | 2 | 2 | 1.000 | 0 | 0 | 0 | 0 | 0 |

## Per task

| task | arm | kind | status | model | wall ms | tokens | tool calls |
| --- | --- | --- | --- | --- | ---: | ---: | ---: |
| answer-loyalty-discount | aft | answer | PASS | opencode-go/deepseek-v4-flash-free | 0 | 0 | 0 |
| answer-loyalty-discount | codegraph | answer | PASS | opencode-go/deepseek-v4-flash-free | 0 | 0 | 0 |
| answer-payment-caller | aft | answer | PASS | opencode-go/deepseek-v4-flash-free | 0 | 0 | 0 |
| answer-payment-caller | codegraph | answer | PASS | opencode-go/deepseek-v4-flash-free | 0 | 0 | 0 |
