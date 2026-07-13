use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::id::{ManifestDigest, validate_canonical_string};

pub const EVIDENCE_CONSUMER_BINDING_SCHEMA_V1: &str = "research-evidence-consumer-binding/v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceReviewStateV1 {
    Reviewed,
    ReviewRequired,
    BlockedProvenance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceLedgerReviewV1 {
    pub ledger_digest: ManifestDigest,
    pub entries: BTreeMap<String, EvidenceReviewStateV1>,
}

impl EvidenceLedgerReviewV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.ledger_digest.validate()?;
        if self.entries.is_empty() {
            return Err(DomainError::Empty {
                field: "evidence ledger entries",
            });
        }
        for entry_id in self.entries.keys() {
            validate_canonical_string(entry_id, "evidence ledger entry id")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceConsumerBindingV1 {
    pub schema_version: String,
    pub consumer_id: String,
    pub evidence_ledger_digest: ManifestDigest,
    pub selected_entry_ids: Vec<String>,
}

impl EvidenceConsumerBindingV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != EVIDENCE_CONSUMER_BINDING_SCHEMA_V1 {
            return Err(DomainError::NonCanonical {
                field: "evidence binding schema version",
            });
        }
        validate_canonical_string(&self.consumer_id, "evidence consumer id")?;
        self.evidence_ledger_digest.validate()?;
        if self.selected_entry_ids.is_empty() {
            return Err(DomainError::Empty {
                field: "selected evidence entry ids",
            });
        }

        let mut seen = BTreeSet::new();
        let mut previous = None;
        for entry_id in &self.selected_entry_ids {
            validate_canonical_string(entry_id, "selected evidence entry id")?;
            if !seen.insert(entry_id) {
                return Err(DomainError::DuplicateId {
                    field: "selected evidence entry ids",
                });
            }
            if previous.is_some_and(|value: &String| value >= entry_id) {
                return Err(DomainError::NonCanonical {
                    field: "selected evidence entry ids",
                });
            }
            previous = Some(entry_id);
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        expected_consumer_id: &str,
        actual_ledger_digest: &ManifestDigest,
        reviewed: &EvidenceLedgerReviewV1,
    ) -> Result<(), DomainError> {
        self.validate()?;
        validate_canonical_string(expected_consumer_id, "expected evidence consumer id")?;
        reviewed.validate()?;
        if self.consumer_id != expected_consumer_id {
            return Err(DomainError::SnapshotMismatch {
                field: "evidence consumer id",
            });
        }
        if self.evidence_ledger_digest != *actual_ledger_digest
            || reviewed.ledger_digest != *actual_ledger_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "evidence ledger digest",
            });
        }
        for entry_id in &self.selected_entry_ids {
            match reviewed.entries.get(entry_id) {
                Some(EvidenceReviewStateV1::Reviewed) => {}
                Some(_) => {
                    return Err(DomainError::SnapshotMismatch {
                        field: "selected evidence review state",
                    });
                }
                None => {
                    return Err(DomainError::UnknownReference {
                        field: "selected evidence entry ids",
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn binding() -> EvidenceConsumerBindingV1 {
        EvidenceConsumerBindingV1 {
            schema_version: EVIDENCE_CONSUMER_BINDING_SCHEMA_V1.into(),
            consumer_id: "pr14a.native-semantic-benchmark".into(),
            evidence_ledger_digest: ManifestDigest::new(DIGEST).unwrap(),
            selected_entry_ids: vec!["entry-a".into(), "entry-b".into()],
        }
    }

    fn review() -> EvidenceLedgerReviewV1 {
        EvidenceLedgerReviewV1 {
            ledger_digest: ManifestDigest::new(DIGEST).unwrap(),
            entries: BTreeMap::from([
                ("entry-a".into(), EvidenceReviewStateV1::Reviewed),
                ("entry-b".into(), EvidenceReviewStateV1::Reviewed),
            ]),
        }
    }

    #[test]
    fn exact_reviewed_binding_passes() {
        binding()
            .validate_against(
                "pr14a.native-semantic-benchmark",
                &ManifestDigest::new(DIGEST).unwrap(),
                &review(),
            )
            .unwrap();
    }

    #[test]
    fn digest_drift_fails_closed() {
        let mut candidate = binding();
        candidate.evidence_ledger_digest =
            ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).unwrap();
        assert!(matches!(
            candidate.validate_against(
                "pr14a.native-semantic-benchmark",
                &ManifestDigest::new(DIGEST).unwrap(),
                &review()
            ),
            Err(DomainError::SnapshotMismatch { .. })
        ));

        let stale_actual = ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).unwrap();
        assert!(matches!(
            binding().validate_against("pr14a.native-semantic-benchmark", &stale_actual, &review()),
            Err(DomainError::SnapshotMismatch { .. })
        ));

        assert!(matches!(
            binding().validate_against(
                "pr36r.host-release-manifest",
                &ManifestDigest::new(DIGEST).unwrap(),
                &review()
            ),
            Err(DomainError::SnapshotMismatch {
                field: "evidence consumer id"
            })
        ));
    }

    #[test]
    fn unreviewed_or_missing_selected_entry_fails_closed() {
        let mut unreviewed = review();
        unreviewed
            .entries
            .insert("entry-b".into(), EvidenceReviewStateV1::ReviewRequired);
        let digest = ManifestDigest::new(DIGEST).unwrap();
        assert!(
            binding()
                .validate_against("pr14a.native-semantic-benchmark", &digest, &unreviewed)
                .is_err()
        );

        let mut missing = review();
        missing.entries.remove("entry-b");
        assert!(matches!(
            binding().validate_against("pr14a.native-semantic-benchmark", &digest, &missing),
            Err(DomainError::UnknownReference { .. })
        ));

        let mut blocked = review();
        blocked
            .entries
            .insert("entry-b".into(), EvidenceReviewStateV1::BlockedProvenance);
        assert!(
            binding()
                .validate_against("pr14a.native-semantic-benchmark", &digest, &blocked)
                .is_err()
        );
    }

    #[test]
    fn selection_requires_exact_schema_sorted_unique_rows() {
        let mut wrong_schema = binding();
        wrong_schema.schema_version = "research-evidence-consumer-binding/v0".into();
        assert!(matches!(
            wrong_schema.validate(),
            Err(DomainError::NonCanonical {
                field: "evidence binding schema version"
            })
        ));

        let mut unsorted = binding();
        unsorted.selected_entry_ids.reverse();
        assert!(matches!(
            unsorted.validate(),
            Err(DomainError::NonCanonical {
                field: "selected evidence entry ids"
            })
        ));

        let mut duplicate = binding();
        duplicate.selected_entry_ids = vec!["entry-a".into(), "entry-a".into()];
        assert!(matches!(
            duplicate.validate(),
            Err(DomainError::DuplicateId {
                field: "selected evidence entry ids"
            })
        ));
    }
}
