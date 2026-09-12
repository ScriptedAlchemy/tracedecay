use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, RwLock};
use tracedecay_automation_runtime::automation::config::AutomationConfig;
use tracedecay_contracts::RequestContext;
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutClaimHandleV1, ContextScoutClaimRequestV1,
    ContextScoutClaimWindowV1, ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1,
    ContextScoutDurableClaimV1, ContextScoutDurableQueueEntryV1, ContextScoutFeedbackV1,
    ContextScoutLeaseV1, ContextScoutModelBackendV1, ContextScoutModelOutcomeV1,
    ContextScoutWorkV1,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookLifecyclePhaseV1, HookReadyGuidanceV1,
};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::model::context_scout_model_assistant_from_project_config;
use super::ports::{
    AdmittedContextScoutHookV1, ContextScoutAuthorityPinV1, ContextScoutConfigurationPinV1,
    ContextScoutLifecycleAddressV1, ProjectContextScoutAddressRegistryV1,
};
use super::{
    ContextScoutBudgetStateV1, ContextScoutCapabilityStateV1, ContextScoutControlV1,
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableRuntimeV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutErrorV1, ContextScoutExplanationV1,
    ContextScoutModelAssistantV1, ContextScoutModelErrorV1, ContextScoutModelExecutionV1,
    ContextScoutModelFuture, ContextScoutModelRequestV1, ContextScoutMutationBindingV1,
    ContextScoutMutationSettlementOutcomeV1, ContextScoutPublicMutationV1,
    ContextScoutRecentReadOutcomeV1, ContextScoutRecentStateV1, ContextScoutRuntimeOutcomeV1,
    ContextScoutSelectionInputV1, ContextScoutServiceStateV1, ContextScoutStatusV1,
    ProjectContextScoutDurableStoreV1,
};
use tracedecay_runtime_core::db::Database;

const STARTUP_RECOVERY_LIMIT: usize = 32;
const DELIVERY_LEASE_MICROS: i64 = 30 * 1_000_000;
const MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES: usize = 256;

/// Typed admission for one hook-claim authority on a project owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextScoutClaimAdmissionV1 {
    /// A new lifecycle was stored under the cap.
    Mounted,
    /// The same lifecycle replaced its previous mount.
    Replaced,
    /// The 256-claim cap is full and this lifecycle is new.
    DeniedAtCapacity,
    /// The claim was never admissible (zero watermark).
    Rejected,
}

#[derive(Clone)]
struct MountedContextScoutClaimV1 {
    registry: Arc<ProjectContextScoutAddressRegistryV1>,
    pin: ContextScoutAuthorityPinV1,
    context: RequestContext,
    lifecycle: ContextScoutLifecycleAddressV1,
    address: ContextScoutAddressV1,
    input_watermark: [u8; 32],
}

pub(crate) type ProjectScoutRuntime = ContextScoutDurableRuntimeV1<
    Arc<ProjectContextScoutDurableStoreV1>,
    Arc<dyn ContextScoutModelAssistantV1>,
>;

type ProjectContextScoutOwnerRegistry = BTreeMap<[u8; 16], Arc<ProjectContextScoutOwnerV1>>;

pub struct ProjectContextScoutOwnerV1 {
    store: Arc<ProjectContextScoutDurableStoreV1>,
    runtime: Mutex<ProjectScoutRuntime>,
    configuration: RwLock<Option<ContextScoutConfigurationPinV1>>,
    inflight: StdMutex<BTreeMap<ContextScoutAddressV1, (u64, CancellationToken)>>,
    next_inflight_id: AtomicU64,
    startup: ContextScoutDurableStartupOutcomeV1,
    claim_authorities: RwLock<Vec<MountedContextScoutClaimV1>>,
}

fn registered_context_scout_owners() -> &'static StdMutex<ProjectContextScoutOwnerRegistry> {
    static OWNERS: OnceLock<StdMutex<ProjectContextScoutOwnerRegistry>> = OnceLock::new();
    OWNERS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub fn lookup_registered_context_scout_owners(
    project_id: [u8; 16],
) -> Vec<Arc<ProjectContextScoutOwnerV1>> {
    let Ok(owners) = registered_context_scout_owners().lock() else {
        return Vec::new();
    };
    owners
        .get(&project_id)
        .cloned()
        .map(|owner| vec![owner])
        .unwrap_or_default()
}

/// What one conditional unregister did to the project's owner slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextScoutOwnerUnregisterOutcomeV1 {
    /// The slot held the owner bound to `database_path` and it was removed.
    Removed,
    /// No owner is registered for the project.
    Vacant,
    /// The slot holds an owner bound to a different database identity (a
    /// replacement that outlived the retiring runtime); it was left in place.
    StaleIdentity,
}

/// Drops the process-global owner for one project identity, but only when the
/// registered owner still binds `database_path`. Retirement of a runtime whose
/// owner was already replaced by a newer database identity is a typed no-op
/// rather than a removal of the live replacement. Project close and isolated
/// tests call this; branch reopen must not.
pub fn unregister_registered_context_scout_owner(
    project_id: [u8; 16],
    database_path: &std::path::Path,
) -> ContextScoutOwnerUnregisterOutcomeV1 {
    let Ok(mut owners) = registered_context_scout_owners().lock() else {
        return ContextScoutOwnerUnregisterOutcomeV1::Vacant;
    };
    let Some(existing) = owners.get(&project_id) else {
        return ContextScoutOwnerUnregisterOutcomeV1::Vacant;
    };
    if !tracedecay_runtime_core::path_safety::same_canonical_path(
        existing.store.database().canonical_database_path(),
        database_path,
    ) {
        return ContextScoutOwnerUnregisterOutcomeV1::StaleIdentity;
    }
    owners.remove(&project_id);
    ContextScoutOwnerUnregisterOutcomeV1::Removed
}

