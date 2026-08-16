use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::ApplicationContractError;
use tracedecay_domain::{
    DeliverySettlementAttemptV1, DeliverySettlementV1, ObservabilityPayloadV1,
    ObservabilityTerminalResultV1,
};
use tracedecay_global_db::{
    DeliveryAttemptClaimV1, DeliverySourceReceiptReadV1, DurableDeliverySettlementReceiptV1,
    PendingDeliverySourceReceiptV1, RegisteredGlobalDbLeaseV1,
};

use super::delivery_spool::recorder_spool_root;
use super::{
    BoundedObservabilityProducerV1, ObservabilityOwnerEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, execution_owner_fact_envelope,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliverySettlementEmissionV1 {
    pub receipt: DurableDeliverySettlementReceiptV1,
    pub observability: Option<ObservabilityOwnerEmissionOutcomeV1>,
}

/// Canonical owner that commits a reported surface outcome before offering its
/// complete census to Plan 26 observability. Callers may replay an exact
/// receipt; this type does not scan or fabricate receipts after process loss.
pub struct DeliverySettlementAuthorityV1 {
    db: RegisteredGlobalDbLeaseV1,
    producer: Arc<BoundedObservabilityProducerV1>,
    identity: ObservabilityProducerIdentityV1,
}

impl DeliverySettlementAuthorityV1 {
    pub fn new(
        db: RegisteredGlobalDbLeaseV1,
        producer: Arc<BoundedObservabilityProducerV1>,
        identity: ObservabilityProducerIdentityV1,
    ) -> Result<Self, &'static str> {
        if identity.authorized_scope_ref.is_empty() {
            return Err("delivery_settlement_scope");
        }
        if producer.identity() != &identity {
            return Err("delivery_settlement_producer_identity");
        }
        if db
            .binding()
            .shard_id
            .scope
            .project_id()
            .is_none_or(|project_id| project_id.as_str() != identity.authorized_scope_ref)
        {
            return Err("delivery_settlement_database_scope");
        }
        Ok(Self {
            db,
            producer,
            identity,
        })
    }

    pub async fn begin(
        &self,
        attempt: &DeliverySettlementAttemptV1,
    ) -> Result<DeliveryAttemptClaimV1, ApplicationContractError> {
        self.db
            .begin_delivery_attempt(&self.identity.authorized_scope_ref, attempt)
            .await
            .map_err(ApplicationContractError::Domain)
    }

    pub async fn begin_receipted(
        &self,
        attempt: &DeliverySettlementAttemptV1,
        source_receipt_ref: &str,
    ) -> Result<DeliveryAttemptClaimV1, ApplicationContractError> {
        self.db
            .begin_receipted_delivery_attempt(
                &self.identity.authorized_scope_ref,
                attempt,
                source_receipt_ref,
            )
            .await
            .map_err(ApplicationContractError::Domain)
    }

    pub async fn attempt_for_receipt(
        &self,
        source_receipt_ref: &str,
    ) -> Result<Option<DeliverySourceReceiptReadV1>, ApplicationContractError> {
        self.db
            .delivery_attempt_for_source_receipt(
                &self.identity.authorized_scope_ref,
                source_receipt_ref,
            )
            .await
            .map_err(ApplicationContractError::Domain)
    }

    pub async fn pending_receipted_attempts_due(
        &self,
        surface: tracedecay_domain::DeliverySurfaceFamilyV1,
        attempted_at_through: tracedecay_domain::UtcMicros,
        limit: usize,
    ) -> Result<Vec<PendingDeliverySourceReceiptV1>, ApplicationContractError> {
        self.db
            .pending_receipted_delivery_attempts_due(
                &self.identity.authorized_scope_ref,
                surface,
                attempted_at_through,
                limit,
            )
            .await
            .map_err(ApplicationContractError::Domain)
    }

    pub(super) fn spool_root(&self) -> PathBuf {
        recorder_spool_root(self.db.db_path())
    }

    pub async fn settle(
        &self,
        settlement: &DeliverySettlementV1,
    ) -> Result<DeliverySettlementEmissionV1, ApplicationContractError> {
        let receipt = self
            .db
            .settle_delivery_attempt(&self.identity.authorized_scope_ref, settlement)
            .await
            .map_err(ApplicationContractError::Domain)?;
        let census = &receipt.census;
        if census.attempted != census.eligible || census.unknown != 0 {
            return Ok(DeliverySettlementEmissionV1 {
                receipt,
                observability: None,
            });
        }
        let owner_transition_ref = format!(
            "delivery:{}:{}",
            census.owner_event_id,
            surface_name(census.surface),
        );
        let terminal_result = if census.dropped > 0 {
            ObservabilityTerminalResultV1::Failed
        } else {
            ObservabilityTerminalResultV1::Succeeded
        };
        let envelope = execution_owner_fact_envelope(
            &self.identity,
            &self.identity.authorized_scope_ref,
            &owner_transition_ref,
            "settle-delivery",
            census.settled_at,
            Some(census.valid_at),
            Some(census.settled_at),
            Some(terminal_result),
            census.coverage,
            ObservabilityPayloadV1::WorkDeliveryFanout(census.as_fanout_observation()),
        )
        .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
        let observability = self.producer.emit_owner_fact(envelope).await?;
        Ok(DeliverySettlementEmissionV1 {
            receipt,
            observability: Some(observability),
        })
    }
}

const fn surface_name(surface: tracedecay_domain::DeliverySurfaceFamilyV1) -> &'static str {
    match surface {
        tracedecay_domain::DeliverySurfaceFamilyV1::Hook => "hook",
        tracedecay_domain::DeliverySurfaceFamilyV1::Mcp => "mcp",
        tracedecay_domain::DeliverySurfaceFamilyV1::Lsp => "lsp",
        tracedecay_domain::DeliverySurfaceFamilyV1::Dashboard => "dashboard",
        tracedecay_domain::DeliverySurfaceFamilyV1::Cli => "cli",
        tracedecay_domain::DeliverySurfaceFamilyV1::Other => "other",
    }
}
