//! Shared adapter-to-daemon dispatch contracts.
//!
//! This module deliberately owns request correlation and transport-neutral
//! admission/reconnect seams only. It does not invoke application services,
//! query stores, or render results.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use tracedecay_application::{
    ApplicationEnvelope, ApplicationInvocation, ApplicationInvocationExecutor,
    ApplicationInvocationFuture, ApplicationProblem, ApplicationProblemKind, ApplicationRequest,
    ApplicationResponse, CancellationContext, CancellationSignal, CancellationStage, Deadline,
    InvocationError, InvocationTarget, PageRequest, RequestId, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingId, BindingSurface, CatalogSnapshotV1, FeatureId,
    ProfileId, SchemaRef, SurfaceOperationName,
};

use tracedecay_application::feedback::observations::{
    FeedbackDeliveryRouteV1, FeedbackSourceEventV1,
};
use tracedecay_application::request_identity::{GlobalRequestSurface, mint_global_request_id};

pub type ScopeSelector = InvocationTarget;

use crate::lsp_wire::ConnectionLocalRequestSequence;
use crate::output_format::RequestedOutputFormat;

/// The shared cancellation reference carried into an application invocation.
pub type CancellationRef = CancellationSignal;

/// The transport-neutral invocation constructed by CLI and MCP adapters.
///
/// `requested_format` is intentionally carried only until
/// [`BoundInvocation::into_application_invocation`] is called. The resulting
/// application invocation has no presentation-format field.
pub struct CanonicalInvocation<T> {
    pub request: T,
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
    pub requested_format: RequestedOutputFormat,
}

/// Common invocation controls after transport syntax validation.
pub struct InvocationControls {
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
    pub requested_format: RequestedOutputFormat,
}

/// Transport-decoded input to the one canonical binding dispatcher.
pub struct DispatchInput<T> {
    pub request_id: RequestId,
    pub binding: BindingResolution,
    pub request: T,
    pub controls: InvocationControls,
}

/// A non-disclosing binding-resolution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchError {
    UnknownOrNotAuthorized,
}

impl<T> CanonicalInvocation<T> {
    pub fn new(
        request: T,
        scope: ScopeSelector,
        page: PageRequest,
        deadline: Option<Deadline>,
        cancellation: CancellationRef,
        requested_format: RequestedOutputFormat,
    ) -> Self {
        Self {
            request,
            scope,
            page,
            deadline,
            cancellation,
            requested_format,
        }
    }
}

/// A canonical invocation after the adapter has resolved its catalog binding.
pub struct BoundInvocation<T> {
    pub binding_id: BindingId,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
    pub invocation: CanonicalInvocation<T>,
}

impl<T> BoundInvocation<T> {
    pub fn new(binding: ResolvedBinding, invocation: CanonicalInvocation<T>) -> Self {
        Self {
            binding_id: binding.binding_id,
            request_schema: binding.request_schema,
            result_schema: binding.result_schema,
            invocation,
        }
    }

    /// Separates presentation from the application call boundary.
    pub fn into_application_invocation(self) -> (AdapterInvocation<T>, RequestedOutputFormat) {
        let Self {
            binding_id,
            request_schema: _,
            result_schema: _,
            invocation,
        } = self;
        let CanonicalInvocation {
            request,
            scope,
            page,
            deadline,
            cancellation,
            requested_format,
        } = invocation;

        (
            AdapterInvocation {
                binding_id,
                request,
                scope,
                page,
                deadline,
                cancellation,
            },
            requested_format,
        )
    }
}

/// The data permitted to cross from an adapter into the application boundary.
///
/// This type deliberately omits presentation format and transport request
/// framing.
pub struct AdapterInvocation<T> {
    pub binding_id: BindingId,
    pub request: T,
    pub scope: ScopeSelector,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationRef,
}

/// Catalog inputs needed to resolve a surface operation to one binding ID.
pub struct BindingResolution {
    pub profile_id: ProfileId,
    pub operation: SurfaceOperationName,
    pub protocol_revision: u32,
    pub negotiated_features: std::collections::BTreeSet<FeatureId>,
}

/// Catalog binding plus the canonical schema references indexed for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub binding_id: BindingId,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
}

/// Resolves a visible, callable surface binding without exposing why lookup
/// failed. `None` intentionally conflates unknown, hidden, unavailable, and
/// incompatible operations.
pub trait BindingResolver {
    fn resolve_binding(
        &self,
        surface: BindingSurface,
        request: &BindingResolution,
    ) -> Option<ResolvedBinding>;
}

/// Metadata-only resolver backed by one immutable catalog snapshot.
pub struct CatalogBindingResolver<'a> {
    catalog: &'a CatalogSnapshotV1,
}

impl<'a> CatalogBindingResolver<'a> {
    pub fn new(catalog: &'a CatalogSnapshotV1) -> Self {
        Self { catalog }
    }
}

impl BindingResolver for CatalogBindingResolver<'_> {
    fn resolve_binding(
        &self,
        surface: BindingSurface,
        request: &BindingResolution,
    ) -> Option<ResolvedBinding> {
        let capability = self.catalog.resolve_binding(
            &request.profile_id,
            surface,
            &request.operation,
            request.protocol_revision,
            &request.negotiated_features,
        )?;

        let binding_id = capability.binding_ids().iter().find_map(|binding_id| {
            let binding = self.catalog.binding(binding_id)?;
            (binding.surface() == surface && binding.operation() == &request.operation)
                .then(|| binding_id.clone())
        })?;
        let request_schema = self
            .catalog
            .schema(
                capability.request_schema().schema_id(),
                capability.request_schema().revision(),
            )?
            .clone();
        let result_schema = self
            .catalog
            .schema(
                capability.result_schema().schema_id(),
                capability.result_schema().revision(),
            )?
            .clone();

        Some(ResolvedBinding {
            binding_id,
            request_schema,
            result_schema,
        })
    }
}

/// Resolve one transport binding and construct the canonical invocation.
///
/// The surface is selected by adapter code, never decoded from user input.
pub fn resolve_dispatch<T>(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    input: DispatchInput<T>,
) -> Result<DispatchedInvocation<T>, DispatchError> {
    let DispatchInput {
        request_id,
        binding,
        request,
        controls,
    } = input;
    let resolved = resolver
        .resolve_binding(surface, &binding)
        .ok_or(DispatchError::UnknownOrNotAuthorized)?;
    let invocation = CanonicalInvocation::new(
        request,
        controls.scope,
        controls.page,
        controls.deadline,
        controls.cancellation,
        controls.requested_format,
    );

    Ok(DispatchedInvocation::new(
        request_id,
        surface,
        BoundInvocation::new(resolved, invocation),
    ))
}

/// An invocation paired with the request identity used for daemon dispatch.
pub struct DispatchedInvocation<T> {
    pub request_id: RequestId,
    pub surface: BindingSurface,
    pub invocation: BoundInvocation<T>,
}

