# ADR-001: Exact-First Intelligence Layer

**Status:** Accepted  
**Date:** 2026-06-15  
**Epic:** `aft-fts5` — AFT Agent-Grade FTS5/Search/Tool Intelligence Upgrade  
**Deciders:** AFT maintainers  
**Supersedes:** PRD "AFT Agent-Grade Tool Intelligence Upgrade, 2026-06-15" (planning text)

---

## Context

AFT already provides strong exact tool execution (`read`, `edit`, `grep`, `glob`, `bash`) backed by tree-sitter parsing, trigram indexing, and LSP diagnostics. The next product layer adds agent-grade intelligence: FTS5 full-text search, semantic search enhancements, repository graph facts, mutation-risk classification, and native symbolic workflows.

These intelligence features must not degrade or replace the exact semantics agents depend on. The reference systems that inspired them (Qartez MCP, Semble, Lean-CTX, Serena) are implementation references only — not required runtime dependencies.

This ADR records the binding architectural decisions that govern the `aft-fts5` epic and any future intelligence-layer work.

---

## Decision 1: Exact-First Invariant

**Rule:** Exact tool behavior is authoritative. Enrichment attaches *after* exact results. Enrichment never replaces exact evidence.

**Rationale:** Agents treat AFT tool output as ground truth. If enrichment masquerades as exact matches, agents make incorrect decisions. Keeping the two lanes separate means enrichment can be suppressed, stale, degraded, or timed out without breaking any exact tool.

**Examples:**
- `grep` returns trigram-indexed exact matches first. Semantic/FTS5 candidates appear in an enrichment section, clearly labeled.
- `read` returns exact file content. Orientation sidecars (symbol summary, imports, risk hints) appear in a separate `context` or `orientation` field.
- `edit`/`write` mutations are exact. Risk advisories appear as warnings, not blockers (unless the Bead explicitly adds a gate).

**Enrichment failure policy:** When enrichment fails (timeout, disabled, stale, corrupt), the exact result is still returned. The enrichment section reports its degraded state (e.g., `"enrichment_state": "timed_out"`). The agent can choose to ignore enrichment without losing any exact evidence.

---

## Decision 2: No Required Runtime Dependency on Reference Projects

**Rule:** Qartez MCP, Semble, Lean-CTX, and Serena are implementation inspiration only. They must not become required runtime dependencies.

**Rationale:** AFT is a standalone Rust binary with thin TypeScript adapters. Adding runtime dependencies on external MCP servers, Python packages, or Node.js tools would:
- Break the single-binary deployment model
- Create version-coupling with fast-moving upstreams
- Make CI and release builds dependent on external availability
- Violate the "one process per project root" architecture

**What this means in practice:**
- Implement features natively in Rust (`crates/aft/`) or in the TypeScript bridge/plugin layer.
- Reference systems may inform design (output shapes, query patterns, ranking strategies) but code must be AFT-native.
- If a feature requires capabilities AFT lacks (e.g., a specific embedding model), use pluggable backends (OpenAI-compatible API, Ollama, local ONNX) rather than bundling the reference system.

---

## Decision 3: Feature Flag and Kill-Switch Policy

**Rule:** Every new intelligence subsystem must have a config-gated enable/disable path. Disabled by default for new subsystems.

**Rationale:** Intelligence features add complexity, indexing overhead, and potential for degraded behavior. Agents and users must be able to opt out without losing exact tool functionality.

**Policy:**
- New subsystems (FTS5, repository graph, mutation risk, symbolic refactor) ship disabled by default.
- Each subsystem has a runtime config key (e.g., `[fts5].enabled`, `[graph].enabled`).
- Each subsystem has a compile-time feature flag where appropriate (e.g., `semantic-fts5`).
- Disabling a subsystem must not break exact tools — only the enrichment layer is affected.
- The `configure` command (or equivalent) can toggle subsystems without restart where possible.

**Existing subsystems** (trigram search, semantic search, LSP diagnostics) retain their current enable/disable behavior.

---

## Decision 4: Output Contract — Result / Evidence / Context / State / Next Move

**Rule:** AFT tool responses follow a structured output contract that separates exact results from enrichment and diagnostics.

**Structure:**

| Field | Purpose | Required? |
|-------|---------|-----------|
| `success` | Whether the requested operation succeeded | Always |
| `data` / `results` | Exact output of the operation | On success |
| `complete` | Whether the result is complete or partial | When applicable |
| `enrichment` | Enrichment/sidecar data (semantic candidates, graph facts, risk hints) | Optional, when available |
| `enrichment_state` | State of enrichment: `healthy`, `disabled`, `building`, `stale`, `timed_out`, `failed` | When enrichment is present |
| `scope_warnings` | Warnings about partial scope, skipped files, etc. | When applicable |
| `skipped_files` | Files that failed to process, with reasons | When applicable |

