use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::remote::{RerankBackend, RerankDoc, RerankError, RerankFingerprint};
use crate::config::SemanticBackendConfig;
use crate::synapse_embed::{SynapseEmbeddingError, SynapseRerankTransport};

pub(crate) struct SynapseReranker {
    model: String,
    required_fingerprint: Mutex<Option<String>>,
    max_queue_ms: u64,
    transport: Mutex<SynapseRerankTransport>,
}

impl SynapseReranker {
    /// The embedding configuration supplies only the daemon connection and route identity.
    /// The rerank model and its pin are independent of the embedding model.
    pub(crate) fn new(
        config: &SemanticBackendConfig,
        model: String,
        required_fingerprint: Option<String>,
        max_queue_ms: u64,
        under_subc: bool,
    ) -> Result<Self, RerankError> {
        if !under_subc {
            return Err(RerankError::Unavailable(
                "Synapse reranking is daemon-only; run AFT under subc with Synapse registered"
                    .into(),
            ));
        }
        if model.trim().is_empty()
            || required_fingerprint
                .as_ref()
                .is_some_and(|pin| pin.trim().is_empty())
        {
            return Err(RerankError::Refused(
                "Synapse reranking requires a model and pinned required_fingerprint".into(),
            ));
        }
        Ok(Self {
            model,
            required_fingerprint: Mutex::new(required_fingerprint),
            max_queue_ms,
            transport: Mutex::new(SynapseRerankTransport::new(config).map_err(transport_error)?),
        })
    }
}

impl SynapseReranker {
    fn pin(&self, deadline: Instant) -> Result<String, RerankError> {
        if let Some(pin) = self
            .required_fingerprint
            .lock()
            .map_err(|_| RerankError::Failed("Synapse pin lock poisoned".into()))?
            .clone()
        {
            return Ok(pin);
        }
        let mut transport = self
            .transport
            .try_lock()
            .map_err(|_| RerankError::Unavailable("Synapse rerank transport is busy".into()))?;
        self.pin_with(&mut transport, deadline)
    }
    fn pin_with(
        &self,
        transport: &mut SynapseRerankTransport,
        deadline: Instant,
    ) -> Result<String, RerankError> {
        let mut pin = self
            .required_fingerprint
            .lock()
            .map_err(|_| RerankError::Failed("Synapse pin lock poisoned".into()))?;
        if let Some(pin) = pin.as_ref() {
            return Ok(pin.clone());
        }
        let response = transport
            .call("models.list", json!({}), deadline)
            .map_err(transport_error)?;
        let discovered = discover_pin(&response, &self.model)?;
        *pin = Some(discovered.clone());
        Ok(discovered)
    }
}

fn discover_pin(response: &Value, model: &str) -> Result<String, RerankError> {
    let envelope = response.get("result").unwrap_or(response);
    let entries = envelope
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| RerankError::Failed("Synapse models.list omitted models".into()))?;
    let entry = entries
        .iter()
        .find(|entry| {
            entry
                .get("model_id")
                .or_else(|| entry.get("model"))
                .and_then(Value::as_str)
                == Some(model)
        })
        .ok_or_else(|| {
            RerankError::Unavailable(format!("Synapse does not serve reranker {model}"))
        })?;
    entry
        .get("fingerprint")
        .and_then(Value::as_str)
        .or_else(|| {
            entry
                .get("fingerprints")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
        })
        .filter(|pin| !pin.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RerankError::Failed("Synapse reranker has no fingerprint".into()))
}