impl<T> DispatchedInvocation<T> {
    pub fn new(
        request_id: RequestId,
        surface: BindingSurface,
        invocation: BoundInvocation<T>,
    ) -> Self {
        Self {
            request_id,
            surface,
            invocation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationCancellationPolicy {
    ReadOnly,
    AuthoritativeEffect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonInvocationError {
    Cancelled {
        stage: CancellationStage,
    },
    TimedOut {
        stage: CancellationStage,
    },
    Unavailable,
    /// The connect phase failed after the restart grace: no daemon accepted
    /// at the endpoint, so the request was never sent. Kept distinct from
    /// [`Self::Unavailable`] because re-dispatching the same request in-process
    /// cannot succeed until a daemon is back — callers fail fast with the
    /// typed connect diagnostic instead of retrying to their deadline.
    Unreachable {
        reason_code: String,
        detail: String,
    },
}

impl DaemonInvocationError {
    pub fn into_application_problem(self) -> ApplicationProblem {
        match self {
            Self::Cancelled { stage } => ApplicationProblem::Cancelled {
                stage,
                retry: tracedecay_application::RetryDirective::Never,
                legal_actions: Vec::new(),
            },
            Self::TimedOut { stage } => ApplicationProblem::TimedOut {
                stage,
                retry: tracedecay_application::RetryDirective::Never,
                legal_actions: Vec::new(),
            },
            Self::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
                code: "daemon_unavailable".to_owned(),
                message: "The owning TraceDecay daemon is unavailable".to_owned(),
            }),
            Self::Unreachable {
                reason_code,
                detail,
            } => ApplicationProblem::unavailable(SafeDiagnostic {
                code: reason_code,
                message: detail,
            }),
        }
    }
}

pub type DaemonInvocationExecutorFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Production execution boundary for the daemon's closed invocation protocol.
///
/// Socket clients and daemon-local project servers implement this same port.
/// Request correlation is already present on `DaemonInvocationRequest`; effect
/// idempotency remains owned by each operation payload and is never reminted
/// by this transport boundary.
///
/// The port is stated purely in terms of the daemon invocation contract, so
/// implementing or calling it does not drag in the daemon's service internals.
pub trait DaemonInvocationExecutor: ApplicationInvocationExecutor + Send + Sync {
    fn invoke_controlled(
        &self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        Result<crate::contract::DaemonInvocationResponse, DaemonInvocationError>,
    >;

    fn observe_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, tracedecay_domain::errors::Result<()>>;
}

/// Authenticated socket client for the daemon's closed invocation protocol.
///
/// This client shares the daemon connection/authentication path with MCP but
/// sends only versioned invocation envelopes. It deliberately cannot issue an
/// arbitrary daemon method or reconstruct a Git/feedback application request.
#[derive(Clone)]
pub struct DaemonInvocationClient {
    connection: crate::connection::DaemonConnection,
    handshake: crate::handshake::DaemonHandshake,
    pool: Arc<DaemonInvocationConnectionPool>,
    activity: Arc<DaemonInvocationClientActivity>,
}

#[derive(Default)]
struct DaemonInvocationClientActivity {
    queued: AtomicUsize,
    in_flight: AtomicUsize,
}

enum DaemonInvocationClientPhase {
    Queued,
    InFlight,
}

struct DaemonInvocationClientActivityGuard {
    activity: Arc<DaemonInvocationClientActivity>,
    phase: DaemonInvocationClientPhase,
}

impl DaemonInvocationClientActivity {
    fn queued(self: &Arc<Self>) -> DaemonInvocationClientActivityGuard {
        self.queued.fetch_add(1, Ordering::AcqRel);
        hotpath::gauge!("daemon.invocation.client.queued").inc(1.0);
        DaemonInvocationClientActivityGuard {
            activity: Arc::clone(self),
            phase: DaemonInvocationClientPhase::Queued,
        }
    }

    fn in_flight(self: &Arc<Self>) -> DaemonInvocationClientActivityGuard {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        hotpath::gauge!("daemon.invocation.client.in_flight").inc(1.0);
        DaemonInvocationClientActivityGuard {
            activity: Arc::clone(self),
            phase: DaemonInvocationClientPhase::InFlight,
        }
    }
}

impl DaemonInvocationClientActivityGuard {
    fn into_in_flight(self) -> Self {
        let activity = Arc::clone(&self.activity);
        drop(self);
        activity.in_flight()
    }
}

impl Drop for DaemonInvocationClientActivityGuard {
    fn drop(&mut self) {
        match self.phase {
            DaemonInvocationClientPhase::Queued => {
                self.activity.queued.fetch_sub(1, Ordering::AcqRel);
                hotpath::gauge!("daemon.invocation.client.queued").inc(-1.0);
            }
            DaemonInvocationClientPhase::InFlight => {
                self.activity.in_flight.fetch_sub(1, Ordering::AcqRel);
                hotpath::gauge!("daemon.invocation.client.in_flight").inc(-1.0);
            }
        }
    }
}

struct DaemonInvocationConnection {
    reader: BufReader<ReadHalf<crate::transport::BrokerStream>>,
    writer: WriteHalf<crate::transport::BrokerStream>,
}

/// Keeps one client well below the daemon's 64-connection per-client
/// admission ceiling while allowing useful request parallelism.
const DAEMON_INVOCATION_CONNECTION_POOL_CAPACITY: usize = 8;

struct DaemonInvocationConnectionPool {
    idle: Mutex<Vec<DaemonInvocationConnection>>,
    permits: Arc<Semaphore>,
}

impl DaemonInvocationConnectionPool {
    fn shared() -> Arc<Self> {
        Arc::new(Self {
            idle: Mutex::new(Vec::with_capacity(
                DAEMON_INVOCATION_CONNECTION_POOL_CAPACITY,
            )),
            permits: Arc::new(Semaphore::new(DAEMON_INVOCATION_CONNECTION_POOL_CAPACITY)),
        })
    }
}

struct InvocationConnectionLease {
    pool: Arc<DaemonInvocationConnectionPool>,
    connection: Option<DaemonInvocationConnection>,
    permit: Option<OwnedSemaphorePermit>,
    return_to_pool: bool,
}

impl InvocationConnectionLease {
    fn connection_mut(
        &mut self,
    ) -> tracedecay_domain::errors::Result<&mut DaemonInvocationConnection> {
        self.connection
            .as_mut()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon invocation connection lease is unavailable".to_owned(),
            })
    }

    fn release_to_pool(&mut self) {
        self.return_to_pool = true;
    }
}

impl Drop for InvocationConnectionLease {
    fn drop(&mut self) {
        if self.return_to_pool
            && let Some(connection) = self.connection.take()
        {
            self.pool
                .idle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(connection);
        }
        drop(self.permit.take());
    }
}

/// Response plus same-connection delivery settlement authority when required.
pub struct DaemonInvocationResult {
    response: crate::contract::DaemonInvocationResponse,
    delivery: Option<DaemonInvocationDelivery>,
}

impl DaemonInvocationResult {
    pub fn into_response(self) -> crate::contract::DaemonInvocationResponse {
        self.response
    }

    pub fn into_parts(
        self,
    ) -> (
        crate::contract::DaemonInvocationResponse,
        Option<DaemonInvocationDelivery>,
    ) {
        (self.response, self.delivery)
    }
}

/// A Work delivery acknowledgement pinned to its response connection.
pub struct DaemonInvocationDelivery {
    lease: InvocationConnectionLease,
    target_request_id: String,
    connection: crate::connection::DaemonConnection,
}

/// Client-owned dispatch ceiling for native `FastEmbed` current+10x evaluation.
///
/// The daemon honors this deadline on `semantic_evaluate_and_publish`; it is
/// not a journey-harness timeout. Sized from a quieter-host measurement of
/// 625s for the pinned 1x+10x `FastEmbed` workload (load 23→18 on 96 cores,
/// isolated `CARGO_TARGET_DIR=/tmp/semantic-rerun-target`) plus margin.
pub const SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS: i64 = 900_000_000;

/// Isolated 10x measurement plus paged incremental copies. Production
/// `evaluate_and_publish_semantic_profile` stays at 900s; eval-direct sizes
/// this from the 906s deadline miss after reused paging.
pub const SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS: i64 = 1_800_000_000;

impl DaemonInvocationClient {
    pub fn new(
        connection: crate::connection::DaemonConnection,
        handshake: crate::handshake::DaemonHandshake,
    ) -> Self {
        Self {
            connection,
            handshake,
            pool: DaemonInvocationConnectionPool::shared(),
            activity: Arc::new(DaemonInvocationClientActivity::default()),
        }
    }

    #[cfg(test)]
    pub fn for_connection_for_test(
        connection: crate::connection::DaemonConnection,
        handshake: crate::handshake::DaemonHandshake,
    ) -> Self {
        Self {
            connection,
            handshake,
            pool: DaemonInvocationConnectionPool::shared(),
            activity: Arc::new(DaemonInvocationClientActivity::default()),
        }
    }

