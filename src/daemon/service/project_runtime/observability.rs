use std::sync::Arc;

use tracedecay_usecases::observability::{
    BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
    DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1, WorkOwnerObservationRecoveryV1,
};

pub(crate) struct RegisteredObservabilityProducerV1 {
    database: Arc<crate::global_db::RegisteredGlobalDb>,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    work_observations: Arc<WorkOwnerObservationRecoveryV1>,
}

impl RegisteredObservabilityProducerV1 {
    pub(crate) fn new(
        database: Arc<crate::global_db::RegisteredGlobalDb>,
        producer: BoundedObservabilityProducerV1,
        delivery_capacity: usize,
    ) -> Result<Self, &'static str> {
        let work_storage = database
            .work_storage()
            .map_err(|_| "work_owner_observation_storage_unavailable")?;
        let producer = Arc::new(producer);
        let authority = Arc::new(DeliverySettlementAuthorityV1::new(
            Arc::clone(&database),
            Arc::clone(&producer),
            producer.identity().clone(),
        )?);
        let delivery_settlements = Arc::new(BoundedDeliverySettlementRecorderV1::start(
            authority,
            delivery_capacity,
        )?);
        let work_observations = Arc::new(WorkOwnerObservationRecoveryV1::start(
            work_storage,
            Arc::clone(&producer),
        )?);
        Ok(Self {
            database,
            producer,
            delivery_settlements,
            work_observations,
        })
    }

    pub(crate) fn producer(&self) -> Arc<BoundedObservabilityProducerV1> {
        Arc::clone(&self.producer)
    }

    pub(crate) fn database(&self) -> Arc<crate::global_db::RegisteredGlobalDb> {
        Arc::clone(&self.database)
    }

    pub(crate) fn delivery_settlement_authority(
        &self,
    ) -> Result<Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>, &'static str>
    {
        tracedecay_usecases::observability::DeliverySettlementAuthorityV1::new(
            Arc::clone(&self.database),
            Arc::clone(&self.producer),
            self.producer.identity().clone(),
        )
        .map(Arc::new)
    }

    pub(crate) fn delivery_settlement_recorder(&self) -> Arc<BoundedDeliverySettlementRecorderV1> {
        Arc::clone(&self.delivery_settlements)
    }

    pub(crate) fn matches(
        &self,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
        identity: &ObservabilityProducerIdentityV1,
    ) -> bool {
        Arc::ptr_eq(&self.database, database) && self.producer.identity() == identity
    }

    pub(crate) async fn shutdown(
        &self,
    ) -> Result<(), tracedecay_application::ApplicationContractError> {
        let mut first_error = None;
        if let Err(error) = self.work_observations.shutdown().await {
            tracing::warn!(%error, "registered Work owner-observation recovery was incomplete");
            first_error = Some(error);
        }
        if let Err(error) = self.delivery_settlements.shutdown().await {
            tracing::warn!(%error, "registered delivery settlement drain was incomplete");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        if let Err(error) = self.producer.shutdown().await {
            tracing::warn!(%error, "registered observability producer shutdown was incomplete");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
