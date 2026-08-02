//! Daemon client admission: shared client deadlines, capacity admission, and
//! typed saturation rejections.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant, timeout, timeout_at};

use super::{
    BrokerStream, BrokerStreamTransport, DaemonAuthPreface, DaemonHandshake, JsonRpcRequest,
    JsonRpcResponse, McpMethod, Result, StoreAdministration, TraceDecayError, classify_mcp_method,
    log_daemon_event, parse_daemon_invocation_request, read_line_handling_wire_oversized,
    write_json_rpc_response,
};
use crate::mcp::ErrorCode;
use crate::support::weak_registry::WeakRegistry;
use tracedecay_application::{ApplicationProblem, LegalAction, RetryDirective, SafeDiagnostic};

pub(crate) const MAX_CONCURRENT_DAEMON_CLIENTS: usize = 64;
pub(crate) const RESERVED_DAEMON_CONTROL_CLIENTS: usize = 4;
/// Allows one proxy meaningful parallelism while preventing it from occupying
/// more than 8 of the 60 general slots.
pub(crate) const MAX_CONCURRENT_REQUESTS_PER_DAEMON_CLIENT: usize = 8;
pub(crate) const DAEMON_SATURATION_RESPONSE_DEADLINE: Duration = Duration::from_millis(250);

/// One monotonic wall-clock budget shared across daemon client connect, write,
/// read, and decode stages. Outer CLI wrappers pass their Instant so a timeout
/// cancels the in-flight stage rather than starting a fresh Duration clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DaemonClientDeadline {
    deadline: Instant,
}

impl DaemonClientDeadline {
    pub(crate) fn until(deadline: Instant) -> Result<Self> {
        if Instant::now() >= deadline {
            return Err(TraceDecayError::Config {
                message: "daemon client deadline already elapsed".to_string(),
            });
        }
        Ok(Self { deadline })
    }

