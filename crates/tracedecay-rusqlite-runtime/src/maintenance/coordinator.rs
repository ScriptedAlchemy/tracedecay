use std::sync::{Arc, Mutex, MutexGuard};

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreRuntimeRegistryPublicationV1};

use super::types::{
    CancellationBoundary, DrainBlockers, DriverMaintenanceError, ExclusiveMaintenancePermit,
    MaintenanceAction, MaintenanceCancellation, MaintenanceError, MaintenanceOwnerId,
    MaintenanceProgress, MaintenanceRequest, MaintenanceStart, PublicationAttempt,
    ReplacementPublicationKind, ReplacementPublicationReceipt, ReplacementPublicationRequest,
};

/// The daemon registry is the sole authority that may allocate and publish a
/// replacement incarnation or authority epoch.
pub trait CanonicalRegistryAuthority: Send + Sync {
    fn request_replacement(
        &self,
        request: &ReplacementPublicationRequest,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError>;
}

/// Physical lifecycle operations. Identity allocation and publication are not
/// part of this port; they remain with [`CanonicalRegistryAuthority`].
pub trait MaintenanceLifecycle {
    fn publication(&self) -> StoreRuntimeRegistryPublicationV1;
    fn state(&self) -> RuntimeMaintenanceStateV1;