impl ProjectContextScoutOwnerV1 {
    pub async fn startup_configured(
        database: Database,
        project_id: [u8; 16],
        now: UtcMicros,
        pin: ContextScoutConfigurationPinV1,
        model_config: Option<&AutomationConfig>,
    ) -> Option<Arc<Self>> {
        let owner = Self::startup(database, project_id, now, model_config).await?;
        owner.install_configuration(pin, model_config).await.ok()?;
        Some(owner)
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.startup",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn startup(
        database: Database,
        project_id: [u8; 16],
        now: UtcMicros,
        model_config: Option<&AutomationConfig>,
    ) -> Option<Arc<Self>> {
        if let Some(existing) = lookup_registered_context_scout_owners(project_id)
            .into_iter()
            .next()
            && existing.binds_database(&database)
        {
            return Some(existing);
        }
        let (store, startup) = ProjectContextScoutDurableStoreV1::startup_from_project_database(
            database,
            project_id,
            now,
            STARTUP_RECOVERY_LIMIT,
        )
        .await?;
        let model = context_scout_model_assistant_from_project_config(model_config);
        let mut runtime = ContextScoutDurableRuntimeV1::new(Arc::clone(&store), model);
        let work_snapshot = store.work_snapshot(now, STARTUP_RECOVERY_LIMIT).await;
        runtime.restore_startup(&work_snapshot).ok()?;
        let owner = Arc::new(Self {
            store,
            runtime: Mutex::new(runtime),
            configuration: RwLock::new(None),
            inflight: StdMutex::new(BTreeMap::new()),
            next_inflight_id: AtomicU64::new(1),
            startup,
            claim_authorities: RwLock::new(Vec::new()),
        });
        let mut owners = registered_context_scout_owners().lock().ok()?;
        if let Some(existing) = owners.get(&project_id)
            && existing.binds_database(owner.store.database())
        {
            return Some(Arc::clone(existing));
        }
        owners.insert(project_id, Arc::clone(&owner));
        Some(owner)
    }

    fn binds_database(&self, database: &Database) -> bool {
        self.store.database().canonical_database_path() == database.canonical_database_path()
    }

    pub fn store(&self) -> Arc<ProjectContextScoutDurableStoreV1> {
        Arc::clone(&self.store)
    }

    /// Publishes one hook-admissible claim authority after the durable
    /// address registry and the current configuration have been checked.
    #[allow(clippy::too_many_arguments)]
    pub async fn mount_current_claim_authority(
        &self,
        registry: Arc<ProjectContextScoutAddressRegistryV1>,
        hook: &AdmittedContextScoutHookV1,
        pin: ContextScoutAuthorityPinV1,
        context: RequestContext,
        lifecycle: ContextScoutLifecycleAddressV1,
        address: ContextScoutAddressV1,
        input_watermark: [u8; 32],
        observed_at: UtcMicros,
        configuration_is_current: bool,
    ) -> ContextScoutClaimAdmissionV1 {
        if input_watermark == [0; 32]
            || !configuration_is_current
            || registry
                .resolve_current_exact(hook, &pin, &lifecycle, &context, observed_at)
                .await
                != super::ports::ContextScoutAddressResolveOutcomeV1::Resolved(address)
        {
            return ContextScoutClaimAdmissionV1::Rejected;
        }
        let mut authorities = self.claim_authorities.write().await;
        if let Some(existing) = authorities
            .iter_mut()
            .find(|existing| existing.lifecycle == lifecycle)
        {
            existing.registry = registry;
            existing.pin = pin;
            existing.context = context;
            existing.address = address;
            existing.input_watermark = input_watermark;
            return ContextScoutClaimAdmissionV1::Replaced;
        }
        if authorities.len() >= MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES {
            return ContextScoutClaimAdmissionV1::DeniedAtCapacity;
        }
        authorities.push(MountedContextScoutClaimV1 {
            registry,
            pin,
            context,
            lifecycle,
            address,
            input_watermark,
        });
        ContextScoutClaimAdmissionV1::Mounted
    }

    pub async fn resolve_current_claim_authority(
        &self,
        hook: &AdmittedContextScoutHookV1,
        lifecycle: &ContextScoutLifecycleAddressV1,
        observed_at: UtcMicros,
        configuration_is_current: bool,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        let mounted = self
            .claim_authorities
            .read()
            .await
            .iter()
            .find(|mounted| mounted.lifecycle == *lifecycle)
            .cloned()?;
        if !configuration_is_current {
            return None;
        }
        let resolved = mounted
            .registry
            .resolve_current_exact(hook, &mounted.pin, lifecycle, &mounted.context, observed_at)
            .await;
        (resolved == super::ports::ContextScoutAddressResolveOutcomeV1::Resolved(mounted.address))
            .then_some((mounted.address, mounted.input_watermark))
    }

    pub async fn mounted_claim_pin(
        &self,
        lifecycle: &ContextScoutLifecycleAddressV1,
    ) -> Option<ContextScoutAuthorityPinV1> {
        self.claim_authorities
            .read()
            .await
            .iter()
            .find(|mounted| mounted.lifecycle == *lifecycle)
            .map(|mounted| mounted.pin.clone())
    }

    pub async fn resolve_admitted_claim(
        &self,
        lifecycle: &ContextScoutLifecycleAddressV1,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        self.claim_authorities
            .read()
            .await
            .iter()
            .find(|mounted| mounted.lifecycle == *lifecycle)
            .map(|mounted| (mounted.address, mounted.input_watermark))
    }

    pub fn startup_outcome(&self) -> &ContextScoutDurableStartupOutcomeV1 {
        &self.startup
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.claim_ready",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn claim_ready_guidance(
        &self,
        hook: &HookEventEnvelopeV2,
        configuration_revision: u64,
        now: UtcMicros,
    ) -> Option<(HookReadyGuidanceV1, ContextScoutDurableClaimV1)> {
        let configuration = self.configuration.read().await;
        let control = configuration.as_ref()?.control();
        let ready = self.store.startup(now, STARTUP_RECOVERY_LIMIT).await;
        let entries = match ready {
            ContextScoutDurableStartupOutcomeV1::Ready { entries, .. } => entries,
            ContextScoutDurableStartupOutcomeV1::Unavailable => return None,
        };
        let mut matching = entries.into_iter().filter(|entry| {
            entry.work.address.project_id == hook.project_id
                && entry.work.address.protected_session_id == hook.protected_session_id
                && entry.envelope.configuration_revision == control.configuration_revision
                && entry.envelope.candidate.expires_at.0 > now.0
        });
        let entry = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        self.claim_ready_entry(hook, configuration_revision, now, entry)
            .await
    }

