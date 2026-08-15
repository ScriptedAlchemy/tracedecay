//! Per-connection serving: one accepted daemon client, start to finish.
//!
//! Covers the authenticated Unix socket path, the routed rmcp bridge, and the
//! portable broker path. Each entry point owns framing, project-owner routing,
//! and connection teardown for exactly one client.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::profile_host_admission_replay::ProfileHostAdmissionBootstrapStatus;
use super::*;
use crate::daemon_contract::DaemonInvocationPayload;

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

pub(super) async fn serve_routed_rmcp_connection(
    server: Arc<crate::mcp::McpServer>,
    transport: BrokerStreamTransport,
    first_request_line: String,
    pending_lines: impl IntoIterator<Item = String>,
    initialize_route: Option<InitializeRouteMetadata>,
    timings_enabled: bool,
    lifecycle: &DaemonLifecycle,
) -> Result<()> {
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
            cancellation.cancel();
            waiting.await
        }
    };
    result.map_err(|error| TraceDecayError::Config {
        message: format!("rmcp server task failed: {error}"),
    })?;
    Ok(())
}

fn is_mcp_initialize_request(line: &str) -> bool {
    serde_json::from_str::<JsonRpcRequest>(line.trim())
        .is_ok_and(|request| request.method == "initialize")
}

