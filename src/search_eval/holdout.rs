#![allow(dead_code)] // in-flight feature APIs not yet wired; see clippy sweep
//! Private holdout authority storage and Ed25519 verification.
//!
//! All sensitive bytes live below the supported `TraceDecay` user profile. Public
//! values are content-addressed locators, digests, fingerprints, and lifecycle
//! metadata; this module never returns a private path or signing seed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AcceptedPr9CandidateEvidenceV1, AgentAdjudicatedLabelProvenanceV1, AgentJudgmentProvenanceV1,
    DecisionOwnerId, EvaluationContractError, FixtureAuthorityV1, FixtureContentDigest,
    HoldoutAccessReceiptV1, HoldoutLabelAuthorityV1, HoldoutRevealCapabilityV1, HoldoutSealDigest,
    HoldoutSealV1, JudgmentId, LabelSetDigest, QueryWorkloadV1, RelevanceJudgmentV1, RunManifestV1,
    SemanticCandidateEvidenceV1,
};

use crate::storage::{self, PrivateStoreIo};

const AUTHORITY_ROOT: &str = "search-quality-holdout-v1";
const REGISTRY_FILE: &str = "immutable-registry-v1.jsonl";
const KEY_REGISTRY_FILE: &str = "decision-owner-keys-v1.jsonl";
const TRUST_EVENTS_FILE: &str = "trust-events-v1.jsonl";
const REVEAL_RECEIPTS_FILE: &str = "reveal-receipts-v1.jsonl";
const LOCATOR_PREFIX: &str = "authorized-store://search-quality/holdout/v1/";
const MAX_PRIVATE_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
const CAPABILITY_SIGNATURE_DOMAIN: &str = "tracedecay.holdout-reveal-capability.v1";
const DELEGATION_SIGNATURE_DOMAIN: &str = "tracedecay.holdout-agent-delegation.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutArtifactKindV1 {
    BlindedPacket,
    SignedDelegation,
    AgentJudgment,
    SealedLabels,
    SignedEnvelope,
    RevealCapability,
}