    /// Claims only one caller-resolved full-lifecycle address and the exact
    /// current publication watermark. Callers obtain `address` from the
    /// current-admission registry path immediately before invoking this.
    pub async fn claim_ready_guidance_exact(
        &self,
        hook: &HookEventEnvelopeV2,
        address: ContextScoutAddressV1,
        current_input_watermark: [u8; 32],
        configuration_revision: u64,
        now: UtcMicros,
    ) -> Option<(HookReadyGuidanceV1, ContextScoutDurableClaimV1)> {
        if current_input_watermark == [0; 32]
            || address.project_id != hook.project_id
            || address.protected_session_id != hook.protected_session_id
        {
            return None;
        }
        let configuration = self.configuration.read().await;
        let control = configuration.as_ref()?.control();
        let ready = self.store.startup(now, STARTUP_RECOVERY_LIMIT).await;
        let entries = match ready {
            ContextScoutDurableStartupOutcomeV1::Ready { entries, .. } => entries,
            ContextScoutDurableStartupOutcomeV1::Unavailable => return None,
        };
        let entry = entries.into_iter().find(|entry| {
            entry.work.address == address
                && entry.work.input_watermark == current_input_watermark
                && entry.envelope.input_watermark == current_input_watermark
                && entry.envelope.configuration_revision == control.configuration_revision
                && entry.envelope.candidate.expires_at.0 > now.0
        })?;
        self.claim_ready_entry(hook, configuration_revision, now, entry)
            .await
    }

    async fn claim_ready_entry(
        &self,
        hook: &HookEventEnvelopeV2,
        configuration_revision: u64,
        now: UtcMicros,
        entry: ContextScoutDurableQueueEntryV1,
    ) -> Option<(HookReadyGuidanceV1, ContextScoutDurableClaimV1)> {
        if !delivery_window_admitted_at_hook(entry.envelope.delivery_window, &hook.event) {
            return None;
        }
        let lease = ContextScoutLeaseV1 {
            lease_id: hook.event_id,
            expires_at: UtcMicros(now.0.saturating_add(DELIVERY_LEASE_MICROS)),
        };
        let claimed = match self.store.claim(entry.work.address, now, lease).await {
            ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
            ContextScoutDurableClaimOutcomeV1::Empty
            | ContextScoutDurableClaimOutcomeV1::Unavailable => return None,
        };
        if claimed.entry != entry {
            let _ = self.store.requeue(claimed).await;
            return None;
        }
        let guidance = HookReadyGuidanceV1 {
            guidance_id: claimed.entry.envelope.envelope_id,
            event_id: hook.event_id,
            configuration_revision,
            expires_at: claimed.entry.envelope.candidate.expires_at,
            text: claimed.entry.envelope.candidate.suggestion_text.clone(),
        };
        Some((guidance, claimed))
    }

    pub async fn requeue(
        &self,
        claim: ContextScoutDurableClaimV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        self.store.requeue(claim).await
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.record_delivery",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn record_delivery(
        &self,
        claim: &ContextScoutDurableClaimV1,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        let configuration = self.configuration.read().await;
        let Some(control) = configuration
            .as_ref()
            .map(ContextScoutConfigurationPinV1::control)
        else {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        };
        if claim.entry.envelope.configuration_revision != control.configuration_revision {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        self.runtime
            .lock()
            .await
            .complete_delivery(claim, receipt)
            .await
            .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    pub async fn record_delivery_by_handle(
        &self,
        claim: &ContextScoutClaimHandleV1,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        let configuration = self.configuration.read().await;
        let Some(control) = configuration
            .as_ref()
            .map(ContextScoutConfigurationPinV1::control)
        else {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        };
        self.store
            .record_delivery_by_lease(
                claim.work,
                claim.envelope_id,
                ContextScoutLeaseV1 {
                    lease_id: claim.lease_id,
                    expires_at: claim.lease_expires_at,
                },
                control.configuration_revision,
                receipt,
            )
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.record_feedback",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn record_feedback(
        &self,
        receipt: &ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        self.runtime
            .lock()
            .await
            .record_feedback(receipt, feedback)
            .await
            .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    pub async fn record_feedback_exact(
        &self,
        address: ContextScoutAddressV1,
        receipt: &ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        let Ok(recent) = self.recent_exact(address, STARTUP_RECOVERY_LIMIT).await else {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        };
        if !recent
            .deliveries
            .iter()
            .any(|delivery| delivery.receipt == *receipt)
        {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        self.record_feedback(receipt, feedback).await
    }

    pub async fn status(
        &self,
        requested: ContextScoutControlV1,
    ) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        if requested != control {
            return Err(ContextScoutErrorV1::ConfigurationUnavailable);
        }
        self.status_for_control(control).await
    }

    pub async fn cancel(
        &self,
        work: ContextScoutWorkV1,
    ) -> Result<ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1> {
        self.runtime.lock().await.cancel(work).await
    }

    pub async fn prepare_configured(
        &self,
        input: &ContextScoutSelectionInputV1,
        deadline: MonotonicDeadline,
        cancellation: CancellationToken,
    ) -> Result<ContextScoutRuntimeOutcomeV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let pin = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?;
        let control = pin.control();
        let execution =
            ContextScoutModelExecutionV1::new(deadline, cancellation.clone(), control.limits)?;
        let inflight_id = self.next_inflight_id.fetch_add(1, Ordering::Relaxed).max(1);
        let superseded = self
            .inflight
            .lock()
            .map_err(|_| ContextScoutErrorV1::ConfigurationUnavailable)?
            .insert(input.address, (inflight_id, cancellation));
        if let Some((_, superseded)) = superseded {
            superseded.cancel();
        }
        let _registration = InflightContextScoutRunV1 {
            inflight: &self.inflight,
            address: input.address,
            inflight_id,
        };
        self.runtime
            .lock()
            .await
            .prepare_controlled(input, control, execution)
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.install_configuration",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn install_configuration(
        &self,
        pin: ContextScoutConfigurationPinV1,
        model_config: Option<&AutomationConfig>,
    ) -> Result<(), ContextScoutErrorV1> {
        let control = pin.control();
        let model = model_config.map_or_else(
            || {
                control.model_path.map_or_else(
                    || context_scout_model_assistant_from_project_config(None),
                    |backend| {
                        Arc::new(UnavailableConfiguredContextScoutModelV1(backend))
                            as Arc<dyn ContextScoutModelAssistantV1>
                    },
                )
            },
            |config| context_scout_model_assistant_from_project_config(Some(config)),
        );
        if control
            .model_path
            .is_some_and(|expected| expected != model.backend())
        {
            return Err(ContextScoutErrorV1::ConfigurationUnavailable);
        }
        let mut configuration = self.configuration.write().await;
        let mut runtime = self.runtime.lock().await;
        runtime.status(control)?;
        runtime.replace_model(model);
        *configuration = Some(pin);
        Ok(())
    }

    /// Installs only an admitted active/paused control transition while
    /// preserving the already-selected model authority.
    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.install_state_transition",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn install_state_transition(
        &self,
        pin: ContextScoutConfigurationPinV1,
    ) -> Result<(), ContextScoutErrorV1> {
        let next = pin.control();
        let mut configuration = self.configuration.write().await;
        let current = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        if !context_scout_state_transition_is_exact(current, next) {
            return Err(ContextScoutErrorV1::ConfigurationUnavailable);
        }
        self.runtime.lock().await.status(next)?;
        *configuration = Some(pin);
        Ok(())
    }

    pub async fn configured_status(&self) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        self.status_for_control(control).await
    }

