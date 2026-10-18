use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, RwLock};
use tracedecay_contracts::RequestContext;
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutClaimRequestV1, ContextScoutClaimWindowV1,
    ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1, ContextScoutDurableClaimV1,
    ContextScoutDurableQueueEntryV1, ContextScoutFeedbackV1, ContextScoutLeaseV1,
    ContextScoutModelBackendV1, ContextScoutModelOutcomeV1, ContextScoutWorkV1,
};
use tracedecay_domain::configuration::{ContextScoutIntentKindV1, ContextScoutIntentSettingsV1};
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookLifecyclePhaseV1, HookReadyGuidanceV1,
};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::address_registry::{
    AdmittedContextScoutHookV1, ContextScoutAddressResolveOutcomeV1, ContextScoutAuthorityPinV1,
    ContextScoutConfigurationPinV1, ContextScoutLifecycleAddressV1,
    ProjectContextScoutAddressRegistryV1,
};
use super::decision_engine::{
    DecisionEngineClientV1, DecisionEngineConfigV1, DecisionEnginePortV1,
};
use super::model::{ContextScoutModelConfig, context_scout_model_assistant_from_project_config};
use super::{
    ContextScoutBudgetStateV1, ContextScoutCapabilityStateV1, ContextScoutControlV1,
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableRuntimeV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1,
    ContextScoutExplanationV1, ContextScoutModelAssistantV1, ContextScoutModelErrorV1,
    ContextScoutModelExecutionV1, ContextScoutModelFuture, ContextScoutModelRequestV1,
    ContextScoutMutationBindingV1, ContextScoutMutationResultV1,
    ContextScoutMutationSettlementOutcomeV1, ContextScoutPublicMutationV1,
    ContextScoutRecentReadOutcomeV1, ContextScoutRecentStateV1, ContextScoutRouteV1,
    ContextScoutRuntimeOutcomeV1, ContextScoutSelectionInputV1, ContextScoutServiceStateV1,
    ContextScoutStatusV1, ContextScoutSuppressionV1, ProjectContextScoutDurableStoreV1,
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
    hook: AdmittedContextScoutHookV1,
    hook_configuration_revision: u64,
    context: RequestContext,
    lifecycle: ContextScoutLifecycleAddressV1,
    address: ContextScoutAddressV1,
    input_watermark: [u8; 32],
}

pub(crate) type ProjectScoutRuntime =
    ContextScoutDurableRuntimeV1<Arc<dyn ContextScoutModelAssistantV1>>;

/// Routes a project identity to its live owner. The graphs that started or
/// joined an owner hold it; the registry never keeps an owner, and with it the
/// graph store, alive after the last of them drops.
type ProjectContextScoutOwnerRegistry = BTreeMap<[u8; 16], Weak<ProjectContextScoutOwnerV1>>;

pub struct ProjectContextScoutOwnerV1 {
    store: Arc<ProjectContextScoutDurableStoreV1>,
    runtime: Mutex<ProjectScoutRuntime>,
    configuration: RwLock<Option<ContextScoutConfigurationPinV1>>,
    inflight: StdMutex<BTreeMap<ContextScoutAddressV1, (u64, CancellationToken)>>,
    next_inflight_id: AtomicU64,
    startup: ContextScoutDurableStartupOutcomeV1,
    claim_authorities: RwLock<Vec<MountedContextScoutClaimV1>>,
    intent_engine: StdMutex<Option<InstalledIntentEngineV1>>,
}

/// The decision engine built from one installed intent configuration. It is
/// rebuilt only when the pinned settings digest changes, so its concurrency
/// bound spans every pass of that configuration.
#[derive(Clone)]
struct InstalledIntentEngineV1 {
    settings_digest: [u8; 32],
    engine: Arc<dyn DecisionEnginePortV1>,
}

/// What the session's Scout state says about one intent pass. Read across
/// every address of the session, unlike the single-address reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextScoutIntentSessionStateV1 {
    /// Another address of the session holds a suggestion still pending.
    pub pending_sibling: bool,
    /// A suggestion was delivered to the session within the configured
    /// minimum delivery gap.
    pub delivered_within_gap: bool,
    pub delivered_dedupe_keys: BTreeSet<[u8; 32]>,
    /// Intents an intent-pass suggestion already delivered this session.
    pub delivered_intents: BTreeSet<ContextScoutIntentKindV1>,
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
        .and_then(Weak::upgrade)
        .map_or_default(|owner| vec![owner])
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