    async fn checkout_connection(
        &self,
    ) -> tracedecay_domain::errors::Result<(
        InvocationConnectionLease,
        DaemonInvocationClientActivityGuard,
    )> {
        let queued = self.activity.queued();
        let permit = hotpath::future!(
            Arc::clone(&self.pool.permits).acquire_owned(),
            label = "daemon.invocation.client.queue_wait"
        )
        .await
        .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon invocation connection pool is closed".to_owned(),
        })?;
        let in_flight = queued.into_in_flight();
        let idle = self
            .pool
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop();
        let connection = match idle {
            Some(connection) => connection,
            None => {
                let stream = hotpath::future!(
                    crate::connection::connect_to_daemon_connection(&self.connection),
                    label = "daemon.invocation.client.connect"
                )
                .await?;
                let (reader, mut writer) = stream.into_split();
                hotpath::future!(
                    crate::connection::write_daemon_preamble(
                        &mut writer,
                        &self.connection,
                        &self.handshake
                    ),
                    label = "daemon.invocation.client.preamble"
                )
                .await?;
                DaemonInvocationConnection {
                    reader: BufReader::new(reader),
                    writer,
                }
            }
        };
        Ok((
            InvocationConnectionLease {
                pool: Arc::clone(&self.pool),
                connection: Some(connection),
                permit: Some(permit),
                return_to_pool: false,
            },
            in_flight,
        ))
    }

    async fn invoke_on_connection(
        &self,
        lease: &mut InvocationConnectionLease,
        request: crate::contract::DaemonInvocationRequest,
    ) -> tracedecay_domain::errors::Result<crate::contract::DaemonInvocationResponse> {
        let request_id = request.request_id.clone();
        let request_label = request.operation().as_str();
        let connection = lease.connection_mut()?;
        let request_json = hotpath::measure_block!(
            "daemon.invocation.client.request.encode",
            serde_json::to_string(&request)
        )?;
        hotpath::gauge!("daemon.invocation.client.request.bytes").set(request_json.len() as f64);
        hotpath::future!(
            async {
                connection.writer.write_all(request_json.as_bytes()).await?;
                connection.writer.write_all(b"\n").await?;
                connection.writer.flush().await
            },
            label = "daemon.invocation.client.request.write"
        )
        .await?;

        let Some(line) = hotpath::future!(
            crate::connection::next_daemon_response_line(
                &mut connection.reader,
                &self.connection,
                request_label,
                crate::connection::DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
            ),
            label = "daemon.invocation.client.response.wait"
        )
        .await?
        else {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "daemon closed the invocation connection after '{request_label}' was sent; the outcome is unknown"
                ),
            });
        };
        hotpath::gauge!("daemon.invocation.client.response.bytes").set(line.len() as f64);
        let response: crate::contract::DaemonInvocationResponse = match hotpath::measure_block!(
            "daemon.invocation.client.response.decode",
            serde_json::from_str(&line)
        ) {
            Ok(response) => response,
            Err(_) => {
                if let Some(refusal) = crate::handshake::DaemonHandshakeRefusal::from_line(&line) {
                    return Err(handshake_refusal_error(&refusal, &self.handshake));
                }
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "daemon returned an invalid invocation response".to_owned(),
                });
            }
        };
        if response.protocol != crate::contract::DAEMON_INVOCATION_PROTOCOL
            || response.revision != crate::contract::DAEMON_INVOCATION_REVISION
            || response.request_id != request_id
        {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon invocation response did not match the request".to_owned(),
            });
        }
        Ok(response)
    }

    /// Invokes one request using a bounded shared connection pool.
    ///
    /// FIFO ordering is preserved on each checked-out connection. Concurrent
    /// requests on different leases have no cross-connection ordering.
    #[hotpath::skip]
    pub async fn invoke(
        &self,
        request: crate::contract::DaemonInvocationRequest,
    ) -> tracedecay_domain::errors::Result<crate::contract::DaemonInvocationResponse> {
        self.invoke_with_delivery(request)
            .await
            .map(DaemonInvocationResult::into_response)
    }

    #[hotpath::measure(label = "daemon.invocation.client", future = true)]
    pub async fn invoke_with_delivery(
        &self,
        request: crate::contract::DaemonInvocationRequest,
    ) -> tracedecay_domain::errors::Result<DaemonInvocationResult> {
        let request_id = request.request_id.clone();
        let delivery_ack_required = request.delivery_ack_deadline().is_some();
        let (mut lease, _in_flight) = self.checkout_connection().await.map_err(|error| {
            with_daemon_version_skew_context(error, &self.connection, &self.handshake)
        })?;
        let result = self.invoke_on_connection(&mut lease, request).await;
        match result {
            Ok(response) => {
                let delivery = if delivery_ack_required {
                    Some(DaemonInvocationDelivery {
                        lease,
                        target_request_id: request_id,
                        connection: self.connection.clone(),
                    })
                } else {
                    lease.release_to_pool();
                    None
                };
                Ok(DaemonInvocationResult { response, delivery })
            }
            Err(error) => Err(with_daemon_version_skew_context(
                error,
                &self.connection,
                &self.handshake,
            )),
        }
    }

    #[hotpath::skip]
    async fn acknowledge_work_delivery_on_connection(
        connection: &mut DaemonInvocationConnection,
        daemon_connection: &crate::connection::DaemonConnection,
        target_request_id: &str,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) -> tracedecay_domain::errors::Result<()> {
        let request = match (outcome, reason) {
            (tracedecay_domain::DeliverySettlementOutcomeV1::Delivered, None) => {
                crate::contract::DaemonInvocationDeliveryAckRequest::delivered(target_request_id)
            }
            (tracedecay_domain::DeliverySettlementOutcomeV1::Dropped, Some(reason)) => {
                crate::contract::DaemonInvocationDeliveryAckRequest::dropped(
                    target_request_id,
                    reason,
                )
            }
            (
                tracedecay_domain::DeliverySettlementOutcomeV1::Delivered
                | tracedecay_domain::DeliverySettlementOutcomeV1::Deduplicated
                | tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
                _,
            ) => {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "invalid Work delivery acknowledgement outcome".to_owned(),
                });
            }
        };
        let request_id = target_request_id.to_owned();
        let result = async {
            connection
                .writer
                .write_all(serde_json::to_string(&request)?.as_bytes())
                .await?;
            connection.writer.write_all(b"\n").await?;
            connection.writer.flush().await?;

            let Some(line) = crate::connection::next_daemon_response_line(
                &mut connection.reader,
                daemon_connection,
                "invocation_delivery_ack",
                crate::connection::DAEMON_TOOL_LIVENESS_POLL_INTERVAL,
            )
            .await?
            else {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message:
                        "daemon closed the invocation connection before acknowledging Work delivery"
                            .to_owned(),
                });
            };
            let response: crate::contract::DaemonInvocationDeliveryAckResponse =
                serde_json::from_str(&line).map_err(|_| {
                    tracedecay_domain::errors::TraceDecayError::Config {
                        message:
                            "daemon returned an invalid Work delivery acknowledgement response"
                                .to_owned(),
                    }
                })?;
            if !response.matches_request(&request_id) {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "daemon Work delivery acknowledgement did not match the request"
                        .to_owned(),
                });
            }
            if let Some(reason) = response.rejection_reason() {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon rejected the Work delivery acknowledgement: {reason:?}"
                    ),
                });
            }
            Ok(())
        }
        .await;
        result
    }

    #[hotpath::skip]
    pub async fn observe_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    ) -> tracedecay_domain::errors::Result<()> {
        let request_id = mint_global_request_id(GlobalRequestSurface::FeedbackObservation)
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: error.to_string(),
            })?;
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::feedback_observation(
                    request_id.as_str(),
                    subject_digest,
                    observed_at,
                    event,
                ),
            )
            .await?;
        if matches!(
            response.outcome,
            crate::contract::DaemonInvocationOutcome::ObservationAccepted
        ) {
            Ok(())
        } else {
            Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon did not accept the feedback observation".to_owned(),
            })
        }
    }

    #[hotpath::skip]
    pub async fn evaluate_and_publish_semantic_profile(
        &self,
        evaluated_profile_id: &str,
    ) -> tracedecay_domain::errors::Result<SemanticEvaluationPublicationResultV1> {
        self.evaluate_and_publish_semantic_profile_until(
            evaluated_profile_id,
            SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
        )
        .await
    }

    /// Run the composed activation journey: evaluate the profile natively,
    /// publish the accepted evaluation, and compare-and-swap it into
    /// `active_profile`. The daemon owns every stage; the evaluation phase
    /// dominates the deadline.
    #[hotpath::skip]
    pub async fn activate_semantic_profile(
        &self,
        evaluated_profile_id: &str,
        set_rollback: bool,
    ) -> tracedecay_domain::errors::Result<SemanticActivationResultV1> {
        self.activate_semantic_profile_until(
            evaluated_profile_id,
            set_rollback,
            SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
        )
        .await
    }

    #[hotpath::skip]
    pub async fn activate_semantic_profile_until(
        &self,
        evaluated_profile_id: &str,
        set_rollback: bool,
        deadline_micros: i64,
    ) -> tracedecay_domain::errors::Result<SemanticActivationResultV1> {
        let request_id =
            mint_global_request_id(GlobalRequestSurface::SemanticEvaluation).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: error.to_string(),
                }
            })?;
        let observed_at = current_system_micros().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "semantic activation clock is unavailable".to_owned(),
            }
        })?;
        let deadline = Deadline::new(UtcMicros(
            observed_at.0.checked_add(deadline_micros).ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "semantic activation deadline is unavailable".to_owned(),
                }
            })?,
        ))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
        let cancellation = CancellationContext::active(format!(
            "cancellation.semantic-activation.{}",
            request_id.as_str()
        ))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
        let response = self
            .invoke(crate::contract::DaemonInvocationRequest::semantic_activate(
                request_id.as_str(),
                evaluated_profile_id.to_owned(),
                set_rollback,
                observed_at,
                deadline,
                cancellation,
            ))
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::SemanticProfileActivated {
                scope,
                profile_digest,
                report_digest,
                configuration_revision,
                rollback_profile_id,
                runtime_state,
            } => Ok(SemanticActivationResultV1 {
                project_id: scope.project_id.as_str().to_owned(),
                profile_digest: profile_digest.as_str().to_owned(),
                report_digest: report_digest.as_str().to_owned(),
                configuration_revision: configuration_revision.to_string(),
                rollback_profile_id,
                runtime_state,
            }),
            crate::contract::DaemonInvocationOutcome::Problem { problem } => {
                Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("semantic activation rejected: {problem:?}"),
                })
            }
            crate::contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                Err(semantic_activation_application_problem(problem))
            }
            _ => Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon returned an invalid semantic activation response".to_owned(),
            }),
        }
    }

    #[hotpath::skip]
    pub async fn evaluate_and_publish_semantic_profile_until(
        &self,
        evaluated_profile_id: &str,
        deadline_micros: i64,
    ) -> tracedecay_domain::errors::Result<SemanticEvaluationPublicationResultV1> {
        let request_id =
            mint_global_request_id(GlobalRequestSurface::SemanticEvaluation).map_err(|error| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: error.to_string(),
                }
            })?;
        let observed_at = current_system_micros().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "semantic evaluation clock is unavailable".to_owned(),
            }
        })?;
        let deadline = Deadline::new(UtcMicros(
            observed_at.0.checked_add(deadline_micros).ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "semantic evaluation deadline is unavailable".to_owned(),
                }
            })?,
        ))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
        let cancellation = CancellationContext::active(format!(
            "cancellation.semantic-evaluation.{}",
            request_id.as_str()
        ))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::semantic_evaluate_and_publish(
                    request_id.as_str(),
                    evaluated_profile_id.to_owned(),
                    observed_at,
                    deadline,
                    cancellation,
                ),
            )
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                scope,
                profile_digest,
                report_digest,
                report,
                source_generation,
                snapshot_digest,
            } => Ok(SemanticEvaluationPublicationResultV1 {
                project_id: scope.project_id.as_str().to_owned(),
                profile_digest: profile_digest.as_str().to_owned(),
                report_digest: report_digest.as_str().to_owned(),
                report,
                source_generation: source_generation.as_str().to_owned(),
                snapshot_digest: snapshot_digest.as_str().to_owned(),
            }),
            crate::contract::DaemonInvocationOutcome::Problem { problem } => {
                Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("semantic evaluation publication rejected: {problem:?}"),
                })
            }
            crate::contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                Err(semantic_evaluation_application_problem(problem))
            }
            _ => Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon returned an invalid semantic evaluation response".to_owned(),
            }),
        }
    }

    #[hotpath::skip]
    pub async fn qualify_semantic_profile_until(
        &self,
        evaluated_profile_id: &str,
        deadline_micros: i64,
        cancellation: CancellationSignal,
    ) -> tracedecay_domain::errors::Result<SemanticEvaluationQualificationResultV1> {
        let request_id = mint_global_request_id(GlobalRequestSurface::SemanticQualification)
            .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
                message: error.to_string(),
            })?;
        let observed_at = current_system_micros().ok_or_else(|| {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: "semantic qualification clock is unavailable".to_owned(),
            }
        })?;
        let deadline = Deadline::new(UtcMicros(
            observed_at.0.checked_add(deadline_micros).ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "semantic qualification deadline is unavailable".to_owned(),
                }
            })?,
        ))
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: error.to_string(),
        })?;
        let response = self
            .invoke_controlled(
                crate::contract::DaemonInvocationRequest::semantic_qualify(
                    request_id.as_str(),
                    evaluated_profile_id.to_owned(),
                    observed_at,
                    deadline.clone(),
                    cancellation.context(),
                ),
                deadline,
                cancellation,
                InvocationCancellationPolicy::ReadOnly,
            )
            .await
            .map_err(|error| {
                semantic_qualification_application_problem(error.into_application_problem())
            })?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::SemanticEvaluatedProfileQualified {
                qualification,
            } => Ok(SemanticEvaluationQualificationResultV1 {
                qualification_bytes: qualification.into_bytes(),
            }),
            crate::contract::DaemonInvocationOutcome::Problem { problem } => {
                Err(semantic_qualification_daemon_problem(problem))
            }
            crate::contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                Err(semantic_qualification_application_problem(problem))
            }
            _ => Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon returned an invalid semantic qualification response".to_owned(),
            }),
        }
    }

    #[hotpath::skip]
    async fn cancel_invocation(
        &self,
        target_request_id: &str,
    ) -> tracedecay_domain::errors::Result<()> {
        let stream = crate::connection::connect_to_daemon_connection(&self.connection).await?;
        let (_reader, mut writer) = stream.into_split();
        crate::connection::write_daemon_preamble(&mut writer, &self.connection, &self.handshake)
            .await?;
        let request = crate::contract::DaemonInvocationCancellationRequest::new(target_request_id);
        writer
            .write_all(serde_json::to_string(&request)?.as_bytes())
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

impl DaemonInvocationDelivery {
    #[hotpath::skip]
    pub async fn acknowledge(
        mut self,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) -> tracedecay_domain::errors::Result<()> {
        let target_request_id = self.target_request_id.clone();
        let daemon_connection = self.connection.clone();
        let result = DaemonInvocationClient::acknowledge_work_delivery_on_connection(
            self.lease.connection_mut()?,
            &daemon_connection,
            &target_request_id,
            outcome,
            reason,
        )
        .await;
        if result.is_ok() {
            self.lease.release_to_pool();
        }
        result
    }
}

impl DaemonInvocationExecutor for DaemonInvocationClient {
    fn invoke_controlled(
        &self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        Result<crate::contract::DaemonInvocationResponse, DaemonInvocationError>,
    > {
        Box::pin(DaemonInvocationClient::invoke_controlled(
            self,
            request,
            deadline,
            cancellation,
            policy,
        ))
    }

    fn observe_feedback(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, tracedecay_domain::errors::Result<()>> {
        Box::pin(DaemonInvocationClient::observe_feedback(
            self,
            subject_digest,
            observed_at,
            event,
        ))
    }
}

/// Decode the envelope-stripped configuration body selected by `operation`.
///
/// This is the socket-client dispatch arm: producers strip the tagged
/// `ConfigurationWireRequestV1` envelope before admission, so the client
/// deserializes the inner request and wraps the operation-selected variant.
fn configuration_request_from_surface_payload(
    operation: ApplicationSurfaceOperation,
    payload: serde_json::Value,
) -> Result<tracedecay_application::ConfigurationWireRequestV1, InvocationError> {
    tracedecay_application::configuration_wire_request_from_invocation_payload(
        operation.as_str(),
        payload,
    )
    .map_err(|_| InvocationError::InvalidRequest)
}

fn feedback_handle_from_surface_payload(
    payload: serde_json::Value,
) -> Result<tracedecay_application::feedback::FeedbackHandleRequestV1, InvocationError> {
    let request: tracedecay_application::feedback::FeedbackHandleRequestV1 =
        serde_json::from_value(payload).map_err(|_| InvocationError::InvalidRequest)?;
    tracedecay_application::feedback::FeedbackHandleRequestV1::new(request.request_handle)
        .map_err(|_| InvocationError::InvalidRequest)
}

impl ApplicationInvocationExecutor for DaemonInvocationClient {
    fn invoke(
        &self,
        invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, Result<ApplicationResponse, InvocationError>> {
        Box::pin(async move {
            let (context, request) = invocation.into_parts();
            let (request_id, target, deadline, cancellation) = context.into_parts();
            match request {
                ApplicationRequest::Surface { binding, payload } => {
                    let (_binding_id, surface, operation, result_contract, _page) =
                        binding.into_parts();
                    let operation = ApplicationSurfaceOperation::from_tool_name(operation.as_str())
                        .ok_or(InvocationError::InvalidRequest)?;
                    let observed_at = invocation_now_micros();
                    let cancellation_context = cancellation.context();
                    let scope = match target {
                        InvocationTarget::CurrentProject => None,
                        InvocationTarget::Resolved(scope) => Some(scope),
                    };
                    let policy = if matches!(
                        operation,
                        ApplicationSurfaceOperation::ConfigurationSet
                            | ApplicationSurfaceOperation::ConfigurationUnset
                            | ApplicationSurfaceOperation::ConfigurationBatch
                    ) {
                        InvocationCancellationPolicy::AuthoritativeEffect
                    } else {
                        InvocationCancellationPolicy::ReadOnly
                    };
                    let request = match operation {
                        ApplicationSurfaceOperation::ConfigurationGet
                        | ApplicationSurfaceOperation::ConfigurationSet
                        | ApplicationSurfaceOperation::ConfigurationUnset
                        | ApplicationSurfaceOperation::ConfigurationBatch => {
                            let request =
                                configuration_request_from_surface_payload(operation, payload)?;
                            crate::contract::DaemonInvocationRequest::configuration(
                                request_id.as_str(),
                                operation,
                                request,
                                observed_at,
                                deadline.clone(),
                                cancellation_context,
                            )
                            .with_resolved_scope(scope)
                        }
                        ApplicationSurfaceOperation::FeedbackGet => {
                            let request = feedback_handle_from_surface_payload(payload)?;
                            crate::contract::DaemonInvocationRequest::feedback(
                                request_id.as_str(),
                                operation,
                                request.request_handle,
                                observed_at,
                                deadline.clone(),
                                cancellation_context,
                            )
                            .with_resolved_scope(scope)
                        }
                        _ => return Err(InvocationError::InvalidRequest),
                    }
                    .with_delivery_route(application_delivery_route(surface));
                    let response = self
                        .invoke_controlled(request, deadline, cancellation, policy)
                        .await
                        .map_err(map_invocation_error)?;
                    application_response(request_id, result_contract, response.outcome)
                }
                ApplicationRequest::FeedbackObservation {
                    configuration_digest,
                    observed_at,
                    event,
                } => {
                    let event = serde_json::from_value(event)
                        .map_err(|_| InvocationError::InvalidRequest)?;
                    self.observe_feedback(configuration_digest, observed_at, event)
                        .await
                        .map_err(|_| InvocationError::Unavailable)?;
                    Ok(ApplicationResponse::ObservationAccepted)
                }
                ApplicationRequest::OperationEvents { .. }
                | ApplicationRequest::OperationCancel { .. } => Err(InvocationError::Unavailable),
            }
        })
    }
}

/// Retained name for its call sites across the daemon, application surface,
/// and CLI commands (the bin target is a separate crate, so `pub(crate)`
/// would hide it from `src/commands`); the saturating clamp is the one
/// shared definition.
pub fn invocation_now_micros() -> UtcMicros {
    tracedecay_application::clock::now_micros()
}

pub fn application_delivery_route(surface: BindingSurface) -> FeedbackDeliveryRouteV1 {
    match surface {
        BindingSurface::Cli => FeedbackDeliveryRouteV1::Cli,
        BindingSurface::Mcp => FeedbackDeliveryRouteV1::Mcp,
        BindingSurface::Http | BindingSurface::Dashboard => FeedbackDeliveryRouteV1::Http,
        BindingSurface::Lsp => FeedbackDeliveryRouteV1::Lsp,
    }
}

pub fn map_invocation_error(error: DaemonInvocationError) -> InvocationError {
    match error {
        DaemonInvocationError::Cancelled { .. } => InvocationError::Cancelled,
        DaemonInvocationError::TimedOut { .. } => InvocationError::DeadlineExceeded,
        DaemonInvocationError::Unavailable => InvocationError::Unavailable,
        DaemonInvocationError::Unreachable {
            reason_code,
            detail,
        } => InvocationError::Unreachable {
            reason_code,
            detail,
        },
    }
}

pub fn application_response(
    request_id: RequestId,
    result_contract: tracedecay_application::ResultContractRef,
    outcome: crate::contract::DaemonInvocationOutcome,
) -> Result<ApplicationResponse, InvocationError> {
    let envelope = match outcome {
        crate::contract::DaemonInvocationOutcome::Feedback { scope, result } => {
            ApplicationEnvelope::evidence(
                result_contract,
                request_id,
                scope,
                result.into_application(),
            )
        }
        crate::contract::DaemonInvocationOutcome::Configuration { scope, outcome } => {
            ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome,
            }
        }
        crate::contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
            // The daemon already resolved this invocation to a typed problem
            // (e.g. `configuration.conflict`); carry it whole so surface
            // adapters republish that diagnostic instead of refabricating a
            // generic one.
            return Err(InvocationError::Problem(Box::new(problem)));
        }
        crate::contract::DaemonInvocationOutcome::Problem { problem } => {
            return Err(match problem {
                crate::contract::DaemonInvocationProblem::InvalidRequest
                | crate::contract::DaemonInvocationProblem::UnsupportedRevision => {
                    InvocationError::InvalidRequest
                }
                crate::contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                    InvocationError::Denied
                }
                crate::contract::DaemonInvocationProblem::ResetRequired => {
                    InvocationError::Problem(Box::new(ApplicationProblem::reset_required(
                        SafeDiagnostic {
                            code: "daemon.reset_required".to_owned(),
                            message: "The owning daemon authority requires an explicit reset"
                                .to_owned(),
                        },
                    )))
                }
                crate::contract::DaemonInvocationProblem::ApplicationContractViolation => {
                    InvocationError::Unavailable
                }
                crate::contract::DaemonInvocationProblem::Unavailable => {
                    InvocationError::Unavailable
                }
            });
        }
        _ => return Err(InvocationError::Unavailable),
    };
    Ok(ApplicationResponse::unary(envelope))
}

fn invocation_error_from_problem(problem: &ApplicationProblem) -> InvocationError {
    match problem.kind() {
        ApplicationProblemKind::NotFoundOrNotAuthorized => InvocationError::Denied,
        ApplicationProblemKind::Cancelled => InvocationError::Cancelled,
        ApplicationProblemKind::TimedOut => InvocationError::DeadlineExceeded,
        ApplicationProblemKind::InvalidRequest => InvocationError::InvalidRequest,
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            InvocationError::Conflict
        }
        ApplicationProblemKind::PartialEffect
        | ApplicationProblemKind::ExecutionFailed
        | ApplicationProblemKind::ResetRequired => {
            InvocationError::Problem(Box::new(problem.clone()))
        }
        ApplicationProblemKind::Unavailable
        | ApplicationProblemKind::Unsupported
        | ApplicationProblemKind::Saturated => InvocationError::Unavailable,
    }
}

