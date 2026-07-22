//! Private holdout authority storage for locked search-quality evaluation.
//!
//! Sensitive label bytes live below the TraceDecay user profile. Public values
//! are content-addressed locators and SHA-256 digests. There is no signature,
//! reveal capability, trust root, attestation, or local anti-forgery keying
//! for PR7–PR10 holdout/owner acceptance.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AcceptedPr9CandidateEvidenceV1, AgentAdjudicatedLabelProvenanceV1, AgentJudgmentProvenanceV1,
    DecisionOwnerId, EvaluationContractError, FixtureContentDigest, HoldoutAccessReceiptV1,
    HoldoutSealDigest, HoldoutSealV1, JudgmentId, LabelSetDigest, QueryWorkloadV1,
    RelevanceJudgmentV1, RunManifestV1, SemanticCandidateEvidenceV1,
};

use crate::storage::{self, PrivateStoreIo};

const AUTHORITY_ROOT: &str = "search-quality-holdout-v1";
const REGISTRY_FILE: &str = "immutable-registry-v1.jsonl";
const ACCESS_RECEIPTS_FILE: &str = "access-receipts-v1.jsonl";
const LOCATOR_PREFIX: &str = "authorized-store://search-quality/holdout/v1/";
const MAX_PRIVATE_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutArtifactKindV1 {
    BlindedPacket,
    OwnerDelegation,
    AgentJudgment,
    SealedLabels,
}

impl HoldoutArtifactKindV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::BlindedPacket => "blinded-packet",
            Self::OwnerDelegation => "owner-delegation",
            Self::AgentJudgment => "agent-judgment",
            Self::SealedLabels => "sealed-labels",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "blinded-packet" => Some(Self::BlindedPacket),
            "owner-delegation" => Some(Self::OwnerDelegation),
            "agent-judgment" => Some(Self::AgentJudgment),
            "sealed-labels" => Some(Self::SealedLabels),
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
pub struct AgentDelegationPayloadV1 {
    pub schema_revision: u32,
    pub delegated_by: DecisionOwnerId,
    pub blinded_packet_digest: FixtureContentDigest,
    pub recorded_at_unix: u64,
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HoldoutAuthorityError {
    #[error("holdout authority storage failed during {operation}: {kind:?}")]
    Storage {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    #[error("holdout authority storage is not private")]
    UnsafeStorage,
    #[error("holdout object exceeds the private store size bound")]
    ObjectTooLarge,
    #[error("holdout locator is unknown")]
    UnknownLocator,
    #[error("holdout locator is malformed")]
    InvalidLocator,
    #[error("holdout artifact kind mismatch")]
    KindMismatch,
    #[error("holdout registry conflict")]
    RegistryConflict,
    #[error("holdout digest mismatch for {locator}")]
    DigestMismatch { locator: String },
    #[error("holdout metadata invalid: {0}")]
    InvalidMetadata(String),
    #[error("holdout access binding mismatch: {field}")]
    AccessBindingMismatch { field: &'static str },
    #[error(transparent)]
    Contract(#[from] EvaluationContractError),
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

    /// Provision sealed labels into the private profile store. Returns only
    /// opaque metadata; label bytes remain inaccessible except through
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
                "blinded packet must be non-empty".to_string(),
            ));
        }
        self.write_immutable(
            HoldoutArtifactKindV1::BlindedPacket,
            contents,
            created_at_unix,
        )
    }

