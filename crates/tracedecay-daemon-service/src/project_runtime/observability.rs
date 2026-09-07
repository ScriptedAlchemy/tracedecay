use std::fmt;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};

use tracedecay_application::ApplicationContractError;
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
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlement_authority: Arc<DeliverySettlementAuthorityV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    work_observations: Arc<WorkOwnerObservationRecoveryV1>,
}

impl StoreObservabilityCoreV1 {
    fn start(
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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

    #[hotpath::skip]
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
    incumbent: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    candidate: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
) -> bool {
    incumbent.binding() == candidate.binding()
        && incumbent.verified_locator() == candidate.verified_locator()
}

/// How a `Stopping` entry's drain is progressing. Only a confirmed drain may
/// settle the retirement: a mount frontend obtained from the owner can
/// outlive every alias handle, so the entry must keep refusing mounts until
/// the core is actually closed.
#[derive(Clone, Copy)]
enum StoreObservabilityDrainV1 {
    /// A retiring caller is awaiting the core drain or has spawned it.
    InFlight,
    /// The last alias was dropped without a tokio runtime, so nothing could
    /// run the drain. The next mount attempt on a live runtime starts it.
    Deferred,
}

enum StoreObservabilityStateV1 {
    Active {
        core: Arc<StoreObservabilityCoreV1>,
        /// Live alias handles onto this owner. Each
        /// [`RegisteredObservabilityProducerV1`] surrenders its single
        /// release token exactly once — by explicit shutdown or by drop —
        /// so this registry-owned count is the one refcount authority.
        aliases: usize,
    },
    Stopping {
        core: Arc<StoreObservabilityCoreV1>,
        drain: StoreObservabilityDrainV1,
    },
    Failed,
}

struct StoreObservabilityEntryV1 {
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    state: StoreObservabilityStateV1,
}

/// One project root's request to mount observability for an exact registered
/// store. An incumbent owner must answer to the same store authority — the
/// authorized scope and the producer revision. `configuration_revision` and
/// `policy_revision` are this root's own provenance at its own open time,
/// stamped by the resulting alias frontend rather than compared against the
/// incumbent: the store's canonical configuration advances while earlier roots
/// stay mounted, so a later linked root legitimately resolves a newer revision
/// for the same store and every emission still carries the exact provenance of
/// the alias that made it.
pub struct StoreObservabilityMountV1 {
    pub(crate) authorized_scope_ref: String,
    pub(crate) producer_revision: String,
    pub(crate) configuration_revision: String,
    pub(crate) policy_revision: String,
    pub(crate) delivery_capacity: usize,
}

impl StoreObservabilityMountV1 {
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn new(
        authorized_scope_ref: String,
        producer_revision: String,
        configuration_revision: String,
        policy_revision: String,
        delivery_capacity: usize,
    ) -> Self {
        Self {
            authorized_scope_ref,
            producer_revision,
            configuration_revision,
            policy_revision,
            delivery_capacity,
        }
    }
}

/// Why observability could not be mounted for a registered store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreObservabilityMountErrorV1 {
    /// A live owner for this exact store answers to a different authorized
    /// scope or producer revision. The mount is refused rather than silently
    /// aliased or given a second store owner.
    Busy,
    /// The last alias is draining. Replacement owners are refused until the
    /// drain finishes and the store entry retires.
    Retiring,
    /// The last owner shutdown failed and is remembered: mounting again
    /// could overlap a worker whose drain never completed.
    ShutdownFailed,
    /// Registry or owner-start infrastructure was unavailable.
    Unavailable(&'static str),
}

impl fmt::Display for StoreObservabilityMountErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::Busy => "store_observability_busy",
            Self::Retiring => "store_observability_retiring",
            Self::ShutdownFailed => "store_observability_shutdown_failed",
            Self::Unavailable(reason) => reason,
        };
        formatter.write_str(code)
    }
}

/// Live observability owners keyed by exact registered-store authority.
///
/// Project roots are aliases onto one refcounted entry: mounting a linked
/// root attaches to the incumbent store owners instead of starting a second
/// producer or settlement recorder, and the last alias to shut down is the
/// one that drains and closes them.
#[derive(Clone, Default)]
pub struct StoreObservabilityRegistryV1 {
    entries: Arc<StdMutex<Vec<StoreObservabilityEntryV1>>>,
}

