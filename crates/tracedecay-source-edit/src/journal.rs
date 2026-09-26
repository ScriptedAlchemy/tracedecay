use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_contracts::{
    CancellationObservation, EffectId, EffectResult, SourceEditVerificationStateV1,
    SourceEditVerificationV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_parent_directory};
use tracedecay_private_fs::{FileLease, LockAdmissionError, lock_until};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::JOURNAL_VERSION;
use super::digest::{load_record, persist_record, source_edit_recovery_digest};
use super::outcome::{SourceEditApplicationResult, SourceEditDurableOutcomeV1, SourceEditOutcome};
use super::plan::PlannedSourceEditFile;
use super::port::SourceEditRuntime;
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
    /// `None` only on in-memory pre-effect records; every persisted journal
    /// carries the exact previewed postimage digest.
    pub(super) predicted_state: Option<ManifestDigest>,
    pub(super) candidate_files: Vec<String>,
    #[serde(default)]
    pub(super) recovery_files: Vec<PlannedSourceEditFile>,
    #[serde(default)]
    pub(super) recovery_digest: Option<ManifestDigest>,
    pub(super) request: SourceEditDurableRequestV1,
    pub(super) state: SourceEditJournalStateV1,
}

impl SourceEditJournalV1 {
    fn validate_persisted(&self) -> Result<()> {
        if self.predicted_state.is_none() {
            return Err(config_error(
                "unsupported source edit journal: it carries no predicted state",
            ));
        }
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
    pub(super) request_id: tracedecay_contracts::RequestId,
    pub(super) actor: tracedecay_domain::ActorId,
    pub(super) scope: tracedecay_contracts::ResolvedScope,
    pub(super) authority: tracedecay_contracts::AuthorityReceipt,
    pub(super) authority_proof: tracedecay_contracts::SourceEditEffectProofV1,
    pub(super) idempotency_key: tracedecay_contracts::IdempotencyKey,
    pub(super) deadline: tracedecay_contracts::Deadline,
    pub(super) started_at: UtcMicros,
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) verification_requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceEditRollbackRecordV1 {
    pub(super) version: u8,
    pub(super) effect_id: EffectId,
    pub(super) input_digest: ManifestDigest,
    pub(super) idempotency_key: tracedecay_contracts::IdempotencyKey,
    pub(super) operation: tracedecay_tool_catalog::UseCaseId,
    pub(super) actor: tracedecay_domain::ActorId,
    pub(super) scope: tracedecay_contracts::ResolvedScope,
    pub(super) expected_state: ManifestDigest,
    pub(super) committed_state: ManifestDigest,
    pub(super) recovery_files: Vec<PlannedSourceEditFile>,
    pub(super) recovery_digest: ManifestDigest,
    pub(super) record_digest: ManifestDigest,
}

impl SourceEditRollbackRecordV1 {
    fn digest(&self) -> Result<ManifestDigest> {
        canonical_sha256(&(
            "tracedecay.source-edit-rollback-record.v1",
            self.version,
            &self.effect_id,
            &self.input_digest,
            &self.idempotency_key,
            &self.operation,
            &self.actor,
            &self.scope,
            &self.expected_state,
            &self.committed_state,
            &self.recovery_files,
            &self.recovery_digest,
        ))
        .map_err(domain_error)
    }

