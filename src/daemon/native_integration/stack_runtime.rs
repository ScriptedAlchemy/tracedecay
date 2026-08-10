//! Daemon-local runtime for durable GitHub stack transition delivery.
//!
//! The synchronous coordinator accesses the asynchronous registered database
//! through one bounded actor owned by the native-integration project owner.
//! Publication advances a durable row only to `host_pending`; authenticated
//! MCP expansion is the sole settlement path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::git::{
    GITHUB_STACK_SIGNAL_EXPAND_OPERATION, GitHubStackSignalEvidenceRefV1,
    GitHubStackSignalExpandPort, GitHubStackSignalExpandPortError,
    GitHubStackSignalExpandRequestV1, GitHubStackSignalExpandSurfaceResultV1,
    git_surface_operation,
};
use tracedecay_application::{
    CancellationSignal, NativeIntegrationContractError, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1, RequestAdmission,
    RequestContext, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, NativeIntegrationApprovalV1, ProjectId, StackDeliveryWatermarkId,
    StackSignalId, UtcMicros, canonical_sha256,
};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_usecases::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, OptionalStackPreflightPort, StackCircuitPolicyV1,
    StackCoordinatorErrorV1, StackCoordinatorStore, StackDeliveryAuthorizationPort,
    StackDeliveryAuthorizationV1, StackDeliveryBatchV1, StackDeliveryPort, StackPendingDeliveryV1,
    StackSignalV1,
};

use crate::global_db::{
    GitHubStackDeliveryKeyV1, GitHubStackDeliveryRecordV1, GitHubStackDeliveryStateV1,
    GitHubStackSignalAppendOutcomeV1, GitHubStackSignalRecordV1,
    MAX_GITHUB_STACK_DELIVERY_BATCH_V1, RegisteredGlobalDb,
};

use super::registry::DaemonProjectNativeIntegrationService;

const STACK_DELIVERY_STORE_ACTOR_CAPACITY: usize = 64;
const STACK_DELIVERY_STORE_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);
const STACK_DELIVERY_TICK: Duration = Duration::from_millis(250);
const STACK_CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const STACK_CIRCUIT_OPEN_MICROS: i64 = 30_000_000;

type StoreReply<T> = SyncSender<Result<T, String>>;

enum StoreCommand {
    Append(
        Box<GitHubStackSignalRecordV1>,
        Vec<String>,
        StoreReply<GitHubStackSignalAppendOutcomeV1>,
    ),
    Pending(StoreReply<Vec<GitHubStackDeliveryRecordV1>>),
    Publish(String, Vec<GitHubStackDeliveryKeyV1>, StoreReply<()>),
    Acknowledge(String, Vec<GitHubStackDeliveryKeyV1>, StoreReply<()>),
    Signal(String, StoreReply<Option<GitHubStackSignalRecordV1>>),
    RecipientState(
        String,
        String,
        StoreReply<Option<GitHubStackDeliveryStateV1>>,
    ),
    AuthorizationLost(String, String, StoreReply<()>),
    HostAcknowledge(String, String, StoreReply<()>),
    PendingHost(String, StoreReply<Vec<GitHubStackDeliveryRecordV1>>),
}

