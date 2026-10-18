//! Daemon-local runtime for durable GitHub stack transition delivery.
//!
//! The coordinator awaits the registered database directly through the
//! native-integration project owner's delivery store. Publication advances a durable row only to `host_pending`; authenticated
//! MCP expansion is the sole settlement path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_application::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, OptionalStackPreflightPort, StackCircuitPolicyV1,
    StackCoordinatorErrorV1, StackCoordinatorStore, StackDeliveryAuthorizationPort,
    StackDeliveryAuthorizationV1, StackDeliveryBatchV1, StackDeliveryPort, StackPendingDeliveryV1,
    StackSignalV1,
};
use tracedecay_contracts::git::{
    GITHUB_STACK_SIGNAL_EXPAND_OPERATION, GitHubStackSignalEvidenceRefV1,
    GitHubStackSignalExpandPort, GitHubStackSignalExpandPortError,
    GitHubStackSignalExpandRequestV1, GitHubStackSignalExpandSurfaceResultV1,
    GitHubStackSignalNativePreviewV1, GitHubStackSignalNativeSourceV1,
    GitHubStackSignalNativeTerminalV1, git_surface_operation,
};
use tracedecay_contracts::{
    CancellationSignal, NativeIntegrationContractError, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1, RequestAdmission,
    RequestContext, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, NativeIntegrationApprovalV1, NativeIntegrationSelectionV1,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationUnavailabilityV1, ProjectId,
    StackDeliveryWatermarkId, StackSignalId, StackSignalKindV1, UtcMicros, canonical_sha256,
};

use tracedecay_global_db::{
    GitHubStackDeliveryKeyV1, GitHubStackDeliveryRecordV1, GitHubStackDeliveryStateV1,
    GitHubStackSignalAppendOutcomeV1, GitHubStackSignalRecordV1,
    MAX_GITHUB_STACK_DELIVERY_BATCH_V1, RegisteredGlobalDbLeaseV1,
};
use tracedecay_runtime_core::native_runtime::{self, NativeTask};

use super::registry::DaemonProjectNativeIntegrationService;
use super::store::DaemonNativeIntegrationStore;

const STACK_DELIVERY_TICK: Duration = Duration::from_millis(250);
const STACK_CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const STACK_CIRCUIT_OPEN_MICROS: i64 = 30_000_000;

/// One project's view of the canonical registered-project delivery tables.
/// It is not an authority: every call awaits the single global-db
/// schema/API owner, where each mutation is one write transaction.
struct DaemonStackDeliveryStoreV1 {
    database: RegisteredGlobalDbLeaseV1,
    project_id: String,
}

impl DaemonStackDeliveryStoreV1 {
    async fn append(
        &self,
        record: GitHubStackSignalRecordV1,
        recipients: Vec<String>,
    ) -> Result<GitHubStackSignalAppendOutcomeV1, String> {
        self.database
            .append_github_stack_signal(record, recipients)
            .await
    }

