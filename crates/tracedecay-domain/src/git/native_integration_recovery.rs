//! Durable proof that resolves a native-integration inspection fence.

use serde::{Deserialize, Serialize};

use crate::{
    DomainError, GitOidV1, ManifestDigest, NativeIntegrationJournalV1,
    NativeIntegrationReceiptOutcomeV1, NativeIntegrationReceiptV1,
    NativeIntegrationRecoveryReceiptId, NativeIntegrationTransactionId, RepositoryId, UtcMicros,
    canonical_sha256,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationRecoveryOutcomeV1 {
    Committed,
    AbortedNoChange,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationRecoveryReceiptV1 {
    pub receipt_id: NativeIntegrationRecoveryReceiptId,
    pub transaction_id: NativeIntegrationTransactionId,
    pub repository_id: RepositoryId,
    pub inspection_receipt_digest: ManifestDigest,
    pub final_snapshot_digest: ManifestDigest,
    pub final_destination_tip: GitOidV1,
    pub final_destination_tree: GitOidV1,
    pub outcome: NativeIntegrationRecoveryOutcomeV1,
    pub recovered_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

#[derive(Serialize)]
struct RecoveryDigestMaterial<'a> {
    domain: &'static str,
    receipt_id: &'a NativeIntegrationRecoveryReceiptId,
    transaction_id: &'a NativeIntegrationTransactionId,
    repository_id: &'a RepositoryId,
    inspection_receipt_digest: &'a ManifestDigest,
    final_snapshot_digest: &'a ManifestDigest,
    final_destination_tip: &'a GitOidV1,
    final_destination_tree: &'a GitOidV1,
    outcome: NativeIntegrationRecoveryOutcomeV1,
    recovered_at: UtcMicros,
}

impl NativeIntegrationRecoveryReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receipt_id: NativeIntegrationRecoveryReceiptId,
        journal: &NativeIntegrationJournalV1,
        inspection_receipt: &NativeIntegrationReceiptV1,
        final_snapshot_digest: ManifestDigest,
        final_destination_tip: GitOidV1,
        final_destination_tree: GitOidV1,
        outcome: NativeIntegrationRecoveryOutcomeV1,
        recovered_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let mut receipt = Self {
            receipt_id,
            transaction_id: journal.transaction_id.clone(),
            repository_id: journal.repository_id.clone(),
            inspection_receipt_digest: inspection_receipt.receipt_digest.clone(),
            final_snapshot_digest,
            final_destination_tip,
            final_destination_tree,
            outcome,
            recovered_at,
            receipt_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
        };
        receipt.receipt_digest = receipt.compute_digest()?;
        receipt.validate_against(journal, inspection_receipt)?;
        Ok(receipt)
    }

    pub fn validate_against(
        &self,
        journal: &NativeIntegrationJournalV1,
        inspection_receipt: &NativeIntegrationReceiptV1,
    ) -> Result<(), DomainError> {
        journal.validate()?;
        self.validate_fields()?;
        if inspection_receipt.outcome != NativeIntegrationReceiptOutcomeV1::NeedsInspection
            || inspection_receipt.transaction_id != journal.transaction_id
            || inspection_receipt.repository_id != journal.repository_id
            || self.transaction_id != journal.transaction_id
            || self.repository_id != journal.repository_id
            || self.inspection_receipt_digest != inspection_receipt.receipt_digest
            || self.recovered_at <= inspection_receipt.committed_at
            || self.receipt_digest != self.compute_digest()?
        {
            return Err(noncanonical("native integration recovery binding"));
        }
        match self.outcome {
            NativeIntegrationRecoveryOutcomeV1::Committed
                if self.final_destination_tip == journal.expected_new_destination_tip
                    && self.final_destination_tree == journal.candidate_tree =>
            {
                Ok(())
            }
            NativeIntegrationRecoveryOutcomeV1::AbortedNoChange
                if !journal.ref_commit_observed
                    && self.final_snapshot_digest
                        == journal.expected_repository_snapshot_digest
                    && self.final_destination_tip == journal.expected_destination_tip
                    && self.final_destination_tree == journal.expected_destination_tree =>
            {
                Ok(())
            }
            NativeIntegrationRecoveryOutcomeV1::RolledBack
                if self.final_snapshot_digest == journal.expected_repository_snapshot_digest
                    && self.final_destination_tip == journal.expected_destination_tip
                    && self.final_destination_tree == journal.expected_destination_tree =>
            {
                Ok(())
            }
            _ => Err(noncanonical("native integration recovery outcome")),
        }
    }

    fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        canonical_sha256(&RecoveryDigestMaterial {
            domain: "tracedecay.native-integration.recovery-receipt.v1",
            receipt_id: &self.receipt_id,
            transaction_id: &self.transaction_id,
            repository_id: &self.repository_id,
            inspection_receipt_digest: &self.inspection_receipt_digest,
            final_snapshot_digest: &self.final_snapshot_digest,
            final_destination_tip: &self.final_destination_tip,
            final_destination_tree: &self.final_destination_tree,
            outcome: self.outcome,
            recovered_at: self.recovered_at,
        })
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.transaction_id.validate()?;
        self.repository_id.validate()?;
        self.inspection_receipt_digest.validate()?;
        self.final_snapshot_digest.validate()?;
        self.final_destination_tip.validate()?;
        self.final_destination_tree.validate()?;
        self.receipt_digest.validate()
    }
}

fn noncanonical(field: &'static str) -> DomainError {
    DomainError::NonCanonical { field }
}
