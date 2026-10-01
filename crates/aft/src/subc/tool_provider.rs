//! The v1 catalog and admission boundary, separate from plugin tool forwarding.

use std::collections::BTreeMap;

use cortexkit_role_tool_provider::{
    call::{check_call, SchemaPin},
    catalog::{
        composition_digest, schema_digest, CatalogAnswer, CatalogRequest, CatalogTool,
        SystemTextAnswer,
    },
    describe::{Major, RoleDescribe},
    errors,
};
use serde_json::{json, Value};
use subc_protocol::ErrorBody;

use super::manifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteRole {
    Legacy,
    ToolProviderV1,
}

/// Only an explicit bind declaration selects v1; a call's pins and harness do not.
/// The 0.27 transport has no role_versions field, so its binds pass None.
pub(super) fn route_role(
    versions: Option<&BTreeMap<String, String>>,
) -> Result<RouteRole, ErrorBody> {
    match versions.and_then(|versions| versions.get("tool-provider")) {
        None => Ok(RouteRole::Legacy),
        Some(version) if version == "v1" => Ok(RouteRole::ToolProviderV1),
        Some(_) => Err(ErrorBody::new(
            "unsupported_role_version",
            "AFT serves tool-provider versions [\"v1\"]",
        )
        .with_detail(json!({"versions": ["v1"]}))),
    }
}

pub(super) fn recognized_operation(op: &str) -> bool {
    matches!(
        op,
        "role.describe"
            | "tool.catalog"
            | "tool.call"
            | "tool.withdraw"
            | "late_results"
            | "late_results.ack"
    )
}

pub(super) fn describe() -> Value {
    serde_json::to_value(RoleDescribe {
        majors: vec![Major {
            version: "tool-provider/v1".into(),
            ops: vec![
                "role.describe".into(),
                "tool.catalog".into(),
                "tool.call".into(),
            ],
            stability: "alpha".into(),
        }],
        implementation_version: env!("CARGO_PKG_VERSION").into(),
        capabilities: vec![],
    })
    .expect("the commons role declaration is JSON")
}

// Each tool has explicit tags and result permissions: read and bash prohibit
// replacement of line-addressed output, while powershell permits replacement.
pub(super) const METADATA: &[(&str, &str, bool)] = &[
    ("status", "", false),
    ("bash", "shell.exec/v1", true),
    ("powershell", "shell.exec/v1", false),
    ("read", "code.read/v1", true),
    ("write", "code.edit/v1", false),
    ("edit", "code.edit/v1", false),
    ("apply_patch", "code.edit/v1", false),
    ("grep", "code.search/v1", false),
    ("glob", "code.search/v1", false),
    ("search", "code.search/v1", false),
    ("outline", "code.outline/v1", false),
    ("zoom", "code.outline/v1", false),
    ("inspect", "code.diagnostics/v1", false),
    ("callgraph", "code.callgraph/v1", false),
    ("conflicts", "aft:git.conflicts/v1", false),
    ("ast_search", "aft:code.ast_grep/v1", false),
    ("ast_replace", "aft:code.ast_grep/v1", false),
    ("delete", "code.files/v1", false),
    ("move", "code.files/v1", false),
    ("import", "aft:code.imports/v1", false),
    ("safety", "aft:safety/v1", false),
    ("bash_status", "shell.exec/v1", false),
    ("bash_kill", "shell.exec/v1", false),
    ("bash_write", "shell.exec/v1", false),
];

pub(super) fn tools(disabled: &[String], powershell_available: bool) -> Vec<CatalogTool> {
    let manifest = manifest::filter_manifest_tools(
        manifest::build_manifest_for_host(powershell_available),
        disabled,
    );
    manifest
        .provides
        .into_iter()
        .filter_map(|role| match role {
            subc_protocol::manifest::ProviderRole::ToolProvider { tools, .. } => Some(tools),
            _ => None,
        })
        .flatten()
        .map(|tool| {
            let (_, tag, restrict_replace) = METADATA
                .iter()
                .find(|(name, _, _)| *name == tool.name)
                .expect("every manifest tool has v1 metadata");
            let mut schema = tool.schema;
            schema
                .as_object_mut()
                .expect("tool schemas are objects")
                .remove("description");
            let digest = schema_digest(&schema).expect("embedded schema has a structural digest");
            let mut entry = CatalogTool::new(tool.name, digest, 1, schema);
            if !tag.is_empty() {
                entry.capabilities.push((*tag).into());
            }
            entry.description = tool.description;
            if *restrict_replace {
                entry.result_ops = Some(vec!["prepend".into(), "append".into()]);
            }
            entry
        })
        .collect()
}

