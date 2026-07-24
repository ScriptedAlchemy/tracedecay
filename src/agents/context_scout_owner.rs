use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Mutex, RwLock};
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{HookEventEnvelopeV2, HookReadyGuidanceV1};

use super::context_scout_model::context_scout_model_assistant_from_project_config;
use super::context_scout_ports::ContextScoutConfigurationPinV1;
use super::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutControlV1, ContextScoutDeliveryReceiptV1,
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableClaimV1, ContextScoutDurableRuntimeV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutErrorV1, ContextScoutFeedbackV1, ContextScoutLeaseV1,
    ContextScoutModelAssistantV1, ContextScoutModelBackendV1, ContextScoutModelErrorV1,
    ContextScoutModelExecutionV1, ContextScoutModelFuture, ContextScoutModelRequestV1,
    ContextScoutRuntimeOutcomeV1, ContextScoutSelectionInputV1, ContextScoutStatusV1,
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

pub struct ProjectContextScoutOwnerV1 {
    store: Arc<ProjectContextScoutDurableStoreV1>,
    runtime: Mutex<ProjectScoutRuntime>,
    configuration: RwLock<Option<ContextScoutConfigurationPinV1>>,
    inflight: StdMutex<BTreeMap<ContextScoutAddressV1, (u64, CancellationToken)>>,
    next_inflight_id: AtomicU64,
    startup: ContextScoutDurableStartupOutcomeV1,
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
        let runtime = ContextScoutDurableRuntimeV1::new(Arc::clone(&store), model);
        Some(Arc::new(Self {
            store,
            runtime: Mutex::new(runtime),
            configuration: RwLock::new(None),
            inflight: StdMutex::new(BTreeMap::new()),
            next_inflight_id: AtomicU64::new(1),
            startup,
        }))
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
        let entry = entries.into_iter().find(|entry| {
            entry.work.address.project_id == hook.project_id
                && entry.work.address.protected_session_id == hook.protected_session_id
                && entry.envelope.configuration_revision == control.configuration_revision
        })?;
        let lease = ContextScoutLeaseV1 {
            lease_id: hook.event_id,
            expires_at: UtcMicros(now.0.saturating_add(DELIVERY_LEASE_MICROS)),
        };
        let claimed = match self.store.claim(entry.work.address, now, lease).await {
            ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
            ContextScoutDurableClaimOutcomeV1::Empty
            | ContextScoutDurableClaimOutcomeV1::Unavailable => return None,
        };
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
            .complete_delivery(&claim.entry, receipt)
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
        self.runtime.lock().await.status(control)
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

    pub async fn configured_status(&self) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        let configuration = self.configuration.read().await;
        let control = configuration
            .as_ref()
            .ok_or(ContextScoutErrorV1::ConfigurationUnavailable)?
            .control();
        self.runtime.lock().await.status(control)
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
