# Docker Rust Validation

## Purpose

Run Rust `fmt`, `check`, `clippy`, and `test` inside a Docker container so
Windows users do not need Microsoft C++ Build Tools (MSVC) installed.

**Docker validation is Linux-target validation, not native Windows MSVC
validation.** It is acceptable for normal Rust implementation work unless you
are touching Windows-specific filesystem/path/process/TUI behavior.

## When to use native Windows validation

Native Windows validation is still required when changes touch:

- Windows-specific path handling
- Process spawning (`std::process::Command` on Windows)
- Terminal/TUI behavior (ANSI sequences, console APIs)
- Packaging/release binaries (cross-compilation)
- Code relying on OS-specific `cfg!(windows)` or `#[cfg(windows)]` paths

For everything else, Docker validation is faster and avoids the MSVC
toolchain dependency.

## Prerequisites

- Docker Desktop (or Docker Engine) installed and running
- The `aft-cargo-registry`, `aft-cargo-git`, and `aft-target` Docker volumes
  (created automatically on first run)

## How to run

All commands below are run from the repo root.

### Using npm/bun scripts (recommended)

```powershell
# Full validation: fmt → check → clippy → test
bun run docker:rust:validate

# Individual steps
bun run docker:rust:fmt
bun run docker:rust:check
bun run docker:rust:clippy
bun run docker:rust:test

# Interactive shell inside the container
bun run docker:rust:shell
```

### Using the PowerShell script directly

```powershell
# Full validation
.\scripts\docker-rust.ps1 validate

# Individual steps
.\scripts\docker-rust.ps1 fmt
.\scripts\docker-rust.ps1 check
.\scripts\docker-rust.ps1 clippy
.\scripts\docker-rust.ps1 test

# Interactive shell
.\scripts\docker-rust.ps1 shell
```

### Overriding the Docker image

```powershell
$env:AFT_RUST_DOCKER_IMAGE = 'rust:1.80-bookworm'
.\scripts\docker-rust.ps1 validate
```

## Caching

The script uses three persistent Docker volumes for Cargo caches:

| Volume | Purpose |
|---|---|
| `aft-cargo-registry` | Crate registry download cache |
| `aft-cargo-git` | Git dependency cache |
| `aft-target` | Compiled artifact cache (`CARGO_TARGET_DIR=/target`) |

These volumes persist across runs so subsequent invocations reuse compiled
artifacts and downloaded crates.

## Cleaning up

```powershell
# Remove Cargo and build caches
docker volume rm aft-cargo-registry aft-cargo-git aft-target

# Remove the Rust image
docker image rm rust:1-bookworm
```

## How it works

1. The script determines the repo root from its own location.
2. It checks that the three Docker volumes exist (creating them if needed).
3. It runs `docker run` with the repo root mounted at `/work` and the volumes
   mounted at their respective Cargo paths.
4. `CARGO_TARGET_DIR=/target` ensures compiled artifacts land on the volume
   instead of inside `/work/target/`.
5. Steps install `rustfmt` or `clippy` via `rustup component add` if the
   component is not already present in the image.
6. Each step fails fast: if `fmt` fails, the validation stops before `check`.

## Design decisions

- **No `Cargo.toml` changes.** Cargo.toml is for Rust workspace/package
  configuration, not Docker orchestration. All Docker logic lives in scripts
  and documentation.
- **No additional `Dockerfile` required for basic usage.** The script pulls
  `rust:1-bookworm` directly. The optional `Dockerfile.rust` at the repo root
  is only needed if you want to pre-install components for faster startup.
- **Native scripts are preserved.** The existing `scripts/release.sh` and
  `package.json` native scripts (`build:rust`, `test:rust`, `format:check`)
  are unchanged and still work for users with a native Rust toolchain.