pub(super) fn broca_text(names: &[&str], worker: bool) -> String {
    let has = |name| names.contains(&name);
    let mut text = String::from("# Agent File Tools\n\n");
    if has("search") {
        text.push_str(
            "Use search to locate code by concept, identifier, or literal before reading it.\n",
        );
    }
    if has("outline") {
        text.push_str("Use outline to map source structure before reading specific sections.\n");
    }
    if has("zoom") {
        text.push_str(
            "Use zoom to inspect named symbols, with callgraph: true for one-level calls.\n",
        );
    }
    if has("callgraph") {
        text.push_str(
            "Use callgraph for callers, impact, and multi-level execution or data-flow traces.\n",
        );
    }
    if has("inspect") {
        text.push_str("Use inspect after editing for fresh diagnostics; run the project's typecheck and relevant tests as the authoritative gates.\n");
    }
    if has("read") {
        text.push_str(
            "Use read for files and bounded line ranges, not shell commands to explore source.\n",
        );
    }
    if has("bash") {
        text.push_str("Use bash with wait: true for long commands. The provider waits for completion or the command deadline; no completion subscriber is required.\n");
    }
    if worker {
        text.push_str("As a worker, keep changes within your assigned scope, verify them, and report the result to your parent.\n");
    }
    text
}

pub(super) fn catalog(
    body: Value,
    disabled: &[String],
    powershell_available: bool,
) -> Result<Value, ErrorBody> {
    let request: CatalogRequest = serde_json::from_value(body)
        .map_err(|e| errors::invalid_request("arguments", e.to_string()))?;
    if request.preset.is_some() {
        return Err(errors::invalid_request(
            "preset",
            "AFT has no catalog presets",
        ));
    }
    if let Some((key, _)) = request.params.iter().next() {
        return Err(errors::invalid_request(
            &format!("params.{key}"),
            "unsupported catalog parameter",
        ));
    }
    let mut answer = CatalogAnswer::new("", "").with_tools(tools(disabled, powershell_available));
    let composition = request.composition.as_ref().map(|composition| {
        composition_digest(&Value::Object(composition.clone())).expect("composition is JSON")
    });
    answer.composition_digest = composition.clone();
    if let Some(item) = request.system_text {
        if item.preset != "broca" {
            return Err(errors::invalid_request(
                "system_text.preset",
                "AFT serves only broca system text",
            ));
        }
        for (key, value) in &item.params {
            if key != "worker" || !value.is_boolean() {
                return Err(errors::invalid_request(
                    &format!("system_text.params.{key}"),
                    "unsupported system-text parameter",
                ));
            }
        }
        let worker = item
            .params
            .get("worker")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let names: Vec<_> = answer.tools.iter().map(|tool| tool.name.as_str()).collect();
        let text = broca_text(&names, worker);
        let digest = composition_digest(&json!({"text": text})).expect("text is JSON");
        let mut rendered = SystemTextAnswer::new(&digest, &digest).with_text(text);
        rendered.composition_digest = composition;
        answer.system_text = Some(rendered);
    }
    // Exclude generation and catalog_digest to avoid hashing the digest itself.
    // The commons helper canonicalizes the remaining JSON before hashing it.
    let mut content = serde_json::to_value(&answer).expect("catalog is JSON");
    content.as_object_mut().unwrap().remove("generation");
    content.as_object_mut().unwrap().remove("catalog_digest");
    let digest = composition_digest(&content).expect("catalog has canonical bytes");
    answer.generation = digest.clone();
    answer.catalog_digest = digest.clone();
    if request.digest_only == Some(true) {
        return Ok(json!({"generation": digest, "catalog_digest": digest}));
    }
    Ok(serde_json::to_value(answer).expect("catalog is JSON"))
}

