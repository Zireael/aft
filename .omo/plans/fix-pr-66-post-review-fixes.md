# Fix: PR #66 Post-Review Fixes

## Objective
Address 6 confirmed issues discovered during code review of PR #66 changes. Each fix is small, targeted, and independently verifiable.

## Files to Modify

### Fix 1: GetModuleFileNameW buffer truncation
**File:** `crates/aft/src/semantic_index.rs`
**Change:** Increase `path_buf` from `[0u16; 260]` to `[0u16; 32767]` (MAX_UNICODEPATH).
**Why:** `GetModuleFileNameW` truncates silently when the DLL path exceeds 260 chars (e.g., deep NuGet package paths). Truncation causes `GetFileVersionInfoSizeW` to fail, `detected_major` stays 0, and the ORT version check is silently bypassed.
**Verification:** `cargo check` + `cargo clippy -D warnings` pass.

### Fix 2: Duplicate PATH scanning in CLI onnx.ts
**File:** `packages/aft-cli/src/lib/onnx.ts`
**Change:** Remove the manual `process.env.PATH.split(";")` loop (lines 89-95). `pathEntriesForPlatform()` already reads PATH with proper filtering (absolute check, null-byte rejection, `.` exclusion, quote stripping).
**Why:** PATH entries are scanned twice. The manual loop misses quote stripping and only checks `PATH` (not `Path` or `path`).
**Verification:** `tsc --noEmit` passes in both packages.

### Fix 3: Diagnostics mutates filesystem (side effect)
**File:** `packages/aft-cli/src/lib/diagnostics.ts`
**Change:** Replace `mkdirSync(storage, { recursive: true })` with an existence check and `try { accessSync(storage, R_OK | W_OK) }` read/write probe.
**Why:** Creating a directory in a read-only diagnostic path is a side effect that can cause permission issues if run as a different user.
**Verification:** `tsc --noEmit` passes.

### Fix 4: Case-sensitive Windows path check
**File:** `packages/aft-bridge/src/onnx-runtime.ts`
**Change:** Change `dir.includes("Program Files") || dir.includes("onnxruntime")` to `dir.toLowerCase().includes("program files") || dir.toLowerCase().includes("onnxruntime")`.
**Why:** Windows paths are case-insensitive. A PATH entry like `c:\program files\...` would fail the case-sensitive check.
**Verification:** `tsc --noEmit` passes.

### Fix 5: Dead code in `suggest_removal_command`
**File:** `crates/aft/src/semantic_index.rs`
**Change:** Remove the unreachable `#[cfg(target_os = "windows")]` return inside the `if lib_path.starts_with("/usr/local/lib")` block.
**Why:** Windows paths never start with `/usr/local/lib`, so this branch is dead code. The fallthrough `format!("   rm '{}'", lib_path)` already handles Windows correctly with absolute paths.
**Verification:** `cargo check` + `cargo clippy -D warnings` pass.

### Fix 6: Silent NuGet scan failure
**File:** `packages/aft-bridge/src/onnx-runtime.ts`
**Change:** Add a `debug?.(...)` log statement inside the `catch` block of the NuGet `readdirSync`.
**Why:** Silent failure makes debugging hard if the NuGet directory is corrupted or permissions change.
**Verification:** `tsc --noEmit` passes.

## Execution Order
1. Fix 1 (Rust, semantic_index.rs)
2. Fix 5 (Rust, semantic_index.rs — same file)
3. Fix 2 (TypeScript, CLI onnx.ts)
4. Fix 3 (TypeScript, diagnostics.ts)
5. Fix 4 (TypeScript, bridge onnx-runtime.ts)
6. Fix 6 (TypeScript, bridge onnx-runtime.ts — same file)

## Verification
After all fixes:
1. `cargo check` in Docker
2. `cargo clippy --all-features -D warnings` in Docker
3. `tsc --noEmit` in `packages/aft-bridge`
4. `tsc --noEmit` in `packages/aft-cli`
5. Commit with message prefix `fix:`