fn semantic_evaluation_application_problem(
    problem: ApplicationProblem,
) -> tracedecay_domain::errors::TraceDecayError {
    let retryable = problem.retry() != RetryDirective::Never;
    match problem.kind() {
        ApplicationProblemKind::Cancelled => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_cancelled",
                retryable,
                "Semantic evaluation was cancelled",
            )
        }
        ApplicationProblemKind::TimedOut => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_deadline_exceeded",
                retryable,
                "Semantic evaluation exceeded its deadline",
            )
        }
        ApplicationProblemKind::Unavailable | ApplicationProblemKind::Saturated => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_unavailable",
                retryable,
                "Semantic evaluation publication is unavailable",
            )
        }
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_conflict",
                retryable,
                "Semantic evaluation publication conflicted with newer state",
            )
        }
        ApplicationProblemKind::PartialEffect => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_partial_effect",
                retryable,
                problem.diagnostic().map_or(
                    "Semantic evaluation publication committed only part of its required effect",
                    |diagnostic| diagnostic.message.as_str(),
                ),
            )
        }
        ApplicationProblemKind::ExecutionFailed => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_execution_failed",
                false,
                "Semantic evaluation execution failed",
            )
        }
        ApplicationProblemKind::ResetRequired => {
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "semantic evaluation publication",
                problem.diagnostic().map_or(
                    "the semantic evaluation authority requires reset",
                    |diagnostic| diagnostic.message.as_str(),
                ),
            )
        }
        ApplicationProblemKind::NotFoundOrNotAuthorized => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_evaluation_denied",
                retryable,
                "Semantic evaluation publication was not found or not authorized",
            )
        }
        ApplicationProblemKind::InvalidRequest | ApplicationProblemKind::Unsupported => {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "semantic evaluation publication rejected: {}",
                    problem.diagnostic().map_or_else(
                        || format!("{problem:?}"),
                        |diagnostic| diagnostic.message.clone(),
                    )
                ),
            }
        }
    }
}

