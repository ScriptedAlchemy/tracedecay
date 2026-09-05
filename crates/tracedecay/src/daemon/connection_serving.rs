//! Per-connection serving: one accepted daemon client, start to finish.
//!
//! Covers the authenticated Unix socket path, the routed rmcp bridge, and the
//! portable broker path. Each entry point owns framing, project-owner routing,
//! and connection teardown for exactly one client.

use super::profile_host_admission_replay::ProfileHostAdmissionBootstrapStatus;
use super::*;
use tracedecay_daemon_protocol::DaemonInvocationPayload;
use tracedecay_daemon_service::{DaemonInvocationService, Lease, cancel, register};

/// Hermetic production-route benchmark support for the typed RMCP transport.
///
/// This is test-only so the benchmark can enter the same broker connection,
/// routing, selected-project response, delivery-settlement, and RMCP adapter
/// path as the daemon without adding a shipped benchmark API.
#[cfg(feature = "rmcp-benchmark")]
pub mod rmcp_benchmark;

type ProjectOwnerAwaitFutureV1<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<(T, VecDeque<String>)>>> + Send + 'a>,
>;

type BrokerConnectionPhaseFutureV1<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

#[inline(never)]
fn boxed_broker_connection_phase<'a, T>(
    future: impl std::future::Future<Output = Result<T>> + Send + 'a,
) -> BrokerConnectionPhaseFutureV1<'a, T>
where
    T: Send + 'a,
{
    Box::pin(future)
}

fn report_profile_host_admission_bootstrap_status(
    status: Option<ProfileHostAdmissionBootstrapStatus>,
) {
    let Some(ProfileHostAdmissionBootstrapStatus::Terminal(error)) = status else {
        return;
    };
    if let Some((authority, reason)) = error.reset_required_context() {
        log_daemon_event(
            "profile_host_admission_bootstrap_terminal_observed",
            &[
                ("reason_code", "reset_required".to_owned()),
                ("authority", authority.to_owned()),
                ("reason", reason.to_owned()),
            ],
        );
    } else if let Some((reason_code, retryable, detail)) = error.hook_runtime_context() {
        log_daemon_event(
            "profile_host_admission_bootstrap_terminal_observed",
            &[
                ("reason_code", reason_code.to_owned()),
                ("retryable", retryable.to_string()),
                ("detail", detail.to_owned()),
            ],
        );
    } else if let Some((reason_code, retryable, detail)) = error.project_route_context() {
        log_daemon_event(
            "profile_host_admission_bootstrap_terminal_observed",
            &[
                ("reason_code", reason_code.to_owned()),
                ("retryable", retryable.to_string()),
                ("detail", detail.to_owned()),
            ],
        );
    } else {
        log_daemon_event(
            "profile_host_admission_bootstrap_terminal_observed",
            &[("reason_code", "bootstrap_operation_failed".to_owned())],
        );
    }
}

#[cfg(all(unix, test))]
pub(super) async fn serve_socket_client(
    stream: tokio::net::UnixStream,
    engine: DaemonEngine,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        BrokerStream::Unix(stream),
        engine,
        None,
        DaemonClientAdmissionClass::General,
    ))
    .await
}

#[cfg(unix)]
pub(super) async fn serve_authenticated_socket_client_with_class(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: String,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    Box::pin(serve_broker_socket_client(
        stream,
        engine,
        Some(auth_token),
        admission_class,
    ))
    .await
}

#[hotpath::measure(label = "daemon.engine.transport.rmcp", future = true)]
pub(super) async fn serve_routed_rmcp_connection(
    server: Arc<crate::mcp::McpServer>,
    transport: BrokerStreamTransport,
    first_request_line: String,
    pending_lines: VecDeque<String>,
    initialize_route: Option<InitializeRouteMetadata>,
    timings_enabled: bool,
    lifecycle: &DaemonLifecycle,
) -> Result<()> {
    serve_routed_rmcp_connection_inner(
        server,
        transport,
        first_request_line,
        pending_lines,
        initialize_route,
        timings_enabled,
        lifecycle,
    )
    .await
}

fn serve_routed_rmcp_connection_inner(
    server: Arc<crate::mcp::McpServer>,
    transport: BrokerStreamTransport,
    first_request_line: String,
    pending_lines: VecDeque<String>,
    initialize_route: Option<InitializeRouteMetadata>,
    timings_enabled: bool,
    lifecycle: &DaemonLifecycle,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
    // Erase the deeply nested rmcp service future before it reaches the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let initialize_response_decorator = initialize_route.map(|route| {
            Arc::new(move |response: &mut JsonRpcResponse| {
                attach_initialize_route_metadata(response, &route);
            }) as RmcpInitializeResponseDecorator
        });
        let mut transport =
            transport.with_project_response_lifecycle(server.project_server_response_lifecycle());
        transport.push_replay(first_request_line)?;
        for line in pending_lines {
            transport.push_replay(line)?;
        }
        let adapter =
            RmcpConnectionAdapter::new(server, timings_enabled, initialize_response_decorator)?;
        let transport = transport
            .with_rmcp_selected_project_responses(adapter.selected_project_responses())
            .with_rmcp_work_delivery_settlement(adapter.work_delivery_settlement());
        let running = adapter
            .serve(transport)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("rmcp server initialization failed: {error}"),
            })?;
        let cancellation = running.cancellation_token();
        let waiting = running.waiting();
        tokio::pin!(waiting);
        let result = tokio::select! {
            result = &mut waiting => result,
            () = lifecycle.wait_for_draining() => {
                hotpath::measure_block!("daemon.engine.transport.cancel", cancellation.cancel());
                waiting.await
            }
        };
        result.map_err(|error| TraceDecayError::Config {
            message: format!("rmcp server task failed: {error}"),
        })?;
        Ok(())
    })
}

fn is_mcp_initialize_request(request: Option<&JsonRpcRequest>) -> bool {
    request.is_some_and(|request| request.method == "initialize")
}

/// Answer an unparseable handshake with one typed refusal frame and drain
/// the input the client already pipelined, then let the connection close.
///
/// Propagating the parse failure with `?` here drops the socket while the
/// client's first request is still unread, which the kernel reports to the
/// client as `Connection reset by peer` — a raw transport error that hides
/// wire-revision skew. The refusal frame plus a drained receive buffer turns
/// that into a readable typed refusal followed by a clean EOF.
async fn refuse_unparseable_handshake(
    transport: &mut (impl tracedecay_mcp::McpTransport + Send),
    handshake_line: &str,
    daemon_version: &str,
) {
    let refusal = tracedecay_daemon_protocol::DaemonHandshakeRefusal::for_unparseable_handshake(
        handshake_line,
        daemon_version,
    );
    write_refusal_and_drain(transport, &refusal).await;
}

/// Answer a rejected auth preface with one typed refusal frame and drain the
/// input the client already pipelined, then let the connection close.
///
/// Tearing the socket down on the bare `Err` left the client's pending read
/// at EOF, which every client surface reported as "connection closed, the
/// outcome is unknown" — a transport mystery for what is a definitive daemon
/// answer. The frame never echoes the supplied token.
async fn refuse_unauthenticated_client(
    transport: &mut (impl tracedecay_mcp::McpTransport + Send),
    daemon_version: &str,
) {
    let refusal = tracedecay_daemon_protocol::DaemonHandshakeRefusal::for_rejected_authentication(
        daemon_version,
    );
    write_refusal_and_drain(transport, &refusal).await;
}

/// The typed refusal a connection answers with when its profile-identity
/// binding did not settle inside [`PROJECT_OPEN_REQUEST_DEADLINE`].
///
/// It carries [`PROJECT_WARMING_RETRY_HINT`], so every client surface already
/// classifies it as retryable through `error_message_is_project_open_retryable`.
fn profile_identity_warming_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("TraceDecay profile runtime {PROJECT_WARMING_RETRY_HINT}"),
    }
}