/// Removes the process-global route to one project's owner, but only when the
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
    let Some(existing) = owners.get(&project_id).and_then(Weak::upgrade) else {
        owners.remove(&project_id);
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
    #[tracing::instrument(name = "hosts.agent.context_scout.startup", level = "trace", skip_all)]
    pub async fn startup(
        database: Database,
        project_id: [u8; 16],
        now: UtcMicros,
        model_config: Option<ContextScoutModelConfig<'_>>,
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
            intent_engine: StdMutex::new(None),
        });
        let mut owners = registered_context_scout_owners().lock().ok()?;
        if let Some(existing) = owners.get(&project_id).and_then(Weak::upgrade)
            && existing.binds_database(owner.store.database())
        {
            return Some(existing);
        }
        owners.retain(|_, registered| registered.strong_count() > 0);
        owners.insert(project_id, Arc::downgrade(&owner));
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
        hook_configuration_revision: u64,
        pin: ContextScoutAuthorityPinV1,
        context: RequestContext,
        lifecycle: ContextScoutLifecycleAddressV1,
        address: ContextScoutAddressV1,
        input_watermark: [u8; 32],
        observed_at: UtcMicros,
        configuration_is_current: bool,
    ) -> ContextScoutClaimAdmissionV1 {
        if input_watermark == [0; 32]
            || hook_configuration_revision == 0
            || !configuration_is_current
            || registry
                .resolve_current_exact(hook, &pin, &lifecycle, &context, observed_at)
                .await
                != ContextScoutAddressResolveOutcomeV1::Resolved(address)
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
            existing.hook = hook.clone();
            existing.hook_configuration_revision = hook_configuration_revision;
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
            hook: hook.clone(),
            hook_configuration_revision,
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
        (resolved == ContextScoutAddressResolveOutcomeV1::Resolved(mounted.address))
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

    /// Returns the pin for one unique mounted producer in the same routed
    /// session. A later host event may have a different turn/message tuple;
    /// the retained producer lifecycle remains the exact registry authority.
    pub async fn unique_session_claim_pin(
        &self,
        lifecycle: &ContextScoutLifecycleAddressV1,
    ) -> Option<ContextScoutAuthorityPinV1> {
        let authorities = self.claim_authorities.read().await;
        let mut matches = authorities
            .iter()
            .filter(|mounted| same_routed_native_session(&mounted.lifecycle, lifecycle));
        let mounted = matches.next()?;
        matches.next().is_none().then(|| mounted.pin.clone())
    }

    pub async fn resolve_current_session_claim_authority(
        &self,
        hook: &AdmittedContextScoutHookV1,
        lifecycle: &ContextScoutLifecycleAddressV1,
        expected_pin: &ContextScoutAuthorityPinV1,
        observed_at: UtcMicros,
        configuration_is_current: bool,
    ) -> Option<(ContextScoutAddressV1, [u8; 32])> {
        if !configuration_is_current {
            return None;
        }
        let mounted = {
            let authorities = self.claim_authorities.read().await;
            let mut matches = authorities
                .iter()
                .filter(|mounted| same_routed_native_session(&mounted.lifecycle, lifecycle));
            let mounted = matches.next()?.clone();
            if matches.next().is_some() {
                return None;
            }
            mounted
        };
        if mounted.pin != *expected_pin {
            return None;
        }
        let resolved = mounted
            .registry
            .resolve_current_exact(
                hook,
                &mounted.pin,
                &mounted.lifecycle,
                &mounted.context,
                observed_at,
            )
            .await;
        (resolved == ContextScoutAddressResolveOutcomeV1::Resolved(mounted.address))
            .then_some((mounted.address, mounted.input_watermark))
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

    /// Supplies the original admitted hook only after the current public
    /// request has proved its exact address, configuration, and routed scope.
    /// The producer obtains fresh feedback authorization when it runs.
    pub async fn current_producer_input(
        &self,
        address: ContextScoutAddressV1,
        configuration: &ContextScoutConfigurationPinV1,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<(
        AdmittedContextScoutHookV1,
        ContextScoutLifecycleAddressV1,
        u64,
    )> {
        if self.configuration.read().await.as_ref() != Some(configuration) {
            return None;
        }
        let mounted = self
            .claim_authorities
            .read()
            .await
            .iter()
            .find(|mounted| mounted.address == address)
            .cloned()?;
        if mounted.pin.configuration() != configuration
            || !mounted
                .registry
                .authorize_current_exact_address(address, configuration, scope)
                .await
        {
            return None;
        }
        Some((
            mounted.hook,
            mounted.lifecycle,
            mounted.hook_configuration_revision,
        ))
    }

    pub fn startup_outcome(&self) -> &ContextScoutDurableStartupOutcomeV1 {
        &self.startup
    }

    #[tracing::instrument(
        name = "hosts.agent.context_scout.claim_ready",
        level = "trace",
        skip_all
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
            ContextScoutDurableStartupOutcomeV1::Reset => Vec::new(),
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

    /// Claims only one caller-resolved full-lifecycle address. The current
    /// publication watermark proves that authority is mounted; queued guidance
    /// can predate the boundary that makes it deliverable.
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
        let recent = match self
            .store
            .recent(address, control.configuration_revision, now, 1)
            .await
        {
            ContextScoutRecentReadOutcomeV1::Ready(recent) => recent,
            ContextScoutRecentReadOutcomeV1::Unavailable => return None,
        };
        let entry = recent.pending.into_iter().find(|entry| {
            entry.work.address == address
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

    #[tracing::instrument(
        name = "hosts.agent.context_scout.record_delivery",
        level = "trace",
        skip_all
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

    #[tracing::instrument(
        name = "hosts.agent.context_scout.record_feedback",
        level = "trace",
        skip_all
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

    #[tracing::instrument(
        name = "hosts.agent.context_scout.install_configuration",
        level = "trace",
        skip_all
    )]
    pub async fn install_configuration(
        &self,
        pin: ContextScoutConfigurationPinV1,
        model_config: Option<ContextScoutModelConfig<'_>>,
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
        self.refresh_intent_engine(&pin);
        *configuration = Some(pin);
        Ok(())
    }

    /// Keeps the installed decision engine when the pinned intent settings
    /// are unchanged, builds one when they changed, and drops it when the
    /// pass is disabled or absent. A configuration the client refuses leaves
    /// no engine, which the pass reports as an unavailable engine.
    fn refresh_intent_engine(&self, pin: &ContextScoutConfigurationPinV1) {
        let Ok(mut installed) = self.intent_engine.lock() else {
            return;
        };
        let (Some(control), Some(settings)) = (pin.control().intent, pin.intent_settings()) else {
            *installed = None;
            return;
        };
        if !control.enabled {
            *installed = None;
            return;
        }
        if installed
            .as_ref()
            .is_some_and(|current| current.settings_digest == control.settings_digest)
        {
            return;
        }
        *installed = intent_engine_client(settings).map(|engine| InstalledIntentEngineV1 {
            settings_digest: control.settings_digest,
            engine,
        });
    }

    /// The decision engine of the currently installed intent configuration.
    pub async fn intent_engine(&self) -> Option<Arc<dyn DecisionEnginePortV1>> {
        let digest = self
            .configuration
            .read()
            .await
            .as_ref()?
            .control()
            .intent
            .filter(|intent| intent.enabled)?
            .settings_digest;
        self.intent_engine
            .lock()
            .ok()?
            .as_ref()
            .filter(|installed| installed.settings_digest == digest)
            .map(|installed| Arc::clone(&installed.engine))
    }

    /// Reads the session's Scout state for one intent pass whose suggestions
    /// live at `intent_address`. Pending work at that address is the pass's
    /// own superseded generation, never a sibling.
    pub async fn intent_session_state(
        &self,
        intent_address: ContextScoutAddressV1,
        now: UtcMicros,
        min_delivery_gap_micros: i64,
    ) -> Result<ContextScoutIntentSessionStateV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        let recent = match self
            .store
            .recent_for_protected_session(
                intent_address.protected_session_id,
                control.configuration_revision,
                now,
                STARTUP_RECOVERY_LIMIT,
            )
            .await
        {
            ContextScoutRecentReadOutcomeV1::Ready(recent) => recent,
            ContextScoutRecentReadOutcomeV1::Unavailable => {
                return Err(ContextScoutErrorV1::ConfigurationUnavailable);
            }
        };
        Ok(intent_session_state(
            &recent,
            intent_address,
            now,
            min_delivery_gap_micros,
        ))
    }

    /// Prepares one intent-pass selection. A suggestion pending at another
    /// address of the same session is suppressed as `PendingSibling`: the
    /// hook claim path refuses a session with more than one pending entry.
    pub async fn prepare_intent(
        &self,
        input: &ContextScoutSelectionInputV1,
        deadline: MonotonicDeadline,
        cancellation: CancellationToken,
    ) -> Result<ContextScoutRuntimeOutcomeV1, ContextScoutErrorV1> {
        let state = self
            .intent_session_state(input.address, input.now, 0)
            .await?;
        if state.pending_sibling {
            let mut runtime = self.runtime.lock().await;
            runtime.last_route = Some(ContextScoutRouteV1::DecisionEngine);
            runtime.last_suppression = Some(ContextScoutSuppressionV1::PendingSibling);
            return Ok(ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::PendingSibling,
            });
        }
        self.prepare_configured(input, deadline, cancellation).await
    }

    /// Records an intent pass that ended before selection (no candidate, or
    /// an engine failure with no action fact). The model slot describes the
    /// configured model and is left untouched.
    pub async fn record_intent_outcome(&self, suppression: ContextScoutSuppressionV1) {
        let mut runtime = self.runtime.lock().await;
        runtime.last_route = Some(ContextScoutRouteV1::DecisionEngine);
        runtime.last_suppression = Some(suppression);
    }

    /// Installs only an admitted active/paused control transition while
    /// preserving the already-selected model authority.
    #[tracing::instrument(
        name = "hosts.agent.context_scout.install_state_transition",
        level = "trace",
        skip_all
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

    #[tracing::instrument(name = "hosts.agent.context_scout.recent", level = "trace", skip_all)]
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

    pub async fn claim(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutDurableClaimOutcomeV1 {
        self.store.claim(address, now, lease).await
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
        // Serialize durable settlement with production so a completed public
        // lease cannot retain a slot or retire a newer producer generation.
        let mut runtime = self.runtime.lock().await;
        let settlement = self
            .store
            .commit_public_mutation(binding, mutation.clone())
            .await;
        if let ContextScoutMutationSettlementOutcomeV1::Reconciled(receipt) = &settlement {
            let retired = match (&mutation, &receipt.result) {
                (
                    ContextScoutPublicMutationV1::Cancel { work },
                    ContextScoutMutationResultV1::Cancel(
                        ContextScoutDurableStoreOutcomeV1::Stored
                        | ContextScoutDurableStoreOutcomeV1::Duplicate,
                    ),
                ) => Some((*work, None)),
                (
                    ContextScoutPublicMutationV1::Delivery { work, .. },
                    ContextScoutMutationResultV1::Delivery {
                        outcome:
                            ContextScoutDurableStoreOutcomeV1::Stored
                            | ContextScoutDurableStoreOutcomeV1::Duplicate,
                        receipt,
                    },
                ) => Some((*work, Some(receipt.outcome))),
                _ => None,
            };
            if let Some((work, delivery)) = retired
                && runtime.coalescer.is_current(work)
            {
                if runtime.coalescer.cancel(work).is_err() {
                    return ContextScoutMutationSettlementOutcomeV1::Unavailable;
                }
                if let Some(outcome) = delivery {
                    runtime.last_delivery_outcome = Some(outcome);
                } else {
                    runtime.last_suppression = Some(super::ContextScoutSuppressionV1::Cancelled);
                }
            }
        }
        settlement
    }
}

/// A producer mounted for a native lifecycle of the same routed session. The
/// session's intent-scoped mount is resolved by its own exact address, so it
/// never makes the native producer ambiguous.
fn same_routed_native_session(
    mounted: &ContextScoutLifecycleAddressV1,
    requested: &ContextScoutLifecycleAddressV1,
) -> bool {
    mounted.profile_id == requested.profile_id
        && mounted.provider_id == requested.provider_id
        && mounted.project_id == requested.project_id
        && mounted.worktree_id == requested.worktree_id
        && mounted.session_id == requested.session_id
        && !mounted.is_intent_scoped()
}

fn intent_engine_client(
    settings: &ContextScoutIntentSettingsV1,
) -> Option<Arc<dyn DecisionEnginePortV1>> {
    let client = DecisionEngineClientV1::new(DecisionEngineConfigV1 {
        base_url: settings.engine.base_url.clone(),
        model: settings.engine.model.clone(),
        timeout: std::time::Duration::from_millis(u64::from(settings.engine.timeout_ms)),
        max_concurrent: usize::from(settings.engine.max_concurrent),
        max_state_bytes: usize::try_from(settings.engine.max_state_bytes).ok()?,
    });
    match client {
        Ok(client) => Some(Arc::new(client)),
        Err(error) => {
            tracing::warn!(%error, "Context Scout decision engine configuration refused");
            None
        }
    }
}

fn intent_session_state(
    recent: &ContextScoutRecentStateV1,
    intent_address: ContextScoutAddressV1,
    now: UtcMicros,
    min_delivery_gap_micros: i64,
) -> ContextScoutIntentSessionStateV1 {
    let gap_start = now.0.saturating_sub(min_delivery_gap_micros);
    ContextScoutIntentSessionStateV1 {
        pending_sibling: recent
            .pending
            .iter()
            .any(|entry| entry.work.address != intent_address),
        delivered_within_gap: min_delivery_gap_micros > 0
            && recent
                .deliveries
                .iter()
                .any(|delivery| delivery.receipt.delivered_at.0 > gap_start),
        delivered_dedupe_keys: recent
            .deliveries
            .iter()
            .map(|delivery| delivery.entry.envelope.candidate.dedupe_key)
            .collect(),
        delivered_intents: recent
            .deliveries
            .iter()
            .filter_map(|delivery| delivery.entry.envelope.candidate.evidence.as_session())
            .map(|evidence| evidence.decision.selected)
            .collect(),
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
        && current.intent == next.intent
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
    let entries = || {
        recent
            .pending
            .iter()
            .chain(recent.deliveries.iter().map(|delivery| &delivery.entry))
    };
    let Some(entry) = entries().max_by_key(|entry| entry.envelope.candidate.expires_at) else {
        return status;
    };
    status.last_route = Some(entry.route);
    if let Some(model_entry) = entries()
        .filter(|entry| entry.route != ContextScoutRouteV1::DecisionEngine)
        .max_by_key(|entry| entry.envelope.candidate.expires_at)
    {
        status.last_model_outcome = Some(model_entry.model_outcome);
        status
            .last_model_receipt
            .clone_from(&model_entry.model_receipt);
    }
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
    use super::super::address_registry::ContextScoutAddressBindOutcomeV1;
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use tracedecay_contracts::context_scout::{
        ContextScoutCandidateV1, ContextScoutCategoryV1, ContextScoutModelReceiptV1,
    };
    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::NativeHostIdentityV1;
    use tracedecay_domain::canonical_sha256;
    use tracedecay_domain::configuration::{
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY, CandidateDispositionV1, ConfigurationCandidateV1,
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
        ConfigurationValueV1, ContextScoutConfigurationModeV1, ContextScoutConfigurationStateV1,
        ContextScoutConfiguredModelPathV1, ContextScoutSettingsV1, DecisionEngineSettingsV1,
        IntentWindowSettingsV1, SettingKey,
    };
    use tracedecay_domain::feedback::FeedbackScopeV1;
    use tracedecay_domain::{ActorId, RepositoryId, WorktreeId};
    use tracedecay_global_db::configuration::contracts::ConfigurationCurrentStateV1;
    use tracedecay_hooks::{
        HookCapabilityV1, HookEventFamily, HookScopeBindingV1, NativeEnvelopeMaterialV1,
        decode_bound_native_hook_event, stock_event_support,
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
            intent: None,
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

        let changed_intent = ContextScoutControlV1 {
            intent: Some(super::super::ContextScoutIntentControlV1 {
                enabled: true,
                settings_digest: [9; 32],
            }),
            ..paused
        };
        assert!(!context_scout_state_transition_is_exact(
            current,
            changed_intent
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

        // An intent-pass suggestion is claimable at the first later boundary
        // of any kind, including the phase-two prompt lookup, unless quiet
        // mode or a delivery inside the spacing gap withholds it from hooks.
        let intent_window = |quiet_mode, has_recent_delivery| {
            super::super::select_context_scout_delivery_window(
                &super::super::ContextScoutDeliverySelectionInputV1 {
                    trigger: super::super::ContextScoutTriggerV1::IntentBoundary,
                    quiet_mode,
                    has_recent_delivery,
                    has_unresolved_interaction: false,
                    critical_safety_evidence: false,
                    delivered_dedupe_keys: BTreeSet::new(),
                },
            )
        };
        assert_eq!(
            intent_window(false, false),
            ContextScoutDeliveryWindowV1::Immediate
        );
        for boundary in [&prompt_boundary, &tool_completed, &session_end] {
            assert!(delivery_window_admitted_at_hook(
                intent_window(false, false),
                boundary,
            ));
        }
        assert_eq!(
            intent_window(true, false),
            ContextScoutDeliveryWindowV1::Suppressed
        );
        assert_eq!(
            intent_window(false, true),
            ContextScoutDeliveryWindowV1::IdleWindow
        );
        for boundary in [&prompt_boundary, &tool_completed, &session_end] {
            assert!(!delivery_window_admitted_at_hook(
                intent_window(false, true),
                boundary,
            ));
        }
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
            host: NativeHostIdentityV1::ClaudeCode,
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
                support: stock_event_support(NativeHostIdentityV1::ClaudeCode, family),
            })
            .collect(),
        };
        let envelope = decode_bound_native_hook_event(
            NativeHostIdentityV1::ClaudeCode,
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
    async fn explicit_producer_input_requires_current_exact_scope_and_configuration() {
        let owner = test_owner([39; 16]).await;
        let (_temporary, database) = test_database().await;
        let (registry, hook, pin, context, observed_at) = claim_mount_authority(database);
        let configuration = pin.configuration().clone();
        owner
            .install_configuration(configuration.clone(), None)
            .await
            .unwrap();
        let lifecycle = claim_lifecycle(1);
        let ContextScoutAddressBindOutcomeV1::Bound(address) =
            registry.bind(&hook, &pin, lifecycle.clone()).await
        else {
            panic!("bind exact lifecycle");
        };
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    registry,
                    &hook,
                    7,
                    pin,
                    context.clone(),
                    lifecycle.clone(),
                    address,
                    [1; 32],
                    observed_at,
                    true,
                )
                .await,
            ContextScoutClaimAdmissionV1::Mounted
        );
        let (admitted, retained_lifecycle, revision) = owner
            .current_producer_input(address, &configuration, context.scope())
            .await
            .unwrap();
        assert_eq!(admitted.envelope(), hook.envelope());
        assert_eq!(retained_lifecycle, lifecycle);
        assert_eq!(revision, 7);
        let mut foreign_address = address;
        foreign_address.turn_id = [99; 16];
        assert!(
            owner
                .current_producer_input(foreign_address, &configuration, context.scope())
                .await
                .is_none()
        );
        let foreign_scope = ResolvedScope::new(
            context.scope().project_id.clone(),
            context.scope().repository_id.clone(),
            WorktreeId::new("worktree.foreign").unwrap(),
            context.scope().reference.clone(),
        )
        .unwrap();
        assert!(
            owner
                .current_producer_input(address, &configuration, &foreign_scope)
                .await
                .is_none()
        );
        // Configuration unavailability cannot turn a retained hook into
        // independent production authority.
        *owner.configuration.write().await = None;
        assert!(
            owner
                .current_producer_input(address, &configuration, context.scope())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn later_session_event_resolves_one_retained_exact_claim_and_refuses_ambiguity() {
        let project_id = [40; 16];
        let owner = test_owner(project_id).await;
        let (_temporary, database) = test_database().await;
        let (registry, hook, pin, context, observed_at) = claim_mount_authority(database);
        let producer_lifecycle = claim_lifecycle(1);
        let producer_address = match registry.bind(&hook, &pin, producer_lifecycle.clone()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected producer address, got {other:?}"),
        };
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    Arc::clone(&registry),
                    &hook,
                    1,
                    pin.clone(),
                    context.clone(),
                    producer_lifecycle,
                    producer_address,
                    [1; 32],
                    observed_at,
                    true,
                )
                .await,
            ContextScoutClaimAdmissionV1::Mounted
        );

        let later_lifecycle = claim_lifecycle(2);
        assert_eq!(
            owner
                .resolve_current_session_claim_authority(
                    &hook,
                    &later_lifecycle,
                    &pin,
                    observed_at,
                    true,
                )
                .await,
            Some((producer_address, [1; 32]))
        );

        // The session's intent-scoped lifecycle is one address for every
        // turn: it binds beside the native lifecycle, rebinds to the same
        // address, and resolves only by its own exact lifecycle, so it never
        // makes the native producer ambiguous.
        let intent_lifecycle = later_lifecycle.intent_scoped().expect("intent lifecycle");
        assert!(intent_lifecycle.is_intent_scoped());
        assert!(!later_lifecycle.is_intent_scoped());
        assert_eq!(
            claim_lifecycle(7).intent_scoped(),
            Some(intent_lifecycle.clone())
        );
        let intent_address = match registry.bind(&hook, &pin, intent_lifecycle.clone()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected intent address, got {other:?}"),
        };
        assert_eq!(
            registry.bind(&hook, &pin, intent_lifecycle.clone()).await,
            ContextScoutAddressBindOutcomeV1::Existing(intent_address)
        );
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    Arc::clone(&registry),
                    &hook,
                    1,
                    pin.clone(),
                    context.clone(),
                    intent_lifecycle.clone(),
                    intent_address,
                    [3; 32],
                    observed_at,
                    true,
                )
                .await,
            ContextScoutClaimAdmissionV1::Mounted
        );
        assert_eq!(
            owner
                .resolve_current_session_claim_authority(
                    &hook,
                    &later_lifecycle,
                    &pin,
                    observed_at,
                    true,
                )
                .await,
            Some((producer_address, [1; 32])),
            "the intent mount never makes the session's native producer ambiguous"
        );
        assert_eq!(
            owner
                .resolve_current_claim_authority(&hook, &intent_lifecycle, observed_at, true)
                .await,
            Some((intent_address, [3; 32]))
        );

        let second_address = match registry.bind(&hook, &pin, later_lifecycle.clone()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address) => address,
            other => panic!("expected second producer address, got {other:?}"),
        };
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    registry,
                    &hook,
                    1,
                    pin.clone(),
                    context,
                    later_lifecycle.clone(),
                    second_address,
                    [2; 32],
                    observed_at,
                    true,
                )
                .await,
            ContextScoutClaimAdmissionV1::Mounted
        );
        assert!(
            owner
                .resolve_current_session_claim_authority(
                    &hook,
                    &later_lifecycle,
                    &pin,
                    observed_at,
                    true,
                )
                .await
                .is_none(),
            "more than one mounted producer in the session must remain ambiguous"
        );
        unregister(project_id, &owner);
    }

    #[tokio::test]
    async fn public_settlement_retires_exact_runtime_work_without_exhausting_capacity() {
        let (_temporary, database) = test_database().await;
        let owner = ProjectContextScoutOwnerV1::startup(database, [10; 16], UtcMicros(1), None)
            .await
            .unwrap();
        for marker in 1..=34 {
            let mut entry = super::super::store_tests::entry([10; 16], 1);
            entry.work.address.logical_message_id = [marker; 16];
            entry.envelope.address = entry.work.address;
            owner
                .runtime
                .lock()
                .await
                .coalescer
                .restore(entry.work)
                .unwrap();
            assert_eq!(
                owner.store.enqueue(entry.clone()).await,
                ContextScoutDurableStoreOutcomeV1::Stored
            );
            let mutation = ContextScoutPublicMutationV1::Cancel { work: entry.work };
            let binding =
                super::super::store_tests::mutation_binding(&mutation, &format!("cancel-{marker}"));
            assert!(matches!(
                owner.commit_public_mutation(binding, mutation).await,
                ContextScoutMutationSettlementOutcomeV1::Reconciled(_)
            ));
            assert!(!owner.runtime.lock().await.coalescer.is_current(entry.work));
        }
        let mut entry = super::super::store_tests::entry([10; 16], 1);
        entry.work.address.logical_message_id = [99; 16];
        entry.envelope.address = entry.work.address;
        owner
            .runtime
            .lock()
            .await
            .coalescer
            .restore(entry.work)
            .unwrap();
        assert_eq!(
            owner.store.enqueue(entry.clone()).await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );
        let lease = ContextScoutLeaseV1 {
            lease_id: [81; 16],
            expires_at: UtcMicros(900),
        };
        assert!(matches!(
            owner
                .store
                .claim(entry.work.address, UtcMicros(20), lease)
                .await,
            ContextScoutDurableClaimOutcomeV1::Claimed(_)
        ));
        let mutation = ContextScoutPublicMutationV1::Delivery {
            work: entry.work,
            envelope_id: entry.envelope.envelope_id,
            lease,
            configuration_revision: entry.envelope.configuration_revision,
            receipt: ContextScoutDeliveryReceiptV1 {
                receipt_id: [82; 16],
                envelope_id: entry.envelope.envelope_id,
                delivered_at: UtcMicros(30),
                outcome: super::super::ContextScoutDeliveryOutcomeV1::Displayed,
            },
        };
        let binding = super::super::store_tests::mutation_binding(&mutation, "delivery-retirement");
        assert!(matches!(
            owner
                .commit_public_mutation(binding.clone(), mutation.clone())
                .await,
            ContextScoutMutationSettlementOutcomeV1::Reconciled(_)
        ));
        assert!(!owner.runtime.lock().await.coalescer.is_current(entry.work));
        let mut newer = entry.work;
        newer.generation += 1;
        owner.runtime.lock().await.coalescer.restore(newer).unwrap();
        assert!(matches!(
            owner.commit_public_mutation(binding, mutation).await,
            ContextScoutMutationSettlementOutcomeV1::Reconciled(_)
        ));
        assert!(owner.runtime.lock().await.coalescer.is_current(newer));
        unregister([10; 16], &owner);
    }

    #[tokio::test]
    async fn exact_hook_event_reclaims_its_lease_and_foreign_event_cannot() {
        let (_registry_temporary, registry_database) = test_database().await;
        let (_registry, admitted, pin, _context, observed_at) =
            claim_mount_authority(registry_database);
        let address = ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: admitted.envelope().protected_session_id,
            thread_id: [3; 16],
            turn_id: [4; 16],
            agent_id: [5; 16],
            logical_message_id: [6; 16],
            project_id: admitted.envelope().project_id,
        };
        let (_owner_temporary, owner_database) = test_database().await;
        let owner = ProjectContextScoutOwnerV1::startup(
            owner_database,
            address.project_id,
            UtcMicros(1),
            None,
        )
        .await
        .expect("owner");
        owner
            .install_configuration(pin.configuration().clone(), None)
            .await
            .expect("current configuration");
        let input_watermark = [14; 32];
        let boundary_watermark = [15; 32];
        let entry = ContextScoutDurableQueueEntryV1 {
            work: ContextScoutWorkV1 {
                address,
                generation: 1,
                input_watermark,
            },
            route: super::super::ContextScoutRouteV1::Deterministic,
            model_outcome: ContextScoutModelOutcomeV1::NotRequested,
            model_receipt: None,
            envelope: tracedecay_contracts::context_scout::ContextScoutSuggestionEnvelopeV1 {
                envelope_id: [17; 16],
                address,
                input_watermark,
                configuration_revision: pin.configuration().control().configuration_revision,
                delivery_window: ContextScoutDeliveryWindowV1::Immediate,
                candidate: ContextScoutCandidateV1 {
                    dedupe_key: [18; 32],
                    category: ContextScoutCategoryV1::Retrieval,
                    relevance_score: 10,
                    suggestion_text: "Use the saved diagnostic anchor.".to_owned(),
                    evidence: super::super::evidence::fixture_context_scout_evidence().into(),
                    expires_at: UtcMicros(1_000),
                },
            },
        };
        assert_eq!(
            owner.store().enqueue(entry.clone()).await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );

        let first = owner
            .claim_ready_guidance_exact(
                admitted.envelope(),
                address,
                boundary_watermark,
                1,
                UtcMicros(observed_at.0 + 1),
            )
            .await
            .expect("first claim");
        let replay = owner
            .claim_ready_guidance_exact(
                admitted.envelope(),
                address,
                boundary_watermark,
                1,
                UtcMicros(observed_at.0 + 2),
            )
            .await
            .expect("same event reclaims its durable lease");
        assert_eq!(replay, first);

        let mut foreign = admitted.envelope().clone();
        foreign.event_id = [99; 16];
        assert!(
            owner
                .claim_ready_guidance_exact(
                    &foreign,
                    address,
                    boundary_watermark,
                    1,
                    UtcMicros(observed_at.0 + 3),
                )
                .await
                .is_none(),
            "a foreign event must not acquire an active lease"
        );
        unregister(address.project_id, &owner);
    }

    fn active_intent_configuration(base: ContextScoutSettingsV1) -> ContextScoutConfigurationPinV1 {
        let setting_key = SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).expect("setting");
        let revision_id = ConfigurationRevisionId::new("revision.scout.intent").expect("revision");
        let settings = ContextScoutSettingsV1 {
            state: ContextScoutConfigurationStateV1::Active,
            intent: Some(ContextScoutIntentSettingsV1 {
                enabled: true,
                engine: DecisionEngineSettingsV1 {
                    base_url: "http://127.0.0.1:9".to_owned(),
                    model: Some("intent-small".to_owned()),
                    timeout_ms: 250,
                    max_concurrent: 1,
                    max_state_bytes: 4_096,
                },
                window: IntentWindowSettingsV1 {
                    max_user_turns: 2,
                    max_messages: 8,
                    max_bytes: 4_096,
                    max_age_secs: 600,
                    freshen_budget_ms: 0,
                },
                questions: ContextScoutIntentSettingsV1::reference_questions(),
                action_fact_score_permille: 700,
                ttl_secs: 600,
                min_delivery_gap_secs: 30,
                prompt_lookup_deadline_ms: 200,
            }),
            ..base
        };
        settings.validate().expect("intent settings");
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                setting_key.clone(),
                ConfigurationValueV1::ContextScoutSettings(settings),
            )]),
            BTreeMap::from([(
                setting_key,
                vec![ConfigurationCandidateV1 {
                    layer: ConfigurationLayerIdV1::Project {
                        project_id: claim_id("project.scout.fixture"),
                    },
                    revision_id: revision_id.clone(),
                    disposition: CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .expect("snapshot");
        ContextScoutConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
            revision_id,
            snapshot,
        })
        .expect("intent configuration pin")
    }

    #[tokio::test]
    async fn intent_entries_refuse_a_pending_sibling_and_supersede_their_own_generation() {
        let (_registry_temporary, registry_database) = test_database().await;
        let (_registry, admitted, _pin, _context, observed_at) =
            claim_mount_authority(registry_database);
        let sibling_address = ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: admitted.envelope().protected_session_id,
            thread_id: [3; 16],
            turn_id: [4; 16],
            agent_id: [5; 16],
            logical_message_id: [6; 16],
            project_id: admitted.envelope().project_id,
        };
        let intent_address = ContextScoutAddressV1 {
            thread_id: [11; 16],
            turn_id: [11; 16],
            agent_id: [11; 16],
            logical_message_id: [11; 16],
            ..sibling_address
        };
        let (_owner_temporary, owner_database) = test_database().await;
        let owner = ProjectContextScoutOwnerV1::startup(
            owner_database,
            sibling_address.project_id,
            UtcMicros(1),
            None,
        )
        .await
        .expect("owner");
        let configuration = active_intent_configuration(ContextScoutSettingsV1::disabled());
        owner
            .install_configuration(configuration.clone(), None)
            .await
            .expect("intent configuration");
        let engine = owner.intent_engine().await.expect("installed engine");
        owner
            .install_configuration(configuration.clone(), None)
            .await
            .expect("reinstalled intent configuration");
        assert!(
            Arc::ptr_eq(
                &engine,
                &owner.intent_engine().await.expect("installed engine")
            ),
            "unchanged intent settings keep the installed engine"
        );
        let revision = configuration.control().configuration_revision;
        // Recent reads filter on the wall clock; keep the fixture unexpired.
        let expires_at = UtcMicros(4_000_000_000_000_000);

        let sibling = ContextScoutDurableQueueEntryV1 {
            work: ContextScoutWorkV1 {
                address: sibling_address,
                generation: 1,
                input_watermark: [14; 32],
            },
            route: super::super::ContextScoutRouteV1::Deterministic,
            model_outcome: ContextScoutModelOutcomeV1::NotRequested,
            model_receipt: None,
            envelope: tracedecay_contracts::context_scout::ContextScoutSuggestionEnvelopeV1 {
                envelope_id: [17; 16],
                address: sibling_address,
                input_watermark: [14; 32],
                configuration_revision: revision,
                delivery_window: ContextScoutDeliveryWindowV1::NextBoundary,
                candidate: ContextScoutCandidateV1 {
                    dedupe_key: [18; 32],
                    category: ContextScoutCategoryV1::Diagnostic,
                    relevance_score: 10,
                    suggestion_text: "Use the saved diagnostic anchor.".to_owned(),
                    evidence: super::super::evidence::fixture_context_scout_evidence().into(),
                    expires_at,
                },
            },
        };
        assert_eq!(
            owner.store().enqueue(sibling.clone()).await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );

        let evidence = super::super::evidence::fixture_context_scout_session_evidence();
        let intent_input = |envelope: u8, watermark: u8| ContextScoutSelectionInputV1 {
            address: intent_address,
            input_watermark: [watermark; 32],
            configuration_revision: revision,
            envelope_id: [envelope; 16],
            now: UtcMicros(observed_at.0 + 1),
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            delivered_dedupe_keys: BTreeSet::new(),
            candidates: vec![ContextScoutCandidateV1 {
                dedupe_key: [21; 32],
                category: ContextScoutCategoryV1::Retrieval,
                relevance_score: 900,
                suggestion_text: "tracedecay hint: recall the earlier session.".to_owned(),
                evidence: evidence.clone().into(),
                expires_at,
            }],
        };
        let deadline =
            || MonotonicDeadline::at(std::time::Instant::now() + std::time::Duration::from_secs(1));

        assert_eq!(
            owner
                .prepare_intent(&intent_input(22, 23), deadline(), CancellationToken::new())
                .await
                .expect("prepare beside a sibling"),
            ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::PendingSibling,
            }
        );
        assert_eq!(
            owner
                .configured_status()
                .await
                .expect("status")
                .last_suppression,
            Some(ContextScoutSuppressionV1::PendingSibling)
        );
        assert_eq!(
            owner
                .recent_exact(intent_address, 8)
                .await
                .expect("intent address")
                .pending
                .len(),
            0,
            "a suppressed pass enqueues nothing"
        );

        assert_eq!(
            owner.store().cancel_work(sibling.work).await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );
        let ContextScoutRuntimeOutcomeV1::Enqueued { entry: first, .. } = owner
            .prepare_intent(&intent_input(22, 23), deadline(), CancellationToken::new())
            .await
            .expect("prepare without a sibling")
        else {
            panic!("an intent entry without a pending sibling must enqueue");
        };
        assert_eq!(first.route, ContextScoutRouteV1::DecisionEngine);
        assert_eq!(first.model_outcome, evidence.decision.outcome);
        assert_eq!(first.model_receipt, None);
        assert_eq!(first.work.generation, 1);
        let status = owner.configured_status().await.expect("status");
        assert_eq!(status.last_route, Some(ContextScoutRouteV1::DecisionEngine));
        assert_eq!(
            status.last_model_outcome, None,
            "the model slot describes the configured model, not the intent engine"
        );

        let ContextScoutRuntimeOutcomeV1::Enqueued { entry: second, .. } = owner
            .prepare_intent(&intent_input(24, 25), deadline(), CancellationToken::new())
            .await
            .expect("newer pass")
        else {
            panic!("a newer pass at the intent address must enqueue");
        };
        assert_eq!(second.work.generation, 2);
        let recent = owner
            .recent_exact(intent_address, 8)
            .await
            .expect("intent address");
        assert_eq!(
            recent.pending,
            vec![*second.clone()],
            "a newer pass supersedes the older generation at the one intent address"
        );

        let (guidance, claim) = owner
            .claim_ready_guidance_exact(
                admitted.envelope(),
                intent_address,
                [25; 32],
                1,
                UtcMicros(observed_at.0 + 2),
            )
            .await
            .expect("intent guidance claims at the next hook boundary");
        assert_eq!(guidance.guidance_id, [24; 16]);
        let delivered_at = UtcMicros(observed_at.0 + 3);
        assert_eq!(
            owner
                .record_delivery(
                    &claim,
                    &ContextScoutDeliveryReceiptV1 {
                        receipt_id: super::super::context_scout_delivery_receipt_id(
                            admitted.envelope().event_id,
                            guidance.guidance_id,
                        ),
                        envelope_id: guidance.guidance_id,
                        delivered_at,
                        outcome: super::super::ContextScoutDeliveryOutcomeV1::Displayed,
                    },
                )
                .await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );
        let gap = 30_000_000;
        let spaced = owner
            .intent_session_state(intent_address, UtcMicros(delivered_at.0 + 1), gap)
            .await
            .expect("session state");
        assert!(!spaced.pending_sibling);
        assert!(spaced.delivered_within_gap);
        assert_eq!(
            spaced.delivered_intents,
            BTreeSet::from([ContextScoutIntentKindV1::SessionRecall])
        );
        assert!(spaced.delivered_dedupe_keys.contains(&[21; 32]));
        let after_gap = owner
            .intent_session_state(intent_address, UtcMicros(delivered_at.0 + gap + 1), gap)
            .await
            .expect("session state after the gap");
        assert!(
            !after_gap.delivered_within_gap,
            "spacing counts deliveries inside the gap, not every delivery ever"
        );
        assert_eq!(after_gap.delivered_intents, spaced.delivered_intents);
        unregister(sibling_address.project_id, &owner);
    }

    #[tokio::test]
    async fn intent_pass_outcomes_leave_the_configured_model_slot_alone() {
        let (_temporary, database) = test_database().await;
        let owner = ProjectContextScoutOwnerV1::startup(database, [43; 16], UtcMicros(1), None)
            .await
            .expect("owner");
        let configuration = active_intent_configuration(ContextScoutSettingsV1 {
            mode: ContextScoutConfigurationModeV1::ConfiguredModel,
            model_path: Some(ContextScoutConfiguredModelPathV1::CodexAppServer),
            model_id: Some("configured-test-model".to_owned()),
            model_timeout_secs: Some(60),
            ..ContextScoutSettingsV1::disabled()
        });
        let revision = configuration.control().configuration_revision;
        owner
            .install_configuration(configuration, None)
            .await
            .expect("configured model installation");

        owner.runtime.lock().await.last_model_outcome = Some(ContextScoutModelOutcomeV1::Succeeded);
        owner
            .record_intent_outcome(ContextScoutSuppressionV1::NoEligibleCandidate)
            .await;
        let capability = owner.capability().await.expect("capability");
        assert!(capability.configured_model_available);
        assert_eq!(
            capability.last_model_outcome,
            Some(ContextScoutModelOutcomeV1::Succeeded)
        );

        {
            let mut runtime = owner.runtime.lock().await;
            runtime.last_model_outcome = Some(ContextScoutModelOutcomeV1::TokenBudgetExceeded);
            runtime.last_model_receipt = Some(ContextScoutModelReceiptV1 {
                requested_backend: ContextScoutModelBackendV1::CodexAppServer,
                actual_provider: Some("codex".to_owned()),
                actual_model: Some("configured-test-model".to_owned()),
                input_tokens: Some(10),
                output_tokens: Some(5),
                estimated_cost_microusd: Some(1),
            });
        }
        let address = ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: [3; 32],
            thread_id: [4; 16],
            turn_id: [5; 16],
            agent_id: [6; 16],
            logical_message_id: [7; 16],
            project_id: [43; 16],
        };
        let evidence = super::super::evidence::fixture_context_scout_session_evidence();
        let intent_entry = ContextScoutDurableQueueEntryV1 {
            work: ContextScoutWorkV1 {
                address,
                generation: 1,
                input_watermark: [8; 32],
            },
            route: ContextScoutRouteV1::DecisionEngine,
            model_outcome: evidence.decision.outcome,
            model_receipt: None,
            envelope: tracedecay_contracts::context_scout::ContextScoutSuggestionEnvelopeV1 {
                envelope_id: [9; 16],
                address,
                input_watermark: [8; 32],
                configuration_revision: revision,
                delivery_window: ContextScoutDeliveryWindowV1::NextBoundary,
                candidate: ContextScoutCandidateV1 {
                    dedupe_key: [10; 32],
                    category: ContextScoutCategoryV1::Retrieval,
                    relevance_score: 900,
                    suggestion_text: "tracedecay hint: recall the earlier session.".to_owned(),
                    evidence: evidence.into(),
                    expires_at: UtcMicros(4_000_000_000_000_000),
                },
            },
        };
        assert_eq!(
            owner.store().enqueue(intent_entry).await,
            ContextScoutDurableStoreOutcomeV1::Stored
        );
        owner
            .record_intent_outcome(ContextScoutSuppressionV1::NoEligibleCandidate)
            .await;

        let budget = owner.budget().await.expect("budget");
        assert!(budget.exhausted);
        assert_eq!(
            budget.last_model_outcome,
            Some(ContextScoutModelOutcomeV1::TokenBudgetExceeded)
        );
        assert_eq!(
            (budget.last_input_tokens, budget.last_output_tokens),
            (Some(10), Some(5))
        );
        assert_eq!(
            owner.configured_status().await.expect("status").last_route,
            Some(ContextScoutRouteV1::DecisionEngine)
        );
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
                        1,
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
                    1,
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
