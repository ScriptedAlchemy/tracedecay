//! Direct filesystem holdout-label loading for Plan 15 search-quality evaluation.
//!
//! Labels are ordinary local files. There is no private owner store, packet
//! import, delegation, judgment import, access-receipt log, reveal capability,
//! or signature/attestation ceremony. Tuning never opens holdout labels.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{FixtureContentDigest, HoldoutLabelAuthorityV1, HoldoutSealV1, RelevanceJudgmentV1};

#[derive(Debug, Error)]
pub enum HoldoutAuthorityError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{0}")]
    InvalidMetadata(String),
    #[error("digest mismatch: {0}")]
    DigestMismatch(String),
}

/// Direct-on-disk sealed holdout label set used by locked evaluation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectHoldoutLabelSetV1 {
    pub schema_revision: u32,
    pub label_authority: HoldoutLabelAuthorityV1,
    pub judgments: Vec<RelevanceJudgmentV1>,
}

impl DirectHoldoutLabelSetV1 {
    pub fn validate(&self) -> Result<(), HoldoutAuthorityError> {
        if self.schema_revision != 1 {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "holdout label schema_revision must be 1".to_owned(),
            ));
        }
        if self.judgments.is_empty() {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "holdout label set must contain judgments".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Load holdout labels from a direct filesystem path and optionally bind them
/// to the committed seal digests. Never writes owner receipts or imports.
pub fn load_direct_holdout_labels(
    path: &Path,
    seal: Option<&HoldoutSealV1>,
) -> Result<(DirectHoldoutLabelSetV1, FixtureContentDigest), HoldoutAuthorityError> {
    let bytes = fs::read(path).map_err(|source| HoldoutAuthorityError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let labels: DirectHoldoutLabelSetV1 =
        serde_json::from_slice(&bytes).map_err(|source| HoldoutAuthorityError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    labels.validate()?;
    let content_digest = digest_bytes(&bytes)?;
    if let Some(seal) = seal {
        seal.validate()
            .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))?;
        if let Some(expected) = &seal.labels_content_digest
            && expected != &content_digest
        {
            return Err(HoldoutAuthorityError::DigestMismatch(
                "holdout labels content digest does not match seal".to_owned(),
            ));
        }
        if let Some(authority) = seal.label_authority
            && authority != labels.label_authority
        {
            return Err(HoldoutAuthorityError::InvalidMetadata(
                "holdout label authority does not match seal".to_owned(),
            ));
        }
    }
    Ok((labels, content_digest))
}

fn digest_bytes(bytes: &[u8]) -> Result<FixtureContentDigest, HoldoutAuthorityError> {
    FixtureContentDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
        .map_err(|error| HoldoutAuthorityError::InvalidMetadata(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn agent_adjudicated_authority_is_rejected_without_owner_machinery() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("labels.json");
        let agent = serde_json::json!({
            "schema_revision": 1,
            "label_authority": "agent_adjudicated",
            "judgments": [{"judgment_id":"j","query_id":"q","document_id":"d","grade":"relevant","rationale":"x","annotator":"a"}]
        });
        fs::write(&path, serde_json::to_vec(&agent).unwrap()).unwrap();
        let err = load_direct_holdout_labels(&path, None).expect_err("agent rejected");
        assert!(
            matches!(err, HoldoutAuthorityError::Parse { .. }),
            "expected parse rejection for removed agent authority, got {err}"
        );
    }
}