/// Binds the handshake's authenticated profile identity under the same deadline
/// every other project-scoped open answers to.
///
/// The stage reaches a cold `DaemonSessionRuntimeRegistryV1::open` through
/// `registered_profile_database`, and the only other arm the callers raced it
/// against was peer full close — which a half-closed one-shot client never
/// satisfies while it is still waiting for its response. A contended cold open
/// therefore pinned the connection, its lifecycle activity permit and its
/// admission slot for as long as the open took.
///
/// The binding runs on its own task so the deadline yields the warming refusal
/// *without* cancelling the open: the registry is a per-profile `OnceCell`, so
/// dropping the initializer future would abandon the partially finished open
/// and make the next client start over. Detaching it instead lets this client's
/// retry — or the next one — find the registry already warm.
async fn bind_authenticated_profile_identity_within_deadline(
    handshake: &mut DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<StoreAdministration> {
    let mut binding_handshake = handshake.clone();
    let binding_administration = store_administration.clone();
    let binding = tokio::spawn(async move {
        Box::pin(bind_authenticated_profile_identity(
            &mut binding_handshake,
            &binding_administration,
        ))
        .await
        .map(|administration| (binding_handshake, administration))
    });
    match tokio::time::timeout(PROFILE_IDENTITY_BIND_DEADLINE, binding).await {
        Ok(Ok(Ok((bound_handshake, administration)))) => {
            *handshake = bound_handshake;
            Ok(administration)
        }
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(join)) => Err(TraceDecayError::Config {
            message: format!("daemon profile identity binding failed to join: {join}"),
        }),
        Err(_) => Err(profile_identity_warming_error()),
    }
}

/// Answers the first request with the retryable warming refusal its
/// profile-identity binding produced, so a client that missed the deadline
/// sees a typed retry instead of a closed socket.
async fn refuse_warming_profile_identity(
    transport: &mut (impl tracedecay_mcp::McpTransport + Send),
    request: &AuthenticatedFirstRequest,
    error: &TraceDecayError,
) -> Result<()> {
    // JSON-RPC 2.0 forbids answering a notification, and a stray null-id frame
    // desynchronizes strict MCP clients mid-handshake. Only an unparseable line
    // falls back to the null id, exactly as `reject_admitted_request` does.
    if request.parsed().is_some_and(|request| request.id.is_none()) {
        return Ok(());
    }
    let request_id = request
        .parsed()
        .and_then(|request| request.id.clone())
        .unwrap_or(serde_json::Value::Null);
    let response = JsonRpcResponse::error(request_id, ErrorCode::InternalError, error.to_string());
    write_json_rpc_response(transport, &response).await
}

async fn write_refusal_and_drain(
    transport: &mut (impl tracedecay_mcp::McpTransport + Send),
    refusal: &tracedecay_daemon_protocol::DaemonHandshakeRefusal,
) {
    hotpath::gauge!("daemon.engine.handshake.refused").inc(1_u64);
    log_daemon_event(
        "daemon_handshake_refused",
        &[
            ("refusal", format!("{:?}", refusal.refusal)),
            ("daemon_version", refusal.daemon_version.clone()),
        ],
    );
    // Best effort: a peer that already vanished cannot read a refusal.
    if let Ok(line) = refusal.to_line() {
        let _ = transport.write_line(&line).await;
        let _ = transport.write_line("\n").await;
        let _ = transport.flush().await;
    }
    for _ in 0..4 {
        match tokio::time::timeout(
            Duration::from_millis(100),
            read_line_handling_wire_oversized(transport),
        )
        .await
        {
            Ok(Ok(Some(_))) => {}
            _ => break,
        }
    }
}

const MAX_PENDING_PROJECT_OPEN_LINES: usize = 64;
const PROJECT_OWNER_HALF_CLOSE_GRACE: Duration = Duration::from_millis(750);

/// Bounds the handshake's profile-identity binding.
///
/// Deliberately *not* `PROJECT_OPEN_REQUEST_DEADLINE`. That 500 ms bound
/// answers "has this route's already-admitted open published yet", and the
/// route keeps warming behind the refusal. The binding stage is a different
/// question — it performs the profile's one cold
/// `DaemonSessionRuntimeRegistryV1::open`, schema convergence included — and
/// measurement says 500 ms is inside that open's normal range, not past it: on
/// this workspace's daemon suite a cold profile open measured 4 ms warm, 331 ms
/// uncontended and over 500 ms with six connections opening their own profiles
/// at once. Refusing at 500 ms therefore turns every contended first request
/// into a retry, which is what `daemon::tests::handshake` and
/// `daemon::tests::socket` observed when this stage was first bounded there.
///
/// This bound exists only so a stuck open can never pin a connection, its
/// lifecycle activity permit and its admission slot for the life of the daemon.
const PROFILE_IDENTITY_BIND_DEADLINE: Duration = Duration::from_secs(10);

struct DaemonWorkDeliveryDescriptorV1 {
    owner_event_id: String,
    channel_ref: String,
    valid_at: tracedecay_domain::UtcMicros,
    event_class: tracedecay_domain::DeliveryEventClassV1,
    kind: DaemonWorkDeliveryKindV1,
    attempt_identity: Option<tracedecay_domain::WorkAttemptIdentityV1>,
}

#[derive(Clone, Copy)]
enum DaemonWorkDeliveryKindV1 {
    Attempt,
    ArtifactPage,
}

