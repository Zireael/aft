//! Daemon-side relay for governed `gh` bot writes.
//!
//! The `gh` shim runs as an agent's child with no subc credentials, so it
//! cannot mint an agent assertion or reach plexus itself: both require the
//! attested `reserved:aft` principal that only this daemon holds. The shim
//! instead sends its per-command ticket (see [`crate::gh_shim_ticket`]) and the
//! unchanged governed request envelope to one of the management operations
//! below. The daemon redeems the ticket to the session that spawned the
//! command, mints an assertion for that session from prefrontal, calls plexus
//! with it, and hands plexus's reply back unchanged.
//!
//! The ticket is the only authority. A session or agent field in the body is
//! never used to pick the speaker, and a missing or unknown ticket is refused
//! before anything leaves the daemon.
//!
//! A bot write goes out on one of two paths:
//!
//! - **stamp**: the session's tool route was bound under a daemon scope that
//!   lets providers act as the session's agent (see [`delegating_agent`]). The
//!   daemon then opens its plexus route under that same scope and sends the
//!   write without an assertion; plexus reads the agent from the scope stamp on
//!   that route and re-checks the scope before each write it sends.
//! - **assertion**: everything else. The daemon mints a signed assertion from
//!   prefrontal and sends it with the write on an unscoped route.
//!
//! The stamp path needs a transport that can open a scoped route
//! ([`RelayTransport::opens_scoped_routes`]). The production transport can:
//! it opens the route with the subc client's scoped open, and never falls back
//! to an unscoped route when that open is refused.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// Relay a governed bot write. Params: `{ticket, request_nonce, request}`.
pub const BOT_REQUEST_OPERATION: &str = "gh_shim.bot_request";
/// Relay plexus's repository-to-agent binding read for the shim's version
/// check. Params: `{ticket, agent_id}`; the daemon derives plexus's handle
/// connection id from the assertion it mints for that agent and session.
pub const BINDINGS_READ_OPERATION: &str = "gh_shim.bindings_read";

pub(crate) const PREFRONTAL_MODULE_ID: &str = "prefrontal-core";
pub(crate) const PLEXUS_MODULE_ID: &str = "plexus";
const MINT_METHOD: &str = "agent.assertion_mint";
/// A cached assertion is re-minted this long before its `exp`, so a token
/// never expires between the cache check and plexus's verification.
const TOKEN_REFRESH_MARGIN_SECS: u64 = 5 * 60;
const RELAY_CALL_TIMEOUT: Duration = Duration::from_secs(20);

pub fn is_relay_operation(operation: &str) -> bool {
    matches!(operation, BOT_REQUEST_OPERATION | BINDINGS_READ_OPERATION)
}

/// Plexus answers a tool call either with its facade reply directly or wrapped
/// in a tool-call result whose first text block holds the reply as JSON.
/// Return the facade reply in both cases.
pub fn facade_reply(value: &Value) -> Value {
    let wrapped = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find_map(|block| block.get("text").and_then(Value::as_str))
        })
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    wrapped.unwrap_or_else(|| value.clone())
}

/// The refusal code in a plexus facade reply, if the reply is a refusal.
pub fn facade_refusal_code(reply: &Value) -> Option<&str> {
    reply
        .get("result")
        .and_then(|result| result.get("refusal_code"))
        .and_then(Value::as_str)
}

/// How a call to prefrontal or plexus failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// The target answered with a subc Error frame carrying this code.
    Refused { code: String, message: String },
    /// Nothing was sent: no connection, no route, or the module is absent.
    Unavailable(String),
    /// The request was sent but no reply arrived; it may have executed.
    OutcomeUnknown(String),
}

/// The daemon scope stamped on a route at bind.
pub type ScopeStamp = subc_protocol::scope::ScopeStamp;
/// The scope a route open asks to be admitted under.
pub type ScopeSelector = subc_protocol::scope::ScopeSelector;

/// The agent a stamp lets a provider act as, if any.
///
/// This is exactly the rule plexus applies to a bot write that carries no
/// assertion: the scope must delegate, its owner must be one the daemon trusts
/// to set agent identity (`owner_authorized`), and it must name an agent. Any
/// other stamp grants plexus no one to act as, so a write without an assertion
/// would be refused (`assertion_absent`).
pub fn delegating_agent(stamp: &ScopeStamp) -> Option<&str> {
    if stamp.attributes.delegates && stamp.owner_authorized {
        stamp.attributes.agent_id.as_deref()
    } else {
        None
    }
}