impl RerankBackend for SynapseReranker {
    fn fingerprint(&self) -> RerankFingerprint {
        RerankFingerprint {
            backend: "synapse",
            model: self.model.clone(),
            revision: self
                .pin(Instant::now() + Duration::from_millis(1500))
                .unwrap_or_else(|_| "unavailable".into()),
        }
    }
    fn max_batch(&self) -> usize {
        20
    }
    fn score(
        &self,
        query: &str,
        docs: &[RerankDoc<'_>],
        deadline: Instant,
    ) -> Result<Vec<f32>, RerankError> {
        if docs.len() > self.max_batch() {
            return Err(RerankError::Refused(
                "Synapse interactive reranking accepts at most 20 candidates".into(),
            ));
        }
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        // Never wait behind another request beyond this call's deadline.
        let mut transport = self
            .transport
            .try_lock()
            .map_err(|_| RerankError::Unavailable("Synapse rerank transport is busy".into()))?;
        let pin = self.pin_with(&mut transport, deadline)?;
        let result = score_via(
            &self.model,
            &pin,
            self.max_queue_ms,
            query,
            docs,
            deadline,
            |params| {
                transport
                    .call("rerank.score", params, deadline)
                    .map_err(transport_error)
            },
        );
        if matches!(&result, Err(RerankError::Refused(message)) if message.contains("substitution_rejected"))
        {
            if let Ok(response) = transport.call("models.list", json!({}), deadline) {
                if let Ok(current) = discover_pin(&response, &self.model) {
                    return Err(RerankError::Refused(format!("Synapse rerank fingerprint changed: pinned {pin}, served {current}; rebuild the backend to re-pin")));
                }
            }
        }
        result
    }
}

fn transport_error(error: SynapseEmbeddingError) -> RerankError {
    match error {
        SynapseEmbeddingError::Timeout(_) => RerankError::Timeout,
        SynapseEmbeddingError::InvalidEnvelope(message) => RerankError::Failed(message),
        other => RerankError::Unavailable(other.to_string()),
    }
}

fn score_via(
    model: &str,
    pin: &str,
    max_queue_ms: u64,
    query: &str,
    docs: &[RerankDoc<'_>],
    deadline: Instant,
    call: impl FnOnce(Value) -> Result<Value, RerankError>,
) -> Result<Vec<f32>, RerankError> {
    if docs.len() > 20 {
        return Err(RerankError::Refused(
            "Synapse interactive reranking accepts at most 20 candidates".into(),
        ));
    }
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(RerankError::Timeout)?;
    let deadline_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
    if deadline_ms == 0 {
        return Err(RerankError::Timeout);
    }
    let response = call(
        json!({"model":model, "query":query, "candidates":docs.iter().map(|doc| doc.text).collect::<Vec<_>>(), "required_fingerprint":pin, "allow_equivalent":false, "accept_declared":false, "max_queue_ms":max_queue_ms.min(deadline_ms), "deadline_ms":deadline_ms}),
    )?;
    parse_response(&response, pin, docs.len())
}

fn parse_response(response: &Value, pin: &str, count: usize) -> Result<Vec<f32>, RerankError> {
    let envelope = response.get("result").unwrap_or(response);
    if let Some(error) = envelope.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(match code {
            "deadline_exceeded" | "timeout" => RerankError::Timeout,
            "model_not_found" | "model_unavailable" | "not_certified" => {
                RerankError::Unavailable(format!("Synapse rerank {code}"))
            }
            _ => RerankError::Refused(format!("Synapse rerank {code}")),
        });
    }
    if envelope.get("fingerprint").and_then(Value::as_str) != Some(pin) {
        return Err(RerankError::Refused(format!(
            "Synapse rerank fingerprint changed: pinned {pin}, served {}",
            envelope
                .get("fingerprint")
                .and_then(Value::as_str)
                .unwrap_or("missing")
        )));
    }
    let scores = envelope
        .get("scores")
        .and_then(Value::as_array)
        .filter(|scores| scores.len() == count)
        .ok_or_else(|| {
            RerankError::Failed("Synapse rerank response omitted candidate scores".into())
        })?;
    // Synapse serves raw cross-encoder logits, not normalized relevance probabilities.
    scores
        .iter()
        .map(|score| {
            score
                .as_f64()
                .map(|score| score as f32)
                .filter(|score| score.is_finite())
                .ok_or_else(|| {
                    RerankError::Failed("Synapse rerank returned a non-finite score".into())
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn config(connection_file: std::path::PathBuf) -> SemanticBackendConfig {
        SemanticBackendConfig {
            backend: crate::config::SemanticBackend::Synapse,
            model: "embedding".into(),
            base_url: None,
            api_key_env: None,
            timeout_ms: 1500,
            query_timeout_ms: 1500,
            query_instruction: "off".into(),
            max_batch_size: 20,
            max_input_tokens: None,
            max_files: 100,
            subc_connection_file: Some(connection_file),
            route_project_root: None,
            route_harness: None,
        }
    }

    fn fake_daemon(
        responses: Vec<Option<Value>>,
        registered: bool,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<Vec<Value>>,
    ) {
        use subc_transport::connection_file::{self, ConnectionInfo, Endpoint, SCHEMA_VERSION};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connection.json");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let key = vec![0x42; subc_transport::KEY_LEN];
        let daemon_id = [0x24; subc_transport::DAEMON_ID_LEN];
        connection_file::write_atomic(
            &path,
            &ConnectionInfo {
                schema: SCHEMA_VERSION,
                wire_version: Some(subc_protocol::PROTOCOL_VERSION),
                endpoints: vec![Endpoint {
                    host: "127.0.0.1".into(),
                    port: listener.local_addr().unwrap().port(),
                }],
                key: key.clone(),
                daemon_id,
                pid: std::process::id(),
                daemon_ver: "synapse-test".into(),
            },
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                tokio::time::timeout(Duration::from_secs(5), async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let (mut stream, _) = listener.accept().await.unwrap();
                    subc_transport::authenticate_server(&mut stream, &key, &daemon_id, "synapse-test", Duration::from_secs(2)).await.unwrap();
                    let catalog = subc_transport::read_frame(&mut stream).await.unwrap().unwrap();
                    let modules = if registered { json!([{"module_id":"synapse","module_version":"1","roles":[{"role":"management_surface","operations":[{"name":"models.list","kind":"query"},{"name":"rerank.score","kind":"query"}],"config_schema":{"type":"object"},"observability":[],"identity_scope":[]}],"control_ops":[]}]) } else { json!([]) };
                    respond(&mut stream, &catalog, json!({"op":"catalog.list","generation":1,"modules":modules,"subc_ops":["catalog.list","route.open"]})).await;
                    if !registered { return Vec::new(); }
                    let route = subc_transport::read_frame(&mut stream).await.unwrap().unwrap();
                    let route_body: Value = serde_json::from_slice(&route.body).unwrap();
                    assert_eq!(route_body["target"], json!({"kind":"management_surface","module_id":"synapse"}));
                    respond(&mut stream, &route, json!({"op":"route.open","route_channel":7,"route_epoch":1})).await;
                    let mut requests = Vec::new();
                    for response in responses {
                        let request = subc_transport::read_frame(&mut stream).await.unwrap().unwrap();
                        requests.push(serde_json::from_slice(&request.body).unwrap());
                        if let Some(response) = response { respond(&mut stream, &request, response).await; }
                        else {
                            // Withhold the reply until the client has timed out, then detect any retry.
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            if let Ok(frame) = tokio::time::timeout(Duration::from_millis(50), subc_transport::read_frame(&mut stream)).await {
                                assert!(frame.unwrap().is_none(), "unexpected retry on route");
                            }
                            assert!(tokio::time::timeout(Duration::from_millis(50), listener.accept()).await.is_err());
                        }
                    }
                    requests
                }).await.expect("fake Synapse watchdog")
            })
        });
        (directory, path, server)
    }

    async fn respond(
        stream: &mut tokio::net::TcpStream,
        request: &subc_protocol::Frame,
        value: Value,
    ) {
        use subc_protocol::{Flags, Frame, FrameType, Priority};
        let frame = Frame::build_with_version(
            request.header.ver,
            FrameType::Response,
            Flags::new(false, Priority::Interactive, false),
            request.header.channel,
            request.header.epoch,
            request.header.corr,
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        subc_transport::write_frame(stream, &frame).await.unwrap();
    }

    #[test]
    fn synapse_transport_discovers_once_and_keeps_pin_after_model_change() {
        let models =
            |pin| json!({"result":{"models":[{"model_id":"reranker","fingerprints":[pin]}]}});
        let (_directory, path, server) = fake_daemon(
            vec![
                Some(models("fp-old")),
                Some(json!({"result":{"fingerprint":"fp-old","scores":[2.0]}})),
                Some(json!({"result":{"error":{"code":"substitution_rejected"}}})),
                Some(models("fp-new")),
            ],
            true,
        );
        let backend =
            SynapseReranker::new(&config(path), "reranker".into(), None, 100, true).unwrap();
        assert_eq!(backend.fingerprint().revision, "fp-old");
        assert_eq!(
            backend
                .score(
                    "q",
                    &[RerankDoc { text: "a" }],
                    Instant::now() + Duration::from_secs(1)
                )
                .unwrap(),
            vec![2.0]
        );
        let error = backend
            .score(
                "q",
                &[RerankDoc { text: "a" }],
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(
            matches!(error, RerankError::Refused(message) if message.contains("fp-old") && message.contains("fp-new"))
        );
        assert_eq!(backend.fingerprint().revision, "fp-old");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0]["method"], "models.list");
        assert_eq!(requests[1]["params"]["required_fingerprint"], "fp-old");
        assert_eq!(requests[2]["params"]["required_fingerprint"], "fp-old");
    }

    #[test]
    fn synapse_backend_refuses_bulk_before_discovery() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let backend = SynapseReranker::new(
            &config(file.path().into()),
            "reranker".into(),
            None,
            100,
            true,
        )
        .unwrap();
        let docs: Vec<_> = (0..21).map(|_| RerankDoc { text: "a" }).collect();
        assert!(matches!(
            backend.score("q", &docs, Instant::now() + Duration::from_secs(1)),
            Err(RerankError::Refused(_))
        ));
        assert_eq!(
            backend.score("q", &[], Instant::now()).unwrap(),
            Vec::<f32>::new()
        );
    }

