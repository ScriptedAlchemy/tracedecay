use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_store::StoreShardIdV1;

use super::model::*;

const DOMAIN_SEPARATOR: &[u8] = b"tracedecay.backup-manifest.v2\n";

#[derive(Serialize)]
struct ManifestWire<'a> {
    artifacts: Vec<ArtifactWire<'a>>,
    backup_set: &'a str,
    deletion: &'static str,
    format_version: u32,
    frozen_watermarks: Vec<WatermarkWire<'a>>,
    payload_closure: Vec<&'a str>,
    privacy: &'static str,
    schema_version: u32,
}

#[derive(Serialize)]
struct ArtifactWire<'a> {
    byte_length: u64,
    identity: ArtifactIdentityWire<'a>,
    sha256: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ArtifactIdentityWire<'a> {
    Store {
        id: &'a StoreShardIdV1,
        kind: &'static str,
    },
    Payload {
        id: &'a str,
        kind: &'static str,
    },
}

#[derive(Serialize)]
struct WatermarkWire<'a> {
    authority_epoch: u64,
    commit_sequence: u64,
    incarnation: u64,
    shard_id: &'a StoreShardIdV1,
}

impl BackupManifest {
    /// Stable typed representation used for corruption and idempotency checks.
    /// Collection order is part of the wire contract, not caller insertion order.
    pub fn stable_bytes(&self) -> Vec<u8> {
        let mut artifacts = self.artifacts.iter().collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.identity.cmp(&right.identity));

        let wire = ManifestWire {
            artifacts: artifacts.into_iter().map(ArtifactWire::from).collect(),
            backup_set: self.backup_set.as_str(),
            deletion: match self.deletion {
                DeletionState::Live => "live",
                DeletionState::Tombstoned => "tombstoned",
            },
            format_version: self.format_version,
            frozen_watermarks: self
                .frozen_watermarks
                .iter()
                .map(|(_, watermark)| WatermarkWire {
                    authority_epoch: watermark.authority_epoch.get(),
                    commit_sequence: watermark.commit_sequence.0,
                    incarnation: watermark.incarnation.get(),
                    shard_id: &watermark.shard_id,
                })
                .collect(),
            payload_closure: self.payload_closure.iter().map(PayloadId::as_str).collect(),
            privacy: match self.privacy {
                PrivacyClass::Profile => "profile",
                PrivacyClass::Project => "project",
                PrivacyClass::Public => "public",
            },
            schema_version: self.schema_version.0,
        };

        let mut serialized =
            serde_json::to_vec(&wire).expect("backup wire values always serialize");
        serialized.push(b'\n');
        [DOMAIN_SEPARATOR, serialized.as_slice()].concat()
    }
}

pub(super) fn sha256(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest(Sha256::digest(bytes).into())
}

pub(super) fn manifest_digest(manifest: &BackupManifest) -> Sha256Digest {
    sha256(&manifest.stable_bytes())
}

impl<'a> From<&'a ArtifactManifest> for ArtifactWire<'a> {
    fn from(artifact: &'a ArtifactManifest) -> Self {
        Self {
            byte_length: artifact.byte_length,
            identity: match &artifact.identity {
                ArtifactIdentity::Store(id) => ArtifactIdentityWire::Store {
                    id,
                    kind: "store_shard",
                },
                ArtifactIdentity::Payload(id) => ArtifactIdentityWire::Payload {
                    id: id.as_str(),
                    kind: "payload",
                },
            },
            sha256: hex(&artifact.sha256.0),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        },
    )
}
