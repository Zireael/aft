# Bead Review Report: aft-znr (Embedding input exceeds context size — no chunking)

**Reviewer:** Hephaestus via `the-fool` (Find the Failure Modes mode)  
**Date:** 2026-06-10  
**Bead Type:** bug  
**Verdict:** ACCEPTABLE with significant gaps — chunking strategy is underspecified and token counting approach is flawed.

---

## Steelmanned Thesis

Large symbols (e.g., Pydantic's BaseModel at 4650 tokens) exceed the embedding model's context window (2048 tokens for CodeRankEmbed), causing HTTP 400 errors and silent symbol drops from the semantic index. AFT should chunk large symbols at natural boundaries, embed each chunk separately, and deduplicate at search time.

---

## Code Verification

**Confirmed from source:**
- `local_embed.rs:40`: `MINILM_MAX_LENGTH = 512` — local ONNX embedder already truncates via tokenizer
- `semantic_index.rs:1402-1466`: `send_embedding_request()` sends raw text to remote backend with retry logic but no pre-flight token counting
- The error in the transcript: `"input (4650 tokens) is larger than the max context size (2048 tokens). skipping"` — confirms remote backend rejects oversized input

**Critical finding:** The local embedder path ALREADY handles truncation. The bug is specifically in the **remote embedding path** (OpenAI-compatible, Ollama, etc.). The bead doesn't distinguish these paths.

---

## Challenges / Failure Modes Found

### 1. Token Counting Strategy is Flawed (P0)

**The bead proposes:** "use `aft-tokenizer` crate" to count tokens before embedding

**Why this fails:**
- `aft-tokenizer` is a Claude-specific tokenizer (claude.rs). It counts tokens for Anthropic's Claude models, NOT for the embedding model being used
- CodeRankEmbed uses its own tokenizer (likely a SentencePiece or BPE variant)
- Using the wrong tokenizer will produce incorrect token counts — we might chunk at 2048 "Claude tokens" when the embedding model actually sees 1800 or 2400 tokens
- The only reliable way to count tokens for an arbitrary embedding model is to use THAT model's tokenizer

**Mitigation:**
- For **local embedders** (`local_embed.rs`): The tokenizer is already loaded (`tokenizers::Tokenizer`). Use it to count tokens before embedding — this is exact
- For **remote embedders**: There's no way to know the remote model's tokenizer without downloading it. Options:
  a. Use a heuristic: `chars / 4` as approximate token count (English avg ~4 chars/token, code ~3-5)
  b. Use tiktoken if model is OpenAI (text-embedding-3-*)
  c. Add a `tokenizer_name` config field so users can specify which tokenizer to use
  d. **Best approach:** Chunk by character count with a safety margin. If `max_embed_tokens = 2048`, chunk at ~1500 tokens worth of characters (e.g., 6000 chars) to leave headroom

---

### 2. Local Embedder Path Doesn't Need This Fix (P1)

**Current behavior:** `local_embed.rs` uses `tokenizers::Tokenizer` with `max_length=512` and `truncation=true`. Long inputs are silently truncated, not rejected.

**Why this matters:**
- The bead's implementation plan would add chunking logic to ALL embedding paths, including local
- Local embedding already works fine (just with truncation). Adding chunking to local would:
  - Increase complexity
  - Slow down indexing (more embedding calls)
  - Change behavior for existing local-embedding users

**Mitigation:**
- Scope the fix to **remote embedding backends only** (`OpenAiCompatible`, `Ollama`, `Perplexity`)
- Local backends (`Fastembed`, `Model2Vec`) should continue using truncation as before
- Add a backend check: only chunk when `backend != Fastembed && backend != Model2Vec`

---

### 3. Chunk Overlap Strategy is Missing (P1)

**The bead says:** "Split at natural boundaries: blank lines, function boundaries, character offsets"

**Why this is incomplete:**
- Splitting at boundaries without overlap can lose context at the boundary
- Example: A function signature at the end of chunk 1 and its body at the start of chunk 2 — the signature context is lost for the body chunk
- Standard chunking strategies use overlap (e.g., 10-20% of chunk size) to preserve boundary context
- The bead mentions "Chunks overlap at boundaries" as an edge case but the implementation plan doesn't include overlap

**Mitigation:**
- Add overlap to the chunking strategy: each chunk includes the last N lines of the previous chunk (where N = ~10% of max chunk size)
- Or: use a sliding window approach where chunks overlap by a fixed character count
- Document the overlap strategy and its tradeoff (more vectors = larger index, but better boundary coverage)

---

### 4. Search-Time Deduplication is Under-Specified (P1)

**The bead says:** "At search time, deduplicate chunks per symbol (keep highest-scoring chunk per symbol)"

**Why this is vague:**
- The semantic index stores vectors keyed by... what? File path? Symbol ID? Chunk ID?
- Currently, the index likely stores one vector per symbol. Adding chunks means either:
  a. Store multiple vectors per symbol (same key, multiple vectors) — requires index structure changes
  b. Store chunks as separate entries with a parent symbol reference — requires adding a `parent_symbol` field
- The bead doesn't specify which storage approach to use
- Deduplication at search time requires the index to support "group by symbol, take max score" — this is a non-trivial query change

**Mitigation:**
- Specify the storage approach: add `chunk_index: Option<usize>` and `total_chunks: Option<usize>` to the vector metadata
- At search time, group results by `(file, name, kind)` and keep the highest-scoring chunk per group
- Update the vector store schema to support chunk metadata

---

### 5. Missing: What Happens to Symbol Metadata? (P2)

**Current behavior:** Each `HybridResult` contains `file`, `name`, `kind`, `start_line`, `end_line`, `snippet`. These map to a single symbol.

**After chunking:**
- A symbol split into 3 chunks will produce 3 vectors
- Each chunk needs its own `start_line` and `end_line` (subset of the original symbol)
- The `snippet` field should contain the chunk text, not the full symbol text
- The bead doesn't mention updating symbol metadata for chunks

**Mitigation:**
- Add explicit implementation step: "Update symbol metadata (start_line, end_line, snippet) for each chunk to reflect chunk boundaries"
- Ensure `HybridResult` construction uses chunk-specific metadata, not original symbol metadata

---

### 6. Missing: How Does Chunking Affect `max_results_per_file`? (P2)

**Current config:** `max_results_per_file: 2` (default) prevents a single file from dominating results.

**After chunking:**
- A single large symbol in a file might produce 5 chunks
- All 5 chunks could score highly for the same query
- `max_results_per_file` would cap them to 2, but deduplication should happen AFTER the per-file cap
- The bead doesn't mention this interaction

**Mitigation:**
- Document that `max_results_per_file` applies AFTER deduplication (i.e., 2 unique symbols per file, not 2 chunks)
- Or: apply deduplication BEFORE the per-file cap so chunks from the same symbol count as one result

---

### 7. Missing: Config Default Should Be Model-Specific (P2)

**The bead says:** "Add `max_embed_tokens` config field (default 2048)"

**Why this is wrong:**
- Different models have different context windows:
  - CodeRankEmbed: 2048 tokens
  - text-embedding-3-small: 8191 tokens
  - text-embedding-3-large: 8191 tokens
  - all-MiniLM-L6-v2 (local): 512 tokens (but uses truncation, not chunking)
  - nomic-embed-text: 8192 tokens
- A single default of 2048 is too conservative for some models and too aggressive for others

**Mitigation:**
- Add per-model defaults in `EmbeddingModelProfile` (already exists in `semantic_index.rs`)
- Allow `max_embed_tokens` to override the model default
- For unknown models, default to 2048 as a safe fallback

---

### 8. Missing: Benchmark Should Verify Recall Improvement, Not Just Absence of Errors (P2)

**The bead says:** "No HTTP 400 errors when indexing Pydantic repository"

**Why this is insufficient:**
- Absence of errors doesn't mean the large symbol is actually searchable
- If chunking produces poor-quality chunks (e.g., splitting in the middle of a function name), the symbol might still be unfindable
- The acceptance criteria should include: "Search queries for chunked symbols return relevant results"

**Mitigation:**
- Add acceptance criterion: "Benchmark recall for queries matching large symbols (e.g., 'pydantic BaseModel validation') is ≥ baseline"
- Add test: "Chunked symbol can be found by search queries targeting different parts of the symbol"

---

## Synthesis

The bead correctly identifies the problem but underestimates the complexity of token counting and index storage changes. Key gaps:

1. **Token counting** — `aft-tokenizer` is the wrong tool. Need model-specific tokenizers or character heuristics
2. **Scope** — Local embedders already handle truncation; fix should target remote backends only
3. **Chunk overlap** — Missing from implementation plan but critical for boundary context
4. **Storage schema** — Need to specify how chunks are stored and how deduplication works at query time
5. **Model-specific defaults** — `max_embed_tokens` should vary by model

**Recommendation:** Before implementing:
- Replace `aft-tokenizer` with either: (a) local tokenizer for local backends, (b) character heuristic for remote backends
- Limit chunking to remote backends
- Add chunk overlap to the plan
- Specify vector store schema changes for chunk metadata
- Add per-model default token limits

---

## Edge Cases Not Covered in Bead

| Edge Case | Severity | Notes |
|---|---|---|
| Wrong tokenizer produces incorrect chunk boundaries | P0 | `aft-tokenizer` is Claude-specific, not model-specific |
| Local backend behavior changes | P1 | Local embedders already truncate; chunking would change behavior |
| Chunk loses semantic meaning at boundary | P1 | No overlap strategy means context is lost at splits |
| Symbol with no natural boundaries (e.g., minified JS) | P2 | Fallback to fixed character offset — needs testing |
| Recursive chunking: child chunk still too large | P2 | Bead mentions "recursively split" but no termination condition |
| Index size explosion: 1 symbol → N chunks | P2 | Large codebase with many large symbols could 10x index size |
| `max_results_per_file` interaction with chunks | P2 | Needs explicit handling |
| Search query matches multiple chunks of same symbol | P2 | Deduplication must handle this, but implementation unspecified |
| Tokenizer unavailable for remote model | P2 | Need fallback to character heuristic |
| CodeRankEmbed query prefix not applied to chunks | P3 | The model card says queries need prefix — chunks might too |

---

## Overall Assessment

| Dimension | Score | Rationale |
|---|---|---|
| Completeness | 5/10 | Core problem identified, but token counting, storage schema, and scope are wrong |
| Coherence | 6/10 | Implementation plan is logical but relies on wrong tokenizer crate |
| Appropriate Staging | 7/10 | Good sequence, but needs model-specific token limits added first |
| Scope Appropriateness | 6/10 | Should exclude local backends; overlaps with index storage schema |
| Edge Case Coverage | 5/10 | Missing overlap, recursive termination, index size concerns |

**Final Verdict:** REVISE before execution. The bead captures the right problem but the solution sketch needs significant refinement on token counting and storage schema. An agent implementing this as-is would use the wrong tokenizer and potentially break local embedding behavior.
