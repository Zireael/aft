use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aft::commands::configure::handle_configure;
use aft::commands::inspect::handle_inspect;
use aft::config::Config;
use aft::context::AppContext;
use aft::lsp::registry::ServerKind;
use aft::parser::TreeSitterProvider;
use aft::protocol::RawRequest;
use serde_json::{json, Value};

fn fixture_project() -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let root = temp_dir.path().join("project");
    fs::create_dir_all(&root).expect("create project root");
    (temp_dir, root)
}

fn fake_server_path() -> PathBuf {
    option_env!("CARGO_BIN_EXE_fake-lsp-server")
        .or(option_env!("CARGO_BIN_EXE_fake_lsp_server"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_fake-lsp-server").map(PathBuf::from))
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_fake_lsp_server").map(PathBuf::from))
        .or_else(|| {
            let mut path = std::env::current_exe().ok()?;
            path.pop();
            path.pop();
            path.push("fake-lsp-server");
            Some(path)
        })
        .filter(|path| path.exists())
        .expect("fake-lsp-server binary path not set")
}

fn write_file(root: &Path, relative_path: &str, contents: &str) -> PathBuf {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(&path, contents).expect("write fixture file");
    path
}

fn request(payload: Value) -> RawRequest {
    serde_json::from_value(payload).expect("request parses")
}

fn configured_context(root: &Path) -> AppContext {
    let storage_dir = root.join(".aft-test-storage");
    let ctx = AppContext::new(
        Box::new(TreeSitterProvider::new()),
        Config {
            storage_dir: Some(storage_dir.clone()),
            ..Config::default()
        },
    );
    let configure = request(json!({
        "id": "configure",
        "command": "configure",
        "harness": "opencode",
        "project_root": root.to_string_lossy(),
        "storage_dir": storage_dir.to_string_lossy(),
        "search_index": false,
        "semantic_search": false,
    }));
    let response = serde_json::to_value(handle_configure(&configure, &ctx))
        .expect("configure response serializes");
    assert_eq!(response["success"], true, "configure failed: {response:#}");
    ctx
}

fn configure_fake_typescript_lsp(ctx: &AppContext) {
    ctx.lsp()
        .override_binary(ServerKind::TypeScript, fake_server_path());
    ctx.lsp().set_extra_env("AFT_FAKE_LSP_PULL", "1");
}

fn inspect(ctx: &AppContext, payload: Value) -> Value {
    let response = handle_inspect(&request(payload), ctx);
    serde_json::to_value(response).expect("inspect response serializes")
}

fn diagnostics_details(response: &Value) -> &[Value] {
    response["details"]["diagnostics"]
        .as_array()
        .expect("diagnostics details")
}

fn inspect_diagnostics_scope(ctx: &AppContext, scope: &str) -> Value {
    inspect(
        ctx,
        json!({
            "id": format!("inspect-diagnostics-{scope}"),
            "command": "inspect",
            "sections": ["diagnostics"],
            "scope": scope,
            "topK": 20,
        }),
    )
}

fn open_with_lsp(ctx: &AppContext, file: &Path, content: &str) {
    let config = ctx.config().clone();
    ctx.lsp()
        .notify_file_changed(file, content, &config)
        .expect("notify file changed");
    let diagnostics = ctx
        .lsp()
        .wait_for_diagnostics(file, &config, Duration::from_secs(2));
    assert!(
        !diagnostics.is_empty(),
        "fake LSP should publish diagnostics for {file:?}"
    );
}

#[test]
fn inspect_diagnostics_scoped_skips_tsconfig_excluded_test_file() {
    let (_temp_dir, root) = fixture_project();
    write_file(
        &root,
        "packages/pkg/tsconfig.json",
        r#"{
          // JSONC is accepted by tsconfig and must be accepted by the filter.
          "compilerOptions": {
            "types": ["bun"],
          },
          "include": ["src/**/*.ts"],
          "exclude": ["src/**/*.test.ts", "src/**/__tests__/**"],
        }"#,
    );
    write_file(
        &root,
        "packages/pkg/src/foo.test.ts",
        "import { test } from 'bun:test';\ntest('works', () => import.meta.dir);\n",
    );
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);

    let response = inspect_diagnostics_scope(&ctx, "packages/pkg/src/foo.test.ts");

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert_eq!(response["summary"]["diagnostics"]["errors"], 0);
    assert!(
        diagnostics_details(&response).is_empty(),
        "excluded test diagnostics must not surface: {response:#}"
    );
    assert_eq!(
        response["summary"]["diagnostics"]["files_without_server"], 0,
        "tsconfig-filtered files are skipped, not counted as no-server: {response:#}"
    );
}