impl DaemonWorkDeliveryDescriptorV1 {
    fn from_request(
        request: &DaemonInvocationRequest,
        handshake: &DaemonHandshake,
    ) -> Option<Self> {
        let DaemonInvocationPayload::WorkApplication {
            request: work_request,
            observed_at,
            ..
        } = &request.payload
        else {
            return None;
        };
        let observed_at = *observed_at;
        let (operation, event_class, kind, attempt_identity) = match work_request.as_ref() {
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::StartAttempt(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::AttemptStatus(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::CancelAttempt(command) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            tracedecay_daemon_protocol::WorkApplicationInvocationV1::HydrateArtifacts(_) => (
                work_request.operation_key(),
                tracedecay_domain::DeliveryEventClassV1::Activity,
                DaemonWorkDeliveryKindV1::ArtifactPage,
                None,
            ),
            _ => return None,
        };
        let project = handshake.project_path.as_ref()?.to_string_lossy();
        let owner = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon-work-delivery.v1",
            project.as_ref(),
            request.request_id.as_str(),
            operation,
            observed_at,
        ))
        .ok()?;
        let channel = tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon-work-channel.v1",
            project.as_ref(),
            handshake.client_instance_id.as_str(),
        ))
        .ok()?;
        Some(Self {
            owner_event_id: format!(
                "work:daemon-response:{}",
                owner.as_str().trim_start_matches("sha256:")
            ),
            channel_ref: format!(
                "cli:daemon:{}",
                channel.as_str().trim_start_matches("sha256:")
            ),
            valid_at: observed_at,
            event_class,
            kind,
            attempt_identity,
        })
    }

    fn is_successful_delivery(&self, response: &DaemonInvocationResponse) -> bool {
        use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

        match (&self.kind, &response.outcome) {
            (
                DaemonWorkDeliveryKindV1::Attempt,
                DaemonInvocationOutcome::WorkApplication {
                    outcome:
                        WorkApplicationOutcomeV1::StartAttempt(outcome)
                        | WorkApplicationOutcomeV1::AttemptStatus(outcome)
                        | WorkApplicationOutcomeV1::CancelAttempt(outcome),
                    ..
                },
            ) => application_outcome_payload(outcome).is_some(),
            (
                DaemonWorkDeliveryKindV1::ArtifactPage,
                DaemonInvocationOutcome::WorkApplication {
                    outcome: WorkApplicationOutcomeV1::HydrateArtifacts(outcome),
                    ..
                },
            ) => application_outcome_payload(outcome).is_some_and(|hydration| {
                matches!(
                    hydration,
                    tracedecay_application::WorkArtifactHydrationV1::Hydrated { attempts, .. }
                        if !attempts.is_empty()
                )
            }),
            _ => false,
        }
    }

    #[hotpath::skip]
    async fn attempts(
        self,
        service: &DaemonInvocationService,
        project_root: Option<&Path>,
        response: &DaemonInvocationResponse,
    ) -> Vec<tracedecay_domain::DeliverySettlementAttemptV1> {
        let identities = self.attempt_identities(response);
        if identities.is_empty() {
            return vec![self.settlement_attempt(self.owner_event_id.clone(), None)];
        }
        let mut attempts = Vec::with_capacity(identities.len());
        for identity in identities {
            let binding = service.work_fan_out_binding(project_root, &identity).await;
            let Ok(owner) = tracedecay_domain::canonical_sha256(&(
                "tracedecay.daemon-work-fan-out-delivery.v1",
                self.owner_event_id.as_str(),
                &identity,
                binding.as_ref(),
            )) else {
                continue;
            };
            attempts.push(self.settlement_attempt(
                format!(
                    "work:fan-out-response:{}",
                    owner.as_str().trim_start_matches("sha256:")
                ),
                Some(identity),
            ));
        }
        attempts
    }

    fn settlement_attempt(
        &self,
        owner_event_id: String,
        work_attempt: Option<tracedecay_domain::WorkAttemptIdentityV1>,
    ) -> tracedecay_domain::DeliverySettlementAttemptV1 {
        let attempted_at =
            std::cmp::max(self.valid_at, tracedecay_application::clock::now_micros());
        tracedecay_domain::DeliverySettlementAttemptV1 {
            owner_event_id,
            event_class: self.event_class,
            channel: tracedecay_domain::DeliveryChannelIdentityV1 {
                surface: tracedecay_domain::DeliverySurfaceFamilyV1::Cli,
                channel_ref: self.channel_ref.clone(),
            },
            work_attempt,
            eligible: 1,
            valid_at: self.valid_at,
            attempted_at,
        }
    }

    fn attempt_identities(
        &self,
        response: &DaemonInvocationResponse,
    ) -> Vec<tracedecay_domain::WorkAttemptIdentityV1> {
        use tracedecay_daemon_protocol::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

        if let Some(identity) = self.attempt_identity.as_ref() {
            return vec![identity.clone()];
        }
        let DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::HydrateArtifacts(outcome),
            ..
        } = &response.outcome
        else {
            return Vec::new();
        };
        let Some(tracedecay_application::WorkArtifactHydrationV1::Hydrated { attempts, .. }) =
            application_outcome_payload(outcome)
        else {
            return Vec::new();
        };
        attempts
            .iter()
            .map(|attempt| attempt.identity.clone())
            .collect()
    }
}

fn application_outcome_payload<T>(
    outcome: &tracedecay_application::ApplicationOutcome<T>,
) -> Option<&T> {
    match outcome {
        tracedecay_application::ApplicationOutcome::Evidence(result) => result.payload.as_ref(),
        tracedecay_application::ApplicationOutcome::Preview(result) => result.payload.as_ref(),
        tracedecay_application::ApplicationOutcome::Effect(result) => result.payload.as_ref(),
    }
}

