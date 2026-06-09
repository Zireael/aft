# Semble Pilot Corpus Selection (aft-t6p.38.1)

## Selection Criteria

1. **Language diversity** — cover Rust, JavaScript, Python, Go to stress-test AFT's parser coverage
2. **Symbol density** — repos with well-defined public APIs (traits, structs, classes) for symbol-type queries
3. **Codebase size** — small enough to clone and index locally (<50MB working tree)
4. **Annotation quality** — Semble provides 10+ annotations per repo with good category mix

## Selected Repos

| Repo | Language | Queries | Symbol | Semantic | Architecture |
|------|----------|---------|--------|----------|--------------|
| axum | Rust | 10 | 3 | 3 | 4 |
| express | JavaScript | 10 | 3 | 3 | 4 |
| pydantic | Python | 10 | 5 | 2 | 3 |
| serde | Rust | 10 | 4 | 0 | 6 |
| gin | Go | 10 | 3 | 4 | 3 |

**Total:** 50 queries across 5 repos, 4 languages

## Rationale

- **axum** — Rust web framework, relevant to AFT's own Rust codebase. Rich trait/API surface (Handler, FromRequest, IntoResponse).
- **express** — JavaScript web framework, tests AFT's JS/TS parser path. Well-known API surface.
- **pydantic** — Python data validation, strong typing focus aligns with AFT's semantic search use case. High symbol density (BaseModel, Field, field_validator).
- **serde** — Rust serialization, tests AFT's derive-macro-heavy Rust parsing. Core Rust ecosystem.
- **gin** — Go web framework, covers Go's unique patterns (radix tree routing, context lifecycle).

## Excluded from Pilot

- Large repos (rails, laravel-framework, zig) — too slow to clone/index locally
- Niche languages (elixir, haskell, scala, zig) — lower priority for initial pilot
- Repos with null benchmark_root (nvm, bash-it, zig-clap) — harder to scope

## Files

- `benchmarks/semble/repos-pilot.json` — 5-repo pilot manifest
- `benchmarks/semble/annotations/*.json` — curated annotation files (10 queries each)
- `benchmarks/semble/repos.json` — full 63-repo Semble lockfile (reference)