    #[test]
    fn synapse_transport_times_out_without_retry() {
        let (_directory, path, server) = fake_daemon(vec![None], true);
        let backend = SynapseReranker::new(
            &config(path),
            "reranker".into(),
            Some("fp".into()),
            100,
            true,
        )
        .unwrap();
        assert!(matches!(
            backend.score(
                "q",
                &[RerankDoc { text: "a" }],
                Instant::now() + Duration::from_millis(200)
            ),
            Err(RerankError::Timeout)
        ));
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn synapse_daemon_only_and_registration_status_are_clear() {
        assert!(
            matches!(SynapseReranker::new(&config("/missing".into()), "r".into(), None, 100, false), Err(RerankError::Unavailable(message)) if message.contains("daemon-only"))
        );
        let (_directory, path, server) = fake_daemon(Vec::new(), false);
        let backend = SynapseReranker::new(
            &config(path),
            "reranker".into(),
            Some("fp".into()),
            100,
            true,
        )
        .unwrap();
        assert!(
            matches!(backend.score("q", &[RerankDoc{text:"a"}], Instant::now()+Duration::from_secs(1)), Err(RerankError::Unavailable(message)) if message.contains("not registered"))
        );
        assert!(server.join().unwrap().is_empty());
    }

    #[test]
    fn synapse_fake_route_pins_identity_and_bounds_admission() {
        let scores = score_via(
            "reranker",
            "fp-a",
            5000,
            "q",
            &[RerankDoc { text: "a" }, RerankDoc { text: "b" }],
            Instant::now() + Duration::from_secs(1),
            |params| {
                assert_eq!(params["model"], "reranker");
                assert_eq!(params["query"], "q");
                assert_eq!(params["candidates"], json!(["a", "b"]));
                assert_eq!(params["required_fingerprint"], "fp-a");
                assert_eq!(params["allow_equivalent"], false);
                assert!(params["deadline_ms"].as_u64().unwrap() <= 1000);
                assert_eq!(params["max_queue_ms"], params["deadline_ms"]);
                Ok(json!({"result":{"fingerprint":"fp-a","scores":[-2.0,3.0]}}))
            },
        )
        .unwrap();
        assert_eq!(scores, vec![-2.0, 3.0]);
    }
    #[test]
    fn synapse_rejects_bulk_and_expired_requests_without_routing() {
        let docs: Vec<_> = (0..21).map(|_| RerankDoc { text: "a" }).collect();
        assert!(matches!(
            score_via(
                "m",
                "fp",
                100,
                "q",
                &docs,
                Instant::now() + Duration::from_secs(1),
                |_| panic!("must not route bulk")
            ),
            Err(RerankError::Refused(_))
        ));
        assert!(matches!(
            score_via("m", "fp", 100, "q", &docs[..1], Instant::now(), |_| panic!(
                "must not route expired call"
            )),
            Err(RerankError::Timeout)
        ));
    }
    #[test]
    fn synapse_rejects_changed_fingerprint_and_malformed_scores() {
        assert!(matches!(
            parse_response(&json!({"fingerprint":"foreign","scores":[1.0]}), "fp", 1),
            Err(RerankError::Refused(_))
        ));
        for scores in [json!([]), json!([1e100]), json!(["NaN"])] {
            assert!(matches!(
                parse_response(&json!({"fingerprint":"fp","scores":scores}), "fp", 1),
                Err(RerankError::Failed(_))
            ));
        }
    }
    #[test]
    fn synapse_error_envelopes_map_to_backend_errors() {
        assert!(matches!(
            parse_response(&json!({"error":{"code":"deadline_exceeded"}}), "fp", 1),
            Err(RerankError::Timeout)
        ));
        assert!(matches!(
            parse_response(&json!({"error":{"code":"model_not_found"}}), "fp", 1),
            Err(RerankError::Unavailable(_))
        ));
        assert!(matches!(
            parse_response(&json!({"error":{"code":"substitution_rejected"}}), "fp", 1),
            Err(RerankError::Refused(_))
        ));
    }
}