pub(super) fn admit(
    call: &cortexkit_role_tool_provider::call::ToolCallRequest,
    disabled: &[String],
    powershell_available: bool,
    session: &str,
    has_scope: bool,
    trusted: bool,
) -> Result<(), ErrorBody> {
    check_call(call)?;
    if session.is_empty() {
        return Err(errors::invalid_request(
            "session",
            "v1 execution requires a bound session",
        ));
    }
    if !has_scope {
        return Err(errors::invalid_request(
            "scope",
            "v1 execution requires a stamped scope",
        ));
    }
    if !METADATA.iter().any(|(name, _, _)| *name == call.name) {
        return Err(errors::unknown_tool(&call.name));
    }
    if crate::tool_gate::refusal_by_name("v1-admission", &call.name, disabled).is_some() {
        return Err(errors::tool_disabled(&call.name));
    }
    if call.name == "powershell" && !powershell_available {
        return Err(
            ErrorBody::new(errors::TOOL_UNAVAILABLE, "PowerShell is not available")
                .with_detail(json!({"tool": call.name, "reason": "under_review"})),
        );
    }
    if !trusted
        && matches!(
            call.name.as_str(),
            "bash" | "powershell" | "bash_status" | "bash_kill" | "bash_write"
        )
    {
        return Err(ErrorBody::new(
            errors::CAPABILITY_NOT_ADMITTED,
            "shell execution and observation require an admitted principal",
        ));
    }
    let tool = tools(&[], powershell_available)
        .into_iter()
        .find(|tool| tool.name == call.name)
        .expect("admitted tool has a schema");
    if let Some(encoded) = &call.schema_pin {
        let pin = SchemaPin::parse(encoded)
            .map_err(|error| errors::invalid_request("schema_pin", error.to_string()))?;
        if pin.schema_digest != tool.schema_digest {
            return Err(ErrorBody::new(errors::TOOL_SCHEMA_CHANGED, "schema pin is stale").with_detail(json!({"tool": call.name, "expected": pin.schema_digest, "current": tool.schema_digest})));
        }
        if pin.semantics != tool.semantics {
            return Err(ErrorBody::new(errors::TOOL_SEMANTICS_CHANGED, "semantics pin is stale").with_detail(json!({"tool": call.name, "expected": pin.semantics, "current": tool.semantics})));
        }
    }
    let arguments = call
        .arguments
        .as_object()
        .ok_or_else(|| errors::invalid_request("arguments", "arguments must be an object"))?;
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("embedded schemas have properties");
    for key in arguments.keys() {
        if !properties.contains_key(key) {
            return Err(errors::invalid_request(
                key,
                "argument is absent from the served schema",
            ));
        }
        if properties[key].get("x-ck-audience").and_then(Value::as_str) == Some("host") {
            return Err(errors::invalid_request(key, "host-only argument"));
        }
    }
    let validator = jsonschema::validator_for(&tool.input_schema)
        .expect("embedded schemas are valid JSON schemas");
    if let Some(error) = validator.iter_errors(&call.arguments).next() {
        let field = error.instance_path.to_string();
        return Err(errors::invalid_request(
            if field.is_empty() {
                "arguments"
            } else {
                field.trim_start_matches('/')
            },
            error.to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cortexkit_role_tool_provider::catalog::{check_flat_schema, structural_schema};
    use std::collections::BTreeSet;

    fn call(name: &str, arguments: Value) -> cortexkit_role_tool_provider::call::ToolCallRequest {
        serde_json::from_value(json!({"name": name, "arguments": arguments})).unwrap()
    }

    #[test]
    fn admission_requires_session() {
        let error = admit(&call("status", json!({})), &[], true, "", true, true).unwrap_err();
        assert_eq!(error.code, errors::INVALID_REQUEST);
        assert_eq!(error.detail.unwrap()["field"], "session");
    }

    #[test]
    fn admission_refuses_arguments_outside_served_schema() {
        let error = admit(
            &call("status", json!({"not_served": true})),
            &[],
            true,
            "session",
            true,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, errors::INVALID_REQUEST);
        assert_eq!(error.detail.unwrap()["field"], "not_served");
    }

    #[test]
    fn admission_disables_companions_regardless_of_task_spelling() {
        for name in ["bash_status", "bash_kill", "bash_write"] {
            for arguments in [
                json!({"task_id":"x"}),
                json!({"taskId":"x"}),
                json!({"task_id":"x", "taskId":"x"}),
            ] {
                let error = admit(
                    &call(name, arguments),
                    &[name.into()],
                    true,
                    "session",
                    true,
                    true,
                )
                .unwrap_err();
                assert_eq!(error.code, errors::TOOL_DISABLED, "{name}");
            }
        }
    }

    #[test]
    fn admission_refuses_malformed_schema_pin() {
        let mut request = call("status", json!({}));
        request.schema_pin = Some("malformed".into());
        let error = admit(&request, &[], true, "session", true, true).unwrap_err();
        assert_eq!(error.code, errors::INVALID_REQUEST);
        assert_eq!(error.detail.unwrap()["field"], "schema_pin");
    }

    #[test]
    fn slice_a_declaration_matches_independent_fixture_without_normalizing_other_fields() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tool_provider_conformance.json"
        ))
        .unwrap();
        let mut actual = describe();
        assert_eq!(actual["implementation_version"], env!("CARGO_PKG_VERSION"));
        actual["implementation_version"] = json!("<built-package-version>");
        assert_eq!(actual, fixtures["slice_a"]["declaration"]);
    }

    #[test]
    fn bind_role_versions_are_explicit_and_unknown_versions_fail_closed() {
        assert_eq!(route_role(None).unwrap(), RouteRole::Legacy);
        assert_eq!(
            route_role(Some(&BTreeMap::from([("other-role".into(), "v2".into())]))).unwrap(),
            RouteRole::Legacy
        );
        for version in ["v1", "v2", "1", ""] {
            let versions = BTreeMap::from([("tool-provider".into(), version.into())]);
            let result = route_role(Some(&versions));
            if version == "v1" {
                assert_eq!(result.unwrap(), RouteRole::ToolProviderV1);
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, "unsupported_role_version");
                assert_eq!(error.detail.unwrap()["versions"], json!(["v1"]));
            }
        }
        assert!(
            serde_json::from_value::<BTreeMap<String, String>>(json!({"tool-provider": 1}))
                .is_err()
        );
    }

    #[test]
    fn metadata_is_a_bijection_with_host_manifest_and_disable_keys() {
        let expected: BTreeSet<_> = METADATA.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(expected.len(), 24);
        for available in [false, true] {
            let inventory = tools(&[], available);
            assert_eq!(inventory.len(), if available { 24 } else { 23 });
            for tool in &inventory {
                assert!(expected.contains(tool.name.as_str()));
                assert_eq!(tool.semantics, 1);
                assert_eq!(tool.capabilities.is_empty(), tool.name == "status");
                assert_eq!(
                    tool.result_ops,
                    matches!(tool.name.as_str(), "read" | "bash")
                        .then(|| vec!["prepend".into(), "append".into()])
                );
                assert!(tool.description.is_some());
                assert!(tool.input_schema.get("description").is_none());
                assert!(check_flat_schema(&tool.input_schema).is_ok());
                assert_eq!(
                    schema_digest(&tool.input_schema).unwrap(),
                    tool.schema_digest
                );
                if let Some(key) = crate::tool_gate::canonical_tool_name(&tool.name) {
                    let filtered = tools(&[key.into()], available);
                    assert!(!filtered.iter().any(|entry| entry.name == tool.name));
                    assert_eq!(filtered.len(), inventory.len() - 1);
                } else {
                    assert_eq!(tool.name, "status");
                }
            }
            if available {
                assert_eq!(
                    inventory
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<BTreeSet<_>>(),
                    expected
                );
            }
        }
    }

    const REGENERATE_CATALOG: &str = "cargo test -p agent-file-tools --lib regenerate_tool_provider_catalog --locked -- --ignored";

    #[test]
    #[ignore = "explicit fixture regeneration, not a verification gate"]
    fn regenerate_tool_provider_catalog() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tool_provider_catalog.json");
        let mut fixtures: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for fixture in fixtures.as_array_mut().unwrap() {
            let disabled: Vec<String> =
                serde_json::from_value(fixture["disabled_tools"].clone()).unwrap();
            fixture["reply"] = catalog(
                fixture["request"].clone(),
                &disabled,
                fixture["powershell_available"].as_bool().unwrap(),
            )
            .unwrap();
        }
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&fixtures).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn full_catalog_goldens_and_digest_only_have_exact_identity() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tool_provider_catalog.json"
        ))
        .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let disabled: Vec<String> =
                serde_json::from_value(fixture["disabled_tools"].clone()).unwrap();
            let available = fixture["powershell_available"].as_bool().unwrap();
            let request = fixture["request"].clone();
            let actual = catalog(request.clone(), &disabled, available).unwrap();
            assert!(
                serde_json::to_vec(&actual).unwrap()
                    == serde_json::to_vec(&fixture["reply"]).unwrap(),
                "full catalog bytes differ for {}; regenerate with: {}",
                fixture["name"],
                REGENERATE_CATALOG
            );
            assert_eq!(actual["generation"], actual["catalog_digest"]);
            let mut digest_request = request;
            digest_request["digest_only"] = json!(true);
            assert_eq!(
                catalog(digest_request, &disabled, available).unwrap(),
                json!({"generation": actual["generation"], "catalog_digest": actual["catalog_digest"]})
            );
            assert_eq!(
                catalog(fixture["request"].clone(), &disabled, available).unwrap(),
                actual
            );
        }
    }

    #[test]
    fn broca_literals_and_preset_refusals() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tool_provider_system_text.json"
        ))
        .unwrap();
        for (disabled, worker, fixture) in [
            (vec![], false, "all_tools_worker_false"),
            (vec![], true, "all_tools_worker_true"),
            (vec!["bash".into()], false, "bash_disabled_worker_false"),
        ] {
            let answer = catalog(
                json!({"system_text": {"preset": "broca", "params": {"worker": worker}}}),
                &disabled,
                true,
            )
            .unwrap();
            assert_eq!(answer["system_text"]["text"], fixtures[fixture]);
            let text = answer["system_text"]["text"].as_str().unwrap();
            assert!(!text.contains("bash_watch"));
            assert!(!text.contains("Long-running-commands"));
            assert_eq!(text.contains("wait: true"), disabled.is_empty());
        }
        for preset in ["opencode", "pi", "unknown", ""] {
            assert_eq!(
                errors::invalid_request_field(
                    &catalog(json!({"system_text": {"preset": preset}}), &[], true).unwrap_err()
                ),
                Some("system_text.preset")
            );
        }
    }

    #[test]
    fn presentation_and_nested_description_edits_do_not_move_schema_pins() {
        let mut raw: Value =
            serde_json::from_str(include_str!("../subc_tool_schemas.json")).unwrap();
        let raw_bash_digest = schema_digest(&raw["bash"]).unwrap();
        let served = tools(&[], true)
            .into_iter()
            .find(|tool| tool.name == "bash")
            .unwrap();
        assert_ne!(raw_bash_digest, served.schema_digest);
        let mut changed = served.input_schema.clone();
        changed["properties"]["command"]["description"] = json!("different nested prose");
        assert_eq!(schema_digest(&changed).unwrap(), served.schema_digest);
        assert_eq!(
            structural_schema(&changed),
            structural_schema(&served.input_schema)
        );
        raw["bash"]["description"] = json!("different top-level prose");
        assert_eq!(schema_digest(&raw["bash"]).unwrap(), raw_bash_digest);
        assert!(served.input_schema["properties"]["description"].is_object());
    }

    #[test]
    fn v1_refusal_precedence_and_served_arguments() {
        for name in ["bash_status", "bash_kill", "bash_write"] {
            let disabled = vec![name.into()];
            for arguments in [
                json!({"task_id": "task"}),
                json!({"taskId": "task"}),
                json!({"taskId": "task", "task_id": "task"}),
            ] {
                assert_eq!(
                    admit(
                        &call(name, arguments),
                        &disabled,
                        true,
                        "session",
                        true,
                        true
                    )
                    .unwrap_err()
                    .code,
                    "tool_disabled"
                );
            }
        }
        for name in [
            "configure",
            "undo_preview",
            "bash_drain_completions",
            "aft_zoom",
            "missing",
        ] {
            assert_eq!(
                admit(&call(name, json!({})), &[], true, "session", true, true)
                    .unwrap_err()
                    .code,
                "unknown_tool"
            );
        }
        for name in ["bash", "powershell"] {
            assert_eq!(
                errors::invalid_request_field(
                    &admit(
                        &call(name, json!({"command": "echo x"})),
                        &[],
                        true,
                        "",
                        true,
                        true
                    )
                    .unwrap_err()
                ),
                Some("session")
            );
            assert_eq!(
                errors::invalid_request_field(
                    &admit(
                        &call(name, json!({"command": "echo x"})),
                        &[],
                        true,
                        "session",
                        false,
                        true
                    )
                    .unwrap_err()
                ),
                Some("scope")
            );
        }
        for key in [
            "foreground_orchestrate",
            "block_to_completion",
            "shell",
            "unknown",
        ] {
            let mut arguments = json!({"command": "echo x"});
            arguments[key] = if key == "shell" {
                json!("powershell")
            } else {
                json!(true)
            };
            assert_eq!(
                errors::invalid_request_field(
                    &admit(&call("bash", arguments), &[], true, "session", true, true).unwrap_err()
                ),
                Some(key)
            );
        }
        for arguments in [
            json!({}),
            json!({"command": 1}),
            json!({"command": "echo", "timeout": 0}),
            json!({"command": "echo", "ptyRows": 61}),
        ] {
            assert_eq!(
                admit(&call("bash", arguments), &[], true, "session", true, true)
                    .unwrap_err()
                    .code,
                "invalid_request"
            );
        }
        assert!(admit(
            &call("bash", json!({"command": "echo", "wait": true})),
            &[],
            true,
            "session",
            true,
            true
        )
        .is_ok());
        assert_eq!(
            admit(
                &call("powershell", json!({"command": "echo"})),
                &[],
                false,
                "session",
                true,
                true
            )
            .unwrap_err()
            .code,
            "tool_unavailable"
        );
        assert_eq!(
            admit(
                &call("bash", json!({"command": "echo", "sandbox": "host"})),
                &[],
                true,
                "session",
                true,
                false
            )
            .unwrap_err()
            .code,
            "capability_not_admitted"
        );
        let schema = tools(&[], true)
            .into_iter()
            .find(|tool| tool.name == "bash")
            .unwrap();
        for (digest, semantics, expected) in [
            (schema.schema_digest.as_str(), 1, None),
            (
                "0000000000000000000000000000000000000000000000000000000000000000",
                2,
                Some("tool_schema_changed"),
            ),
            (
                schema.schema_digest.as_str(),
                2,
                Some("tool_semantics_changed"),
            ),
        ] {
            let mut request = call("bash", json!({"command": "echo"}));
            request.schema_pin = Some(SchemaPin::new("bash", digest, semantics).encode().unwrap());
            let result = admit(&request, &[], true, "session", true, true);
            match expected {
                Some(code) => assert_eq!(result.unwrap_err().code, code),
                None => assert!(result.is_ok()),
            }
        }
    }
}

