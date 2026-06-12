# AFT SemanticBackendConfig compile fix

This archive contains a conservative patch script for `Zireael/aft` branch `semantic-search-enhancement`.

## Apply

From the repository root:

```bash
python /path/to/apply_semantic_backend_config_fix.py
```

Or copy it into the repo and run:

```bash
python scripts/apply_semantic_backend_config_fix.py
```

## What it changes

`crates/aft/src/semantic_index.rs`

- Patches explicit `SemanticBackendConfig { ... }` literals that do not use struct update syntax.
- Skips `..Default::default()`, `..SemanticBackendConfig::default()`, and `..config_int8` literals.
- Adds:
  - `rerank_api_type: crate::config::RerankApiType::Chat,`
  - `rerank_max_candidate_chars_cross_encoder: 512,`
  - `max_embed_tokens: 512,`
  - `chunk_overlap_tokens: 100,`
- Also adds `max_files: 20_000,` to the model2vec helper literal if still missing.

`crates/aft/src/config.rs`

- Patches `impl Default for SemanticBackendConfig` if the branch still lacks the new default fields.
- Adds:
  - `rerank_api_type: RerankApiType::Chat,`
  - `rerank_max_candidate_chars_cross_encoder: 512,`
  - `max_embed_tokens: 512,`
  - `chunk_overlap_tokens: 100,`

## Verify

```bash
grep -n "rerank_api_type\|rerank_max_candidate_chars_cross_encoder\|max_embed_tokens\|chunk_overlap_tokens" crates/aft/src/semantic_index.rs crates/aft/src/config.rs
bash scripts/zir-aft-check.sh quick --keep-going
```

Suggested commit:

```bash
git add crates/aft/src/config.rs crates/aft/src/semantic_index.rs
git commit -m "fix(semantic): add missing SemanticBackendConfig fields"
```