    async fn pending(&self) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        self.database
            .pending_github_stack_deliveries(&self.project_id, MAX_GITHUB_STACK_DELIVERY_BATCH_V1)
            .await
    }

    async fn publish(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: Vec<GitHubStackDeliveryKeyV1>,
    ) -> Result<(), String> {
        self.database
            .publish_github_stack_deliveries(&self.project_id, watermark_id.as_str(), &deliveries)
            .await
    }

    async fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: Vec<GitHubStackDeliveryKeyV1>,
    ) -> Result<(), String> {
        self.database
            .acknowledge_github_stack_deliveries(
                &self.project_id,
                watermark_id.as_str(),
                &deliveries,
            )
            .await
    }

    async fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<GitHubStackSignalRecordV1>, String> {
        self.database
            .github_stack_signal(&self.project_id, signal_id.as_str())
            .await
    }

    async fn oldest_pending(
        &self,
        scope: &ResolvedScope,
        recipient: &ActorId,
    ) -> Result<Option<GitHubStackDeliveryRecordV1>, String> {
        self.database
            .oldest_host_pending_github_stack_delivery(
                &self.project_id,
                scope.scope_digest.as_str(),
                recipient.as_str(),
            )
            .await
    }

    async fn recipient_state(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<Option<GitHubStackDeliveryStateV1>, String> {
        self.database
            .github_stack_recipient_state(&self.project_id, signal_id.as_str(), recipient.as_str())
            .await
    }

    async fn authorization_lost(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<(), String> {
        self.database
            .record_github_stack_authorization_loss(
                &self.project_id,
                signal_id.as_str(),
                recipient.as_str(),
            )
            .await
    }

    async fn host_acknowledge(
        &self,
        signal_id: &StackSignalId,
        recipient: &ActorId,
    ) -> Result<(), String> {
        self.database
            .acknowledge_github_stack_host_delivery(
                &self.project_id,
                signal_id.as_str(),
                recipient.as_str(),
            )
            .await
    }

    async fn pending_host(
        &self,
        scope: &ResolvedScope,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, String> {
        self.database
            .pending_host_github_stack_deliveries(
                &self.project_id,
                scope.scope_digest.as_str(),
                MAX_GITHUB_STACK_DELIVERY_BATCH_V1,
            )
            .await
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
    native_store: DaemonNativeIntegrationStore,
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

    async fn pending_host_for_scope(
        &self,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, StackCoordinatorErrorV1> {
        self.store
            .pending_host(&self.scope)
            .await
            .map_err(Self::database_error)
    }
}

impl StackCoordinatorStore for StackRuntimePortsV1 {
    async fn append_signal(
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
            .await
            .map_err(Self::database_error)?
        {
            GitHubStackSignalAppendOutcomeV1::Saturated { .. } => {
                Err(StackCoordinatorErrorV1::Saturated)
            }
            GitHubStackSignalAppendOutcomeV1::Appended { .. }
            | GitHubStackSignalAppendOutcomeV1::Replayed { .. } => Ok(()),
        }
    }

    async fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorErrorV1> {
        self.store
            .pending()
            .await
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

    async fn acknowledge(
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
            .await
            .map_err(Self::database_error)
    }

    async fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
        self.store
            .signal(signal_id)
            .await
            .map_err(Self::database_error)?
            .map(|record| self.record_to_signal(&record))
            .transpose()
    }

    async fn record_authorization_loss(
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
            .await
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

    async fn authorize(
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
        match self
            .store
            .recipient_state(&signal.signal_id, recipient)
            .await
        {
            Ok(Some(GitHubStackDeliveryStateV1::AuthorizationLost) | None) => {
                StackDeliveryAuthorizationV1::Denied
            }
            Ok(Some(_)) => StackDeliveryAuthorizationV1::Authorized,
            Err(_) => StackDeliveryAuthorizationV1::Unavailable,
        }
    }
}

impl StackDeliveryPort for StackRuntimePortsV1 {
    #[tracing::instrument(
        name = "daemon.native_integration.stack_deliver",
        level = "trace",
        skip_all
    )]
    async fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorErrorV1> {
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
            .await
            .map_err(Self::database_error)
    }
}

fn map_native_preflight_error(error: NativeIntegrationContractError) -> StackCoordinatorErrorV1 {
    match error {
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
    }
}

impl OptionalStackPreflightPort for StackRuntimePortsV1 {
    async fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        let request_digest = canonical_sha256(request)
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?;
        let outcome = self
            .native_service
            .preflight(request.clone(), cancellation)
            .await
            .map_err(map_native_preflight_error)?;
        self.preflight_outcomes
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .insert(request_digest, outcome.clone());
        Ok(outcome)
    }
}

/// One project-bound runtime with a bounded drain task, cancelled with its
/// native-integration owner; no global task or fallback route is created.
pub struct DaemonGitHubStackRuntimeV1 {
    scope: ResolvedScope,
    coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    ports: StackRuntimePortsV1,
    tick_task: NativeTask<()>,
}

/// Drains due deliveries every [`STACK_DELIVERY_TICK`] after the previous
/// drain finishes, so a slow drain never runs back to back. Ends only when
/// its task is aborted or no native timer starts.
async fn drain_loop(coordinator: Arc<DaemonGitHubStackCoordinatorV1>, ports: StackRuntimePortsV1) {
    loop {
        let drained = match StackRuntimePortsV1::now() {
            Ok(now) => coordinator.drain_due(&ports, &ports, &ports, now).await,
            Err(error) => Err(error),
        };
        match drained {
            Ok(_) => {}
            Err(StackCoordinatorErrorV1::Unavailable) => {
                tracing::debug!(
                    "GitHub stack drain deferred because its durable store is unavailable"
                );
            }
            Err(error) => tracing::warn!(?error, "GitHub stack delivery drain failed"),
        }
        if let Err(error) = native_runtime::sleep_until(Instant::now() + STACK_DELIVERY_TICK).await
        {
            tracing::warn!(%error, "GitHub stack delivery drain stopped without a timer");
            return;
        }
    }
}

// A `BTreeMap` of these runtimes proves the native service before `Send`
// bottoms out (rust-lang/rust#159228). Each field is checked on its own.
unsafe impl Send for DaemonGitHubStackRuntimeV1 {}
unsafe impl Sync for DaemonGitHubStackRuntimeV1 {}

const _: fn(&DaemonGitHubStackRuntimeV1) = |value| {
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    let DaemonGitHubStackRuntimeV1 {
        scope,
        coordinator,
        ports,
        tick_task,
    } = value;
    assert_send_sync(scope);
    assert_send_sync(coordinator);
    assert_send_sync(ports);
    assert_send_sync(tick_task);
};

impl DaemonGitHubStackRuntimeV1 {
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(
        name = "daemon.native_integration.stack_mount",
        level = "trace",
        skip_all
    )]
    pub fn mount(
        project_id: ProjectId,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
        database: RegisteredGlobalDbLeaseV1,
        coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
        native_service: Arc<DaemonProjectNativeIntegrationService>,
        native_store: DaemonNativeIntegrationStore,
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
        let store = Arc::new(DaemonStackDeliveryStoreV1 {
            database,
            project_id: project_id.as_str().to_owned(),
        });
        let ports = StackRuntimePortsV1 {
            scope: scope.clone(),
            access: Arc::new(Mutex::new(access)),
            store,
            native_service,
            native_store,
            authorizations: Arc::new(Mutex::new(BTreeMap::new())),
            preflight_outcomes: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let tick_task = NativeTask::spawn(drain_loop(Arc::clone(&coordinator), ports.clone()))
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?;
        Ok(Arc::new(Self {
            scope,
            coordinator,
            ports,
            tick_task,
        }))
    }

    pub async fn enqueue_from_preflight(
        &self,
        signal: StackSignalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports.authorization_from_context(&signal, context)?;
        self.coordinator
            .enqueue_transition(&self.ports, &self.ports, &self.scope, signal)
            .await
    }

    pub fn refresh_access(
        &self,
        access: ProjectSourceAccessSnapshot,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports.replace_access(access)
    }

    pub async fn enqueue_from_approval(
        &self,
        signal: StackSignalV1,
        approval: &NativeIntegrationApprovalV1,
        context: &RequestContext,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.ports
            .authorization_from_approval(&signal, approval, context)?;
        self.coordinator
            .enqueue_transition(&self.ports, &self.ports, &self.scope, signal)
            .await
    }

    /// Executes preflight through the coordinator's circuit and bounded
    /// admission path. The port stores the exact outcome produced by that
    /// invocation so callers do not bypass the circuit with a second Git run.
    pub async fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        self.preflight_via_circuit(request, cancellation).await
    }

    async fn preflight_via_circuit(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        if matches!(
            request.topology.selection,
            tracedecay_contracts::NativeIntegrationSelectionBindingV1::IndependentBranch { .. }
        ) {
            return self
                .ports
                .native_service
                .preflight(request.clone(), cancellation)
                .await
                .map_err(map_native_preflight_error);
        }
        let request_digest = canonical_sha256(request)
            .map_err(|error| StackCoordinatorErrorV1::Invalid(error.to_string()))?;
        let disposition = self
            .coordinator
            .optional_preflight(
                &self.ports,
                request,
                cancellation,
                StackRuntimePortsV1::now()?,
            )
            .await?;
        let outcome = self
            .ports
            .preflight_outcomes
            .lock()
            .map_err(|_| StackCoordinatorErrorV1::Unavailable)?
            .remove(&request_digest);
        match disposition {
            tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Complete
            | tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Partial => {
                outcome.ok_or(StackCoordinatorErrorV1::Unavailable)
            }
            tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::SuppressedOpenCircuit
            | tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Unavailable
            | tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Saturated => {
                Ok(NativeIntegrationPreflightOutcomeV1::Unavailable)
            }
            tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Cancelled => {
                Ok(NativeIntegrationPreflightOutcomeV1::Cancelled)
            }
            tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Stale => {
                Ok(NativeIntegrationPreflightOutcomeV1::Stale)
            }
            tracedecay_application::stack_coordinator::OptionalPreflightDispositionV1::Denied => {
                Ok(NativeIntegrationPreflightOutcomeV1::Denied)
            }
        }
    }

    pub async fn pending_host_deliveries(
        &self,
    ) -> Result<Vec<GitHubStackDeliveryRecordV1>, StackCoordinatorErrorV1> {
        self.ports.pending_host_for_scope().await
    }

    async fn expand_request(
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
        let selected_signal_id = match request.signal_id() {
            Some(signal_id) => signal_id.clone(),
            None => self
                .ports
                .store
                .oldest_pending(&self.scope, request.context().actor())
                .await
                .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?
                .map(|delivery| StackSignalId::new(delivery.signal.signal_id))
                .transpose()
                .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?
                .ok_or(GitHubStackSignalExpandPortError::Concealed)?,
        };
        let signal = self
            .coordinator
            .expand_transition(
                &self.ports,
                &self.ports,
                request.context().actor(),
                &selected_signal_id,
            )
            .await
            .map_err(map_expand_error)?
            .ok_or(GitHubStackSignalExpandPortError::Concealed)?;
        if request
            .expected_watermark_id()
            .is_some_and(|expected| expected != &signal.watermark_id)
        {
            return Err(GitHubStackSignalExpandPortError::Stale);
        }
        let preview_by_state = self
            .ports
            .native_store
            .read_preview_by_digest(&signal.state_digest)
            .await
            .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        let (preview, terminal) = match preview_by_state {
            Some(preview) => (preview, None),
            None => {
                let receipt = self
                    .ports
                    .native_store
                    .read_receipt_by_digest(&signal.state_digest)
                    .await
                    .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?
                    .ok_or(GitHubStackSignalExpandPortError::NativeEvidenceUnavailable)?;
                let preview = self
                    .ports
                    .native_store
                    .read_preview_by_digest(&receipt.status.preview_digest)
                    .await
                    .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?
                    .ok_or(GitHubStackSignalExpandPortError::NativeEvidenceUnavailable)?;
                if preview.preview_id != receipt.status.preview_id
                    || preview.preview_digest != receipt.status.preview_digest
                    || receipt.receipt_digest != signal.state_digest
                {
                    return Err(GitHubStackSignalExpandPortError::Stale);
                }
                (preview, Some(receipt))
            }
        };
        let NativeIntegrationSelectionV1::DeclaredStackEdge(selection) = &preview.selection else {
            return Err(GitHubStackSignalExpandPortError::NativeEvidenceUnavailable);
        };
        if selection.revision.revision_id != signal.stack_revision_id
            || selection.revision.digest != signal.stack_revision_digest
            || preview.repository_snapshot.repository_id != signal.repository_id
            || preview.repository_snapshot.project_id != self.scope.project_id
            || preview.repository_snapshot.destination_ref
                != *preview
                    .selection
                    .destination_ref()
                    .map_err(|_| GitHubStackSignalExpandPortError::Stale)?
            || self.scope.reference.as_ref() != Some(&preview.repository_snapshot.destination_ref)
        {
            return Err(GitHubStackSignalExpandPortError::Stale);
        }
        let native_preview = GitHubStackSignalNativePreviewV1 {
            preview_id: preview.preview_id.clone(),
            preview_digest: preview.preview_digest.clone(),
            direction: selection.direction,
            source_ref: preview.repository_snapshot.source_ref.clone(),
            destination_ref: preview.repository_snapshot.destination_ref.clone(),
            source_tip: preview.repository_snapshot.source_tip.clone(),
            destination_tip: preview.repository_snapshot.destination_tip.clone(),
            disposition: preview.disposition.clone(),
        };
        let native_source = match terminal {
            None => {
                if preview.preview_digest != signal.state_digest
                    || !matches!(
                        (signal.kind, &preview.disposition),
                        (
                            StackSignalKindV1::DependencyReady,
                            tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
                        ) | (
                            StackSignalKindV1::DependencyReady,
                            tracedecay_domain::NativeIntegrationPreviewDispositionV1::Partial {
                                reason: NativeIntegrationUnavailabilityV1::DestinationOccupied,
                            }
                        ) | (
                            StackSignalKindV1::ActualConflict,
                            tracedecay_domain::NativeIntegrationPreviewDispositionV1::NativeConflict { .. }
                        )
                    )
                {
                    return Err(GitHubStackSignalExpandPortError::Stale);
                }
                GitHubStackSignalNativeSourceV1::Preflight {
                    preview: native_preview,
                }
            }
            Some(receipt) => {
                let outcome = receipt
                    .status
                    .terminal_outcome
                    .ok_or(GitHubStackSignalExpandPortError::Stale)?;
                if !matches!(
                    (signal.kind, outcome),
                    (
                        StackSignalKindV1::IntegrationCommitted,
                        NativeIntegrationTerminalOutcomeV1::Committed
                    ) | (
                        StackSignalKindV1::IntegrationNeedsInspection,
                        NativeIntegrationTerminalOutcomeV1::NeedsInspection
                    )
                ) {
                    return Err(GitHubStackSignalExpandPortError::Stale);
                }
                GitHubStackSignalNativeSourceV1::Terminal {
                    preview: native_preview,
                    terminal: GitHubStackSignalNativeTerminalV1 {
                        transaction_id: receipt.status.transaction_id,
                        receipt_digest: receipt.receipt_digest,
                        outcome,
                        final_ref_tip: receipt.final_ref_tip,
                        completed_at: receipt.completed_at,
                    },
                }
            }
        };
        let evidence = GitHubStackSignalEvidenceRefV1::new(
            signal.signal_id.clone(),
            signal.watermark_id,
            signal.kind,
            signal.stack_revision_id,
            signal.stack_revision_digest,
            signal.state_digest,
            signal.github_stack_digest,
            signal.observed_at,
            native_source,
        )
        .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        self.ports
            .store
            .host_acknowledge(&signal.signal_id, request.context().actor())
            .await
            .map_err(|_| GitHubStackSignalExpandPortError::Unavailable)?;
        Ok(GitHubStackSignalExpandSurfaceResultV1::Expanded {
            evidence: Box::new(evidence),
        })
    }
}

impl GitHubStackSignalExpandPort for DaemonGitHubStackRuntimeV1 {
    async fn expand(
        &self,
        request: GitHubStackSignalExpandRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<GitHubStackSignalExpandSurfaceResultV1, GitHubStackSignalExpandPortError> {
        self.expand_request(request, cancellation).await
    }
}

impl Drop for DaemonGitHubStackRuntimeV1 {
    fn drop(&mut self) {
        self.tick_task.abort();
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

#[cfg(test)]
mod tests {
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::native_runtime::NativeTaskOutcome;

    use super::*;

    #[tokio::test]
    async fn delivery_store_awaited_on_a_native_worker_never_parks_it() {
        let database = RegisteredGlobalDbHarness::open("github-stack-native-worker").await;
        let store = DaemonStackDeliveryStoreV1 {
            database: database.registered.clone(),
            project_id: "project.stack-native-worker".to_owned(),
        };
        let writer_waits = native_runtime::native_thread_writer_waits();
        let reader_waits = native_runtime::native_thread_reader_waits();

        let task = NativeTask::spawn(async move {
            assert!(
                native_runtime::Runtime::current_handle().is_some(),
                "the store must be awaited on a native worker"
            );
            store
                .append(
                    GitHubStackSignalRecordV1 {
                        project_id: store.project_id.clone(),
                        signal_id: "signal.stack-native-worker".to_owned(),
                        scope_digest: "scope.stack-native-worker".to_owned(),
                        repository_id: "repository.stack-native-worker".to_owned(),
                        watermark_id: "watermark.stack-native-worker".to_owned(),
                        observed_at_micros: 1,
                        signal_json: "{}".to_owned(),
                    },
                    vec!["actor.stack-native-worker".to_owned()],
                )
                .await?;
            store.pending().await
        })
        .expect("native task admitted");

        let NativeTaskOutcome::Completed(pending) = task.wait().await else {
            panic!("store task did not complete");
        };
        let pending = pending.expect("store calls settle on a native worker");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].recipient, "actor.stack-native-worker");
        assert_eq!(
            native_runtime::native_thread_writer_waits(),
            writer_waits,
            "no writer wait parked a native worker"
        );
        assert_eq!(
            native_runtime::native_thread_reader_waits(),
            reader_waits,
            "no reader wait parked a native worker"
        );
    }
}