/// The selector that opens a route under the scope a stamp names, pinned to
/// the stamp's epoch so a newer session under the same ref is not picked up.
fn scope_selector(stamp: &ScopeStamp) -> ScopeSelector {
    ScopeSelector {
        owner: stamp.owner.clone(),
        scope_ref: stamp.scope_ref.clone(),
        scope_epoch: Some(stamp.scope_epoch),
    }
}

/// The scope stamps of AFT's bound tool routes, keyed by the session each
/// route was bound for. The subc frame loop copies them when a relay request
/// arrives, because the relay runs on its own task and cannot read the loop's
/// route table.
#[derive(Clone, Debug, Default)]
pub struct RouteScopes {
    routes: Vec<(String, Option<ScopeStamp>)>,
}

impl RouteScopes {
    /// Record one bound tool route and the stamp the daemon put on it.
    pub fn push(&mut self, session: &str, stamp: Option<ScopeStamp>) {
        self.routes.push((session.to_string(), stamp));
    }

    /// The stamp of the session's tool route, when there is exactly one answer.
    ///
    /// A ticket names a session, not a route, and a session can have more than
    /// one route bound at a time. When those routes carry different stamps, or
    /// one of them carries none, the relay cannot tell which route launched the
    /// command, so this returns `None` and the write uses the assertion path,
    /// which is what an unstamped session gets.
    pub fn for_session(&self, session: &str) -> Option<&ScopeStamp> {
        let mut stamps = self
            .routes
            .iter()
            .filter(|(bound, _)| bound == session)
            .map(|(_, stamp)| stamp.as_ref());
        let first = stamps.next()??;
        stamps.all(|other| other == Some(first)).then_some(first)
    }
}

/// The two outbound calls the relay makes. Production uses the daemon's own
/// `reserved:aft` subc connection; tests use fakes that record the bodies.
pub trait RelayTransport: Send + Sync {
    /// Send `body` to prefrontal-core and return the parsed JSON reply.
    fn prefrontal(
        &self,
        project_root: &str,
        session: &str,
        body: Value,
    ) -> impl std::future::Future<Output = Result<Value, TransportError>> + Send;
    /// Send `body` to plexus's tool provider and return the parsed JSON reply.
    /// With `scope`, the route to plexus must be opened under that scope, so
    /// plexus sees its stamp; without one the route is unscoped.
    fn plexus(
        &self,
        project_root: &str,
        session: &str,
        scope: Option<&ScopeSelector>,
        body: Value,
    ) -> impl std::future::Future<Output = Result<Value, TransportError>> + Send;
    /// Whether [`RelayTransport::plexus`] can open its route under a scope.
    /// The stamp path is only taken when this is true: a write without an
    /// assertion on an unscoped route would be refused by plexus.
    fn opens_scoped_routes(&self) -> bool;
}

#[derive(Clone, Debug)]
struct CachedToken {
    token: Value,
    exp: u64,
}

/// Minted assertions kept in daemon memory, keyed by agent and session. The
/// map is only touched briefly from the relay task, never from the subc frame
/// loop.
#[derive(Default)]
pub struct TokenCache {
    tokens: Mutex<HashMap<(String, String), CachedToken>>,
}

impl TokenCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), CachedToken>> {
        self.tokens
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn fresh(&self, agent: &str, session: &str, now: u64) -> Option<Value> {
        self.lock()
            .get(&(agent.to_string(), session.to_string()))
            .filter(|cached| now.saturating_add(TOKEN_REFRESH_MARGIN_SECS) < cached.exp)
            .map(|cached| cached.token.clone())
    }

    fn store(&self, agent: &str, session: &str, token: Value, exp: u64) {
        self.lock().insert(
            (agent.to_string(), session.to_string()),
            CachedToken { token, exp },
        );
    }

    fn drop_token(&self, agent: &str, session: &str) {
        self.lock()
            .remove(&(agent.to_string(), session.to_string()));
    }
}

/// What the daemon sends back to the shim. `ok` replies carry plexus's reply
/// unchanged; refusals carry `{refusal_code, stage, message}`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelayReply {
    pub ok: bool,
    pub data: Value,
}

impl RelayReply {
    fn relayed(reply: Value) -> Self {
        Self {
            ok: true,
            data: reply,
        }
    }

