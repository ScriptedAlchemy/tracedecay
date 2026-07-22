use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_store::{StoreAuthorityEpochV1, StoreRuntimeBindingV1};

macro_rules! opaque_id {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub(crate) struct $name(String);

        impl $name {
            pub(crate) fn new(value: impl Into<String>) -> Result<Self, MigrationError> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > 512
                    || value.trim() != value
                    || value.chars().any(char::is_control)
                    || value.contains(['/', '\\'])
                {
                    return Err(MigrationError::InvalidContract($field));
                }
                Ok(Self(value))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

opaque_id!(MigrationId, "migration id");
opaque_id!(SchemaId, "schema id");
opaque_id!(RecordId, "record id");
opaque_id!(PayloadId, "payload id");
opaque_id!(QuarantineId, "quarantine id");
opaque_id!(StagingId, "staging id");
opaque_id!(BackupId, "backup id");
opaque_id!(FreezeAcceptanceId, "freeze acceptance id");
opaque_id!(PublicationId, "publication id");

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Digest(pub(crate) [u8; 32]);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum StoreFamily {
    Profile,
    Project,
    ProjectSessions,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FamilySchemaManifest {
    pub(crate) family: StoreFamily,
    pub(crate) schema_digest: Digest,
    pub(crate) transform_revision: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SchemaManifest {
    pub(crate) schema_id: SchemaId,
    pub(crate) revision: u32,
    pub(crate) canonical_digest: Digest,
    pub(crate) families: BTreeMap<StoreFamily, FamilySchemaManifest>,
}

impl SchemaManifest {
    fn validate(&self) -> Result<(), MigrationError> {
        if self.revision == 0 || self.families.is_empty() {
            return Err(MigrationError::InvalidContract("schema manifest"));
        }
        if self.families.iter().any(|(family, manifest)| {
            *family != manifest.family || manifest.transform_revision == 0
        }) {
            return Err(MigrationError::InvalidContract("family schema manifest"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LastReleasedSchemaManifest(SchemaManifest);

impl LastReleasedSchemaManifest {
    pub(crate) fn new(manifest: SchemaManifest) -> Result<Self, MigrationError> {
        manifest.validate()?;
        Ok(Self(manifest))
    }

    pub(crate) fn manifest(&self) -> &SchemaManifest {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinalSchemaManifest(SchemaManifest);

impl FinalSchemaManifest {
    pub(crate) fn new(manifest: SchemaManifest) -> Result<Self, MigrationError> {
        manifest.validate()?;
        Ok(Self(manifest))
    }

    pub(crate) fn manifest(&self) -> &SchemaManifest {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseFreezeProof {
    pub(crate) acceptance_id: FreezeAcceptanceId,
    pub(crate) last_released_digest: Digest,
    pub(crate) final_digest: Digest,
    pub(crate) proof_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MigrationRequest {
    pub(crate) migration_id: MigrationId,
    /// Canonical daemon-supplied bindings. Adapters must not derive these from locators.
    pub(crate) source_bindings: BTreeMap<StoreFamily, Vec<StoreRuntimeBindingV1>>,
    pub(crate) destination_epoch: StoreAuthorityEpochV1,
    pub(crate) freeze_proof: ReleaseFreezeProof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservedSchema {
    LastReleased(Digest),
    Final(Digest),
    Unknown {
        revision: Option<u32>,
        digest: Digest,
    },
    Corrupt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FamilyPreflight {
    pub(crate) family: StoreFamily,
    pub(crate) bindings: Vec<StoreRuntimeBindingV1>,
    pub(crate) observed_schema: ObservedSchema,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreflightReport {
    pub(crate) families: BTreeMap<StoreFamily, FamilyPreflight>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupReceipt {
    pub(crate) backup_id: BackupId,
    pub(crate) source_manifest_digest: Digest,
    pub(crate) artifact_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StagingHandle {
    pub(crate) staging_id: StagingId,
    pub(crate) migration_id: MigrationId,
    pub(crate) destination_epoch: StoreAuthorityEpochV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FamilyTransformPlan {
    pub(crate) family: StoreFamily,
    pub(crate) source_bindings: Vec<StoreRuntimeBindingV1>,
    pub(crate) source_schema_digest: Digest,
    pub(crate) destination_schema_digest: Digest,
    pub(crate) transform_revision: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FamilyVerification {
    pub(crate) source_count: u64,
    pub(crate) destination_count: u64,
    pub(crate) source_ids: BTreeSet<RecordId>,
    pub(crate) destination_ids: BTreeSet<RecordId>,
    pub(crate) source_ids_digest: Digest,
    pub(crate) destination_ids_digest: Digest,
    pub(crate) source_content_digest: Digest,
    pub(crate) destination_content_digest: Digest,
    pub(crate) source_fts_count: u64,
    pub(crate) destination_fts_count: u64,
    pub(crate) source_fts_digest: Digest,
    pub(crate) destination_fts_digest: Digest,
    pub(crate) source_payload_count: u64,
    pub(crate) destination_payload_count: u64,
    pub(crate) source_payload_ids: BTreeSet<PayloadId>,
    pub(crate) destination_payload_ids: BTreeSet<PayloadId>,
    pub(crate) source_payload_digest: Digest,
    pub(crate) destination_payload_digest: Digest,
    pub(crate) source_deletion_digest: Digest,
    pub(crate) destination_deletion_digest: Digest,
    pub(crate) source_deleted_ids: BTreeSet<RecordId>,
    pub(crate) destination_deleted_ids: BTreeSet<RecordId>,
    pub(crate) source_quarantine_digest: Digest,
    pub(crate) destination_quarantine_digest: Digest,
    pub(crate) source_quarantine_ids: BTreeSet<QuarantineId>,
    pub(crate) destination_quarantine_ids: BTreeSet<QuarantineId>,
}

impl FamilyVerification {
    pub(crate) fn is_exact(&self) -> bool {
        self.source_count == self.destination_count
            && self.source_count == self.source_ids.len() as u64
            && self.destination_count == self.destination_ids.len() as u64
            && self.source_ids == self.destination_ids
            && self.source_ids_digest == self.destination_ids_digest
            && self.source_content_digest == self.destination_content_digest
            && self.source_fts_count == self.destination_fts_count
            && self.source_fts_digest == self.destination_fts_digest
            && self.source_payload_count == self.destination_payload_count
            && self.source_payload_count == self.source_payload_ids.len() as u64
            && self.destination_payload_count == self.destination_payload_ids.len() as u64
            && self.source_payload_ids == self.destination_payload_ids
            && self.source_payload_digest == self.destination_payload_digest
            && self.source_deletion_digest == self.destination_deletion_digest
            && self.source_deleted_ids == self.destination_deleted_ids
            && self.source_quarantine_digest == self.destination_quarantine_digest
            && self.source_quarantine_ids == self.destination_quarantine_ids
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerificationReport {
    pub(crate) destination_manifest_digest: Digest,
    pub(crate) families: BTreeMap<StoreFamily, FamilyVerification>,
    pub(crate) integrity_check_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum DestinationCheckpoint {
    Staged {
        migration_id: MigrationId,
        staging_id: StagingId,
        destination_epoch: StoreAuthorityEpochV1,
        backup: BackupReceipt,
        final_manifest_digest: Digest,
    },
    Transforming {
        migration_id: MigrationId,
        staging_id: StagingId,
        destination_epoch: StoreAuthorityEpochV1,
        backup: BackupReceipt,
        final_manifest_digest: Digest,
        completed: Vec<StoreFamily>,
    },
    Verified {
        migration_id: MigrationId,
        staging_id: StagingId,
        destination_epoch: StoreAuthorityEpochV1,
        backup: BackupReceipt,
        final_manifest_digest: Digest,
        verification_digest: Digest,
    },
}

impl DestinationCheckpoint {
    pub(crate) fn backup(&self) -> &BackupReceipt {
        match self {
            Self::Staged { backup, .. }
            | Self::Transforming { backup, .. }
            | Self::Verified { backup, .. } => backup,
        }
    }

    pub(crate) fn completed(&self) -> &[StoreFamily] {
        match self {
            Self::Transforming { completed, .. } => completed,
            Self::Verified { .. } => &[],
            Self::Staged { .. } => &[],
        }
    }

    pub(crate) fn final_manifest_digest(&self) -> Digest {
        match self {
            Self::Staged {
                final_manifest_digest,
                ..
            }
            | Self::Transforming {
                final_manifest_digest,
                ..
            }
            | Self::Verified {
                final_manifest_digest,
                ..
            } => *final_manifest_digest,
        }
    }

    pub(crate) fn migration_id(&self) -> &MigrationId {
        match self {
            Self::Staged { migration_id, .. }
            | Self::Transforming { migration_id, .. }
            | Self::Verified { migration_id, .. } => migration_id,
        }
    }

    pub(crate) fn staging_id(&self) -> &StagingId {
        match self {
            Self::Staged { staging_id, .. }
            | Self::Transforming { staging_id, .. }
            | Self::Verified { staging_id, .. } => staging_id,
        }
    }

    pub(crate) fn destination_epoch(&self) -> StoreAuthorityEpochV1 {
        match self {
            Self::Staged {
                destination_epoch, ..
            }
            | Self::Transforming {
                destination_epoch, ..
            }
            | Self::Verified {
                destination_epoch, ..
            } => *destination_epoch,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PublicationReceipt {
    pub(crate) publication_id: PublicationId,
    pub(crate) migration_id: MigrationId,
    pub(crate) final_manifest_digest: Digest,
    pub(crate) destination_epoch: StoreAuthorityEpochV1,
    pub(crate) verification_digest: Digest,
    pub(crate) backup: BackupReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MigrationOutcome {
    Published(PublicationReceipt),
    Replayed(PublicationReceipt),
    AlreadyFinal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MigrationStage {
    FreezeProof,
    Preflight,
    Backup,
    Staging,
    Transform(StoreFamily),
    Verification,
    Publication,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MigrationError {
    InvalidContract(&'static str),
    FreezeProofRejected,
    UnknownFamily(StoreFamily),
    CorruptFamily(StoreFamily),
    SchemaMismatch(StoreFamily),
    BindingMismatch(StoreFamily),
    StaleDestinationEpoch,
    CheckpointMismatch,
    VerificationFailed(StoreFamily),
    Port {
        stage: MigrationStage,
        detail: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "migration blocked: {self:?}")
    }
}

impl std::error::Error for MigrationError {}
