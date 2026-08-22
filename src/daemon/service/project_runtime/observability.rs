use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use tracedecay_usecases::observability::{
    BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
    DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1, WorkOwnerObservationRecoveryV1,
};

/// The live observability owners for one registered project-session store.
///
/// The producer, the delivery settlement recorder, and Work owner-observation
/// recovery all drain and settle against the registered store itself, so
/// exactly one of each runs per registered store client no matter how many
/// project roots (linked worktrees) mount observability for that store.
struct StoreObservabilityCoreV1 {
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlement_authority: Arc<DeliverySettlementAuthorityV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    work_observations: Arc<WorkOwnerObservationRecoveryV1>,
}

impl StoreObservabilityCoreV1 {
    fn start(
        database: crate::global_db::RegisteredGlobalDbLeaseV1,
        producer: BoundedObservabilityProducerV1,
        delivery_capacity: usize,
    ) -> Result<Self, &'static str> {
        let work_storage = database
            .work_storage()
            .map_err(|_| "work_owner_observation_storage_unavailable")?;
        let producer = Arc::new(producer);
        let delivery_settlement_authority = Arc::new(DeliverySettlementAuthorityV1::new(
            database.clone(),
            Arc::clone(&producer),
            producer.identity().clone(),
        )?);
        let delivery_settlements = Arc::new(BoundedDeliverySettlementRecorderV1::start(
            Arc::clone(&delivery_settlement_authority),
            delivery_capacity,
        )?);
        let work_observations = Arc::new(WorkOwnerObservationRecoveryV1::start(
            work_storage,
            Arc::clone(&producer),
        )?);
        Ok(Self {
            database,
            producer,
            delivery_settlement_authority,
            delivery_settlements,
            work_observations,
        })
    }

    async fn shutdown(&self) -> Result<(), tracedecay_application::ApplicationContractError> {
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

/// Whether both leases carry one exact registered-store authority: the same
/// registered client token for the same store runtime binding and verified
/// locator. Logical shard ids alone never match: two stores that share a
/// brain/profile/project id remain distinct authorities.
fn same_registered_store_authority(
    incumbent: &crate::global_db::RegisteredGlobalDbLeaseV1,
    candidate: &crate::global_db::RegisteredGlobalDbLeaseV1,
) -> bool {
    incumbent.shares_client_with(candidate)
        && incumbent.binding() == candidate.binding()
        && incumbent.verified_locator() == candidate.verified_locator()
}

struct StoreObservabilityEntryV1 {
    core: Arc<StoreObservabilityCoreV1>,
    aliases: usize,
}

/// Live observability owners keyed by exact registered-store authority.
///
/// Project roots are aliases onto one refcounted entry: mounting a linked
/// root attaches to the incumbent store owners instead of starting a second
/// producer or settlement recorder, and the last alias to shut down is the
/// one that drains and closes them.
#[derive(Clone, Default)]
pub(crate) struct StoreObservabilityRegistryV1 {
    entries: Arc<StdMutex<Vec<StoreObservabilityEntryV1>>>,
}

impl StoreObservabilityRegistryV1 {
    fn lock_entries(&self) -> MutexGuard<'_, Vec<StoreObservabilityEntryV1>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Attach an alias to the incumbent owners of this exact registered
    /// store, or start them. An incumbent whose identity the caller does not
    /// accept refuses the mount instead of running a second store owner.
    pub(crate) fn acquire_or_start<E>(
        &self,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
        accepts_incumbent: impl FnOnce(&ObservabilityProducerIdentityV1) -> bool,
        refused: impl FnOnce() -> E,
        start_producer: impl FnOnce() -> Result<BoundedObservabilityProducerV1, E>,
        delivery_capacity: usize,
        core_start_failed: impl FnOnce(&'static str) -> E,
    ) -> Result<RegisteredObservabilityProducerV1, E> {
        let mut entries = self.lock_entries();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| same_registered_store_authority(&entry.core.database, database))
        {
            if !accepts_incumbent(entry.core.producer.identity()) {
                return Err(refused());
            }
            entry.aliases += 1;
            return Ok(RegisteredObservabilityProducerV1::alias(
                self.clone(),
                Arc::clone(&entry.core),
            ));
        }
        let producer = start_producer()?;
        let core = Arc::new(
            StoreObservabilityCoreV1::start(database.clone(), producer, delivery_capacity)
                .map_err(core_start_failed)?,
        );
        entries.push(StoreObservabilityEntryV1 {
            core: Arc::clone(&core),
            aliases: 1,
        });
        Ok(RegisteredObservabilityProducerV1::alias(self.clone(), core))
    }
}

/// One project root's alias onto its store's observability owners.
pub(crate) struct RegisteredObservabilityProducerV1 {
    registry: StoreObservabilityRegistryV1,
    core: Arc<StoreObservabilityCoreV1>,
    released: AtomicBool,
}

impl RegisteredObservabilityProducerV1 {
    fn alias(registry: StoreObservabilityRegistryV1, core: Arc<StoreObservabilityCoreV1>) -> Self {
        Self {
            registry,
            core,
            released: AtomicBool::new(false),
        }
    }

    pub(crate) fn producer(&self) -> Arc<BoundedObservabilityProducerV1> {
        Arc::clone(&self.core.producer)
    }

    pub(crate) fn database(&self) -> crate::global_db::RegisteredGlobalDbLeaseV1 {
        self.core.database.clone()
    }

    pub(crate) fn delivery_settlement_authority(
        &self,
    ) -> Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1> {
        Arc::clone(&self.core.delivery_settlement_authority)
    }

    pub(crate) fn delivery_settlement_recorder(&self) -> Arc<BoundedDeliverySettlementRecorderV1> {
        Arc::clone(&self.core.delivery_settlements)
    }

    pub(crate) fn matches(
        &self,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
        identity: &ObservabilityProducerIdentityV1,
    ) -> bool {
        self.core.database.shares_client_with(database) && self.core.producer.identity() == identity
    }

    /// Releases this alias from its store entry, reporting whether it was the
    /// last one. Idempotent: shutdown and drop release at most once.
    fn release(&self) -> bool {
        if self.released.swap(true, Ordering::AcqRel) {
            return false;
        }
        let mut entries = self.registry.lock_entries();
        let Some(index) = entries
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.core, &self.core))
        else {
            return false;
        };
        entries[index].aliases -= 1;
        if entries[index].aliases == 0 {
            entries.remove(index);
            return true;
        }
        false
    }

    pub(crate) async fn shutdown(
        &self,
    ) -> Result<(), tracedecay_application::ApplicationContractError> {
        if !self.release() {
            return Ok(());
        }
        self.core.shutdown().await
    }
}

impl Drop for RegisteredObservabilityProducerV1 {
    fn drop(&mut self) {
        // An alias dropped without shutdown still releases its store entry,
        // so a later mount starts fresh owners instead of attaching to owners
        // nobody is left to flush.
        self.release();
    }
}
