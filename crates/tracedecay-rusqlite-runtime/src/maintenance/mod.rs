//! Linear authority used by writer-owned exclusive maintenance operations.

use tracedecay_store::{StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1};

use crate::checkpoint::CheckpointBlockers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceOwnerId(u64);

impl MaintenanceOwnerId {
    pub fn new(value: u64) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainBlockers {
    pub admissions: u32,
    pub readers: u32,
    pub snapshots: CheckpointBlockers,
    pub writer_active: bool,
}

impl DrainBlockers {
    pub const fn is_clear(&self) -> bool {
        self.admissions == 0
            && self.readers == 0
            && self.snapshots.is_clear()
            && !self.writer_active
    }
}

/// Linear evidence that every runtime user was observed drained for one exact
/// canonical registry publication.
#[derive(Debug, PartialEq, Eq)]
pub struct DrainedStateProof {
    publication: StoreRuntimeRegistryPublicationV1,
    observed: DrainBlockers,
}

impl DrainedStateProof {
    pub fn observe(
        publication: StoreRuntimeRegistryPublicationV1,
        blockers: DrainBlockers,
    ) -> Result<Self, MaintenancePermitError> {
        if !blockers.is_clear() {
            return Err(MaintenancePermitError::NotDrained);
        }
        Ok(Self {
            publication,
            observed: blockers,
        })
    }
}

/// Linear exclusive capability. It intentionally cannot be cloned: exactly
/// one terminal maintenance operation consumes it.
#[derive(Debug, PartialEq, Eq)]
pub struct ExclusiveMaintenancePermit {
    owner: MaintenanceOwnerId,
    publication: StoreRuntimeRegistryPublicationV1,
    _drained: DrainedStateProof,
}

impl ExclusiveMaintenancePermit {
    pub fn issue_after_drain(
        owner: MaintenanceOwnerId,
        publication: StoreRuntimeRegistryPublicationV1,
        drained: DrainedStateProof,
    ) -> Result<Self, MaintenancePermitError> {
        if drained.publication != publication || !drained.observed.is_clear() {
            return Err(MaintenancePermitError::FenceMismatch);
        }
        Ok(Self {
            owner,
            publication,
            _drained: drained,
        })
    }

    #[cfg(test)]
    pub(crate) fn issue(owner: MaintenanceOwnerId, binding: StoreRuntimeBindingV1) -> Self {
        let publication: StoreRuntimeRegistryPublicationV1 =
            serde_json::from_value(serde_json::json!({
                "publication_id": "publication.test-only",
                "binding": binding,
                "published_at": 1
            }))
            .expect("test publication is valid");
        let drained = DrainedStateProof::observe(publication.clone(), DrainBlockers::default())
            .expect("test runtime is drained");
        Self::issue_after_drain(owner, publication, drained).expect("test permit is fenced")
    }

    pub const fn owner(&self) -> MaintenanceOwnerId {
        self.owner
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.publication.binding
    }

    pub fn publication(&self) -> &StoreRuntimeRegistryPublicationV1 {
        &self.publication
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenancePermitError {
    NotDrained,
    FenceMismatch,
}

impl std::fmt::Display for MaintenancePermitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotDrained => "exclusive maintenance requires a drained runtime",
            Self::FenceMismatch => {
                "exclusive maintenance proof does not match the publication fence"
            }
        })
    }
}

impl std::error::Error for MaintenancePermitError {}