impl HoldoutArtifactKindV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::BlindedPacket => "blinded-packet",
            Self::SignedDelegation => "signed-delegation",
            Self::AgentJudgment => "agent-judgment",
            Self::SealedLabels => "sealed-labels",
            Self::SignedEnvelope => "signed-envelope",
            Self::RevealCapability => "reveal-capability",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "blinded-packet" => Some(Self::BlindedPacket),
            "signed-delegation" => Some(Self::SignedDelegation),
            "agent-judgment" => Some(Self::AgentJudgment),
            "sealed-labels" => Some(Self::SealedLabels),
            "signed-envelope" => Some(Self::SignedEnvelope),
            "reveal-capability" => Some(Self::RevealCapability),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutRegistryRecordV1 {
    pub schema_revision: u32,
    pub locator: String,
    pub kind: HoldoutArtifactKindV1,
    pub content_digest: FixtureContentDigest,
    pub byte_len: u64,
    pub created_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOwnerKeySpecV1 {
    pub owner_id: DecisionOwnerId,
    pub trust_root_id: String,
    pub not_before_unix: u64,
    pub not_after_unix: u64,
    pub rotation_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOwnerKeyReceiptV1 {
    pub owner_id: DecisionOwnerId,
    pub trust_root_id: String,
    pub public_key_fingerprint: FixtureContentDigest,
    pub not_before_unix: u64,
    pub not_after_unix: u64,
    pub rotation_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionOwnerKeyRecordV1 {
    schema_revision: u32,
    spec: DecisionOwnerKeySpecV1,
    public_key_hex: String,
    public_key_fingerprint: FixtureContentDigest,
    secret_id: String,
    created_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustRootRecordV1 {
    owner_id: DecisionOwnerId,
    trust_root_id: String,
    public_key_hex: String,
    public_key_fingerprint: FixtureContentDigest,
    not_before_unix: u64,
    not_after_unix: u64,
    rotation_epoch: u64,
    registered_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum TrustEventV1 {
    Registered {
        schema_revision: u32,
        root: TrustRootRecordV1,
    },
    Rotated {
        schema_revision: u32,
        previous_trust_root_id: String,
        root: TrustRootRecordV1,
        rotated_at_unix: u64,
    },
    Revoked {
        schema_revision: u32,
        trust_root_id: String,
        revoked_at_unix: u64,
        reason: String,
    },
}

pub struct ResolvedHoldoutTrustRootV1 {
    pub owner_id: DecisionOwnerId,
    pub trust_root_id: String,
    pub public_key_fingerprint: FixtureContentDigest,
    pub not_before_unix: u64,
    pub not_after_unix: u64,
    pub rotation_epoch: u64,
    public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDelegationPayloadV1 {
    pub schema_revision: u32,
    pub delegated_by: DecisionOwnerId,
    pub blinded_packet_digest: FixtureContentDigest,
    pub signed_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedAgentDelegationV1 {
    pub schema_revision: u32,
    pub payload: AgentDelegationPayloadV1,
    pub trust_root_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hex: Option<String>,
}

#[derive(Serialize)]
struct AgentDelegationSignaturePayload<'a> {
    domain: &'static str,
    schema_revision: u32,
    payload: &'a AgentDelegationPayloadV1,
    trust_root_id: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentJudgmentArtifactV1 {
    pub schema_revision: u32,
    pub independent_judgment_id: JudgmentId,
    pub adjudicator_instance_id: String,
    pub adjudicator_model: String,
    pub adjudicator_version: String,
    pub judged_at_unix: u64,
    pub blinded_packet_digest: FixtureContentDigest,
    pub label_set_digest: LabelSetDigest,
    pub judgments: Vec<RelevanceJudgmentV1>,
}

impl AgentJudgmentArtifactV1 {
    pub fn provenance(
        &self,
        immutable_judgment_artifact_digest: FixtureContentDigest,
    ) -> AgentJudgmentProvenanceV1 {
        AgentJudgmentProvenanceV1 {
            independent_judgment_id: self.independent_judgment_id.clone(),
            adjudicator_instance_id: self.adjudicator_instance_id.clone(),
            adjudicator_model: self.adjudicator_model.clone(),
            adjudicator_version: self.adjudicator_version.clone(),
            judged_at_unix: self.judged_at_unix,
            blinded_packet_digest: self.blinded_packet_digest.clone(),
            immutable_judgment_artifact_digest,
            label_set_digest: self.label_set_digest.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutEnvelopePayloadV1 {
    pub schema_revision: u32,
    pub labels_locator: String,
    pub labels_content_digest: FixtureContentDigest,
    pub seal_digest: HoldoutSealDigest,
    pub label_authority: HoldoutLabelAuthorityV1,
    pub signed_by: DecisionOwnerId,
    pub trust_root_id: String,
    pub signed_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedHoldoutEnvelopeV1 {
    pub schema_revision: u32,
    pub payload: HoldoutEnvelopePayloadV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hex: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldoutEnvelopeExpectationV1 {
    pub labels_locator: String,
    pub labels_content_digest: FixtureContentDigest,
    pub seal_digest: HoldoutSealDigest,
    pub label_authority: HoldoutLabelAuthorityV1,
    pub signed_envelope_digest: FixtureContentDigest,
    pub decision_owners: Vec<DecisionOwnerId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedHoldoutEnvelopeV1 {
    pub labels_locator: String,
    pub labels_content_digest: FixtureContentDigest,
    pub seal_digest: HoldoutSealDigest,
    pub label_authority: HoldoutLabelAuthorityV1,
    pub signed_by: DecisionOwnerId,
    pub trust_root_id: String,
    pub public_key_fingerprint: FixtureContentDigest,
    pub rotation_epoch: u64,
    pub signed_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedHoldoutRevealCapabilityV1 {
    pub schema_revision: u32,
    pub capability: HoldoutRevealCapabilityV1,
    pub signed_by: DecisionOwnerId,
    pub trust_root_id: String,
    pub signed_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_hex: Option<String>,
}

#[derive(Serialize)]
struct RevealCapabilitySignaturePayload<'a> {
    domain: &'static str,
    schema_revision: u32,
    capability: &'a HoldoutRevealCapabilityV1,
    signed_by: &'a DecisionOwnerId,
    trust_root_id: &'a str,
    signed_at_unix: u64,
}

struct VerifiedRevealCapabilityV1 {
    capability: HoldoutRevealCapabilityV1,
    capability_digest: FixtureContentDigest,
    signed_by: DecisionOwnerId,
    trust_root_id: String,
}

#[derive(Debug, Error)]
pub enum HoldoutAuthorityError {
    #[error("private holdout storage rejected an unsafe path")]
    UnsafeStorage,
    #[error("private holdout storage failed during {operation} ({kind:?})")]
    Storage {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    #[error("private holdout object exceeds the size limit")]
    ObjectTooLarge,
    #[error("invalid holdout locator")]
    InvalidLocator,
    #[error("holdout registry has no immutable record for the locator")]
    UnknownLocator,
    #[error("holdout registry contains conflicting immutable records")]
    RegistryConflict,
    #[error("holdout object kind does not match the requested authority")]
    KindMismatch,
    #[error("holdout content digest mismatch for {locator}")]
    DigestMismatch { locator: String },
    #[error("holdout metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("decision-owner trust root is already registered: {root_id}")]
    DuplicateTrustRoot { root_id: String },
    #[error("decision-owner trust root is unknown: {root_id}")]
    UnknownTrustRoot { root_id: String },
    #[error("decision-owner trust root is retired: {root_id}")]
    RetiredTrustRoot { root_id: String },
    #[error("decision-owner trust root is revoked: {root_id}")]
    RevokedTrustRoot { root_id: String },
    #[error("decision-owner trust root is outside its validity window: {root_id}")]
    TrustRootOutsideValidity { root_id: String },
    #[error("decision-owner key material is unavailable")]
    KeyUnavailable,
    #[error("signed holdout envelope does not match {field}")]
    EnvelopeBindingMismatch { field: &'static str },
    #[error("signed holdout envelope signature is invalid")]
    InvalidSignature,
    #[error("failed to generate cryptographic randomness")]
    Randomness,
}

pub struct HoldoutAuthorityStoreV1 {
    root: PathBuf,
}

impl HoldoutAuthorityStoreV1 {
    pub fn open_default() -> Result<Self, HoldoutAuthorityError> {
        let profile_root =
            storage::default_profile_root().map_err(|_| HoldoutAuthorityError::Storage {
                operation: "resolve user profile",
                kind: io::ErrorKind::NotFound,
            })?;
        Self::open_at(profile_root)
    }

    pub fn open_at(profile_root: impl AsRef<Path>) -> Result<Self, HoldoutAuthorityError> {
        let root = profile_root.as_ref().join(AUTHORITY_ROOT);
        PrivateStoreIo::create_dir_all(&root)
            .map_err(|error| map_io("create authority root", error))?;
        verify_private_directory(&root)?;
        Ok(Self { root })
    }

    fn write_immutable(
        &self,
        kind: HoldoutArtifactKindV1,
        contents: &[u8],
        created_at_unix: u64,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        let byte_len =
            u64::try_from(contents.len()).map_err(|_| HoldoutAuthorityError::ObjectTooLarge)?;
        if byte_len == 0 || byte_len > MAX_PRIVATE_OBJECT_BYTES {
            return Err(HoldoutAuthorityError::ObjectTooLarge);
        }
        let content_digest = digest_bytes(contents)?;
        let locator = locator_for(kind, &content_digest)?;
        let objects_root = self.root.join("objects");
        PrivateStoreIo::create_dir_all(&objects_root)
            .map_err(|error| map_io("create immutable object root", error))?;
        PrivateStoreIo::create_dir_all(&objects_root.join(kind.as_str()))
            .map_err(|error| map_io("create immutable object directory", error))?;
        let object_path = self.object_path(kind, &content_digest)?;
        match create_private_file_new(&object_path, contents) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = read_private_file(&object_path)?;
                if existing != contents {
                    return Err(HoldoutAuthorityError::DigestMismatch {
                        locator: locator.clone(),
                    });
                }
            }
            Err(error) => return Err(map_io("write immutable object", error)),
        }

        let record = HoldoutRegistryRecordV1 {
            schema_revision: 1,
            locator,
            kind,
            content_digest,
            byte_len,
            created_at_unix,
        };
        let records = self.registry_records()?;
        if let Some(existing) = records.iter().find(|entry| entry.locator == record.locator) {
            if existing.kind != record.kind
                || existing.content_digest != record.content_digest
                || existing.byte_len != record.byte_len
            {
                return Err(HoldoutAuthorityError::RegistryConflict);
            }
            return Ok(existing.clone());
        }
        self.append_json_line(REGISTRY_FILE, &record)?;
        Ok(record)
    }

    fn resolve_immutable(
        &self,
        locator: &str,
        expected_kind: HoldoutArtifactKindV1,
    ) -> Result<Vec<u8>, HoldoutAuthorityError> {
        let (kind, digest) = parse_locator(locator)?;
        if kind != expected_kind {
            return Err(HoldoutAuthorityError::KindMismatch);
        }
        let matching = self
            .registry_records()?
            .into_iter()
            .filter(|record| record.locator == locator)
            .collect::<Vec<_>>();
        let first = matching
            .first()
            .cloned()
            .ok_or(HoldoutAuthorityError::UnknownLocator)?;
        if matching.iter().any(|record| {
            record.kind != first.kind
                || record.content_digest != first.content_digest
                || record.byte_len != first.byte_len
        }) {
            return Err(HoldoutAuthorityError::RegistryConflict);
        }
        if first.kind != kind || first.content_digest != digest {
            return Err(HoldoutAuthorityError::RegistryConflict);
        }
        if first.byte_len == 0 || first.byte_len > MAX_PRIVATE_OBJECT_BYTES {
            return Err(HoldoutAuthorityError::ObjectTooLarge);
        }
        let path = self.object_path(kind, &digest)?;
        let bytes = read_private_file(&path)?;
        if u64::try_from(bytes.len()).ok() != Some(first.byte_len)
            || digest_bytes(&bytes)? != first.content_digest
        {
            return Err(HoldoutAuthorityError::DigestMismatch {
                locator: locator.to_string(),
            });
        }
        Ok(bytes)
    }

    /// Provision sealed labels into the private profile store. This returns
    /// only opaque metadata; label bytes remain inaccessible except through
    /// [`HoldoutAuthorityStoreV1::evaluate_locked_labels`].
    pub fn import_sealed_labels(
        &self,
        contents: &[u8],
        created_at_unix: u64,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        self.write_immutable(
            HoldoutArtifactKindV1::SealedLabels,
            contents,
            created_at_unix,
        )
    }

    pub fn import_blinded_packet(
        &self,
        contents: &[u8],
        created_at_unix: u64,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if contents.is_empty() {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "blinded packet is empty".to_string(),
            ));
        }
        self.write_immutable(
            HoldoutArtifactKindV1::BlindedPacket,
            contents,
            created_at_unix,
        )
    }

    pub fn sign_agent_delegation(
        &self,
        payload: AgentDelegationPayloadV1,
        trust_root_id: &str,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if payload.schema_revision != 1 || payload.signed_at_unix == 0 {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "invalid agent delegation metadata".to_string(),
            ));
        }
        let packet_locator = locator_for(
            HoldoutArtifactKindV1::BlindedPacket,
            &payload.blinded_packet_digest,
        )?;
        self.resolve_immutable(&packet_locator, HoldoutArtifactKindV1::BlindedPacket)?;
        let trust_root_id = if trust_root_id.is_empty() {
            local_owner_trust_root_id(&payload.delegated_by)
        } else {
            trust_root_id.to_string()
        };
        let signature_payload = AgentDelegationSignaturePayload {
            domain: DELEGATION_SIGNATURE_DOMAIN,
            schema_revision: 1,
            payload: &payload,
            trust_root_id: &trust_root_id,
        };
        let payload_bytes = serde_json::to_vec(&signature_payload)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        let signature_hex = if is_local_owner_trust_root(&payload.delegated_by, &trust_root_id) {
            None
        } else {
            let root = self.resolve_trust_root(&trust_root_id, payload.signed_at_unix)?;
            if root.owner_id != payload.delegated_by {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "agent delegation owner",
                });
            }
            Some(hex::encode(
                self.load_signing_key(&trust_root_id)?
                    .sign(&payload_bytes)
                    .to_bytes(),
            ))
        };
        let signed = SignedAgentDelegationV1 {
            schema_revision: 1,
            payload,
            trust_root_id,
            signature_hex,
        };
        let bytes = serde_json::to_vec_pretty(&signed)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        self.write_immutable(
            HoldoutArtifactKindV1::SignedDelegation,
            &bytes,
            signed.payload.signed_at_unix,
        )
    }

    pub fn import_agent_judgment(
        &self,
        artifact: &AgentJudgmentArtifactV1,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if artifact.schema_revision != 1
            || artifact.adjudicator_instance_id.trim().is_empty()
            || artifact.adjudicator_model.trim().is_empty()
            || artifact.adjudicator_version.trim().is_empty()
            || artifact.judged_at_unix == 0
            || artifact.judgments.is_empty()
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "invalid agent judgment artifact".to_string(),
            ));
        }
        let packet_locator = locator_for(
            HoldoutArtifactKindV1::BlindedPacket,
            &artifact.blinded_packet_digest,
        )?;
        self.resolve_immutable(&packet_locator, HoldoutArtifactKindV1::BlindedPacket)?;
        if super::sealed_holdout_label_set_digest(&artifact.judgments)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?
            != artifact.label_set_digest
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "agent judgment label-set digest mismatch".to_string(),
            ));
        }
        let bytes = serde_json::to_vec_pretty(artifact)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        self.write_immutable(
            HoldoutArtifactKindV1::AgentJudgment,
            &bytes,
            artifact.judged_at_unix,
        )
    }

    pub fn generate_decision_owner_key(
        &self,
        spec: DecisionOwnerKeySpecV1,
        created_at_unix: u64,
    ) -> Result<DecisionOwnerKeyReceiptV1, HoldoutAuthorityError> {
        validate_key_spec(&spec)?;
        if self
            .key_records()?
            .iter()
            .any(|record| record.spec.trust_root_id == spec.trust_root_id)
            || self.trust_roots()?.contains_key(&spec.trust_root_id)
        {
            return Err(HoldoutAuthorityError::DuplicateTrustRoot {
                root_id: spec.trust_root_id,
            });
        }
        let (key_record, root) = self.create_key_record(spec, created_at_unix)?;
        self.append_json_line(KEY_REGISTRY_FILE, &key_record)?;
        self.append_json_line(
            TRUST_EVENTS_FILE,
            &TrustEventV1::Registered {
                schema_revision: 1,
                root: root.clone(),
            },
        )?;
        Ok(key_receipt(&root))
    }

    pub fn decision_owner_keys(
        &self,
    ) -> Result<Vec<DecisionOwnerKeyReceiptV1>, HoldoutAuthorityError> {
        Ok(self.trust_roots()?.values().map(key_receipt).collect())
    }

    pub fn rotate_decision_owner_key(
        &self,
        previous_trust_root_id: &str,
        spec: DecisionOwnerKeySpecV1,
        rotated_at_unix: u64,
    ) -> Result<DecisionOwnerKeyReceiptV1, HoldoutAuthorityError> {
        let previous = self.resolve_trust_root(previous_trust_root_id, rotated_at_unix)?;
        validate_key_spec(&spec)?;
        if previous.owner_id != spec.owner_id
            || spec.rotation_epoch != previous.rotation_epoch.saturating_add(1)
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "rotation must retain owner and increment the epoch exactly once".to_string(),
            ));
        }
        if self.trust_roots()?.contains_key(&spec.trust_root_id) {
            return Err(HoldoutAuthorityError::DuplicateTrustRoot {
                root_id: spec.trust_root_id,
            });
        }
        let (key_record, root) = self.create_key_record(spec, rotated_at_unix)?;
        self.append_json_line(KEY_REGISTRY_FILE, &key_record)?;
        self.append_json_line(
            TRUST_EVENTS_FILE,
            &TrustEventV1::Rotated {
                schema_revision: 1,
                previous_trust_root_id: previous_trust_root_id.to_string(),
                root: root.clone(),
                rotated_at_unix,
            },
        )?;
        Ok(key_receipt(&root))
    }

    pub fn revoke_trust_root(
        &self,
        trust_root_id: &str,
        revoked_at_unix: u64,
        reason: &str,
    ) -> Result<(), HoldoutAuthorityError> {
        if reason.trim().is_empty() {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "revocation reason is empty".to_string(),
            ));
        }
        if !self.trust_roots()?.contains_key(trust_root_id) {
            return Err(HoldoutAuthorityError::UnknownTrustRoot {
                root_id: trust_root_id.to_string(),
            });
        }
        if self.revocations()?.contains(trust_root_id) {
            return Err(HoldoutAuthorityError::RevokedTrustRoot {
                root_id: trust_root_id.to_string(),
            });
        }
        self.append_json_line(
            TRUST_EVENTS_FILE,
            &TrustEventV1::Revoked {
                schema_revision: 1,
                trust_root_id: trust_root_id.to_string(),
                revoked_at_unix,
                reason: reason.to_string(),
            },
        )
    }

    pub fn resolve_trust_root(
        &self,
        trust_root_id: &str,
        at_unix: u64,
    ) -> Result<ResolvedHoldoutTrustRootV1, HoldoutAuthorityError> {
        let roots = self.trust_roots()?;
        let root =
            roots
                .get(trust_root_id)
                .ok_or_else(|| HoldoutAuthorityError::UnknownTrustRoot {
                    root_id: trust_root_id.to_string(),
                })?;
        if self.revocations()?.contains(trust_root_id) {
            return Err(HoldoutAuthorityError::RevokedTrustRoot {
                root_id: trust_root_id.to_string(),
            });
        }
        if self.retired_roots(at_unix)?.contains(trust_root_id) {
            return Err(HoldoutAuthorityError::RetiredTrustRoot {
                root_id: trust_root_id.to_string(),
            });
        }
        if at_unix < root.registered_at_unix
            || at_unix < root.not_before_unix
            || at_unix > root.not_after_unix
        {
            return Err(HoldoutAuthorityError::TrustRootOutsideValidity {
                root_id: trust_root_id.to_string(),
            });
        }
        Ok(ResolvedHoldoutTrustRootV1 {
            owner_id: root.owner_id.clone(),
            trust_root_id: root.trust_root_id.clone(),
            public_key_fingerprint: root.public_key_fingerprint.clone(),
            not_before_unix: root.not_before_unix,
            not_after_unix: root.not_after_unix,
            rotation_epoch: root.rotation_epoch,
            public_key: decode_hex_array(&root.public_key_hex)
                .map_err(|()| HoldoutAuthorityError::KeyUnavailable)?,
        })
    }

    pub fn sign_envelope(
        &self,
        mut payload: HoldoutEnvelopePayloadV1,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if payload.trust_root_id.is_empty() {
            payload.trust_root_id = local_owner_trust_root_id(&payload.signed_by);
        }
        validate_envelope_payload(&payload)?;
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        let signature_hex = if is_local_owner_trust_root(&payload.signed_by, &payload.trust_root_id)
        {
            None
        } else {
            let root = self.resolve_trust_root(&payload.trust_root_id, payload.signed_at_unix)?;
            if root.owner_id != payload.signed_by {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "decision owner",
                });
            }
            let signing_key = self.load_signing_key(&payload.trust_root_id)?;
            if signing_key.verifying_key().to_bytes() != root.public_key {
                return Err(HoldoutAuthorityError::KeyUnavailable);
            }
            Some(hex::encode(signing_key.sign(&payload_bytes).to_bytes()))
        };
        let envelope = SignedHoldoutEnvelopeV1 {
            schema_revision: 1,
            payload,
            signature_hex,
        };
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        self.write_immutable(
            HoldoutArtifactKindV1::SignedEnvelope,
            &bytes,
            envelope.payload.signed_at_unix,
        )
    }

    fn verify_envelope(
        &self,
        envelope_locator: &str,
        expected: &HoldoutEnvelopeExpectationV1,
        now_unix: u64,
    ) -> Result<VerifiedHoldoutEnvelopeV1, HoldoutAuthorityError> {
        let bytes =
            self.resolve_immutable(envelope_locator, HoldoutArtifactKindV1::SignedEnvelope)?;
        if digest_bytes(&bytes)? != expected.signed_envelope_digest {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "signed envelope digest",
            });
        }
        let envelope: SignedHoldoutEnvelopeV1 = serde_json::from_slice(&bytes)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if envelope.schema_revision != 1 || envelope.payload.schema_revision != 1 {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "unsupported signed envelope revision".to_string(),
            ));
        }
        for (matches, field) in [
            (
                envelope.payload.labels_locator == expected.labels_locator,
                "labels locator",
            ),
            (
                envelope.payload.labels_content_digest == expected.labels_content_digest,
                "labels content digest",
            ),
            (
                envelope.payload.seal_digest == expected.seal_digest,
                "seal digest",
            ),
            (
                envelope.payload.label_authority == expected.label_authority,
                "label authority",
            ),
            (
                expected
                    .decision_owners
                    .contains(&envelope.payload.signed_by),
                "decision owner",
            ),
        ] {
            if !matches {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch { field });
            }
        }
        let (public_key_fingerprint, rotation_epoch) = if is_local_owner_trust_root(
            &envelope.payload.signed_by,
            &envelope.payload.trust_root_id,
        ) {
            if envelope.signature_hex.is_some()
                || envelope.payload.signed_at_unix == 0
                || envelope.payload.signed_at_unix > now_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "local owner envelope metadata",
                });
            }
            (
                digest_bytes(envelope.payload.signed_by.as_str().as_bytes())?,
                0,
            )
        } else {
            let root = self.resolve_trust_root(&envelope.payload.trust_root_id, now_unix)?;
            if root.owner_id != envelope.payload.signed_by
                || envelope.payload.signed_at_unix < root.not_before_unix
                || envelope.payload.signed_at_unix > root.not_after_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "trust root owner or validity",
                });
            }
            let signature_hex = envelope
                .signature_hex
                .as_deref()
                .ok_or(HoldoutAuthorityError::InvalidSignature)?;
            let signature_bytes: [u8; 64] = decode_hex_array(signature_hex)
                .map_err(|()| HoldoutAuthorityError::InvalidSignature)?;
            let payload_bytes = serde_json::to_vec(&envelope.payload)
                .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
            VerifyingKey::from_bytes(&root.public_key)
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?
                .verify_strict(&payload_bytes, &Signature::from_bytes(&signature_bytes))
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?;
            (root.public_key_fingerprint, root.rotation_epoch)
        };
        Ok(VerifiedHoldoutEnvelopeV1 {
            labels_locator: envelope.payload.labels_locator,
            labels_content_digest: envelope.payload.labels_content_digest,
            seal_digest: envelope.payload.seal_digest,
            label_authority: envelope.payload.label_authority,
            signed_by: envelope.payload.signed_by,
            trust_root_id: envelope.payload.trust_root_id,
            public_key_fingerprint,
            rotation_epoch,
            signed_at_unix: envelope.payload.signed_at_unix,
        })
    }

    pub fn sign_reveal_capability(
        &self,
        capability: HoldoutRevealCapabilityV1,
        trust_root_id: &str,
        signed_at_unix: u64,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        capability
            .validate_shape()
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        let trust_root_id = if trust_root_id.is_empty() {
            local_owner_trust_root_id(&capability.revealed_by)
        } else {
            trust_root_id.to_string()
        };
        let signed_by = capability.revealed_by.clone();
        let payload = RevealCapabilitySignaturePayload {
            domain: CAPABILITY_SIGNATURE_DOMAIN,
            schema_revision: 1,
            capability: &capability,
            signed_by: &signed_by,
            trust_root_id: &trust_root_id,
            signed_at_unix,
        };
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        let signature_hex = if is_local_owner_trust_root(&capability.revealed_by, &trust_root_id) {
            None
        } else {
            let root = self.resolve_trust_root(&trust_root_id, signed_at_unix)?;
            if root.owner_id != capability.revealed_by {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "reveal capability signer",
                });
            }
            let signing_key = self.load_signing_key(&trust_root_id)?;
            if signing_key.verifying_key().to_bytes() != root.public_key {
                return Err(HoldoutAuthorityError::KeyUnavailable);
            }
            Some(hex::encode(signing_key.sign(&payload_bytes).to_bytes()))
        };
        let signed = SignedHoldoutRevealCapabilityV1 {
            schema_revision: 1,
            capability,
            signed_by,
            trust_root_id,
            signed_at_unix,
            signature_hex,
        };
        let bytes = serde_json::to_vec_pretty(&signed)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        self.write_immutable(
            HoldoutArtifactKindV1::RevealCapability,
            &bytes,
            signed_at_unix,
        )
    }

    fn verify_reveal_capability(
        &self,
        locator: &str,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
        now_unix: u64,
    ) -> Result<VerifiedRevealCapabilityV1, HoldoutAuthorityError> {
        run.validate()
            .and_then(|()| run.verify_digest())
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if run.scope != tracedecay_domain::EvalRunScopeV1::Locked
            || run.authority != FixtureAuthorityV1::LockedQuality
            || run.locked_outcomes_accessed
            || run.holdout_seal_digest != seal.seal_digest
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "frozen run",
            });
        }
        let bytes = self.resolve_immutable(locator, HoldoutArtifactKindV1::RevealCapability)?;
        let capability_digest = digest_bytes(&bytes)?;
        let signed: SignedHoldoutRevealCapabilityV1 = serde_json::from_slice(&bytes)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if signed.schema_revision != 1 {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "unsupported signed reveal capability revision".to_string(),
            ));
        }
        signed
            .capability
            .validate_shape()
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        signed
            .capability
            .is_valid_at(now_unix)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if signed.capability.run_id != run.run_id
            || signed.capability.run_manifest_digest != run.digest
            || signed.capability.labels_locator != seal.locator
            || signed.capability.envelope_locator != seal.signature_locator
            || signed.capability.seal_digest != seal.seal_digest
            || signed.capability.revealed_by != signed.signed_by
            || !decision_owners.contains(&signed.signed_by)
            || !run.decision_owners.contains(&signed.signed_by)
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "signed reveal capability",
            });
        }
        let payload = RevealCapabilitySignaturePayload {
            domain: CAPABILITY_SIGNATURE_DOMAIN,
            schema_revision: signed.schema_revision,
            capability: &signed.capability,
            signed_by: &signed.signed_by,
            trust_root_id: &signed.trust_root_id,
            signed_at_unix: signed.signed_at_unix,
        };
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if is_local_owner_trust_root(&signed.signed_by, &signed.trust_root_id) {
            if signed.signature_hex.is_some()
                || signed.signed_at_unix == 0
                || signed.signed_at_unix > now_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "local reveal capability metadata",
                });
            }
        } else {
            let root = self.resolve_trust_root(&signed.trust_root_id, now_unix)?;
            if root.owner_id != signed.signed_by
                || signed.signed_at_unix < root.not_before_unix
                || signed.signed_at_unix > root.not_after_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "reveal capability trust root",
                });
            }
            let signature_hex = signed
                .signature_hex
                .as_deref()
                .ok_or(HoldoutAuthorityError::InvalidSignature)?;
            let signature_bytes: [u8; 64] = decode_hex_array(signature_hex)
                .map_err(|()| HoldoutAuthorityError::InvalidSignature)?;
            VerifyingKey::from_bytes(&root.public_key)
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?
                .verify_strict(&payload_bytes, &Signature::from_bytes(&signature_bytes))
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?;
        }
        Ok(VerifiedRevealCapabilityV1 {
            capability: signed.capability,
            capability_digest,
            signed_by: signed.signed_by,
            trust_root_id: signed.trust_root_id,
        })
    }

    /// The sole label-reading operation. It validates the frozen run and the
    /// signed, run-bound capability before opening either the signed envelope
    /// or sealed label object, then durably appends the reveal receipt before
    /// invoking the evaluator.
    pub(crate) fn evaluate_locked_labels<T>(
        &self,
        capability_locator: &str,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
        now_unix: u64,
        evaluate: impl FnOnce(&[u8]) -> Result<T, HoldoutAuthorityError>,
    ) -> Result<(T, HoldoutAccessReceiptV1), HoldoutAuthorityError> {
        let verified = self.verify_reveal_capability(
            capability_locator,
            run,
            seal,
            decision_owners,
            now_unix,
        )?;
        let labels_content_digest = seal.labels_content_digest.clone().ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no labels content digest".to_string(),
            )
        })?;
        let label_authority = seal.label_authority.ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no label authority".to_string(),
            )
        })?;
        let envelope = self.verify_envelope(
            &verified.capability.envelope_locator,
            &HoldoutEnvelopeExpectationV1 {
                labels_locator: seal.locator.clone(),
                labels_content_digest: labels_content_digest.clone(),
                seal_digest: seal.seal_digest.clone(),
                label_authority,
                signed_envelope_digest: seal.signed_envelope_digest.clone(),
                decision_owners: decision_owners.to_vec(),
            },
            now_unix,
        )?;
        if envelope.signed_by != verified.signed_by
            || envelope.trust_root_id != verified.trust_root_id
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "capability and envelope signer",
            });
        }
        let labels = self.resolve_immutable(
            &verified.capability.labels_locator,
            HoldoutArtifactKindV1::SealedLabels,
        )?;
        if digest_bytes(&labels)? != labels_content_digest {
            return Err(HoldoutAuthorityError::DigestMismatch {
                locator: verified.capability.labels_locator.clone(),
            });
        }
        let receipt = verified
            .capability
            .issue_receipt(
                run,
                seal,
                decision_owners,
                verified.capability_digest,
                verified.signed_by,
                verified.trust_root_id,
                now_unix,
            )
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        self.append_json_line(REVEAL_RECEIPTS_FILE, &receipt)?;
        let result = evaluate(&labels)?;
        Ok((result, receipt))
    }

    fn reveal_receipts(&self) -> Result<Vec<HoldoutAccessReceiptV1>, HoldoutAuthorityError> {
        self.read_json_lines(REVEAL_RECEIPTS_FILE)
    }

    fn verify_persisted_receipt(
        &self,
        receipt: &HoldoutAccessReceiptV1,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
    ) -> Result<(), HoldoutAuthorityError> {
        receipt
            .validate_for_run(run, seal, decision_owners)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if !self
            .reveal_receipts()?
            .iter()
            .any(|persisted| persisted == receipt)
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "holdout reveal receipt is absent from the durable authority log".to_string(),
            ));
        }
        let capability_locator = locator_for(
            HoldoutArtifactKindV1::RevealCapability,
            &receipt.capability_digest,
        )?;
        let capability = self.verify_reveal_capability(
            &capability_locator,
            run,
            seal,
            decision_owners,
            receipt.revealed_at_unix,
        )?;
        if capability.capability_digest != receipt.capability_digest
            || capability.signed_by != receipt.signed_by
            || capability.trust_root_id != receipt.trust_root_id
            || capability.capability.revealed_by != receipt.revealed_by
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "durable reveal receipt capability",
            });
        }
        let labels_content_digest = seal.labels_content_digest.clone().ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no labels content digest".to_string(),
            )
        })?;
        let label_authority = seal.label_authority.ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no label authority".to_string(),
            )
        })?;
        let envelope = self.verify_envelope(
            &seal.signature_locator,
            &HoldoutEnvelopeExpectationV1 {
                labels_locator: seal.locator.clone(),
                labels_content_digest: labels_content_digest.clone(),
                seal_digest: seal.seal_digest.clone(),
                label_authority,
                signed_envelope_digest: seal.signed_envelope_digest.clone(),
                decision_owners: decision_owners.to_vec(),
            },
            receipt.revealed_at_unix,
        )?;
        if envelope.signed_by != receipt.signed_by
            || envelope.trust_root_id != receipt.trust_root_id
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "durable reveal receipt envelope",
            });
        }
        let labels = self.resolve_immutable(&seal.locator, HoldoutArtifactKindV1::SealedLabels)?;
        if digest_bytes(&labels)? != labels_content_digest {
            return Err(HoldoutAuthorityError::DigestMismatch {
                locator: seal.locator.clone(),
            });
        }
        Ok(())
    }

    /// Promotion-authority validation is intentionally store-local: callers
    /// cannot inject an alternate receipt verifier.
    pub(crate) fn validate_accepted_pr9_evidence(
        &self,
        evidence: &AcceptedPr9CandidateEvidenceV1,
        run: &RunManifestV1,
        workload: &QueryWorkloadV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
    ) -> Result<(), EvaluationContractError> {
        evidence.validate_structure_for_run(run, workload, decision_owners)?;
        for batch in &evidence.evidence_batches {
            for receipt in &batch.holdout_receipts {
                self.verify_persisted_receipt(receipt, run, seal, decision_owners)
                    .map_err(|error| {
                        EvaluationContractError::HoldoutAccessViolation(format!(
                            "durable holdout receipt authority failed: {error}"
                        ))
                    })?;
            }
        }
        Ok(())
    }

    pub(crate) fn validate_semantic_candidate_evidence(
        &self,
        semantic: &SemanticCandidateEvidenceV1,
        accepted_pr9: &AcceptedPr9CandidateEvidenceV1,
        run: &RunManifestV1,
        workload: &QueryWorkloadV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
    ) -> Result<(), EvaluationContractError> {
        self.validate_accepted_pr9_evidence(accepted_pr9, run, workload, seal, decision_owners)?;
        semantic.validate_structure_for_accepted_pr9(run, workload, decision_owners, accepted_pr9)
    }

    pub(crate) fn verify_agent_adjudication(
        &self,
        provenance: &AgentAdjudicatedLabelProvenanceV1,
        now_unix: u64,
    ) -> Result<(), HoldoutAuthorityError> {
        let packet_locator = locator_for(
            HoldoutArtifactKindV1::BlindedPacket,
            &provenance.blinded_packet_digest,
        )?;
        self.resolve_immutable(&packet_locator, HoldoutArtifactKindV1::BlindedPacket)?;

        let delegation_locator = locator_for(
            HoldoutArtifactKindV1::SignedDelegation,
            &provenance.signed_delegation_digest,
        )?;
        let delegation_bytes =
            self.resolve_immutable(&delegation_locator, HoldoutArtifactKindV1::SignedDelegation)?;
        let delegation: SignedAgentDelegationV1 = serde_json::from_slice(&delegation_bytes)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if delegation.schema_revision != 1
            || delegation.payload.schema_revision != 1
            || delegation.payload.delegated_by != provenance.delegated_by
            || delegation.payload.blinded_packet_digest != provenance.blinded_packet_digest
        {
            return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "signed agent delegation",
            });
        }
        let signature_payload = AgentDelegationSignaturePayload {
            domain: DELEGATION_SIGNATURE_DOMAIN,
            schema_revision: 1,
            payload: &delegation.payload,
            trust_root_id: &delegation.trust_root_id,
        };
        let payload_bytes = serde_json::to_vec(&signature_payload)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if is_local_owner_trust_root(&delegation.payload.delegated_by, &delegation.trust_root_id) {
            if delegation.signature_hex.is_some()
                || delegation.payload.signed_at_unix == 0
                || delegation.payload.signed_at_unix > now_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "local agent delegation metadata",
                });
            }
        } else {
            let root = self
                .resolve_trust_root(&delegation.trust_root_id, delegation.payload.signed_at_unix)?;
            if root.owner_id != delegation.payload.delegated_by
                || delegation.payload.signed_at_unix > now_unix
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "signed agent delegation owner or time",
                });
            }
            let signature_hex = delegation
                .signature_hex
                .as_deref()
                .ok_or(HoldoutAuthorityError::InvalidSignature)?;
            let signature_bytes: [u8; 64] = decode_hex_array(signature_hex)
                .map_err(|()| HoldoutAuthorityError::InvalidSignature)?;
            VerifyingKey::from_bytes(&root.public_key)
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?
                .verify_strict(&payload_bytes, &Signature::from_bytes(&signature_bytes))
                .map_err(|_| HoldoutAuthorityError::InvalidSignature)?;
        }

        for judgment in provenance
            .independent_judgments
            .iter()
            .chain(provenance.separate_adjudication.iter())
        {
            let artifact_locator = locator_for(
                HoldoutArtifactKindV1::AgentJudgment,
                &judgment.immutable_judgment_artifact_digest,
            )?;
            let artifact_bytes =
                self.resolve_immutable(&artifact_locator, HoldoutArtifactKindV1::AgentJudgment)?;
            let artifact: AgentJudgmentArtifactV1 = serde_json::from_slice(&artifact_bytes)
                .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
            if artifact.provenance(judgment.immutable_judgment_artifact_digest.clone()) != *judgment
            {
                return Err(HoldoutAuthorityError::EnvelopeBindingMismatch {
                    field: "immutable agent judgment artifact",
                });
            }
        }
        Ok(())
    }

    fn create_key_record(
        &self,
        spec: DecisionOwnerKeySpecV1,
        created_at_unix: u64,
    ) -> Result<(DecisionOwnerKeyRecordV1, TrustRootRecordV1), HoldoutAuthorityError> {
        let mut seed = [0_u8; 32];
        getrandom::getrandom(&mut seed).map_err(|_| HoldoutAuthorityError::Randomness)?;
        let signing_key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        let public_key = signing_key.verifying_key().to_bytes();
        let public_key_hex = hex::encode(public_key);
        let public_key_fingerprint = digest_bytes(&public_key)?;
        let secret_id = hex::encode(Sha256::digest(spec.trust_root_id.as_bytes()));
        let secret_path = self.key_path(&secret_id)?;
        create_private_file_new(&secret_path, signing_key.as_bytes())
            .map_err(|error| map_io("create decision-owner key", error))?;
        let key_record = DecisionOwnerKeyRecordV1 {
            schema_revision: 1,
            spec: spec.clone(),
            public_key_hex: public_key_hex.clone(),
            public_key_fingerprint: public_key_fingerprint.clone(),
            secret_id,
            created_at_unix,
        };
        let root = TrustRootRecordV1 {
            owner_id: spec.owner_id,
            trust_root_id: spec.trust_root_id,
            public_key_hex,
            public_key_fingerprint,
            not_before_unix: spec.not_before_unix,
            not_after_unix: spec.not_after_unix,
            rotation_epoch: spec.rotation_epoch,
            registered_at_unix: created_at_unix,
        };
        Ok((key_record, root))
    }

    fn load_signing_key(&self, trust_root_id: &str) -> Result<SigningKey, HoldoutAuthorityError> {
        let records = self
            .key_records()?
            .into_iter()
            .filter(|record| record.spec.trust_root_id == trust_root_id)
            .collect::<Vec<_>>();
        if records.len() != 1 {
            return Err(HoldoutAuthorityError::KeyUnavailable);
        }
        let path = self.key_path(&records[0].secret_id)?;
        let bytes = read_private_file(&path)?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| HoldoutAuthorityError::KeyUnavailable)?;
        Ok(SigningKey::from_bytes(&seed))
    }

    fn registry_records(&self) -> Result<Vec<HoldoutRegistryRecordV1>, HoldoutAuthorityError> {
        self.read_json_lines(REGISTRY_FILE)
    }

    fn key_records(&self) -> Result<Vec<DecisionOwnerKeyRecordV1>, HoldoutAuthorityError> {
        self.read_json_lines(KEY_REGISTRY_FILE)
    }

    fn trust_events(&self) -> Result<Vec<TrustEventV1>, HoldoutAuthorityError> {
        self.read_json_lines(TRUST_EVENTS_FILE)
    }

    fn trust_roots(&self) -> Result<BTreeMap<String, TrustRootRecordV1>, HoldoutAuthorityError> {
        let mut roots = BTreeMap::new();
        for event in self.trust_events()? {
            let root = match event {
                TrustEventV1::Registered { root, .. } | TrustEventV1::Rotated { root, .. } => root,
                TrustEventV1::Revoked { .. } => continue,
            };
            match roots.get(&root.trust_root_id) {
                Some(existing) if existing != &root => {
                    return Err(HoldoutAuthorityError::RegistryConflict);
                }
                Some(_) => {}
                None => {
                    roots.insert(root.trust_root_id.clone(), root);
                }
            }
        }
        Ok(roots)
    }

    fn retired_roots(&self, at_unix: u64) -> Result<BTreeSet<String>, HoldoutAuthorityError> {
        Ok(self
            .trust_events()?
            .into_iter()
            .filter_map(|event| match event {
                TrustEventV1::Rotated {
                    previous_trust_root_id,
                    rotated_at_unix,
                    ..
                } if rotated_at_unix <= at_unix => Some(previous_trust_root_id),
                _ => None,
            })
            .collect())
    }

    fn revocations(&self) -> Result<BTreeSet<String>, HoldoutAuthorityError> {
        Ok(self
            .trust_events()?
            .into_iter()
            .filter_map(|event| match event {
                TrustEventV1::Revoked { trust_root_id, .. } => Some(trust_root_id),
                _ => None,
            })
            .collect())
    }

    fn append_json_line<T: Serialize>(
        &self,
        filename: &str,
        value: &T,
    ) -> Result<(), HoldoutAuthorityError> {
        let line = serde_json::to_string(value)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        let path = self.root.join(filename);
        PrivateStoreIo::append_line(&path, &line)
            .map_err(|error| map_io("append private metadata", error))?;
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|error| map_io("sync private metadata", error))?;
        sync_directory(&self.root).map_err(|error| map_io("sync authority root", error))
    }

    fn read_json_lines<T>(&self, filename: &str) -> Result<Vec<T>, HoldoutAuthorityError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let path = self.root.join(filename);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = read_private_file(&path)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))
            })
            .collect()
    }

    fn object_path(
        &self,
        kind: HoldoutArtifactKindV1,
        digest: &FixtureContentDigest,
    ) -> Result<PathBuf, HoldoutAuthorityError> {
        let encoded = digest_hex(digest)?;
        Ok(self
            .root
            .join("objects")
            .join(kind.as_str())
            .join(format!("{encoded}.bin")))
    }

    fn key_path(&self, secret_id: &str) -> Result<PathBuf, HoldoutAuthorityError> {
        if !is_lower_hex(secret_id, 64) {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "invalid decision-owner secret identity".to_string(),
            ));
        }
        Ok(self.root.join("keys").join(format!("{secret_id}.seed")))
    }
}

