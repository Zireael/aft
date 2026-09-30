use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Watch the actual Git metadata paths, including linked worktrees, so a
    // new commit cannot leave the binary reporting a cached build revision.
    for name in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            if Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    // Source archives may have no Git metadata; omit the SHA rather than
    // inventing a revision for those builds.
    if let Some(revision) = git(&["rev-parse", "HEAD"]) {
        println!("cargo:rustc-env=AFT_BUILD_GIT_SHA={revision}");
    }
}