fn offer_daemon_work_delivery(
    recorder: Option<&Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
    attempt: Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
    drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
) -> std::result::Result<(), tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason> {
    let (Some(recorder), Some(attempt)) = (recorder, attempt) else {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    };
    let settlement = tracedecay_domain::DeliverySettlementV1 {
        settled_at: std::cmp::max(
            attempt.attempted_at,
            tracedecay_application::clock::now_micros(),
        ),
        attempt,
        outcome,
        drop_reason,
    };
    match recorder.try_record(settlement) {
        Ok(tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {
            Ok(())
        }
        Ok(tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity) => {
            tracing::warn!("daemon Work delivery receipt was dropped at recorder capacity");
            Err(
                tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderAtCapacity,
            )
        }
        Err(error) => {
            tracing::warn!(%error, "daemon Work delivery receipt was refused");
            Err(
                tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
            )
        }
    }
}

/// Settle the exact attempts resolved immediately before the daemon response
/// was written.  In particular, do not resolve fan-out bindings again when a
/// client ACK arrives: the Work response and its receipt must share one
/// immutable identity even if the workflow owner changes in the meantime.
/// `attempted_at` is response-write-adjacent; `settled_at` is stamped when
/// this terminal ACK is observed by the daemon.
fn settle_daemon_work_delivery(
    attempts: Option<&[tracedecay_domain::DeliverySettlementAttemptV1]>,
    recorder: Option<&Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>>,
    outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
    drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
) -> std::result::Result<(), tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason> {
    let Some(attempts) = attempts else {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    };
    if attempts.is_empty() {
        return Err(
            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    }
    let mut result = Ok(());
    for attempt in attempts {
        if let Err(error) =
            offer_daemon_work_delivery(recorder, Some(attempt.clone()), outcome, drop_reason)
        {
            result = Err(error);
        }
    }
    result
}

async fn write_daemon_delivery_ack_response(
    transport: &mut impl McpTransport,
    response: &tracedecay_daemon_protocol::DaemonInvocationDeliveryAckResponse,
) -> Result<()> {
    let payload = hotpath::measure_block!(
        "daemon.engine.transport.serialize",
        serde_json::to_string(response)
    )?;
    transport.write_line(&payload).await?;
    transport.write_line("\n").await?;
    transport.flush().await?;
    Ok(())
}

enum DaemonDeliveryAckWait {
    Line(Option<String>),
    Deadline,
    Cancelled,
    Draining,
}

fn classify_daemon_delivery_ack_wait(
    wait: DaemonDeliveryAckWait,
) -> std::result::Result<Option<String>, tracedecay_domain::DeliveryDropReasonV1> {
    match wait {
        DaemonDeliveryAckWait::Line(line) => Ok(line),
        DaemonDeliveryAckWait::Deadline => Err(tracedecay_domain::DeliveryDropReasonV1::Deadline),
        DaemonDeliveryAckWait::Cancelled => Err(tracedecay_domain::DeliveryDropReasonV1::Cancelled),
        DaemonDeliveryAckWait::Draining => {
            Err(tracedecay_domain::DeliveryDropReasonV1::Disconnected)
        }
    }
}

async fn await_daemon_delivery_ack<F>(
    transport: &mut (impl McpTransport + Send),
    timeout: Duration,
    cancellation: Option<tracedecay_runtime_core::cancellation::CancellationToken>,
    draining: F,
) -> Result<DaemonDeliveryAckWait>
where
    F: std::future::Future<Output = ()>,
{
    let cancellation_wait = async move {
        if let Some(cancellation) = cancellation {
            cancellation.cancelled().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(cancellation_wait);
    tokio::pin!(draining);
    tokio::select! {
        result = read_line_handling_wire_oversized(transport) =>
            result.map(DaemonDeliveryAckWait::Line),
        () = &mut draining => Ok(DaemonDeliveryAckWait::Draining),
        () = tokio::time::sleep(timeout) => Ok(DaemonDeliveryAckWait::Deadline),
        () = &mut cancellation_wait => Ok(DaemonDeliveryAckWait::Cancelled),
    }
}

#[hotpath::measure(label = "daemon.engine.transport.await_owner", future = true)]
pub(super) async fn await_project_owner_or_disconnect<T: Send>(
    transport: &mut (impl McpTransport + Send),
    open: impl std::future::Future<Output = Result<T>> + Send,
) -> Result<Option<(T, VecDeque<String>)>> {
    await_project_owner_or_disconnect_inner(transport, open).await
}

fn await_project_owner_or_disconnect_inner<'a, T, O>(
    transport: &'a mut (impl McpTransport + Send),
    open: O,
) -> ProjectOwnerAwaitFutureV1<'a, T>
where
    T: Send,
    O: std::future::Future<Output = Result<T>> + Send + 'a,
{
    // Erase the deeply nested project-owner await future before it reaches
    // the measured wrapper so every profiling feature can compute its
    // layout.
    Box::pin(async move {
        tokio::pin!(open);
        let mut pending_lines = VecDeque::new();
        loop {
            // This loop continues after the read branch, so unlike the one-shot
            // selects below it drops an in-flight read every time `open` wins the
            // race — and the same transport is then handed to the routed server.
            // That is only safe because the transport's read half keeps its
            // partial-frame accumulator (`tracedecay_framing::BoundedLineReader`), so a
            // dropped read resumes mid-frame instead of losing the bytes it already
            // consumed and desynchronizing JSON-RPC framing for the connection.
            tokio::select! {
                result = &mut open => return result.map(|owner| Some((owner, pending_lines))),
                incoming = transport.read_line() => {
                    let Some(line) = incoming? else {
                        // EOF closes only the client's request half. It may still
                        // be reading the response, as one-shot CLI clients do.
                        // Give a bounded owner lookup enough time to produce its
                        // warming response, but do not retain a connection permit
                        // indefinitely when the peer fully disappeared.
                        let peer_full_close = transport.peer_fully_closed_after_eof();
                        tokio::pin!(peer_full_close);
                        return tokio::select! {
                            result = &mut open =>
                                result.map(|owner| Some((owner, pending_lines))),
                            () = &mut peer_full_close => Ok(None),
                            () = tokio::time::sleep(PROJECT_OWNER_HALF_CLOSE_GRACE) =>
                                Err(TraceDecayError::Config {
                                    message: format!(
                                        "TraceDecay project owner {PROJECT_WARMING_RETRY_HINT}"
                                    ),
                                }),
                        };
                    };
                    if pending_lines.len() >= MAX_PENDING_PROJECT_OPEN_LINES {
                        return Err(TraceDecayError::Config {
                            message: "daemon client pipelined too many requests while the project owner was opening"
                                .to_owned(),
                        });
                    }
                    pending_lines.push_back(line);
                }
            }
        }
    })
}

#[cfg(unix)]
#[hotpath::measure(label = "daemon.engine.transport.broker", future = true)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    serve_broker_socket_client_inner(stream, engine, auth_token, admission_class).await
}

#[cfg(unix)]
fn serve_broker_socket_client_inner(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>> {
    // Erase the deeply nested broker connection future before it reaches the
    // measured wrapper so every profiling feature can compute its layout.
    Box::pin(async move {
        let Some((
            mut transport,
            engine,
            mut handshake,
            first_request,
            setup_activity,
            _per_client_permit,
        )) = boxed_broker_connection_phase(async move {
            let mut transport = BrokerStreamTransport::new(stream);
        if let Some(expected_token) = auth_token.as_deref() {
            let preface_line = tokio::select! {
                result = read_line_handling_wire_oversized(&mut transport) => result?,
                () = engine.lifecycle.wait_for_draining() => return Ok(None),
            };
            let Some(preface_line) = preface_line else {
                return Ok(None);
            };
            let authenticated = DaemonAuthPreface::from_line(&preface_line)
                .is_ok_and(|preface| preface.authenticate(expected_token));
            if !authenticated {
                refuse_unauthenticated_client(&mut transport, binary_version()?).await;
                return Ok(None);
            }
        }
        let line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(None),
        };
        let Some(line) = line else {
            return Ok(None);
        };
        let Some(setup_activity) = engine.lifecycle.try_enter() else {
            return Ok(None);
        };
        let mut handshake = match DaemonHandshake::from_line(&line) {
            Ok(handshake) => handshake,
            Err(_) => {
                drop(setup_activity);
                refuse_unparseable_handshake(&mut transport, &line, binary_version()?).await;
                return Ok(None);
            }
        };
        let first_request_line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(None),
        };
        let Some(first_request_line) = first_request_line else {
            return Ok(None);
        };
        let first_request = AuthenticatedFirstRequest::new(first_request_line);
        // Ordered after the first request, exactly as the portable broker does,
        // so a binding that misses its deadline is answered as a typed retry on
        // that request's id instead of closing the socket with no evidence.
        let peer_full_close = transport.peer_fully_closed_after_eof();
        tokio::pin!(peer_full_close);
        let store_administration = tokio::select! {
            result = bind_authenticated_profile_identity_within_deadline(
                &mut handshake,
                &engine.store_administration,
            ) => match result {
                Ok(store_administration) => store_administration,
                Err(error) if error_message_is_project_warming(&error.to_string()) => {
                    drop(setup_activity);
                    refuse_warming_profile_identity(&mut transport, &first_request, &error).await?;
                    return Ok(None);
                }
                Err(error) => return Err(error),
            },
            () = &mut peer_full_close => return Ok(None),
        };
        let mut engine = engine;
        engine.store_administration = store_administration;
        let reserved_control_request = is_reserved_control_request(&first_request);
        if admission_class == DaemonClientAdmissionClass::ReservedControl
            && !reserved_control_request
        {
            drop(setup_activity);
            reject_reserved_bulk_request(
                &mut transport,
                &first_request,
                MAX_CONCURRENT_DAEMON_CLIENTS,
            )
            .await?;
            return Ok(None);
        }
        let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
            match engine
                .per_client_admission
                .try_admit_request(&handshake, &first_request)
            {
                Ok(permit) => Some(permit),
                Err(response) => {
                    drop(setup_activity);
                    reject_admitted_request(&mut transport, &first_request, response).await?;
                    return Ok(None);
                }
            }
        } else {
            None
        };
            Ok::<_, TraceDecayError>(Some((
                transport,
                engine,
                handshake,
                first_request,
                setup_activity,
                _per_client_permit,
            )))
        })
        .await?
        else {
            return Ok(());
        };

        boxed_broker_connection_phase(async move {
        let Some((
            mut transport,
            engine,
            handshake,
            first_request,
            setup_activity,
            _per_client_permit,
            initialize_route,
        )) = boxed_broker_connection_phase(async move {
        if let Some(cancellation) =
            tracedecay_daemon_protocol::parse_daemon_invocation_cancellation_request(
                first_request.raw(),
            )
        {
            hotpath::measure_block!("daemon.engine.transport.cancel", {
cancel(cancellation.target_request_id());
            });
            drop(setup_activity);
            return Ok(None);
        }
        let git_watcher_health = if doctor_runtime_request(first_request.parsed()).is_some() {
            Some(
                Box::pin(engine.git_watcher_health(handshake.project_path.as_deref())).await,
            )
        } else {
            None
        };
        let Some(setup_activity) = Box::pin(serve_core_doctor_runtime_request(
            &mut transport,
            &handshake,
            &engine.store_administration,
            setup_activity,
            &first_request,
            git_watcher_health,
            || {
                Box::pin(async {
                    Ok(engine
                        .cached_project_server(&handshake)
                        .await?
                        .is_some_and(|server| server.doctor_report_ready()))
                })
            },
        ))
        .await?
        else {
            return Ok(None);
        };
        Box::pin(engine.log_client_version_skew(&handshake)).await?;
        report_profile_host_admission_bootstrap_status(
            Box::pin(schedule_user_profile_host_admission_replay_for_identity(
                &engine.store_administration,
                &handshake.client_identity,
            ))
            .await,
        );
        // Resolve initialize roots only after authentication and inside daemon
        // authority. The proxy process never opens the registry database.
        // Resolution failures (deferred repository discovery, registry refusals)
        // are answered as typed responses: dropping the connection here would
        // surface as a hard host failure and leave the client without the
        // retryable state the deferral carries.
        let initialize_route = match Box::pin(apply_daemon_initialize_route(
            &mut handshake,
            &first_request,
            &engine.store_administration,
        ))
        .await
        {
            Ok(route) => route,
            Err(error) => {
                drop(setup_activity);
                Box::pin(write_project_open_error(
                    &mut transport,
                    &first_request,
                    &handshake.client_instance_id,
                    &error,
                ))
                .await?;
                return Ok(None);
            }
        };
            Ok::<_, TraceDecayError>(Some((
                transport,
                engine,
                handshake,
                first_request,
                setup_activity,
                _per_client_permit,
                initialize_route,
            )))
        })
        .await?
        else {
            return Ok(());
        };

        boxed_broker_connection_phase(async move {
        if let Some(request) = parse_branch_admin_request(first_request.parsed()) {
            return boxed_broker_connection_phase(async move {
            let result = match request.action.clone() {
                Ok(action) => engine.execute_branch_admin(&handshake, action).await,
                Err(message) => Err(TraceDecayError::Config { message }),
            };
            drop(setup_activity);
            write_branch_admin_response(&mut transport, request, result).await?;
            Ok(())
            })
            .await;
        }
        if let Some(request) = parse_branch_add_request(first_request.parsed()) {
            return boxed_broker_connection_phase(async move {
            let response = match await_project_owner_or_disconnect(
                &mut transport,
                engine.project_server_for_request(&handshake, ProjectServerRequirement::Core),
            )
            .await
            {
                Ok(Some(_)) => {
                    branch_add_response(
                        &engine.store_administration,
                        Some(&engine.invocation.code_index_schedulers),
                        &handshake,
                        &request,
                    )
                    .await
                }
                Ok(None) => return Ok(()),
                Err(error) => JsonRpcResponse::error(
                    request.id.clone(),
                    ErrorCode::InternalError,
                    error.to_string(),
                ),
            };
            drop(setup_activity);
            write_json_rpc_response(&mut transport, &response).await?;
            Ok(())
            })
            .await;
        }
        if let Some(invocation) = parse_daemon_invocation_request(first_request.raw()) {
            return boxed_broker_connection_phase(async move {
            let mut invocation = invocation;
            let mut owned_lsp_sessions = HashMap::new();
            let mut pending_line = None;
            // Keep the retained invocation loop out of the broker connection
            // future's inline state. With Hotpath enabled the surrounding
            // transport wrapper is polled on Tokio's ordinary worker stack;
            // embedding this loop there makes construction alone exceed that
            // stack before the first request can be served.
            let result = boxed_broker_connection_phase(async {
                loop {
                    let delivery = invocation.as_ref().ok().and_then(|request| {
                        DaemonWorkDeliveryDescriptorV1::from_request(request, &handshake)
                    });
                    let request_id = invocation
                        .as_ref()
                        .ok()
                        .map(|request| request.request_id.clone());
                    let ack_deadline = invocation
                        .as_ref()
                        .ok()
                        .and_then(|request| request.delivery_ack_deadline())
                        .cloned();
                    let session_transition = invocation
                        .as_ref()
                        .ok()
                        .and_then(invocation_lsp_session_transition);
                    let response = match invocation {
                        Ok(request) => {
                            Box::pin(execute_daemon_invocation(&engine, &handshake, request)).await
                        }
                        Err(response) => response,
                    };
                    update_connection_lsp_sessions(
                        &mut owned_lsp_sessions,
                        session_transition.as_ref(),
                        &response,
                    );
                    let delivery =
                        delivery.filter(|delivery| delivery.is_successful_delivery(&response));
                    // Resolve fan-out bindings before the socket response crosses
                    // the wire. The same immutable attempts are used for a
                    // Delivered or Dropped ACK; no mutable Work lookup occurs at
                    // terminal-ACK time.
                    let delivery_attempts = if let Some(delivery) = delivery {
                        Some(
                            delivery
                                .attempts(
                                    &engine.invocation.service,
                                    handshake.project_path.as_deref(),
                                    &response,
                                )
                                .await,
                        )
                    } else {
                        None
                    };
                    let write_result =
                        write_daemon_invocation_response(&mut transport, &response).await;
                    if let Err(error) = write_result {
                        let recorder = engine
                            .invocation
                            .service
                            .delivery_settlement_recorder(handshake.project_path.as_deref())
                            .await;
                        let _ = settle_daemon_work_delivery(
                            delivery_attempts.as_deref(),
                            recorder.as_ref(),
                            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                            Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                        );
                        return Err(error);
                    }
                    if delivery_attempts.is_some() {
                        let recorder = engine
                            .invocation
                            .service
                            .delivery_settlement_recorder(handshake.project_path.as_deref())
                            .await;
                        let ack_timeout = ack_deadline
                            .as_ref()
                            .and_then(tracedecay_daemon_protocol::deadline_remaining);
                        let Some(ack_timeout) = ack_timeout else {
                            let _ = settle_daemon_work_delivery(
                                delivery_attempts.as_deref(),
                                recorder.as_ref(),
                                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                Some(tracedecay_domain::DeliveryDropReasonV1::Deadline),
                            );
                            return Ok(());
                        };
                        let delivery_cancellation = request_id
                            .as_deref()
                            .and_then(register);
                        let cancellation = delivery_cancellation
                            .as_ref()
                            .map(Lease::token);
                        let ack_line = match await_daemon_delivery_ack(
                            &mut transport,
                            ack_timeout,
                            cancellation,
                            engine.lifecycle.wait_for_draining(),
                        )
                        .await
                        {
                            Ok(wait) => match classify_daemon_delivery_ack_wait(wait) {
                                Ok(line) => line,
                                Err(reason) => {
                                    let _ = settle_daemon_work_delivery(
                                        delivery_attempts.as_deref(),
                                        recorder.as_ref(),
                                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                        Some(reason),
                                    );
                                    return Ok(());
                                }
                            },
                            Err(error) => {
                                let _ = settle_daemon_work_delivery(
                                    delivery_attempts.as_deref(),
                                    recorder.as_ref(),
                                    tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                    Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                                );
                                return Err(error);
                            }
                        };
                        match ack_line {
                            Some(line) => {
                                let ack = tracedecay_daemon_protocol::parse_daemon_invocation_delivery_ack_request(
                                    &line,
                                );
                                if let Some(ack) = ack.filter(|ack| {
                                    request_id
                                        .as_deref()
                                        .is_some_and(|request_id| ack.target_request_id() == request_id)
                                }) {
                                    let target_request_id = ack.target_request_id().to_owned();
                                    let (outcome, drop_reason) = ack.outcome();
                                    let settlement_result = settle_daemon_work_delivery(
                                        delivery_attempts.as_deref(),
                                        recorder.as_ref(),
                                        outcome,
                                        drop_reason,
                                    );
                                    let ack_response = match &settlement_result {
                                        Ok(()) => {
                                            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckResponse::accepted(
                                                target_request_id.clone(),
                                            )
                                        }
                                        Err(reason) => {
                                            tracedecay_daemon_protocol::DaemonInvocationDeliveryAckResponse::rejected(
                                                target_request_id.clone(),
                                                *reason,
                                            )
                                        }
                                    };
                                    write_daemon_delivery_ack_response(&mut transport, &ack_response)
                                        .await?;
                                    if let Err(reason) = settlement_result {
                                        return Err(TraceDecayError::Config {
                                            message: format!(
                                                "daemon could not durably record Work delivery ACK: {reason:?}"
                                            ),
                                        });
                                    }
                                } else {
                                    let _ = settle_daemon_work_delivery(
                                        delivery_attempts.as_deref(),
                                        recorder.as_ref(),
                                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                        Some(tracedecay_domain::DeliveryDropReasonV1::Invalid),
                                    );
                                    pending_line = Some(line);
                                }
                            }
                            None => {
                                let _ = settle_daemon_work_delivery(
                                    delivery_attempts.as_deref(),
                                    recorder.as_ref(),
                                    tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                    Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                                );
                                return Ok(())
                            }
                        }
                    }
                    let next_line = if let Some(line) = pending_line.take() {
                        Some(line)
                    } else {
                        tokio::select! {
                            result = read_line_handling_wire_oversized(&mut transport) => result?,
                            () = engine.lifecycle.wait_for_draining() => return Ok(()),
                        }
                    };
                    let Some(next_line) = next_line else {
                        return Ok(());
                    };
                    let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                        return Ok(());
                    };
                    invocation = next_invocation;
                }
            })
            .await;
            cleanup_connection_lsp_sessions(&engine.invocation, owned_lsp_sessions).await;
            result
            })
            .await;
        }
        let bootstrap_handled = boxed_broker_connection_phase(async {
        if let Some(request) = first_request.parsed() {
            let initialized_project_server_ready =
                matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                    && handshake.project_path.is_some()
                    && engine.cached_project_server(&handshake).await?.is_some();
            let project_node_count =
                if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                    if handshake.project_path.is_some() {
                        cached_project_node_count(&engine.store_administration, &handshake).await
                    } else {
                        Some(0)
                    }
                } else {
                    None
                };
            if !initialized_project_server_ready
                && let Some(mut response) = daemon_bootstrap_response(
                    request,
                    initialize_route.as_ref(),
                    project_node_count,
                )
            {
                let project_open_error = if handshake.project_path.is_some()
                    && matches!(
                        classify_mcp_method(&request.method),
                        McpMethod::Initialize | McpMethod::ToolsList
                    ) {
                    match engine.cached_project_open_failure(&handshake).await {
                        Ok(Some(failure)) => Some(failure.to_error()),
                        Ok(None)
                            if matches!(
                                classify_mcp_method(&request.method),
                                McpMethod::Initialize
                            ) =>
                        {
                            Box::pin(
                                engine.schedule_project_server_warmup(
                                    handshake.clone(),
                                    request.clone(),
                                ),
                            )
                            .await
                            .err()
                        }
                        Ok(None) => None,
                        Err(error) => Some(error),
                    }
                } else {
                    None
                };
                if let Some(error) = project_open_error {
                    response = request
                        .id
                        .clone()
                        .map(|id| project_open_error_response(id, &error));
                }
                // Keep catalog-refresh bookkeeping consistent with the regular MCP
                // server path. Only a warming `tools/list` (no published node count)
                // or an initialize answered while a project graph is still opening
                // is provisional. `project_node_count` is computed only for
                // `tools/list`, so treating every `None` as provisional also
                // skipped projectless initialize (must mark current) and
                // `notifications/initialized` (must emit a pending refresh).
                let catalog_is_provisional = match classify_mcp_method(&request.method) {
                    McpMethod::ToolsList => project_node_count.is_none(),
                    McpMethod::Initialize => handshake.project_path.is_some(),
                    _ => false,
                };
                if let Some(key) = engine
                    .claim_catalog_refresh(
                        &handshake,
                        first_request.parsed(),
                        catalog_is_provisional,
                    )
                    .await
                    && let Err(error) = write_tool_list_changed_notification(&mut transport).await
                {
                    engine.release_catalog_refresh(key).await;
                    return Err(error);
                }
                if let Some(response) = response {
                    write_json_rpc_response(&mut transport, &response).await?;
                }
                return Ok::<_, TraceDecayError>(true);
            }
        }
        Ok(false)
        })
        .await?;
        if bootstrap_handled {
            drop(setup_activity);
            return Ok(());
        }

        let user_session_request = projectless_user_session_request(first_request.parsed());
        let project_owner = boxed_broker_connection_phase(async {
        if handshake.project_path.is_some() && !user_session_request {
            match await_project_owner_or_disconnect(
                &mut transport,
                engine.project_server_for_request(
                    &handshake,
                    project_server_requirement(first_request.parsed()),
                ),
            )
            .await
            {
                Ok(Some((server, pending_lines))) => Ok(Some((Some(server), pending_lines))),
                Ok(None) => Ok(None),
                Err(error) => {
                    write_project_open_error(
                        &mut transport,
                        &first_request,
                        &handshake.client_instance_id,
                        &error,
                    )
                    .await?;
                    Ok(None)
                }
            }
        } else {
            Ok::<_, TraceDecayError>(Some((None, VecDeque::new())))
        }
        })
        .await?;
        drop(setup_activity);
        let Some((server, pending_project_open_lines)) = project_owner else {
            return Ok(());
        };
        if !engine.lifecycle.accepting() {
            return Ok(());
        }

        // The stdio proxy creates one daemon connection per request. The request
        // was peeked above so initialize-root routing happens before project open.
        if let Some(key) = engine
            .claim_catalog_refresh(&handshake, first_request.parsed(), false)
            .await
            && let Err(error) = write_tool_list_changed_notification(&mut transport).await
        {
            engine.release_catalog_refresh(key).await;
            return Err(error);
        }
        if let Some(server) = server {
            if is_mcp_initialize_request(first_request.parsed()) {
                #[cfg(test)]
                tests::record_mcp_route(
                    &handshake.client_instance_id,
                    tests::ObservedMcpRoute::Rmcp,
                );
                #[cfg(test)]
                tests::record_first_request_replay(
                    &handshake.client_instance_id,
                    first_request.raw(),
                );
                Box::pin(serve_routed_rmcp_connection(
                    server,
                    transport,
                    first_request.into_raw(),
                    pending_project_open_lines,
                    initialize_route,
                    handshake.timings,
                    &engine.lifecycle,
                ))
                .await?;
            } else {
                #[cfg(test)]
                tests::record_mcp_route(
                    &handshake.client_instance_id,
                    tests::ObservedMcpRoute::Legacy,
                );
                #[cfg(test)]
                tests::record_first_request_replay(
                    &handshake.client_instance_id,
                    first_request.raw(),
                );
                let mut transport = ReplayTransport::new(transport);
                transport.push_replay(first_request.into_raw())?;
                for line in pending_project_open_lines {
                    transport.push_replay(line)?;
                }
                Box::pin(server.run_daemon_connection_with_timings(
                    &mut transport,
                    handshake.timings,
                    &engine.lifecycle,
                ))
                .await?;
            }
        } else {
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request.into_raw())?;
            for line in pending_project_open_lines {
                transport.push_replay(line)?;
            }
            Box::pin(serve_projectless_client(
                &mut transport,
                &handshake.client_identity,
                &engine.lifecycle,
                &engine.store_administration,
            ))
            .await?;
        }
        Ok(())
        })
        .await
        })
        .await
    })
}