/// A daemon that could not serve this client's handshake refused before any
/// request ran; name the refusal, both versions, and the action.
pub fn handshake_refusal_error(
    refusal: &crate::handshake::DaemonHandshakeRefusal,
    handshake: &crate::handshake::DaemonHandshake,
) -> tracedecay_domain::errors::TraceDecayError {
    if refusal.refusal == crate::handshake::DaemonHandshakeRefusalReason::AuthenticationRejected {
        // Not version skew: the daemon read the preface and rejected the
        // token. The token came from the daemon authority record, so a stale
        // record (daemon restarted after this client resolved it) is the
        // ordinary cause and a fresh attempt re-resolves it.
        return tracedecay_domain::errors::TraceDecayError::project_route(
            DAEMON_AUTHENTICATION_REJECTED,
            true,
            format!(
                "daemon (version {}) rejected this client's authentication token; \
                 the daemon authority record this client resolved is likely stale — \
                 retry, or check `tracedecay daemon status`",
                refusal.daemon_version
            ),
        );
    }
    let action =
        crate::handshake::version_skew_action(&refusal.daemon_version, &handshake.client_version);
    tracedecay_domain::errors::TraceDecayError::project_route(
        DAEMON_PROTOCOL_REVISION_SKEW,
        false,
        format!(
            "daemon (version {}) refused this client's handshake as {:?} \
             (client version {}); {action}",
            refusal.daemon_version, refusal.refusal, handshake.client_version
        ),
    )
}

