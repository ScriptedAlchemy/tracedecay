use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, RwLock};
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookLifecyclePhaseV1, HookReadyGuidanceV1,
};

use super::context_scout_model::context_scout_model_assistant_from_project_config;
use super::context_scout_ports::ContextScoutConfigurationPinV1;
use super::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutBudgetStateV1, ContextScoutCapabilityStateV1,
    ContextScoutControlV1, ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1,
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableClaimV1, ContextScoutDurableRuntimeV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutErrorV1, ContextScoutExplanationV1,
    ContextScoutFeedbackV1, ContextScoutLeaseV1, ContextScoutModelAssistantV1,
    ContextScoutModelBackendV1, ContextScoutModelErrorV1, ContextScoutModelExecutionV1,
    ContextScoutModelFuture, ContextScoutModelRequestV1, ContextScoutModelRunOutcomeV1,
    ContextScoutRecentReadOutcomeV1, ContextScoutRecentStateV1, ContextScoutRuntimeOutcomeV1,
    ContextScoutSelectionInputV1, ContextScoutServiceStateV1, ContextScoutStatusV1,
    ContextScoutWorkV1, ProjectContextScoutDurableStoreV1,
};
use crate::application::context::{CancellationToken, MonotonicDeadline};
use crate::automation::config::AutomationConfig;
use crate::db::Database;

const STARTUP_RECOVERY_LIMIT: usize = 32;
const DELIVERY_LEASE_MICROS: i64 = 30 * 1_000_000;

pub(crate) type ProjectScoutRuntime = ContextScoutDurableRuntimeV1<
    Arc<ProjectContextScoutDurableStoreV1>,
    Arc<dyn ContextScoutModelAssistantV1>,
>;

type ProjectContextScoutOwnerRegistry = BTreeMap<[u8; 16], Vec<Weak<ProjectContextScoutOwnerV1>>>;

pub struct ProjectContextScoutOwnerV1 {
    store: Arc<ProjectContextScoutDurableStoreV1>,
    runtime: Mutex<ProjectScoutRuntime>,
    configuration: RwLock<Option<ContextScoutConfigurationPinV1>>,
    inflight: StdMutex<BTreeMap<ContextScoutAddressV1, (u64, CancellationToken)>>,
    next_inflight_id: AtomicU64,
    startup: ContextScoutDurableStartupOutcomeV1,
}

fn registered_context_scout_owners() -> &'static StdMutex<ProjectContextScoutOwnerRegistry> {
    static OWNERS: OnceLock<StdMutex<ProjectContextScoutOwnerRegistry>> = OnceLock::new();
    OWNERS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub fn lookup_registered_context_scout_owners(
    project_id: [u8; 16],
) -> Vec<Arc<ProjectContextScoutOwnerV1>> {
    let Ok(mut owners) = registered_context_scout_owners().lock() else {
        return Vec::new();
    };
    let Some(project_owners) = owners.get_mut(&project_id) else {
        return Vec::new();
    };
    project_owners.retain(|owner| owner.strong_count() > 0);
    project_owners.iter().filter_map(Weak::upgrade).collect()
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

    pub async fn startup(
        database: Database,
        project_id: [u8; 16],
        now: UtcMicros,
        model_config: Option<&AutomationConfig>,
    ) -> Option<Arc<Self>> {
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
        });
        let mut owners = registered_context_scout_owners().lock().ok()?;
        let project_owners = owners.entry(project_id).or_default();
        project_owners.retain(|owner| owner.strong_count() > 0);
        project_owners.push(Arc::downgrade(&owner));
        Some(owner)
    }

    pub fn store(&self) -> Arc<ProjectContextScoutDurableStoreV1> {
        Arc::clone(&self.store)
    }

    pub fn startup_outcome(&self) -> &ContextScoutDurableStartupOutcomeV1 {
        &self.startup
    }

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
        entry: crate::agents::context_scout_v2::ContextScoutDurableQueueEntryV1,
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
                    ContextScoutModelRunOutcomeV1::Disabled
                        | ContextScoutModelRunOutcomeV1::Unavailable
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
                == Some(ContextScoutModelRunOutcomeV1::TokenBudgetExceeded),
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
        let _ = self.store.requeue(claim).await;
        ContextScoutDurableClaimOutcomeV1::Empty
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
    use super::*;

    #[test]
    fn state_transition_rejects_model_route_or_limit_drift() {
        let current = ContextScoutControlV1 {
            configuration_revision: [1; 32],
            state: super::super::context_scout_v2::ContextScoutServiceStateV1::Active,
            mode: super::super::context_scout_v2::ContextScoutRuntimeModeV1::ConfiguredModel,
            model_path: Some(ContextScoutModelBackendV1::CodexAppServer),
            limits: super::super::context_scout_v2::ContextScoutLimitsV1::bounded_defaults(),
        };
        let paused = ContextScoutControlV1 {
            configuration_revision: [2; 32],
            state: super::super::context_scout_v2::ContextScoutServiceStateV1::Paused,
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
            limits: super::super::context_scout_v2::ContextScoutLimitsV1 {
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
}