#[cfg(test)]
pub(super) async fn serve_windows_broker_client(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonPerClientAdmission::default(),
        DaemonClientAdmissionClass::General,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(test)]
// Cohesive per-connection serving context; bundling into a params struct would churn every caller.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_windows_broker_client_with_class(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    Box::pin(serve_windows_broker_client_with_class_and_invocation(
        stream,
        auth_token,
        lifecycle,
        store_administration,
        project_open_gates,
        DaemonInvocationState::default(),
        http_application::DaemonHttpApplicationRegistry::default(),
        per_client_admission,
        admission_class,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
}

#[cfg(any(not(unix), test))]
// The foreground portable broker supplies one daemon-generation invocation state.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.engine.transport.dispatch", future = true)]
pub(super) async fn serve_windows_broker_client_with_class_and_invocation(
    stream: BrokerStream,
    auth_token: &str,
    lifecycle: &DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    per_client_admission: DaemonPerClientAdmission,
    admission_class: DaemonClientAdmissionClass,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    let Some(preface_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let authenticated = DaemonAuthPreface::from_line(&preface_line)
        .is_ok_and(|preface| preface.authenticate(auth_token));
    if !authenticated {
        refuse_unauthenticated_client(&mut transport, binary_version()?).await;
        return Ok(());
    }
    let Some(handshake_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = match DaemonHandshake::from_line(&handshake_line) {
        Ok(handshake) => handshake,
        Err(_) => {
            drop(setup_activity);
            refuse_unparseable_handshake(&mut transport, &handshake_line, binary_version()?).await;
            return Ok(());
        }
    };
    let Some(first_request_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let first_request = AuthenticatedFirstRequest::new(first_request_line);
    if let Some(response) = daemon_shutdown_response(&first_request) {
        lifecycle.begin_draining();
        write_json_rpc_response(&mut transport, &response).await?;
        drop(setup_activity);
        return Ok(());
    }
    let peer_full_close = transport.peer_fully_closed_after_eof();
    tokio::pin!(peer_full_close);
    let store_administration = tokio::select! {
        result = Box::pin(bind_authenticated_profile_identity_within_deadline(
            &mut handshake,
            &store_administration,
        )) => match result {
            Ok(store_administration) => store_administration,
            Err(error) if error_message_is_project_warming(&error.to_string()) => {
                drop(setup_activity);
                refuse_warming_profile_identity(&mut transport, &first_request, &error).await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        },
        () = &mut peer_full_close => return Ok(()),
    };
    let reserved_control_request = is_reserved_control_request(&first_request);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match per_client_admission.try_admit_request(&handshake, &first_request) {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(cancellation) =
        tracedecay_daemon_protocol::parse_daemon_invocation_cancellation_request(
            first_request.raw(),
        )
    {
        hotpath::measure_block!("daemon.engine.transport.cancel", {
            cancel(cancellation.target_request_id());
        });
        drop(setup_activity);
        return Ok(());
    }
    let Some(setup_activity) = Box::pin(serve_core_doctor_runtime_request(
        &mut transport,
        &handshake,
        &store_administration,
        setup_activity,
        &first_request,
        None,
        || async {
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            Ok(Box::pin(portable_cached_project_server(
                &store_administration,
                &canonical_project_path,
                &handshake,
                ProjectServerRequirement::Core,
            ))
            .await?
            .is_some_and(|server| server.doctor_report_ready()))
        },
    ))
    .await?
    else {
        return Ok(());
    };
    report_profile_host_admission_bootstrap_status(
        Box::pin(schedule_user_profile_host_admission_replay_for_identity(
            &store_administration,
            &handshake.client_identity,
        ))
        .await,
    );
    // Same contract as the Unix broker path: a route-resolution failure is a
    // typed response, never a dropped connection.
    let initialize_route = match Box::pin(apply_daemon_initialize_route(
        &mut handshake,
        &first_request,
        &store_administration,
    ))
    .await
    {
        Ok(route) => route,
        Err(error) => {
            drop(setup_activity);
            write_project_open_error(
                &mut transport,
                &first_request,
                &handshake.client_instance_id,
                &error,
            )
            .await?;
            return Ok(());
        }
    };
    if let Some(request) = parse_branch_admin_request(first_request.parsed()) {
        let result = match request.action.clone() {
            Ok(action) => {
                Box::pin(store_administration.execute_branch_admin_for_handshake(
                    &invocation.code_index_schedulers,
                    &handshake,
                    action,
                ))
                .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(first_request.parsed()) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            Box::pin(portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry.clone(),
                &handshake,
                ProjectServerRequirement::Core,
                #[cfg(test)]
                project_open_attempts.clone(),
            )),
        )
        .await
        {
            Ok(Some(_)) => {
                Box::pin(branch_add_response(
                    &store_administration,
                    Some(&invocation.code_index_schedulers),
                    &handshake,
                    &request,
                ))
                .await
            }
            Ok(None) => return Ok(()),
            Err(error) => JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InternalError,
                error.to_string(),
            ),
        };
        drop(setup_activity);
        write_json_rpc_response(&mut transport, &response).await?;
        return Ok(());
    }
    if let Some(invocation_request) = parse_daemon_invocation_request(first_request.raw()) {
        let mut invocation_request = invocation_request;
        let mut owned_lsp_sessions = HashMap::new();
        let mut pending_line = None;
        let result = async {
            loop {
                let delivery = invocation_request.as_ref().ok().and_then(|request| {
                    DaemonWorkDeliveryDescriptorV1::from_request(request, &handshake)
                });
                let request_id = invocation_request
                    .as_ref()
                    .ok()
                    .map(|request| request.request_id.clone());
                let ack_deadline = invocation_request
                    .as_ref()
                    .ok()
                    .and_then(|request| request.delivery_ack_deadline())
                    .cloned();
                let session_transition = invocation_request
                    .as_ref()
                    .ok()
                    .and_then(invocation_lsp_session_transition);
                let response = match invocation_request {
                    Ok(request) => {
                        Box::pin(execute_portable_daemon_invocation(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            &handshake,
                            &invocation,
                            http_application_registry.clone(),
                            request,
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        ))
                        .await
                    }
                    Err(response) => response,
                };
                update_connection_lsp_sessions(
                    &mut owned_lsp_sessions,
                    session_transition.as_ref(),
                    &response,
                );
                let delivery =
                    delivery.filter(|delivery| delivery.is_successful_delivery(&response));
                // Resolve fan-out bindings before the socket response crosses
                // the wire. The same immutable attempts are used for a
                // Delivered or Dropped ACK; no mutable Work lookup occurs at
                // terminal-ACK time.
                let delivery_attempts = if let Some(delivery) = delivery {
                    Some(
                        delivery
                            .attempts(
                                &invocation.service,
                                handshake.project_path.as_deref(),
                                &response,
                            )
                            .await,
                    )
                } else {
                    None
                };
                let write_result =
                    write_daemon_invocation_response(&mut transport, &response).await;
                if let Err(error) = write_result {
                    let recorder = invocation
                        .service
                        .delivery_settlement_recorder(handshake.project_path.as_deref())
                        .await;
                    let _ = settle_daemon_work_delivery(
                        delivery_attempts.as_deref(),
                        recorder.as_ref(),
                        tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                        Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                    );
                    return Err(error);
                }
                if delivery_attempts.is_some() {
                    let recorder = invocation
                        .service
                        .delivery_settlement_recorder(handshake.project_path.as_deref())
                        .await;
                    let ack_timeout = ack_deadline
                        .as_ref()
                        .and_then(tracedecay_daemon_protocol::deadline_remaining);
                    let Some(ack_timeout) = ack_timeout else {
                        let _ = settle_daemon_work_delivery(
                            delivery_attempts.as_deref(),
                            recorder.as_ref(),
                            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                            Some(tracedecay_domain::DeliveryDropReasonV1::Deadline),
                        );
                        return Ok(());
                    };
                    let delivery_cancellation = request_id
                        .as_deref()
                        .and_then(register);
                    let cancellation = delivery_cancellation
                        .as_ref()
                        .map(Lease::token);
                    let ack_line = match await_daemon_delivery_ack(
                        &mut transport,
                        ack_timeout,
                        cancellation,
                        lifecycle.wait_for_draining(),
                    )
                    .await
                    {
                        Ok(wait) => match classify_daemon_delivery_ack_wait(wait) {
                            Ok(line) => line,
                            Err(reason) => {
                                let _ = settle_daemon_work_delivery(
                                    delivery_attempts.as_deref(),
                                    recorder.as_ref(),
                                    tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                    Some(reason),
                                );
                                return Ok(());
                            }
                        },
                        Err(error) => {
                            let _ = settle_daemon_work_delivery(
                                delivery_attempts.as_deref(),
                                recorder.as_ref(),
                                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                            );
                            return Err(error);
                        }
                    };
                    match ack_line {
                        Some(line) => {
                            let ack = tracedecay_daemon_protocol::parse_daemon_invocation_delivery_ack_request(
                                &line,
                            );
                            if let Some(ack) = ack.filter(|ack| {
                                request_id
                                    .as_deref()
                                    .is_some_and(|request_id| ack.target_request_id() == request_id)
                            }) {
                                let target_request_id = ack.target_request_id().to_owned();
                                let (outcome, drop_reason) = ack.outcome();
                                let settlement_result = settle_daemon_work_delivery(
                                    delivery_attempts.as_deref(),
                                    recorder.as_ref(),
                                    outcome,
                                    drop_reason,
                                );
                                let ack_response = match &settlement_result {
                                    Ok(()) => {
                                        tracedecay_daemon_protocol::DaemonInvocationDeliveryAckResponse::accepted(
                                            target_request_id.clone(),
                                        )
                                    }
                                    Err(reason) => {
                                        tracedecay_daemon_protocol::DaemonInvocationDeliveryAckResponse::rejected(
                                            target_request_id.clone(),
                                            *reason,
                                        )
                                    }
                                };
                                write_daemon_delivery_ack_response(&mut transport, &ack_response)
                                    .await?;
                                if let Err(reason) = settlement_result {
                                    return Err(TraceDecayError::Config {
                                        message: format!(
                                            "daemon could not durably record Work delivery ACK: {reason:?}"
                                        ),
                                    });
                                }
                            } else {
                                let _ = settle_daemon_work_delivery(
                                    delivery_attempts.as_deref(),
                                    recorder.as_ref(),
                                    tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                    Some(tracedecay_domain::DeliveryDropReasonV1::Invalid),
                                );
                                pending_line = Some(line);
                            }
                        }
                        None => {
                            let _ = settle_daemon_work_delivery(
                                delivery_attempts.as_deref(),
                                recorder.as_ref(),
                                tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                                Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
                            );
                            return Ok(())
                        }
                    }
                }
                let next_line = if let Some(line) = pending_line.take() {
                    Some(line)
                } else {
                    tokio::select! {
                        result = read_line_handling_wire_oversized(&mut transport) => result?,
                        () = lifecycle.wait_for_draining() => return Ok(()),
                    }
                };
                let Some(next_line) = next_line else {
                    return Ok(());
                };
                let Some(next_invocation) = parse_daemon_invocation_request(&next_line) else {
                    return Ok(());
                };
                invocation_request = next_invocation;
            }
        }
        .await;
        cleanup_connection_lsp_sessions(&invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Some(request) = first_request.parsed() {
        let initialized_project_server_ready =
            if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
            {
                let (project_path, _) = project_route_for_handshake(&handshake)?;
                Box::pin(portable_cached_project_server(
                    &store_administration,
                    &project_path,
                    &handshake,
                    ProjectServerRequirement::Core,
                ))
                .await?
                .is_some()
            } else {
                false
            };
        let project_node_count =
            if matches!(classify_mcp_method(&request.method), McpMethod::ToolsList) {
                if handshake.project_path.is_some() {
                    cached_project_node_count(&store_administration, &handshake).await
                } else {
                    Some(0)
                }
            } else {
                None
            };
        if !initialized_project_server_ready
            && let Some(mut response) =
                daemon_bootstrap_response(request, initialize_route.as_ref(), project_node_count)
        {
            let project_open_error = if handshake.project_path.is_some()
                && matches!(
                    classify_mcp_method(&request.method),
                    McpMethod::Initialize | McpMethod::ToolsList
                ) {
                match portable_cached_project_open_failure(project_open_gates.as_ref(), &handshake)
                    .await
                {
                    Ok(Some(failure)) => Some(failure.to_error()),
                    Ok(None)
                        if matches!(
                            classify_mcp_method(&request.method),
                            McpMethod::Initialize
                        ) =>
                    {
                        Box::pin(schedule_portable_project_server_warmup(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            invocation.clone(),
                            http_application_registry.clone(),
                            handshake.clone(),
                            request.clone(),
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        ))
                        .await
                        .err()
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            if let Some(error) = project_open_error {
                response = request
                    .id
                    .clone()
                    .map(|id| project_open_error_response(id, &error));
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(first_request.parsed());
    if handshake.project_path.is_some() && !user_session_request {
        // Heap-allocate the owner-await composition: embedded by value it
        // dominates this serve future's resident frame and overflows the
        // worker stack in perf-profile layouts.
        let server = match Box::pin(await_project_owner_or_disconnect(
            &mut transport,
            Box::pin(portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry,
                &handshake,
                project_server_requirement(first_request.parsed()),
                #[cfg(test)]
                project_open_attempts.clone(),
            )),
        ))
        .await
        {
            Ok(Some(server)) => server,
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(
                    &mut transport,
                    &first_request,
                    &handshake.client_instance_id,
                    &error,
                )
                .await?;
                return Ok(());
            }
        };
        drop(setup_activity);
        let (server, pending_lines) = server;
        if is_mcp_initialize_request(first_request.parsed()) {
            #[cfg(test)]
            tests::record_mcp_route(&handshake.client_instance_id, tests::ObservedMcpRoute::Rmcp);
            #[cfg(test)]
            tests::record_first_request_replay(&handshake.client_instance_id, first_request.raw());
            Box::pin(serve_routed_rmcp_connection(
                server,
                transport,
                first_request.into_raw(),
                pending_lines,
                initialize_route,
                handshake.timings,
                lifecycle,
            ))
            .await?;
        } else {
            #[cfg(test)]
            tests::record_mcp_route(
                &handshake.client_instance_id,
                tests::ObservedMcpRoute::Legacy,
            );
            #[cfg(test)]
            tests::record_first_request_replay(&handshake.client_instance_id, first_request.raw());
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request.into_raw())?;
            for line in pending_lines {
                transport.push_replay(line)?;
            }
            Box::pin(server.run_daemon_connection_with_timings(
                &mut transport,
                handshake.timings,
                lifecycle,
            ))
            .await?;
        }
    } else {
        drop(setup_activity);
        let mut transport = ReplayTransport::new(transport);
        transport.push_replay(first_request.into_raw())?;
        Box::pin(serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            lifecycle,
            &store_administration,
        ))
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod delivery_ack_tests {
    use super::{
        DaemonDeliveryAckWait, await_daemon_delivery_ack, classify_daemon_delivery_ack_wait,
    };
    use std::time::Duration;
    use tracedecay_mcp::transport::ChannelTransport;

    #[tokio::test(start_paused = true)]
    async fn delivery_ack_wait_uses_the_exact_deadline_budget() {
        let (mut transport, _input, _output) = ChannelTransport::new();
        let wait = await_daemon_delivery_ack(
            &mut transport,
            Duration::from_secs(3),
            None,
            std::future::pending::<()>(),
        );
        tokio::pin!(wait);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;

        assert!(matches!(wait.await, Ok(DaemonDeliveryAckWait::Deadline)));
    }

    #[test]
    fn withheld_ack_terminalizes_as_deadline_drop() {
        assert_eq!(
            classify_daemon_delivery_ack_wait(DaemonDeliveryAckWait::Deadline),
            Err(tracedecay_domain::DeliveryDropReasonV1::Deadline)
        );
        assert_eq!(
            classify_daemon_delivery_ack_wait(DaemonDeliveryAckWait::Cancelled),
            Err(tracedecay_domain::DeliveryDropReasonV1::Cancelled)
        );
    }
}