#[test]
fn inspect_diagnostics_scoped_surfaces_included_ts_file() {
    let (_temp_dir, root) = fixture_project();
    write_file(
        &root,
        "packages/pkg/tsconfig.json",
        r#"{
          "include": ["src/**/*.ts"],
          "exclude": ["src/**/*.test.ts"]
        }"#,
    );
    write_file(&root, "packages/pkg/src/foo.ts", "export const foo = 1;\n");
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);

    let response = inspect_diagnostics_scope(&ctx, "packages/pkg/src/foo.ts");

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert_eq!(response["summary"]["diagnostics"]["errors"], 1);
    let details = diagnostics_details(&response);
    assert_eq!(
        details.len(),
        1,
        "included file diagnostics should surface: {response:#}"
    );
    assert_eq!(details[0]["file"], "packages/pkg/src/foo.ts");
    assert_eq!(details[0]["message"], "test pull diagnostic");
}

#[test]
fn inspect_diagnostics_warm_filters_excluded_file_and_keeps_included_file() {
    let (_temp_dir, root) = fixture_project();
    let included = write_file(&root, "pkg/src/included.ts", "export const included = 1;\n");
    let excluded = write_file(
        &root,
        "pkg/src/included.test.ts",
        "import { test } from 'bun:test';\ntest('x', () => import.meta.dir);\n",
    );
    write_file(
        &root,
        "pkg/tsconfig.json",
        r#"{
          "include": ["src/**/*.ts"],
          "exclude": ["src/**/*.test.ts"]
        }"#,
    );
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);
    open_with_lsp(&ctx, &included, "export const included = 1;\n");
    open_with_lsp(
        &ctx,
        &excluded,
        "import { test } from 'bun:test';\ntest('x', () => import.meta.dir);\n",
    );

    let response = inspect(
        &ctx,
        json!({
            "id": "inspect-diagnostics-warm-tsconfig-membership",
            "command": "inspect",
            "sections": ["diagnostics"],
            "topK": 20,
        }),
    );

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert_eq!(response["summary"]["diagnostics"]["errors"], 1);
    assert_eq!(response["summary"]["diagnostics"]["warnings"], 1);
    let details = diagnostics_details(&response);
    assert_eq!(
        details.len(),
        2,
        "only included warm diagnostics should surface: {response:#}"
    );
    assert!(details
        .iter()
        .all(|item| item["file"] == "pkg/src/included.ts"));
}

#[test]
fn inspect_diagnostics_extends_chain_applies_inherited_exclude() {
    let (_temp_dir, root) = fixture_project();
    write_file(
        &root,
        "packages/pkg/tsconfig.base.json",
        r#"{
          "exclude": ["src/**/*.test.ts"]
        }"#,
    );
    write_file(
        &root,
        "packages/pkg/tsconfig.json",
        r#"{
          "extends": "./tsconfig.base",
          "include": ["src/**/*.ts"]
        }"#,
    );
    write_file(
        &root,
        "packages/pkg/src/chain.test.ts",
        "import { test } from 'bun:test';\ntest('x', () => import.meta.dir);\n",
    );
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);

    let response = inspect_diagnostics_scope(&ctx, "packages/pkg/src/chain.test.ts");

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert!(
        diagnostics_details(&response).is_empty(),
        "exclude inherited through extends must be applied: {response:#}"
    );
}

#[test]
fn inspect_diagnostics_no_tsconfig_keeps_current_behavior() {
    let (_temp_dir, root) = fixture_project();
    write_file(&root, "package.json", "{\"name\":\"no-tsconfig\"}\n");
    write_file(&root, "src/plain.ts", "export const plain = 1;\n");
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);

    let response = inspect_diagnostics_scope(&ctx, "src/plain.ts");

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert_eq!(response["summary"]["diagnostics"]["errors"], 1);
    assert_eq!(
        diagnostics_details(&response).len(),
        1,
        "no-tsconfig files must not be skipped: {response:#}"
    );
}

#[test]
fn inspect_diagnostics_malformed_tsconfig_falls_through() {
    let (_temp_dir, root) = fixture_project();
    write_file(&root, "tsconfig.json", "{ this is not valid jsonc");
    write_file(&root, "src/malformed.ts", "export const malformed = 1;\n");
    let ctx = configured_context(&root);
    configure_fake_typescript_lsp(&ctx);

    let response = inspect_diagnostics_scope(&ctx, "src/malformed.ts");

    assert_eq!(response["success"], true, "inspect failed: {response:#}");
    assert_eq!(response["summary"]["diagnostics"]["errors"], 1);
    assert_eq!(
        diagnostics_details(&response).len(),
        1,
        "malformed tsconfig should fall through instead of skipping: {response:#}"
    );
}