    #[hotpath::measure(
        future = true,
        label = "hosts.agent.context_scout.recent",
        impl_type = "ProjectContextScoutOwnerV1"
    )]
    pub async fn recent(
        &self,
        protected_session_id: [u8; 32],
        limit: usize,
    ) -> Result<ContextScoutRecentStateV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        let observed_at =
            current_utc_micros().ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?;
        match self
            .store
            .recent_for_protected_session(
                protected_session_id,
                control.configuration_revision,
                observed_at,
                limit,
            )
            .await
        {
            ContextScoutRecentReadOutcomeV1::Ready(recent)
                if recent_has_single_exact_address(&recent) =>
            {
                Ok(recent)
            }
            ContextScoutRecentReadOutcomeV1::Ready(_) => {
                Err(ContextScoutErrorV1::ConfigurationUnavailable)
            }
            ContextScoutRecentReadOutcomeV1::Unavailable => {
                Err(ContextScoutErrorV1::ConfigurationUnavailable)
            }
        }
    }

    pub async fn recent_exact(
        &self,
        address: ContextScoutAddressV1,
        limit: usize,
    ) -> Result<ContextScoutRecentStateV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        let observed_at =
            current_utc_micros().ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?;
        match self
            .store
            .recent(address, control.configuration_revision, observed_at, limit)
            .await
        {
            ContextScoutRecentReadOutcomeV1::Ready(recent) => Ok(recent),
            ContextScoutRecentReadOutcomeV1::Unavailable => {
                Err(ContextScoutErrorV1::ConfigurationUnavailable)
            }
        }
    }

    pub async fn explain(
        &self,
        protected_session_id: [u8; 32],
        limit: usize,
    ) -> Result<ContextScoutExplanationV1, ContextScoutErrorV1> {
        let recent = self.recent(protected_session_id, limit).await?;
        Ok(ContextScoutExplanationV1 {
            status: status_with_recent(self.configured_status().await?, &recent),
            recent,
        })
    }

    pub async fn explain_exact(
        &self,
        address: ContextScoutAddressV1,
        limit: usize,
    ) -> Result<ContextScoutExplanationV1, ContextScoutErrorV1> {
        let recent = self.recent_exact(address, limit).await?;
        Ok(ContextScoutExplanationV1 {
            status: status_with_recent(self.configured_status().await?, &recent),
            recent,
        })
    }

    pub async fn capability(&self) -> Result<ContextScoutCapabilityStateV1, ContextScoutErrorV1> {
        let status = self.configured_status().await?;
        let recent = self.recent_project_state(1).await?;
        let status = status_with_recent(status, &recent);
        let configured_model_available = status.model_path.is_some()
            && !matches!(
                status.last_model_outcome,
                None | Some(
                    ContextScoutModelOutcomeV1::Disabled | ContextScoutModelOutcomeV1::Unavailable
                )
            );
        Ok(ContextScoutCapabilityStateV1 {
            state: status.state,
            mode: status.mode,
            deterministic_available: true,
            configured_model: status.model_path,
            configured_model_available,
            last_model_outcome: status.last_model_outcome,
        })
    }

    pub async fn budget(&self) -> Result<ContextScoutBudgetStateV1, ContextScoutErrorV1> {
        let status = status_with_recent(
            self.configured_status().await?,
            &self.recent_project_state(1).await?,
        );
        Ok(ContextScoutBudgetStateV1 {
            limits: status.limits,
            last_model_outcome: status.last_model_outcome,
            exhausted: status.last_model_outcome
                == Some(ContextScoutModelOutcomeV1::TokenBudgetExceeded),
            last_input_tokens: status
                .last_model_receipt
                .as_ref()
                .and_then(|receipt| receipt.input_tokens),
            last_output_tokens: status
                .last_model_receipt
                .as_ref()
                .and_then(|receipt| receipt.output_tokens),
            last_estimated_cost_microusd: status
                .last_model_receipt
                .and_then(|receipt| receipt.estimated_cost_microusd),
        })
    }

    async fn recent_project_state(
        &self,
        limit: usize,
    ) -> Result<ContextScoutRecentStateV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        let observed_at =
            current_utc_micros().ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?;
        match self
            .store
            .recent_project(control.configuration_revision, observed_at, limit)
            .await
        {
            ContextScoutRecentReadOutcomeV1::Ready(recent) => Ok(recent),
            ContextScoutRecentReadOutcomeV1::Unavailable => {
                Err(ContextScoutErrorV1::ConfigurationUnavailable)
            }
        }
    }

    async fn status_for_control(
        &self,
        control: ContextScoutControlV1,
    ) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        let status = self.runtime.lock().await.status(control)?;
        let recent = self.recent_project_state(STARTUP_RECOVERY_LIMIT).await?;
        Ok(status_with_recent(status, &recent))
    }

    pub async fn configure_model(&self, config: &AutomationConfig) {
        let configuration = self.configuration.read().await;
        let Some(control) = configuration
            .as_ref()
            .map(ContextScoutConfigurationPinV1::control)
        else {
            return;
        };
        let model = context_scout_model_assistant_from_project_config(Some(config));
        if control
            .model_path
            .is_some_and(|expected| expected != model.backend())
        {
            return;
        }
        self.runtime.lock().await.replace_model(model);
    }

    pub async fn claim(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutDurableClaimOutcomeV1 {
        self.store.claim(address, now, lease).await
    }

    pub async fn claim_delivery_exact(
        &self,
        address: ContextScoutAddressV1,
        window: ContextScoutDeliveryWindowV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutDurableClaimOutcomeV1 {
        if !matches!(
            window,
            ContextScoutDeliveryWindowV1::IdleWindow | ContextScoutDeliveryWindowV1::OnRequest
        ) {
            return ContextScoutDurableClaimOutcomeV1::Unavailable;
        }
        let claimed = self.store.claim(address, now, lease).await;
        let ContextScoutDurableClaimOutcomeV1::Claimed(claim) = claimed else {
            return claimed;
        };
        if claim.entry.work.address == address && claim.entry.envelope.delivery_window == window {
            return ContextScoutDurableClaimOutcomeV1::Claimed(claim);
        }
        match self.store.requeue(claim).await {
            ContextScoutDurableStoreOutcomeV1::Unavailable => {
                ContextScoutDurableClaimOutcomeV1::Unavailable
            }
            ContextScoutDurableStoreOutcomeV1::Stored
            | ContextScoutDurableStoreOutcomeV1::Duplicate
            | ContextScoutDurableStoreOutcomeV1::Superseded => {
                ContextScoutDurableClaimOutcomeV1::Empty
            }
        }
    }

    pub async fn claim_delivery_request(
        &self,
        request: &ContextScoutClaimRequestV1,
        now: UtcMicros,
        expires_at: UtcMicros,
    ) -> ContextScoutDurableClaimOutcomeV1 {
        let Some(ContextScoutPublicMutationV1::Claim {
            address,
            window,
            configuration_revision,
            lease,
            ..
        }) = self.public_claim_mutation(request, now, expires_at).await
        else {
            return ContextScoutDurableClaimOutcomeV1::Unavailable;
        };
        let claimed = self.claim_delivery_exact(address, window, now, lease).await;
        let ContextScoutDurableClaimOutcomeV1::Claimed(claim) = claimed else {
            return claimed;
        };
        if claim.entry.envelope.configuration_revision == configuration_revision {
            return ContextScoutDurableClaimOutcomeV1::Claimed(claim);
        }
        match self.store.requeue(claim).await {
            ContextScoutDurableStoreOutcomeV1::Unavailable => {
                ContextScoutDurableClaimOutcomeV1::Unavailable
            }
            ContextScoutDurableStoreOutcomeV1::Stored
            | ContextScoutDurableStoreOutcomeV1::Duplicate
            | ContextScoutDurableStoreOutcomeV1::Superseded => {
                ContextScoutDurableClaimOutcomeV1::Empty
            }
        }
    }

    pub async fn public_claim_mutation(
        &self,
        request: &ContextScoutClaimRequestV1,
        now: UtcMicros,
        expires_at: UtcMicros,
    ) -> Option<ContextScoutPublicMutationV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .map(ContextScoutConfigurationPinV1::control)?;
        let window = match request.window {
            ContextScoutClaimWindowV1::IdleWindow => ContextScoutDeliveryWindowV1::IdleWindow,
            ContextScoutClaimWindowV1::OnRequest => ContextScoutDeliveryWindowV1::OnRequest,
        };
        let digest = canonical_sha256(&(
            "tracedecay.context-scout.delivery-lease.v1",
            &request.idempotency_key,
            request.address,
            request.window,
            control.configuration_revision,
        ))
        .ok()?;
        let encoded = digest.hex_suffix().and_then(|encoded| encoded.get(..32))?;
        let mut lease_id = [0; 16];
        if hex::decode_to_slice(encoded, &mut lease_id).is_err() {
            return None;
        }
        Some(ContextScoutPublicMutationV1::Claim {
            address: request.address,
            window,
            configuration_revision: control.configuration_revision,
            now,
            lease: ContextScoutLeaseV1 {
                lease_id,
                expires_at,
            },
        })
    }

    pub async fn commit_public_mutation(
        &self,
        binding: ContextScoutMutationBindingV1,
        mutation: ContextScoutPublicMutationV1,
    ) -> ContextScoutMutationSettlementOutcomeV1 {
        self.store.commit_public_mutation(binding, mutation).await
    }
}

