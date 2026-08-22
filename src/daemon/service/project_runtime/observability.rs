use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use tracedecay_application::ApplicationContractError;
use tracedecay_domain::ManifestDigest;
use tracedecay_usecases::observability::{
    BoundedDeliverySettlementRecorderV1, BoundedObservabilityProducerV1,
    DeliverySettlementAuthorityV1, ObservabilityProducerIdentityV1, WorkOwnerObservationRecoveryV1,
};

/// The live observability owners for one registered project-session store.
///
/// The producer, the delivery settlement recorder, and Work owner-observation
/// recovery all drain and settle against the registered store itself, so
/// exactly one of each runs per registered store authority no matter how many
/// project roots (linked worktrees) mount observability for that store.
struct StoreObservabilityCoreV1 {
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    // Canonical configuration resolution provenance is store-wide. Exact
    // digest equality proves that linked-root policy differences come only
    // from their scopes; a different provenance must not reuse this owner.
    configuration_provenance_revision: ManifestDigest,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlement_authority: Arc<DeliverySettlementAuthorityV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    work_observations: Arc<WorkOwnerObservationRecoveryV1>,
}

impl StoreObservabilityCoreV1 {
    fn start(
        database: crate::global_db::RegisteredGlobalDbLeaseV1,
        configuration_provenance_revision: ManifestDigest,
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
            configuration_provenance_revision,
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

/// Whether both leases carry one exact registered-store authority. Fresh
/// owner issuances deliberately have disjoint client tokens, so the durable
/// runtime binding and its verified locator are the canonical equality.
/// Logical shard ids alone never match: two stores that share a
/// brain/profile/project id remain distinct authorities.
fn same_registered_store_authority(
    incumbent: &crate::global_db::RegisteredGlobalDbLeaseV1,
    candidate: &crate::global_db::RegisteredGlobalDbLeaseV1,
) -> bool {
    incumbent.binding() == candidate.binding()
        && incumbent.verified_locator() == candidate.verified_locator()
}

enum StoreObservabilityStateV1 {
    Active {
        core: Arc<StoreObservabilityCoreV1>,
        aliases: usize,
    },
    Stopping {
        core: Arc<StoreObservabilityCoreV1>,
    },
    Failed,
}

struct StoreObservabilityEntryV1 {
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    state: StoreObservabilityStateV1,
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
    fn lock_entries(&self) -> Result<MutexGuard<'_, Vec<StoreObservabilityEntryV1>>, &'static str> {
        self.entries
            .lock()
            .map_err(|_| "store_observability_registry_lock_poisoned")
    }

    /// Attach an alias to the incumbent owners of this exact registered
    /// store, or start them. An incumbent whose identity the caller does not
    /// accept refuses the mount instead of running a second store owner.
    pub(crate) fn acquire_or_start<E>(
        &self,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
        configuration_provenance_revision: &ManifestDigest,
        accepts_incumbent: impl FnOnce(&ObservabilityProducerIdentityV1) -> bool,
        emission_identity: impl FnOnce(
            &ObservabilityProducerIdentityV1,
        ) -> ObservabilityProducerIdentityV1,
        refused: impl FnOnce() -> E,
        start_producer: impl FnOnce() -> Result<BoundedObservabilityProducerV1, E>,
        delivery_capacity: usize,
        unavailable: impl Fn(&'static str) -> E,
    ) -> Result<RegisteredObservabilityProducerV1, E> {
        let mut entries = self.lock_entries().map_err(&unavailable)?;
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| same_registered_store_authority(&entry.database, database))
        {
            match &mut entry.state {
                StoreObservabilityStateV1::Active { core, aliases } => {
                    if core.configuration_provenance_revision != *configuration_provenance_revision
                        || !accepts_incumbent(core.producer.identity())
                    {
                        return Err(refused());
                    }
                    let emission_identity = emission_identity(core.producer.identity());
                    let producer = Arc::new(
                        core.producer
                            .alias_with_policy_identity(emission_identity)
                            .map_err(&unavailable)?,
                    );
                    let next_aliases = aliases.checked_add(1).ok_or_else(|| {
                        unavailable("store_observability_alias_capacity_exhausted")
                    })?;
                    let registered = RegisteredObservabilityProducerV1::alias(
                        self.clone(),
                        Arc::clone(core),
                        producer,
                    )
                    .map_err(&unavailable)?;
                    *aliases = next_aliases;
                    return Ok(registered);
                }
                StoreObservabilityStateV1::Stopping { .. } => {
                    return Err(unavailable("store_observability_retiring"));
                }
                StoreObservabilityStateV1::Failed => {
                    return Err(unavailable("store_observability_shutdown_failed"));
                }
            }
        }
        let producer = start_producer()?;
        let core = Arc::new(
            StoreObservabilityCoreV1::start(
                database.clone(),
                configuration_provenance_revision.clone(),
                producer,
                delivery_capacity,
            )
            .map_err(&unavailable)?,
        );
        let registered = RegisteredObservabilityProducerV1::alias(
            self.clone(),
            Arc::clone(&core),
            Arc::clone(&core.producer),
        )
        .map_err(&unavailable)?;
        entries.push(StoreObservabilityEntryV1 {
            database: database.clone(),
            state: StoreObservabilityStateV1::Active {
                core: Arc::clone(&core),
                aliases: 1,
            },
        });
        Ok(registered)
    }

    fn begin_retirement(
        &self,
        core: &Arc<StoreObservabilityCoreV1>,
    ) -> Result<bool, ApplicationContractError> {
        let mut entries =
            self.lock_entries()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "store_observability_registry_lock_poisoned",
                })?;
        let Some(entry) = entries.iter_mut().find(|entry| {
            same_registered_store_authority(&entry.database, &core.database)
                && matches!(
                    &entry.state,
                    StoreObservabilityStateV1::Active {
                        core: incumbent,
                        ..
                    } if Arc::ptr_eq(incumbent, core)
                )
        }) else {
            return Err(ApplicationContractError::Inconsistent {
                field: "store_observability_active_owner",
            });
        };
        let StoreObservabilityStateV1::Active { aliases, .. } = &mut entry.state else {
            return Err(ApplicationContractError::Inconsistent {
                field: "store_observability_active_owner",
            });
        };
        match *aliases {
            0 => {
                return Err(ApplicationContractError::Inconsistent {
                    field: "store_observability_alias_count",
                });
            }
            1 => {}
            _ => {
                *aliases -= 1;
                return Ok(false);
            }
        }
        entry.state = StoreObservabilityStateV1::Stopping {
            core: Arc::clone(core),
        };
        Ok(true)
    }