    fn refused(code: &str, stage: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: json!({
                "refusal_code": code,
                "stage": stage,
                "message": message.into(),
            }),
        }
    }

    fn outcome_label(&self) -> String {
        if self.ok {
            return match facade_refusal_code(&facade_reply(&self.data)) {
                Some(code) => code.to_string(),
                None => "completed".to_string(),
            };
        }
        self.data
            .get("refusal_code")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_string()
    }
}

/// One relay request as seen by the relay core.
pub struct RelayCall<'a> {
    pub operation: &'a str,
    pub params: &'a Value,
    /// Whether the management route this arrived on belongs to a first-party
    /// principal. Anything else is refused even with a valid ticket.
    pub first_party: bool,
    /// Current Unix time in seconds, for the token cache.
    pub now: u64,
    /// Scope stamps of the daemon's bound tool routes when the request arrived.
    pub route_scopes: &'a RouteScopes,
}

/// Which path a bot write took, for the relay log line.
const PATH_STAMP: &str = "stamp";
const PATH_ASSERTION: &str = "assertion";

/// Run one relay operation. `log` receives exactly one line per call carrying
/// the redeemed session, task id, nonce, action, repository, the path the
/// write took (`stamp` or `assertion`, `-` when refused before one was chosen)
/// and the outcome; the ticket, the assertion and the scope never appear in it.
pub async fn relay<T: RelayTransport>(
    transport: &T,
    cache: &TokenCache,
    call: RelayCall<'_>,
    log: &(dyn Fn(&str) + Send + Sync),
) -> RelayReply {
    let params = call.params;
    let nonce = params
        .get("request_nonce")
        .and_then(Value::as_str)
        .unwrap_or("");
    let request = params.get("request");
    let action = match call.operation {
        BINDINGS_READ_OPERATION => "bindings.read",
        _ => request
            .and_then(|request| request.get("action").or_else(|| request.get("verb")))
            .and_then(Value::as_str)
            .unwrap_or("-"),
    };
    let repository = request
        .and_then(|request| request.get("repository"))
        .and_then(Value::as_str)
        .unwrap_or("-");

    let mut session = "-".to_string();
    let mut task = "-".to_string();
    let mut path = "-";
    let reply = 'reply: {
        if !call.first_party {
            break 'reply RelayReply::refused(
                "untrusted_principal",
                "admission",
                "gh shim relay is only served to first-party principals",
            );
        }
        let Some(ticket) = params
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|ticket| !ticket.is_empty())
        else {
            break 'reply RelayReply::refused(
                "ticket_absent",
                "ticket",
                "no agent session is attached to this command",
            );
        };
        let Some(redeemed) = crate::gh_shim_ticket::redeem(ticket) else {
            break 'reply RelayReply::refused(
                "ticket_unknown",
                "ticket",
                "the command's session ticket is not live; it ended or was never issued",
            );
        };
        session = redeemed.session_id.clone();
        task = if redeemed.task_id.is_empty() {
            "-".to_string()
        } else {
            redeemed.task_id.clone()
        };
        match call.operation {
            BINDINGS_READ_OPERATION => {
                path = PATH_ASSERTION;
                bindings_read(transport, cache, &redeemed, params, call.now).await
            }
            _ => {
                let stamp = call.route_scopes.for_session(&redeemed.session_id);
                let (reply, taken) =
                    bot_request(transport, cache, &redeemed, stamp, params, call.now).await;
                path = taken;
                reply
            }
        }
    };

    log(&format!(
        "gh_shim relay: op={} session={} task={} nonce={} action={} repository={} path={} outcome={}",
        call.operation,
        session,
        task,
        if nonce.is_empty() { "-" } else { nonce },
        action,
        repository,
        path,
        reply.outcome_label(),
    ));
    reply
}