fn context_scout_state_transition_is_exact(
    current: ContextScoutControlV1,
    next: ContextScoutControlV1,
) -> bool {
    current.configuration_revision != next.configuration_revision
        && current.mode == next.mode
        && current.model_path == next.model_path
        && current.limits == next.limits
        && matches!(
            (current.state, next.state),
            (
                ContextScoutServiceStateV1::Active,
                ContextScoutServiceStateV1::Paused
            ) | (
                ContextScoutServiceStateV1::Paused,
                ContextScoutServiceStateV1::Active
            )
        )
}

fn delivery_window_admitted_at_hook(
    window: ContextScoutDeliveryWindowV1,
    event: &HookEventV2,
) -> bool {
    match window {
        ContextScoutDeliveryWindowV1::Immediate => true,
        ContextScoutDeliveryWindowV1::NextBoundary => matches!(
            event,
            HookEventV2::PromptBoundary
                | HookEventV2::SessionBoundary {
                    boundary: HookBoundaryV1::End | HookBoundaryV1::TurnComplete,
                }
                | HookEventV2::ToolLifecycle {
                    phase: HookLifecyclePhaseV1::Completed
                        | HookLifecyclePhaseV1::Failed
                        | HookLifecyclePhaseV1::Cancelled,
                    ..
                }
                | HookEventV2::TestLifecycle {
                    phase: HookLifecyclePhaseV1::Completed
                        | HookLifecyclePhaseV1::Failed
                        | HookLifecyclePhaseV1::Cancelled,
                    ..
                }
        ),
        // Hook V2 has no authenticated idle or explicit-request event.
        // Those windows require their dedicated owner operation rather than
        // being inferred from unrelated host activity.
        ContextScoutDeliveryWindowV1::IdleWindow
        | ContextScoutDeliveryWindowV1::OnRequest
        | ContextScoutDeliveryWindowV1::Suppressed => false,
    }
}