/// Typed reason code for a daemon/client wire-revision mismatch.
pub const DAEMON_PROTOCOL_REVISION_SKEW: &str = "daemon_protocol_revision_skew";

/// Typed reason code for a daemon that refused the client's auth preface.
pub const DAEMON_AUTHENTICATION_REJECTED: &str = "daemon_authentication_rejected";

/// Attach version-skew context to a transport-level invocation failure.
///
/// Every error the invocation path produces here is transport-shaped (the
/// daemon's typed problems arrive as `Ok` responses), so a version mismatch
/// between the authority record's daemon and this client is the most likely
/// cause and must be visible instead of a bare `Connection reset by peer`.
fn with_daemon_version_skew_context(
    error: tracedecay_domain::errors::TraceDecayError,
    connection: &crate::connection::DaemonConnection,
    handshake: &crate::handshake::DaemonHandshake,
) -> tracedecay_domain::errors::TraceDecayError {
    // A refusal the daemon authored is already the definitive answer; naming
    // version skew over it would misdirect the operator.
    if let Some((code, _, _)) = error.project_route_context()
        && (code == DAEMON_PROTOCOL_REVISION_SKEW || code == DAEMON_AUTHENTICATION_REJECTED)
    {
        return error;
    }
    let Some(daemon_version) = connection.daemon_version.as_deref() else {
        return error;
    };
    if crate::handshake::client_version_skew(&handshake.client_version, daemon_version).is_none() {
        return error;
    }
    let action = crate::handshake::version_skew_action(daemon_version, &handshake.client_version);
    tracedecay_domain::errors::TraceDecayError::project_route(
        DAEMON_PROTOCOL_REVISION_SKEW,
        false,
        format!(
            "{error}. The daemon (version {daemon_version}) and this client \
             (version {}) are different builds of the invocation protocol, \
             which is the most likely cause of this transport failure; {action}",
            handshake.client_version
        ),
    )
}

/// Map a typed activation-journey problem onto the client error surface.
///
/// The stage that refused is carried by the diagnostic the daemon attached
/// (evaluation problems come from the semantic evaluation route,
/// configuration problems from the mutation route); this mapping preserves
/// that detail instead of flattening it into a generic message.
fn semantic_activation_application_problem(
    problem: ApplicationProblem,
) -> tracedecay_domain::errors::TraceDecayError {
    let retryable = problem.retry() != RetryDirective::Never;
    let detail = problem
        .diagnostic()
        .map(|diagnostic| format!(" ({}: {})", diagnostic.code, diagnostic.message))
        .unwrap_or_default();
    match problem.kind() {
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_conflict",
                retryable,
                format!(
                    "Semantic activation lost its configuration compare-and-swap; \
                     the configuration changed while the profile evaluated — re-run \
                     the activation{detail}"
                ),
            )
        }
        ApplicationProblemKind::Cancelled => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_cancelled",
                retryable,
                format!("Semantic activation was cancelled{detail}"),
            )
        }
        ApplicationProblemKind::TimedOut => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_deadline_exceeded",
                retryable,
                format!("Semantic activation exceeded its deadline{detail}"),
            )
        }
        ApplicationProblemKind::Unavailable | ApplicationProblemKind::Saturated => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_unavailable",
                retryable,
                format!("Semantic activation is unavailable{detail}"),
            )
        }
        ApplicationProblemKind::PartialEffect => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_partial_effect",
                retryable,
                format!("Semantic activation committed only part of its effect{detail}"),
            )
        }
        ApplicationProblemKind::ExecutionFailed => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_execution_failed",
                false,
                format!("Semantic activation execution failed{detail}"),
            )
        }
        ApplicationProblemKind::ResetRequired => {
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "semantic activation",
                problem.diagnostic().map_or(
                    "the semantic activation authority requires reset",
                    |diagnostic| diagnostic.message.as_str(),
                ),
            )
        }
        ApplicationProblemKind::NotFoundOrNotAuthorized => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_activation_denied",
                retryable,
                format!("Semantic activation was not found or not authorized{detail}"),
            )
        }
        ApplicationProblemKind::InvalidRequest | ApplicationProblemKind::Unsupported => {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "semantic activation rejected: {}",
                    problem.diagnostic().map_or_else(
                        || format!("{problem:?}"),
                        |diagnostic| diagnostic.message.clone(),
                    )
                ),
            }
        }
    }
}

fn semantic_qualification_daemon_problem(
    problem: crate::contract::DaemonInvocationProblem,
) -> tracedecay_domain::errors::TraceDecayError {
    match problem {
        crate::contract::DaemonInvocationProblem::InvalidRequest
        | crate::contract::DaemonInvocationProblem::UnsupportedRevision => {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!("semantic qualification rejected: {problem:?}"),
            }
        }
        crate::contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_denied",
                false,
                "Semantic qualification was not found or not authorized",
            )
        }
        crate::contract::DaemonInvocationProblem::ResetRequired => {
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "semantic qualification",
                "the semantic qualification authority requires reset",
            )
        }
        crate::contract::DaemonInvocationProblem::ApplicationContractViolation
        | crate::contract::DaemonInvocationProblem::Unavailable => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_unavailable",
                false,
                "Semantic qualification is unavailable",
            )
        }
    }
}

fn semantic_qualification_application_problem(
    problem: ApplicationProblem,
) -> tracedecay_domain::errors::TraceDecayError {
    let retryable = problem.retry() != RetryDirective::Never;
    match problem.kind() {
        ApplicationProblemKind::Cancelled => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_cancelled",
                retryable,
                "Semantic qualification was cancelled",
            )
        }
        ApplicationProblemKind::TimedOut => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_deadline_exceeded",
                retryable,
                "Semantic qualification exceeded its deadline",
            )
        }
        ApplicationProblemKind::Unavailable | ApplicationProblemKind::Saturated => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_unavailable",
                retryable,
                "Semantic qualification is unavailable",
            )
        }
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_stale",
                retryable,
                "Semantic qualification became stale",
            )
        }
        ApplicationProblemKind::PartialEffect => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_partial_result",
                retryable,
                problem.diagnostic().map_or(
                    "Semantic qualification returned only a partial result",
                    |diagnostic| diagnostic.message.as_str(),
                ),
            )
        }
        ApplicationProblemKind::ExecutionFailed => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_execution_failed",
                false,
                "Semantic qualification execution failed",
            )
        }
        ApplicationProblemKind::ResetRequired => {
            tracedecay_domain::errors::TraceDecayError::reset_required(
                "semantic qualification",
                problem.diagnostic().map_or(
                    "the semantic qualification authority requires reset",
                    |diagnostic| diagnostic.message.as_str(),
                ),
            )
        }
        ApplicationProblemKind::NotFoundOrNotAuthorized => {
            tracedecay_domain::errors::TraceDecayError::project_route(
                "semantic_qualification_denied",
                retryable,
                "Semantic qualification was not found or not authorized",
            )
        }
        ApplicationProblemKind::InvalidRequest | ApplicationProblemKind::Unsupported => {
            tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "semantic qualification rejected: {}",
                    problem.diagnostic().map_or_else(
                        || format!("{problem:?}"),
                        |diagnostic| diagnostic.message.clone(),
                    )
                ),
            }
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct SemanticEvaluationPublicationResultV1 {
    pub project_id: String,
    pub profile_digest: String,
    pub report_digest: String,
    pub report: serde_json::Value,
    pub source_generation: String,
    pub snapshot_digest: String,
}

