//! Published conformance cases driven through the real aft --subc process.
use async_trait::async_trait;
use cortexkit_role_harness::{
    Harness, HarnessError, KillPoint, KillReport, PointDeclaration, RouteStamp, Trigger,
};
use cortexkit_role_tool_provider_conformance::{
    CallSpec, Capability, Exchange, ObservedFrame, RouteFailure, ScopedPrincipals,
    ToolProviderSubject, ToolRoute,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use subc_protocol::session::{ModuleControlRequest, ModuleControlResponse};
use subc_protocol::{
    BindIdentity, Flags, Frame, FrameType, ModuleHelloAckBody, Principal, Priority, RouteTarget,
    PROTOCOL_VERSION,
};
use subc_transport::{
    authenticate_server,
    connection_file::{self, ConnectionInfo, Endpoint, SCHEMA_VERSION},
    read_frame, write_frame,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Mutex as AsyncMutex,
};

struct Subject;
struct Process {
    child: Mutex<Child>,
    stream: Arc<AsyncMutex<Wire>>,
    root: PathBuf,
}
struct Wire {
    stream: TcpStream,
    next_corr: u64,
    next_channel: u16,
}
#[derive(Clone)]
struct Route {
    wire: Arc<AsyncMutex<Wire>>,
    channel: u16,
}
impl Drop for Process {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}
fn flags() -> Flags {
    Flags::new(false, Priority::Interactive, false)
}
fn harness_error(e: impl std::fmt::Display) -> HarnessError {
    HarnessError::new(e.to_string())
}
async fn next_frame(stream: &mut TcpStream) -> Result<Frame, HarnessError> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(30), read_frame(stream))
            .await
            .map_err(harness_error)?
            .map_err(harness_error)?
            .ok_or_else(|| HarnessError::new("module EOF"))?;
        if frame.header.ty == FrameType::Ping {
            let pong = Frame::build(FrameType::Pong, flags(), 0, 0, frame.header.corr, vec![])
                .map_err(harness_error)?;
            write_frame(stream, &pong).await.map_err(harness_error)?;
            continue;
        }
        if frame.header.ty == FrameType::Push {
            continue;
        }
        return Ok(frame);
    }
}
#[async_trait]
impl Harness for Subject {
    type Handle = Process;
    type Route = Route;
    fn declared_points(&self) -> Vec<PointDeclaration> {
        vec![]
    }
    async fn spawn(&self, root: &Path) -> Result<Process, HarnessError> {
        std::fs::create_dir_all(root.join("project/.cortexkit")).map_err(harness_error)?;
        std::fs::write(root.join("project/.cortexkit/aft.jsonc"), serde_json::to_vec(&json!({"disabled_tools": ["aft_outline"], "callgraph_store": false, "search_index": false, "semantic_search": false})).unwrap()).map_err(harness_error)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(harness_error)?;
        let connection = ConnectionInfo {
            schema: SCHEMA_VERSION,
            wire_version: Some(PROTOCOL_VERSION),
            endpoints: vec![Endpoint {
                host: "127.0.0.1".into(),
                port: listener.local_addr().map_err(harness_error)?.port(),
            }],
            key: vec![0x42; subc_transport::KEY_LEN],
            daemon_id: [0x24; subc_transport::DAEMON_ID_LEN],
            pid: std::process::id(),
            daemon_ver: "tool-provider-conformance".into(),
        };
        let connection_path = root.join("connection.json");
        connection_file::write_atomic(&connection_path, &connection).map_err(harness_error)?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_aft"));
        command
            .arg("--subc")
            .arg(&connection_path)
            .env_remove("SUBC_MODULE_ID")
            .env_remove("SUBC_LAUNCH_NONCE")
            .env_remove("AFT_STORAGE_DIR")
            .env("AFT_TEST_DISABLE_FILE_WATCHER", "1")
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(harness_error)?;
        // Own the child before awaiting the handshake so failed setup also reaps it.
        struct ChildGuard(Option<Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let mut guard = ChildGuard(Some(child));
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .map_err(harness_error)?
            .map_err(harness_error)?;
        authenticate_server(
            &mut stream,
            &[0x42; subc_transport::KEY_LEN],
            &[0x24; subc_transport::DAEMON_ID_LEN],
            "tool-provider-conformance",
            Duration::from_secs(5),
        )
        .await
        .map_err(harness_error)?;
        let hello = next_frame(&mut stream).await?;
        if hello.header.ty != FrameType::Hello {
            return Err(HarnessError::new("expected module Hello"));
        }
        let ack = ModuleHelloAckBody {
            negotiated_ver: PROTOCOL_VERSION,
            subc_ops: vec![],
            subc_capabilities: vec![],
            storage: None,
            machine_id: None,
        };
        write_frame(
            &mut stream,
            &Frame::build(
                FrameType::HelloAck,
                flags(),
                0,
                0,
                hello.header.corr,
                serde_json::to_vec(&ack).unwrap(),
            )
            .map_err(harness_error)?,
        )
        .await
        .map_err(harness_error)?;
        Ok(Process {
            child: Mutex::new(guard.0.take().unwrap()),
            stream: Arc::new(AsyncMutex::new(Wire {
                stream,
                next_corr: 100,
                next_channel: 1,
            })),
            root: root.to_path_buf(),
        })
    }
    async fn route(&self, handle: &Process, stamp: &RouteStamp) -> Result<Route, HarnessError> {
        assert_eq!(stamp.principal, "direct");
        assert!(stamp.scope.is_none());
        bind_route(
            handle,
            &handle.root.join("project"),
            "runner",
            "conformance-session",
        )
        .await
    }
    async fn kill_at(
        &self,
        _handle: Process,
        _point: &KillPoint,
        _trigger: Trigger<'_>,
    ) -> Result<KillReport, HarnessError> {
        Err(HarnessError::new(
            "Slice A does not declare durable kill points",
        ))
    }
    async fn restart(&self, root: &Path) -> Result<Process, HarnessError> {
        self.spawn(root).await
    }
}
async fn bind_route(
    handle: &Process,
    project: &Path,
    harness: &str,
    session: &str,
) -> Result<Route, HarnessError> {
    let mut wire = handle.stream.lock().await;
    let channel = wire.next_channel;
    wire.next_channel += 1;
    let corr = wire.next_corr;
    wire.next_corr += 1;
    let bind = ModuleControlRequest::RouteBind {
        route_channel: channel,
        epoch: 1,
        target: RouteTarget::ToolProvider {
            module_id: "aft".into(),
        },
        identity: BindIdentity::new(project, harness, session),
        principal: Some(Principal::Direct),
        consumer_capabilities: None,
        admission_facts: None,
        scope: None,
    };
    write_frame(
        &mut wire.stream,
        &Frame::build(
            FrameType::Request,
            flags(),
            0,
            0,
            corr,
            serde_json::to_vec(&bind).unwrap(),
        )
        .map_err(harness_error)?,
    )
    .await
    .map_err(harness_error)?;
    let ack = next_frame(&mut wire.stream).await?;
    if ack.header.ty != FrameType::Response || ack.header.corr != corr {
        return Err(HarnessError::new(format!(
            "unexpected bind frame: {:?} {}",
            ack.header.ty,
            String::from_utf8_lossy(&ack.body)
        )));
    }
    let response: ModuleControlResponse =
        serde_json::from_slice(&ack.body).map_err(harness_error)?;
    if response != (ModuleControlResponse::RouteBindAck {}) {
        return Err(HarnessError::new("no bind acknowledgement"));
    }
    Ok(Route {
        wire: handle.stream.clone(),
        channel,
    })
}
impl Route {
    async fn exchange(&self, body: Value, cancel: bool) -> Result<Exchange, RouteFailure> {
        self.exchange_inner(body, cancel)
            .await
            .map_err(|error| RouteFailure::new(error.to_string()))
    }
    async fn exchange_inner(&self, body: Value, cancel: bool) -> Result<Exchange, HarnessError> {
        let mut wire = self.wire.lock().await;
        let corr = wire.next_corr;
        wire.next_corr += 1;
        write_frame(
            &mut wire.stream,
            &Frame::build(
                FrameType::Request,
                flags(),
                self.channel,
                1,
                corr,
                serde_json::to_vec(&body).unwrap(),
            )
            .map_err(harness_error)?,
        )
        .await
        .map_err(harness_error)?;
        if cancel {
            tokio::time::sleep(Duration::from_millis(100)).await;
            write_frame(
                &mut wire.stream,
                &Frame::build(FrameType::Cancel, flags(), self.channel, 1, corr, vec![])
                    .map_err(harness_error)?,
            )
            .await
            .map_err(harness_error)?;
        }
        let mut exchange = Exchange::default();
        let mut terminal = false;
        loop {
            let frame = if terminal {
                match tokio::time::timeout(Duration::from_millis(100), next_frame(&mut wire.stream))
                    .await
                {
                    Ok(frame) => frame?,
                    Err(_) => break,
                }
            } else {
                next_frame(&mut wire.stream).await?
            };
            if frame.header.channel != self.channel || frame.header.corr != corr {
                return Err(HarnessError::new(
                    "unexpected correlation during conformance exchange",
                ));
            }
            let observed = match frame.header.ty {
                FrameType::Response => ObservedFrame::Response(
                    serde_json::from_slice(&frame.body).map_err(harness_error)?,
                ),
                FrameType::Error => ObservedFrame::Error(
                    serde_json::from_slice(&frame.body).map_err(harness_error)?,
                ),
                FrameType::StreamEnd => ObservedFrame::StreamEnd,
                _ => {
                    ObservedFrame::Data(serde_json::from_slice(&frame.body).map_err(harness_error)?)
                }
            };
            terminal |= observed.is_terminal();
            exchange.frames.push(observed);
        }
        Ok(exchange)
    }
}
#[async_trait]
impl ToolRoute for Route {
    async fn request(&self, body: Value) -> Result<Exchange, RouteFailure> {
        self.exchange(body, false).await
    }
    async fn request_then_cancel(&self, body: Value) -> Result<Exchange, RouteFailure> {
        self.exchange(body, true).await
    }
}
#[async_trait]
impl ToolProviderSubject for Subject {
    fn capabilities(&self) -> BTreeSet<Capability> {
        BTreeSet::from([
            Capability::DisableTool,
            Capability::Cancellation,
            Capability::CallKey,
            Capability::SchemaPin,
        ])
    }
    fn plain_stamp(&self) -> RouteStamp {
        RouteStamp {
            principal: "direct".into(),
            scope: None,
        }
    }
    fn scoped_principals(&self) -> Option<ScopedPrincipals> {
        None
    }
    fn catalog_arguments(&self) -> Value {
        json!({})
    }
    fn quick_call(&self) -> CallSpec {
        ("status".into(), json!({}))
    }
    fn slow_call(&self) -> Option<CallSpec> {
        Some((
            "bash".into(),
            json!({"command": "sleep 5", "wait": true, "timeout": 5000}),
        ))
    }
    fn disabled_tool(&self) -> Option<String> {
        Some("outline".into())
    }
    fn held_call(&self, _marker: &Path) -> Option<CallSpec> {
        None
    }
    async fn await_held(&self, _key: &str) -> Result<(), HarnessError> {
        Err(HarnessError::new("not declared in Slice A"))
    }
    async fn approve(&self, _key: &str) -> Result<(), HarnessError> {
        Err(HarnessError::new("not declared in Slice A"))
    }
    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn slice_a_real_module_conformance_inventory() {
    use cortexkit_role_tool_provider_conformance::CaseOutcome;
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/tool_provider_conformance.json")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let report =
        cortexkit_role_tool_provider_conformance::run_suite(&Subject, &dir.path().join("run"))
            .await
            .unwrap();
    let mut enabled = BTreeSet::new();
    let mut skipped = BTreeSet::new();
    let mut failed = BTreeSet::new();
    for case in &report.cases {
        match &case.outcome {
            CaseOutcome::Skipped { .. } => {
                skipped.insert(case.case.to_string());
            }
            CaseOutcome::Passed => {
                enabled.insert(case.case.to_string());
            }
            CaseOutcome::Failed { reason } => {
                enabled.insert(case.case.to_string());
                failed.insert(case.case.to_string());
                eprintln!(
                    "FAIL {}: {}",
                    case.case,
                    reason.chars().take(200).collect::<String>()
                );
            }
        }
    }
    let expected = |field| {
        serde_json::from_value::<BTreeSet<String>>(fixtures["slice_a"][field].clone()).unwrap()
    };
    assert_eq!(enabled, expected("enabled_cases"));
    assert_eq!(skipped, expected("skipped_cases"));
    // SUBC 0.27 RouteBind has no role_versions field. The real module therefore
    // selects legacy routes here, not negotiated tool-provider v1 routes.
    // Keep their failures explicit rather than claiming a successful v1 suite.
    assert_eq!(failed, expected("pending_0_28_cases"));
    assert!(matches!(
        report.verdict,
        cortexkit_role_tool_provider_conformance::SuiteVerdict::Failed { .. }
    ));
    eprintln!("0.27 legacy transport: 12 passed, 3 pending admission, 14 capability skips; NOT a v1 release pass");
}

#[test]
fn registry_manifest_lock_fixture_equalities_and_independent_declaration() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/tool_provider_conformance.json")).unwrap();
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let lock: toml::Value = toml::from_str(include_str!("../../../Cargo.lock")).unwrap();
    for artifact in fixtures["registry_artifacts"].as_array().unwrap() {
        let name = artifact["name"].as_str().unwrap();
        let section = if artifact["dependency_kind"] == "normal" {
            "dependencies"
        } else {
            "dev-dependencies"
        };
        assert_eq!(
            manifest[section][name].as_str().unwrap(),
            format!("={}", artifact["version"].as_str().unwrap())
        );
        let package = lock["package"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"].as_str() == Some(name))
            .unwrap();
        for field in ["version", "source", "checksum"] {
            assert_eq!(
                package[field].as_str().unwrap(),
                artifact[field].as_str().unwrap()
            );
        }
        assert!(!artifact.as_object().unwrap().contains_key("sha"));
    }
    assert_eq!(
        fixtures["slice_a"]["declaration"]["majors"][0]["ops"],
        json!(["role.describe", "tool.catalog", "tool.call"])
    );
}

async fn catalog_reply(route: &Route, request: Value) -> Value {
    let exchange = route
        .request(json!({"name":"tool.catalog","arguments":request}))
        .await
        .unwrap();
    match cortexkit_role_tool_provider_conformance::single_terminal(&exchange).unwrap() {
        ObservedFrame::Response(reply) => reply.clone(),
        other => panic!("catalog refused: {other:?}"),
    }
}

#[tokio::test]
async fn real_module_project_harness_matrix_rebind_and_restart_identity() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let process = Subject.spawn(&state).await.unwrap();
    std::fs::create_dir_all(state.join("config/cortexkit")).unwrap();
    std::fs::write(state.join("config/cortexkit/aft.jsonc"), serde_json::to_vec(&json!({"disabled_tools": [], "harnesses": {"runner": {"disabled_tools": ["aft_outline"]}, "opencode": {"disabled_tools": ["aft_search"]}}})).unwrap()).unwrap();
    let p1 = state.join("project");
    let p2 = state.join("project2");
    std::fs::create_dir_all(p2.join(".cortexkit")).unwrap();
    let project_config = |disabled: Vec<&str>| {
        serde_json::to_vec(&json!({"disabled_tools": disabled, "callgraph_store":false, "search_index":false, "semantic_search":false})).unwrap()
    };
    std::fs::write(p1.join(".cortexkit/aft.jsonc"), project_config(vec![])).unwrap();
    std::fs::write(
        p2.join(".cortexkit/aft.jsonc"),
        project_config(vec!["aft_inspect"]),
    )
    .unwrap();
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/tool_provider_catalog.json")).unwrap();
    let request = json!({"system_text":{"preset":"broca","params":{"worker":false}}});
    let mut routes = vec![];
    for (root, harness, fixture) in [
        (&p1, "runner", "p1_runner"),
        (&p1, "opencode", "p1_opencode"),
        (&p2, "runner", "p2_runner"),
        (&p2, "opencode", "p2_opencode"),
    ] {
        let route = bind_route(&process, root, harness, fixture).await.unwrap();
        let reply = catalog_reply(&route, request.clone()).await;
        let available = reply["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "powershell");
        let name = format!("{fixture}{}", if available { "_pwsh" } else { "_no_pwsh" });
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap();
        assert!(
            serde_json::to_vec(&reply).unwrap() == serde_json::to_vec(&expected["reply"]).unwrap(),
            "{name} full bytes differ"
        );
        routes.push((route, reply));
    }
    std::fs::write(
        p1.join(".cortexkit/aft.jsonc"),
        project_config(vec!["aft_zoom"]),
    )
    .unwrap();
    for (route, reply) in &routes {
        assert_eq!(&catalog_reply(route, request.clone()).await, reply);
    }
    for (harness, fixture) in [
        ("runner", "p1_runner_rebound"),
        ("opencode", "p1_opencode_rebound"),
    ] {
        let route = bind_route(&process, &p1, harness, fixture).await.unwrap();
        let reply = catalog_reply(&route, request.clone()).await;
        let available = reply["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "powershell");
        let name = format!("{fixture}{}", if available { "_pwsh" } else { "_no_pwsh" });
        let expected = fixtures
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap();
        assert_eq!(reply, expected["reply"]);
    }
    for (route, reply) in &routes {
        assert_eq!(&catalog_reply(route, request.clone()).await, reply);
    }
    std::fs::write(p1.join(".cortexkit/aft.jsonc"), project_config(vec![])).unwrap();
    let restored = bind_route(&process, &p1, "runner", "restored")
        .await
        .unwrap();
    assert_eq!(catalog_reply(&restored, request.clone()).await, routes[0].1);
    drop(routes);
    drop(restored);
    drop(process);
    let restarted = Subject.restart(&state).await.unwrap();
    std::fs::write(p1.join(".cortexkit/aft.jsonc"), project_config(vec![])).unwrap();
    let route = bind_route(&restarted, &p1, "runner", "restart")
        .await
        .unwrap();
    let reply = catalog_reply(&route, request).await;
    let available = reply["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "powershell");
    let expected = fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == format!("p1_runner{}", if available { "_pwsh" } else { "_no_pwsh" }))
        .unwrap();
    assert_eq!(reply, expected["reply"]);
    assert!(bind_route(&restarted, &p1, "broca", "unsupported-harness")
        .await
        .is_err());
}
