<#
.SYNOPSIS
Run Rust validation inside a Docker container — fmt, check, clippy, test, or all four.

.DESCRIPTION
Mounts the repo root into a Rust Docker image and runs Cargo commands with
persistent volumes for the Cargo registry, git cache, and target directory.

This is Linux-target validation, NOT native Windows MSVC validation. It is
acceptable for normal Rust implementation work unless you are touching
Windows-specific filesystem/path/process/TUI behavior.

.PARAMETER Task
Which task to run: fmt, check, clippy, test, validate, or shell.
Defaults to validate.

.EXAMPLE
.\scripts\docker-rust.ps1 fmt
.\scripts\docker-rust.ps1 check
.\scripts\docker-rust.ps1 clippy
.\scripts\docker-rust.ps1 test
.\scripts\docker-rust.ps1 validate
.\scripts\docker-rust.ps1 shell

.PARAMETER Image
Docker image to use. Override via $env:AFT_RUST_DOCKER_IMAGE.
Defaults to rust:1-bookworm.
#>

param(
    [Parameter(Position = 0)]
    [ValidateSet('fmt', 'check', 'clippy', 'test', 'validate', 'shell')]
    [string]$Task = 'validate'
)

$ErrorActionPreference = 'Stop'

# --- Image ---
$Image = if ($env:AFT_RUST_DOCKER_IMAGE) { $env:AFT_RUST_DOCKER_IMAGE } else { 'rust:1-bookworm' }

# --- Volumes ---
$Volumes = @(
    '--volume', 'aft-cargo-registry:/usr/local/cargo/registry',
    '--volume', 'aft-cargo-git:/usr/local/cargo/git',
    '--volume', 'aft-target:/target'
)

# --- Determine repo root (where this script lives) ---
$RepoRoot = Split-Path -Parent $PSScriptRoot

# --- Helper: run a Docker command ---
function Invoke-DockerTask {
    param([string[]]$DockerArgs)

    $fullArgs = @(
        'run', '--rm',
        '--workdir', '/work'
    ) + $Volumes + @(
        '--env', 'CARGO_TARGET_DIR=/target'
    ) + $DockerArgs

    Write-Host "docker $($fullArgs -join ' ')" -ForegroundColor Cyan
    & docker $fullArgs
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        Write-Host "Docker command failed with exit code $exitCode" -ForegroundColor Red
        exit $exitCode
    }
}

# --- Ensure Docker volumes exist ---
foreach ($vol in 'aft-cargo-registry', 'aft-cargo-git', 'aft-target') {
    $existing = docker volume ls --format '{{.Name}}' | Select-String -Pattern "^$vol$"
    if (-not $existing) {
        Write-Host "Creating Docker volume: $vol" -ForegroundColor Yellow
        docker volume create $vol | Out-Null
    }
}

# --- Task dispatch ---
switch ($Task) {
    'fmt' {
        Write-Host "=== cargo fmt --check ===" -ForegroundColor Green
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'rustup component add rustfmt && cargo fmt --check'
        )
    }

    'check' {
        Write-Host "=== cargo check --workspace --all-targets ===" -ForegroundColor Green
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'cargo check --workspace --all-targets'
        )
    }

    'clippy' {
        Write-Host "=== cargo clippy --workspace --all-targets --all-features -- -D warnings ===" -ForegroundColor Green
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'rustup component add clippy && cargo clippy --workspace --all-targets --all-features -- -D warnings'
        )
    }

    'test' {
        Write-Host "=== cargo test --workspace --all-targets ===" -ForegroundColor Green
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'cargo test --workspace --all-targets'
        )
    }

    'validate' {
        Write-Host "=== Running full validation: fmt → check → clippy → test ===" -ForegroundColor Green

        Write-Host "`n--- Step 1/4: cargo fmt --check ---" -ForegroundColor Cyan
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'rustup component add rustfmt && cargo fmt --check'
        )

        Write-Host "`n--- Step 2/4: cargo check --workspace --all-targets ---" -ForegroundColor Cyan
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'cargo check --workspace --all-targets'
        )

        Write-Host "`n--- Step 3/4: cargo clippy --workspace --all-targets --all-features -- -D warnings ---" -ForegroundColor Cyan
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'rustup component add clippy && cargo clippy --workspace --all-targets --all-features -- -D warnings'
        )

        Write-Host "`n--- Step 4/4: cargo test --workspace --all-targets ---" -ForegroundColor Cyan
        Invoke-DockerTask -DockerArgs @(
            '--volume', "${RepoRoot}:/work",
            $Image,
            'sh', '-c',
            'cargo test --workspace --all-targets'
        )

        Write-Host "`n=== All validation steps passed ===" -ForegroundColor Green
    }

    'shell' {
        Write-Host "=== Starting interactive shell in container ===" -ForegroundColor Green
        $fullArgs = @(
            'run', '--rm', '-it',
            '--workdir', '/work'
        ) + $Volumes + @(
            '--env', 'CARGO_TARGET_DIR=/target',
            '--volume', "${RepoRoot}:/work",
            $Image,
            'bash'
        )
        Write-Host "docker $($fullArgs -join ' ')" -ForegroundColor Cyan
        & docker $fullArgs
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            Write-Host "Docker shell exited with code $exitCode" -ForegroundColor Red
            exit $exitCode
        }
    }
}
