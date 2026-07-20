# `semantic_index.rs` Migration Design — Provider-Aware Vectors + Concurrency Modernization Reconciliation

**Date:** 2026-07-20
**File:** `crates/aft/src/semantic_index.rs`
**Conflict:** 47 hunks (the largest conflict in the merge)
**Fork size:** 11,245 lines (V8) | **Upstream size:** 7,306 lines (V7)
**Cross-ref:** [merge-conflict-analysis-upstream-20260720.md](./merge-conflict-analysis-upstream-20260720.md) §3.1
**Beads:** `aft-fts5e2e`, `aft-t6p`, `bd-aft-db`, `aft-ri-v31`

---

## 1. The Divergence — Two Orthogonal Feature Axes

Unlike the DB schema collision (where both sides claimed the same version number), the `semantic_index.rs` divergence is **two orthogonal feature axes** that happen to touch the same code regions:

| Axis | Fork (HEAD, V8) | Upstream (cortexkit/main, V7) |
| :--- | :--- | :--- |
| **Provider awareness** (PR #87) | ✅ `VectorKind`, `TypedVector`, `StoredVector`, `EmbeddingModelProfile`, extended `SemanticIndexFingerprint`, `StorageStrategy`, `DistanceMetric`, model2vec | ❌ None of these exist |
| **Concurrency/sharing** | ❌ No `SharedSemanticBase`, no query timeout, flat struct | ✅ `SharedSemanticBase` with `OnceLock`/`Weak` registry, `QueryBudget`/timeout, `parse_source_with_cached_parser`, cached `norm: f32` in `EmbeddingEntry` |
| **Version** | V8 (file manifest + per-entry `chunk_hash`) | V7 (paths relative to root + content hashes) |
| **Struct shape** | `SemanticIndex { snapshot: Arc<SemanticIndexSnapshot>, lifecycle, fingerprint, ... }` | `SemanticIndex { entries, file_mtimes, file_sizes, file_hashes, dimension, fingerprint, shared_base, ... }` (flat) |

**The fork is AHEAD on version (V8 > V7)** but BEHIND on architectural modernization. The merge must preserve both axes.

---

## 2. Version Number Reconciliation

### 2.1 Current state

| Side | `CURRENT` version | `to_bytes` writes | `from_bytes`/`read_from_disk` accepts |
| :--- | :--- | :--- | :--- |
| Fork | V8 (8) | V8 | V1–V8 |
| Upstream | V7 (7) | V7 | V1–V7 |

### 2.2 Resolution: Keep V8 as the floor, accept V7

The fork's V8 is a superset of upstream's V7 (V8 added `FileRecord` file manifest + per-entry `chunk_hash` on top of V7's relative paths + content hashes). The resolution:

1. **`CURRENT_SCHEMA_VERSION = V8`** (take the fork's higher version)
2. **`to_bytes` writes V8** (fork's behavior)
3. **`from_bytes`/`read_from_disk` accepts V1–V8** (fork's behavior — it already accepts V7)
4. **V7 indexes from upstream DBs** will be accepted and **upgraded to V8 on next write** (the file manifest and chunk_hash will be populated during the next refresh)

This is **not** a collision — V8 is a strict superset of V7. No reconciliation guard is needed (unlike the DB schema migration).

### 2.3 Version history (both sides)

| Version | What it added | Which side has it |
| :--- | :--- | :--- |
| V1–V2 | Initial format | Both |
| V3 | `subsec_nanos` in file-mtime | Both |
| V4 | Rebuild snippets (1-based → 0-based line fix) | Both |
| V5 | File sizes in metadata | Both |
| V6 | Paths relative to `project_root` + content hashes | Both |
| V7 | Fingerprint invalidation fields (`source_vector_kind`, `stored_vector_kind`, `normalization`, `query_prompt_hash`) | **Fork only** (upstream's V7 is just the relative-paths version) |
| V8 | `FileRecord` file manifest + per-entry `chunk_hash` | **Fork only** |

> ⚠️ **Note:** Both sides define V7, but with **different content**. The fork's V7 added fingerprint invalidation fields; upstream's V7 is the relative-paths + content-hashes version. Since the fork's `from_bytes` already accepts upstream's V7 format (it accepts V1–V8), and the fork's V8 builds on V7, upstream V7 indexes will deserialize correctly on the fork. The reverse is not true — upstream's `from_bytes` rejects V8. **The merged version must accept V1–V8.**

---

## 3. Structural Comparison — `SemanticIndex` Struct

### 3.1 Fork's `SemanticIndex` (L2774)

```rust
pub struct SemanticIndex {
    snapshot: Arc<SemanticIndexSnapshot>,
    lifecycle: SemanticIndexLifecycle,
    last_error: Option<String>,
    fingerprint: Option<SemanticIndexFingerprint>,
    deferred_files: HashSet<PathBuf>,
}
```

The fork delegates data storage to `SemanticIndexSnapshot` (L2649):
```rust
pub struct SemanticIndexSnapshot {
    // ... entries, file_manifest, dimension, project_root, next_chunk_id, fingerprint_string
    pub(crate) file_manifest: HashMap<PathBuf, FileRecord>,  // V8 addition
}
```

### 3.2 Upstream's `SemanticIndex` (L1618)

```rust
pub struct SemanticIndex {
    entries: Vec<EmbeddingEntry>,
    file_mtimes: HashMap<PathBuf, SystemTime>,
    file_sizes: HashMap<PathBuf, u64>,
    any_missing_sizes: bool,
    file_hashes: HashMap<PathBuf, blake3::Hash>,
    dimension: usize,
    fingerprint: Option<SemanticIndexFingerprint>,
    project_root: PathBuf,
    deferred_files: HashSet<PathBuf>,
    shared_base: Option<Arc<SharedSemanticBase>>,  // ← upstream-only
    #[cfg(test)]
    removal_retain_passes: usize,
}
```

### 3.3 Key difference: flat vs. snapshot + `SharedSemanticBase`

Upstream keeps data **flat** in `SemanticIndex` and optionally shares it via `Arc<SharedSemanticBase>`. The fork wraps data in `Arc<SemanticIndexSnapshot>` with a `file_manifest`.

**Resolution:** Keep the fork's `SemanticIndexSnapshot` pattern (it's needed for the V8 file manifest and `chunk_hash` tracking) but **add upstream's `shared_base: Option<Arc<SharedSemanticBase>>` field**. The `SharedSemanticBase` registry allows multiple `SemanticIndex` instances for the same project to share the same embedding model connection — a significant memory optimization the fork must adopt.

```rust
// MERGED SemanticIndex
pub struct SemanticIndex {
    snapshot: Arc<SemanticIndexSnapshot>,           // fork
    lifecycle: SemanticIndexLifecycle,               // fork
    last_error: Option<String>,                      // fork
    fingerprint: Option<SemanticIndexFingerprint>,   // fork (extended)
    deferred_files: HashSet<PathBuf>,                // both
    shared_base: Option<Arc<SharedSemanticBase>>,    // upstream (NEW)
}
```

---

## 4. `EmbeddingEntry` — Fork's `chunk_hash` vs Upstream's `norm`

### 4.1 Current definitions

| Field | Fork | Upstream |
| :--- | :--- | :--- |
| `chunk` | `pub(crate) SemanticChunk` | `SemanticChunk` (private) |
| `vector` | `pub(crate) Vec<f32>` | `Vec<f32>` (private) |
| `chunk_hash` | `pub(crate) String` ✅ | ❌ absent |
| `norm` | ❌ absent | `f32` (cached L2 norm) ✅ |

### 4.2 Resolution: Keep BOTH fields

The fork's `chunk_hash` (V8 per-entry hash for staleness tracing) and upstream's `norm` (cached L2 norm for search optimization) serve **different purposes** and do not conflict:

```rust
// MERGED EmbeddingEntry
pub struct EmbeddingEntry {
    pub(crate) chunk: SemanticChunk,
    pub(crate) vector: Vec<f32>,
    pub(crate) chunk_hash: String,    // fork: V8 per-entry hash
    norm: f32,                         // upstream: cached L2 norm for search
}
```

**Serialization impact:** The `norm` field must be included in `to_bytes`/`from_bytes`. If it's `#[serde(default)]` or computed on load, V7 indexes (from upstream) that lack it will get a default/computed value. The `chunk_hash` is already handled by V8 serialization.

**Search path:** Upstream's search uses `entry.norm` in the cosine similarity denominator (`let denom = query_norm * entry.norm`). The fork must adopt this optimization — currently the fork may recompute norms per query.

---

## 5. `SemanticIndexFingerprint` — Fork's Extended Invalidation

### 5.1 Current definitions

**Upstream (simple, 5 fields):**
```rust
pub struct SemanticIndexFingerprint {
    pub backend: String,
    pub model: String,
    pub base_url: String,
    pub dimension: usize,
    pub chunking_version: u32,
}
```

**Fork (extended, 15+ fields):**
```rust
pub struct SemanticIndexFingerprint {
    // upstream fields
    pub backend: String,
    pub model: String,
    pub base_url: String,
    pub dimension: usize,
    pub chunking_version: u32,
    // PR #87 provider-aware additions
    pub output_encoding: ...,         // dense_f32, base64_int8, base64_binary
    pub storage_strategy: ...,        // NativeF32, DecodeNormalizeF32, BinaryPacked
    pub distance_metric: ...,         // cosine, hamming (default: auto)
    pub input_mode: ...,              // passage, contextualized
    pub source_vector_kind: ...,      // DenseF32, DenseInt8, BinaryPacked
    pub stored_vector_kind: ...,      // DenseF32, BinaryPacked
    pub normalization: ...,           // none, l2
    pub document_prompt_hash: ...,
    pub query_prompt_hash: ...,
    pub file_policy_hash: ...,
    pub docs_chunker_version: ...,
}
```

### 5.2 Resolution: Take the fork's extended fingerprint

The fork's `SemanticIndexFingerprint` is a **strict superset** of upstream's. Take the fork's version entirely. The `FingerprintChange` enum (`Rebuild` / `ClearQueryCache` / `None`) and the comparison logic that triggers index invalidation when provider settings change must also come from the fork.

**Upstream's fingerprint** is simpler because upstream doesn't have provider-aware vectors. The merge drops upstream's simpler version and uses the fork's extended one. Since all added fields have `#[serde(default)]`, upstream V7 indexes will deserialize with default values for the new fields — which is correct (they were built without provider awareness, so defaulting to `DenseF32`/`NativeF32`/`cosine` is the right behavior).

---

## 6. Provider-Aware Vector Storage (Fork-Only — Must Preserve)

### 6.1 The type hierarchy

```
TypedVector (incoming from provider)
  ├── DenseF32(Vec<f32>)
  ├── DenseInt8(Vec<i8>)
  └── BinaryPacked { bytes: Vec<u8>, logical_dims: usize }
      │
      │ into_stored(StorageStrategy)
      ▼
StoredVector (on disk / in snapshot)
  ├── DenseF32(Vec<f32>)     → cosine/dot-product search
  └── BinaryPacked { ... }   → Hamming distance search
```

### 6.2 Key components (all fork-only, all must be preserved)

| Component | Location (fork) | Purpose |
| :--- | :--- | :--- |
| `VectorKind` enum | L85 | Declares provider's output format |
| `TypedVector` enum | L120 | Captures incoming raw embeddings |
| `StoredVector` type | ~L130 | Final storage format (DenseF32 or BinaryPacked) |
| `StorageStrategy` enum | ~L140 | Controls conversion (NativeF32, DecodeNormalizeF32, BinaryPacked) |
| `DistanceMetric` enum | L6 | Selects cosine vs Hamming |
| `EmbeddingModelProfile` struct | L404 | Provider capability declaration (dimension, encoding, metric, batch size) |
| `decode_base64_int8()` | L218 | Decodes base64-encoded int8 vectors |
| `decode_base64_binary()` | L228 | Decodes base64-encoded binary vectors |
| `l2_normalize()` | L282 | Normalizes vectors for cosine search |
| `parse_embedding_value()` | ~L300 | High-level parser routing to decoders |
| `SemanticEmbeddingModel` struct | L1238 | Extended with `output_encoding`, `storage_strategy`, `distance_metric`, `input_mode` |

### 6.3 Upstream has NONE of these

Confirmed: upstream's `semantic_index.rs` contains zero matches for `VectorKind`, `TypedVector`, `StoredVector`, `DenseInt8`, `BinaryPacked`, `Hamming`, `base64`, `output_encoding`, `storage_strategy`, `distance_metric`, or `normalization`. These are entirely fork-only.

### 6.4 Migration action

**Take the fork's entire provider-aware vector storage layer verbatim.** These types and functions are self-contained — they don't depend on upstream's `SharedSemanticBase` or `QueryBudget`. The only integration point is where `TypedVector::into_stored()` produces the final `Vec<f32>` that goes into `EmbeddingEntry.vector` — that path must also compute and store the `norm` field (upstream's addition, §4.2).

---

## 7. Model2vec Backend (Fork-Only — Must Preserve)

### 7.1 Feature gate

```rust
#[cfg(feature = "semantic-model2vec")]
use model2vec_rs::model::StaticModel as Model2VecStaticModel;
```

The model2vec backend is feature-gated with `semantic-model2vec` and is entirely fork-only. It provides local offline embeddings using the Potion Code 16M model.

### 7.2 Migration action

**Take the fork's model2vec code verbatim.** The feature gate ensures it's compiled out when the feature is disabled. No upstream code touches this path. The only consideration is that the `SharedSemanticBase` registry (upstream) should also cache model2vec embedding models — but this is an optimization, not a blocker. The fork's current direct model instantiation works correctly.

---

## 8. Upstream's Concurrency Modernization (Must Adopt)

### 8.1 `SharedSemanticBase` — shared embedding model registry

Upstream introduced a process-wide registry of shared semantic bases:

```rust
struct SharedSemanticBase {
    entries: Vec<EmbeddingEntry>,
    file_mtimes: HashMap<PathBuf, SystemTime>,
    file_sizes: HashMap<PathBuf, u64>,
    any_missing_sizes: bool,
    file_hashes: HashMap<PathBuf, blake3::Hash>,
    dimension: usize,
    fingerprint: Option<SemanticIndexFingerprint>,
    deferred_files: HashSet<PathBuf>,
}

// Global registry via OnceLock + Weak pointers
fn shared_semantic_bases() -> &'static Mutex<SharedSemanticBaseRegistry> {
    static REGISTRY: OnceLock<Mutex<SharedSemanticBaseRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}
```

**Purpose:** Multiple `SemanticIndex` instances for the same project share the same base data via `Arc<SharedSemanticBase>`, avoiding duplicate embedding model connections and reducing memory.

### 8.2 `QueryBudget` — per-query timeout

```rust
pub struct QueryBudget {
    timeout_ms: u64,
}

pub enum EmbeddingRequestPolicy {
    Build,              // No timeout for background builds
    Query(QueryBudget), // Timeout for interactive queries
}
```

**Purpose:** Interactive semantic searches have a configurable timeout (default `DEFAULT_SEMANTIC_QUERY_TIMEOUT_MS`), clamped between `MIN` and `MAX`. Background builds have no timeout. This prevents a slow embedding server from hanging the agent.

### 8.3 `parse_source_with_cached_parser` — cached tree-sitter parsing

Upstream switched from `grammar_for` to `parse_source_with_cached_parser` for source file parsing during index collection, with `Instant`-based performance tracking:

```rust
let parse_started = Instant::now();
let tree_result = parse_source_with_cached_parser(file, source, lang)
    .map_err(|error| error.to_string());
phases.parse += parse_started.elapsed();
```

### 8.4 Migration action

1. **Adopt `SharedSemanticBase` + registry** — add the struct, the `OnceLock`/`Weak` registry, and the `shared_base` field to `SemanticIndex`. The fork's `SemanticIndexSnapshot` can coexist — the shared base wraps the snapshot's data.
2. **Adopt `QueryBudget` + `EmbeddingRequestPolicy`** — add the timeout constants and the policy enum. Wire it into the fork's `embed_query` / `embed_query_cached` methods so interactive queries respect the timeout.
3. **Adopt `parse_source_with_cached_parser`** — replace `grammar_for` with the cached parser in the fork's index collection path. Add `Instant`-based phase timing.
4. **Adopt the `norm: f32` field** in `EmbeddingEntry` and the search-path optimization that uses it.

---

## 9. Conflict Hunk Categories (47 hunks)

Based on the structural analysis, the 47 conflict hunks fall into these categories:

| Category | Approx. hunks | Nature | Resolution |
| :--- | :--- | :--- | :--- |
| **Imports** | ~5 | Fork imports provider-aware types; upstream imports `AtomicUsize`, `OnceLock`, `Weak`, `Instant`, `parse_source_with_cached_parser` | **Union both** import sets |
| **Version constants** | ~2 | Fork: V1–V8; upstream: V1–V7 | **Take fork's V1–V8** (superset) |
| **`SemanticIndexFingerprint`** | ~3 | Fork's extended struct vs upstream's simple struct | **Take fork's** (superset) |
| **`EmbeddingEntry`** | ~2 | Fork: `chunk_hash`; upstream: `norm` | **Keep both fields** |
| **`SemanticIndex` struct** | ~3 | Fork: snapshot+lifecycle; upstream: flat+shared_base | **Take fork's struct + add `shared_base`** |
| **`SharedSemanticBase` + registry** | ~5 | Upstream-only: struct, registry, `from_shared_base`, `materialize_shared_base` | **Take upstream's verbatim** |
| **`QueryBudget` + timeout** | ~3 | Upstream-only: struct, policy enum, constants | **Take upstream's verbatim** |
| **Parser entry point** | ~2 | Fork: `grammar_for`; upstream: `parse_source_with_cached_parser` + `Instant` timing | **Take upstream's** |
| **Provider-aware vector types** | ~8 | Fork-only: `VectorKind`, `TypedVector`, `StoredVector`, `StorageStrategy`, decoders, normalizers | **Take fork's verbatim** |
| **`EmbeddingModelProfile`** | ~3 | Fork-only: provider capability struct | **Take fork's verbatim** |
| **`SemanticEmbeddingModel`** | ~4 | Fork extended with provider fields; upstream has `norm` caching + `QueryBudget` | **Merge: fork's provider fields + upstream's norm/timeout** |
| **model2vec** | ~2 | Fork-only, feature-gated | **Take fork's verbatim** |
| **Serialization (`to_bytes`/`from_bytes`)** | ~3 | Fork: V8 with file manifest + chunk_hash; upstream: V7 with norm | **Take fork's V8 + add `norm` field** |
| **Search path** | ~2 | Fork: provider-aware metric selection; upstream: cached `norm` optimization | **Merge: fork's metric selection + upstream's norm-based denom** |

---

## 10. Implementation Plan (ordered)

### Step 1: Union the imports
Take both sides' `use` statements. Fork's provider-aware imports + upstream's `AtomicUsize`, `OnceLock`, `Weak`, `Instant`, `parse_source_with_cached_parser`, `QueryBudget`.

### Step 2: Take fork's version constants (V1–V8)
Keep `SEMANTIC_INDEX_VERSION_V8` as the current version. The fork already accepts V1–V8 in `from_bytes`.

### Step 3: Take fork's `SemanticIndexFingerprint` (extended)
The fork's 15+ field struct with `#[serde(default)]` on all new fields. Upstream V7 indexes will deserialize with defaults for the provider-aware fields.

### Step 4: Take fork's `FingerprintChange` enum + comparison logic
The `Rebuild`/`ClearQueryCache`/`None` enum and the diff logic that detects when provider settings change.

### Step 5: Take fork's provider-aware vector types verbatim
`VectorKind`, `TypedVector`, `StoredVector`, `StorageStrategy`, `DistanceMetric`, `EmbeddingModelProfile`, `decode_base64_int8`, `decode_base64_binary`, `l2_normalize`, `parse_embedding_value`. These are self-contained.

### Step 6: Take fork's `SemanticEmbeddingModel` + add upstream's fields
Merge the fork's extended model struct (with `output_encoding`, `storage_strategy`, `distance_metric`, `input_mode`) with upstream's `QueryBudget`/timeout integration and `norm` caching.

### Step 7: Take fork's model2vec code verbatim
Feature-gated, self-contained. No upstream interaction.

### Step 8: Adopt upstream's `SharedSemanticBase` + registry
Add the struct, the `OnceLock`/`Weak` registry, `from_shared_base`, `materialize_shared_base`. Add the `shared_base: Option<Arc<SharedSemanticBase>>` field to `SemanticIndex`.

### Step 9: Adopt upstream's `QueryBudget` + `EmbeddingRequestPolicy`
Add the timeout constants, the `QueryBudget` struct, and the `EmbeddingRequestPolicy` enum. Wire `Query` policy into the fork's `embed_query` / `embed_query_cached` methods.

### Step 10: Adopt upstream's `parse_source_with_cached_parser`
Replace `grammar_for` with `parse_source_with_cached_parser` in the index collection path. Add `Instant`-based `SemanticCollectPhaseTimings`.

### Step 11: Merge `EmbeddingEntry` — keep both `chunk_hash` and `norm`
```rust
pub struct EmbeddingEntry {
    pub(crate) chunk: SemanticChunk,
    pub(crate) vector: Vec<f32>,
    pub(crate) chunk_hash: String,  // fork V8
    norm: f32,                       // upstream optimization
}
```
Update `to_bytes`/`from_bytes` to serialize both. For V7 indexes (from upstream), `norm` will be computed on load; `chunk_hash` will be populated on next refresh.

### Step 12: Merge the search path
Combine the fork's provider-aware metric selection (cosine for `DenseF32`, Hamming for `BinaryPacked`) with upstream's cached `norm` optimization (`denom = query_norm * entry.norm`).

### Step 13: Compile-gate
```bash
cargo check -p aft --features semantic-model2vec,semantic-fts5
```

### Step 14: Test-gate
```bash
cargo test -p aft --features semantic-model2vec,semantic-fts5 -- semantic
cargo clippy -p aft --all-targets --features semantic-model2vec,semantic-fts5 -- -D warnings
```

### Step 15: Validate fingerprint invalidation
Run `aft-fts5e2e.1` acceptance criteria — verify that changing provider settings (backend, model, encoding, metric) triggers the correct `FingerprintChange::Rebuild` and that the index is invalidated and rebuilt.

---

## 11. Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
| :--- | :--- | :--- | :--- |
| `SharedSemanticBase` wrapping doesn't work with fork's `SemanticIndexSnapshot` | Medium | High | The shared base can wrap the snapshot's `entries`/`file_mtimes`/etc. — verify field compatibility; may need an adapter |
| `norm` field not computed for BinaryPacked vectors | Medium | Medium | Hamming distance doesn't use `norm` — set `norm = 0.0` for binary vectors, or skip the norm-based denom for Hamming |
| V7 indexes from upstream users fail to deserialize on merged code | Low | High | Fork's `from_bytes` already accepts V1–V8; V7 is accepted with defaults for provider-aware fields |
| `parse_source_with_cached_parser` API differs from `grammar_for` | Medium | Medium | Check the function signature; both take `(file, source, lang)` and return a tree result |
| `QueryBudget` timeout breaks long-running embedding builds | Low | Medium | `EmbeddingRequestPolicy::Build` returns `None` (no timeout) — builds are unaffected |
| model2vec + `SharedSemanticBase` registry interaction | Low | Low | model2vec is feature-gated; registry can skip model2vec models or cache them separately |
| 47 hunks → merge introduces subtle logic errors | High | High | Resolve in order (§10); compile-gate after each major step; run semantic tests after step 14 |

---

## 12. Serialization Compatibility Matrix

| Index origin | Version | Has `chunk_hash`? | Has `norm`? | Has provider fields? | Merged code behavior |
| :--- | :---: | :---: | :---: | :---: | :--- |
| Fresh build (merged) | V8 | ✅ | ✅ | ✅ | Full features |
| Fork V8 index | 8 | ✅ | ❌ | ✅ | Deserializes; `norm` computed on load |
| Upstream V7 index | 7 | ❌ | ❌ | ❌ | Deserializes; `chunk_hash` + `norm` + provider fields defaulted; populated on next refresh |
| Upstream V6 index | 6 | ❌ | ❌ | ❌ | Same as V7 |
| Fork V7 index | 7 | ❌ | ❌ | ✅ (partial) | Deserializes; `chunk_hash` + `norm` defaulted |

All index versions V1–V8 are accepted by the merged code. V7+ indexes are upgraded to V8 on the next write (refresh or rebuild).

---

## 13. Summary

| Feature area | Source | Action |
| :--- | :--- | :--- |
| Version V8 (file manifest + chunk_hash) | Fork | **Keep** (V8 is the floor) |
| `SemanticIndexFingerprint` (15+ fields) | Fork | **Keep** (superset of upstream) |
| `FingerprintChange` enum + invalidation logic | Fork | **Keep** |
| `VectorKind`/`TypedVector`/`StoredVector`/`StorageStrategy` | Fork | **Keep** (self-contained) |
| `EmbeddingModelProfile` | Fork | **Keep** |
| model2vec backend | Fork | **Keep** (feature-gated) |
| `SemanticIndexSnapshot` struct pattern | Fork | **Keep** |
| `SharedSemanticBase` + `OnceLock`/`Weak` registry | Upstream | **Adopt** (add `shared_base` field) |
| `QueryBudget` + `EmbeddingRequestPolicy` | Upstream | **Adopt** (wire into query path) |
| `parse_source_with_cached_parser` + `Instant` timing | Upstream | **Adopt** (replace `grammar_for`) |
| `EmbeddingEntry.norm: f32` | Upstream | **Adopt** (add alongside fork's `chunk_hash`) |
| Search path (norm-based denom) | Upstream | **Adopt** (combine with fork's metric selection) |

**14 implementation steps** (§10), compile-gated after each major step, test-gated at the end with `cargo test --features semantic-model2vec,semantic-fts5 -- semantic`.

---

*This is a design document. No source code was modified. The implementation steps in §10 are ready for execution during the merge phase.*
