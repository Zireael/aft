# Patch summary

Apply the script in this archive to produce the effective diff below.

## crates/aft/src/config.rs

Inside `impl Default for SemanticBackendConfig`, after:

```rust
rerank_max_candidate_chars: 2500,
```

insert:

```rust
rerank_api_type: RerankApiType::Chat,
rerank_max_candidate_chars_cross_encoder: 512,
```

After:

```rust
max_files: 20_000,
```

insert:

```rust
max_embed_tokens: 512,
chunk_overlap_tokens: 100,
```

## crates/aft/src/semantic_index.rs

For every explicit `SemanticBackendConfig { ... }` literal without `..` spread syntax, after:

```rust
rerank_max_candidate_chars: 2500,
```

insert:

```rust
rerank_api_type: crate::config::RerankApiType::Chat,
rerank_max_candidate_chars_cross_encoder: 512,
```

After:

```rust
max_files: 20_000,
```

insert:

```rust
max_embed_tokens: 512,
chunk_overlap_tokens: 100,
```

For the `make_model2vec_config` helper, also add `max_files: 20_000,` before `max_embed_tokens` if absent.
