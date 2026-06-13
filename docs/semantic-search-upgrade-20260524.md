You are an expert Rust coding agent working on the AFT repository:
https://github.com/cortexkit/aft

Task:
Refactor AFT’s semantic search implementation to support a two-stage embedding + reranking pipeline, while preserving backward compatibility with the existing semantic search behavior.

Current known behavior:
- AFT has semantic search using cAST-style symbol chunking.
- AFT currently supports embedding backends: fastembed, openai_compatible, and ollama.
- The default embedding backend is fastembed with all-MiniLM-L6-v2.
- Existing semantic search computes query embeddings, compares them with stored chunk embeddings, optionally fuses lexical results, and returns ranked results.
- AFT does not currently have a first-class reranking pipeline.
- OpenAI-compatible embeddings currently send raw `input` and `model` only.
- Some embedding models, such as OASIS-code-embedding (this is just an example of a model used in this workflow, however users may set in settings a model with different name, but off a similar type. Models will follow openai_compatible or ollama architecture behind the sccenes), benefit from query-side instruction prompts. The default all-MiniLM-L6-v2 should not be forced to use priming prompts unless explicitly configured.

Primary goal:
Implement an optional retrieval pipeline:

query
→ optional query prompt/template
→ embed query
→ semantic retrieval top N
→ optional lexical/hybrid fusion
→ optional reranking top M candidates with a second model
→ return final ranked results
→ expose useful search diagnostics and metrics

Do not break existing users. With default config, AFT should behave the same as before.

Implementation requirements:

1. Add embedding prompt-template support

Add optional fields to the semantic backend config:

- query_prompt_template: Option<String>
- document_prompt_template: Option<String>

Behavior:
- `query_prompt_template` is applied only when embedding user search queries.
- `document_prompt_template` is applied only when embedding indexed code chunks.
- If unset, use raw text exactly as today.
- Template syntax can be minimal: replace `{query}` or `{text}` with the raw input.
- For document chunks, `{text}` should refer to the enriched cAST chunk text currently embedded by AFT.
- Do not apply query prompts to indexed chunks.
- Do not apply document prompts to user queries.
- Include the prompt-template values or a hash of them in the semantic index fingerprint, because changing document prompts changes the vector space and must force a rebuild.
- Query prompt changes may not require rebuilding indexed vectors, but include it in diagnostics so users understand query behavior.

Important model-specific defaults:
- fastembed/all-MiniLM-L6-v2: default query/document prompt templates should remain unset.
- openai_compatible: default templates should remain unset.
- ollama: default templates should remain unset.
- Users can explicitly configure OASIS-style prompting, for example:
  query_prompt_template = "Instruct: Given a code search query, retrieve relevant code snippet that answer the query\nQuery: {query}"

Acceptance tests:
- Existing configs deserialize successfully.
- Existing default config produces raw query embeddings with no prompt.
- Config with query_prompt_template embeds the transformed query.
- Config with document_prompt_template embeds transformed chunk text and changes the index fingerprint.
- Config without document_prompt_template does not trigger unnecessary rebuilds.

2. Add reranking config

Add a new optional config block, probably named `rerank` or `semantic_rerank`.

Suggested shape:

{
  "semantic_search": true,
  "semantic": {
    "backend": "openai_compatible",
    "model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "base_url": "http://127.0.0.1:10001/v1",
    "query_prompt_template": "Instruct: Given a code search query, retrieve relevant code snippet that answer the query\nQuery: {query}",
    "timeout_ms": 60000,
    "max_batch_size": 16,
	"semantic_diagnostics": true
  },
  "rerank": {
    "enabled": true,
    "backend": "openai_compatible_chat",
    "model": "CodeRankLLM.Q4_K_M",
    "base_url": "http://127.0.0.1:10001/v1",
    "api_key_env": null,
    "timeout_ms": 120000,
    "candidate_count": 50,
    "window_size": 10,
    "max_output_tokens": 256,
    "temperature": 0,
    "prompt_template": null
  }
}