    fn finish_retirement(
        &self,
        core: &Arc<StoreObservabilityCoreV1>,
        succeeded: bool,
    ) -> Result<(), ApplicationContractError> {
        let mut entries =
            self.lock_entries()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "store_observability_registry_lock_poisoned",
                })?;
        let Some(index) = entries.iter().position(|entry| {
            same_registered_store_authority(&entry.database, &core.database)
                && matches!(
                    &entry.state,
                    StoreObservabilityStateV1::Stopping { core: incumbent }
                        if Arc::ptr_eq(incumbent, core)
                )
        }) else {
            return Err(ApplicationContractError::Inconsistent {
                field: "store_observability_retiring_owner",
            });
        };
        if succeeded {
            entries.remove(index);
        } else {
            entries[index].state = StoreObservabilityStateV1::Failed;
        }
        Ok(())
    }
}

/// One project root's alias onto its store's observability owners.
pub(crate) struct RegisteredObservabilityProducerV1 {
    registry: StoreObservabilityRegistryV1,
    core: Arc<StoreObservabilityCoreV1>,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlement_authority: Arc<DeliverySettlementAuthorityV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    released: AtomicBool,
}

impl RegisteredObservabilityProducerV1 {
    fn alias(
        registry: StoreObservabilityRegistryV1,
        core: Arc<StoreObservabilityCoreV1>,
        producer: Arc<BoundedObservabilityProducerV1>,
    ) -> Result<Self, &'static str> {
        let emission_identity = producer.identity().clone();
        let delivery_settlement_authority = Arc::new(
            core.delivery_settlement_authority
                .alias_with_policy_identity(emission_identity.clone())?,
        );
        let delivery_settlements = Arc::new(
            core.delivery_settlements
                .alias_with_policy_identity(emission_identity)?,
        );
        Ok(Self {
            registry,
            core,
            producer,
            delivery_settlement_authority,
            delivery_settlements,
            released: AtomicBool::new(false),
        })
    }

    pub(crate) fn producer(&self) -> Arc<BoundedObservabilityProducerV1> {
        Arc::clone(&self.producer)
    }

    pub(crate) fn database(&self) -> crate::global_db::RegisteredGlobalDbLeaseV1 {
        self.core.database.clone()
    }

    pub(crate) fn delivery_settlement_authority(
        &self,
    ) -> Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1> {
        Arc::clone(&self.delivery_settlement_authority)
    }

    pub(crate) fn delivery_settlement_recorder(&self) -> Arc<BoundedDeliverySettlementRecorderV1> {
        Arc::clone(&self.delivery_settlements)
    }

    pub(crate) fn matches(
        &self,
        database: &crate::global_db::RegisteredGlobalDbLeaseV1,
        configuration_provenance_revision: &ManifestDigest,
        identity: &ObservabilityProducerIdentityV1,
    ) -> bool {
        same_registered_store_authority(&self.core.database, database)
            && self.core.configuration_provenance_revision == *configuration_provenance_revision
            && *self.producer.identity() == *identity
    }

    /// Releases this alias from its store entry, reporting whether it was the
    /// last one. Idempotent: shutdown and drop release at most once.
    fn begin_retirement(&self) -> Result<bool, ApplicationContractError> {
        if self.released.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }
        self.registry.begin_retirement(&self.core)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ApplicationContractError> {
        if !self.begin_retirement()? {
            return Ok(());
        }
        let result = self.core.shutdown().await;
        let retirement = self.registry.finish_retirement(&self.core, result.is_ok());
        match result {
            Ok(()) => retirement,
            Err(error) => {
                if let Err(retirement_error) = retirement {
                    tracing::warn!(
                        %retirement_error,
                        "failed observability shutdown could not be retained"
                    );
                }
                Err(error)
            }
        }
    }
}

impl Drop for RegisteredObservabilityProducerV1 {
    fn drop(&mut self) {
        let is_last = match self.begin_retirement() {
            Ok(is_last) => is_last,
            Err(error) => {
                tracing::warn!(%error, "observability alias release was incomplete");
                return;
            }
        };
        if !is_last {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            if let Err(error) = self.registry.finish_retirement(&self.core, false) {
                tracing::warn!(%error, "observability owner failure could not be retained");
            }
            return;
        };
        let registry = self.registry.clone();
        let core = Arc::clone(&self.core);
        runtime.spawn(async move {
            let result = core.shutdown().await;
            if let Err(error) = &result {
                tracing::warn!(%error, "dropped observability owner shutdown was incomplete");
            }
            if let Err(error) = registry.finish_retirement(&core, result.is_ok()) {
                tracing::warn!(%error, "dropped observability retirement was incomplete");
            }
        });
    }
}