const MAX_PENDING_PROJECT_OPEN_LINES: usize = 64;
const PROJECT_OWNER_HALF_CLOSE_GRACE: Duration = Duration::from_millis(750);

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
        let (operation, observed_at, event_class, kind, attempt_identity) = match &request.payload {
            DaemonInvocationPayload::WorkApplication {
                request:
                    request @ crate::daemon_contract::WorkApplicationInvocationV1::StartAttempt(command),
                observed_at,
                ..
            } => (
                request.operation_key(),
                *observed_at,
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            DaemonInvocationPayload::WorkApplication {
                request:
                    request
                    @ crate::daemon_contract::WorkApplicationInvocationV1::AttemptStatus(command),
                observed_at,
                ..
            } => (
                request.operation_key(),
                *observed_at,
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            DaemonInvocationPayload::WorkApplication {
                request:
                    request
                    @ crate::daemon_contract::WorkApplicationInvocationV1::CancelAttempt(command),
                observed_at,
                ..
            } => (
                request.operation_key(),
                *observed_at,
                tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
                DaemonWorkDeliveryKindV1::Attempt,
                tracedecay_domain::WorkAttemptIdentityV1::new(
                    command.task_id.clone(),
                    command.run_id.clone(),
                    command.attempt_id.clone(),
                )
                .ok(),
            ),
            DaemonInvocationPayload::WorkApplication {
                request:
                    request @ crate::daemon_contract::WorkApplicationInvocationV1::HydrateArtifacts(_),
                observed_at,
                ..
            } => (
                request.operation_key(),
                *observed_at,
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
        use crate::daemon_contract::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

        match (&self.kind, &response.outcome) {
            (
                DaemonWorkDeliveryKindV1::Attempt,
                DaemonInvocationOutcome::WorkApplication { outcome, .. },
            ) => match outcome {
                WorkApplicationOutcomeV1::StartAttempt(outcome)
                | WorkApplicationOutcomeV1::AttemptStatus(outcome)
                | WorkApplicationOutcomeV1::CancelAttempt(outcome) => {
                    application_outcome_payload(outcome).is_some()
                }
                _ => false,
            },
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

    async fn attempts(
        self,
        service: &crate::daemon::service::invocation::DaemonInvocationService,
        project_root: Option<&Path>,
        response: &DaemonInvocationResponse,
    ) -> Vec<tracedecay_domain::DeliverySettlementAttemptV1> {
        let identities = self.attempt_identities(response);
        if identities.is_empty() {
            return vec![self.into_attempt(self.owner_event_id.clone(), None)];
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
            attempts.push(self.into_attempt(
                format!(
                    "work:fan-out-response:{}",
                    owner.as_str().trim_start_matches("sha256:")
                ),
                Some(identity),
            ));
        }
        attempts
    }

    fn into_attempt(
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
        use crate::daemon_contract::{DaemonInvocationOutcome, WorkApplicationOutcomeV1};

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
) -> std::result::Result<(), crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason> {
    let (Some(recorder), Some(attempt)) = (recorder, attempt) else {
        return Err(
            crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
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
                crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason::RecorderAtCapacity,
            )
        }
        Err(error) => {
            tracing::warn!(%error, "daemon Work delivery receipt was refused");
            Err(
                crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
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
) -> std::result::Result<(), crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason> {
    let Some(attempts) = attempts else {
        return Err(
            crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
        );
    };
    if attempts.is_empty() {
        return Err(
            crate::daemon_contract::DaemonInvocationDeliveryAckRejectReason::RecorderUnavailable,
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
    response: &crate::daemon_contract::DaemonInvocationDeliveryAckResponse,
) -> Result<()> {
    transport
        .write_line(&serde_json::to_string(response)?)
        .await?;
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
    transport: &mut impl McpTransport,
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

#[cfg(test)]
mod delivery_ack_tests {
    use super::{
        DaemonDeliveryAckWait, await_daemon_delivery_ack, classify_daemon_delivery_ack_wait,
    };
    use crate::mcp::transport::ChannelTransport;
    use std::time::Duration;

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

pub(super) async fn await_project_owner_or_disconnect<T>(
    transport: &mut impl McpTransport,
    open: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<(T, VecDeque<String>)>> {
    tokio::pin!(open);
    let mut pending_lines = VecDeque::new();
    loop {
        // This loop continues after the read branch, so unlike the one-shot
        // selects below it drops an in-flight read every time `open` wins the
        // race — and the same transport is then handed to the routed server.
        // That is only safe because the transport's read half keeps its
        // partial-frame accumulator (`host_admission::BoundedLineReader`), so a
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
}

#[cfg(unix)]
async fn serve_broker_socket_client(
    stream: BrokerStream,
    engine: DaemonEngine,
    auth_token: Option<String>,
    admission_class: DaemonClientAdmissionClass,
) -> Result<()> {
    let mut transport = BrokerStreamTransport::new(stream);
    if let Some(expected_token) = auth_token.as_deref() {
        let preface_line = tokio::select! {
            result = read_line_handling_wire_oversized(&mut transport) => result?,
            () = engine.lifecycle.wait_for_draining() => return Ok(()),
        };
        let Some(preface_line) = preface_line else {
            return Ok(());
        };
        let preface =
            DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            })?;
        if !preface.authenticate(expected_token) {
            return Err(TraceDecayError::Config {
                message: "daemon client authentication failed".to_string(),
            });
        }
    }
    let line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(line) = line else {
        return Ok(());
    };
    let Some(setup_activity) = engine.lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&line)?;
    let peer_full_close = transport.peer_fully_closed_after_eof();
    tokio::pin!(peer_full_close);
    let store_administration = tokio::select! {
        result = bind_authenticated_profile_identity(&mut handshake, &engine.store_administration) => result?,
        () = &mut peer_full_close => return Ok(()),
    };
    let mut engine = engine;
    engine.store_administration = store_administration;
    let first_request_line = tokio::select! {
        result = read_line_handling_wire_oversized(&mut transport) => result?,
        () = engine.lifecycle.wait_for_draining() => return Ok(()),
    };
    let Some(first_request_line) = first_request_line else {
        return Ok(());
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match engine
            .per_client_admission
            .try_admit_request(&handshake, &first_request_line)
        {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(cancellation) =
        crate::daemon_contract::parse_daemon_invocation_cancellation_request(&first_request_line)
    {
        crate::daemon::request_cancellation::cancel(cancellation.target_request_id());
        drop(setup_activity);
        return Ok(());
    }
    let git_watcher_health = if doctor_runtime_request(&first_request_line).is_some() {
        Some(
            engine
                .git_watcher_health(handshake.project_path.as_deref())
                .await,
        )
    } else {
        None
    };
    let Some(setup_activity) = serve_core_doctor_runtime_request(
        &mut transport,
        &handshake,
        &engine.store_administration,
        setup_activity,
        &first_request_line,
        git_watcher_health,
        || async {
            Ok(engine
                .cached_project_server(&handshake)
                .await?
                .is_some_and(|server| server.doctor_report_ready()))
        },
    )
    .await?
    else {
        return Ok(());
    };
    engine.log_client_version_skew(&handshake).await;
    report_profile_host_admission_bootstrap_status(
        schedule_user_profile_host_admission_replay_for_identity(
            &engine.store_administration,
            &handshake.client_identity,
        )
        .await,
    );
    // Resolve initialize roots only after authentication and inside daemon
    // authority. The proxy process never opens the registry database.
    let initialize_route = apply_daemon_initialize_route(
        &mut handshake,
        &first_request_line,
        &engine.store_administration,
    )
    .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => engine.execute_branch_admin(&handshake, action).await,
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
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
        return Ok(());
    }
    if let Some(invocation) = parse_daemon_invocation_request(&first_request_line) {
        let mut invocation = invocation;
        let mut owned_lsp_sessions = HashMap::new();
        let mut pending_line = None;
        let result = async {
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
                        .and_then(crate::daemon_client::deadline_remaining);
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
                        .and_then(crate::daemon::request_cancellation::register);
                    let cancellation = delivery_cancellation
                        .as_ref()
                        .map(|lease| lease.token());
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
                            let ack = crate::daemon_contract::parse_daemon_invocation_delivery_ack_request(
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
                                        crate::daemon_contract::DaemonInvocationDeliveryAckResponse::accepted(
                                            target_request_id.clone(),
                                        )
                                    }
                                    Err(reason) => {
                                        crate::daemon_contract::DaemonInvocationDeliveryAckResponse::rejected(
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
        }
        .await;
        cleanup_connection_lsp_sessions(&engine.invocation, owned_lsp_sessions).await;
        return result;
    }
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
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
            && let Some(mut response) =
                daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
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
                            engine
                                .schedule_project_server_warmup(handshake.clone(), request.clone()),
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
                .claim_catalog_refresh(&handshake, &first_request_line, catalog_is_provisional)
                .await
                && let Err(error) = write_tool_list_changed_notification(&mut transport).await
            {
                engine.release_catalog_refresh(key).await;
                return Err(error);
            }
            drop(setup_activity);
            if let Some(response) = response {
                write_json_rpc_response(&mut transport, &response).await?;
            }
            return Ok(());
        }
    }
    let user_session_request = projectless_user_session_request(&first_request_line);
    let mut pending_project_open_lines = VecDeque::new();
    let server = if handshake.project_path.is_some() && !user_session_request {
        match await_project_owner_or_disconnect(
            &mut transport,
            engine.project_server_for_request(
                &handshake,
                project_server_requirement(&first_request_line),
            ),
        )
        .await
        {
            Ok(Some((server, pending_lines))) => {
                pending_project_open_lines = pending_lines;
                Some(server)
            }
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    drop(setup_activity);
    if !engine.lifecycle.accepting() {
        return Ok(());
    }

    // The stdio proxy creates one daemon connection per request. The request
    // was peeked above so initialize-root routing happens before project open.
    if let Some(key) = engine
        .claim_catalog_refresh(&handshake, &first_request_line, false)
        .await
        && let Err(error) = write_tool_list_changed_notification(&mut transport).await
    {
        engine.release_catalog_refresh(key).await;
        return Err(error);
    }
    if let Some(server) = server {
        if is_mcp_initialize_request(&first_request_line) {
            #[cfg(test)]
            tests::record_mcp_route(&handshake.client_instance_id, tests::ObservedMcpRoute::Rmcp);
            serve_routed_rmcp_connection(
                server,
                transport,
                first_request_line,
                pending_project_open_lines,
                initialize_route,
                handshake.timings,
                &engine.lifecycle,
            )
            .await?;
        } else {
            #[cfg(test)]
            tests::record_mcp_route(
                &handshake.client_instance_id,
                tests::ObservedMcpRoute::Legacy,
            );
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request_line)?;
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
        transport.push_replay(first_request_line)?;
        for line in pending_project_open_lines {
            transport.push_replay(line)?;
        }
        serve_projectless_client(
            &mut transport,
            &handshake.client_identity,
            &engine.lifecycle,
            &engine.store_administration,
        )
        .await?;
    }
    Ok(())
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
    let preface =
        DaemonAuthPreface::from_line(&preface_line).map_err(|_| TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        })?;
    if !preface.authenticate(auth_token) {
        return Err(TraceDecayError::Config {
            message: "daemon client authentication failed".to_string(),
        });
    }
    let Some(handshake_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    let Some(setup_activity) = lifecycle.try_enter() else {
        return Ok(());
    };
    let mut handshake = DaemonHandshake::from_line(&handshake_line)?;
    let Some(first_request_line) = read_line_handling_wire_oversized(&mut transport).await? else {
        return Ok(());
    };
    if let Some(response) = daemon_shutdown_response(&first_request_line) {
        lifecycle.begin_draining();
        write_json_rpc_response(&mut transport, &response).await?;
        drop(setup_activity);
        return Ok(());
    }
    let peer_full_close = transport.peer_fully_closed_after_eof();
    tokio::pin!(peer_full_close);
    let store_administration = tokio::select! {
        result = bind_authenticated_profile_identity(&mut handshake, &store_administration) => result?,
        () = &mut peer_full_close => return Ok(()),
    };
    let reserved_control_request = is_reserved_control_request(&first_request_line);
    if admission_class == DaemonClientAdmissionClass::ReservedControl && !reserved_control_request {
        drop(setup_activity);
        reject_reserved_bulk_request(
            &mut transport,
            &first_request_line,
            MAX_CONCURRENT_DAEMON_CLIENTS,
        )
        .await?;
        return Ok(());
    }
    let _per_client_permit = if admission_class == DaemonClientAdmissionClass::General {
        match per_client_admission.try_admit_request(&handshake, &first_request_line) {
            Ok(permit) => Some(permit),
            Err(response) => {
                drop(setup_activity);
                reject_admitted_request(&mut transport, &first_request_line, response).await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    if let Some(cancellation) =
        crate::daemon_contract::parse_daemon_invocation_cancellation_request(&first_request_line)
    {
        crate::daemon::request_cancellation::cancel(cancellation.target_request_id());
        drop(setup_activity);
        return Ok(());
    }
    let Some(setup_activity) = serve_core_doctor_runtime_request(
        &mut transport,
        &handshake,
        &store_administration,
        setup_activity,
        &first_request_line,
        None,
        || async {
            let (canonical_project_path, _) = project_route_for_handshake(&handshake)?;
            Ok(portable_cached_project_server(
                &store_administration,
                &canonical_project_path,
                &handshake,
                ProjectServerRequirement::Core,
            )
            .await?
            .is_some_and(|server| server.doctor_report_ready()))
        },
    )
    .await?
    else {
        return Ok(());
    };
    report_profile_host_admission_bootstrap_status(
        schedule_user_profile_host_admission_replay_for_identity(
            &store_administration,
            &handshake.client_identity,
        )
        .await,
    );
    let initialize_route =
        apply_daemon_initialize_route(&mut handshake, &first_request_line, &store_administration)
            .await?;
    if let Some(request) = parse_branch_admin_request(&first_request_line) {
        let result = match request.action.clone() {
            Ok(action) => {
                store_administration
                    .execute_branch_admin_for_handshake(&handshake, action)
                    .await
            }
            Err(message) => Err(TraceDecayError::Config { message }),
        };
        drop(setup_activity);
        write_branch_admin_response(&mut transport, request, result).await?;
        return Ok(());
    }
    if let Some(request) = parse_branch_add_request(&first_request_line) {
        let response = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry.clone(),
                &handshake,
                ProjectServerRequirement::Core,
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(_)) => {
                branch_add_response(
                    &store_administration,
                    Some(&invocation.code_index_schedulers),
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
        return Ok(());
    }
    if let Some(invocation_request) = parse_daemon_invocation_request(&first_request_line) {
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
                        execute_portable_daemon_invocation(
                            lifecycle.clone(),
                            store_administration.clone(),
                            Arc::clone(&project_open_gates),
                            &handshake,
                            &invocation,
                            http_application_registry.clone(),
                            request,
                            #[cfg(test)]
                            project_open_attempts.clone(),
                        )
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
                        .and_then(crate::daemon_client::deadline_remaining);
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
                        .and_then(crate::daemon::request_cancellation::register);
                    let cancellation = delivery_cancellation
                        .as_ref()
                        .map(|lease| lease.token());
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
                            let ack = crate::daemon_contract::parse_daemon_invocation_delivery_ack_request(
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
                                        crate::daemon_contract::DaemonInvocationDeliveryAckResponse::accepted(
                                            target_request_id.clone(),
                                        )
                                    }
                                    Err(reason) => {
                                        crate::daemon_contract::DaemonInvocationDeliveryAckResponse::rejected(
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
    if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(first_request_line.trim()) {
        let initialized_project_server_ready =
            if matches!(classify_mcp_method(&request.method), McpMethod::Initialize)
                && handshake.project_path.is_some()
            {
                let (project_path, _) = project_route_for_handshake(&handshake)?;
                portable_cached_project_server(
                    &store_administration,
                    &project_path,
                    &handshake,
                    ProjectServerRequirement::Core,
                )
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
                daemon_bootstrap_response(&request, initialize_route.as_ref(), project_node_count)
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
    let user_session_request = projectless_user_session_request(&first_request_line);
    if handshake.project_path.is_some() && !user_session_request {
        let server = match await_project_owner_or_disconnect(
            &mut transport,
            portable_project_server_for_request(
                lifecycle.clone(),
                store_administration.clone(),
                Arc::clone(&project_open_gates),
                invocation.clone(),
                http_application_registry,
                &handshake,
                project_server_requirement(&first_request_line),
                #[cfg(test)]
                project_open_attempts.clone(),
            ),
        )
        .await
        {
            Ok(Some(server)) => server,
            Ok(None) => {
                drop(setup_activity);
                return Ok(());
            }
            Err(error) => {
                drop(setup_activity);
                write_project_open_error(&mut transport, &first_request_line, &error).await?;
                return Ok(());
            }
        };
        drop(setup_activity);
        let (server, pending_lines) = server;
        if is_mcp_initialize_request(&first_request_line) {
            #[cfg(test)]
            tests::record_mcp_route(&handshake.client_instance_id, tests::ObservedMcpRoute::Rmcp);
            serve_routed_rmcp_connection(
                server,
                transport,
                first_request_line,
                pending_lines,
                initialize_route,
                handshake.timings,
                lifecycle,
            )
            .await?;
        } else {
            #[cfg(test)]
            tests::record_mcp_route(
                &handshake.client_instance_id,
                tests::ObservedMcpRoute::Legacy,
            );
            let mut transport = ReplayTransport::new(transport);
            transport.push_replay(first_request_line)?;
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
        transport.push_replay(first_request_line)?;
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