Config rules:
- Reranking is disabled by default.
- Reranker config must be user-level only for network/base_url/api_key fields, following AFT’s existing trust-boundary model for embedding backends.
- Project-level config may tune safe parameters such as candidate_count/window_size only if this matches existing AFT security policy.
- Validate base_url using the same SSRF policy used for embedding backends.
- Do not store API keys in config or logs.

Supported reranker MVP:
- Implement OpenAI-compatible chat/completions first.
- Use a deterministic listwise reranking prompt.
- The reranker should receive:
  - original query
  - candidate ID
  - file path
  - symbol name
  - symbol kind
  - line range
  - existing semantic/hybrid score
  - snippet/code excerpt
- It should return only a JSON array of candidate IDs in ranked order.
- Parse the response robustly:
  - accept a bare JSON array
  - tolerate markdown fences if necessary
  - ignore unknown IDs
  - append omitted candidates after returned IDs in original order
  - on parse failure, fall back to pre-rerank ordering and emit diagnostics

Suggested default reranker prompt:

You are a code search reranker.
Given a search query and candidate code snippets, rank the candidates by relevance.
Prefer candidates that directly implement, define, configure, or call the behavior requested by the query.
Return only a JSON array of candidate IDs from most relevant to least relevant.

Query:
{query}

Candidates:
{candidates}

Return only JSON.

Reranking flow:
- First-stage retrieval should overfetch candidates using candidate_count.
- If reranking is enabled:
  - retrieve candidate_count results
  - rerank in windows of window_size
  - return topK final results
- Keep original semantic/hybrid/lexical score fields.
- Add rerank_position and rerank_source fields if the public result type can support them without breaking clients.
- If result schema compatibility is strict, put rerank diagnostics under metadata instead of altering required fields.

Recommended defaults:
- candidate_count: 50
- window_size: 10
- timeout_ms: 120000
- temperature: 0
- max_output_tokens: 256

Acceptance tests:
- Reranking disabled preserves existing ordering.
- Reranking enabled reorders candidates according to a mocked reranker response.
- Invalid reranker JSON falls back cleanly.
- Missing candidate IDs are appended.
- Unknown candidate IDs are ignored.
- Timeout/failure does not fail the entire search unless config explicitly requests strict mode.

3. Add search pipeline metrics

Add lightweight metrics collection around semantic search.

Track per-query metrics:
- query string hash, not raw query, unless verbose debug logging is explicitly enabled
- timestamp
- total query latency_ms
- query_embedding_latency_ms
- lexical_latency_ms
- semantic_search_latency_ms
- hybrid_fusion_latency_ms
- rerank_latency_ms
- final_result_count
- semantic_candidate_count
- lexical_candidate_count
- rerank_candidate_count
- embedding_backend
- embedding_model
- embedding_dimension
- rerank_enabled
- rerank_backend
- rerank_model
- query_embedding_cache_hit
- score_min
- score_median
- score_max
- score_mean
- top1_score
- topK_score_spread
- source_counts: semantic / lexical / hybrid / reranked
- index_status: ready / building / empty / stale / unavailable
- index_entry_count
- chunking_version
- prompt_template_active: query/document booleans

Track aggregate in-memory metrics:
- rolling query count
- rolling p50/p95/p99 latency
- rolling p50/p95 top1 score
- rolling median result count
- reranker failure rate
- embedding failure rate
- query embedding cache hit rate
- percentage of queries with zero results
- percentage of queries with very low top1 score

Add thresholds for warning diagnostics:
- zero results
- top1 semantic score below configurable warning threshold
- median score below configurable warning threshold
- reranker failure rate above threshold
- embedding backend timeout/failure
- index empty/building/stale
- suspiciously low semantic score distribution across many queries

Do not overclaim “model quality” from scores alone. These are heuristics. The warning should say the pipeline may be misconfigured, not that the model is definitively bad.

Suggested warning:
"Semantic search returned low-confidence matches for recent queries. This may indicate an embedding/model mismatch, missing query prompt, stale index, poor chunking, or an unsuitable embedding model."

