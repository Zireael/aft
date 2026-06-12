# Meta-Prompt: Fix SemanticBackendConfig Struct Literal Compilation Errors

## Problem Summary

4 new fields were added to `SemanticBackendConfig` in `crates/aft/src/config.rs`:
- `rerank_api_type: RerankApiType` (default: `Chat`)
- `rerank_max_candidate_chars_cross_encoder: usize` (default: `512`)
- `max_embed_tokens: usize` (default: `512`)
- `chunk_overlap_tokens: usize` (default: `100`)

These fields are present in the struct definition and the `Default` impl, but 12+ test struct literals in `crates/aft/src/semantic_index.rs` list all fields explicitly without using `..Default::default()`. The Rust compiler requires ALL fields to be present in struct literals unless `..Default::default()` is used.

## Files Requiring Changes

1. **`crates/aft/src/config.rs`** — ALREADY FIXED (Default impl updated in a prior commit)
2. **`crates/aft/src/semantic_index.rs`** — NEEDS FIXING (12 test struct literals missing new fields)
3. **`crates/aft/src/semantic_rerank.rs`** — ALREADY OK (all 8 test structs use `..Default::default()`)
4. **`crates/aft/src/commands/configure.rs`** — ALREADY OK (test uses `..SemanticBackendConfig::default()`)
5. **`crates/aft/src/commands/semantic_search.rs`** — ALREADY OK (test uses `..SemanticBackendConfig::default()`)

## Correct Fix Strategy

### Step 1: Identify Target Struct Literals

In `crates/aft/src/semantic_index.rs`, find all `SemanticBackendConfig {` blocks that are ACTUAL struct literals (not function signatures like `-> SemanticBackendConfig {`).

**How to identify struct literals vs function signatures:**
- Struct literals appear after `=` or as function return values (without `->` on the same or preceding line)
- Function signatures have `fn` keyword and `->` before the type
- Check if the block ends with `..Default::default()` or `..SemanticBackendConfig::default()` — if so, skip

Use grep + context to find candidates:
```bash
grep -n "SemanticBackendConfig {" crates/aft/src/semantic_index.rs
```

Then check each match:
- If line contains `fn` or is preceded by `->` on the same or prior line → FUNCTION SIGNATURE, skip
- If block contains `..Default::default()` or `..SemanticBackendConfig::default()` → skip
- Otherwise → NEEDS FIXING

### Step 2: Add Missing Fields to Each Target

For each struct literal that needs fixing, add the 4 fields BEFORE the closing `}`:

```rust
// Before:
SemanticBackendConfig {
    backend: SemanticBackend::OpenAiCompatible,
    model: "test-embedding".to_string(),
    // ... other fields ...
    max_files: 20_000,
}

// After:
SemanticBackendConfig {
    backend: SemanticBackend::OpenAiCompatible,
    model: "test-embedding".to_string(),
    // ... other fields ...
    max_files: 20_000,
    rerank_api_type: crate::config::RerankApiType::Chat,
    rerank_max_candidate_chars_cross_encoder: 512,
    max_embed_tokens: 512,
    chunk_overlap_tokens: 100,
}
```

**Important rules:**
1. Use `crate::config::RerankApiType::Chat` — NOT just `RerankApiType::Chat`
2. Indent with the same level as the last existing field
3. Add a trailing comma after each new field
4. Place fields BEFORE the closing `}`, AFTER the last existing field

### Step 3: Verify No Syntax Errors

After editing, verify each modified block:
1. No fields appear AFTER `..Default::default()` (this is a Rust syntax error)
2. The closing `}` is still in the correct position
3. No extra or missing commas

Use grep to spot-check:
```bash
grep -n "rerank_api_type\|Default::default()" crates/aft/src/semantic_index.rs | head -30
```

### Step 4: Commit

```bash
git add crates/aft/src/semantic_index.rs
git commit -m "fix(semantic): add missing fields to test struct literals

The SemanticBackendConfig struct gained 4 new fields in the recent
reranker and chunking bead implementations. Test struct literals that
list all fields explicitly (without ..Default::default()) need these
fields added to compile.

Fields added:
- rerank_api_type: Chat (default)
- rerank_max_candidate_chars_cross_encoder: 512
- max_embed_tokens: 512
- chunk_overlap_tokens: 100"
```

## Common Mistakes to Avoid

1. **Matching function signatures**: Do NOT modify `-> SemanticBackendConfig {` in function signatures
2. **Inserting after `..Default::default()`**: In Rust, `..Default::default()` must be LAST in the struct literal. Never add fields after it
3. **Line ending corruption**: When using Python scripts, read with `newline=''` and write with `newline=''` to preserve CRLF/LF
4. **Missing indent**: Match the indentation of existing fields (typically 12 spaces inside `mod tests`)
5. **Using wrong module path**: Use `crate::config::RerankApiType::Chat` because tests are inside `#[cfg(test)] mod tests`

## Verification Commands

```bash
# Check which structs need fixing (look for ones without Default spread)
grep -B2 -A2 "SemanticBackendConfig {" crates/aft/src/semantic_index.rs | grep -v "Default::default"

# Verify the diff is minimal and correct
git diff --ignore-all-space crates/aft/src/semantic_index.rs | head -50

# If Docker is available, compile check
bash scripts/zir-aft-check.sh quick --keep-going
```