/// Obtain the assertion for this agent and session, minting one from
/// prefrontal if none is cached or the cached one is near expiry.
async fn obtain_token<T: RelayTransport>(
    transport: &T,
    cache: &TokenCache,
    redeemed: &crate::gh_shim_ticket::Redeemed,
    agent_id: &str,
    now: u64,
) -> Result<Value, RelayReply> {
    let session = redeemed.session_id.as_str();
    if let Some(token) = cache.fresh(agent_id, session, now) {
        return Ok(token);
    }
    let body = json!({
        "method": MINT_METHOD,
        "params": {"agent_id": agent_id, "session": session},
    });
    let reply = transport
        .prefrontal(&redeemed.project_root, session, body)
        .await
        .map_err(|error| transport_refusal("mint", error))?;
    let Some(token) = reply
        .get("result")
        .and_then(|result| result.get("token"))
        .filter(|token| token.is_object())
        .cloned()
    else {
        return Err(RelayReply::refused(
            "mint_reply_malformed",
            "mint",
            "prefrontal's assertion mint reply had no result.token",
        ));
    };
    let exp = token
        .get("claims")
        .and_then(|claims| claims.get("exp"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    cache.store(agent_id, session, token.clone(), exp);
    Ok(token)
}

/// Plexus names a GitHub handle connection after the App slug, the bot login
/// without its `[bot]` suffix. The assertion prefrontal mints for the agent
/// carries that login as `claims.handle`, so the connection id comes from it
/// rather than from anything the shim sends.
pub fn handle_connection_id(token: &Value) -> Result<String, String> {
    let handle = token
        .get("claims")
        .and_then(|claims| claims.get("handle"))
        .and_then(Value::as_str)
        .ok_or_else(|| "the minted assertion carries no claims.handle".to_string())?;
    let slug = handle
        .strip_suffix("[bot]")
        .filter(|slug| !slug.is_empty())
        .ok_or_else(|| {
            format!("the minted assertion's handle {handle:?} is not a GitHub App bot login (<slug>[bot])")
        })?;
    Ok(format!("github-handle-{slug}"))
}

async fn bindings_read<T: RelayTransport>(
    transport: &T,
    cache: &TokenCache,
    redeemed: &crate::gh_shim_ticket::Redeemed,
    params: &Value,
    now: u64,
) -> RelayReply {
    let Some(agent_id) = params
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|agent| !agent.is_empty())
    else {
        return RelayReply::refused(
            "request_malformed",
            "request",
            "bindings read needs an agent_id",
        );
    };
    let token = match obtain_token(transport, cache, redeemed, agent_id, now).await {
        Ok(token) => token,
        Err(refusal) => return refusal,
    };
    let connection_id = match handle_connection_id(&token) {
        Ok(connection_id) => connection_id,
        Err(message) => return RelayReply::refused("assertion_handle_malformed", "mint", message),
    };
    let body = json!({
        "name": "github",
        "arguments": {"op": "bindings.read", "connection_id": connection_id},
    });
    match transport
        .plexus(&redeemed.project_root, &redeemed.session_id, None, body)
        .await
    {
        Ok(reply) => RelayReply::relayed(reply),
        Err(error) => transport_refusal("plexus", error),
    }
}

/// Relay one bot write, returning plexus's reply and the path it took.
async fn bot_request<T: RelayTransport>(
    transport: &T,
    cache: &TokenCache,
    redeemed: &crate::gh_shim_ticket::Redeemed,
    stamp: Option<&ScopeStamp>,
    params: &Value,
    now: u64,
) -> (RelayReply, &'static str) {
    let Some(nonce) = params
        .get("request_nonce")
        .and_then(Value::as_str)
        .filter(|nonce| !nonce.is_empty())
    else {
        return (
            RelayReply::refused("request_malformed", "request", "request_nonce is required"),
            "-",
        );
    };
    let Some(request) = params.get("request").filter(|request| request.is_object()) else {
        return (
            RelayReply::refused("request_malformed", "request", "request must be an object"),
            "-",
        );
    };
    // The agent is the one the shim's signed manifest binds to the target
    // repository. It names whose bot to mint for; the session comes from the
    // ticket alone, and plexus re-checks the agent against its own bindings.
    let Some(agent_id) = request
        .get("metadata")
        .and_then(|metadata| metadata.get("agent_id"))
        .and_then(Value::as_str)
        .filter(|agent| !agent.is_empty())
    else {
        return (
            RelayReply::refused(
                "request_malformed",
                "request",
                "request.metadata.agent_id is required",
            ),
            "-",
        );
    };
    let session = redeemed.session_id.as_str();

    // The stamp path: the session's scope lets plexus act as its agent, and
    // the route to plexus can be opened under that scope so plexus sees it.
    if let Some((stamp_agent, stamp)) = stamp
        .filter(|_| transport.opens_scoped_routes())
        .and_then(|stamp| delegating_agent(stamp).map(|agent| (agent, stamp)))
    {
        // Plexus takes the agent from the stamp, so a request naming another
        // agent would speak as the wrong bot. Plexus refuses the same mismatch
        // under this code when a stamp and an assertion disagree.
        if stamp_agent != agent_id {
            return (
                RelayReply::refused(
                    "agent_identity_conflict",
                    "scope",
                    format!(
                        "the session's scope acts as agent {stamp_agent}, but the request names agent {agent_id}"
                    ),
                ),
                PATH_STAMP,
            );
        }
        let selector = scope_selector(stamp);
        let body = json!({
            "name": "github",
            "arguments": {
                "op": "bot_request",
                "request_nonce": nonce,
                "request": request,
            },
        });
        // Every reply, refusals included (`scope_unverifiable`, `scope_ended`,
        // `delegation_withdrawn`, `agent_identity_conflict`, with their
        // `legs_sent`), goes back to the shim as plexus sent it. The write is
        // never retried on the assertion path: plexus refused because of the
        // scope, and an assertion would get the write past that decision.
        let reply = match transport
            .plexus(&redeemed.project_root, session, Some(&selector), body)
            .await
        {
            Ok(reply) => RelayReply::relayed(reply),
            Err(error) => transport_refusal("plexus", error),
        };
        return (reply, PATH_STAMP);
    }

    let token = match obtain_token(transport, cache, redeemed, agent_id, now).await {
        Ok(token) => token,
        Err(refusal) => return (refusal, PATH_ASSERTION),
    };
    let body = json!({
        "name": "github",
        "arguments": {
            "op": "bot_request",
            "assertion": token,
            "request_nonce": nonce,
            "request": request,
        },
    });
    let reply = match transport
        .plexus(&redeemed.project_root, session, None, body)
        .await
    {
        Ok(reply) => {
            // A refusal that names the assertion means the cached token is no
            // longer good; the shim's retry must get a freshly minted one.
            if facade_refusal_code(&facade_reply(&reply))
                .is_some_and(|code| code.starts_with("assertion_"))
            {
                cache.drop_token(agent_id, session);
            }
            RelayReply::relayed(reply)
        }
        Err(error) => transport_refusal("plexus", error),
    };
    (reply, PATH_ASSERTION)
}

fn transport_refusal(stage: &str, error: TransportError) -> RelayReply {
    match error {
        TransportError::Refused { code, message } => RelayReply::refused(&code, stage, message),
        TransportError::Unavailable(message) => {
            RelayReply::refused("relay_unavailable", stage, message)
        }
        TransportError::OutcomeUnknown(message) => {
            RelayReply::refused("outcome_unknown", stage, message)
        }
    }
}

/// The daemon's relay state: the token cache and the outbound transport.
pub struct GhRelay {
    pub cache: TokenCache,
    pub transport: SubcRelayTransport,
}

impl GhRelay {
    pub fn new(connection_file: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            cache: TokenCache::default(),
            transport: SubcRelayTransport::new(connection_file),
        })
    }
}