4. Expose diagnostics in aft_search response

Enhance `aft_search` response with optional diagnostics metadata while keeping current human-readable output stable.

Suggested metadata:
{
  "diagnostics": {
    "pipeline": "semantic" | "hybrid" | "semantic_rerank" | "hybrid_rerank",
    "query_latency_ms": 123,
    "embedding_latency_ms": 20,
    "rerank_latency_ms": 80,
    "matched_chunks": 50,
    "returned_results": 10,
    "score_min": 0.31,
    "score_median": 0.48,
    "score_max": 0.71,
    "top1_score": 0.71,
    "semantic_backend": "openai_compatible",
    "semantic_model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "rerank_enabled": true,
    "rerank_model": "CodeRankLLM.Q4_K_M",
    "query_prompt_active": true,
    "document_prompt_active": false,
    "warnings": []
  }
}

Human-readable output should include a compact one-line footer, for example:
Found 10 result(s). [index: ready] [pipeline: hybrid+rerank] [latency: 143ms] [chunks: 50→10] [score: min 0.31 / med 0.48 / max 0.71]

5. Add TUI/status integration

Find the existing TUI/status component that displays AFT status, semantic index state, or sidebar metadata.

Add a compact semantic search diagnostics panel or status line showing:
- semantic index status
- embedding backend/model
- index entry count
- last query latency
- last query matched chunks
- last query score min/median/max
- rerank enabled/disabled
- reranker model if enabled
- rerank latency
- recent warning if low-confidence results are detected

Avoid noisy UI. Use one-line summary by default and expandable details if the TUI supports it.

Suggested TUI lines:
Semantic: ready · Rerank: on
OASIS-code-embedding · CodeRankLLM.Q4_K_M
18,420 chunks · last 142ms
Score max/med/min: 0.72/0.49/0.31 

If reranking failed:
Semantic: ready · rerank failed, fallback used · last 96ms · score max/med/min 0.61/0.38/0.22

6. Add config documentation

Update README/config docs to describe:
- query_prompt_template
- document_prompt_template
- why most models should leave prompts unset
- why instruction-tuned embedding models may require query prompts
- rerank config
- performance implications
- security boundaries
- how changing document_prompt_template triggers index rebuild
- how to interpret metrics

Add example configs:

A. Default fastembed:
{
  "semantic_search": true
}

B. OASIS embedding only:
{
  "semantic_search": true,
  "semantic": {
    "backend": "openai_compatible",
    "model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "base_url": "http://127.0.0.1:10001/v1",
    "query_prompt_template": "Instruct: Given a code search query, retrieve relevant code snippet that answer the query\nQuery: {query}",
    "timeout_ms": 60000,
    "max_batch_size": 16
  }
}

C. OASIS + CodeRankLLM:
{
  "semantic_search": true,
  "semantic": {
    "backend": "openai_compatible",
    "model": "OASIS-code-embedding-1.5B.i1-Q4_K_M",
    "base_url": "http://127.0.0.1:10001/v1",
    "query_prompt_template": "Instruct: Given a code search query, retrieve relevant code snippets that answer the query\nQuery: {query}",
    "timeout_ms": 60000,
    "max_batch_size": 16,
	"semantic_diagnostics": true
  },
  "rerank": {
    "enabled": true,
    "backend": "openai_compatible_chat",
    "model": "CodeRankLLM.Q4_K_M",
    "base_url": "http://127.0.0.1:10001/v1",
    "candidate_count": 50,
    "window_size": 10,
    "temperature": 0,
    "timeout_ms": 120000
  }
}

7. Add tests

Add unit tests for:
- config parsing with missing rerank block
- config parsing with rerank block
- query prompt application
- document prompt application
- prompt template validation
- semantic fingerprint change when document prompt changes
- no semantic fingerprint change when only query prompt changes, unless the existing design chooses otherwise
- reranker JSON parsing
- reranker fallback behavior
- metrics summary calculation: min/median/max/mean
- zero-result diagnostics
- low-score diagnostics