fn current_utc_micros() -> Option<UtcMicros> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_micros();
    i64::try_from(micros).ok().map(UtcMicros)
}

fn status_with_recent(
    mut status: ContextScoutStatusV1,
    recent: &ContextScoutRecentStateV1,
) -> ContextScoutStatusV1 {
    status.active_suggestions = recent.pending.len();
    let Some(entry) = recent
        .pending
        .iter()
        .chain(recent.deliveries.iter().map(|delivery| &delivery.entry))
        .max_by_key(|entry| entry.envelope.candidate.expires_at)
    else {
        return status;
    };
    status.last_route = Some(entry.route);
    status.last_model_outcome = Some(entry.model_outcome);
    status.last_model_receipt.clone_from(&entry.model_receipt);
    if let Some(delivered) = recent
        .deliveries
        .iter()
        .find(|delivery| delivery.entry.envelope.envelope_id == entry.envelope.envelope_id)
    {
        status.last_delivery_outcome = Some(delivered.receipt.outcome);
        status.last_feedback = delivered.feedback.map(|feedback| feedback.kind);
    }
    status
}

fn recent_has_single_exact_address(recent: &ContextScoutRecentStateV1) -> bool {
    let mut address = None;
    recent
        .pending
        .iter()
        .map(|entry| entry.work.address)
        .chain(
            recent
                .deliveries
                .iter()
                .map(|delivery| delivery.entry.work.address),
        )
        .all(|candidate| match address {
            Some(expected) => expected == candidate,
            None => {
                address = Some(candidate);
                true
            }
        })
}

struct InflightContextScoutRunV1<'a> {
    inflight: &'a StdMutex<BTreeMap<ContextScoutAddressV1, (u64, CancellationToken)>>,
    address: ContextScoutAddressV1,
    inflight_id: u64,
}

impl Drop for InflightContextScoutRunV1<'_> {
    fn drop(&mut self) {
        if let Ok(mut inflight) = self.inflight.lock()
            && inflight
                .get(&self.address)
                .is_some_and(|(current, _)| *current == self.inflight_id)
        {
            inflight.remove(&self.address);
        }
    }
}

struct UnavailableConfiguredContextScoutModelV1(ContextScoutModelBackendV1);

impl ContextScoutModelAssistantV1 for UnavailableConfiguredContextScoutModelV1 {
    fn backend(&self) -> ContextScoutModelBackendV1 {
        self.0
    }

    fn propose(
        &self,
        _request: ContextScoutModelRequestV1,
        _execution: ContextScoutModelExecutionV1,
    ) -> ContextScoutModelFuture<'_> {
        Box::pin(async { Err(ContextScoutModelErrorV1::Unavailable) })
    }
}

#[cfg(test)]
mod tests {
    use super::super::ports::ContextScoutAddressBindOutcomeV1;
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use tracedecay_application::configuration::ConfigurationCurrentStateV1;
    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::canonical_sha256;
    use tracedecay_domain::configuration::{
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY, CandidateDispositionV1, ConfigurationCandidateV1,
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
        ConfigurationValueV1, ContextScoutSettingsV1, SettingKey,
    };
    use tracedecay_domain::feedback::FeedbackScopeV1;
    use tracedecay_domain::{ActorId, RepositoryId, WorktreeId};
    use tracedecay_hooks::{
        HookCapabilityV1, HookEventFamily, HookHostV1, HookScopeBindingV1,
        NativeEnvelopeMaterialV1, decode_bound_native_hook_event, stock_event_support,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    #[test]
    fn state_transition_rejects_model_route_or_limit_drift() {
        let current = ContextScoutControlV1 {
            configuration_revision: [1; 32],
            state: super::super::ContextScoutServiceStateV1::Active,
            mode: super::super::ContextScoutRuntimeModeV1::ConfiguredModel,
            model_path: Some(ContextScoutModelBackendV1::CodexAppServer),
            limits: super::super::ContextScoutLimitsV1::bounded_defaults(),
        };
        let paused = ContextScoutControlV1 {
            configuration_revision: [2; 32],
            state: super::super::ContextScoutServiceStateV1::Paused,
            ..current
        };
        assert!(context_scout_state_transition_is_exact(current, paused));

        let changed_model = ContextScoutControlV1 {
            model_path: Some(ContextScoutModelBackendV1::Unsupported),
            ..paused
        };
        assert!(!context_scout_state_transition_is_exact(
            current,
            changed_model
        ));

        let changed_limits = ContextScoutControlV1 {
            limits: super::super::ContextScoutLimitsV1 {
                max_candidates: paused.limits.max_candidates.saturating_add(1),
                ..paused.limits
            },
            ..paused
        };
        assert!(!context_scout_state_transition_is_exact(
            current,
            changed_limits
        ));
    }

    #[test]
    fn delayed_windows_require_their_exact_native_boundary() {
        let saved_edit = HookEventV2::SavedEdit {
            file_id: [1; 16],
            changed_range_count: 1,
        };
        assert!(delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::Immediate,
            &saved_edit,
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::NextBoundary,
            &saved_edit,
        ));

        let prompt_boundary = HookEventV2::PromptBoundary;
        assert!(delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::NextBoundary,
            &prompt_boundary,
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::OnRequest,
            &prompt_boundary,
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::IdleWindow,
            &prompt_boundary,
        ));