/// Route-open refusals of a scoped route that end the call at once. The subc
/// client retries the transient ones (`scope_not_synced`, `scope_changed`)
/// inside the call's deadline and returns these four without retrying. Each
/// goes back to the shim under its own code, the way plexus's own scope
/// refusals do, and the write is never retried on the assertion path: the
/// daemon refused because of the scope, and an assertion would get the write
/// past that decision.
const TERMINAL_SCOPE_OPEN_REFUSALS: [&str; 4] = [
    subc_protocol::error_codes::SCOPE_NOT_LIVE,
    subc_protocol::error_codes::SCOPE_ENDED,
    subc_protocol::error_codes::SCOPE_EPOCH_REQUIRED,
    subc_protocol::error_codes::SCOPE_NOT_CARRIER,
];

/// The typed refusal for a failed route open, when the daemon refused it for
/// one of the scope reasons above.
fn scope_open_refusal(error: &subc_client_rs::CallError) -> Option<TransportError> {
    let body = error.route_open_refusal()?;
    TERMINAL_SCOPE_OPEN_REFUSALS
        .contains(&body.code.as_str())
        .then(|| TransportError::Refused {
            code: body.code.clone(),
            message: body.message.clone(),
        })
}

/// The reason, when `push` says the daemon closed a route to plexus because of
/// its scope and the route must not be reopened under that scope.
///
/// The daemon closes a scoped route with a `scope_*` reason when the scope
/// ends or the opener's standing under it changes, and the subc client classes
/// every such close as `MustNotReopen`. Closes for other reasons (a plexus
/// reload, crash or disable) say nothing about the scope, so they do not stop
/// a later route under it.
fn scope_close_reason(push: &subc_client_rs::ControlPush) -> Option<String> {
    if push.body.get("module_id").and_then(Value::as_str) != Some(PLEXUS_MODULE_ID) {
        return None;
    }
    let must_not_reopen = push.route_close_reason()?.disposition()
        == subc_client_rs::RouteCloseDisposition::MustNotReopen;
    let reason = push.body.get("reason").and_then(Value::as_str)?;
    (must_not_reopen && reason.starts_with("scope_")).then(|| reason.to_string())
}