fn validate_key_spec(spec: &DecisionOwnerKeySpecV1) -> Result<(), HoldoutAuthorityError> {
    if !is_canonical_id(&spec.trust_root_id)
        || spec.not_before_unix > spec.not_after_unix
        || spec.rotation_epoch == 0
    {
        return Err(HoldoutAuthorityError::InvalidMetadata(
            "invalid decision-owner key specification".to_string(),
        ));
    }
    Ok(())
}

fn validate_envelope_payload(
    payload: &HoldoutEnvelopePayloadV1,
) -> Result<(), HoldoutAuthorityError> {
    if payload.schema_revision != 1
        || !matches!(
            parse_locator(&payload.labels_locator),
            Ok((HoldoutArtifactKindV1::SealedLabels, _))
        )
        || !is_canonical_id(&payload.trust_root_id)
    {
        return Err(HoldoutAuthorityError::InvalidMetadata(
            "invalid signed holdout envelope payload".to_string(),
        ));
    }
    Ok(())
}

fn key_receipt(root: &TrustRootRecordV1) -> DecisionOwnerKeyReceiptV1 {
    DecisionOwnerKeyReceiptV1 {
        owner_id: root.owner_id.clone(),
        trust_root_id: root.trust_root_id.clone(),
        public_key_fingerprint: root.public_key_fingerprint.clone(),
        not_before_unix: root.not_before_unix,
        not_after_unix: root.not_after_unix,
        rotation_epoch: root.rotation_epoch,
    }
}

