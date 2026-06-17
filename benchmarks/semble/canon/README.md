# Canon file usage

Recommended integration:

1. Load `repos.json`.
2. Load one suite file at a time.
3. Check repo revision and path existence.
4. Initialize benchmark modes by suite.
5. Emit one attempt row per `(query, mode)`.
6. Aggregate by `(suite, mode)`, never by mode alone.

Recommended suite-to-mode mapping is in `mode-matrix.json`.

## Relevant entry semantics

- `relevant`: primary expected files/symbols.
- `secondary`: useful supporting files; may count with lower grade or be used for diagnostics only.
- `confidence`: initial confidence for review prioritization.
- `review_status`: `seed` until a human/model validates the row against a pinned checkout.

## Runtime validation

Runtime validation may fail fast on missing paths. It must not rewrite relevance fields.