impl StoreObservabilityRegistryV1 {
    fn lock_entries(&self) -> Result<MutexGuard<'_, Vec<StoreObservabilityEntryV1>>, &'static str> {
        self.entries
            .lock()
            .map_err(|_| "store_observability_registry_lock_poisoned")
    }

    /// Attach an alias to the incumbent owners of this exact registered
    /// store, or start them via `start_producer`. An incumbent that does not
    /// match the mount's store-authority fields refuses the mount instead of
    /// running a second store owner.
    #[hotpath::measure(label = "daemon.service.project_runtime.observability_acquire")]
    pub fn acquire_or_start(
        &self,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        mount: &StoreObservabilityMountV1,
        start_producer: impl FnOnce() -> Result<
            BoundedObservabilityProducerV1,
            StoreObservabilityMountErrorV1,
        >,
    ) -> Result<RegisteredObservabilityProducerV1, StoreObservabilityMountErrorV1> {
        let mut entries = self
            .lock_entries()
            .map_err(StoreObservabilityMountErrorV1::Unavailable)?;
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| same_registered_store_authority(&entry.database, database))
        {
            return match &mut entry.state {
                StoreObservabilityStateV1::Active { core, aliases } => {
                    let incumbent = core.producer.identity();
                    // Only store authority is compared. An incumbent's
                    // configuration and policy revisions are frozen at its own
                    // mount time and the store's canonical configuration keeps
                    // advancing underneath it, so comparing them refused every
                    // later root of a store whose configuration had been
                    // written once — permanently, for the daemon's life.
                    if incumbent.authorized_scope_ref != mount.authorized_scope_ref
                        || incumbent.producer_revision != mount.producer_revision
                    {
                        return Err(StoreObservabilityMountErrorV1::Busy);
                    }
                    // The alias joins the incumbent's boot stream and stamps
                    // this root's own configuration and policy provenance.
                    let emission_identity = ObservabilityProducerIdentityV1 {
                        authorized_scope_ref: mount.authorized_scope_ref.clone(),
                        process_boot_id: incumbent.process_boot_id.clone(),
                        producer_revision: mount.producer_revision.clone(),
                        configuration_revision: mount.configuration_revision.clone(),
                        policy_revision: mount.policy_revision.clone(),
                    };
                    let next_aliases = aliases.checked_add(1).ok_or(
                        StoreObservabilityMountErrorV1::Unavailable(
                            "store_observability_alias_capacity_exhausted",
                        ),
                    )?;
                    let producer = Arc::new(
                        core.producer
                            .alias_with_policy_identity(emission_identity)
                            .map_err(StoreObservabilityMountErrorV1::Unavailable)?,
                    );
                    let registered = RegisteredObservabilityProducerV1::alias(
                        self.clone(),
                        Arc::clone(core),
                        producer,
                    )
                    .map_err(StoreObservabilityMountErrorV1::Unavailable)?;
                    *aliases = next_aliases;
                    Ok(registered)
                }
                StoreObservabilityStateV1::Stopping { core, drain } => {
                    // A deferred drain (the last alias was dropped without a
                    // runtime) starts now that a caller with a live runtime
                    // has arrived. The mount is still refused: only the
                    // confirmed drain may vacate the entry.
                    if matches!(drain, StoreObservabilityDrainV1::Deferred)
                        && let Ok(runtime) = tokio::runtime::Handle::try_current()
                    {
                        *drain = StoreObservabilityDrainV1::InFlight;
                        self.spawn_retirement_drain(&runtime, Arc::clone(core));
                    }
                    Err(StoreObservabilityMountErrorV1::Retiring)
                }
                StoreObservabilityStateV1::Failed => {
                    Err(StoreObservabilityMountErrorV1::ShutdownFailed)
                }
            };
        }
        let producer = start_producer()?;
        let core = Arc::new(
            StoreObservabilityCoreV1::start(database.clone(), producer, mount.delivery_capacity)
                .map_err(StoreObservabilityMountErrorV1::Unavailable)?,
        );
        let registered = RegisteredObservabilityProducerV1::alias(
            self.clone(),
            Arc::clone(&core),
            Arc::clone(&core.producer),
        )
        .map_err(StoreObservabilityMountErrorV1::Unavailable)?;
        entries.push(StoreObservabilityEntryV1 {
            database: database.clone(),
            state: StoreObservabilityStateV1::Active { core, aliases: 1 },
        });
        Ok(registered)
    }

    /// Surrenders one alias release. Reports whether it was the last one, in
    /// which case the entry is now `Stopping` with the given drain progress:
    /// an `InFlight` caller must drain the core and finish the retirement; a
    /// `Deferred` retirement waits for the next mount attempt on a live
    /// runtime to start the drain.
    fn begin_retirement(
        &self,
        core: &Arc<StoreObservabilityCoreV1>,
        drain: StoreObservabilityDrainV1,
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
            drain,
        };
        Ok(true)
    }

    /// Runs the core drain in the background and settles the retirement with
    /// the drain's confirmed outcome.
    fn spawn_retirement_drain(
        &self,
        runtime: &tokio::runtime::Handle,
        core: Arc<StoreObservabilityCoreV1>,
    ) {
        let registry = self.clone();
        runtime.spawn(async move {
            let result = core.shutdown().await;
            if let Err(error) = &result {
                tracing::warn!(%error, "background observability owner drain was incomplete");
            }
            if let Err(error) = registry.finish_retirement(&core, result.is_ok()) {
                tracing::warn!(%error, "background observability retirement was incomplete");
            }
        });
    }

    /// Settles a `Stopping` entry: a releasable retirement removes it so a
    /// fresh owner may mount; a genuine shutdown failure is remembered as
    /// `Failed` and refuses all future mounts.
    fn finish_retirement(
        &self,
        core: &Arc<StoreObservabilityCoreV1>,
        releasable: bool,
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
                    StoreObservabilityStateV1::Stopping { core: incumbent, .. }
                        if Arc::ptr_eq(incumbent, core)
                )
        }) else {
            return Err(ApplicationContractError::Inconsistent {
                field: "store_observability_retiring_owner",
            });
        };
        if releasable {
            entries.remove(index);
        } else {
            entries[index].state = StoreObservabilityStateV1::Failed;
        }
        Ok(())
    }
}