    fn validate(&self) -> Result<()> {
        if self.version != JOURNAL_VERSION
            || self.recovery_files.is_empty()
            || self.recovery_digest != source_edit_recovery_digest(&self.recovery_files)?
            || self.record_digest != self.digest()?
        {
            return Err(config_error(
                "source edit rollback record failed its private digest binding",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceEditDurableResultV1 {
    pub(super) version: u8,
    pub(super) input_digest: ManifestDigest,
    pub(super) authority_proof: tracedecay_contracts::SourceEditEffectProofV1,
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
    pub(super) planned_files: Vec<PlannedSourceEditFile>,
}

/// Same-project callers queue this long for the store before the edit fails
/// with a typed lock-deadline error.
const SOURCE_EDIT_ADMISSION_DEADLINE: Duration = Duration::from_secs(30);

/// Held for one edit: the in-process queue position and the cross-process
/// lock file, released together on drop.
pub(super) struct SourceEditLease {
    _file: FileLease,
    _queued: tokio::sync::OwnedMutexGuard<()>,
}

/// The daemon's single owner per source-edit store root.
// ponytail: entries are never evicted; one empty mutex per project store this
// process has edited, bounded by its registered projects.
fn source_edit_owner(root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static OWNERS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut owners = OWNERS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Arc::clone(owners.entry(root.to_path_buf()).or_default())
}

impl SourceEditDurability {
    pub(super) fn for_graph(graph: &SourceEditRuntime) -> Self {
        Self {
            root: graph
                .store_layout()
                .data_root
                .join("source-edit-transactions-v1"),
        }
    }

    /// Serializes every preview, apply, rollback, and reconciliation on this
    /// store. The daemon owns the store: same-project callers queue on its
    /// in-process owner, and the lock file fences any other process. Both
    /// waits share one admission deadline, past which the edit fails with a
    /// typed, retryable lock-deadline error.
    #[hotpath::measure(label = "usecases.edit.lock", future = true)]
    pub(super) async fn lock(&self) -> Result<SourceEditLease> {
        let deadline = Instant::now() + SOURCE_EDIT_ADMISSION_DEADLINE;
        let lock_path = self.root.join("source-edit.lock");
        let deadline_error = || TraceDecayError::LockDeadline {
            resource: "source-edit writer lock",
            deadline_ms: u64::try_from(SOURCE_EDIT_ADMISSION_DEADLINE.as_millis())
                .unwrap_or(u64::MAX),
        };
        let queued =
            tokio::time::timeout_at(deadline.into(), source_edit_owner(&self.root).lock_owned())
                .await
                .map_err(|_| deadline_error())?;
        fs::create_dir_all(&self.root)
            .map_err(|error| io_error("create source edit root", error))?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| io_error("open source edit lock", error))?;
        let file = tokio::task::spawn_blocking(move || lock_until(&file, deadline).map(|()| file))
            .await
            .map_err(|error| {
                config_error(format!("source edit lock admission task failed: {error}"))
            })?
            .map_err(|error| match error {
                LockAdmissionError::TimedOut => deadline_error(),
                LockAdmissionError::Io(error) => io_error("acquire source edit lock", error),
            })?;
        Ok(SourceEditLease {
            _file: FileLease::held(file, "source_edit.writer"),
            _queued: queued,
        })
    }

    pub(super) fn journal_path(&self) -> PathBuf {
        self.root.join("active.json")
    }

    pub(super) fn receipt_path(
        &self,
        key: &tracedecay_contracts::IdempotencyKey,
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
        key: &tracedecay_contracts::IdempotencyKey,
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

    fn rollback_record_path(&self, effect_id: &EffectId) -> Result<PathBuf> {
        let digest = canonical_sha256(&(
            "tracedecay.source-edit-rollback-record-key.v1",
            effect_id.as_str(),
        ))
        .map_err(domain_error)?;
        Ok(self.root.join("rollback-records").join(format!(
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
            journal.validate_persisted()?;
        }
        Ok(journal)
    }

    pub(super) fn persist_journal(&self, journal: &SourceEditJournalV1) -> Result<()> {
        persist_record(&self.journal_path(), "source-edit-journal", journal)
    }

    pub(super) fn load_rollback_record(
        &self,
        effect_id: &EffectId,
    ) -> Result<Option<SourceEditRollbackRecordV1>> {
        let record = load_record::<SourceEditRollbackRecordV1>(
            &self.rollback_record_path(effect_id)?,
            "source edit rollback record",
        )?;
        if let Some(record) = &record {
            record.validate()?;
            if &record.effect_id != effect_id {
                return Err(config_error(
                    "source edit rollback record effect identity does not match its key",
                ));
            }
        }
        Ok(record)
    }

    pub(super) fn persist_rollback_record(
        &self,
        journal: &SourceEditJournalV1,
        committed_state: &ManifestDigest,
        succeeded: bool,
    ) -> Result<()> {
        let move_operation = tracedecay_contracts::source_edit_operation(
            tracedecay_contracts::SourceEditKind::MoveSymbol,
        )
        .map_err(application_contract_error)?;
        if &journal.request.operation != move_operation.use_case_id()
            || journal.recovery_files.is_empty()
        {
            return Ok(());
        }
        if !succeeded || journal.predicted_state.as_ref() != Some(committed_state) {
            return Ok(());
        }
        let recovery_digest = source_edit_recovery_digest(&journal.recovery_files)?;
        let mut record = SourceEditRollbackRecordV1 {
            version: JOURNAL_VERSION,
            effect_id: journal.effect_id.clone(),
            input_digest: journal.input_digest.clone(),
            idempotency_key: journal.request.idempotency_key.clone(),
            operation: journal.request.operation.clone(),
            actor: journal.request.actor.clone(),
            scope: journal.request.scope.clone(),
            expected_state: journal.expected_state.clone(),
            committed_state: committed_state.clone(),
            recovery_files: journal.recovery_files.clone(),
            recovery_digest,
            record_digest: canonical_sha256(&"tracedecay.source-edit-rollback-record.pending")
                .map_err(domain_error)?,
        };
        record.record_digest = record.digest()?;
        if let Some(stored) = self.load_rollback_record(&journal.effect_id)? {
            if stored != record {
                return Err(config_error(
                    "source edit rollback record conflicts with retained effect material",
                ));
            }
            return Ok(());
        }
        persist_record(
            &self.rollback_record_path(&journal.effect_id)?,
            "source-edit-rollback-record",
            &record,
        )
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
        key: &tracedecay_contracts::IdempotencyKey,
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
        key: &tracedecay_contracts::IdempotencyKey,
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
    authority: &tracedecay_contracts::AuthorityReceipt,
    proof: &tracedecay_contracts::SourceEditEffectProofV1,
) -> Result<()> {
    proof
        .validate_for(authority)
        .map_err(application_contract_error)
}

pub(super) fn same_source_edit_authority(
    left: &tracedecay_contracts::AuthorityReceipt,
    right: &tracedecay_contracts::AuthorityReceipt,
) -> bool {
    left.grant_id == right.grant_id
        && left.grant_revision == right.grant_revision
        && left.grant_digest == right.grant_digest
        && left.authorized_scope_digest == right.authorized_scope_digest
        && left.disclosure == right.disclosure
        && left.policy == right.policy
}
