//! Sources travel inside the test executable so nextest archives need no checkout.

#[allow(dead_code)]
#[path = "binary_cache.rs"]
mod binary_cache;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SOURCES: &[(&str, &[u8])] = &[
    ("Cargo.toml", include_bytes!("../lsp/helper/Cargo.toml.in")),
    ("Cargo.lock", include_bytes!("../lsp/helper/Cargo.lock.in")),
    ("main.rs", include_bytes!("../lsp/helper/main.rs.in")),
    ("fake_server.rs", include_bytes!("../lsp/fake_server.rs")),
    ("jsonrpc.rs", include_bytes!("../../src/lsp/jsonrpc.rs")),
    ("transport.rs", include_bytes!("../../src/lsp/transport.rs")),
];

pub fn fake_server_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| build_in(&binary_cache::root()))
        .clone()
}

#[allow(dead_code)]
pub(crate) fn build_in(root: &Path) -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(&rustc)
        .arg("-vV")
        .output()
        .expect("rustc is required for test helpers");
    assert!(version.status.success(), "rustc version failed");
    let version = String::from_utf8(version.stdout).expect("rustc version is UTF-8");
    let target = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc host target");
    let key = binary_cache::key(
        &SOURCES.iter().map(|(_, bytes)| *bytes).collect::<Vec<_>>(),
        &version,
        target,
    );
    binary_cache::install(root, &key, target, |pending| {
        eprintln!("Building shared AFT test helper fake-lsp-server ({key})");
        let project = tempfile::tempdir_in(root)?;
        for (name, source) in SOURCES {
            std::fs::write(project.path().join(name), source)?;
        }
        // An isolated target directory avoids taking the test runner's Cargo
        // lock recursively; the per-helper lock serializes all nested builds.
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(project.path())
            .env("RUSTC", &rustc)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTFLAGS")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .args(["build", "--locked", "--target", target, "--jobs", "2"])
            .arg("--target-dir")
            .arg(root.join("build"))
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        std::fs::copy(
            binary_cache::executable(&root.join("build").join(target).join("debug"), target),
            pending,
        )?;
        Ok(())
    })
    .expect("build shared fake LSP helper")
}