    /// Persist one owner-authored delegation record by canonical content digest.
    pub fn import_owner_delegation(
        &self,
        payload: AgentDelegationPayloadV1,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if payload.schema_revision != 1 || payload.recorded_at_unix == 0 {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "owner delegation payload is malformed".to_string(),
            ));
        }
        let bytes = serde_json::to_vec(&payload).map_err(|error| {
            HoldoutAuthorityError::InvalidMetadata(format!("serialize owner delegation: {error}"))
        })?;
        self.write_immutable(
            HoldoutArtifactKindV1::OwnerDelegation,
            &bytes,
            payload.recorded_at_unix,
        )
    }

    pub fn import_agent_judgment(
        &self,
        artifact: &AgentJudgmentArtifactV1,
    ) -> Result<HoldoutRegistryRecordV1, HoldoutAuthorityError> {
        if artifact.schema_revision != 1
            || artifact.adjudicator_instance_id.trim().is_empty()
            || artifact.judgments.is_empty()
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "agent judgment artifact is malformed".to_string(),
            ));
        }
        let bytes = serde_json::to_vec(artifact).map_err(|error| {
            HoldoutAuthorityError::InvalidMetadata(format!("serialize agent judgment: {error}"))
        })?;
        self.write_immutable(
            HoldoutArtifactKindV1::AgentJudgment,
            &bytes,
            artifact.judged_at_unix,
        )
    }

    /// Sole label-reading operation: validate the frozen run and seal digests,
    /// open sealed labels, append a durable access receipt, then evaluate.
    pub(crate) fn evaluate_locked_labels<T>(
        &self,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
        accessed_by: &DecisionOwnerId,
        now_unix: u64,
        evaluate: impl FnOnce(&[u8]) -> Result<T, HoldoutAuthorityError>,
    ) -> Result<(T, HoldoutAccessReceiptV1), HoldoutAuthorityError> {
        seal.validate()?;
        let labels_content_digest = seal.labels_content_digest.clone().ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no labels content digest".to_string(),
            )
        })?;
        let _label_authority = seal.label_authority.ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no label authority".to_string(),
            )
        })?;
        if seal.locator.is_empty() {
            return Err(HoldoutAuthorityError::InvalidLocator);
        }
        let labels = self.resolve_immutable(&seal.locator, HoldoutArtifactKindV1::SealedLabels)?;
        if digest_bytes(&labels)? != labels_content_digest {
            return Err(HoldoutAuthorityError::DigestMismatch {
                locator: seal.locator.clone(),
            });
        }
        let seal_digest = HoldoutSealDigest::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(&labels))
        ))
        .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if seal_digest != seal.seal_digest {
            return Err(HoldoutAuthorityError::AccessBindingMismatch {
                field: "seal digest over sealed label bytes",
            });
        }
        let receipt = HoldoutAccessReceiptV1::issue_for_run(
            run,
            seal,
            decision_owners,
            accessed_by.clone(),
            now_unix,
        )?;
        self.append_json_line(ACCESS_RECEIPTS_FILE, &receipt)?;
        let result = evaluate(&labels)?;
        Ok((result, receipt))
    }

    fn access_receipts(&self) -> Result<Vec<HoldoutAccessReceiptV1>, HoldoutAuthorityError> {
        self.read_json_lines(ACCESS_RECEIPTS_FILE)
    }

    fn verify_persisted_receipt(
        &self,
        receipt: &HoldoutAccessReceiptV1,
        run: &RunManifestV1,
        seal: &HoldoutSealV1,
        decision_owners: &[DecisionOwnerId],
    ) -> Result<(), HoldoutAuthorityError> {
        receipt.validate_for_run(run, seal, decision_owners)?;
        if !self
            .access_receipts()?
            .iter()
            .any(|persisted| persisted == receipt)
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "holdout access receipt is absent from the durable authority log".to_string(),
            ));
        }
        let labels = self.resolve_immutable(&seal.locator, HoldoutArtifactKindV1::SealedLabels)?;
        let labels_content_digest = seal.labels_content_digest.clone().ok_or_else(|| {
            HoldoutAuthorityError::InvalidMetadata(
                "locked-quality seal has no labels content digest".to_string(),
            )
        })?;
        if digest_bytes(&labels)? != labels_content_digest
            || digest_bytes(&labels)? != receipt.labels_content_digest
        {
            return Err(HoldoutAuthorityError::DigestMismatch {
                locator: seal.locator.clone(),
            });
        }
        Ok(())
    }

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

    // Retained for PR10 semantic-holdout wiring; signature-only callers were
    // removed with the local anti-forgery/signing surface.
    #[allow(dead_code)]
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
        _now_unix: u64,
    ) -> Result<(), HoldoutAuthorityError> {
        provenance.validate()?;
        if !provenance.is_sealable()? {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "agent adjudication is not sealable".to_string(),
            ));
        }
        let _packet = self.resolve_immutable(
            &locator_for(
                HoldoutArtifactKindV1::BlindedPacket,
                &provenance.blinded_packet_digest,
            )?,
            HoldoutArtifactKindV1::BlindedPacket,
        )?;
        let _delegation = self.resolve_immutable(
            &locator_for(
                HoldoutArtifactKindV1::OwnerDelegation,
                &provenance.owner_delegation_digest,
            )?,
            HoldoutArtifactKindV1::OwnerDelegation,
        )?;
        let mut seen = BTreeSet::new();
        for judgment in &provenance.independent_judgments {
            if !seen.insert(judgment.immutable_judgment_artifact_digest.clone()) {
                return Err(HoldoutAuthorityError::InvalidMetadata(
                    "duplicate agent judgment artifact".to_string(),
                ));
            }
            let _bytes = self.resolve_immutable(
                &locator_for(
                    HoldoutArtifactKindV1::AgentJudgment,
                    &judgment.immutable_judgment_artifact_digest,
                )?,
                HoldoutArtifactKindV1::AgentJudgment,
            )?;
        }
        if let Some(adjudication) = &provenance.separate_adjudication {
            let _bytes = self.resolve_immutable(
                &locator_for(
                    HoldoutArtifactKindV1::AgentJudgment,
                    &adjudication.immutable_judgment_artifact_digest,
                )?,
                HoldoutArtifactKindV1::AgentJudgment,
            )?;
        }
        Ok(())
    }

    fn registry_records(&self) -> Result<Vec<HoldoutRegistryRecordV1>, HoldoutAuthorityError> {
        self.read_json_lines(REGISTRY_FILE)
    }

    fn append_json_line<T: Serialize>(
        &self,
        file_name: &str,
        value: &T,
    ) -> Result<(), HoldoutAuthorityError> {
        let path = self.root.join(file_name);
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&path)
            .map_err(|error| map_io("open append-only log", error))?;
        let mut line = serde_json::to_vec(value).map_err(|error| {
            HoldoutAuthorityError::InvalidMetadata(format!("serialize log record: {error}"))
        })?;
        line.push(b'\n');
        file.write_all(&line)
            .and_then(|_| file.sync_all())
            .map_err(|error| map_io("append authority log", error))?;
        set_private_file_permissions(&path).map_err(|error| map_io("chmod authority log", error))?;
        Ok(())
    }

    fn read_json_lines<T: for<'de> Deserialize<'de>>(
        &self,
        file_name: &str,
    ) -> Result<Vec<T>, HoldoutAuthorityError> {
        let path = self.root.join(file_name);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = read_private_file(&path)?;
        let mut out = Vec::new();
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            out.push(serde_json::from_slice(line).map_err(|error| {
                HoldoutAuthorityError::InvalidMetadata(format!(
                    "parse {file_name} line {}: {error}",
                    index + 1
                ))
            })?);
        }
        Ok(out)
    }

    fn object_path(
        &self,
        kind: HoldoutArtifactKindV1,
        digest: &FixtureContentDigest,
    ) -> Result<PathBuf, HoldoutAuthorityError> {
        Ok(self
            .root
            .join("objects")
            .join(kind.as_str())
            .join(digest_hex(digest)?))
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

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn sealed_labels_are_content_addressed_without_signing_machinery() {
        let profile = TempDir::new().unwrap();
        let store = HoldoutAuthorityStoreV1::open_at(profile.path()).unwrap();
        let bytes = br#"{"schema_revision":1,"label_authority":"deterministic","judgments":[]}"#;
        let record = store.import_sealed_labels(bytes, 100).unwrap();
        assert!(record.locator.contains("sealed-labels/"));
        assert_eq!(record.content_digest, digest_bytes(bytes).unwrap());
        let resolved = store
            .resolve_immutable(&record.locator, HoldoutArtifactKindV1::SealedLabels)
            .unwrap();
        assert_eq!(resolved, bytes);
        assert!(!store.root.join("keys").exists());
        assert!(!store.root.join("decision-owner-keys-v1.jsonl").exists());
        assert!(!store.root.join("trust-events-v1.jsonl").exists());
    }
}
