use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_application::{
    CancellationObservation, DirectorySyncPolicy, EffectId, EffectResult,
    SourceEditVerificationStateV1, SourceEditVerificationV1, sync_parent_directory,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::Result;

use super::JOURNAL_VERSION;
use super::digest::{load_record, persist_record, source_edit_recovery_digest};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::verify::{application_contract_error, config_error, domain_error, io_error};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(super) enum SourceEditJournalStateV1 {
    Prepared,
    Applied {
        outcome: SourceEditDurableOutcomeV1,
        committed_state: ManifestDigest,
        ended_at: UtcMicros,
        #[serde(default)]
        control_observation: Option<CancellationObservation>,
        #[serde(default)]
        verification_state: Option<SourceEditVerificationStateV1>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceEditJournalV1 {
    pub(super) version: u8,
    pub(super) effect_id: EffectId,
    pub(super) input_digest: ManifestDigest,
    pub(super) expected_state: ManifestDigest,
    #[serde(default)]
    pub(super) predicted_state: Option<ManifestDigest>,
    pub(super) candidate_files: Vec<String>,
    #[serde(default)]
    pub(super) recovery_files: Vec<crate::tracedecay::PlannedSourceEditFile>,
    #[serde(default)]
    pub(super) recovery_digest: Option<ManifestDigest>,
    pub(super) request: SourceEditDurableRequestV1,
    pub(super) state: SourceEditJournalStateV1,
}

impl SourceEditJournalV1 {
    fn validate_recovery(&self) -> Result<()> {
        match (&self.recovery_digest, self.recovery_files.is_empty()) {
            (None, true) => Ok(()),
            (Some(digest), false)
                if digest == &source_edit_recovery_digest(&self.recovery_files)? =>
            {
                Ok(())
            }
            _ => Err(config_error(
                "source edit recovery journal digest does not match its preimages",
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceEditDurableRequestV1 {
    pub(super) operation: tracedecay_tool_catalog::UseCaseId,
    pub(super) request_id: tracedecay_application::RequestId,
    pub(super) actor: tracedecay_domain::ActorId,
    pub(super) scope: tracedecay_application::ResolvedScope,
    pub(super) authority: tracedecay_application::AuthorityReceipt,
    pub(super) authority_proof: tracedecay_application::SourceEditEffectProofV1,
    pub(super) idempotency_key: tracedecay_application::IdempotencyKey,
    pub(super) deadline: tracedecay_application::Deadline,
    pub(super) started_at: UtcMicros,
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) verification_requested: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceEditDurableResultV1 {
    pub(super) version: u8,
    pub(super) input_digest: ManifestDigest,
    pub(super) authority_proof: tracedecay_application::SourceEditEffectProofV1,
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) predicted_state: Option<ManifestDigest>,
    pub(super) outcome: SourceEditDurableOutcomeV1,
    pub(super) effect: EffectResult<Value>,
}

pub(super) struct SourceEditDurability {
    pub(super) root: PathBuf,
}

pub(super) struct ResolvedSourceEditPreview {
    pub(super) outcome: SourceEditOutcome,
    pub(super) candidate_files: Vec<String>,
    pub(super) expected_state: Option<ManifestDigest>,
    pub(super) predicted_state: Option<ManifestDigest>,
    pub(super) planned_files: Vec<crate::tracedecay::PlannedSourceEditFile>,
}

impl SourceEditDurability {
    pub(super) fn for_graph(graph: &TraceDecay) -> Self {
        Self {
            root: graph
                .store_layout()
                .data_root
                .join("source-edit-transactions-v1"),
        }
    }

    pub(super) fn lock(&self) -> Result<crate::tracedecay::SyncLockGuard> {
        crate::tracedecay::try_acquire_sync_lock_at(&self.root.join("source-edit.lock"))
    }

    pub(super) fn journal_path(&self) -> PathBuf {
        self.root.join("active.json")
    }

    pub(super) fn receipt_path(
        &self,
        key: &tracedecay_application::IdempotencyKey,
    ) -> Result<PathBuf> {
        let digest = canonical_sha256(&("tracedecay.source-edit-receipt-key.v1", key.as_str()))
            .map_err(domain_error)?;
        Ok(self.root.join("receipts").join(format!(
            "{}.json",
            digest.as_str().trim_start_matches("sha256:")
        )))
    }

    fn reconciliation_receipt_path(
        &self,
        key: &tracedecay_application::IdempotencyKey,
    ) -> Result<PathBuf> {
        let digest = canonical_sha256(&(
            "tracedecay.source-edit-reconciliation-receipt-key.v1",
            key.as_str(),
        ))
        .map_err(domain_error)?;
        Ok(self.root.join("reconciliation-receipts").join(format!(
            "{}.json",
            digest.as_str().trim_start_matches("sha256:")
        )))
    }

    pub(super) fn load_journal(&self) -> Result<Option<SourceEditJournalV1>> {
        let journal =
            load_record::<SourceEditJournalV1>(&self.journal_path(), "source edit journal")?;
        if let Some(journal) = &journal {
            validate_durable_authority(
                &journal.request.authority,
                &journal.request.authority_proof,
            )?;
            journal.validate_recovery()?;
        }
        Ok(journal)
    }

    pub(super) fn persist_journal(&self, journal: &SourceEditJournalV1) -> Result<()> {
        persist_record(&self.journal_path(), "source-edit-journal", journal)
    }

    pub(super) fn clear_journal(&self) -> Result<()> {
        let path = self.journal_path();
        match fs::remove_file(&path) {
            Ok(()) => sync_parent_directory(&path, DirectorySyncPolicy::Strict)
                .map_err(|error| io_error("sync source edit journal removal", error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error("remove source edit journal", error)),
        }
    }

    pub(super) fn load_receipt(
        &self,
        key: &tracedecay_application::IdempotencyKey,
    ) -> Result<Option<SourceEditDurableResultV1>> {
        let receipt = load_record::<SourceEditDurableResultV1>(
            &self.receipt_path(key)?,
            "source edit receipt",
        )?;
        if receipt
            .as_ref()
            .is_some_and(|receipt| receipt.version != JOURNAL_VERSION)
        {
            return Err(config_error(
                "unsupported source edit durable receipt version",
            ));
        }
        if let Some(receipt) = &receipt {
            receipt.validate_authority()?;
        }
        Ok(receipt)
    }

    pub(super) fn persist_receipt(&self, receipt: &SourceEditDurableResultV1) -> Result<()> {
        persist_record(
            &self.receipt_path(&receipt.effect.idempotency_key)?,
            "source-edit-receipt",
            receipt,
        )
    }

    pub(super) fn load_reconciliation_receipt(
        &self,
        key: &tracedecay_application::IdempotencyKey,
    ) -> Result<Option<SourceEditDurableResultV1>> {
        let receipt = load_record::<SourceEditDurableResultV1>(
            &self.reconciliation_receipt_path(key)?,
            "source edit reconciliation receipt",
        )?;
        if receipt
            .as_ref()
            .is_some_and(|receipt| receipt.version != JOURNAL_VERSION)
        {
            return Err(config_error(
                "unsupported source edit reconciliation receipt version",
            ));
        }
        if let Some(receipt) = &receipt {
            receipt.validate_authority()?;
        }
        Ok(receipt)
    }

    pub(super) fn persist_reconciliation_receipt(
        &self,
        receipt: &SourceEditDurableResultV1,
    ) -> Result<()> {
        persist_record(
            &self.reconciliation_receipt_path(&receipt.effect.idempotency_key)?,
            "source-edit-reconciliation-receipt",
            receipt,
        )
    }
}

impl SourceEditDurableResultV1 {
    fn validate_authority(&self) -> Result<()> {
        validate_durable_authority(&self.effect.authority, &self.authority_proof)?;
        let receipt = &self.effect.receipt;
        if receipt.policy_digest != self.authority_proof.policy_digest
            || receipt.configuration_digest != self.authority_proof.configuration_digest
            || receipt.catalog_digest != self.authority_proof.catalog_digest
            || receipt.privacy_digest != self.authority_proof.privacy_digest
            || receipt.external_proof != self.authority_proof.external_proof
        {
            return Err(config_error(
                "source edit durable receipt authority proof is inconsistent",
            ));
        }
        Ok(())
    }

    pub(super) fn into_application_result(self, replayed: bool) -> SourceEditApplicationResult {
        SourceEditApplicationResult {
            outcome: SourceEditOutcome::DurableMetadata(self.outcome),
            dry_run: self.dry_run,
            expected_state: self.effect.expected_state.clone(),
            predicted_state: self.predicted_state,
            verification: None,
            effect: Some(self.effect),
            replayed,
        }
    }

    pub(super) fn into_live_application_result(
        self,
        outcome: SourceEditOutcome,
        verification: Option<SourceEditVerificationV1>,
    ) -> SourceEditApplicationResult {
        SourceEditApplicationResult {
            outcome,
            dry_run: self.dry_run,
            expected_state: self.effect.expected_state.clone(),
            predicted_state: self.predicted_state,
            verification,
            effect: Some(self.effect),
            replayed: false,
        }
    }
}

fn validate_durable_authority(
    authority: &tracedecay_application::AuthorityReceipt,
    proof: &tracedecay_application::SourceEditEffectProofV1,
) -> Result<()> {
    proof
        .validate_for(authority)
        .map_err(application_contract_error)
}

pub(super) fn same_source_edit_authority(
    left: &tracedecay_application::AuthorityReceipt,
    right: &tracedecay_application::AuthorityReceipt,
) -> bool {
    left.grant_id == right.grant_id
        && left.grant_revision == right.grant_revision
        && left.grant_digest == right.grant_digest
        && left.authorized_scope_digest == right.authorized_scope_digest
        && left.disclosure == right.disclosure
        && left.policy == right.policy
}