/// The settlement frontends that stamp `producer`'s exact provenance onto the
/// observations they raise, over the store owner's one recorder and authority.
fn settlement_frontends(
    core: &StoreObservabilityCoreV1,
    producer: &BoundedObservabilityProducerV1,
) -> Result<
    (
        Arc<DeliverySettlementAuthorityV1>,
        Arc<BoundedDeliverySettlementRecorderV1>,
    ),
    &'static str,
> {
    let emission_identity = producer.identity().clone();
    Ok((
        Arc::new(
            core.delivery_settlement_authority
                .alias_with_policy_identity(emission_identity.clone())?,
        ),
        Arc::new(
            core.delivery_settlements
                .alias_with_policy_identity(emission_identity)?,
        ),
    ))
}

/// One project root's alias onto its store's observability owners.
pub struct RegisteredObservabilityProducerV1 {
    registry: StoreObservabilityRegistryV1,
    core: Arc<StoreObservabilityCoreV1>,
    producer: Arc<BoundedObservabilityProducerV1>,
    delivery_settlement_authority: Arc<DeliverySettlementAuthorityV1>,
    delivery_settlements: Arc<BoundedDeliverySettlementRecorderV1>,
    /// The single release token: `Some` exactly while this alias is counted
    /// by its store entry. It is taken once — by the consuming [`Self::shutdown`]
    /// or by drop — so the registry's alias count is derived from exactly one
    /// release per handle, with no separate released flag to keep consistent.
    release: Option<Arc<StoreObservabilityCoreV1>>,
}