    pub(crate) fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| TraceDecayError::Config {
                message: "daemon client deadline already elapsed".to_string(),
            })
    }

    pub(crate) async fn run<F, T>(
        &self,
        stage: &'static str,
        request_label: &str,
        fut: F,
    ) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        match timeout_at(self.deadline, fut).await {
            Ok(result) => result,
            Err(_) => Err(TraceDecayError::Config {
                message: format!(
                    "daemon {request_label} timed out during {stage} before deadline; request outcome may be unknown"
                ),
            }),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DaemonClientAdmission {
    general_permits: Arc<tokio::sync::Semaphore>,
    reserved_permits: Arc<tokio::sync::Semaphore>,
    capacity: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonClientAdmissionClass {
    General,
    ReservedControl,
}

/// How long a park may keep its admission permit before surrendering it.
///
/// A request that finishes inside this grace never touches the semaphore, so
/// the hot path costs one timer registration and nothing else. Only a request
/// that is genuinely parked — waiting on a project open, on the writer gate, or
/// on a single-flight generation decode — gives its slot back.
pub(crate) const ADMISSION_PARK_GRACE: Duration = Duration::from_millis(50);

/// The admission slot one accepted connection currently holds.
///
/// Live defect this exists for: the 60 general admission slots were held by
/// requests *parked* on warm-up, on the writer gate, or on a generation decode
/// while the reader pool sat completely idle. Every arriving request — including
/// tools that need no generation at all — was then shed with the retryable
/// `bulk_capacity_reached`. Admission must bound concurrent *work*, and a
/// request asleep on a barrier is not work.
///
/// The permit therefore lives here behind a lock instead of inside the connection
/// future, so [`park_admission`] can take it out for the duration of a park and
/// put it back afterwards.
pub(crate) struct ParkableConnectionAdmission {
    permits: Arc<tokio::sync::Semaphore>,
    held: std::sync::Mutex<Option<tokio::sync::OwnedSemaphorePermit>>,
}

impl ParkableConnectionAdmission {
    fn new(
        permits: Arc<tokio::sync::Semaphore>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Self {
        Self {
            permits,
            held: std::sync::Mutex::new(Some(permit)),
        }
    }

    /// Surrender the slot. Reports whether this call is the one that released it,
    /// so a nested park leaves the outer park's re-acquisition as the only one.
    fn release(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some()
    }

    /// Re-take a slot after the park ended.
    ///
    /// The wait is fair and unbounded on purpose. It cannot deadlock: no park in
    /// this daemon completes by way of another *admitted* request — project opens
    /// and generation rebuilds run on their own background tasks — so the permits
    /// this caller waits on are held only by requests that are actively finishing.
    /// Tokio hands a released permit to a queued waiter before any newly arriving
    /// `try_acquire`, so a request resuming from a park is served ahead of a fresh
    /// connection rather than being starved by the load that made it park.
    async fn reacquire(&self) {
        let Ok(permit) = Arc::clone(&self.permits).acquire_owned().await else {
            return;
        };
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_none() {
            *held = Some(permit);
        }
    }
}

tokio::task_local! {
    static CONNECTION_ADMISSION: Arc<ParkableConnectionAdmission>;
}

/// Run `future` without holding a general admission slot across a long park.
///
/// Wrap the wait, never the work: the returned future keeps its slot for
/// [`ADMISSION_PARK_GRACE`], and only surrenders it if the wait outlives that
/// grace. Nesting is safe — the innermost park that still finds a held permit is
/// the one that releases and re-acquires it.
///
/// Outside a connection scope (tests, background tasks, reserved-control
/// clients) this is a transparent passthrough.
pub(crate) async fn park_admission<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    // The same pinned future is resumed after the grace expires, so the grace is
    // an observation of the wait, never a cancellation of it.
    let mut future = std::pin::pin!(future);
    if let Ok(output) = timeout(ADMISSION_PARK_GRACE, &mut future).await {
        return output;
    }
    let Ok(lease) = CONNECTION_ADMISSION.try_with(Arc::clone) else {
        return future.await;
    };
    if !lease.release() {
        return future.await;
    }
    let output = future.await;
    lease.reacquire().await;
    output
}

/// Capture the calling connection's admission lease, if it has one.
///
/// Needed because `rmcp` serves each connection from a task it spawns itself.
/// A task-local does not cross that spawn, so the adapter captures the lease
/// while it is still constructed on the connection task and re-enters the scope
/// per request with [`in_connection_admission`]. Without that, every MCP
/// `tools/call` — the daemon's main serving path — would park on a generation
/// decode while still holding its admission slot.
pub(crate) fn current_connection_admission() -> Option<Arc<ParkableConnectionAdmission>> {
    CONNECTION_ADMISSION.try_with(Arc::clone).ok()
}

/// Re-enter a captured connection's admission scope on another task.
pub(crate) async fn in_connection_admission<F>(
    lease: Option<Arc<ParkableConnectionAdmission>>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    match lease {
        Some(lease) => CONNECTION_ADMISSION.scope(lease, future).await,
        None => future.await,
    }
}

/// Serve one accepted connection under its admission permit.
///
/// General clients run inside the parkable scope so every [`park_admission`]
/// site below them can hand the slot back. Reserved-control clients answer
/// catalog and status traffic that touches no barrier, so they simply hold their
/// permit for the connection.
pub(crate) async fn with_connection_admission<F>(permit: DaemonClientPermit, future: F) -> F::Output
where
    F: std::future::Future,
{
    if permit.class == DaemonClientAdmissionClass::General {
        let lease = Arc::clone(&permit.lease);
        return CONNECTION_ADMISSION
            .scope(lease, async move {
                let _permit = permit;
                future.await
            })
            .await;
    }
    let _permit = permit;
    future.await
}

pub(crate) struct DaemonClientPermit {
    lease: Arc<ParkableConnectionAdmission>,
    class: DaemonClientAdmissionClass,
}

impl DaemonClientPermit {
    pub(crate) fn class(&self) -> DaemonClientAdmissionClass {
        self.class
    }
}

pub(crate) enum DaemonClientAdmissionOutcome {
    Admitted(DaemonClientPermit),
    Saturated(DaemonClientSaturationResponse),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DaemonClientFairnessKey {
    profile_root: PathBuf,
    global_db_path: PathBuf,
    client_instance_id: String,
}

#[derive(Clone)]
pub(crate) struct DaemonPerClientAdmission {
    /// Weak entries retain no client state after the last in-flight lease.
    /// Reconnects with the same validated process id reuse the live semaphore.
    clients: Arc<WeakRegistry<DaemonClientFairnessKey, tokio::sync::Semaphore>>,
    capacity: usize,
}

#[derive(Debug)]
pub(crate) struct DaemonPerClientPermit {
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DaemonClientSaturationResponse {
    pub(crate) kind: DaemonClientSaturationKind,
    pub(crate) retryable: bool,
    pub(crate) capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// Variant names are the serialized wire contract (`*_capacity_reached`); renaming is not allowed.
#[allow(clippy::enum_variant_names)]
pub(crate) enum DaemonClientSaturationKind {
    ClientCapacityReached,
    PerClientCapacityReached,
    BulkCapacityReached,
}

impl Default for DaemonPerClientAdmission {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_REQUESTS_PER_DAEMON_CLIENT)
    }
}

impl DaemonPerClientAdmission {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            clients: Arc::new(WeakRegistry::new()),
            capacity,
        }
    }

    pub(crate) fn try_admit(
        &self,
        handshake: &DaemonHandshake,
    ) -> std::result::Result<DaemonPerClientPermit, DaemonClientSaturationResponse> {
        if !valid_client_instance_id(&handshake.client_instance_id) {
            return Ok(DaemonPerClientPermit { _permit: None });
        }
        let key = DaemonClientFairnessKey {
            profile_root: handshake.client_identity.profile_root.clone(),
            global_db_path: handshake.client_identity.global_db_path.clone(),
            client_instance_id: handshake.client_instance_id.clone(),
        };
        let capacity = self.capacity;
        let (semaphore, _hit) = self
            .clients
            .get_or_insert_with(key, || Arc::new(tokio::sync::Semaphore::new(capacity)));
        Arc::clone(&semaphore)
            .try_acquire_owned()
            .map(|permit| DaemonPerClientPermit {
                _permit: Some(permit),
            })
            .map_err(|_| DaemonClientSaturationResponse {
                kind: DaemonClientSaturationKind::PerClientCapacityReached,
                retryable: true,
                capacity: self.capacity,
            })
    }

    pub(crate) fn try_admit_request(
        &self,
        handshake: &DaemonHandshake,
        request_line: &str,
    ) -> std::result::Result<DaemonPerClientPermit, DaemonClientSaturationResponse> {
        if is_reserved_control_request(request_line) {
            return Ok(DaemonPerClientPermit { _permit: None });
        }
        self.try_admit(handshake)
    }

    #[cfg(test)]
    pub(crate) fn tracked_client_count(&self) -> usize {
        self.clients.retain_live();
        self.clients.len()
    }
}

pub(crate) fn valid_client_instance_id(client_instance_id: &str) -> bool {
    let bytes = client_instance_id.as_bytes();
    (bytes.len() == 32
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
        || client_instance_id.strip_prefix("mcp-").is_some_and(|tail| {
            !tail.is_empty() && tail.len() <= 20 && tail.bytes().all(|byte| byte.is_ascii_digit())
        })
}

impl DaemonClientAdmission {
    pub(crate) fn new(capacity: usize) -> Self {
        let reserved = if capacity == 0 {
            0
        } else {
            RESERVED_DAEMON_CONTROL_CLIENTS.min(capacity)
        };
        Self::with_reserved_capacity(capacity, reserved)
    }

    pub(crate) fn with_reserved_capacity(capacity: usize, reserved: usize) -> Self {
        let reserved = reserved.min(capacity);
        Self {
            general_permits: Arc::new(tokio::sync::Semaphore::new(capacity - reserved)),
            reserved_permits: Arc::new(tokio::sync::Semaphore::new(reserved)),
            capacity,
        }
    }

    /// General slots currently free. Test probe for "the park gave its slot
    /// back" and for permit-leak assertions.
    #[cfg(test)]
    pub(crate) fn available_general_permits(&self) -> usize {
        self.general_permits.available_permits()
    }

    pub(crate) fn try_admit(&self) -> DaemonClientAdmissionOutcome {
        if let Ok(permit) = Arc::clone(&self.general_permits).try_acquire_owned() {
            return DaemonClientAdmissionOutcome::Admitted(DaemonClientPermit {
                lease: Arc::new(ParkableConnectionAdmission::new(
                    Arc::clone(&self.general_permits),
                    permit,
                )),
                class: DaemonClientAdmissionClass::General,
            });
        }
        if let Ok(permit) = Arc::clone(&self.reserved_permits).try_acquire_owned() {
            return DaemonClientAdmissionOutcome::Admitted(DaemonClientPermit {
                lease: Arc::new(ParkableConnectionAdmission::new(
                    Arc::clone(&self.reserved_permits),
                    permit,
                )),
                class: DaemonClientAdmissionClass::ReservedControl,
            });
        }
        DaemonClientAdmissionOutcome::Saturated(DaemonClientSaturationResponse {
            kind: DaemonClientSaturationKind::ClientCapacityReached,
            retryable: true,
            capacity: self.capacity,
        })
    }
}

impl DaemonClientSaturationResponse {
    pub(crate) fn into_json_rpc_with_id(self, id: serde_json::Value) -> JsonRpcResponse {
        let message = match self.kind {
            DaemonClientSaturationKind::ClientCapacityReached => "daemon client capacity reached",
            DaemonClientSaturationKind::PerClientCapacityReached => {
                "daemon per-client capacity reached; retry after this client's active requests finish"
            }
            DaemonClientSaturationKind::BulkCapacityReached => {
                "daemon bulk capacity reached; reserved capacity is limited to health and status"
            }
        };
        JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InternalError,
            message.to_string(),
            serde_json::to_value(self).ok(),
        )
    }
}

fn invocation_saturation_response(
    request_line: &str,
    saturation: &DaemonClientSaturationResponse,
) -> Option<super::DaemonInvocationResponse> {
    let request = parse_daemon_invocation_request(request_line)?.ok()?;
    let code = match saturation.kind {
        DaemonClientSaturationKind::ClientCapacityReached => "daemon_client_capacity_saturated",
        DaemonClientSaturationKind::PerClientCapacityReached => {
            "daemon_per_client_capacity_saturated"
        }
        DaemonClientSaturationKind::BulkCapacityReached => "daemon_bulk_capacity_saturated",
    };
    Some(super::DaemonInvocationResponse::application_problem(
        request.request_id,
        ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic {
                code: code.to_owned(),
                message: "The owning TraceDecay daemon has no request capacity".to_owned(),
            },
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
    ))
}

async fn write_invocation_response(
    transport: &mut impl crate::mcp::McpTransport,
    response: &super::DaemonInvocationResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

/// MCP handshake and catalog-discovery methods.
///
/// These are the only requests whose rejection costs a client its entire tool
/// registry rather than one call, so admission treats them as control traffic.
pub(crate) fn is_mcp_discovery_method(method: &str) -> bool {
    matches!(
        classify_mcp_method(method),
        McpMethod::Initialize | McpMethod::InitializedAck | McpMethod::ToolsList
    )
}

pub(crate) fn is_reserved_control_request(request_line: &str) -> bool {
    let Ok(request) = serde_json::from_str::<JsonRpcRequest>(request_line.trim()) else {
        return false;
    };
    if request.method == super::DAEMON_SHUTDOWN_METHOD {
        return request.id.is_some_and(|id| !id.is_null());
    }
    // MCP discovery and handshake are reserved, never bulk. They render the
    // immutable tool catalog and touch no store, so they cost O(catalog) and
    // cannot be what saturated the daemon. Rejecting them is uniquely
    // unrecoverable: a host caches `tools/list` for the whole session, so one
    // saturation rejection leaves that client with *zero* tracedecay tools
    // until it restarts, while a rejected `tools/call` is merely retried.
    if is_mcp_discovery_method(&request.method) {
        return true;
    }
    if request.method != "tools/call" {
        return false;
    }
    let Ok((tool_name, _)) = super::projectless_tool_call(request.params.as_ref()) else {
        return false;
    };
    matches!(
        tool_name,
        "tracedecay_status"
            | "tracedecay_storage_status"
            | "tracedecay_runtime"
            | "tracedecay_health"
            | "tracedecay_diagnostics"
            | "tracedecay_diagnose"
            | "tracedecay_memory_status"
            | "tracedecay_lcm_status"
            | "tracedecay_lcm_doctor"
    )
}

#[cfg(any(not(unix), test))]
pub(crate) fn daemon_shutdown_response(request_line: &str) -> Option<JsonRpcResponse> {
    let request = serde_json::from_str::<JsonRpcRequest>(request_line.trim()).ok()?;
    let id = request.id.filter(|id| !id.is_null())?;
    (request.method == super::DAEMON_SHUTDOWN_METHOD)
        .then(|| JsonRpcResponse::success(id, serde_json::json!({"accepted": true})))
}

pub(crate) async fn reject_reserved_bulk_request(
    transport: &mut impl crate::mcp::McpTransport,
    request_line: &str,
    capacity: usize,
) -> Result<()> {
    reject_admitted_request(
        transport,
        request_line,
        DaemonClientSaturationResponse {
            kind: DaemonClientSaturationKind::BulkCapacityReached,
            retryable: true,
            capacity,
        },
    )
    .await
}

pub(crate) async fn reject_admitted_request(
    transport: &mut impl crate::mcp::McpTransport,
    request_line: &str,
    saturation: DaemonClientSaturationResponse,
) -> Result<()> {
    let outcome = match saturation.kind {
        DaemonClientSaturationKind::ClientCapacityReached => "client_capacity_reached",
        DaemonClientSaturationKind::PerClientCapacityReached => "per_client_capacity_reached",
        DaemonClientSaturationKind::BulkCapacityReached => "bulk_capacity_reached",
    };
    if let Some(response) = invocation_saturation_response(request_line, &saturation) {
        write_invocation_response(transport, &response).await?;
    } else {
        let parsed = serde_json::from_str::<JsonRpcRequest>(request_line).ok();
        // A notification (a well-formed request carrying no id) must never be
        // answered: JSON-RPC 2.0 forbids it, and a stray null-id error frame
        // desynchronizes strict MCP clients mid-handshake. Only an
        // unparseable line falls back to a null-id error, which is the
        // correct reply for a malformed request.
        let notification = parsed.as_ref().is_some_and(|request| request.id.is_none());
        if !notification {
            let request_id = parsed
                .and_then(|request| request.id)
                .unwrap_or(serde_json::Value::Null);
            let response = saturation.into_json_rpc_with_id(request_id);
            write_json_rpc_response(transport, &response).await?;
        }
    }
    log_daemon_event("daemon_client", &[("outcome", outcome.to_string())]);
    Ok(())
}

async fn saturated_request_line(transport: &mut BrokerStreamTransport) -> Result<Option<String>> {
    // A broker client sends a handshake before its JSON-RPC request, optionally
    // preceded by an auth preface. Consume those frames so the rejection uses
    // the request ID instead of being discarded as a notification.
    let Some(first_line) = read_line_handling_wire_oversized(transport).await? else {
        return Ok(None);
    };
    let handshake_line = if DaemonAuthPreface::from_line(&first_line).is_ok() {
        let Some(handshake_line) = read_line_handling_wire_oversized(transport).await? else {
            return Ok(None);
        };
        handshake_line
    } else {
        first_line
    };
    DaemonHandshake::from_line(&handshake_line)?;
    read_line_handling_wire_oversized(transport).await
}

pub(crate) async fn reject_saturated_daemon_client(
    stream: BrokerStream,
    response: DaemonClientSaturationResponse,
) {
    let mut transport = BrokerStreamTransport::new(stream);
    let response = async {
        let request_line = saturated_request_line(&mut transport)
            .await?
            .unwrap_or_default();
        if let Some(invocation) = invocation_saturation_response(&request_line, &response) {
            write_invocation_response(&mut transport, &invocation).await
        } else {
            let request_id = serde_json::from_str::<JsonRpcRequest>(&request_line)
                .ok()
                .and_then(|request| request.id)
                .unwrap_or(serde_json::Value::Null);
            write_json_rpc_response(&mut transport, &response.into_json_rpc_with_id(request_id))
                .await
        }
    };
    match timeout(DAEMON_SATURATION_RESPONSE_DEADLINE, response).await {
        Ok(Ok(())) => log_daemon_event(
            "daemon_client",
            &[("outcome", "client_capacity_reached".to_string())],
        ),
        Ok(Err(error)) => log_daemon_event(
            "daemon_client",
            &[
                ("outcome", "saturation_response_failed".to_string()),
                ("error", error.to_string()),
            ],
        ),
        Err(_) => log_daemon_event(
            "daemon_client",
            &[("outcome", "saturation_response_timeout".to_string())],
        ),
    }
}

pub(super) fn coordinated_dashboard_automation_writer(
    administration: StoreAdministration,
) -> crate::dashboard::DashboardAutomationWriter {
    Arc::new(move |operation| {
        let administration = administration.clone();
        Box::pin(async move { administration.with_writer(operation).await })
    })
}

pub(super) fn coordinated_background_refresh_writer(
    administration: StoreAdministration,
) -> crate::mcp::server::BackgroundRefreshWriter {
    Arc::new(move |mut request| {
        let administration = administration.clone();
        Box::pin(async move {
            let canonical_root = request
                .project_root
                .canonicalize()
                .unwrap_or_else(|_| request.project_root.clone());
            let active_branch = crate::branch::current_branch(&canonical_root);
            let graph = administration
                .mounted_project_graphs()
                .await
                .into_iter()
                .find(|graph| {
                    graph.project_root() == canonical_root
                        && graph.active_branch() == active_branch.as_deref()
                })
                .ok_or_else(|| TraceDecayError::Config {
                    message: "retained background refresh graph is unavailable".to_string(),
                })?;
            request.graph = graph;
            administration
                .with_writer(|| async move {
                    crate::mcp::server::execute_background_refresh_direct(request).await
                })
                .await
        })
    })
}