/// Terminal receipt of the composed semantic activation journey.
/// `runtime_state` is the daemon-serialized `SemanticRuntimeStateV1`.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct SemanticActivationResultV1 {
    pub project_id: String,
    pub profile_digest: String,
    pub report_digest: String,
    pub configuration_revision: String,
    pub rollback_profile_id: Option<String>,
    pub runtime_state: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticEvaluationQualificationResultV1 {
    pub qualification_bytes: Vec<u8>,
}

pub fn deadline_remaining(deadline: &Deadline) -> Option<Duration> {
    let now = current_system_micros().map_or(i64::MAX, |now| now.0);
    let remaining = deadline.expires_at.0.checked_sub(now)?;
    (remaining > 0).then(|| Duration::from_micros(remaining as u64))
}

fn current_system_micros() -> Option<UtcMicros> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .map(UtcMicros)
}

mod lsp_session;
pub use lsp_session::DaemonLspSessionClient;

pub async fn wait_for_cancellation(cancellation: CancellationSignal) {
    cancellation.cancelled().await;
}

mod controlled_invocation;

#[cfg(test)]
mod controlled_invocation_tests;

#[cfg(test)]
mod tests {
    use super::{
        DaemonInvocationError, SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS,
        SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
        SemanticEvaluationPublicationResultV1, SemanticEvaluationQualificationResultV1,
        application_response, configuration_request_from_surface_payload,
        feedback_handle_from_surface_payload, semantic_evaluation_application_problem,
        semantic_qualification_application_problem,
    };
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemKind, CancellationStage, ConfigurationWireRequestV1,
        InvocationError, RequestId, ResultContractRef,
    };
    use tracedecay_tool_catalog::ApplicationSurfaceOperation;
    use tracedecay_tool_catalog::SchemaId;

    #[test]
    fn daemon_invocation_errors_keep_canonical_problem_categories() {
        for (error, expected) in [
            (
                DaemonInvocationError::Cancelled {
                    stage: CancellationStage::BeforeAdmission,
                },
                ApplicationProblemKind::Cancelled,
            ),
            (
                DaemonInvocationError::TimedOut {
                    stage: CancellationStage::BeforeAdmission,
                },
                ApplicationProblemKind::TimedOut,
            ),
            (
                DaemonInvocationError::Unavailable,
                ApplicationProblemKind::Unavailable,
            ),
        ] {
            assert_eq!(error.into_application_problem().kind(), expected);
        }
    }

    #[test]
    fn daemon_reset_response_remains_an_authoritative_typed_problem() {
        let error = application_response(
            RequestId::new("request.daemon-client.reset").expect("request"),
            ResultContractRef::new(
                SchemaId::new("schema.test.daemon-client-reset-result").expect("schema"),
                1,
            )
            .expect("contract"),
            crate::contract::DaemonInvocationOutcome::Problem {
                problem: crate::contract::DaemonInvocationProblem::ResetRequired,
            },
        )
        .expect_err("reset-required must not become a successful response");

        let InvocationError::Problem(problem) = error else {
            panic!("reset-required must remain an authoritative typed problem");
        };
        assert_eq!(problem.kind(), ApplicationProblemKind::ResetRequired);
    }

    #[test]
    fn isolated_evaluation_dispatch_deadline_is_eval_scoped_not_production_900s() {
        assert_eq!(SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS, 900_000_000);
        assert_eq!(
            SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS,
            1_800_000_000
        );
        const {
            assert!(
                SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS
                    > SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS
            );
        }
    }

    #[test]
    fn semantic_evaluation_client_maps_typed_application_problems() {
        for (problem, expected_reason) in [
            (
                ApplicationProblem::cancelled_before_admission(),
                "semantic_evaluation_cancelled",
            ),
            (
                ApplicationProblem::timed_out_before_admission(),
                "semantic_evaluation_deadline_exceeded",
            ),
        ] {
            let error = semantic_evaluation_application_problem(problem);
            let (reason, retryable, _) = error
                .project_route_context()
                .expect("typed semantic evaluation error");
            assert_eq!(reason, expected_reason);
            assert!(!retryable);
        }
    }

    fn unused_test_endpoint() -> crate::transport::DaemonEndpoint {
        crate::transport::DaemonEndpoint::loopback(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("loopback endpoint")
    }

    #[test]
    fn transport_failures_name_version_skew_when_the_authority_daemon_differs() {
        let connection = crate::connection::DaemonConnection::new(unused_test_endpoint(), None)
            .with_daemon_version("0.1.0-beta.36+aaaa");
        let mut handshake = test_skew_handshake();
        handshake.client_version = "0.1.0-beta.37+bbbb".to_owned();

        let error = super::with_daemon_version_skew_context(
            tracedecay_domain::errors::TraceDecayError::Io(std::io::Error::from(
                std::io::ErrorKind::ConnectionReset,
            )),
            &connection,
            &handshake,
        );
        let (code, retryable, _) = error
            .project_route_context()
            .expect("skewed transport failures must be typed");
        assert_eq!(code, super::DAEMON_PROTOCOL_REVISION_SKEW);
        assert!(!retryable);
        let message = error.to_string();
        assert!(
            message.contains("0.1.0-beta.36+aaaa") && message.contains("0.1.0-beta.37+bbbb"),
            "the skew error must name both versions: {message}"
        );
        assert!(
            message.contains("daemon restart"),
            "an older daemon must be pointed at `tracedecay daemon restart`: {message}"
        );
    }

    #[test]
    fn transport_failures_stay_untouched_without_version_skew() {
        let matching = crate::connection::DaemonConnection::new(unused_test_endpoint(), None)
            .with_daemon_version("0.1.0-beta.37+cccc");
        let unknown = crate::connection::DaemonConnection::new(unused_test_endpoint(), None);
        let mut handshake = test_skew_handshake();
        handshake.client_version = "0.1.0-beta.37+cccc".to_owned();

        for connection in [matching, unknown] {
            let error = super::with_daemon_version_skew_context(
                tracedecay_domain::errors::TraceDecayError::Io(std::io::Error::from(
                    std::io::ErrorKind::ConnectionReset,
                )),
                &connection,
                &handshake,
            );
            assert!(
                error.project_route_context().is_none(),
                "matching or unknown daemon versions must not invent a skew: {error}"
            );
        }
    }

    #[test]
    fn handshake_refusal_frames_become_typed_revision_skew_errors() {
        let refusal = crate::handshake::DaemonHandshakeRefusal {
            protocol: crate::handshake::DAEMON_HANDSHAKE_REFUSAL_PROTOCOL.to_owned(),
            refusal: crate::handshake::DaemonHandshakeRefusalReason::UnsupportedRevision,
            daemon_version: "0.1.0-beta.36+dddd".to_owned(),
        };
        let mut handshake = test_skew_handshake();
        handshake.client_version = "0.1.0-beta.37+eeee".to_owned();

        let error = super::handshake_refusal_error(&refusal, &handshake);
        let (code, retryable, _) = error
            .project_route_context()
            .expect("handshake refusals must be typed");
        assert_eq!(code, super::DAEMON_PROTOCOL_REVISION_SKEW);
        assert!(!retryable);
        let message = error.to_string();
        assert!(
            message.contains("UnsupportedRevision")
                && message.contains("0.1.0-beta.36+dddd")
                && message.contains("0.1.0-beta.37+eeee"),
            "the refusal error must name the reason and both versions: {message}"
        );
    }

    #[test]
    fn authentication_refusal_frames_become_typed_auth_errors() {
        let refusal = crate::handshake::DaemonHandshakeRefusal::for_rejected_authentication(
            "0.1.0-beta.36+dddd",
        );
        let mut handshake = test_skew_handshake();
        handshake.client_version = "0.1.0-beta.37+eeee".to_owned();

        let error = super::handshake_refusal_error(&refusal, &handshake);
        let (code, retryable, _) = error
            .project_route_context()
            .expect("authentication refusals must be typed");
        assert_eq!(code, super::DAEMON_AUTHENTICATION_REJECTED);
        assert!(
            retryable,
            "a stale authority record is recoverable by re-resolving it"
        );
        let message = error.to_string();
        assert!(
            message.contains("rejected this client's authentication token")
                && message.contains("0.1.0-beta.36+dddd")
                && message.contains("tracedecay daemon status"),
            "the auth error must name the refusal and the next action: {message}"
        );

        // The skew decorator must not relabel the daemon's definitive answer,
        // even when the authority record names a different daemon version.
        let connection = crate::connection::DaemonConnection::new(unused_test_endpoint(), None)
            .with_daemon_version("0.1.0-beta.36+dddd");
        let decorated = super::with_daemon_version_skew_context(error, &connection, &handshake);
        let (code, _, _) = decorated
            .project_route_context()
            .expect("decorated auth refusal stays typed");
        assert_eq!(code, super::DAEMON_AUTHENTICATION_REJECTED);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refused_handshake_surfaces_typed_skew_instead_of_transport_noise() {
        let isolation = tempfile::TempDir::new().expect("socket isolation");
        let socket = isolation.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind fake daemon");
        let refusal_line = crate::handshake::DaemonHandshakeRefusal {
            protocol: crate::handshake::DAEMON_HANDSHAKE_REFUSAL_PROTOCOL.to_owned(),
            refusal: crate::handshake::DaemonHandshakeRefusalReason::UnsupportedRevision,
            daemon_version: "0.1.0-beta.36+ffff".to_owned(),
        }
        .to_line()
        .expect("refusal line");
        let daemon = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (stream, _) = listener.accept().await.expect("accept client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            // Handshake line, then the pipelined request line.
            let _ = lines.next_line().await.expect("read handshake");
            let _ = lines.next_line().await.expect("read request");
            writer
                .write_all(format!("{refusal_line}\n").as_bytes())
                .await
                .expect("write refusal");
            writer.flush().await.expect("flush refusal");
        });

        let mut handshake = test_skew_handshake();
        handshake.client_version = "0.1.0-beta.37+gggg".to_owned();
        let client = super::DaemonInvocationClient::new(
            crate::connection::DaemonConnection::new(
                crate::transport::DaemonEndpoint::Unix(socket),
                None,
            )
            .with_daemon_version("0.1.0-beta.36+ffff"),
            handshake,
        );

        let error = client
            .evaluate_and_publish_semantic_profile("hybrid-conservative")
            .await
            .expect_err("a refused handshake must not look like a published profile");
        let (code, _, _) = error
            .project_route_context()
            .expect("the refusal must surface as a typed revision skew");
        assert_eq!(code, super::DAEMON_PROTOCOL_REVISION_SKEW);
        assert!(
            error.to_string().contains("0.1.0-beta.36+ffff"),
            "the error must name the refusing daemon version: {error}"
        );
        daemon.await.expect("fake daemon completes");
    }

    fn test_skew_handshake() -> crate::handshake::DaemonHandshake {
        crate::handshake::DaemonHandshake {
            project_path: None,
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: crate::client_identity::DaemonClientIdentity::new(
                std::path::PathBuf::from("/tmp/profile"),
                std::path::PathBuf::from("/tmp/profile/global.db"),
            ),
            client_version: String::new(),
            client_instance_id: "00000000000000000000000000000000".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
        }
    }

    #[test]
    fn semantic_qualification_client_maps_typed_application_problems() {
        for (problem, expected_reason) in [
            (
                ApplicationProblem::cancelled_before_admission(),
                "semantic_qualification_cancelled",
            ),
            (
                ApplicationProblem::timed_out_before_admission(),
                "semantic_qualification_deadline_exceeded",
            ),
        ] {
            let error = semantic_qualification_application_problem(problem);
            let (reason, retryable, _) = error
                .project_route_context()
                .expect("typed semantic qualification error");
            assert_eq!(reason, expected_reason);
            assert!(!retryable);
        }
    }

    #[test]
    fn semantic_evaluation_client_prints_rejection_diagnostic() {
        let error = semantic_evaluation_application_problem(ApplicationProblem::InvalidRequest {
            diagnostic: tracedecay_application::SafeDiagnostic {
                code: "semantic_evaluation.rejected".to_owned(),
                message: "exact eligible chunks current expected 2170, measured 2184".to_owned(),
            },
            retry: tracedecay_application::RetryDirective::Never,
            legal_actions: Vec::new(),
        });
        let message = error.to_string();
        assert!(
            message.contains("2184"),
            "client must print the SearchEvalError detail: {message}"
        );
        assert!(
            message.contains("semantic evaluation publication rejected"),
            "client must keep the publication rejection prefix: {message}"
        );
    }

    #[test]
    fn semantic_evaluation_result_retains_the_direct_report() {
        let result = SemanticEvaluationPublicationResultV1 {
            project_id: "project-1".to_owned(),
            profile_digest: format!("sha256:{}", "1".repeat(64)),
            report_digest: format!("sha256:{}", "2".repeat(64)),
            report: serde_json::json!({
                "command": "compare",
                "status": "pass",
                "workload_digest": format!("sha256:{}", "3".repeat(64)),
                "corpus_digest": format!("sha256:{}", "4".repeat(64)),
                "fixture_source_repository_commit": "fixture-commit",
                "fixture_source_repository_tree": "fixture-tree",
                "execution_contract": {
                    "exact_file_count": 0,
                    "exact_corpus_bytes": 0,
                    "exact_eligible_chunks_current": 0,
                    "exact_eligible_chunks_10x": 0,
                    "exact_query_count": 0,
                    "model_revision": "model.serialization-test.v1",
                    "projection_revision": "projection.serialization-test.v1",
                    "fusion_revision": "fusion.serialization-test.v1",
                    "runtime_revision": "runtime.serialization-test.v1",
                    "cache_state": "empty",
                    "concurrency": {
                        "query_workers": 1,
                        "projection_workers": 1,
                        "query_execution": "serial"
                    }
                },
                "profile_material_digests": {},
                "raw_output_digest": format!("sha256:{}", "6".repeat(64)),
                "raw_outputs": [],
                "profiles": []
            }),
            source_generation: "generation-1".to_owned(),
            snapshot_digest: format!("sha256:{}", "5".repeat(64)),
        };

        let encoded = serde_json::to_value(result).expect("serialize evaluation result");
        assert_eq!(encoded["report"]["status"], "pass");
        assert_eq!(encoded["report"]["command"], "compare");
    }

    #[test]
    fn semantic_qualification_result_preserves_canonical_bytes() {
        let result = SemanticEvaluationQualificationResultV1 {
            qualification_bytes: vec![0x51, 0x55, 0x41, 0x4c],
        };

        assert_eq!(result.qualification_bytes, vec![0x51, 0x55, 0x41, 0x4c]);
    }

    #[test]
    fn configuration_dispatch_accepts_envelope_stripped_get_and_set_payloads() {
        let get = configuration_request_from_surface_payload(
            ApplicationSurfaceOperation::ConfigurationGet,
            serde_json::json!({"key": "mcp.tool_timings"}),
        )
        .expect("stripped get payload");
        assert!(matches!(
            get,
            ConfigurationWireRequestV1::Get(request) if request.key.as_str() == "mcp.tool_timings"
        ));

        let set = configuration_request_from_surface_payload(
            ApplicationSurfaceOperation::ConfigurationSet,
            serde_json::json!({
                "layer": {"kind": "default"},
                "key": "mcp.tool_timings",
                "value": {"kind": "boolean", "value": true},
                "expected_revision": "revision.test-configuration-set",
                "idempotency_key": "configuration.idempotency.test-set"
            }),
        )
        .expect("stripped set payload");
        assert!(matches!(set, ConfigurationWireRequestV1::Set(_)));
    }

    #[test]
    fn configuration_dispatch_rejects_the_tagged_envelope() {
        assert!(matches!(
            configuration_request_from_surface_payload(
                ApplicationSurfaceOperation::ConfigurationGet,
                serde_json::json!({
                    "operation": "get",
                    "request": {"key": "mcp.tool_timings"}
                }),
            ),
            Err(InvocationError::InvalidRequest)
        ));
    }

    #[test]
    fn feedback_get_dispatch_validates_handles_client_side() {
        let accepted = feedback_handle_from_surface_payload(serde_json::json!({
            "request_handle": "feedback.handle.v1"
        }))
        .expect("valid handle");
        assert_eq!(accepted.request_handle, "feedback.handle.v1");
        assert_eq!(
            feedback_handle_from_surface_payload(serde_json::json!({
                "request_handle": " leading"
            })),
            Err(InvocationError::InvalidRequest)
        );
    }
}