impl RegisteredObservabilityProducerV1 {
    fn alias(
        registry: StoreObservabilityRegistryV1,
        core: Arc<StoreObservabilityCoreV1>,
        producer: Arc<BoundedObservabilityProducerV1>,
    ) -> Result<Self, &'static str> {
        let (delivery_settlement_authority, delivery_settlements) =
            settlement_frontends(&core, &producer)?;
        Ok(Self {
            registry,
            release: Some(Arc::clone(&core)),
            core,
            producer,
            delivery_settlement_authority,
            delivery_settlements,
        })
    }

    pub fn producer(&self) -> Arc<BoundedObservabilityProducerV1> {
        Arc::clone(&self.producer)
    }

    pub(crate) fn database(&self) -> tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        self.core.database.clone()
    }

    pub(crate) fn delivery_settlement_authority(
        &self,
    ) -> Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1> {
        Arc::clone(&self.delivery_settlement_authority)
    }

    pub fn delivery_settlement_recorder(&self) -> Arc<BoundedDeliverySettlementRecorderV1> {
        Arc::clone(&self.delivery_settlements)
    }

    /// Whether this alias already answers for the same store authority: the
    /// exact registered store, the same authorized scope, and the same
    /// producer revision. Configuration and policy provenance belong to the
    /// mounting root at its own open time, so they are never owner identity.
    pub(crate) fn matches(
        &self,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        authorized_scope_ref: &str,
        producer_revision: &str,
    ) -> bool {
        let identity = self.producer.identity();
        same_registered_store_authority(&self.core.database, database)
            && identity.authorized_scope_ref == authorized_scope_ref
            && identity.producer_revision == producer_revision
    }

    /// Re-stamps this root's own provenance onto the frontends it hands out.
    ///
    /// Configuration and policy provenance are the mounting root's at its own
    /// open time, not owner identity, so a same-root remount resolves the
    /// store's current revisions and must stamp them. Only the frontends are
    /// replaced: the store owner keeps its one queue, sequence, worker and
    /// settlement recorder, and observations already admitted under the
    /// previous revisions keep the provenance stamped at their admission.
    pub(crate) fn restamp_provenance(
        &mut self,
        configuration_revision: &str,
        policy_revision: &str,
    ) -> Result<(), &'static str> {
        let incumbent = self.producer.identity();
        if incumbent.configuration_revision == configuration_revision
            && incumbent.policy_revision == policy_revision
        {
            return Ok(());
        }
        let producer = Arc::new(self.core.producer.alias_with_policy_identity(
            ObservabilityProducerIdentityV1 {
                configuration_revision: configuration_revision.to_owned(),
                policy_revision: policy_revision.to_owned(),
                ..incumbent.clone()
            },
        )?);
        let (delivery_settlement_authority, delivery_settlements) =
            settlement_frontends(&self.core, &producer)?;
        self.producer = producer;
        self.delivery_settlement_authority = delivery_settlement_authority;
        self.delivery_settlements = delivery_settlements;
        Ok(())
    }

    /// Releases this alias; the last release drains and closes the store
    /// owners. Consuming the handle is what makes the release single-shot:
    /// the token taken here is the same one drop would take.
    #[hotpath::measure(
        label = "daemon.service.project_runtime.observability_shutdown",
        future = true
    )]
    pub async fn shutdown(mut self) -> Result<(), ApplicationContractError> {
        let Some(core) = self.release.take() else {
            // Unreachable for an owned handle: only drop takes the token
            // otherwise. Releasing twice is an idempotent no-op by contract.
            return Ok(());
        };
        let registry = self.registry.clone();
        drop(self);
        if !registry.begin_retirement(&core, StoreObservabilityDrainV1::InFlight)? {
            return Ok(());
        }
        let result = core.shutdown().await;
        let retirement = registry.finish_retirement(&core, result.is_ok());
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
        let Some(core) = self.release.take() else {
            return;
        };
        // A drop without a runtime cannot await the drain, and retained
        // mount frontends may still hold the shared producer core open, so
        // inability to drain is never a confirmed close. The entry stays
        // retiring — refusing replacement mounts that would start a
        // duplicate producer — until the deferred drain, started by the next
        // mount attempt on a live runtime, confirms the close.
        let runtime = tokio::runtime::Handle::try_current().ok();
        let drain = if runtime.is_some() {
            StoreObservabilityDrainV1::InFlight
        } else {
            StoreObservabilityDrainV1::Deferred
        };
        let is_last = match self.registry.begin_retirement(&core, drain) {
            Ok(is_last) => is_last,
            Err(error) => {
                tracing::warn!(%error, "observability alias release was incomplete");
                return;
            }
        };
        if !is_last {
            return;
        }
        match runtime {
            Some(runtime) => self.registry.spawn_retirement_drain(&runtime, core),
            None => tracing::warn!(
                "observability owners dropped without a runtime; the store stays \
                 retiring until a deferred drain confirms the close"
            ),
        }
    }
}