**Anti-patterns prevented:**
- Returning `success: true` with empty results when scope resolved to zero files (must use `no_files_matched_scope` or `path_not_found`).
- Silently dropping files that fail to parse (must include `skipped_files`).
- Asserting completeness when results are truncated (must use `complete: false`).

**Note:** This contract aligns with the existing Honest Reporting Convention in `ARCHITECTURE.md`. This ADR extends it to cover enrichment fields.

---

## Decision 5: Tool-Surface Policy

**Rule:** Minimize the number of new model-facing tools. Prefer enriching existing tools over adding new ones.

**Rationale:** Every new tool surface increases the agent's cognitive load and the schema maintenance burden. Existing tools (`read`, `edit`, `grep`, `glob`, `bash`) already cover the core use cases. Enrichment is better delivered as additional fields on existing tool responses than as separate tools.

**Policy:**
- Enrichment features attach to existing tool responses (e.g., `grep` gains semantic candidates, `read` gains orientation context).
- New model-facing tools are added only when the capability cannot be expressed as enrichment on an existing tool (e.g., `aft_callgraph` for multi-hop traversal, `aft_inspect` for codebase health).
- New tools must have a clear production entry point, not only helper-level tests.
- Each new tool requires an explicit Bead that specifies its schema, tool description, and integration path.

**Existing tool semantics are preserved.** AFT's hoisted tools (`read`, `write`, `edit`, `grep`, `glob`, `bash`) continue to behave as they do today. Enrichment is additive.

---

## Decision 6: Approval Triggers

**Rule:** Create a blocking approval Bead before implementing changes that would:
- Make graph/semantic/FTS5 indexing mandatory for normal exact tools.
- Change public OpenCode tool semantics incompatibly.
- Add a required external runtime dependency.
- Introduce persistent storage migration risk without rollback.
- Remove existing rollback/checkpoint guarantees.
- Hard-block user edits by default.

**Rationale:** These changes affect all users and downstream consumers. They require explicit human review before implementation proceeds.

**Non-blocking changes** (no approval required):
- Adding enrichment fields to existing tool responses.
- Adding new optional config keys with safe defaults.
- Adding new model-facing tools behind feature flags.
- Adding tests and documentation.

---

## Decision 7: Stale and Degraded State Visibility

**Rule:** Every enrichment subsystem must report its state clearly. Degraded or stale enrichment must not masquerade as healthy.

**States:**

| State | Meaning | Agent action |
|-------|---------|-------------|
| `healthy` | Enrichment is current and complete | Use freely |
| `disabled` | Subsystem is turned off in config | No action needed |
| `not_configured` | Subsystem has no config section | No action needed |
| `building` | Index is being built or updated | Wait or use exact results only |
| `stale` | Enrichment data is older than freshness threshold | Use with caution |
| `timed out` | Enrichment computation exceeded time budget | Use exact results only |
| `failed` | Enrichment computation encountered an error | Use exact results only |
| `partial` | Some enrichment succeeded, some did not | Use available partial data |
| `corrupt` | Enrichment data is malformed or inconsistent | Rebuild or disable |

**Safety-relevant subsystems** (mutation risk, verify) must not silently degrade. They must report `failed` or `degraded` state so the agent knows the risk advisory is missing.

---

## Consequences

### Positive
- Exact tool behavior is stable and predictable across all intelligence-layer changes.
- Intelligence features can be developed, tested, and shipped incrementally without risk to core tools.
- Users can opt out of intelligence features without losing functionality.
- The output contract provides a clear schema for tool responses, reducing ambiguity for agents.

### Negative
- Enrichment features that depend on exact results must handle missing or stale data gracefully.
- Adding enrichment fields increases response size; context-budget management is required.
- The two-lane architecture requires discipline to maintain — enrichment code must never override exact results.

### Risks
- If enrichment becomes essential for agent performance, the "optional" policy may need revision. This is acceptable as long as the exact-first invariant is preserved.

---

## References

- PRD: AFT Agent-Grade Tool Intelligence Upgrade, 2026-06-15
- Epic: `aft-fts5` (Beads)
- Related: `aft-fts5e2e` (FTS5 e2e side feature), `bd-aft-ri` (Qartez-style intelligence), `bd-aft-db` (persistent graph)
- ARCHITECTURE.md: Honest Reporting Convention, Bash Output Compression
- Reference systems: Qartez MCP, Semble, Lean-CTX, Serena (inspiration only, not dependencies)
