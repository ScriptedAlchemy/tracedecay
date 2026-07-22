use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{HookEventEnvelopeV2, HookReadyGuidanceV1};

use super::context_scout_model::context_scout_model_assistant_from_project_config;
use super::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutControlV1, ContextScoutDeliveryReceiptV1,
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableClaimV1, ContextScoutDurableRuntimeV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutErrorV1, ContextScoutFeedbackV1, ContextScoutLeaseV1,
    ContextScoutModelAssistantV1, ContextScoutStatusV1, ContextScoutWorkV1,
    ProjectContextScoutDurableStoreV1,
};
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
    startup: ContextScoutDurableStartupOutcomeV1,
}

impl ProjectContextScoutOwnerV1 {
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
        let ready = self.store.startup(now, STARTUP_RECOVERY_LIMIT).await;
        let entries = match ready {
            ContextScoutDurableStartupOutcomeV1::Ready { entries, .. } => entries,
            ContextScoutDurableStartupOutcomeV1::Unavailable => return None,
        };
        let entry = entries.into_iter().find(|entry| {
            entry.work.address.project_id == hook.project_id
                && entry.work.address.protected_session_id == hook.protected_session_id
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
        self.store.record_delivery(&claim.entry, receipt).await
    }

    pub async fn record_feedback(
        &self,
        receipt: &ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        self.store.record_feedback(receipt, feedback).await
    }

    pub(crate) async fn runtime(&self) -> tokio::sync::MutexGuard<'_, ProjectScoutRuntime> {
        self.runtime.lock().await
    }

    pub async fn status(
        &self,
        control: ContextScoutControlV1,
    ) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        self.runtime.lock().await.status(control)
    }

    pub async fn cancel(
        &self,
        work: ContextScoutWorkV1,
    ) -> Result<ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1> {
        self.runtime.lock().await.cancel(work).await
    }

    pub async fn configure_model(&self, config: &AutomationConfig) {
        let model = context_scout_model_assistant_from_project_config(Some(config));
        *self.runtime.lock().await =
            ContextScoutDurableRuntimeV1::new(Arc::clone(&self.store), model);
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