#[cfg(test)]
mod route_tests {
    use super::super::*;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ACTIONS: AtomicUsize = AtomicUsize::new(0);
    static EXCHANGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn exchange(
        body: Value,
        role: RouteRole,
        session: &str,
        disabled: Vec<String>,
    ) -> WriterFrame {
        let (_dir, root) = test_support::test_root("tool-provider-admission");
        let ctx = test_support::test_ctx();
        ctx.mark_database_runtime_initializing_for_test();
        let executor = Arc::new(Executor::new());
        assert!(executor.register_actor(root.clone(), ctx));
        let identity = RouteIdentity(Arc::new(RouteIdentityData {
            root: root.clone(), project_root: root.as_path().into(), harness: "runner".into(), session: session.into(), role,
            trust: BindTrust::FirstParty, spawn_principal: AuthenticatedPrincipal::FirstParty,
            consumer_elicitation_capable: false, disabled_tools: Arc::new(disabled),
            scope: Some(serde_json::from_value(json!({"owner": {"kind": "direct"}, "ref": "scope", "scope_epoch": 1, "kind": "head", "owner_authorized": true})).unwrap()),
        }));
        let routes = HashMap::from([(route_key(41, 1), identity)]);
        let frame = Frame::build(
            FrameType::Request,
            control_flags(),
            41,
            1,
            7,
            serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        let (writer, mut replies) = mpsc::channel(8);
        let (bash_tx, _bash_rx) = mpsc::channel(8);
        let (touch_tx, _touch_rx) = mpsc::channel(8);
        let (deferred_tx, _deferred_rx) = mpsc::unbounded_channel();
        handle_tool_call(
            &writer,
            &frame,
            PhaseTrace::new(Instant::now()),
            &routes,
            &HashMap::new(),
            &mut HashMap::new(),
            &executor,
            &Arc::default(),
            &Arc::new(AtomicUsize::new(0)),
            &Arc::new(Notify::new()),
            &PersistentCancelSignal::new(),
            &bash_tx,
            &touch_tx,
            &Arc::new(DispatchPathMetrics::new()),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut 1,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
            &mut HashMap::new(),
            |request, _| {
                ACTIONS.fetch_add(1, Ordering::SeqCst);
                Response::success(request.id, json!({}))
            },
            &deferred_tx,
            false,
            1024 * 1024,
            &drain::ModuleDrainWindow::default(),
        )
        .await
        .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(3), replies.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            reply.header.ty,
            FrameType::Response | FrameType::Error | FrameType::StreamEnd
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), replies.recv())
                .await
                .is_err()
        );
        reply
    }

    #[test]
    fn v1_standard_edit_grammar_does_not_register_or_change_plugin_hashline_bindings() {
        let (_dir, root) = test_support::test_root("v1-edit-grammar");
        let arguments =
            json!({"filePath": "file.rs", "edits": [{"oldString":"old", "newString":"new"}]});
        for session in ["session", crate::protocol::DEFAULT_SESSION_ID] {
            for before in [false, true] {
                let ctx = test_support::test_ctx();
                ctx.update_config(|config| config.hashline_enabled = true);
                let registration = || {
                    ctx.hashline_bindings().register(
                        root.as_path(),
                        session,
                        crate::hashline::integration::RegistrationRequest {
                            configured_enabled: true,
                            edit_slot_survives: true,
                            read_slot_survives: true,
                        },
                    )
                };
                if before {
                    registration();
                }
                let call_context = ToolCallContext {
                    project_root: root.as_path().into(),
                    session_id: Some(session.into()),
                    request_id: "standard-edit".into(),
                    diagnostics_on_edit: false,
                    preview: false,
                    edit_slot_survives: None,
                    report_registration_downgrade: false,
                    standard_edit_grammar: true,
                    disabled_tools: Some(Arc::default()),
                    worker_session: false,
                };
                let result = prepare_tool_call(
                    "edit",
                    arguments.clone(),
                    &crate::subc_format::FormatContext::default(),
                    &call_context,
                    &ctx,
                    None,
                )
                .unwrap();
                assert_eq!(result.request.command, "batch");
                assert_eq!(result.request.params["edits"][0]["match"], "old");
                assert_eq!(result.request.params["edits"][0]["replacement"], "new");
                registration();
                let capture = ctx.hashline_bindings().capture(root.as_path(), session);
                assert!(crate::hashline::integration::effective_for_capture(
                    capture.as_ref()
                ));
                let mut legacy_context = call_context.clone();
                legacy_context.standard_edit_grammar = false;
                assert!(
                    prepare_tool_call(
                        "edit",
                        arguments.clone(),
                        &crate::subc_format::FormatContext::default(),
                        &legacy_context,
                        &ctx,
                        None
                    )
                    .is_err(),
                    "legacy hashline binding unexpectedly accepts standard edits"
                );
                let capture = ctx.hashline_bindings().capture(root.as_path(), session);
                assert!(crate::hashline::integration::effective_for_capture(
                    capture.as_ref()
                ));
            }
        }
    }

    #[tokio::test]
    async fn catalog_queues_behind_reads_but_not_database_or_cold_builds() {
        let (_dir, root) = test_support::test_root("catalog-pure-read");
        let (_heavy_dir, heavy_root) = test_support::test_root("catalog-cold-build");
        let executor = Arc::new(Executor::with_config(crate::executor::ExecutorConfig {
            pool_size: 6,
            read_cap: 1,
            actor_cap: 2,
            heavy_permits: 1,
            drr_quantum: 1,
        }));
        let ctx = test_support::test_ctx();
        ctx.mark_database_runtime_initializing_for_test();
        assert!(executor.register_actor(root.clone(), ctx.clone()));
        assert!(executor.register_actor(heavy_root.clone(), test_support::test_ctx()));
        let (heavy_started, started) = std::sync::mpsc::channel();
        let (release_heavy, heavy_release) = std::sync::mpsc::channel();
        let heavy = executor.submit_async(
            heavy_root,
            Lane::HeavyInit,
            "held-cold-build".into(),
            Box::new(move |_| {
                heavy_started.send(()).unwrap();
                heavy_release.recv().unwrap();
                Response::success("heavy", json!({}))
            }),
        );
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let (read_started, started) = std::sync::mpsc::channel();
        let (release_read, read_release) = std::sync::mpsc::channel();
        let read = executor.submit_async(
            root.clone(),
            Lane::PureRead,
            "held-read".into(),
            Box::new(move |_| {
                read_started.send(()).unwrap();
                read_release.recv().unwrap();
                Response::success("read", json!({}))
            }),
        );
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let (writer, mut replies) = mpsc::channel(8);
        let frame = Frame::build(FrameType::Request, control_flags(), 41, 1, 7, vec![]).unwrap();
        submit_provider_read(
            &writer,
            &frame,
            test_support::route_identity(&root, ""),
            &executor,
            &Arc::default(),
            &Arc::new(DispatchPathMetrics::new()),
            "tool.catalog",
            json!({}),
        )
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), replies.recv())
                .await
                .is_err(),
            "catalogs must share existing read permits"
        );
        release_read.send(()).unwrap();
        assert!(read.await.unwrap().success);
        let reply = tokio::time::timeout(Duration::from_secs(3), replies.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.header.ty, FrameType::Response);
        assert!(ctx.database_runtime_pending("write"));
        assert_eq!(executor.heavy_permits(), 1);
        release_heavy.send(()).unwrap();
        assert!(heavy.await.unwrap().success);
    }

    #[tokio::test]
    async fn legacy_provider_calls_are_unsupported_without_actions() {
        let _guard = EXCHANGE_LOCK.lock().await;
        ACTIONS.store(0, Ordering::SeqCst);
        let mut failures = Vec::new();
        for op in ["tool.call", "tool.withdraw"] {
            let reply = exchange(
                json!({"op":op, "name":"status", "arguments":{}}),
                RouteRole::Legacy,
                "session",
                vec![],
            )
            .await;
            let actions = ACTIONS.load(Ordering::SeqCst);
            if reply.header.ty != FrameType::Error {
                failures.push(format!(
                    "{op}: expected Error unsupported_operation, got {:?}; actions={actions}",
                    reply.header.ty
                ));
            } else {
                let error: subc_protocol::ErrorBody = serde_json::from_slice(&reply.body).unwrap();
                if error.code != "unsupported_operation" || actions != 0 {
                    failures.push(format!("{op}: code={}, actions={actions}", error.code));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
    }

    #[tokio::test]
    async fn legacy_route_keeps_ordinary_management_and_opaque_pin_behavior() {
        let _guard = EXCHANGE_LOCK.lock().await;
        ACTIONS.store(0, Ordering::SeqCst);
        for body in [
            json!({"name":"status", "arguments":{}}),
            json!({"name":"status", "arguments":{}, "call_key":"legacy-key", "schema_pin":"opaque-pin"}),
        ] {
            let reply = exchange(body, RouteRole::Legacy, "session", vec![]).await;
            assert_eq!(reply.header.ty, FrameType::Response);
            let response: Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(response["isError"], false);
            assert_eq!(response["structuredContent"]["success"], true);
        }
        assert_eq!(ACTIONS.load(Ordering::SeqCst), 2);
        let reply = exchange(
            json!({"op": crate::commands::health_digest::HEALTH_DIGEST_OPERATION, "params":{}}),
            RouteRole::Legacy,
            "session",
            vec![],
        )
        .await;
        assert_eq!(reply.header.ty, FrameType::Response);
    }

    #[test]
    fn legacy_value_decoding_collapses_duplicate_fields_unlike_base_bytes() {
        let bytes = br#"{"name":"write","name":"status","arguments":{}}"#;
        assert!(serde_json::from_slice::<RouteRequest>(bytes).is_err());
        let envelope: Value = serde_json::from_slice(bytes).unwrap();
        let RouteRequest::ToolCall(call) = decode_legacy_route_request(envelope).unwrap() else {
            panic!("expected tool call")
        };
        assert_eq!(call.name, "status");
    }

    #[test]
    fn legacy_decoding_preserves_base_commit_identity() {
        // Legacy decoding uses op/params only after the untagged request fails.
        // A legacy schema_pin is an opaque token, not a parsed v1 schema pin.
        let health = crate::commands::health_digest::HEALTH_DIGEST_OPERATION;
        let cases = [
            (
                json!({"name":"status","arguments":{"extra":"雪"},"preview":true,"worker_session":true,"edit_slot_survives":true}),
                "status",
                json!({"extra":"雪"}),
                true,
            ),
            (
                json!({"op":health,"params":{"limit":3}}),
                health,
                json!({"limit":3}),
                false,
            ),
            (
                json!({"name":"status","arguments":{},"call_key":"legacy-key","schema_pin":"opaque-pin"}),
                "status",
                json!({}),
                false,
            ),
        ];
        for (body, name, arguments, flags) in cases {
            let decoded = decode_legacy_route_request(body.clone()).unwrap();
            let RouteRequest::ToolCall(call) = decoded else {
                panic!("expected tool call")
            };
            assert_eq!(call.name, name);
            assert_eq!(call.arguments, arguments);
            assert_eq!(call.preview, flags);
            assert_eq!(call.worker_session, flags);
            assert_eq!(call.edit_slot_survives, flags.then_some(true));
            assert_eq!(
                call.call_key.as_deref(),
                body.get("call_key").and_then(Value::as_str)
            );
            assert_eq!(
                call.schema_pin.as_deref(),
                body.get("schema_pin").and_then(Value::as_str)
            );
            if let Ok(base) =
                serde_json::from_slice::<RouteRequest>(&serde_json::to_vec(&body).unwrap())
            {
                assert_eq!(
                    format!("{base:?}"),
                    format!("{:?}", RouteRequest::ToolCall(call))
                );
            } else {
                assert_eq!(name, health);
            }
        }
        assert!(matches!(
            decode_legacy_route_request(json!({"op":"bg_events", "name":"status"})).unwrap(),
            RouteRequest::BgEvents(_)
        ));
    }

    #[tokio::test]
    async fn recognized_operations_and_v1_refusals_never_dispatch_legacy_actions() {
        let _guard = EXCHANGE_LOCK.lock().await;
        ACTIONS.store(0, Ordering::SeqCst);
        for op in [
            "role.describe",
            "tool.catalog",
            "tool.call",
            "tool.withdraw",
            "late_results",
            "late_results.ack",
        ] {
            for malformed in [false, true] {
                let body = json!({"op": op, "params": if malformed {json!(false)} else {json!({})}, "name": "write", "arguments": {"filePath": "action", "content": "mutate"}});
                let reply = exchange(body, RouteRole::Legacy, "session", vec![]).await;
                assert_eq!(
                    ACTIONS.load(Ordering::SeqCst),
                    0,
                    "{op} fell through to legacy"
                );
                if !matches!(op, "role.describe" | "tool.catalog") {
                    assert_eq!(reply.header.ty, FrameType::Error);
                }
            }
        }
        for name in ["bash_status", "bash_kill", "bash_write"] {
            for args in [
                json!({"task_id": "x"}),
                json!({"taskId": "x"}),
                json!({"task_id":"x","taskId":"x"}),
            ] {
                let reply = exchange(
                    json!({"name":name,"arguments":args}),
                    RouteRole::ToolProviderV1,
                    "session",
                    vec![name.into()],
                )
                .await;
                let error: subc_protocol::ErrorBody = serde_json::from_slice(&reply.body).unwrap();
                assert_eq!(error.code, "tool_disabled");
                assert_eq!(ACTIONS.load(Ordering::SeqCst), 0);
            }
        }
        let request = json!({"name":"write", "arguments":{"filePath":"action","content":"mutate","foreground_orchestrate":true}});
        let reply = exchange(request.clone(), RouteRole::ToolProviderV1, "", vec![]).await;
        assert_eq!(
            serde_json::from_slice::<subc_protocol::ErrorBody>(&reply.body)
                .unwrap()
                .detail
                .unwrap()["field"],
            "session"
        );
        let reply = exchange(
            request.clone(),
            RouteRole::ToolProviderV1,
            "session",
            vec![],
        )
        .await;
        assert_eq!(
            serde_json::from_slice::<subc_protocol::ErrorBody>(&reply.body)
                .unwrap()
                .detail
                .unwrap()["field"],
            "foreground_orchestrate"
        );
        assert_eq!(ACTIONS.load(Ordering::SeqCst), 0);
        // A pure legacy tool is deliberately allowed to carry extra arguments and
        // a missing session, without requiring a working database.
        let reply = exchange(
            json!({"name":"status","arguments":{"foreground_orchestrate":true}}),
            RouteRole::Legacy,
            "",
            vec![],
        )
        .await;
        assert_eq!(reply.header.ty, FrameType::Response);
        assert_eq!(ACTIONS.load(Ordering::SeqCst), 1);
    }
}