Add integration tests with mocked HTTP servers:
- OpenAI-compatible embedding endpoint receives prompted query
- OpenAI-compatible embedding endpoint receives prompted document chunks only when configured
- reranker endpoint receives candidate list
- reranker ordering changes final output
- reranker failure falls back to original result order

8. Compatibility and safety constraints

Do not:
- hardcode OASIS behavior globally
- hardcode CodeRankLLM globally
- force prompts on all models
- break fastembed default behavior
- send raw queries or code snippets to logs unless debug mode is explicitly enabled
- allow project config to redirect reranker or embedding endpoints to unsafe URLs
- make reranker failure break search by default
- overwrite semantic scores with reranker scores unless the reranker actually produces calibrated numeric scores, which CodeRankLLM likely does not

Do:
- preserve current behavior by default
- make all new behavior opt-in
- keep security model consistent with existing embedding config
- keep diagnostics useful but compact
- make reranker failures visible
- keep original first-stage scores for debugging
- include metrics in a form that helps identify poor retrieval, stale indexes, bad prompt templates, and model/backend mismatch

9. Suggested implementation sequence

Step 1:
Inspect current semantic search files:
- config.rs
- semantic_index.rs
- aft_search command implementation
- status/TUI files
- tests around semantic search and config

Step 2:
Add config structs and serde defaults.

Step 3:
Refactor embedding model methods to separate:
- embed_documents(...)
- embed_query(...)
- apply_query_template(...)
- apply_document_template(...)

Step 4:
Update semantic index fingerprint to include document prompt template identity.

Step 5:
Add SearchDiagnostics/SearchMetrics structs.

Step 6:
Instrument existing semantic/hybrid search path without reranking.

Step 7:
Implement reranker client behind a trait:
- trait Reranker { fn rerank(&self, query, candidates) -> Result<RerankOutput, RerankError>; }

Step 8:
Add OpenAI-compatible chat reranker implementation.

Step 9:
Integrate reranking after first-stage retrieval and before final truncation to topK.

Step 10:
Update TUI/status output.

Step 11:
Add docs and examples.

Step 12:
Run:
- cargo fmt
- cargo clippy
- cargo test
- targeted semantic search tests
- manual test with default fastembed
- manual test with openai_compatible mock
- manual test with local llama-swap OASIS + CodeRankLLM if available

## Model2vec fixture contract

Model2vec tests use a tiny inline tokenizer fixture built by
`build_tokenizer_json()` in `crates/aft/src/semantic_index.rs` (test module).

Key constraints:
- `"padding": null` — partial padding is invalid; use null when padding is not needed.
- `"pre_tokenizer": { "type": "Whitespace" }` — ensures "hello world" tokenizes as separate tokens.
- `"ignore_merges": true` — allows whole-token vocab hits even with empty merges.
- Root-level `"strategy"` is NOT a valid tokenizer JSON field.
- All root fields must be present: version, truncation, padding, added_tokens, normalizer, pre_tokenizer, post_processor, decoder, and model.
- `model2vec-rs` 0.2.1 resolves `tokenizers` 0.21.4 (transitive), not AFT's direct 0.22.2.

Do not "simplify" the fixture by removing fields or adding partial padding.

10. Definition of done

The patch is complete when:
- Existing default AFT semantic search still works unchanged.
- Users can configure OASIS query prompting without patching source code.
- Users can enable a second reranker model through config.
- Reranking reorders first-stage candidates and falls back safely on failure.
- Search responses expose useful diagnostics.
- TUI/status shows semantic pipeline health.
- Metrics make it obvious when most queries produce zero or very low-confidence matches.
- Tests cover config, prompt templates, reranker parsing, fallback, and metrics.
- Documentation includes fastembed default, OASIS embedding-only, and OASIS + CodeRankLLM examples.

Be conservative. This is infrastructure code used by AI agents. Prefer boring, typed, testable changes over clever abstractions.