/// Bounded synchronous view of the canonical registered-project delivery
/// tables.  This actor is not an authority: every command delegates to the
/// single global-db schema/API owner.
struct DaemonStackDeliveryStoreV1 {
    commands: Mutex<Option<SyncSender<StoreCommand>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DaemonStackDeliveryStoreV1 {
    fn open(database: Arc<RegisteredGlobalDb>, project_id: &ProjectId) -> Result<Self, String> {
        let project_id = project_id.as_str().to_owned();
        let (commands, receiver) = sync_channel(STACK_DELIVERY_STORE_ACTOR_CAPACITY);
        let (ready, started) = sync_channel::<Result<(), String>>(1);
        let worker = std::thread::Builder::new()
            .name("tracedecay-github-stack-delivery-store".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| format!("start stack delivery actor: {error}"));
                let Ok(runtime) = runtime else {
                    let _ = ready.send(Err("start stack delivery actor".to_owned()));
                    return;
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                run_delivery_store_actor(&runtime, &database, &project_id, &receiver);
            })
            .map_err(|error| format!("spawn stack delivery actor: {error}"))?;
        match started.recv_timeout(STACK_DELIVERY_STORE_ACTOR_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                commands: Mutex::new(Some(commands)),
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(error)) => {
                drop(commands);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                drop(commands);
                let _ = worker.join();
                Err("start stack delivery actor timed out".to_owned())
            }
        }
    }

    fn submit<T>(&self, make: impl FnOnce(StoreReply<T>) -> StoreCommand) -> Result<T, String> {
        let (reply, receiver) = sync_channel(1);
        self.commands
            .lock()
            .map_err(|_| "stack delivery actor lock is poisoned".to_owned())?
            .as_ref()
            .ok_or_else(|| "stack delivery actor is closed".to_owned())?
            .try_send(make(reply))
            .map_err(|error| match error {
                TrySendError::Full(_) => "stack delivery actor is saturated".to_owned(),
                TrySendError::Disconnected(_) => "stack delivery actor is unavailable".to_owned(),
            })?;
        receiver
            .recv_timeout(STACK_DELIVERY_STORE_ACTOR_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => "stack delivery actor timed out".to_owned(),
                RecvTimeoutError::Disconnected => "stack delivery actor is unavailable".to_owned(),
            })?
    }

    fn append(
        &self,
        record: GitHubStackSignalRecordV1,
        recipients: Vec<String>,
    ) -> Result<GitHubStackSignalAppendOutcomeV1, String> {
        self.submit(|reply| StoreCommand::Append(Box::new(record), recipients, reply))
    }

    fn pending(&self) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        self.submit(StoreCommand::Pending)
    }

    fn publish(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: Vec<GitHubStackDeliveryKeyV1>,
    ) -> Result<(), String> {
        self.submit(|reply| {
            StoreCommand::Publish(watermark_id.as_str().to_owned(), deliveries, reply)
        })
    }

    fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: Vec<GitHubStackDeliveryKeyV1>,
    ) -> Result<(), String> {
        self.submit(|reply| {
            StoreCommand::Acknowledge(watermark_id.as_str().to_owned(), deliveries, reply)
        })
    }

    fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<GitHubStackSignalRecordV1>, String> {
        self.submit(|reply| StoreCommand::Signal(signal_id.as_str().to_owned(), reply))
    }

    fn recipient_state(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<Option<GitHubStackDeliveryStateV1>, String> {
        self.submit(|reply| {
            StoreCommand::RecipientState(
                signal_id.as_str().to_owned(),
                recipient.as_str().to_owned(),
                reply,
            )
        })
    }

    fn authorization_lost(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<(), String> {
        self.submit(|reply| {
            StoreCommand::AuthorizationLost(
                signal_id.as_str().to_owned(),
                recipient.as_str().to_owned(),
                reply,
            )
        })
    }

    fn host_acknowledge(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<(), String> {
        self.submit(|reply| {
            StoreCommand::HostAcknowledge(
                signal_id.as_str().to_owned(),
                recipient.as_str().to_owned(),
                reply,
            )
        })
    }

    fn pending_host(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        self.submit(|reply| {
            StoreCommand::PendingHost(scope.scope_digest.as_str().to_owned(), reply)
        })
    }

    fn shutdown(&self) {
        if let Ok(mut commands) = self.commands.lock() {
            commands.take();
        }
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

impl Drop for DaemonStackDeliveryStoreV1 {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_delivery_store_actor(
    runtime: &tokio::runtime::Runtime,
    database: &RegisteredGlobalDb,
    project_id: &str,
    receiver: &Receiver<StoreCommand>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            StoreCommand::Append(record, recipients, reply) => {
                let _ = reply.send(
                    runtime.block_on(database.append_github_stack_signal(*record, recipients)),
                );
            }
            StoreCommand::Pending(reply) => {
                let _ = reply.send(runtime.block_on(database.pending_github_stack_deliveries(
                    project_id,
                    MAX_GITHUB_STACK_DELIVERY_BATCH_V1,
                )));
            }
            StoreCommand::Publish(watermark, deliveries, reply) => {
                let _ = reply.send(runtime.block_on(database.publish_github_stack_deliveries(
                    project_id,
                    &watermark,
                    &deliveries,
                )));
            }
            StoreCommand::Acknowledge(watermark, deliveries, reply) => {
                let _ = reply.send(
                    runtime.block_on(database.acknowledge_github_stack_deliveries(
                        project_id,
                        &watermark,
                        &deliveries,
                    )),
                );
            }
            StoreCommand::Signal(signal_id, reply) => {
                let _ = reply
                    .send(runtime.block_on(database.github_stack_signal(project_id, &signal_id)));
            }
            StoreCommand::RecipientState(signal_id, recipient, reply) => {
                let _ = reply.send(runtime.block_on(
                    database.github_stack_recipient_state(project_id, &signal_id, &recipient),
                ));
            }
            StoreCommand::AuthorizationLost(signal_id, recipient, reply) => {
                let _ = reply.send(
                    runtime.block_on(database.record_github_stack_authorization_loss(
                        project_id, &signal_id, &recipient,
                    )),
                );
            }
            StoreCommand::HostAcknowledge(signal_id, recipient, reply) => {
                let _ = reply.send(
                    runtime.block_on(database.acknowledge_github_stack_host_delivery(
                        project_id, &signal_id, &recipient,
                    )),
                );
            }
            StoreCommand::PendingHost(scope_digest, reply) => {
                let _ = reply.send(runtime.block_on(
                    database.pending_host_github_stack_deliveries(
                        project_id,
                        &scope_digest,
                        MAX_GITHUB_STACK_DELIVERY_BATCH_V1,
                    ),
                ));
            }
        }
    }
}

#[derive(Clone)]
struct DeliveryAuthorizationEvidenceV1 {
    scope_digest: ManifestDigest,
    expires_at: UtcMicros,
    recipients: BTreeSet<ActorId>,
}

#[derive(Clone)]
struct StackRuntimePortsV1 {
    scope: ResolvedScope,
    access: Arc<Mutex<ProjectSourceAccessSnapshot>>,
    store: Arc<DaemonStackDeliveryStoreV1>,
    native_service: Arc<DaemonProjectNativeIntegrationService>,
    authorizations: Arc<Mutex<BTreeMap<StackSignalId, DeliveryAuthorizationEvidenceV1>>>,
    preflight_outcomes: Arc<Mutex<BTreeMap<ManifestDigest, NativeIntegrationPreflightOutcomeV1>>>,
}

impl StackRuntimePortsV1 {
    fn now() -> Result<UtcMicros, StackCoordinatorErrorV1> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                StackCoordinatorErrorV1::Invalid(format!("read system time: {error}"))
            })?;
        let micros = i64::try_from(duration.as_micros())
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?;
        Ok(UtcMicros(micros))
    }

    fn database_error(error: String) -> StackCoordinatorErrorV1 {
        tracing::warn!(error = %error, "GitHub stack delivery store is unavailable");
        StackCoordinatorErrorV1::Unavailable
    }

    fn validate_scope(&self, signal: &StackSignalV1) -> Result<(), StackCoordinatorErrorV1> {
        self.scope
            .validate()
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?;
        signal.validate(&self.scope)
    }

    fn access_is_current(&self, observed_at: UtcMicros) -> bool {
        self.access
            .lock()
            .is_ok_and(|access| access.scope == self.scope && observed_at < access.grant_expires_at)
    }

    fn replace_access(
        &self,
        access: ProjectSourceAccessSnapshot,
    ) -> Result<(), StackCoordinatorErrorV1> {
        if access.scope != self.scope {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        *self
            .access
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)? = access;
        Ok(())
    }

    fn install_authorization(
        &self,
        signal: &StackSignalV1,
        recipients: BTreeSet<ActorId>,
        expires_at: UtcMicros,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.validate_scope(signal)?;
        let observed_at = Self::now()?;
        if recipients.is_empty()
            || !self.access_is_current(observed_at)
            || observed_at >= expires_at
        {
            return Err(StackCoordinatorErrorV1::Denied);
        }
        let mut authorizations = self
            .authorizations
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?;
        authorizations.retain(|_, evidence| observed_at < evidence.expires_at);
        authorizations.insert(
            signal.signal_id.clone(),
            DeliveryAuthorizationEvidenceV1 {
                scope_digest: self.scope.scope_digest.clone(),
                expires_at,
                recipients,
            },
        );
        Ok(())
    }

    fn authorization_from_context(
        &self,
        signal: &StackSignalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let observed_at = Self::now()?;
        if context.validate().is_err()
            || context.admission_at(observed_at) != RequestAdmission::Admitted
            || context.scope() != &self.scope
            || !self.access_is_current(observed_at)
        {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        self.install_authorization(
            signal,
            BTreeSet::from([context.actor().clone()]),
            context.grant().expires_at,
        )
    }

    fn authorization_from_approval(
        &self,
        signal: &StackSignalV1,
        approval: &NativeIntegrationApprovalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let observed_at = Self::now()?;
        if approval.validate().is_err()
            || context.validate().is_err()
            || context.admission_at(observed_at) != RequestAdmission::Admitted
            || context.scope() != &self.scope
            || context.actor() != &approval.principal
            || context.grant().digest != approval.grant_digest
            || observed_at >= approval.expires_at
            || !self.access_is_current(observed_at)
        {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        let mut recipients = BTreeSet::from([approval.principal.clone()]);
        if let Some(delegate) = &approval.delegated_agent {
            recipients.insert(delegate.clone());
        }
        self.install_authorization(signal, recipients, approval.expires_at)
    }

    fn signal_to_record(
        &self,
        signal: &StackSignalV1,
    ) -> Result<GitHubStackSignalRecordV1, StackCoordinatorErrorV1> {
        self.validate_scope(signal)?;
        let observed_at_micros = signal.observed_at.0;
        if observed_at_micros <= 0 {
            return Err(StackCoordinatorErrorV1::Invalid(
                "stack signal timestamp must be positive".to_owned(),
            ));
        }
        let signal_json = serde_json::to_string(signal).map_err(|error| {
            StackCoordinatorErrorV1::Invalid(format!("encode stack signal: {error}"))
        })?;
        Ok(GitHubStackSignalRecordV1 {
            project_id: self.scope.project_id.as_str().to_owned(),
            signal_id: signal.signal_id.as_str().to_owned(),
            scope_digest: signal.scope_digest.as_str().to_owned(),
            repository_id: signal.repository_id.as_str().to_owned(),
            watermark_id: signal.watermark_id.as_str().to_owned(),
            observed_at_micros,
            signal_json,
        })
    }

    fn record_to_signal(
        &self,
        record: &GitHubStackSignalRecordV1,
    ) -> Result<StackSignalV1, StackCoordinatorErrorV1> {
        if record.project_id != self.scope.project_id.as_str()
            || record.scope_digest != self.scope.scope_digest.as_str()
            || record.repository_id != self.scope.repository_id.as_str()
        {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        let signal =
            serde_json::from_str::<StackSignalV1>(&record.signal_json).map_err(|error| {
                StackCoordinatorErrorV1::Invalid(format!("decode durable stack signal: {error}"))
            })?;
        if record.signal_id != signal.signal_id.as_str()
            || record.watermark_id != signal.watermark_id.as_str()
            || record.observed_at_micros != signal.observed_at.0
        {
            return Err(StackCoordinatorErrorV1::Invalid(
                "durable stack signal identity mismatch".to_owned(),
            ));
        }
        self.validate_scope(&signal)?;
        Ok(signal)
    }

    fn pending_host_for_scope(
        &self,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, StackCoordinatorErrorV1> {
        self.store
            .pending_host(&self.scope)
            .map_err(Self::database_error)
    }
}

impl StackCoordinatorStore for StackRuntimePortsV1 {
    fn append_signal(
        &self,
        signal: StackSignalV1,
        recipients: Vec<ActorId>,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let record = self.signal_to_record(&signal)?;
        let recipient_ids = recipients
            .iter()
            .map(|recipient| recipient.as_str().to_owned())
            .collect::<Vec<_>>();
        match self
            .store
            .append(record, recipient_ids)
            .map_err(Self::database_error)?
        {
            GitHubStackSignalAppendOutcomeV1::Saturated { .. } => {
                Err(StackCoordinatorErrorV1::Saturated)
            }
            GitHubStackSignalAppendOutcomeV1::Appended { .. }
            | GitHubStackSignalAppendOutcomeV1::Replayed { .. } => Ok(()),
        }
    }

    fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorErrorV1> {
        self.store
            .pending()
            .map_err(Self::database_error)?
            .into_iter()
            .map(|delivery| {
                let signal = self.record_to_signal(&delivery.signal)?;
                let recipient = ActorId::new(delivery.recipient).map_err(|error| {
                    StackCoordinatorErrorV1::Invalid(format!("decode stack recipient: {error}"))
                })?;
                Ok((
                    StackPendingDeliveryV1 {
                        recipient,
                        signal_id: signal.signal_id.clone(),
                    },
                    signal,
                ))
            })
            .collect()
    }

    fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: &[StackPendingDeliveryV1],
    ) -> Result<(), StackCoordinatorErrorV1> {
        let keys = deliveries
            .iter()
            .map(|delivery| GitHubStackDeliveryKeyV1 {
                signal_id: delivery.signal_id.as_str().to_owned(),
                recipient: delivery.recipient.as_str().to_owned(),
            })
            .collect();
        self.store
            .acknowledge(watermark_id, keys)
            .map_err(Self::database_error)
    }

    fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
        self.store
            .signal(signal_id)
            .map_err(Self::database_error)?
            .map(|record| self.record_to_signal(&record))
            .transpose()
    }

    fn record_authorization_loss(
        &self,
        signal: &StackSignalV1,
        recipient: &ActorId,
        outcome: StackDeliveryAuthorizationV1,
    ) -> Result<(), StackCoordinatorErrorV1> {
        if !matches!(
            outcome,
            StackDeliveryAuthorizationV1::Denied | StackDeliveryAuthorizationV1::Stale
        ) {
            return Err(StackCoordinatorErrorV1::Invalid(
                "authorization loss must be denied or stale".to_owned(),
            ));
        }
        self.validate_scope(signal)?;
        self.store
            .authorization_lost(&signal.signal_id, recipient)
            .map_err(Self::database_error)
    }
}

impl StackDeliveryAuthorizationPort for StackRuntimePortsV1 {
    fn select_recipients(
        &self,
        scope: &ResolvedScope,
        signal: &StackSignalV1,
    ) -> Result<Vec<ActorId>, StackCoordinatorErrorV1> {
        if scope != &self.scope {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        self.validate_scope(signal)?;
        let observed_at = Self::now()?;
        let evidence = self
            .authorizations
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .get(&signal.signal_id)
            .cloned()
            .ok_or(StackCoordinatorErrorV1::Denied)?;
        if evidence.scope_digest != self.scope.scope_digest
            || !self.access_is_current(observed_at)
            || observed_at >= evidence.expires_at
        {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        Ok(evidence.recipients.into_iter().collect())
    }

    fn authorize(
        &self,
        recipient: &ActorId,
        signal: &StackSignalV1,
    ) -> StackDeliveryAuthorizationV1 {
        if self.validate_scope(signal).is_err() {
            return StackDeliveryAuthorizationV1::Stale;
        }
        let Ok(observed_at) = Self::now() else {
            return StackDeliveryAuthorizationV1::Unavailable;
        };
        if !self.access_is_current(observed_at) {
            return StackDeliveryAuthorizationV1::Stale;
        }
        match self.authorizations.lock() {
            Ok(evidence) => {
                if let Some(evidence) = evidence.get(&signal.signal_id) {
                    if evidence.scope_digest != self.scope.scope_digest
                        || observed_at >= evidence.expires_at
                    {
                        return StackDeliveryAuthorizationV1::Stale;
                    }
                    if evidence.recipients.contains(recipient) {
                        return StackDeliveryAuthorizationV1::Authorized;
                    }
                    return StackDeliveryAuthorizationV1::Denied;
                }
            }
            Err(_) => return StackDeliveryAuthorizationV1::Unavailable,
        }
        match self.store.recipient_state(&signal.signal_id, recipient) {
            Ok(Some(GitHubStackDeliveryStateV1::AuthorizationLost)) | Ok(None) => {
                StackDeliveryAuthorizationV1::Denied
            }
            Ok(Some(_)) => StackDeliveryAuthorizationV1::Authorized,
            Err(_) => StackDeliveryAuthorizationV1::Unavailable,
        }
    }
}

impl StackDeliveryPort for StackRuntimePortsV1 {
    fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorErrorV1> {
        if batch.deliveries.is_empty()
            || batch.deliveries.len() > MAX_GITHUB_STACK_DELIVERY_BATCH_V1
        {
            return Err(StackCoordinatorErrorV1::Invalid(
                "invalid GitHub stack delivery batch".to_owned(),
            ));
        }
        for signal in &batch.signals {
            self.validate_scope(signal)?;
            if signal.watermark_id != batch.watermark_id {
                return Err(StackCoordinatorErrorV1::Invalid(
                    "delivery batch watermark mismatch".to_owned(),
                ));
            }
        }
        let keys = batch
            .deliveries
            .iter()
            .map(|delivery| GitHubStackDeliveryKeyV1 {
                signal_id: delivery.signal_id.as_str().to_owned(),
                recipient: delivery.recipient.as_str().to_owned(),
            })
            .collect();
        self.store
            .publish(&batch.watermark_id, keys)
            .map_err(Self::database_error)
    }
}

impl OptionalStackPreflightPort for StackRuntimePortsV1 {
    fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        let request_digest = canonical_sha256(request)
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?;
        let outcome = self
            .native_service
            .preflight(request.clone(), cancellation)
            .map_err(|error| match error {
                NativeIntegrationContractError::Port(NativeIntegrationPortError::Cancelled) => {
                    StackCoordinatorErrorV1::Cancelled
                }
                NativeIntegrationContractError::Port(NativeIntegrationPortError::Stale) => {
                    StackCoordinatorErrorV1::Stale
                }
                NativeIntegrationContractError::Port(NativeIntegrationPortError::Denied) => {
                    StackCoordinatorErrorV1::Denied
                }
                NativeIntegrationContractError::Port(_) => StackCoordinatorErrorV1::Unavailable,
                NativeIntegrationContractError::Contract(error) => {
                    StackCoordinatorErrorV1::Invalid(error.to_string())
                }
            })?;
        self.preflight_outcomes
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .insert(request_digest, outcome.clone());
        Ok(outcome)
    }
}

/// One project-bound runtime with a bounded drain task, cancelled with its
/// native-integration owner; no global task or fallback route is created.
pub(crate) struct DaemonGitHubStackRuntimeV1 {
    scope: ResolvedScope,
    coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    ports: StackRuntimePortsV1,
    cancellation: tokio_util::sync::CancellationToken,
    tick_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl DaemonGitHubStackRuntimeV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mount(
        project_id: ProjectId,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        database: Arc<RegisteredGlobalDb>,
        coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
        native_service: Arc<DaemonProjectNativeIntegrationService>,
    ) -> Result<Arc<Self>, StackCoordinatorErrorV1> {
        if project_id != scope.project_id || access.scope != scope {
            return Err(StackCoordinatorErrorV1::Stale);
        }
        let policy = StackCircuitPolicyV1 {
            revision: 1,
            policy_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
                .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?,
            failure_threshold: STACK_CIRCUIT_FAILURE_THRESHOLD,
            open_micros: STACK_CIRCUIT_OPEN_MICROS,
        }
        .seal()?;
        coordinator.register_circuit_policy(&scope, policy)?;
        let store = Arc::new(
            DaemonStackDeliveryStoreV1::open(database, &project_id)
                .map_err(StackRuntimePortsV1::database_error)?,
        );
        let ports = StackRuntimePortsV1 {
            scope: scope.clone(),
            access: Arc::new(Mutex::new(access)),
            store,
            native_service,
            authorizations: Arc::new(Mutex::new(BTreeMap::new())),
            preflight_outcomes: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let cancellation = tokio_util::sync::CancellationToken::new();
        let runtime = Arc::new(Self {
            scope,
            coordinator,
            ports: ports.clone(),
            cancellation: cancellation.clone(),
            tick_task: Mutex::new(None),
        });
        let coordinator = Arc::clone(&runtime.coordinator);
        let tick_ports = ports;
        let task = tokio::runtime::Handle::try_current()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .spawn(async move {
                let mut interval = tokio::time::interval(STACK_DELIVERY_TICK);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = cancellation.cancelled() => break,
                        _ = interval.tick() => {
                            let coordinator = Arc::clone(&coordinator);
                            let ports = tick_ports.clone();
                            let drained = tokio::task::spawn_blocking(move || {
                                let now = StackRuntimePortsV1::now()?;
                                coordinator.drain_due(&ports, &ports, &ports, now)
                            }).await;
                            match drained {
                                Ok(Ok(_)) => {},
                                Ok(Err(StackCoordinatorErrorV1::Unavailable)) => {
                                    tracing::debug!("GitHub stack drain deferred because its durable store is unavailable");
                                }
                                Ok(Err(error)) => tracing::warn!(?error, "GitHub stack delivery drain failed"),
                                Err(error) => tracing::warn!(%error, "GitHub stack delivery drain task failed"),
                            }
                        }
                    }
                }
            });
        *runtime
            .tick_task
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)? = Some(task);
        Ok(runtime)
    }

    pub(crate) fn enqueue_from_preflight(
        &self,
        signal: StackSignalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports.authorization_from_context(&signal, context)?;
        self.coordinator
            .enqueue_transition(&self.ports, &self.ports, &self.scope, signal)
    }

    pub(crate) fn refresh_access(
        &self,
        access: ProjectSourceAccessSnapshot,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports.replace_access(access)
    }

    pub(crate) fn enqueue_from_approval(
        &self,
        signal: StackSignalV1,
        approval: &NativeIntegrationApprovalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports
            .authorization_from_approval(&signal, approval, context)?;
        self.coordinator
            .enqueue_transition(&self.ports, &self.ports, &self.scope, signal)
    }

    /// Executes preflight through the coordinator's circuit and bounded
    /// admission path. The port stores the exact outcome produced by that
    /// invocation so callers do not bypass the circuit with a second Git run.
    pub(crate) fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        let request_digest = canonical_sha256(request)
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?;
        let disposition = self.coordinator.optional_preflight(
            &self.ports,
            request,
            cancellation,
            StackRuntimePortsV1::now()?,
        )?;
        let outcome = self
            .ports
            .preflight_outcomes
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .remove(&request_digest);
        match disposition {
            tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Complete
            | tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Partial => {
                outcome.ok_or(StackCoordinatorErrorV1::Unavailable)
            }
            tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::SuppressedOpenCircuit
            | tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Unavailable
            | tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Saturated => {
                Ok(NativeIntegrationPreflightOutcomeV1::Unavailable)
            }
            tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Cancelled => {
                Ok(NativeIntegrationPreflightOutcomeV1::Cancelled)
            }
            tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Stale => {
                Ok(NativeIntegrationPreflightOutcomeV1::Stale)
            }
            tracedecay_usecases::stack_coordinator::OptionalPreflightDispositionV1::Denied => {
                Ok(NativeIntegrationPreflightOutcomeV1::Denied)
            }
        }
    }

    pub(crate) fn pending_host_deliveries(
        &self,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, StackCoordinatorErrorV1> {
        self.ports.pending_host_for_scope()
    }

    fn expand_request(
        &self,
        request: GitHubStackSignalExpandRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<GitHubStackSignalExpandSurfaceResultV1, GitHubStackSignalExpandPortError> {
        let now = StackRuntimePortsV1::now()
            .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        if cancellation.is_cancelled()
            || request.context().admission_at(now) == RequestAdmission::Cancelled
        {
            return Err(GitHubStackSignalExpandPortError::Cancelled);
        }
        if request.context().validate().is_err() {
            return Err(GitHubStackSignalExpandPortError::Concealed);
        }
        if request.context().admission_at(now) != RequestAdmission::Admitted {
            return Err(GitHubStackSignalExpandPortError::Stale);
        }
        let operation = git_surface_operation(GITHUB_STACK_SIGNAL_EXPAND_OPERATION)
            .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?
            .ok_or(GitHubStackSignalExpandPortError::Unavailable)?;
        if !request
            .context()
            .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(GitHubStackSignalExpandPortError::Concealed);
        }
        if request.context().scope() != &self.scope || !self.ports.access_is_current(now) {
            return Err(GitHubStackSignalExpandPortError::Stale);
        }
        let signal = self
            .coordinator
            .expand_transition(
                &self.ports,
                &self.ports,
                request.context().actor(),
                request.signal_id(),
            )
            .map_err(map_expand_error)?
            .ok_or(GitHubStackSignalExpandPortError::Concealed)?;
        if request
            .expected_watermark_id()
            .is_some_and(|expected| expected != &signal.watermark_id)
        {
            return Err(GitHubStackSignalExpandPortError::Stale);
        }
        self.ports
            .store
            .host_acknowledge(&signal.signal_id, request.context().actor())
            .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        let evidence = GitHubStackSignalEvidenceRefV1::new(
            signal.signal_id,
            signal.watermark_id,
            signal.stack_revision_digest,
            signal.state_digest,
            signal.github_stack_digest,
            signal.observed_at,
        )
        .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        Ok(GitHubStackSignalExpandSurfaceResultV1::Expanded { evidence })
    }
}

impl GitHubStackSignalExpandPort for DaemonGitHubStackRuntimeV1 {
    fn expand(
        &self,
        request: GitHubStackSignalExpandRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<GitHubStackSignalExpandSurfaceResultV1, GitHubStackSignalExpandPortError> {
        self.expand_request(request, cancellation)
    }
}

impl Drop for DaemonGitHubStackRuntimeV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut task) = self.tick_task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

fn map_expand_error(error: StackCoordinatorErrorV1) -> GitHubStackSignalExpandPortError {
    match error {
        StackCoordinatorErrorV1::Denied => GitHubStackSignalExpandPortError::Concealed,
        StackCoordinatorErrorV1::Stale => GitHubStackSignalExpandPortError::Stale,
        StackCoordinatorErrorV1::Cancelled => GitHubStackSignalExpandPortError::Cancelled,
        StackCoordinatorErrorV1::Unavailable
        | StackCoordinatorErrorV1::Saturated
        | StackCoordinatorErrorV1::Invalid(_) => GitHubStackSignalExpandPortError::Unavailable,
    }
}
