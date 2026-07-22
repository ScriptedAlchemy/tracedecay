use std::{fs::Metadata as FileMetadata, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Command, ErrorCode, ErrorPayload, MAX_AUTHORITY_ID_BYTES, MAX_REQUEST_ID_BYTES,
    PROTOCOL_VERSION, validate_command,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u16,
    pub request_id: String,
    pub database: CopiedDatabase,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CopiedDatabase {
    pub path: PathBuf,
    pub kind: DatabaseKind,
    pub provenance: CopiedSnapshotProvenance,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    CopiedSnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CopiedSnapshotProvenance {
    pub authority_identity: String,
    pub staging_root: PathBuf,
    pub canonical_path: PathBuf,
    pub byte_len: u64,
    pub content_digest: String,
    pub file_identity: SnapshotFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum SnapshotFileIdentity {
    Unix {
        device: u64,
        inode: u64,
        links: u64,
    },
    Windows {
        volume_serial: u32,
        file_index: u64,
        links: u32,
    },
    Unsupported,
}

impl SnapshotFileIdentity {
    #[must_use]
    pub fn from_metadata(metadata: &FileMetadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Self::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
                links: metadata.nlink(),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = metadata;
            Self::Unsupported
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCopiedSnapshot {
    pub authority_identity: String,
    pub canonical_path: PathBuf,
    pub byte_len: u64,
    pub content_digest: String,
    pub file_identity: SnapshotFileIdentity,
}

pub fn validate_request(request: &Request) -> Result<(), ErrorPayload> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(ErrorPayload::new(
            ErrorCode::UnsupportedProtocolVersion,
            format!(
                "unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                request.protocol_version
            ),
        ));
    }
    if request.request_id.is_empty() || request.request_id.len() > MAX_REQUEST_ID_BYTES {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidRequest,
            format!("request_id must contain 1..={MAX_REQUEST_ID_BYTES} bytes"),
        ));
    }
    validate_copied_snapshot_provenance(&request.database.provenance)?;
    validate_command(&request.command)
}

pub fn validate_copied_snapshot_provenance(
    provenance: &CopiedSnapshotProvenance,
) -> Result<(), ErrorPayload> {
    if provenance.authority_identity.is_empty()
        || provenance.authority_identity.len() > MAX_AUTHORITY_ID_BYTES
        || provenance.authority_identity.as_bytes().contains(&0)
    {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!(
                "authority_identity must be nonempty, NUL-free, and at most {MAX_AUTHORITY_ID_BYTES} bytes"
            ),
        ));
    }
    if !is_canonical_sha256_digest(&provenance.content_digest) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "content_digest must be a lowercase sha256:<64 hex digits> value",
        ));
    }
    Ok(())
}

#[must_use]
pub fn is_canonical_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