    fn stop_admissions_and_begin_drain(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<(), MaintenanceError>;

    fn drain_blockers(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<DrainBlockers, MaintenanceError>;

    /// Transition the drained physical runtime into exclusive maintenance.
    /// The coordinator issues the opaque linear permit only after this returns.
    fn enter_exclusive(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
        owner: MaintenanceOwnerId,
    ) -> Result<(), MaintenanceError>;

    /// Consume the sole permit and install a registry-issued publication.
    fn reopen(
        &self,
        permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError>;

    /// Consume the sole permit and record a registry-issued fault publication.
    fn fault(
        &self,
        permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError>;
}

/// Closed SQLite maintenance seam. There is deliberately no generic execute,
/// callback, connection, SQL, or path-bearing method.
pub trait MaintenanceDriver {
    fn migrate(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        plan: &super::types::MigrationPlanId,
    ) -> Result<(), DriverMaintenanceError>;
    fn rebuild_fts(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        index: &super::types::FtsIndexId,
    ) -> Result<(), DriverMaintenanceError>;
    fn restore(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        artifact: &super::types::VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError>;
    fn compact(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        mode: super::types::CompactionMode,
    ) -> Result<(), DriverMaintenanceError>;
    fn replace_shard(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        artifact: &super::types::VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError>;
}

#[derive(Clone, Debug)]
struct ActiveMaintenance {
    request: MaintenanceRequest,
    cancellation_recorded: bool,
}

pub struct MaintenanceCoordinator {
    authority: Arc<dyn CanonicalRegistryAuthority>,
    active: Mutex<Option<ActiveMaintenance>>,
}

impl MaintenanceCoordinator {
    pub fn new(authority: Arc<dyn CanonicalRegistryAuthority>) -> Self {
        Self {
            authority,
            active: Mutex::new(None),
        }
    }

    pub fn start(
        &self,
        lifecycle: &dyn MaintenanceLifecycle,
        request: MaintenanceRequest,
        cancellation: &dyn MaintenanceCancellation,
    ) -> Result<MaintenanceStart, MaintenanceError> {
        let mut active = self.lock_active();
        if let Some(current) = active.as_ref() {
            return Err(MaintenanceError::AlreadyOwned {
                owner: current.request.owner,
            });
        }
        require_publication(lifecycle, &request.expected)?;
        require_state(lifecycle, RuntimeMaintenanceStateV1::Ready)?;
        validate_action_target(&request)?;
        if cancellation.is_cancelled(CancellationBoundary::BeforeDrain) {
            return Ok(MaintenanceStart::Cancelled);
        }
        lifecycle.stop_admissions_and_begin_drain(&request.expected)?;
        *active = Some(ActiveMaintenance {
            request,
            cancellation_recorded: false,
        });
        Ok(MaintenanceStart::Started)
    }

    /// Advance one bounded lifecycle turn. Cancellation is sampled outside the
    /// driver call, and the linear permit is consumed by one terminal action.
    pub fn advance(
        &self,
        owner: MaintenanceOwnerId,
        lifecycle: &dyn MaintenanceLifecycle,
        driver: &mut dyn MaintenanceDriver,
        cancellation: &dyn MaintenanceCancellation,
    ) -> Result<MaintenanceProgress, MaintenanceError> {
        let mut active = self.lock_active();
        let session = active
            .as_mut()
            .ok_or(MaintenanceError::NoActiveMaintenance)?;
        if session.request.owner != owner {
            return Err(MaintenanceError::WrongOwner);
        }
        require_publication(lifecycle, &session.request.expected)?;
        require_state(lifecycle, RuntimeMaintenanceStateV1::Draining)?;

        if cancellation.is_cancelled(CancellationBoundary::AwaitingDrain) {
            session.cancellation_recorded = true;
        }
        let blockers = lifecycle.drain_blockers(&session.request.expected)?;
        if !blockers.is_clear() {
            return Ok(MaintenanceProgress::Blocked {
                blockers,
                cancellation_recorded: session.cancellation_recorded,
            });
        }

        lifecycle.enter_exclusive(&session.request.expected, owner)?;
        let proof =
            super::types::DrainedStateProof::observe(session.request.expected.clone(), blockers)?;
        let permit = ExclusiveMaintenancePermit::issue_after_drain(
            owner,
            session.request.expected.clone(),
            proof,
        )?;
        if cancellation.is_cancelled(CancellationBoundary::BeforeAction) {
            session.cancellation_recorded = true;
        }
        let action_performed = !session.cancellation_recorded;
        if let Some(Err(error)) =
            action_performed.then(|| perform(driver, &permit, &session.request.action))
        {
            let progress = self.publish_fault(
                lifecycle,
                permit,
                &session.request.expected,
                MaintenanceError::Driver(error),
            );
            *active = None;
            return Ok(progress);
        }

        let request = ReplacementPublicationRequest {
            prior: session.request.expected.clone(),
            kind: ReplacementPublicationKind::Reopen,
        };
        let receipt = match self.authority.request_replacement(&request) {
            Ok(receipt) => receipt,
            Err(error) => {
                let progress =
                    self.publish_fault(lifecycle, permit, &session.request.expected, error);
                *active = None;
                return Ok(progress);
            }
        };
        if let Err(error) = validate_receipt(&request, &receipt) {
            let progress = self.publish_fault(lifecycle, permit, &session.request.expected, error);
            *active = None;
            return Ok(progress);
        }
        let receipt = match lifecycle.reopen(permit, receipt.clone()) {
            Ok(receipt) => receipt,
            Err(error) => {
                *active = None;
                return Ok(MaintenanceProgress::Faulted {
                    error,
                    publication: Box::new(PublicationAttempt {
                        request,
                        receipt: Some(receipt),
                    }),
                });
            }
        };
        *active = None;
        Ok(MaintenanceProgress::Reopened {
            publication: Box::new(receipt),
            action_performed,
        })
    }

    fn publish_fault(
        &self,
        lifecycle: &dyn MaintenanceLifecycle,
        permit: ExclusiveMaintenancePermit,
        prior: &StoreRuntimeRegistryPublicationV1,
        error: MaintenanceError,
    ) -> MaintenanceProgress {
        let request = ReplacementPublicationRequest {
            prior: prior.clone(),
            kind: ReplacementPublicationKind::Fault,
        };
        let receipt = self
            .authority
            .request_replacement(&request)
            .and_then(|receipt| {
                validate_receipt(&request, &receipt)?;
                lifecycle.fault(permit, receipt)
            });
        match receipt {
            Ok(receipt) => MaintenanceProgress::Faulted {
                error,
                publication: Box::new(PublicationAttempt {
                    request,
                    receipt: Some(receipt),
                }),
            },
            Err(publication_error) => MaintenanceProgress::Faulted {
                error: publication_error,
                publication: Box::new(PublicationAttempt {
                    request,
                    receipt: None,
                }),
            },
        }
    }

    fn lock_active(&self) -> MutexGuard<'_, Option<ActiveMaintenance>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn require_publication(
    lifecycle: &dyn MaintenanceLifecycle,
    expected: &StoreRuntimeRegistryPublicationV1,
) -> Result<(), MaintenanceError> {
    let actual = lifecycle.publication();
    (actual == *expected)
        .then_some(())
        .ok_or_else(|| MaintenanceError::Fenced {
            expected: Box::new(expected.binding.clone()),
            actual: Box::new(actual.binding),
        })
}

fn require_state(
    lifecycle: &dyn MaintenanceLifecycle,
    expected: RuntimeMaintenanceStateV1,
) -> Result<(), MaintenanceError> {
    let actual = lifecycle.state();
    (actual == expected)
        .then_some(())
        .ok_or(MaintenanceError::InvalidState { expected, actual })
}

fn validate_action_target(request: &MaintenanceRequest) -> Result<(), MaintenanceError> {
    let artifact = match &request.action {
        MaintenanceAction::Restore { artifact }
        | MaintenanceAction::ShardReplacement { artifact } => Some(artifact),
        _ => None,
    };
    if artifact.is_some_and(|artifact| artifact.shard_id != request.expected.binding.shard_id) {
        return Err(MaintenanceError::ArtifactShardMismatch);
    }
    Ok(())
}

fn validate_receipt(
    request: &ReplacementPublicationRequest,
    receipt: &ReplacementPublicationReceipt,
) -> Result<(), MaintenanceError> {
    let prior = &request.prior.binding;
    let replacement = &receipt.publication.binding;
    if receipt.request != *request
        || replacement.shard_id != prior.shard_id
        || replacement.incarnation <= prior.incarnation
        || replacement.authority_epoch <= prior.authority_epoch
    {
        return Err(MaintenanceError::CanonicalAuthority {
            stage: "invalid replacement publication receipt",
        });
    }
    Ok(())
}

fn perform(
    driver: &mut dyn MaintenanceDriver,
    permit: &ExclusiveMaintenancePermit,
    action: &MaintenanceAction,
) -> Result<(), DriverMaintenanceError> {
    match action {
        MaintenanceAction::Migration { plan } => driver.migrate(permit, plan),
        MaintenanceAction::FtsRebuild { index } => driver.rebuild_fts(permit, index),
        MaintenanceAction::Restore { artifact } => driver.restore(permit, artifact),
        MaintenanceAction::Compaction { mode } => driver.compact(permit, *mode),
        MaintenanceAction::ShardReplacement { artifact } => driver.replace_shard(permit, artifact),
    }
}