fn locator_for(
    kind: HoldoutArtifactKindV1,
    digest: &FixtureContentDigest,
) -> Result<String, HoldoutAuthorityError> {
    Ok(format!(
        "{LOCATOR_PREFIX}{}/{}",
        kind.as_str(),
        digest_hex(digest)?
    ))
}

fn parse_locator(
    locator: &str,
) -> Result<(HoldoutArtifactKindV1, FixtureContentDigest), HoldoutAuthorityError> {
    let remainder = locator
        .strip_prefix(LOCATOR_PREFIX)
        .ok_or(HoldoutAuthorityError::InvalidLocator)?;
    let (kind, digest) = remainder
        .split_once('/')
        .ok_or(HoldoutAuthorityError::InvalidLocator)?;
    if digest.contains('/') || !is_lower_hex(digest, 64) {
        return Err(HoldoutAuthorityError::InvalidLocator);
    }
    let kind = HoldoutArtifactKindV1::parse(kind).ok_or(HoldoutAuthorityError::InvalidLocator)?;
    let digest = FixtureContentDigest::new(format!("sha256:{digest}"))
        .map_err(|_| HoldoutAuthorityError::InvalidLocator)?;
    Ok((kind, digest))
}

fn digest_hex(digest: &FixtureContentDigest) -> Result<&str, HoldoutAuthorityError> {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .filter(|value| is_lower_hex(value, 64))
        .ok_or(HoldoutAuthorityError::InvalidLocator)
}

