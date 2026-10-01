// The disk tests use thread-local warn-level log capture from `test_helpers`.
// Expose that module so tests/semantic/main.rs can reuse it rather than compiling
// a second copy of the helper tests.
#[path = "helpers/mod.rs"]
pub(crate) mod test_helpers;

#[path = "integration/semantic_disk_test.rs"]
mod semantic_disk_test;