/// How many ended scopes the relay remembers. There is one entry per session
/// epoch whose route the daemon closed for good, so the cap only bounds a very
/// long-lived daemon; the oldest entry is forgotten first.
const ENDED_SCOPES_CAP: usize = 1024;

fn push_distinct(scopes: &mut Vec<ScopeSelector>, scope: &ScopeSelector) {
    if !scopes.contains(scope) {
        scopes.push(scope.clone());
    }
}

/// The scoped routes to plexus the relay has open, and the scopes it must not
/// open a route under again.
///
/// The daemon's `route.closed` push names the module and the reason but not
/// the route, so a scope close is recorded as ending a scope only when it can
/// belong to no other: exactly one scope had a route to plexus open during the
/// current and the previous drain window. A drain window runs from one read of
/// the pending pushes to the next; the relay reads them before each scoped call
/// and again when that call's request returns. Looking back one extra window
/// covers a push that arrives just after the route it closed. When scopes
/// overlap the close is not recorded, and nothing unsafe follows: the daemon
/// itself refuses a reopen under a scope that ended (`scope_not_live`) or that
/// the relay may no longer carry (`scope_not_carrier`), and that refusal goes
/// back to the shim typed.
#[derive(Default)]
struct ScopedRoutes {
    /// One entry per scoped route open now; a scope repeats when two calls
    /// under it overlap.
    open: Vec<ScopeSelector>,
    /// Distinct scopes with a route open at any time in the current window.
    window: Vec<ScopeSelector>,
    /// The same, for the window before it.
    previous_window: Vec<ScopeSelector>,
    /// Scopes whose route the daemon closed for good, with the close reason.
    ended: std::collections::VecDeque<(ScopeSelector, String)>,
}

impl ScopedRoutes {
    /// The close reason, when the daemon closed this scope's route for good.
    fn ended(&self, scope: &ScopeSelector) -> Option<&str> {
        self.ended
            .iter()
            .find(|(ended, _)| ended == scope)
            .map(|(_, reason)| reason.as_str())
    }

    fn opened(&mut self, scope: &ScopeSelector) {
        self.open.push(scope.clone());
        push_distinct(&mut self.window, scope);
    }

    fn closed(&mut self, scope: &ScopeSelector) {
        if let Some(index) = self.open.iter().position(|open| open == scope) {
            self.open.swap_remove(index);
        }
    }

    /// Take the scope-close reasons read from the pushes since the last drain,
    /// then start a new window with the routes still open.
    fn drained(&mut self, reasons: Vec<String>) {
        if let Some(reason) = reasons.into_iter().next() {
            let mut candidates = self.window.clone();
            for scope in &self.previous_window {
                push_distinct(&mut candidates, scope);
            }
            if let [only] = candidates.as_slice() {
                if self.ended(only).is_none() {
                    self.ended.push_back((only.clone(), reason));
                    if self.ended.len() > ENDED_SCOPES_CAP {
                        self.ended.pop_front();
                    }
                }
            }
        }
        self.previous_window = std::mem::take(&mut self.window);
        for scope in &self.open {
            push_distinct(&mut self.window, scope);
        }
    }
}

/// What the transport tracks about scoped routes, and the receiver the daemon's
/// control pushes arrive on for the current connection.
#[derive(Default)]
struct ScopeState {
    routes: ScopedRoutes,
    pushes: Option<tokio::sync::mpsc::Receiver<subc_client_rs::ControlPush>>,
}