fn digest_bytes(bytes: &[u8]) -> Result<FixtureContentDigest, HoldoutAuthorityError> {
    FixtureContentDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
        .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))
}

fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N], ()> {
    let bytes = hex::decode(value).map_err(|_| ())?;
    bytes.try_into().map_err(|_| ())
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_canonical_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn local_owner_trust_root_id(owner: &DecisionOwnerId) -> String {
    let digest = hex::encode(Sha256::digest(owner.as_str().as_bytes()));
    format!("local-owner-{}", &digest[..24])
}

fn is_local_owner_trust_root(owner: &DecisionOwnerId, trust_root_id: &str) -> bool {
    trust_root_id == local_owner_trust_root_id(owner)
}

fn create_private_file_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing private parent"))?;
    PrivateStoreIo::create_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    set_private_file_permissions(path)?;
    sync_directory(parent)
}

fn read_private_file(path: &Path) -> Result<Vec<u8>, HoldoutAuthorityError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| map_io("inspect private file", error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(HoldoutAuthorityError::UnsafeStorage);
    }
    verify_private_file_mode(&metadata)?;
    if metadata.len() > MAX_PRIVATE_OBJECT_BYTES {
        return Err(HoldoutAuthorityError::ObjectTooLarge);
    }
    let capacity =
        usize::try_from(metadata.len()).map_err(|_| HoldoutAuthorityError::ObjectTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| map_io("read private file", error))?;
    Ok(bytes)
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_private_file_mode(metadata: &fs::Metadata) -> Result<(), HoldoutAuthorityError> {
    if metadata.permissions().mode() & 0o077 != 0 {
        Err(HoldoutAuthorityError::UnsafeStorage)
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn verify_private_file_mode(_metadata: &fs::Metadata) -> Result<(), HoldoutAuthorityError> {
    Ok(())
}

#[cfg(unix)]
fn verify_private_directory(path: &Path) -> Result<(), HoldoutAuthorityError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| map_io("inspect private directory", error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        Err(HoldoutAuthorityError::UnsafeStorage)
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn verify_private_directory(path: &Path) -> Result<(), HoldoutAuthorityError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| map_io("inspect private directory", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        Err(HoldoutAuthorityError::UnsafeStorage)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn map_io(operation: &'static str, error: io::Error) -> HoldoutAuthorityError {
    if error.kind() == io::ErrorKind::InvalidInput {
        HoldoutAuthorityError::UnsafeStorage
    } else {
        HoldoutAuthorityError::Storage {
            operation,
            kind: error.kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::TempDir;
    use tracedecay_domain::{
        AcceptedPr9CandidateEvidenceDigest, AcceptedPr9CandidateEvidenceV1,
        AgentAdjudicatedLabelProvenanceV1, AgentAdjudicationStateV1, CandidateListV1,
        CorpusDocumentId, DecisionOwnerId, DecisionRecordDigest, DecisionRecordV1, EvalOutcomeV1,
        EvalPartitionV1, EvalQueryId, EvalQueryV1, EvalRunScopeV1, EvaluationRunBudgetV1,
        EvidenceBatchDigest, EvidenceBatchId, EvidenceBatchV1, FixtureFileDigestV1,
        FixtureManifestDigest, HoldoutAccessPolicyV1, LabelEvidenceRoleV1, QueryFamilyV1,
        QueryWorkloadV1, RelevanceGradeV1, RelevanceJudgmentV1, RetrieverLaneId, RunId,
        RunManifestDigest, SavedCandidateSetDigest, SavedCandidateSetV1,
        SemanticCandidateEvidenceDigest, SemanticCandidateEvidenceV1, SnapshotId, WorkloadDigest,
    };

    use super::*;

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    struct LockedFixture {
        profile: TempDir,
        store: HoldoutAuthorityStoreV1,
        run: RunManifestV1,
        workload: QueryWorkloadV1,
        seal: HoldoutSealV1,
        capability: HoldoutRegistryRecordV1,
        labels: Vec<u8>,
    }

    fn locked_fixture() -> LockedFixture {
        let profile = tempfile::tempdir().unwrap();
        let store = HoldoutAuthorityStoreV1::open_at(profile.path()).unwrap();
        let owner = DecisionOwnerId::new("owner-search-quality-lead").unwrap();
        store
            .generate_decision_owner_key(
                DecisionOwnerKeySpecV1 {
                    owner_id: owner.clone(),
                    trust_root_id: "root-v1".to_string(),
                    not_before_unix: 100,
                    not_after_unix: 10_000,
                    rotation_epoch: 1,
                },
                100,
            )
            .unwrap();
        let labels = br#"{"query_id":"q-hold-001","grade":"relevant"}"#.to_vec();
        let labels_record = store.import_sealed_labels(&labels, 110).unwrap();
        let seal_digest = HoldoutSealDigest::new(ZERO_DIGEST).unwrap();
        let envelope = store
            .sign_envelope(HoldoutEnvelopePayloadV1 {
                schema_revision: 1,
                labels_locator: labels_record.locator.clone(),
                labels_content_digest: labels_record.content_digest.clone(),
                seal_digest: seal_digest.clone(),
                label_authority: HoldoutLabelAuthorityV1::HumanAuthoritative,
                signed_by: owner.clone(),
                trust_root_id: "root-v1".to_string(),
                signed_at_unix: 120,
            })
            .unwrap();
        let seal = HoldoutSealV1 {
            locator: labels_record.locator.clone(),
            seal_digest: seal_digest.clone(),
            labels_content_digest: Some(labels_record.content_digest),
            label_authority: Some(HoldoutLabelAuthorityV1::HumanAuthoritative),
            signed_envelope_digest: envelope.content_digest,
            signature_locator: envelope.locator,
            access_policy: HoldoutAccessPolicyV1::SealedRevealRequiresReceipt,
            reveal_contract: "signed, run-bound reveal with durable receipt".to_string(),
            schema_revision: 1,
        };
        let query = |query_id: &str, partition| EvalQueryV1 {
            query_id: EvalQueryId::new(query_id).unwrap(),
            partition,
            family: QueryFamilyV1::ExactSymbol,
            provider: "cursor".to_string(),
            language: "rust".to_string(),
            repository_family_cluster: "tracedecay".to_string(),
            snapshot_id: SnapshotId::new("snapshot-v1").unwrap(),
            snapshot_commit: "eda50f53000ab4f96ef30e1f3a46b748b3fea6e0".to_string(),
            as_of_unix_micros: 1,
            principal_class: "fixture".to_string(),
            privacy_domain_class: "fixture".to_string(),
            allowed_scope_ids: vec!["scope.fixture".to_string()],
            query_text: "UtcMicros".to_string(),
            authorized_private_query_locator_digest: None,
            contamination_groups: Vec::new(),
            forbidden_document_ids: Vec::new(),
        };
        let mut workload = QueryWorkloadV1 {
            revision: 1,
            queries: vec![
                query("q-dev-001", EvalPartitionV1::Development),
                query("q-hold-001", EvalPartitionV1::SealedHoldout),
            ],
            digest: WorkloadDigest::new(ZERO_DIGEST).unwrap(),
        };
        workload.digest = workload.compute_digest().unwrap();
        let mut run = RunManifestV1 {
            run_id: RunId::new("run-locked-v1").unwrap(),
            revision: 1,
            scope: EvalRunScopeV1::Locked,
            authority: FixtureAuthorityV1::LockedQuality,
            fixture_manifest_digest: FixtureManifestDigest::new(ZERO_DIGEST).unwrap(),
            workload_file_digest: FixtureContentDigest::new(ZERO_DIGEST).unwrap(),
            development_label_file_digest: FixtureContentDigest::new(ZERO_DIGEST).unwrap(),
            holdout_seal_digest: seal_digest,
            artifact_files: vec![FixtureFileDigestV1 {
                path: "queries-v1.jsonl".to_string(),
                byte_len: 1,
                digest: FixtureContentDigest::new(ZERO_DIGEST).unwrap(),
            }],
            candidate_revision: "pr9-exact-lexical-graph-v1".to_string(),
            profile_matrix: vec!["candidate".to_string()],
            model_revision: "none".to_string(),
            tokenizer_revision: "tokenizer-v1".to_string(),
            runtime_revision: "runtime-v1".to_string(),
            command_revision: "command-v1".to_string(),
            budget: EvaluationRunBudgetV1 {
                candidate_limit_per_lane: 10,
                context_byte_limit: 4096,
                context_token_limit: 1024,
                deadline_millis: 1_000,
            },
            cache_states: vec!["cold".to_string()],
            execution_order: vec![EvalQueryId::new("q-hold-001").unwrap()],
            sample_size_rationale: "fixture".to_string(),
            measurement_tools: vec!["fixture".to_string()],
            statistical_procedures: vec!["fixture".to_string()],
            output_schema: "fixture-v1".to_string(),
            locked_outcomes_accessed: false,
            decision_expression: "fixture".to_string(),
            decision_owners: vec![owner.clone()],
            digest: RunManifestDigest::new(ZERO_DIGEST).unwrap(),
        };
        run.digest = run.compute_digest().unwrap();
        let capability = store
            .sign_reveal_capability(
                HoldoutRevealCapabilityV1 {
                    schema_revision: 1,
                    labels_locator: seal.locator.clone(),
                    envelope_locator: seal.signature_locator.clone(),
                    seal_digest: seal.seal_digest.clone(),
                    run_id: run.run_id.clone(),
                    run_manifest_digest: run.digest.clone(),
                    revealed_by: owner,
                    operation: "evaluate_locked_quality_v1".to_string(),
                    not_before_unix: 130,
                    expires_at_unix: 200,
                },
                "root-v1",
                125,
            )
            .unwrap();
        LockedFixture {
            profile,
            store,
            run,
            workload,
            seal,
            capability,
            labels,
        }
    }

    #[test]
    fn agent_adjudication_resolves_signed_private_artifacts() {
        let fixture = locked_fixture();
        let now_unix = 150;
        let packet = fixture
            .store
            .import_blinded_packet(b"{\"schema_revision\":1,\"opaque_queries\":[]}", now_unix)
            .unwrap();
        let delegation = fixture
            .store
            .sign_agent_delegation(
                AgentDelegationPayloadV1 {
                    schema_revision: 1,
                    delegated_by: fixture.run.decision_owners[0].clone(),
                    blinded_packet_digest: packet.content_digest.clone(),
                    signed_at_unix: now_unix,
                },
                "",
            )
            .unwrap();
        let judgments = vec![RelevanceJudgmentV1 {
            judgment_id: JudgmentId::new("sealed-judgment-v1").unwrap(),
            query_id: fixture.run.execution_order[0].clone(),
            document_id: CorpusDocumentId::new("doc-private-v1").unwrap(),
            symbol: None,
            grade: RelevanceGradeV1::Relevant,
            evidence_role: LabelEvidenceRoleV1::Primary,
            valid_from_unix_micros: 1,
            valid_until_unix_micros: None,
            supersedes_judgment_id: None,
            logical_copy_group: None,
            forbidden_anchor_ids: Vec::new(),
            abstention_oracle: false,
            task_oracle: None,
            labeler: "sol".to_string(),
            labeler_provenance: "blinded-agent-judgment".to_string(),
            adjudication: "independent".to_string(),
            correction_revision: 0,
            note: None,
        }];
        let label_set_digest = super::super::sealed_holdout_label_set_digest(&judgments).unwrap();
        let import = |id: &str, instance: &str| {
            let artifact = AgentJudgmentArtifactV1 {
                schema_revision: 1,
                independent_judgment_id: JudgmentId::new(id).unwrap(),
                adjudicator_instance_id: instance.to_string(),
                adjudicator_model: "sol".to_string(),
                adjudicator_version: "gpt-5.6-sol".to_string(),
                judged_at_unix: now_unix,
                blinded_packet_digest: packet.content_digest.clone(),
                label_set_digest: label_set_digest.clone(),
                judgments: judgments.clone(),
            };
            let record = fixture.store.import_agent_judgment(&artifact).unwrap();
            artifact.provenance(record.content_digest)
        };
        let provenance = AgentAdjudicatedLabelProvenanceV1 {
            delegated_by: fixture.run.decision_owners[0].clone(),
            blinded_packet_digest: packet.content_digest.clone(),
            signed_delegation_digest: delegation.content_digest.clone(),
            final_label_set_digest: Some(label_set_digest.clone()),
            state: AgentAdjudicationStateV1::Agreement,
            independent_judgments: vec![
                import("judgment-private-a", "sol-instance-a"),
                import("judgment-private-b", "sol-instance-b"),
            ],
            separate_adjudication: None,
        };
        fixture
            .store
            .verify_agent_adjudication(&provenance, now_unix)
            .unwrap();

        let mut mismatched = provenance;
        mismatched.independent_judgments[0].blinded_packet_digest =
            delegation.content_digest.clone();
        assert!(
            fixture
                .store
                .verify_agent_adjudication(&mismatched, now_unix)
                .is_err()
        );
    }

    fn accepted_evidence(
        fixture: &LockedFixture,
        receipt: HoldoutAccessReceiptV1,
    ) -> AcceptedPr9CandidateEvidenceV1 {
        let candidate_lists = ["exact", "lexical", "graph"]
            .into_iter()
            .map(|lane| CandidateListV1 {
                query_id: fixture.run.execution_order[0].clone(),
                lane: RetrieverLaneId::new(lane).unwrap(),
                candidates: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut saved_candidates = SavedCandidateSetV1 {
            schema_revision: 1,
            run_id: fixture.run.run_id.clone(),
            run_manifest_digest: fixture.run.digest.clone(),
            scope: EvalRunScopeV1::Locked,
            workload_digest: fixture.workload.digest.clone(),
            candidate_lists: candidate_lists.clone(),
            digest: SavedCandidateSetDigest::new(ZERO_DIGEST).unwrap(),
        };
        saved_candidates.digest = saved_candidates.compute_digest().unwrap();
        let mut batch = EvidenceBatchV1 {
            batch_id: EvidenceBatchId::new("batch-locked-v1").unwrap(),
            run_id: fixture.run.run_id.clone(),
            scope: EvalRunScopeV1::Locked,
            workload_digest: fixture.workload.digest.clone(),
            candidate_lists,
            holdout_receipts: vec![receipt],
            digest: EvidenceBatchDigest::new(ZERO_DIGEST).unwrap(),
        };
        batch.digest = batch.compute_digest().unwrap();
        let mut decision = DecisionRecordV1 {
            run_id: fixture.run.run_id.clone(),
            outcome: EvalOutcomeV1::Accepted,
            rationale: "persisted receipt-backed acceptance".to_string(),
            decided_by: fixture.run.decision_owners[0].clone(),
            saved_candidate_set_digest: Some(saved_candidates.digest.clone()),
            evidence_batches: vec![batch.digest.clone()],
            digest: DecisionRecordDigest::new(ZERO_DIGEST).unwrap(),
        };
        decision.digest = decision.compute_digest().unwrap();
        let mut accepted = AcceptedPr9CandidateEvidenceV1 {
            schema_revision: 1,
            decision,
            evidence_batches: vec![batch],
            saved_candidates,
            digest: AcceptedPr9CandidateEvidenceDigest::new(ZERO_DIGEST).unwrap(),
        };
        accepted.digest = accepted.compute_digest().unwrap();
        accepted
    }

    fn rehash_receipt_evidence(evidence: &mut AcceptedPr9CandidateEvidenceV1) {
        let receipt = &mut evidence.evidence_batches[0].holdout_receipts[0];
        receipt.digest = receipt.compute_digest().unwrap();
        evidence.evidence_batches[0].digest =
            evidence.evidence_batches[0].compute_digest().unwrap();
        evidence.decision.evidence_batches[0] = evidence.evidence_batches[0].digest.clone();
        evidence.decision.digest = evidence.decision.compute_digest().unwrap();
        evidence.digest = evidence.compute_digest().unwrap();
    }

    #[test]
    fn gated_evaluation_appends_a_durable_fully_bound_receipt() {
        let fixture = locked_fixture();
        let (label_count, receipt) = fixture
            .store
            .evaluate_locked_labels(
                &fixture.capability.locator,
                &fixture.run,
                &fixture.seal,
                &fixture.run.decision_owners,
                150,
                |labels| Ok(labels.len()),
            )
            .unwrap();
        assert_eq!(label_count, fixture.labels.len());
        assert_eq!(receipt.capability_digest, fixture.capability.content_digest);
        assert_eq!(receipt.run_manifest_digest, fixture.run.digest);
        assert_eq!(receipt.seal_digest, fixture.seal.seal_digest);
        assert_eq!(receipt.signed_by, fixture.run.decision_owners[0]);
        assert_eq!(receipt.revealed_by, fixture.run.decision_owners[0]);
        assert_eq!(receipt.trust_root_id, "root-v1");
        assert_eq!(receipt.revealed_at_unix, 150);
        receipt
            .validate_for_run(&fixture.run, &fixture.seal, &fixture.run.decision_owners)
            .unwrap();
        assert_eq!(
            fixture.store.reveal_receipts().unwrap(),
            vec![receipt.clone()]
        );

        let reopened = HoldoutAuthorityStoreV1::open_at(fixture.profile.path()).unwrap();
        assert_eq!(reopened.reveal_receipts().unwrap(), vec![receipt]);
    }

    #[test]
    fn local_owner_authority_requires_no_standalone_signing_key() {
        let fixture = locked_fixture();
        let now_unix = 150;
        let owner = fixture.run.decision_owners[0].clone();
        let key_count = fixture.store.decision_owner_keys().unwrap().len();
        let envelope = fixture
            .store
            .sign_envelope(HoldoutEnvelopePayloadV1 {
                schema_revision: 1,
                labels_locator: fixture.seal.locator.clone(),
                labels_content_digest: fixture.seal.labels_content_digest.clone().unwrap(),
                seal_digest: fixture.seal.seal_digest.clone(),
                label_authority: fixture.seal.label_authority.unwrap(),
                signed_by: owner.clone(),
                trust_root_id: String::new(),
                signed_at_unix: now_unix,
            })
            .unwrap();
        let local_root = local_owner_trust_root_id(&owner);
        let seal = HoldoutSealV1 {
            signed_envelope_digest: envelope.content_digest,
            signature_locator: envelope.locator,
            ..fixture.seal.clone()
        };
        let mut run = fixture.run.clone();
        run.holdout_seal_digest = seal.seal_digest.clone();
        run.digest = run.compute_digest().unwrap();
        let capability = fixture
            .store
            .sign_reveal_capability(
                HoldoutRevealCapabilityV1 {
                    schema_revision: 1,
                    labels_locator: seal.locator.clone(),
                    envelope_locator: seal.signature_locator.clone(),
                    seal_digest: seal.seal_digest.clone(),
                    run_id: run.run_id.clone(),
                    run_manifest_digest: run.digest.clone(),
                    revealed_by: owner,
                    operation: "evaluate_locked_quality_v1".to_string(),
                    not_before_unix: now_unix - 1,
                    expires_at_unix: now_unix + 30,
                },
                "",
                now_unix,
            )
            .unwrap();
        let ((), receipt) = fixture
            .store
            .evaluate_locked_labels(
                &capability.locator,
                &run,
                &seal,
                &run.decision_owners,
                now_unix,
                |_| Ok(()),
            )
            .unwrap();

        assert_eq!(receipt.trust_root_id, local_root);
        assert_eq!(
            fixture.store.decision_owner_keys().unwrap().len(),
            key_count
        );
    }

    #[test]
    fn acceptance_requires_the_persisted_receipt_and_revalidates_its_authority() {
        let fixture = locked_fixture();
        let ((), receipt) = fixture
            .store
            .evaluate_locked_labels(
                &fixture.capability.locator,
                &fixture.run,
                &fixture.seal,
                &fixture.run.decision_owners,
                150,
                |_| Ok(()),
            )
            .unwrap();
        let accepted = accepted_evidence(&fixture, receipt);
        fixture
            .store
            .validate_accepted_pr9_evidence(
                &accepted,
                &fixture.run,
                &fixture.workload,
                &fixture.seal,
                &fixture.run.decision_owners,
            )
            .unwrap();

        let mut semantic_candidates = accepted.saved_candidates.clone();
        semantic_candidates
            .candidate_lists
            .retain(|list| list.lane.as_str() == "exact");
        semantic_candidates.candidate_lists[0].lane = RetrieverLaneId::new("semantic").unwrap();
        semantic_candidates.digest = semantic_candidates.compute_digest().unwrap();
        let mut semantic = SemanticCandidateEvidenceV1 {
            schema_revision: 1,
            accepted_pr9_evidence_digest: accepted.digest.clone(),
            saved_candidates: semantic_candidates,
            digest: SemanticCandidateEvidenceDigest::new(ZERO_DIGEST).unwrap(),
        };
        semantic.digest = semantic.compute_digest().unwrap();
        fixture
            .store
            .validate_semantic_candidate_evidence(
                &semantic,
                &accepted,
                &fixture.run,
                &fixture.workload,
                &fixture.seal,
                &fixture.run.decision_owners,
            )
            .unwrap();

        let missing_log = tempfile::tempdir().unwrap();
        let empty_store = HoldoutAuthorityStoreV1::open_at(missing_log.path()).unwrap();
        assert!(
            empty_store
                .validate_accepted_pr9_evidence(
                    &accepted,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .unwrap_err()
                .to_string()
                .contains("absent from the durable authority log")
        );

        let mut forged = accepted.clone();
        forged.evidence_batches[0].holdout_receipts[0].capability_digest =
            FixtureContentDigest::new(ZERO_DIGEST).unwrap();
        rehash_receipt_evidence(&mut forged);
        assert!(
            fixture
                .store
                .validate_accepted_pr9_evidence(
                    &forged,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .is_err()
        );

        let mut wrong_run = accepted.clone();
        wrong_run.evidence_batches[0].holdout_receipts[0].run_manifest_digest =
            RunManifestDigest::new(ZERO_DIGEST).unwrap();
        rehash_receipt_evidence(&mut wrong_run);
        assert!(
            fixture
                .store
                .validate_accepted_pr9_evidence(
                    &wrong_run,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .is_err()
        );

        let mut wrong_seal = accepted.clone();
        wrong_seal.evidence_batches[0].holdout_receipts[0].seal_digest = HoldoutSealDigest::new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap();
        rehash_receipt_evidence(&mut wrong_seal);
        assert!(
            fixture
                .store
                .validate_accepted_pr9_evidence(
                    &wrong_seal,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .is_err()
        );

        let mut wrong_signer = accepted.clone();
        wrong_signer.evidence_batches[0].holdout_receipts[0].signed_by =
            DecisionOwnerId::new("owner-forged").unwrap();
        rehash_receipt_evidence(&mut wrong_signer);
        assert!(
            fixture
                .store
                .validate_accepted_pr9_evidence(
                    &wrong_signer,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .is_err()
        );

        let labels_path = fixture
            .store
            .object_path(
                HoldoutArtifactKindV1::SealedLabels,
                fixture.seal.labels_content_digest.as_ref().unwrap(),
            )
            .unwrap();
        fs::write(labels_path, b"tampered labels").unwrap();
        assert!(
            fixture
                .store
                .validate_accepted_pr9_evidence(
                    &accepted,
                    &fixture.run,
                    &fixture.workload,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                )
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
    }

    #[test]
    fn unsigned_or_tampered_capability_fails_before_label_evaluation() {
        let fixture = locked_fixture();
        let raw_capability = HoldoutRevealCapabilityV1 {
            schema_revision: 1,
            labels_locator: fixture.seal.locator.clone(),
            envelope_locator: fixture.seal.signature_locator.clone(),
            seal_digest: fixture.seal.seal_digest.clone(),
            run_id: fixture.run.run_id.clone(),
            run_manifest_digest: fixture.run.digest.clone(),
            revealed_by: fixture.run.decision_owners[0].clone(),
            operation: "evaluate_locked_quality_v1".to_string(),
            not_before_unix: 130,
            expires_at_unix: 200,
        };
        let raw_bytes = serde_json::to_vec(&raw_capability).unwrap();
        let unsigned = fixture
            .store
            .write_immutable(HoldoutArtifactKindV1::RevealCapability, &raw_bytes, 130)
            .unwrap();
        let evaluated = Cell::new(false);
        assert!(
            fixture
                .store
                .evaluate_locked_labels(
                    &unsigned.locator,
                    &fixture.run,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                    150,
                    |_| {
                        evaluated.set(true);
                        Ok(())
                    },
                )
                .is_err()
        );
        assert!(!evaluated.get());
        assert!(fixture.store.reveal_receipts().unwrap().is_empty());

        let capability_path = fixture
            .store
            .object_path(
                HoldoutArtifactKindV1::RevealCapability,
                &fixture.capability.content_digest,
            )
            .unwrap();
        fs::write(capability_path, b"tampered").unwrap();
        assert!(
            fixture
                .store
                .evaluate_locked_labels(
                    &fixture.capability.locator,
                    &fixture.run,
                    &fixture.seal,
                    &fixture.run.decision_owners,
                    150,
                    |_| {
                        evaluated.set(true);
                        Ok(())
                    },
                )
                .is_err()
        );
        assert!(!evaluated.get());
        assert!(fixture.store.reveal_receipts().unwrap().is_empty());
    }

    #[test]
    fn capability_for_another_frozen_run_fails_before_envelope_open() {
        let fixture = locked_fixture();
        let envelope_path = fixture
            .store
            .object_path(
                HoldoutArtifactKindV1::SignedEnvelope,
                &fixture.seal.signed_envelope_digest,
            )
            .unwrap();
        fs::write(envelope_path, b"tampered envelope").unwrap();
        let mut other_run = fixture.run.clone();
        other_run.candidate_revision = "different-after-freeze".to_string();
        other_run.digest = other_run.compute_digest().unwrap();
        let evaluated = Cell::new(false);
        let error = fixture
            .store
            .evaluate_locked_labels(
                &fixture.capability.locator,
                &other_run,
                &fixture.seal,
                &fixture.run.decision_owners,
                150,
                |_| {
                    evaluated.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            HoldoutAuthorityError::EnvelopeBindingMismatch {
                field: "signed reveal capability"
            }
        ));
        assert!(!evaluated.get());
        assert!(fixture.store.reveal_receipts().unwrap().is_empty());
    }

    #[test]
    fn expired_capability_fails_before_envelope_open() {
        let fixture = locked_fixture();
        let envelope_path = fixture
            .store
            .object_path(
                HoldoutArtifactKindV1::SignedEnvelope,
                &fixture.seal.signed_envelope_digest,
            )
            .unwrap();
        fs::write(envelope_path, b"tampered envelope").unwrap();
        let evaluated = Cell::new(false);
        let error = fixture
            .store
            .evaluate_locked_labels(
                &fixture.capability.locator,
                &fixture.run,
                &fixture.seal,
                &fixture.run.decision_owners,
                201,
                |_| {
                    evaluated.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(matches!(error, HoldoutAuthorityError::InvalidMetadata(_)));
        assert!(!evaluated.get());
        assert!(fixture.store.reveal_receipts().unwrap().is_empty());
    }

    #[test]
    fn evaluator_failure_still_leaves_a_durable_reveal_receipt() {
        let fixture = locked_fixture();
        let error = fixture
            .store
            .evaluate_locked_labels(
                &fixture.capability.locator,
                &fixture.run,
                &fixture.seal,
                &fixture.run.decision_owners,
                150,
                |_| {
                    Err::<(), _>(HoldoutAuthorityError::InvalidMetadata(
                        "evaluator failed".to_string(),
                    ))
                },
            )
            .unwrap_err();
        assert!(matches!(error, HoldoutAuthorityError::InvalidMetadata(_)));
        let receipts = fixture.store.reveal_receipts().unwrap();
        assert_eq!(receipts.len(), 1);
        receipts[0]
            .validate_for_run(&fixture.run, &fixture.seal, &fixture.run.decision_owners)
            .unwrap();
    }
}