        let tool_started = HookEventV2::ToolLifecycle {
            tool_id: [2; 16],
            phase: HookLifecyclePhaseV1::Started,
            effect_receipt_id: None,
        };
        let tool_completed = HookEventV2::ToolLifecycle {
            tool_id: [2; 16],
            phase: HookLifecyclePhaseV1::Completed,
            effect_receipt_id: None,
        };
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::NextBoundary,
            &tool_started,
        ));
        assert!(delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::NextBoundary,
            &tool_completed,
        ));

        let session_end = HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::End,
        };
        assert!(delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::NextBoundary,
            &session_end,
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::IdleWindow,
            &session_end,
        ));
    }

    #[test]
    fn application_claim_windows_are_exact_and_never_inferred_from_hooks() {
        assert!(matches!(
            ContextScoutDeliveryWindowV1::IdleWindow,
            ContextScoutDeliveryWindowV1::IdleWindow
        ));
        assert!(matches!(
            ContextScoutDeliveryWindowV1::OnRequest,
            ContextScoutDeliveryWindowV1::OnRequest
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::IdleWindow,
            &HookEventV2::PromptBoundary,
        ));
        assert!(!delivery_window_admitted_at_hook(
            ContextScoutDeliveryWindowV1::OnRequest,
            &HookEventV2::PromptBoundary,
        ));
    }

    async fn test_database() -> (tempfile::TempDir, Database) {
        crate::register_test_schema_installer();
        let temporary = tempfile::tempdir().expect("owner fixture");
        let path = temporary.path().join("graph.db");
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &path,
            "Context Scout owner",
        )
        .expect("database authority");
        let database = Database::publish_test_runtime(
            &path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("project database")
        .0;
        (temporary, database)
    }

    async fn test_owner(project_id: [u8; 16]) -> Arc<ProjectContextScoutOwnerV1> {
        let (_temporary, database) = test_database().await;
        ProjectContextScoutOwnerV1::startup(database, project_id, UtcMicros(1), None)
            .await
            .expect("owner")
    }

    fn unregister(
        project_id: [u8; 16],
        owner: &ProjectContextScoutOwnerV1,
    ) -> ContextScoutOwnerUnregisterOutcomeV1 {
        unregister_registered_context_scout_owner(
            project_id,
            owner.store().database().canonical_database_path(),
        )
    }

    fn claim_id<T: TryFrom<String>>(value: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn claim_lifecycle(marker: u16) -> ContextScoutLifecycleAddressV1 {
        ContextScoutLifecycleAddressV1 {
            profile_id: claim_id("profile.scout.fixture"),
            provider_id: claim_id("provider.claude"),
            project_id: claim_id("project.scout.fixture"),
            worktree_id: claim_id("worktree.scout.fixture"),
            session_id: claim_id("session.scout.fixture"),
            thread_id: claim_id("thread.scout.fixture"),
            turn_id: claim_id("turn.scout.fixture"),
            agent_id: claim_id("agent.scout.fixture"),
            logical_message_id: claim_id(&format!("message.scout.cap.{marker:03}")),
        }
    }

    fn claim_mount_authority(
        database: Database,
    ) -> (
        Arc<ProjectContextScoutAddressRegistryV1>,
        AdmittedContextScoutHookV1,
        ContextScoutAuthorityPinV1,
        RequestContext,
        UtcMicros,
    ) {
        let observed_at = UtcMicros(10);
        let project_id = claim_id::<tracedecay_domain::ProjectId>("project.scout.fixture");
        let repository_id = claim_id::<RepositoryId>("repository.scout.fixture");
        let worktree_id = claim_id::<WorktreeId>("worktree.scout.fixture");
        let scope = ResolvedScope::new(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            Some(claim_id("refs/heads/main")),
        )
        .expect("scope");
        let capability = CapabilityId::new("capability.scout.cap").expect("capability");
        let use_case = UseCaseId::new("use-case.scout.cap").expect("use case");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.scout.cap").expect("grant"),
            1,
            canonical_sha256(&"scout.cap").expect("digest"),
            ActorId::new("actor.scout.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.scout.requester").expect("actor"),
            scope.clone(),
            grant,
            RequestId::new("request.scout.cap").expect("request"),
            Deadline::new(UtcMicros(10_000)).expect("deadline"),
            CancellationContext::active("cancel.scout.cap").expect("cancel"),
        )
        .expect("request context");
        let setting_key = SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).expect("setting");
        let revision_id = ConfigurationRevisionId::new("revision.scout.cap").expect("revision");
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                setting_key.clone(),
                ConfigurationValueV1::ContextScoutSettings(ContextScoutSettingsV1::disabled()),
            )]),
            BTreeMap::from([(
                setting_key,
                vec![ConfigurationCandidateV1 {
                    layer: ConfigurationLayerIdV1::Project {
                        project_id: project_id.clone(),
                    },
                    revision_id: revision_id.clone(),
                    disposition: CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .expect("snapshot");
        let configuration =
            ContextScoutConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
                revision_id,
                snapshot,
            })
            .expect("configuration pin");
        let feedback_scope = FeedbackScopeV1 {
            project_id,
            repository_id,
            worktree_id,
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: claim_id("commit.scout.fixture"),
        };
        let pin =
            ContextScoutAuthorityPinV1::new(&context, feedback_scope, configuration, observed_at)
                .expect("authority pin");
        let binding = HookScopeBindingV1 {
            host: HookHostV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 1,
            binding_token: [4; 32],
            capabilities: [
                HookEventFamily::SessionBoundary,
                HookEventFamily::PromptBoundary,
                HookEventFamily::ToolLifecycle,
                HookEventFamily::SavedEdit,
                HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| HookCapabilityV1 {
                family,
                support: stock_event_support(HookHostV1::ClaudeCode, family),
            })
            .collect(),
        };
        let envelope = decode_bound_native_hook_event(
            HookHostV1::ClaudeCode,
            include_bytes!(
                "../../../../../tests/fixtures/packaged_host_events/claude/post_tool_use_write.json"
            ),
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [5; 16],
                protected_session_id: [6; 32],
                observed_at,
                tool_id: Some([7; 16]),
                effect_receipt_id: Some([8; 16]),
                file_id: Some([9; 16]),
                changed_range_count: 1,
            },
        )
        .expect("hook envelope");
        let hook = AdmittedContextScoutHookV1::new(envelope, &binding).expect("admitted hook");
        let registry =
            ProjectContextScoutAddressRegistryV1::new(database, claim_id("project.scout.fixture"))
                .expect("address registry");
        (registry, hook, pin, context, observed_at)
    }

    #[tokio::test]
    async fn stale_retirement_of_replaced_owner_keeps_the_live_owner() {
        let project_id = [44; 16];
        let (_first_temporary, first_database) = test_database().await;
        let first =
            ProjectContextScoutOwnerV1::startup(first_database, project_id, UtcMicros(1), None)
                .await
                .expect("owner A");
        let (_second_temporary, second_database) = test_database().await;
        let second =
            ProjectContextScoutOwnerV1::startup(second_database, project_id, UtcMicros(2), None)
                .await
                .expect("owner B replaces A");
        assert!(!Arc::ptr_eq(&first, &second));

        // Late retirement of runtime A must not evict B.
        assert_eq!(
            unregister(project_id, &first),
            ContextScoutOwnerUnregisterOutcomeV1::StaleIdentity
        );
        let registered = lookup_registered_context_scout_owners(project_id);
        assert_eq!(registered.len(), 1);
        assert!(
            Arc::ptr_eq(&registered[0], &second),
            "stale retirement must leave the replacement owner registered"
        );

        assert_eq!(
            unregister(project_id, &second),
            ContextScoutOwnerUnregisterOutcomeV1::Removed
        );
        assert!(lookup_registered_context_scout_owners(project_id).is_empty());
        assert_eq!(
            unregister(project_id, &second),
            ContextScoutOwnerUnregisterOutcomeV1::Vacant
        );
    }

    #[tokio::test]
    async fn startup_replaces_owner_when_database_identity_differs() {
        let project_id = [43; 16];
        let (_first_temporary, first_database) = test_database().await;
        let first =
            ProjectContextScoutOwnerV1::startup(first_database, project_id, UtcMicros(1), None)
                .await
                .expect("first owner");
        let (_second_temporary, second_database) = test_database().await;
        let second = ProjectContextScoutOwnerV1::startup(
            second_database.clone(),
            project_id,
            UtcMicros(2),
            None,
        )
        .await
        .expect("replaced owner");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "a different database identity must not reuse the stale owner"
        );
        assert_eq!(
            second.store().database().canonical_database_path(),
            second_database.canonical_database_path()
        );
        unregister(project_id, &second);
    }

    #[tokio::test]
    async fn startup_reuses_the_live_owner_for_the_same_project() {
        let project_id = [41; 16];
        let (_temporary, database) = test_database().await;
        let first =
            ProjectContextScoutOwnerV1::startup(database.clone(), project_id, UtcMicros(1), None)
                .await
                .expect("owner");
        let again = ProjectContextScoutOwnerV1::startup(database, project_id, UtcMicros(2), None)
            .await
            .expect("reused owner");
        assert!(Arc::ptr_eq(&first, &again));
        unregister(project_id, &again);
    }

    #[tokio::test]
    async fn two_hundred_fifty_seventh_claim_is_a_typed_denial() {
        let project_id = [42; 16];
        let owner = test_owner(project_id).await;
        let (_temporary, database) = test_database().await;
        let (registry, hook, pin, context, observed_at) = claim_mount_authority(database);
        for marker in 0..MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES {
            let lifecycle = claim_lifecycle(u16::try_from(marker).expect("marker"));
            let address = match registry.bind(&hook, &pin, lifecycle.clone()).await {
                ContextScoutAddressBindOutcomeV1::Bound(address)
                | ContextScoutAddressBindOutcomeV1::Existing(address) => address,
                other => panic!("expected bound address, got {other:?}"),
            };
            assert_eq!(
                owner
                    .mount_current_claim_authority(
                        Arc::clone(&registry),
                        &hook,
                        pin.clone(),
                        context.clone(),
                        lifecycle,
                        address,
                        [1; 32],
                        observed_at,
                        true,
                    )
                    .await,
                ContextScoutClaimAdmissionV1::Mounted
            );
        }
        let (_overflow_temporary, overflow_database) = test_database().await;
        let (overflow_registry, overflow_hook, overflow_pin, overflow_context, overflow_at) =
            claim_mount_authority(overflow_database);
        let overflow_lifecycle = claim_lifecycle(
            u16::try_from(MAX_MOUNTED_CONTEXT_SCOUT_CLAIM_AUTHORITIES).expect("cap"),
        );
        let overflow_address = match overflow_registry
            .bind(&overflow_hook, &overflow_pin, overflow_lifecycle.clone())
            .await
        {
            ContextScoutAddressBindOutcomeV1::Bound(address)
            | ContextScoutAddressBindOutcomeV1::Existing(address) => address,
            other => panic!("expected overflow bound address, got {other:?}"),
        };
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    overflow_registry,
                    &overflow_hook,
                    overflow_pin,
                    overflow_context,
                    overflow_lifecycle,
                    overflow_address,
                    [1; 32],
                    overflow_at,
                    true,
                )
                .await,
            ContextScoutClaimAdmissionV1::DeniedAtCapacity
        );
        unregister(project_id, &owner);
    }
}