impl ScopeState {
    /// Read every pending control push and hand the scope closes among them to
    /// the route ledger.
    fn drain(&mut self) {
        let mut reasons = Vec::new();
        if let Some(pushes) = self.pushes.as_mut() {
            while let Ok(push) = pushes.try_recv() {
                reasons.extend(scope_close_reason(&push));
            }
        }
        self.routes.drained(reasons);
    }
}

/// Room for control pushes between two drains. A push that finds the receiver
/// full is dropped by the subc client, which only costs the ledger a record:
/// the daemon still refuses a reopen under an ended scope.
const CONTROL_PUSH_CAPACITY: usize = 64;

/// A failed outbound call, and whether the connection should be reopened
/// before the next one.
struct CallFailure {
    error: TransportError,
    reconnect: bool,
}

/// Open a route to `target`, send `body` on it, and close the route again.
///
/// With `scope`, the route is opened under that scope with the subc client's
/// scoped open. A refused scoped open is returned as it is and never retried
/// as an unscoped open: that would send the write without the stamp that
/// stands in for its assertion. The route is closed through its handle,
/// because the client's close by route key only finds unscoped routes.
async fn route_request(
    consumer: &subc_client_rs::SubcConsumer,
    target: subc_protocol::RouteTarget,
    identity: subc_protocol::BindIdentity,
    scope: Option<&ScopeSelector>,
    options: subc_client_rs::CallOptions,
    body: &Value,
) -> Result<Value, CallFailure> {
    use subc_client_rs::{CallError, CallOptions, CloseRouteOptions};
    let bytes = serde_json::to_vec(body).map_err(|error| CallFailure {
        error: TransportError::Unavailable(error.to_string()),
        reconnect: false,
    })?;
    let opened = match scope {
        Some(scope) => {
            consumer
                .open_route_scoped(target, identity, scope.clone(), options)
                .await
        }
        None => consumer.open_route(target, identity, options).await,
    };
    let route = match opened {
        Ok(route) => route,
        Err(error) => {
            if let Some(refusal) = scope.and_then(|_| scope_open_refusal(&error)) {
                return Err(CallFailure {
                    error: refusal,
                    reconnect: false,
                });
            }
            return Err(match error {
                CallError::Module(body) => CallFailure {
                    error: TransportError::Refused {
                        code: body.code,
                        message: body.message,
                    },
                    reconnect: false,
                },
                // A broken connection is reopened on the next relay.
                error => CallFailure {
                    error: TransportError::Unavailable(error.to_string()),
                    reconnect: true,
                },
            });
        }
    };
    let response = consumer
        .request(&route, bytes, CallOptions::default())
        .await;
    let _ = consumer
        .close_handle(&route, CloseRouteOptions::default())
        .await;
    match response {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| CallFailure {
            error: TransportError::OutcomeUnknown("the reply was not JSON".to_string()),
            reconnect: false,
        }),
        Err(CallError::Module(body)) => Err(CallFailure {
            error: TransportError::Refused {
                code: body.code,
                message: body.message,
            },
            reconnect: false,
        }),
        Err(CallError::NotSent(error)) => Err(CallFailure {
            error: TransportError::Unavailable(error.to_string()),
            reconnect: true,
        }),
        Err(error) => Err(CallFailure {
            error: TransportError::OutcomeUnknown(error.to_string()),
            reconnect: true,
        }),
    }
}

/// Outbound calls over the daemon's own authenticated subc connection. The
/// connection is opened lazily on first use and reopened after a failure.
pub struct SubcRelayTransport {
    connection_file: PathBuf,
    consumer: tokio::sync::Mutex<Option<Arc<subc_client_rs::SubcConsumer>>>,
    scopes: Mutex<ScopeState>,
}

impl SubcRelayTransport {
    fn new(connection_file: PathBuf) -> Self {
        Self {
            connection_file,
            consumer: tokio::sync::Mutex::new(None),
            scopes: Mutex::new(ScopeState::default()),
        }
    }

    /// A transport already holding `consumer`, for tests that serve it from a
    /// fake daemon without a module identity.
    #[cfg(test)]
    fn with_consumer(consumer: subc_client_rs::SubcConsumer) -> Self {
        let transport = Self::new(PathBuf::from("/nonexistent/aft-gh-relay-test"));
        let consumer = transport.adopt(consumer);
        *transport
            .consumer
            .try_lock()
            .expect("a new transport's consumer slot is free") = Some(consumer);
        transport
    }

