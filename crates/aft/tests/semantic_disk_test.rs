// This standalone binary re-includes the integration `semantic_disk_test`
// module, which uses `crate::test_helpers` for shared warn-level log capture.
// Declare the same helpers module here so the import resolves in this binary's
// crate root too (the integration binary declares it via tests/integration/main.rs).
#[path = "helpers/mod.rs"]
mod test_helpers;

#[path = "integration/semantic_disk_test.rs"]
mod semantic_disk_test;
