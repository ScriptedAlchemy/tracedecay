use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_store::{FrozenWatermarkVectorV1, StoreRuntimeBindingV1, StoreShardIdV1};

pub const BACKUP_FORMAT_VERSION: u32 = 2;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub struct $name(pub(super) String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > 256
                    || value.chars().any(char::is_control)
                    || value.contains(['/', '\\'])
                    || matches!(value.as_str(), "." | "..")
                {
                    return Err(ManifestError::InvalidIdentity);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(PayloadId);
string_id!(BackupSetId);
string_id!(StagingId);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SchemaVersion(pub u32);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum PrivacyClass {
    Profile,
    Project,
    Public,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum DeletionState {
    Live,
    Tombstoned,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum ArtifactIdentity {
    Store(StoreShardIdV1),
    Payload(PayloadId),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    pub fn to_hex(self) -> String {
        use std::fmt::Write;

        self.0.iter().fold(
            String::with_capacity(self.0.len() * 2),
            |mut output, byte| {
                write!(output, "{byte:02x}").expect("writing to a string cannot fail");
                output
            },
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ArtifactManifest {
    pub identity: ArtifactIdentity,
    pub byte_length: u64,
    pub sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub backup_set: BackupSetId,
    pub frozen_watermarks: FrozenWatermarkVectorV1,
    pub schema_version: SchemaVersion,
    pub privacy: PrivacyClass,
    pub deletion: DeletionState,
    pub payload_closure: BTreeSet<PayloadId>,
    pub artifacts: Vec<ArtifactManifest>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StoredBackupManifest {
    pub manifest: BackupManifest,
    pub manifest_sha256: Sha256Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotArtifact {
    pub identity: ArtifactIdentity,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenFamilySnapshot {
    pub frozen_watermarks: FrozenWatermarkVectorV1,
    pub schema_version: SchemaVersion,
    pub privacy: PrivacyClass,
    pub deletion: DeletionState,
    pub payload_closure: BTreeSet<PayloadId>,
    pub artifacts: Vec<SnapshotArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreTarget {
    pub replacement_bindings: Vec<StoreRuntimeBindingV1>,
    pub staging: StagingId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    InvalidIdentity,
    UnsupportedFormat,
    EmptyStoreFamily,
    DuplicateArtifact,
    IncompleteStoreFamily,
    IncompletePayloadClosure,
    UnexpectedArtifact,
    LengthOverflow,
}