    fn identity_available() -> bool {
        crate::launch_nonce::consumer_identity().is_some()
    }

    fn lock_scopes(&self) -> std::sync::MutexGuard<'_, ScopeState> {
        self.scopes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Start reading a new connection's control pushes, so scope closes on it
    /// reach the route ledger.
    fn adopt(&self, consumer: subc_client_rs::SubcConsumer) -> Arc<subc_client_rs::SubcConsumer> {
        let pushes = consumer.control_pushes(CONTROL_PUSH_CAPACITY);
        self.lock_scopes().pushes = Some(pushes);
        Arc::new(consumer)
    }

    async fn consumer(&self) -> Result<Arc<subc_client_rs::SubcConsumer>, TransportError> {
        let mut slot = self.consumer.lock().await;
        if let Some(consumer) = slot.as_ref() {
            return Ok(Arc::clone(consumer));
        }
        // The identity is fixed for the life of the process, so checking it
        // before each new connection is the same as checking it on every call.
        if !Self::identity_available() {
            return Err(TransportError::Unavailable(
                "the daemon has no subc module identity to relay with".to_string(),
            ));
        }
        let options = subc_client_rs::ConsumerOptions {
            call_timeout: RELAY_CALL_TIMEOUT,
            ..subc_client_rs::ConsumerOptions::default()
        };
        let consumer = crate::fleet_status::connect_subc_consumer(&self.connection_file, options)
            .await
            .map_err(|error| TransportError::Unavailable(error.to_string()))?;
        let consumer = self.adopt(consumer);
        *slot = Some(Arc::clone(&consumer));
        Ok(consumer)
    }

    async fn call(
        &self,
        target: subc_protocol::RouteTarget,
        project_root: &str,
        session: &str,
        scope: Option<&ScopeSelector>,
        body: Value,
    ) -> Result<Value, TransportError> {
        if let Some(scope) = scope {
            let ended = {
                let mut state = self.lock_scopes();
                state.drain();
                state.routes.ended(scope).map(str::to_string)
            };
            if let Some(reason) = ended {
                return Err(TransportError::Refused {
                    message: format!(
                        "the daemon closed this session's scoped route to plexus ({reason}); a route closed that way is not reopened under the same scope"
                    ),
                    code: reason,
                });
            }
        }
        let consumer = self.consumer().await?;
        let identity = subc_protocol::BindIdentity::new(
            if project_root.is_empty() {
                "/"
            } else {
                project_root
            }
            .to_string(),
            "aft-gh-relay",
            session.to_string(),
        );
        let options = subc_client_rs::CallOptions {
            consumer_identity: crate::launch_nonce::consumer_identity(),
            ..subc_client_rs::CallOptions::default()
        };
        if let Some(scope) = scope {
            self.lock_scopes().routes.opened(scope);
        }
        let outcome = route_request(&consumer, target, identity, scope, options, &body).await;
        if let Some(scope) = scope {
            let mut state = self.lock_scopes();
            state.drain();
            state.routes.closed(scope);
        }
        match outcome {
            Ok(reply) => Ok(reply),
            Err(failure) => {
                if failure.reconnect {
                    *self.consumer.lock().await = None;
                }
                Err(failure.error)
            }
        }
    }
}

impl RelayTransport for SubcRelayTransport {
    async fn prefrontal(
        &self,
        project_root: &str,
        session: &str,
        body: Value,
    ) -> Result<Value, TransportError> {
        self.call(
            subc_protocol::RouteTarget::ManagementSurface {
                module_id: PREFRONTAL_MODULE_ID.to_string(),
            },
            project_root,
            session,
            None,
            body,
        )
        .await
    }

    async fn plexus(
        &self,
        project_root: &str,
        session: &str,
        scope: Option<&ScopeSelector>,
        body: Value,
    ) -> Result<Value, TransportError> {
        self.call(
            subc_protocol::RouteTarget::ToolProvider {
                module_id: PLEXUS_MODULE_ID.to_string(),
            },
            project_root,
            session,
            scope,
            body,
        )
        .await
    }

    /// True: the subc client opens a route to plexus under the session's scope
    /// (`SubcConsumer::open_route_scoped`, since subc-client-rs 0.23.2), so a
    /// session whose scope delegates to its agent takes the stamp path.
    fn opens_scoped_routes(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
