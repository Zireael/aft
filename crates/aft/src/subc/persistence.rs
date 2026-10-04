//! Persistence readiness waits must not occupy the frame loop or an executor worker.

use super::*;

const WAIT_BUDGET: Duration = Duration::from_secs(10);
const MAX_WAITERS: usize = 256;

#[derive(Default)]
pub(super) struct DatabaseWaits {
    tasks: tokio::task::JoinSet<DecodedFrame>,
    pending: HashMap<(RouteChannel, u64), (tokio::task::AbortHandle, RequestFrameMeta)>,
}

impl DatabaseWaits {
    pub(super) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Return the frame unchanged unless this is a persistence-dependent call
    /// on a bound root whose database is opening. Failure and overflow are
    /// answered by the ordinary tool refusal path without waiting.
    pub(super) fn defer(
        &mut self,
        decoded: DecodedFrame,
        routes: &HashMap<RouteChannel, RouteIdentity>,
        executor: &Executor,
    ) -> Result<(), DecodedFrame> {
        let route = route_key(decoded.frame.header.channel, decoded.frame.header.epoch);
        let Some(identity) = routes.get(&route) else {
            return Err(decoded);
        };
        let Some(ctx) = executor.actor_context(&identity.root) else {
            return Err(decoded);
        };
        let Ok(body) = serde_json::from_slice::<Value>(&decoded.frame.body) else {
            return Err(decoded);
        };
        if body
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(tool_provider::recognized_operation)
            || body
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(tool_provider::recognized_operation)
        {
            return Err(decoded);
        }
        if identity.role == tool_provider::RouteRole::ToolProviderV1 {
            let Ok(call) = serde_json::from_value::<
                cortexkit_role_tool_provider::call::ToolCallRequest,
            >(body.clone()) else {
                return Err(decoded);
            };
            if tool_provider::admit(
                &call,
                identity.scope.is_some(),
                &identity.disabled_tools,
                crate::bash_background::powershell_available(),
                &identity.session,
                !matches!(identity.trust, BindTrust::Untrusted),
            )
            .is_err()
            {
                return Err(decoded);
            }
        }
        let Some(name) = body
            .get("name")
            .or_else(|| body.get("op"))
            .and_then(Value::as_str)
        else {
            return Err(decoded);
        };
        if crate::tool_gate::refusal(
            "database-admission",
            name,
            body.get("arguments").unwrap_or(&Value::Null),
            &identity.disabled_tools,
        )
        .is_some()
        {
            return Err(decoded);
        }
        if ctx.claim_database_runtime_retry(name) {
            let generation = ctx.configure_generation();
            let response = executor.submit_maintenance_async(
                identity.root.clone(),
                crate::executor::Lane::MaintenanceCommit,
                format!("database-retry-{generation}"),
                Box::new(move |ctx| {
                    // Serialize pool installation with config-changing binds, just
                    // like the initial deferred open. A newer bind owns its retry.
                    if ctx.configure_generation() == generation {
                        ctx.retry_database_runtime();
                    }
                    crate::protocol::Response::success("database-retry", serde_json::json!({}))
                }),
            );
            let retry_ctx = ctx.clone();
            tokio::spawn(async move {
                if !response.await.is_ok_and(|response| response.success)
                    && retry_ctx.configure_generation() == generation
                {
                    retry_ctx.finish_database_runtime_error(
                        "Database retry admission failed".into(),
                        true,
                    );
                }
            });
        }
        if !ctx.database_runtime_pending(name) {
            return Err(decoded);
        }
        // A root reported as initializing always has its open staged or
        // running. Hand a staged open to the database-open thread now rather
        // than trusting that the bind's dispatch or the configure tail will
        // get to it soon: the tail can be queued behind other roots for
        // minutes, and a missed dispatch must not leave this root refusing
        // mutations indefinitely. A no-op when the open is already running.
        crate::database_open::dispatch(&ctx);
        if self.pending.len() >= MAX_WAITERS {
            return Err(decoded);
        }
        let name = name.to_string();
        let key = (route, decoded.frame.header.corr);
        let meta = RequestFrameMeta {
            ver: decoded.frame.header.ver,
            flags: decoded.frame.header.flags,
        };
        // A caller may shorten the wait with an absolute Unix-millisecond
        // deadline. The bash timeout limits command execution, not the wait
        // for permission to admit the command.
        let mut deadline = decoded.phase_trace.received_at() + WAIT_BUDGET;
        if let Some(epoch_ms) = body.get("deadline_ms").and_then(Value::as_u64) {
            let remaining = Duration::from_millis(
                epoch_ms.saturating_sub(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                ),
            );
            deadline = deadline.min(Instant::now() + remaining.min(WAIT_BUDGET));
        }
        let handle = self.tasks.spawn(async move {
            ctx.wait_for_database_runtime(&name, deadline).await;
            decoded
        });
        if let Some((old, _)) = self.pending.insert(key, (handle, meta)) {
            old.abort();
        }
        Ok(())
    }

    pub(super) async fn next(&mut self) -> DecodedFrame {
        loop {
            match self.tasks.join_next().await {
                Some(Ok(decoded)) => {
                    self.pending.remove(&(
                        route_key(decoded.frame.header.channel, decoded.frame.header.epoch),
                        decoded.frame.header.corr,
                    ));
                    return decoded;
                }
                Some(Err(_)) => continue,
                None => std::future::pending::<()>().await,
            }
        }
    }

    pub(super) fn cancel(&mut self, route: RouteChannel, corr: u64) -> Option<RequestFrameMeta> {
        let (handle, meta) = self.pending.remove(&(route, corr))?;
        handle.abort();
        Some(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_wait_notification_releases_only_persistence_calls_and_cancel_aborts_wait() {
        let (_dir, root) = test_support::test_root("database-waits");
        let ctx = test_support::test_ctx();
        // Initializing with no staged open, so nothing finishes the open
        // behind this test's back and only `finish_database_runtime` below
        // releases the waits.
        ctx.mark_database_runtime_initializing_for_test();
        let executor = Executor::new();
        assert!(executor.register_actor(root.clone(), Arc::clone(&ctx)));
        let route = route_key(42, 1);
        let routes = HashMap::from([(route, test_support::route_identity(&root, "test"))]);
        let frame = |name: &str, corr| DecodedFrame {
            frame: Frame::build(
                FrameType::Request,
                control_flags(),
                42,
                1,
                corr,
                serde_json::to_vec(&json!({"name": name, "arguments": {}})).unwrap(),
            )
            .unwrap(),
            phase_trace: PhaseTrace::new(Instant::now()),
        };
        let mut waits = DatabaseWaits::default();
        assert!(waits.defer(frame("bash", 1), &routes, &executor).is_ok());
        for name in [
            "read",
            "grep",
            "glob",
            "outline",
            "zoom",
            "aft_search",
            "aft_inspect",
        ] {
            assert!(
                waits.defer(frame(name, 2), &routes, &executor).is_err(),
                "{name} waited for DB"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), waits.next())
                .await
                .is_err()
        );
        assert!(waits.cancel(route, 1).is_some());
        assert!(waits.pending.is_empty());
        assert!(waits.defer(frame("bash", 3), &routes, &executor).is_ok());
        ctx.finish_database_runtime(Ok(()));
        let resumed = tokio::time::timeout(Duration::from_millis(500), waits.next())
            .await
            .unwrap();
        assert_eq!(
            resumed.frame.header.corr, 3,
            "cancelled call must never resume"
        );
        assert!(waits.pending.is_empty());
    }
}
