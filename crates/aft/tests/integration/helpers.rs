pub use crate::test_helpers::{fixture_path, AftProcess};

/// Skip the current test when running as root (UID 0). Root bypasses
/// filesystem permission checks, so tests that depend on read-only files
/// producing write errors are meaningless under root.
pub fn skip_if_root() {
    #[cfg(unix)]
    {
        // SAFETY: getuid(2) is always safe to call.
        if unsafe { libc::getuid() } == 0 {
            eprintln!("skipping: running as root, permission checks are bypassed");
            // panic!(...) is the standard nextest skip mechanism.
            panic!("skipped under root");
        }
    }
}
