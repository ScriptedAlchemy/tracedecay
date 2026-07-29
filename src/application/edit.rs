use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    ApiMigrationApplyResultV1, ApiMigrationPlanRequestV1, ApiMigrationPlanV1, ApplicationOperation,
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, DirectorySyncPolicy,
    EffectId, EffectReceipt, EffectResult, EffectTermination, OperationBudgetUsage,
    OperationReceipt, OperationTermination, ReconciliationState, SourceEditAuthorizationPort,
    SourceEditDiagnosticV1, SourceEditEffectRequestV1, SourceEditReconciliationDispositionV1,
    SourceEditReconciliationRequestV1, SourceEditRequest, SourceEditVerificationStateV1,
    SourceEditVerificationV1, read_bounded, source_edit_operation,
    source_edit_reconciliation_operation, sync_parent_directory, with_owned_temp_publish,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::{AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult};

const JOURNAL_VERSION: u8 = 1;
const MAX_DURABLE_RECORD_BYTES: usize = 4 * 1024 * 1024;
const SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1: &str = "tracedecay.source-edit-state.v1";

#[derive(Clone, Debug)]
pub struct SourceEditEffectControlV1 {
    deadline: Deadline,
    cancellation: CancellationSignal,
}

impl SourceEditEffectControlV1 {
    pub fn new(deadline: Deadline, cancellation: CancellationSignal) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    fn checkpoint(&self, stage: CancellationStage) -> Option<SourceEditControlStopV1> {
        let observed_at = now_micros();
        let cancellation_requested_at = self.cancellation.cancelled_at();
        let deadline_elapsed = self.deadline.is_elapsed_at(observed_at);
        let termination = match (cancellation_requested_at, deadline_elapsed) {
            (Some(requested_at), true) if requested_at > self.deadline.expires_at => {
                EffectTermination::TimedOut
            }
            (Some(_), _) => EffectTermination::Cancelled,
            (None, true) => EffectTermination::TimedOut,
            (None, false) => return None,
        };
        Some(SourceEditControlStopV1 {
            termination,
            observation: CancellationObservation { stage, observed_at },
        })
    }
}

struct SourceEditControlStopV1 {
    termination: EffectTermination,
    observation: CancellationObservation,
}

/// Body-free metadata retained after the live edit response is returned.
///
/// Durable records intentionally keep no caller-supplied edit text, preview
/// diff, moved/replaced span, import text, diagnostic text, or impact detail.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditDurableOutcomeV1 {
    operation: tracedecay_tool_catalog::UseCaseId,
    success: bool,
    files: Vec<String>,
    change_count: Option<usize>,
    line: Option<u32>,
    before: Option<bool>,
    import_count: Option<usize>,
    finding_count: Option<usize>,
    #[serde(default)]
    failed: bool,
    cancelled: bool,
    timed_out: bool,
    effect_unknown: bool,
    reconciled: bool,
}

impl SourceEditDurableOutcomeV1 {
    fn from_live(
        operation: &tracedecay_tool_catalog::UseCaseId,
        outcome: &SourceEditOutcome,
    ) -> Self {
        let (change_count, line, before, import_count, finding_count) = match outcome {
            SourceEditOutcome::MultiEdit(result) => {
                (Some(result.applied_count), None, None, None, None)
            }
            SourceEditOutcome::Insert(result) => (
                None,
                Some(result.anchor_line),
                Some(result.before),
                None,
                None,
            ),
            SourceEditOutcome::Move(result) => (
                None,
                None,
                None,
                Some(result.applied_imports.len()),
                Some(result.impact.len()),
            ),
            SourceEditOutcome::ApiMigration(result) => {
                (Some(result.changed_sites), None, None, None, None)
            }
            _ => (None, None, None, None, None),
        };
        Self {
            operation: operation.clone(),
            success: outcome.success(),
            files: outcome.candidate_files(),
            change_count,
            line,
            before,
            import_count,
            finding_count,
            failed: matches!(outcome, SourceEditOutcome::Failed { .. }),
            cancelled: matches!(outcome, SourceEditOutcome::Cancelled { .. }),
            timed_out: matches!(outcome, SourceEditOutcome::TimedOut { .. }),
            effect_unknown: matches!(outcome, SourceEditOutcome::EffectUnknown { .. }),
            reconciled: matches!(outcome, SourceEditOutcome::Reconciled { .. }),
        }
    }

    fn value(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.insert("durable_metadata_only".to_owned(), Value::Bool(true));
            object.insert(
                "message".to_owned(),
                Value::String(
                    if self.failed {
                        "source edit failed before the effect"
                    } else if self.cancelled {
                        "source edit was cancelled"
                    } else if self.timed_out {
                        "source edit timed out"
                    } else if self.effect_unknown {
                        "source edit effect is unknown and requires reconciliation"
                    } else if self.reconciled {
                        "source edit reconciliation completed"
                    } else {
                        "source edit completed; detailed edit output was not retained"
                    }
                    .to_owned(),
                ),
            );
        }
        value
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SourceEditOutcome {
    Edit(EditResult),
    MultiEdit(MultiEditResult),
    Insert(InsertResult),
    AstGrep(AstGrepResult),
    Move(MoveResult),
    ApiMigration(ApiMigrationApplyResultV1),
    Failed { message: String },
    Cancelled { message: String },
    TimedOut { message: String },
    EffectUnknown { message: String },
    Reconciled { success: bool, message: String },
    DurableMetadata(SourceEditDurableOutcomeV1),
}

impl SourceEditOutcome {
    pub fn success(&self) -> bool {
        match self {
            Self::Edit(result) => result.success,
            Self::MultiEdit(result) => result.success,
            Self::Insert(result) => result.success,
            Self::AstGrep(result) => result.success,
            Self::Move(result) => result.success,
            Self::ApiMigration(result) => result.success,
            Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::EffectUnknown { .. } => false,
            Self::Reconciled { success, .. } => *success,
            Self::DurableMetadata(result) => result.success,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Edit(result) => &result.message,
            Self::MultiEdit(result) => &result.message,
            Self::Insert(result) => &result.message,
            Self::AstGrep(result) => &result.message,
            Self::Move(result) => &result.message,
            Self::ApiMigration(result) => &result.message,
            Self::Failed { message } | Self::Cancelled { message } | Self::TimedOut { message } => {
                message
            }
            Self::EffectUnknown { message } => message,
            Self::Reconciled { message, .. } => message,
            Self::DurableMetadata(result) if result.failed => {
                "source edit failed before the effect"
            }
            Self::DurableMetadata(result) if result.cancelled => "source edit was cancelled",
            Self::DurableMetadata(result) if result.timed_out => "source edit timed out",
            Self::DurableMetadata(result) if result.effect_unknown => {
                "source edit effect is unknown and requires reconciliation"
            }
            Self::DurableMetadata(result) if result.reconciled => {
                "source edit reconciliation completed"
            }
            Self::DurableMetadata(_) => {
                "source edit completed; detailed edit output was not retained"
            }
        }
    }

    pub fn touched_files(&self, dry_run: bool) -> Vec<String> {
        if dry_run || !self.success() {
            return Vec::new();
        }
        match self {
            Self::Edit(result) => vec![result.file_path.clone()],
            Self::MultiEdit(result) => vec![result.file_path.clone()],
            Self::Insert(result) => vec![result.file_path.clone()],
            Self::AstGrep(result) => vec![result.file_path.clone()],
            Self::Move(result) => vec![result.source_file.clone(), result.dest_file.clone()],
            Self::ApiMigration(result) => result.changed_files.clone(),
            Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::EffectUnknown { .. } => Vec::new(),
            Self::Reconciled { .. } => Vec::new(),
            Self::DurableMetadata(result) => result.files.clone(),
        }
    }

    fn candidate_files(&self) -> Vec<String> {
        match self {
            Self::Edit(result) => vec![result.file_path.clone()],
            Self::MultiEdit(result) => vec![result.file_path.clone()],
            Self::Insert(result) => vec![result.file_path.clone()],
            Self::AstGrep(result) => vec![result.file_path.clone()],
            Self::Move(result) => vec![result.source_file.clone(), result.dest_file.clone()],
            Self::ApiMigration(result) => result.changed_files.clone(),
            Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::EffectUnknown { .. } => Vec::new(),
            Self::Reconciled { .. } => Vec::new(),
            Self::DurableMetadata(result) => result.files.clone(),
        }
    }

    pub fn file_path(&self) -> Option<&str> {
        match self {
            Self::Edit(result) => Some(&result.file_path),
            Self::MultiEdit(result) => Some(&result.file_path),
            Self::Insert(result) => Some(&result.file_path),
            Self::AstGrep(result) => Some(&result.file_path),
            Self::Move(_)
            | Self::ApiMigration(_)
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::EffectUnknown { .. }
            | Self::Reconciled { .. } => None,
            Self::DurableMetadata(result) if result.files.len() == 1 => {
                result.files.first().map(String::as_str)
            }
            Self::DurableMetadata(_) => None,
        }
    }

    pub fn as_move(&self) -> Option<&MoveResult> {
        match self {
            Self::Move(result) => Some(result),
            _ => None,
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Edit(result) => serde_json::to_value(result),
            Self::MultiEdit(result) => serde_json::to_value(result),
            Self::Insert(result) => serde_json::to_value(result),
            Self::AstGrep(result) => serde_json::to_value(result),
            Self::Move(result) => serde_json::to_value(result),
            Self::ApiMigration(result) => serde_json::to_value(result),
            Self::Failed { message } => Ok(json!({
                "success": false,
                "failed": true,
                "message": message,
            })),
            Self::Cancelled { message } => Ok(json!({
                "success": false,
                "cancelled": true,
                "message": message,
            })),
            Self::TimedOut { message } => Ok(json!({
                "success": false,
                "timed_out": true,
                "message": message,
            })),
            Self::EffectUnknown { message } => Ok(json!({
                "success": false,
                "effect_unknown": true,
                "message": message,
            })),
            Self::Reconciled { success, message } => Ok(json!({
                "success": success,
                "reconciled": true,
                "message": message,
            })),
            Self::DurableMetadata(result) => Ok(result.value()),
        }
        .unwrap_or_default()
    }
}

pub struct SourceEditApplicationResult {
    pub outcome: SourceEditOutcome,
    pub dry_run: bool,
    pub expected_state: ManifestDigest,
    pub predicted_state: Option<ManifestDigest>,
    pub verification: Option<SourceEditVerificationV1>,
    pub effect: Option<EffectResult<Value>>,
    pub replayed: bool,
}

impl SourceEditApplicationResult {
    pub fn value(&self) -> Value {
        let mut value = self.outcome.to_value();
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "expected_state".to_owned(),
                serde_json::to_value(&self.expected_state).unwrap_or_default(),
            );
            if let Some(predicted_state) = &self.predicted_state {
                object.insert(
                    "predicted_state".to_owned(),
                    serde_json::to_value(predicted_state).unwrap_or_default(),
                );
            }
            if let Some(verification) = &self.verification {
                object.insert(
                    "verification".to_owned(),
                    serde_json::to_value(verification).unwrap_or_default(),
                );
            }
            if let Some(effect) = &self.effect {
                object.insert(
                    "effect".to_owned(),
                    serde_json::to_value(effect).unwrap_or_default(),
                );
                object.insert("replayed".to_owned(), Value::Bool(self.replayed));
            }
        }
        value
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum SourceEditJournalStateV1 {
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
struct SourceEditJournalV1 {
    version: u8,
    effect_id: EffectId,
    input_digest: ManifestDigest,
    expected_state: ManifestDigest,
    #[serde(default)]
    predicted_state: Option<ManifestDigest>,
    candidate_files: Vec<String>,
    request: SourceEditDurableRequestV1,
    state: SourceEditJournalStateV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEditDurableRequestV1 {
    operation: tracedecay_tool_catalog::UseCaseId,
    request_id: tracedecay_application::RequestId,
    actor: tracedecay_domain::ActorId,
    scope: tracedecay_application::ResolvedScope,
    authority: tracedecay_application::AuthorityReceipt,
    authority_proof: tracedecay_application::SourceEditEffectProofV1,
    idempotency_key: tracedecay_application::IdempotencyKey,
    deadline: tracedecay_application::Deadline,
    started_at: UtcMicros,
    dry_run: bool,
    #[serde(default)]
    verification_requested: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEditDurableResultV1 {
    version: u8,
    input_digest: ManifestDigest,
    authority_proof: tracedecay_application::SourceEditEffectProofV1,
    dry_run: bool,
    #[serde(default)]
    predicted_state: Option<ManifestDigest>,
    outcome: SourceEditDurableOutcomeV1,
    effect: EffectResult<Value>,
}

struct SourceEditDurability {
    root: PathBuf,
}

struct ResolvedSourceEditPreview {
    outcome: SourceEditOutcome,
    candidate_files: Vec<String>,
    expected_state: Option<ManifestDigest>,
    predicted_state: Option<ManifestDigest>,
    planned_files: Vec<crate::tracedecay::PlannedSourceEditFile>,
}

impl SourceEditDurability {
    fn for_graph(graph: &TraceDecay) -> Self {
        Self {
            root: graph
                .store_layout()
                .data_root
                .join("source-edit-transactions-v1"),
        }
    }

    fn lock(&self) -> Result<crate::tracedecay::SyncLockGuard> {
        crate::tracedecay::try_acquire_sync_lock_at(&self.root.join("source-edit.lock"))
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("active.json")
    }

    fn receipt_path(&self, key: &tracedecay_application::IdempotencyKey) -> Result<PathBuf> {
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

    fn load_journal(&self) -> Result<Option<SourceEditJournalV1>> {
        let journal =
            load_record::<SourceEditJournalV1>(&self.journal_path(), "source edit journal")?;
        if let Some(journal) = &journal {
            validate_durable_authority(
                &journal.request.authority,
                &journal.request.authority_proof,
            )?;
        }
        Ok(journal)
    }

    fn persist_journal(&self, journal: &SourceEditJournalV1) -> Result<()> {
        persist_record(&self.journal_path(), "source-edit-journal", journal)
    }

    fn clear_journal(&self) -> Result<()> {
        let path = self.journal_path();
        match fs::remove_file(&path) {
            Ok(()) => sync_parent_directory(&path, DirectorySyncPolicy::Strict)
                .map_err(|error| io_error("sync source edit journal removal", error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error("remove source edit journal", error)),
        }
    }

    fn load_receipt(
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

    fn persist_receipt(&self, receipt: &SourceEditDurableResultV1) -> Result<()> {
        persist_record(
            &self.receipt_path(&receipt.effect.idempotency_key)?,
            "source-edit-receipt",
            receipt,
        )
    }

    fn load_reconciliation_receipt(
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

    fn persist_reconciliation_receipt(&self, receipt: &SourceEditDurableResultV1) -> Result<()> {
        persist_record(
            &self.reconciliation_receipt_path(&receipt.effect.idempotency_key)?,
            "source-edit-reconciliation-receipt",
            receipt,
        )
    }
}

/// Capture the exact candidate-file CAS digest returned by a dry-run preview.
/// Apply callers must echo this digest; the executor independently repeats the
/// preview and recaptures state under its edit lock.
pub async fn preview_source_edit_expected_state(
    graph: &TraceDecay,
    edit: SourceEditRequest,
) -> Result<ManifestDigest> {
    let preview = resolve_source_edit_preview(graph, edit).await?;
    if !preview.outcome.success() {
        return Err(config_error(preview.outcome.message().to_owned()));
    }
    preview
        .expected_state
        .ok_or_else(|| config_error("source edit preview resolved no expected state"))
}

fn durable_request(
    operation: &ApplicationOperation,
    request: &SourceEditEffectRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
) -> SourceEditDurableRequestV1 {
    SourceEditDurableRequestV1 {
        operation: operation.use_case_id().clone(),
        request_id: request.context.request_id().clone(),
        actor: request.context.actor().clone(),
        scope: request.context.scope().clone(),
        authority: authority.receipt.clone(),
        authority_proof: authority.proof.clone(),
        idempotency_key: request.idempotency_key.clone(),
        deadline: request.context.deadline().clone(),
        started_at: request.observed_at,
        dry_run: request.edit.dry_run(),
        verification_requested: request.edit.verify(),
    }
}

#[allow(clippy::too_many_arguments)]
fn persist_pre_effect_result(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditEffectRequestV1,
    authority: &tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &ManifestDigest,
    outcome: SourceEditOutcome,
    expected_state: ManifestDigest,
    predicted_state: Option<ManifestDigest>,
    candidate_files: Vec<String>,
    termination: EffectTermination,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditApplicationResult> {
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != *input_digest {
            return Err(config_error(
                "source edit idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != authority.proof {
            return Err(config_error("source edit receipt authority changed"));
        }
        return Ok(stored.into_application_result(true));
    }
    let journal = SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id: effect_id(&request.idempotency_key, input_digest)?,
        input_digest: input_digest.clone(),
        expected_state: expected_state.clone(),
        predicted_state,
        candidate_files,
        request: durable_request(operation, request, authority),
        state: SourceEditJournalStateV1::Prepared,
    };
    let committed_state = (termination == EffectTermination::Completed).then_some(expected_state);
    let record = durable_record(
        &journal,
        SourceEditDurableOutcomeV1::from_live(operation.use_case_id(), &outcome),
        committed_state,
        now_micros(),
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )?;
    durability.persist_receipt(&record)?;
    Ok(record.into_live_application_result(outcome, None))
}

fn failed_pre_effect_outcome() -> SourceEditOutcome {
    SourceEditOutcome::Failed {
        message: "source edit failed before the effect".to_owned(),
    }
}

pub async fn execute_source_edit<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_inner(graph, operation, request, authorization, None).await
}

pub async fn execute_source_edit_with_control<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
    control: &SourceEditEffectControlV1,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    execute_source_edit_inner(graph, operation, request, authorization, Some(control)).await
}

async fn execute_source_edit_inner<A>(
    graph: &TraceDecay,
    operation: &ApplicationOperation,
    request: SourceEditEffectRequestV1,
    authorization: &A,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    request.validate().map_err(application_contract_error)?;
    let expected =
        source_edit_operation(request.edit.kind()).map_err(application_contract_error)?;
    if operation != &expected {
        return Err(config_error(
            "source edit request does not match its catalog operation",
        ));
    }
    let durability = SourceEditDurability::for_graph(graph);
    let _lock = durability.lock()?;
    let input_digest = request.input_digest().map_err(application_contract_error)?;
    let requested_authority = tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
        request.authority.clone(),
        request.proof.clone(),
        request.context.scope(),
    )
    .map_err(application_contract_error)?;
    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeAdmission))
    {
        let outcome = match stop.termination {
            EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
                message: "source edit was cancelled before admission".to_owned(),
            },
            EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
                message: "source edit timed out before admission".to_owned(),
            },
            _ => unreachable!("source edit control only yields cancellation or timeout"),
        };
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &requested_authority,
            &input_digest,
            outcome,
            request.expected_state.clone(),
            None,
            Vec::new(),
            stop.termination,
            Some(stop.observation),
        );
    }
    let admission = match authorization
        .admit(&request.context, operation, request.observed_at)
        .await
    {
        Ok(admission) => admission,
        Err(_) => {
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &requested_authority,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                None,
                Vec::new(),
                EffectTermination::Failed,
                None,
            );
        }
    };
    if admission.receipt != request.authority || admission.proof != request.proof {
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &requested_authority,
            &input_digest,
            failed_pre_effect_outcome(),
            request.expected_state.clone(),
            None,
            Vec::new(),
            EffectTermination::Failed,
            None,
        );
    }
    let current_authority = match authorization
        .recheck_effect(&request.context, operation, &admission, now_micros())
        .await
    {
        Ok(authority)
            if same_source_edit_authority(&authority.receipt, &request.authority)
                && authority.proof == request.proof =>
        {
            authority
        }
        Ok(_) => {
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &requested_authority,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                None,
                Vec::new(),
                EffectTermination::Failed,
                None,
            );
        }
        Err(error) => {
            if durability.load_receipt(&request.idempotency_key)?.is_some()
                || durability.load_journal()?.is_some()
            {
                return Err(application_problem(error));
            }
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &admission,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                None,
                Vec::new(),
                EffectTermination::Failed,
                None,
            );
        }
    };
    if let Some(result) = recover_or_replay(&durability, &request, &input_digest)? {
        return Ok(result);
    }

    let preview = match resolve_source_edit_preview(graph, request.edit.clone()).await {
        Ok(preview) => preview,
        Err(_) => {
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                None,
                Vec::new(),
                EffectTermination::Failed,
                None,
            );
        }
    };
    if !preview.outcome.success() {
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            preview.outcome,
            preview
                .expected_state
                .unwrap_or_else(|| request.expected_state.clone()),
            preview.predicted_state,
            preview.candidate_files,
            EffectTermination::Failed,
            None,
        );
    }
    let predicted_state = preview
        .predicted_state
        .ok_or_else(|| config_error("successful source edit preview omitted predicted state"))?;
    let planned_files = preview.planned_files;
    let candidate_files = preview.candidate_files;
    let current_state = preview
        .expected_state
        .ok_or_else(|| config_error("successful source edit preview omitted expected state"))?;
    if !request.edit.dry_run() && current_state != request.expected_state {
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            failed_pre_effect_outcome(),
            request.expected_state.clone(),
            Some(predicted_state),
            candidate_files,
            EffectTermination::Failed,
            None,
        );
    }
    if request.edit.dry_run() {
        if let Some(stop) =
            control.and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
        {
            let outcome = match stop.termination {
                EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
                    message: "source edit preview was cancelled".to_owned(),
                },
                EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
                    message: "source edit preview timed out".to_owned(),
                },
                _ => unreachable!("source edit control only yields cancellation or timeout"),
            };
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                outcome,
                current_state,
                Some(predicted_state),
                candidate_files,
                stop.termination,
                Some(stop.observation),
            );
        }
        let current_authority = match authorization
            .recheck_effect(&request.context, operation, &admission, now_micros())
            .await
        {
            Ok(authority)
                if same_source_edit_authority(&authority.receipt, &request.authority)
                    && authority.proof == request.proof =>
            {
                authority
            }
            _ => {
                return persist_pre_effect_result(
                    &durability,
                    operation,
                    &request,
                    &current_authority,
                    &input_digest,
                    failed_pre_effect_outcome(),
                    current_state,
                    Some(predicted_state),
                    candidate_files,
                    EffectTermination::Failed,
                    None,
                );
            }
        };
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            preview.outcome,
            current_state,
            Some(predicted_state),
            candidate_files,
            EffectTermination::Completed,
            None,
        );
    }

    // Current authority and policy are checked again after every preview/read,
    // immediately before expected-state recapture and journal publication.
    let current_authority = match authorization
        .recheck_effect(&request.context, operation, &admission, now_micros())
        .await
    {
        Ok(authority)
            if same_source_edit_authority(&authority.receipt, &request.authority)
                && authority.proof == request.proof =>
        {
            authority
        }
        _ => {
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                Some(predicted_state),
                candidate_files,
                EffectTermination::Failed,
                None,
            );
        }
    };
    let recaptured_state = match source_edit_state_digest(graph.project_root(), &candidate_files) {
        Ok(state) => state,
        Err(_) => {
            return persist_pre_effect_result(
                &durability,
                operation,
                &request,
                &current_authority,
                &input_digest,
                failed_pre_effect_outcome(),
                request.expected_state.clone(),
                Some(predicted_state),
                candidate_files,
                EffectTermination::Failed,
                None,
            );
        }
    };
    if recaptured_state != request.expected_state {
        return persist_pre_effect_result(
            &durability,
            operation,
            &request,
            &current_authority,
            &input_digest,
            failed_pre_effect_outcome(),
            request.expected_state.clone(),
            Some(predicted_state),
            candidate_files,
            EffectTermination::Failed,
            None,
        );
    }

    let effect_id = effect_id(&request.idempotency_key, &input_digest)?;
    let durable_request = durable_request(operation, &request, &current_authority);
    let mut journal = SourceEditJournalV1 {
        version: JOURNAL_VERSION,
        effect_id,
        input_digest: input_digest.clone(),
        expected_state: request.expected_state.clone(),
        predicted_state: Some(predicted_state.clone()),
        candidate_files,
        request: durable_request,
        state: SourceEditJournalStateV1::Prepared,
    };
    durability.persist_journal(&journal)?;

    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeEffect))
    {
        let live_outcome = match stop.termination {
            EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
                message: "source edit was cancelled before the effect".to_owned(),
            },
            EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
                message: "source edit timed out before the effect".to_owned(),
            },
            _ => unreachable!("source edit control only yields cancellation or timeout"),
        };
        let record = interrupted_record(&journal, &live_outcome, stop)?;
        durability.persist_receipt(&record)?;
        durability.clear_journal()?;
        return Ok(record.into_live_application_result(live_outcome, None));
    }

    let (effect_result, plan_complete) = crate::tracedecay::apply_source_edit_plan(
        planned_files,
        run_source_edit(graph, request.edit.clone().with_dry_run(false), control),
    )
    .await;
    let mut outcome = match effect_result {
        Ok(outcome) => outcome,
        Err(error) => {
            // The edit primitive may have crossed its atomic rename boundary.
            // Retain Prepared and report EffectUnknown; never retry implicitly.
            let live_outcome = SourceEditOutcome::EffectUnknown {
                message: format!(
                    "source edit effect is unknown and requires reconciliation: {}",
                    error.to_string().chars().take(1024).collect::<String>()
                ),
            };
            let record = unknown_record(&journal)?;
            durability.persist_receipt(&record)?;
            return Ok(record.into_live_application_result(live_outcome, None));
        }
    };
    let mut control_observation = control
        .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
        .map(|stop| stop.observation);
    let mut committed_state =
        source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
    if outcome.success() && (!plan_complete || committed_state != predicted_state) {
        let live_outcome = SourceEditOutcome::EffectUnknown {
            message: "source edit effect is unknown and requires reconciliation: the observed committed state did not match the exact preview".to_owned(),
        };
        let record = unknown_record(&journal)?;
        durability.persist_receipt(&record)?;
        return Ok(record.into_live_application_result(live_outcome, None));
    }
    if !outcome.success() && committed_state != journal.expected_state {
        let live_outcome = SourceEditOutcome::EffectUnknown {
            message: "source edit effect is unknown and requires reconciliation: the edit reported failure after candidate state changed".to_owned(),
        };
        let record = unknown_record(&journal)?;
        durability.persist_receipt(&record)?;
        return Ok(record.into_live_application_result(live_outcome, None));
    }
    let verification = if request.edit.verify() && outcome.success() {
        let files = outcome.candidate_files();
        if files.is_empty() {
            None
        } else {
            Some(run_edit_verifications(graph, &files).await)
        }
    } else {
        None
    };
    if verification
        .as_ref()
        .is_some_and(|result| !matches!(result.state, SourceEditVerificationStateV1::Clean))
        && let (
            SourceEditRequest::ApiMigrationApply { plan, .. },
            SourceEditOutcome::ApiMigration(result),
        ) = (&request.edit, &mut outcome)
    {
        graph.rollback_api_migration_plan(plan).await?;
        result.success = false;
        result.rolled_back = true;
        result.changed_files.clear();
        "API migration verification did not pass; every changed file was restored"
            .clone_into(&mut result.message);
        committed_state = source_edit_state_digest(graph.project_root(), &journal.candidate_files)?;
        if committed_state != journal.expected_state {
            let live_outcome = SourceEditOutcome::EffectUnknown {
                message: "API migration verification rollback did not restore the previewed state"
                    .to_owned(),
            };
            let record = unknown_record(&journal)?;
            durability.persist_receipt(&record)?;
            return Ok(record.into_live_application_result(live_outcome, verification));
        }
    }

    let ended_at = now_micros();
    journal.state = SourceEditJournalStateV1::Applied {
        outcome: SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
        committed_state: committed_state.clone(),
        ended_at,
        control_observation: control_observation.clone(),
        verification_state: None,
    };
    durability.persist_journal(&journal)?;

    if let SourceEditJournalStateV1::Applied {
        verification_state, ..
    } = &mut journal.state
    {
        *verification_state = verification.as_ref().map(|result| result.state);
    }
    if request.edit.verify() {
        durability.persist_journal(&journal)?;
    }
    if control_observation.is_none() {
        control_observation = control
            .and_then(|control| control.checkpoint(CancellationStage::AfterCommit))
            .map(|stop| stop.observation);
        if let SourceEditJournalStateV1::Applied {
            control_observation: durable_observation,
            ..
        } = &mut journal.state
        {
            durable_observation.clone_from(&control_observation);
        }
        if control_observation.is_some() {
            durability.persist_journal(&journal)?;
        }
    }
    let record = applied_record(
        &journal,
        &outcome,
        committed_state,
        ended_at,
        control_observation,
    )?;
    durability.persist_receipt(&record)?;
    durability.clear_journal()?;
    Ok(record.into_live_application_result(outcome, verification))
}

async fn resolve_source_edit_preview(
    graph: &TraceDecay,
    edit: SourceEditRequest,
) -> Result<ResolvedSourceEditPreview> {
    let (outcome, planned_files) = crate::tracedecay::capture_source_edit_plan(run_source_edit(
        graph,
        edit.with_dry_run(true),
        None,
    ))
    .await;
    let outcome = outcome?;
    if !outcome.success() {
        return Ok(ResolvedSourceEditPreview {
            outcome,
            candidate_files: Vec::new(),
            expected_state: None,
            predicted_state: None,
            planned_files: Vec::new(),
        });
    }
    let candidate_files =
        normalize_candidate_files(graph.project_root(), outcome.candidate_files())?;
    let planned_candidate_files = normalize_candidate_files(
        graph.project_root(),
        planned_files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect(),
    )?;
    if planned_files.len() != candidate_files.len() || planned_candidate_files != candidate_files {
        return Err(config_error(
            "source edit preview did not produce one exact plan for every candidate file",
        ));
    }
    let expected_state = planned_source_edit_state_digest(&candidate_files, &planned_files, false)?;
    let observed_state = source_edit_state_digest(graph.project_root(), &candidate_files)?;
    if observed_state != expected_state {
        return Err(config_error(
            "source edit candidate state changed while its exact preview was captured",
        ));
    }
    let predicted_state = planned_source_edit_state_digest(&candidate_files, &planned_files, true)?;
    Ok(ResolvedSourceEditPreview {
        outcome,
        candidate_files,
        expected_state: Some(expected_state),
        predicted_state: Some(predicted_state),
        planned_files,
    })
}

/// Resolve one retained `EffectUnknown` only after an authorized inspection
/// explicitly proves either the exact committed state or the exact rollback
/// state. A mismatch retains the journal and its uncertainty.
pub async fn reconcile_source_edit_effect_unknown_with_control<A>(
    graph: &TraceDecay,
    request: SourceEditReconciliationRequestV1,
    authorization: &A,
    control: &SourceEditEffectControlV1,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    reconcile_source_edit_effect_unknown_inner(graph, request, authorization, Some(control)).await
}

async fn reconcile_source_edit_effect_unknown_inner<A>(
    graph: &TraceDecay,
    request: SourceEditReconciliationRequestV1,
    authorization: &A,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditApplicationResult>
where
    A: SourceEditAuthorizationPort,
{
    request.validate().map_err(application_contract_error)?;
    let attempt_input_digest = request
        .attempt_input_digest()
        .map_err(application_contract_error)?;
    let reconciliation_operation =
        source_edit_reconciliation_operation().map_err(application_contract_error)?;
    let original_operation =
        source_edit_operation(request.kind).map_err(application_contract_error)?;
    let durability = SourceEditDurability::for_graph(graph);
    let _lock = durability.lock()?;
    if let Some(stored) =
        recover_reconciliation_attempt(&durability, &request, &attempt_input_digest)?
    {
        return Ok(stored);
    }
    if let Some(stop) =
        control.and_then(|control| control.checkpoint(CancellationStage::BeforeAdmission))
    {
        let journal = durability
            .load_journal()?
            .ok_or_else(|| config_error("no source edit effect requires reconciliation"))?;
        if journal.version != JOURNAL_VERSION
            || journal.effect_id != request.effect_id
            || journal.request.idempotency_key != request.idempotency_key
            || journal.input_digest != request.input_digest
            || journal.request.operation != *original_operation.use_case_id()
            || journal.request.actor != *request.context.actor()
            || journal.request.scope != *request.context.scope()
            || !matches!(journal.state, SourceEditJournalStateV1::Prepared)
        {
            return Err(config_error(
                "source edit reconciliation identity does not match the retained effect",
            ));
        }
        let authority = tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
            request.authority.clone(),
            request.proof.clone(),
            request.context.scope(),
        )
        .map_err(application_contract_error)?;
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &reconciliation_operation,
            authority: &authority,
            input_digest: &attempt_input_digest,
            control,
        };
        return persist_interrupted_reconciliation_attempt(
            &durability,
            &journal,
            &request,
            &attempt,
            stop,
        );
    }
    let admission = authorization
        .admit(
            &request.context,
            &reconciliation_operation,
            request.observed_at,
        )
        .await
        .map_err(application_problem)?;
    if admission.receipt != request.authority || admission.proof != request.proof {
        return Err(config_error(
            "source edit reconciliation admission differs from its authority receipt",
        ));
    }
    let current_authority = authorization
        .recheck_effect(
            &request.context,
            &reconciliation_operation,
            &admission,
            now_micros(),
        )
        .await
        .map_err(application_problem)?;
    if !same_source_edit_authority(&current_authority.receipt, &request.authority)
        || current_authority.proof != request.proof
    {
        return Err(config_error(
            "source edit reconciliation current authority changed",
        ));
    }

    reconcile_prepared_source_edit_controlled(
        &durability,
        graph.project_root(),
        &original_operation,
        request,
        Some(SourceEditReconciliationAttemptV1 {
            operation: &reconciliation_operation,
            authority: &current_authority,
            input_digest: &attempt_input_digest,
            control,
        }),
    )
}

fn recover_reconciliation_attempt(
    durability: &SourceEditDurability,
    request: &SourceEditReconciliationRequestV1,
    attempt_input_digest: &ManifestDigest,
) -> Result<Option<SourceEditApplicationResult>> {
    let Some(stored) = durability.load_reconciliation_receipt(&request.attempt_idempotency_key)?
    else {
        return Ok(None);
    };
    if stored.input_digest != *attempt_input_digest {
        return Err(config_error(
            "source edit reconciliation attempt idempotency key conflicts with a prior input",
        ));
    }
    if stored.authority_proof != request.proof
        || !same_source_edit_authority(&stored.effect.authority, &request.authority)
    {
        return Err(config_error(
            "source edit reconciliation replay authority changed",
        ));
    }
    if stored.effect.receipt.outcome == EffectTermination::Completed {
        let original = durability
            .load_receipt(&request.idempotency_key)?
            .ok_or_else(|| {
                config_error(
                    "completed reconciliation attempt is missing its original effect receipt",
                )
            })?;
        if original.input_digest != request.input_digest
            || original.effect.reconciliation != ReconciliationState::Reconciled
            || original.effect.receipt.outcome == EffectTermination::EffectUnknown
        {
            return Err(config_error(
                "completed reconciliation attempt does not match a terminal original effect",
            ));
        }
        if let Some(journal) = durability.load_journal()?
            && journal.effect_id == request.effect_id
            && journal.request.idempotency_key == request.idempotency_key
            && journal.input_digest == request.input_digest
            && matches!(journal.state, SourceEditJournalStateV1::Prepared)
        {
            durability.clear_journal()?;
        }
    }
    Ok(Some(stored.into_application_result(true)))
}

struct SourceEditReconciliationAttemptV1<'a> {
    operation: &'a ApplicationOperation,
    authority: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
    input_digest: &'a ManifestDigest,
    control: Option<&'a SourceEditEffectControlV1>,
}

fn retained_reconciliation_journal(
    durability: &SourceEditDurability,
    operation: &ApplicationOperation,
    request: &SourceEditReconciliationRequestV1,
) -> Result<SourceEditJournalV1> {
    let journal = durability
        .load_journal()?
        .ok_or_else(|| config_error("no source edit effect requires reconciliation"))?;
    if journal.version != JOURNAL_VERSION
        || journal.effect_id != request.effect_id
        || journal.request.idempotency_key != request.idempotency_key
        || journal.input_digest != request.input_digest
        || &journal.request.operation != operation.use_case_id()
        || &journal.request.actor != request.context.actor()
        || &journal.request.scope != request.context.scope()
    {
        return Err(config_error(
            "source edit reconciliation identity does not match the retained effect",
        ));
    }
    if !matches!(journal.state, SourceEditJournalStateV1::Prepared) {
        return Err(config_error(
            "source edit effect already has a durable applied-state proof",
        ));
    }
    Ok(journal)
}

#[cfg(test)]
fn reconcile_prepared_source_edit(
    durability: &SourceEditDurability,
    project_root: &Path,
    operation: &ApplicationOperation,
    request: SourceEditReconciliationRequestV1,
) -> Result<SourceEditApplicationResult> {
    reconcile_prepared_source_edit_controlled(durability, project_root, operation, request, None)
}

fn reconcile_prepared_source_edit_controlled(
    durability: &SourceEditDurability,
    project_root: &Path,
    operation: &ApplicationOperation,
    request: SourceEditReconciliationRequestV1,
    attempt: Option<SourceEditReconciliationAttemptV1<'_>>,
) -> Result<SourceEditApplicationResult> {
    let journal = retained_reconciliation_journal(durability, operation, &request)?;
    if let Some(attempt) = &attempt
        && let Some(stop) = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::BeforeEffect))
    {
        return persist_interrupted_reconciliation_attempt(
            durability, &journal, &request, attempt, stop,
        );
    }
    let observed_state = source_edit_state_digest(project_root, &journal.candidate_files)?;
    if let Some(attempt) = &attempt
        && let Some(stop) = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::EffectInFlight))
    {
        return persist_interrupted_reconciliation_attempt(
            durability, &journal, &request, attempt, stop,
        );
    }
    let ended_at = now_micros();
    let (_outcome, record) = match request.disposition.clone() {
        SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } => {
            let predicted_state = journal.predicted_state.as_ref().ok_or_else(|| {
                config_error(
                    "source edit committed state cannot be proven from this legacy journal",
                )
            })?;
            if &committed_state != predicted_state || observed_state != *predicted_state {
                return Err(config_error(
                    "source edit committed-state inspection does not match the exact preview",
                ));
            }
            let outcome = SourceEditOutcome::Reconciled {
                success: true,
                message: "source edit effect was independently confirmed committed".to_owned(),
            };
            let record = applied_record(&journal, &outcome, committed_state, ended_at, None)?;
            (outcome, record)
        }
        SourceEditReconciliationDispositionV1::ConfirmRolledBack => {
            if observed_state != journal.expected_state {
                return Err(config_error(
                    "source edit rollback inspection does not match the admitted expected state",
                ));
            }
            let outcome = SourceEditOutcome::Reconciled {
                success: false,
                message: "source edit effect was independently confirmed rolled back".to_owned(),
            };
            let record = durable_record(
                &journal,
                SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
                None,
                ended_at,
                EffectTermination::Failed,
                ReconciliationState::Reconciled,
                None,
            )?;
            (outcome, record)
        }
    };
    durability.persist_receipt(&record)?;
    let result = if let Some(attempt) = attempt {
        let after_commit_observation = attempt
            .control
            .and_then(|control| control.checkpoint(CancellationStage::AfterCommit))
            .map(|stop| stop.observation);
        let attempt_outcome = SourceEditOutcome::Reconciled {
            success: true,
            message: "source edit reconciliation attempt completed".to_owned(),
        };
        let attempt_record = reconciliation_attempt_record(
            &journal,
            &request,
            &attempt,
            &attempt_outcome,
            record.effect.receipt.committed_state.clone(),
            ended_at,
            EffectTermination::Completed,
            after_commit_observation,
        )?;
        durability.persist_reconciliation_receipt(&attempt_record)?;
        attempt_record.into_live_application_result(attempt_outcome, None)
    } else {
        record.into_application_result(true)
    };
    durability.clear_journal()?;
    Ok(result)
}

fn recover_or_replay(
    durability: &SourceEditDurability,
    request: &SourceEditEffectRequestV1,
    input_digest: &ManifestDigest,
) -> Result<Option<SourceEditApplicationResult>> {
    if let Some(stored) = durability.load_receipt(&request.idempotency_key)? {
        if stored.input_digest != *input_digest {
            return Err(config_error(
                "source edit idempotency key conflicts with a prior input",
            ));
        }
        if stored.authority_proof != request.proof
            || !same_source_edit_authority(&stored.effect.authority, &request.authority)
        {
            return Err(config_error("source edit replay authority changed"));
        }
        if let Some(journal) = durability.load_journal()?
            && journal.request.idempotency_key == request.idempotency_key
            && journal.input_digest == *input_digest
            && matches!(journal.state, SourceEditJournalStateV1::Applied { .. })
        {
            durability.clear_journal()?;
        }
        return Ok(Some(stored.into_application_result(true)));
    }
    durability
        .load_journal()?
        .map(|journal| reconcile_journal(durability, journal, request, input_digest))
        .transpose()
}

fn reconcile_journal(
    durability: &SourceEditDurability,
    journal: SourceEditJournalV1,
    request: &SourceEditEffectRequestV1,
    input_digest: &ManifestDigest,
) -> Result<SourceEditApplicationResult> {
    if journal.version != JOURNAL_VERSION {
        return Err(config_error(
            "unsupported source edit transaction journal version",
        ));
    }
    if journal.request.idempotency_key != request.idempotency_key
        || journal.input_digest != *input_digest
        || !same_source_edit_authority(&journal.request.authority, &request.authority)
        || journal.request.authority_proof != request.proof
    {
        return Err(config_error(
            "a source edit transaction requires reconciliation before another mutation",
        ));
    }
    let record = match &journal.state {
        SourceEditJournalStateV1::Prepared => unknown_record(&journal)?,
        SourceEditJournalStateV1::Applied {
            outcome,
            committed_state,
            ended_at,
            control_observation,
            verification_state,
        } => applied_durable_record(
            &journal,
            outcome.clone(),
            committed_state.clone(),
            *ended_at,
            control_observation.clone(),
            *verification_state,
        )?,
    };
    durability.persist_receipt(&record)?;
    if matches!(journal.state, SourceEditJournalStateV1::Applied { .. }) {
        durability.clear_journal()?;
    }
    Ok(record.into_application_result(true))
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

    fn into_application_result(self, replayed: bool) -> SourceEditApplicationResult {
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

    fn into_live_application_result(
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

fn same_source_edit_authority(
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

fn applied_record(
    journal: &SourceEditJournalV1,
    outcome: &SourceEditOutcome,
    committed_state: ManifestDigest,
    ended_at: UtcMicros,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let verification_state = match &journal.state {
        SourceEditJournalStateV1::Applied {
            verification_state, ..
        } => *verification_state,
        SourceEditJournalStateV1::Prepared => None,
    };
    let termination = applied_effect_termination(
        journal.request.verification_requested,
        verification_state,
        outcome.success(),
    );
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, outcome),
        Some(committed_state),
        ended_at,
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )
}

fn applied_durable_record(
    journal: &SourceEditJournalV1,
    outcome: SourceEditDurableOutcomeV1,
    committed_state: ManifestDigest,
    ended_at: UtcMicros,
    control_observation: Option<CancellationObservation>,
    verification_state: Option<SourceEditVerificationStateV1>,
) -> Result<SourceEditDurableResultV1> {
    let termination = applied_effect_termination(
        journal.request.verification_requested,
        verification_state,
        outcome.success,
    );
    durable_record(
        journal,
        outcome,
        Some(committed_state),
        ended_at,
        termination,
        ReconciliationState::Reconciled,
        control_observation,
    )
}

fn applied_effect_termination(
    verification_requested: bool,
    verification_state: Option<SourceEditVerificationStateV1>,
    source_edit_succeeded: bool,
) -> EffectTermination {
    if !source_edit_succeeded {
        return EffectTermination::Failed;
    }
    if !verification_requested
        || matches!(
            verification_state,
            Some(SourceEditVerificationStateV1::Clean | SourceEditVerificationStateV1::Errors)
        )
    {
        EffectTermination::Completed
    } else {
        EffectTermination::Partial
    }
}

fn unknown_record(journal: &SourceEditJournalV1) -> Result<SourceEditDurableResultV1> {
    let outcome = SourceEditOutcome::EffectUnknown {
        message: "source edit effect is unknown and requires reconciliation".to_owned(),
    };
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
        None,
        now_micros(),
        EffectTermination::EffectUnknown,
        ReconciliationState::Pending,
        None,
    )
}

fn interrupted_record(
    journal: &SourceEditJournalV1,
    outcome: &SourceEditOutcome,
    stop: SourceEditControlStopV1,
) -> Result<SourceEditDurableResultV1> {
    durable_record(
        journal,
        SourceEditDurableOutcomeV1::from_live(&journal.request.operation, outcome),
        None,
        stop.observation.observed_at,
        stop.termination,
        ReconciliationState::Reconciled,
        Some(stop.observation),
    )
}

fn persist_interrupted_reconciliation_attempt(
    durability: &SourceEditDurability,
    journal: &SourceEditJournalV1,
    request: &SourceEditReconciliationRequestV1,
    attempt: &SourceEditReconciliationAttemptV1<'_>,
    stop: SourceEditControlStopV1,
) -> Result<SourceEditApplicationResult> {
    let outcome = match stop.termination {
        EffectTermination::Cancelled => SourceEditOutcome::Cancelled {
            message: "source edit reconciliation attempt was cancelled".to_owned(),
        },
        EffectTermination::TimedOut => SourceEditOutcome::TimedOut {
            message: "source edit reconciliation attempt timed out".to_owned(),
        },
        _ => unreachable!("source edit control only yields cancellation or timeout"),
    };
    let record = reconciliation_attempt_record(
        journal,
        request,
        attempt,
        &outcome,
        None,
        stop.observation.observed_at,
        stop.termination,
        Some(stop.observation),
    )?;
    durability.persist_reconciliation_receipt(&record)?;
    Ok(record.into_live_application_result(outcome, None))
}

#[allow(clippy::too_many_arguments)]
fn reconciliation_attempt_record(
    journal: &SourceEditJournalV1,
    request: &SourceEditReconciliationRequestV1,
    attempt: &SourceEditReconciliationAttemptV1<'_>,
    outcome: &SourceEditOutcome,
    committed_state: Option<ManifestDigest>,
    ended_at: UtcMicros,
    termination: EffectTermination,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let committed_state = if termination == EffectTermination::Completed {
        Some(match &request.disposition {
            SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } => {
                committed_state.clone()
            }
            SourceEditReconciliationDispositionV1::ConfirmRolledBack => {
                journal.expected_state.clone()
            }
        })
    } else {
        committed_state
    };
    let operation_termination = match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    };
    let execution = OperationReceipt {
        started_at: request.observed_at,
        ended_at: ended_at.max(request.observed_at),
        effective_deadline: request.context.deadline().clone(),
        cancellation: control_observation,
        budget: OperationBudgetUsage::default(),
        termination: operation_termination,
    };
    let receipt = EffectReceipt {
        operation: attempt.operation.use_case_id().clone(),
        request_id: request.context.request_id().clone(),
        actor: request.context.actor().clone(),
        scope: request.context.scope().clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::SourceEdit,
        idempotency_key: request.attempt_idempotency_key.clone(),
        input_digest: attempt.input_digest.clone(),
        expected_state: journal.expected_state.clone(),
        policy_digest: attempt.authority.proof.policy_digest.clone(),
        configuration_digest: attempt.authority.proof.configuration_digest.clone(),
        catalog_digest: attempt.authority.proof.catalog_digest.clone(),
        privacy_digest: attempt.authority.proof.privacy_digest.clone(),
        outcome: termination,
        committed_state,
        external_proof: attempt.authority.proof.external_proof.clone(),
    };
    let effect_id =
        reconciliation_attempt_effect_id(&request.attempt_idempotency_key, attempt.input_digest)?;
    let durable_outcome =
        SourceEditDurableOutcomeV1::from_live(attempt.operation.use_case_id(), outcome);
    let effect = EffectResult::new(
        effect_id,
        tracedecay_tool_catalog::EffectClass::SourceEdit,
        request.attempt_idempotency_key.clone(),
        attempt.authority.receipt.clone(),
        journal.expected_state.clone(),
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(durable_outcome.value()),
    )
    .map_err(application_contract_error)?;
    Ok(SourceEditDurableResultV1 {
        version: JOURNAL_VERSION,
        input_digest: attempt.input_digest.clone(),
        authority_proof: attempt.authority.proof.clone(),
        dry_run: false,
        predicted_state: journal.predicted_state.clone(),
        outcome: durable_outcome,
        effect,
    })
}

fn durable_record(
    journal: &SourceEditJournalV1,
    outcome: SourceEditDurableOutcomeV1,
    committed_state: Option<ManifestDigest>,
    ended_at: UtcMicros,
    termination: EffectTermination,
    reconciliation: ReconciliationState,
    control_observation: Option<CancellationObservation>,
) -> Result<SourceEditDurableResultV1> {
    let request = &journal.request;
    let operation_termination = match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    };
    let execution = OperationReceipt {
        started_at: request.started_at,
        ended_at: ended_at.max(request.started_at),
        effective_deadline: request.deadline.clone(),
        cancellation: control_observation,
        budget: OperationBudgetUsage::default(),
        termination: operation_termination,
    };
    let receipt = EffectReceipt {
        operation: request.operation.clone(),
        request_id: request.request_id.clone(),
        actor: request.actor.clone(),
        scope: request.scope.clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::SourceEdit,
        idempotency_key: request.idempotency_key.clone(),
        input_digest: journal.input_digest.clone(),
        expected_state: journal.expected_state.clone(),
        policy_digest: request.authority_proof.policy_digest.clone(),
        configuration_digest: request.authority_proof.configuration_digest.clone(),
        catalog_digest: request.authority_proof.catalog_digest.clone(),
        privacy_digest: request.authority_proof.privacy_digest.clone(),
        outcome: termination,
        committed_state,
        external_proof: request.authority_proof.external_proof.clone(),
    };
    let effect = EffectResult::new(
        journal.effect_id.clone(),
        tracedecay_tool_catalog::EffectClass::SourceEdit,
        request.idempotency_key.clone(),
        request.authority.clone(),
        journal.expected_state.clone(),
        execution,
        reconciliation,
        receipt,
        Some(outcome.value()),
    )
    .map_err(application_contract_error)?;
    Ok(SourceEditDurableResultV1 {
        version: JOURNAL_VERSION,
        input_digest: journal.input_digest.clone(),
        authority_proof: request.authority_proof.clone(),
        dry_run: request.dry_run,
        predicted_state: journal.predicted_state.clone(),
        outcome,
        effect,
    })
}

async fn run_source_edit(
    graph: &TraceDecay,
    request: SourceEditRequest,
    control: Option<&SourceEditEffectControlV1>,
) -> Result<SourceEditOutcome> {
    Ok(match request {
        SourceEditRequest::StrReplace {
            path,
            old_str,
            new_str,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(
            graph
                .str_replace(&path, &old_str, &new_str, dry_run)
                .await?,
        ),
        SourceEditRequest::MultiStrReplace {
            path,
            replacements,
            dry_run,
            ..
        } => {
            let replacements = replacements
                .iter()
                .map(|(old, new)| (old.as_str(), new.as_str()))
                .collect::<Vec<_>>();
            SourceEditOutcome::MultiEdit(
                graph
                    .multi_str_replace(&path, &replacements, dry_run)
                    .await?,
            )
        }
        SourceEditRequest::InsertAt {
            path,
            anchor,
            content,
            before,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            graph
                .insert_at(&path, &anchor, &content, before, dry_run)
                .await?,
        ),
        SourceEditRequest::AstGrepRewrite {
            path,
            pattern,
            rewrite,
            dry_run,
            ..
        } => SourceEditOutcome::AstGrep(
            graph
                .ast_grep_rewrite(&path, &pattern, &rewrite, dry_run)
                .await?,
        ),
        SourceEditRequest::ReplaceSymbol {
            symbol,
            new_source,
            dry_run,
            ..
        } => SourceEditOutcome::Edit(graph.replace_symbol(&symbol, &new_source, dry_run).await?),
        SourceEditRequest::InsertAtSymbol {
            symbol,
            content,
            position,
            dry_run,
            ..
        } => SourceEditOutcome::Insert(
            graph
                .insert_at_symbol(&symbol, &content, &position, dry_run)
                .await?,
        ),
        SourceEditRequest::MoveSymbol {
            symbol,
            dest_file,
            dry_run,
            update_references,
        } => SourceEditOutcome::Move(
            graph
                .move_symbol(&symbol, &dest_file, dry_run, update_references)
                .await?,
        ),
        SourceEditRequest::ApiMigrationApply {
            plan,
            plan_digest,
            dry_run,
            ..
        } => {
            if plan.plan_digest != plan_digest {
                return Err(config_error(
                    "API migration apply digest does not match its immutable plan",
                ));
            }
            let replanned = crate::application::api_migration::plan_api_migration(
                graph,
                ApiMigrationPlanRequestV1 {
                    family_id: plan.family_id.clone(),
                    operations: plan.operations.clone(),
                },
            )
            .await?;
            validate_replanned_api_migration(&plan, &replanned)?;
            SourceEditOutcome::ApiMigration(
                graph
                    .apply_api_migration_plan(&replanned, dry_run, || {
                        control
                            .and_then(|control| {
                                control.checkpoint(CancellationStage::EffectInFlight)
                            })
                            .is_some()
                    })
                    .await?,
            )
        }
    })
}

fn validate_replanned_api_migration(
    supplied: &ApiMigrationPlanV1,
    replanned: &ApiMigrationPlanV1,
) -> Result<()> {
    if supplied != replanned {
        return Err(config_error(
            "API migration plan does not match current graph-backed evidence; replan before apply",
        ));
    }
    Ok(())
}

fn normalize_candidate_files(root: &Path, files: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::with_capacity(files.len());
    for file in files {
        let path = Path::new(&file);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(config_error(
                "source edit candidate path is outside the authorized worktree",
            ));
        }
        let value = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            .collect::<PathBuf>();
        crate::tracedecay::validate_source_edit_candidate_parent(root, &value)?;
        normalized.push(value.to_string_lossy().into_owned());
    }
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() {
        return Err(config_error(
            "source edit preview resolved no candidate files",
        ));
    }
    Ok(normalized)
}

fn source_edit_state_digest(root: &Path, files: &[String]) -> Result<ManifestDigest> {
    let mut states = Vec::with_capacity(files.len());
    for relative in files {
        let state = match crate::tracedecay::read_source_edit_candidate(root, Path::new(relative))? {
            Some(bytes) => Some(hash_source_edit_content(&bytes)?),
            None => None,
        };
        states.push((relative, state));
    }
    canonical_sha256(&(SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1, states)).map_err(domain_error)
}

fn planned_source_edit_state_digest(
    files: &[String],
    planned_files: &[crate::tracedecay::PlannedSourceEditFile],
    intended: bool,
) -> Result<ManifestDigest> {
    let mut states = Vec::with_capacity(files.len());
    for relative in files {
        let mut matches = planned_files
            .iter()
            .filter(|planned| &planned.relative_path == relative);
        let planned = matches.next().ok_or_else(|| {
            config_error("source edit candidate is missing from its exact preview plan")
        })?;
        if matches.next().is_some() {
            return Err(config_error(
                "source edit candidate appears more than once in its exact preview plan",
            ));
        }
        let content = if intended {
            planned.intended.as_deref()
        } else {
            planned.expected.as_deref()
        };
        states.push((
            relative,
            content
                .map(|content| hash_source_edit_content(content.as_bytes()))
                .transpose()?,
        ));
    }
    canonical_sha256(&(SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1, states)).map_err(domain_error)
}

fn hash_source_edit_content(content: &[u8]) -> Result<ManifestDigest> {
    ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(content))))
        .map_err(domain_error)
}

fn effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    let digest = canonical_sha256(&("tracedecay.source-edit-effect-id.v1", key, input_digest))
        .map_err(domain_error)?;
    EffectId::new(format!(
        "effect.source-edit.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(application_contract_error)
}

fn reconciliation_attempt_effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    let digest = canonical_sha256(&(
        "tracedecay.source-edit-reconciliation-attempt-effect-id.v1",
        key,
        input_digest,
    ))
    .map_err(domain_error)?;
    EffectId::new(format!(
        "effect.source-edit-reconciliation.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(application_contract_error)
}

fn persist_record<T: Serialize>(path: &Path, kind: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| config_error(error.to_string()))?;
    if bytes.len() > MAX_DURABLE_RECORD_BYTES {
        return Err(config_error("source edit durable record exceeds its bound"));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create source edit durable directory", error))?;
    }
    with_owned_temp_publish(
        path,
        kind,
        |temporary, destination| {
            crate::db::DatabaseAuthority::replace_file_atomically(
                temporary,
                destination,
                "source edit durable record",
            )
            .map_err(|error| std::io::Error::other(error.to_string()))
        },
        |output| output.write_all(&bytes),
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| io_error("persist source edit durable record", error))
}

fn load_record<T>(path: &Path, kind: &'static str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(bytes) =
        read_bounded(path, MAX_DURABLE_RECORD_BYTES).map_err(|error| io_error(kind, error))?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| config_error(format!("{kind} is malformed: {error}")))
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(i64::MAX as u128) as i64;
    UtcMicros(micros)
}

async fn run_edit_verification(graph: &TraceDecay, file_path: &str) -> SourceEditVerificationV1 {
    let scope = crate::diagnostics::Scope::File {
        path: file_path.to_owned(),
    };
    let diagnostics = match crate::diagnostics::run_all(graph.project_root(), &scope).await {
        Ok(diagnostics) => diagnostics,
        Err(error) => return failed_edit_verification(error),
    };
    let mut error_count = 0;
    let mut warning_count = 0;
    let mut first_errors = Vec::new();
    for diagnostic in diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.file == file_path)
    {
        match diagnostic.level.as_str() {
            "error" => {
                error_count += 1;
                if first_errors.len() < 3 {
                    first_errors.push(SourceEditDiagnosticV1 {
                        line: diagnostic.line_start,
                        code: diagnostic.code,
                        message: diagnostic.message,
                    });
                }
            }
            "warning" => warning_count += 1,
            _ => {}
        }
    }
    let (state, verdict) = if error_count == 0 {
        (SourceEditVerificationStateV1::Clean, "clean")
    } else {
        (SourceEditVerificationStateV1::Errors, "errors")
    };
    SourceEditVerificationV1 {
        state,
        verdict: verdict.to_owned(),
        error_count,
        warning_count,
        first_errors,
        message: None,
    }
}

async fn run_edit_verifications(
    graph: &TraceDecay,
    file_paths: &[String],
) -> SourceEditVerificationV1 {
    let mut aggregate = SourceEditVerificationV1 {
        state: SourceEditVerificationStateV1::Clean,
        verdict: "clean".to_owned(),
        error_count: 0,
        warning_count: 0,
        first_errors: Vec::new(),
        message: None,
    };
    for file_path in file_paths {
        let result = run_edit_verification(graph, file_path).await;
        aggregate.error_count += result.error_count;
        aggregate.warning_count += result.warning_count;
        for error in result.first_errors {
            if aggregate.first_errors.len() < 3 {
                aggregate.first_errors.push(error);
            }
        }
        if verification_priority(result.state) > verification_priority(aggregate.state) {
            aggregate.state = result.state;
            aggregate.verdict = result.verdict;
            aggregate.message = result.message;
        }
    }
    aggregate
}

const fn verification_priority(state: SourceEditVerificationStateV1) -> u8 {
    match state {
        SourceEditVerificationStateV1::Clean => 0,
        SourceEditVerificationStateV1::Unavailable => 1,
        SourceEditVerificationStateV1::Errors => 2,
        SourceEditVerificationStateV1::Cancelled => 3,
        SourceEditVerificationStateV1::Failed => 4,
    }
}

fn failed_edit_verification(error: TraceDecayError) -> SourceEditVerificationV1 {
    let (state, verdict) = match &error {
        TraceDecayError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            (SourceEditVerificationStateV1::Cancelled, "cancelled")
        }
        TraceDecayError::Config { message }
            if message.to_ascii_lowercase().contains("unavailable") =>
        {
            (SourceEditVerificationStateV1::Unavailable, "unavailable")
        }
        _ => (SourceEditVerificationStateV1::Failed, "failed"),
    };
    SourceEditVerificationV1 {
        state,
        verdict: verdict.to_owned(),
        error_count: 0,
        warning_count: 0,
        first_errors: Vec::new(),
        message: Some(error.to_string().chars().take(1024).collect()),
    }
}

fn application_contract_error(
    error: tracedecay_application::ApplicationContractError,
) -> TraceDecayError {
    config_error(format!(
        "source edit application contract is invalid: {error}"
    ))
}

fn application_problem(_error: tracedecay_application::ApplicationProblem) -> TraceDecayError {
    config_error("source edit was not found or not authorized")
}

fn domain_error(error: tracedecay_domain::DomainError) -> TraceDecayError {
    config_error(format!("source edit durable identity is invalid: {error}"))
}

fn io_error(operation: &'static str, error: std::io::Error) -> TraceDecayError {
    config_error(format!("{operation}: {error}"))
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::ops::Deref;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::{TempDir, tempdir};
    use tracedecay_application::{
        ApiCompatibilityDispositionV1, ApiCompatibilityLifetimeV1, ApiDefinitionInsertionV1,
        ApiMigrationOperationRequestV1, ApiMigrationSiteDispositionV1, ApiMigrationSymbolV1,
        AuthorityReceipt, CancellationContext, CancellationSignal, CancellationStage,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, IdempotencyKey, PolicyDecisionRef,
        RequestContext, RequestId, ResolvedScope, SourceEditAuthorizationFuture,
        SourceEditEffectProofV1, SourceEditKind, api_migration_definition_digest,
    };
    use tracedecay_domain::{ActorId, ComponentVersion, ProjectId, RepositoryId, WorktreeId};

    use super::*;
    use crate::tracedecay::TraceDecayOpenOptions;
    use crate::types::MoveHint;

    const SHA256_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA256_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    struct FixtureGraph {
        graph: TraceDecay,
        _database_scope: crate::db::DaemonDatabaseScope,
    }

    impl Deref for FixtureGraph {
        type Target = TraceDecay;

        fn deref(&self) -> &Self::Target {
            &self.graph
        }
    }

    async fn fixture_graph(project_root: &Path) -> FixtureGraph {
        let profile_root = project_root.join(".tracedecay-test-profile");
        let open_options = TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        };
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root).unwrap();
        let database_scope = crate::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "source-edit-test-runtime",
        )
        .unwrap();
        let runtime_registry = Arc::new(
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await
            .unwrap(),
        );
        let profile_database = runtime_registry.profile_database().await.unwrap();
        let store_layout = TraceDecay::resolve_first_touch_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
            true,
        )
        .await
        .unwrap();
        let project_id = ProjectId::new(
            store_layout
                .identity
                .project_id
                .clone()
                .expect("fixture layout has a project identity"),
        )
        .unwrap();
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let configuration_database = runtime_registry
            .project_sessions(
                project_id,
                [
                    project_root.to_path_buf(),
                    store_layout.project_root.clone(),
                ],
            )
            .await
            .unwrap();
        FixtureGraph {
            graph: TraceDecay::init_with_registered_configuration(
                project_root,
                open_options,
                store_layout,
                configuration_database,
                profile_database,
                runtime_registry,
            )
            .await
            .unwrap(),
            _database_scope: database_scope,
        }
    }

    fn git(project_root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(project_root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} must succeed");
    }

    async fn indexed_api_migration_fixture(initial_source: &str) -> (TempDir, FixtureGraph) {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), initial_source).unwrap();
        git(
            project.path(),
            &["init", "--quiet", "--initial-branch=main"],
        );
        git(project.path(), &["add", "src/lib.rs"]);
        git(
            project.path(),
            &[
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
        let graph = fixture_graph(project.path()).await;
        let indexed = graph.index_all().await.unwrap();
        assert!(indexed.node_count > 0);
        (project, graph)
    }

    async fn api_migration_symbol(graph: &TraceDecay, name: &str) -> ApiMigrationSymbolV1 {
        let node = graph
            .get_nodes_by_name(name)
            .await
            .unwrap()
            .into_iter()
            .find(|node| node.file_path == "src/lib.rs")
            .unwrap_or_else(|| panic!("indexed fixture symbol {name}"));
        ApiMigrationSymbolV1 {
            node_id: node.id,
            qualified_name: node.qualified_name,
            kind: node.kind.as_str().to_owned(),
            file: node.file_path,
            old_name: node.name,
        }
    }

    async fn plan_api_migration_fixture(
        graph: &TraceDecay,
        family_id: &str,
        operation: ApiMigrationOperationRequestV1,
    ) -> ApiMigrationPlanV1 {
        crate::application::api_migration::plan_api_migration(
            graph,
            ApiMigrationPlanRequestV1 {
                family_id: family_id.to_owned(),
                operations: vec![operation],
            },
        )
        .await
        .unwrap()
    }

    async fn apply_api_migration_fixture(
        graph: &TraceDecay,
        plan: ApiMigrationPlanV1,
    ) -> ApiMigrationApplyResultV1 {
        let plan_digest = plan.plan_digest.clone();
        let outcome = run_source_edit(
            graph,
            SourceEditRequest::ApiMigrationApply {
                plan,
                plan_digest,
                dry_run: false,
                verify: false,
            },
            None,
        )
        .await
        .unwrap();
        match outcome {
            SourceEditOutcome::ApiMigration(result) => result,
            unexpected => panic!("unexpected API migration outcome: {unexpected:?}"),
        }
    }

    #[tokio::test]
    async fn api_migration_promote_primary_plans_and_applies_the_replacement_definition() {
        let initial = "pub fn legacy_api() -> &'static str {\n    \"legacy\"\n}\n";
        let expected = "pub fn primary_api() -> &'static str {\n    \"primary\"\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::PromotePrimary {
            operation_id: "promote-primary".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "legacy_api").await,
            expected_definition_digest: api_migration_definition_digest(initial).unwrap(),
            replacement_definition: expected.to_owned(),
        };

        let plan = plan_api_migration_fixture(&graph, "family.promote-primary", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(plan.sites[0].reason, "whole definition replacement");
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(result.changed_files, ["src/lib.rs"]);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_replace_definition_replans_rejects_stale_bytes_then_applies_current_plan()
     {
        let initial = "pub fn current_value() -> i32 {\n    1\n}\n";
        let concurrent = "pub fn current_value() -> i32 {\n    2\n}\n";
        let expected = "pub fn current_value() -> i32 {\n    3\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::ReplaceDefinition {
            operation_id: "replace-definition".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "current_value").await,
            expected_definition_digest: api_migration_definition_digest(initial).unwrap(),
            replacement_definition: expected.to_owned(),
        };
        let plan = plan_api_migration_fixture(&graph, "family.replace-definition", operation).await;
        assert_eq!(plan.files[0].intended_content, expected);

        fs::write(project.path().join("src/lib.rs"), concurrent).unwrap();
        let stale_error = run_source_edit(
            &graph,
            SourceEditRequest::ApiMigrationApply {
                plan: plan.clone(),
                plan_digest: plan.plan_digest.clone(),
                dry_run: false,
                verify: false,
            },
            None,
        )
        .await
        .unwrap_err();

        assert!(
            stale_error
                .to_string()
                .contains("plan does not match current graph-backed evidence; replan before apply")
        );
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            concurrent
        );
        fs::write(project.path().join("src/lib.rs"), initial).unwrap();
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_rename_bound_symbol_plans_and_applies_declaration_and_caller_sites() {
        let initial = "pub fn legacy_name() -> i32 {\n    1\n}\n\npub fn caller() -> i32 {\n    legacy_name()\n}\n";
        let expected = "pub fn current_name() -> i32 {\n    1\n}\n\npub fn caller() -> i32 {\n    current_name()\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::RenameBoundSymbol {
            operation_id: "rename-bound-symbol".to_owned(),
            depends_on: Vec::new(),
            symbol: api_migration_symbol(&graph, "legacy_name").await,
            new_name: "current_name".to_owned(),
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.rename-bound-symbol", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 2);
        assert!(
            plan.sites
                .iter()
                .all(|site| { site.disposition == ApiMigrationSiteDispositionV1::Changed })
        );
        assert!(
            plan.sites
                .iter()
                .any(|site| site.reason == "bound declaration rename")
        );
        assert!(
            plan.sites
                .iter()
                .any(|site| site.reason == "graph-bound caller rename")
        );
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 2);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_insert_compatibility_plans_and_applies_the_definition_after_its_anchor()
    {
        let initial = "pub fn current_api() -> i32 {\n    7\n}\n";
        let compatibility = "#[deprecated]\npub fn legacy_api() -> i32 {\n    current_api()\n}";
        let expected = format!("{initial}\n{compatibility}");
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::InsertCompatibility {
            operation_id: "insert-compatibility".to_owned(),
            depends_on: Vec::new(),
            anchor: api_migration_symbol(&graph, "current_api").await,
            position: ApiDefinitionInsertionV1::After,
            definition: compatibility.to_owned(),
            disposition: ApiCompatibilityDispositionV1 {
                lifetime: ApiCompatibilityLifetimeV1::StablePublicContract,
                external_consumer: "fixture consumer".to_owned(),
                owner: "fixture API team".to_owned(),
                deprecation_policy: "retained as a stable compatibility alias".to_owned(),
                pr19_deletion_condition: None,
            },
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.insert-compatibility", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(plan.sites[0].reason, "deliberate compatibility definition");
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.compatibility_sites, 1);
        assert_eq!(result.changed_sites, 1);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_replace_selected_terminology_plans_and_applies_only_selected_ast_occurrences()
     {
        let initial = "pub fn terminology() -> i32 {\n    let legacy = 1;\n    legacy\n}\n";
        let expected = "pub fn terminology() -> i32 {\n    let current = 1;\n    current\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
            operation_id: "replace-selected-terminology".to_owned(),
            depends_on: Vec::new(),
            enclosing_symbol: api_migration_symbol(&graph, "terminology").await,
            old_term: "legacy".to_owned(),
            new_term: "current".to_owned(),
            occurrence_indexes: vec![0, 1],
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.replace-selected-terminology", operation)
                .await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 2);
        assert!(plan.sites.iter().all(|site| {
            site.reason == "selected AST terminology replacement"
                && site.disposition == ApiMigrationSiteDispositionV1::Changed
        }));
        assert_eq!(plan.files[0].intended_content, expected);
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 2);
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn api_migration_assert_stable_value_plans_and_applies_a_byte_identical_protected_site() {
        let initial = "pub fn stable_value() -> i32 {\n    42\n}\n";
        let (project, graph) = indexed_api_migration_fixture(initial).await;
        let operation = ApiMigrationOperationRequestV1::AssertStableValue {
            operation_id: "assert-stable-value".to_owned(),
            depends_on: Vec::new(),
            enclosing_symbol: api_migration_symbol(&graph, "stable_value").await,
            category: "wire discriminant".to_owned(),
            exact_bytes: "42".to_owned(),
            occurrence_indexes: vec![0],
        };

        let plan =
            plan_api_migration_fixture(&graph, "family.assert-stable-value", operation).await;

        assert!(!plan.blocked);
        assert_eq!(plan.sites.len(), 1);
        assert_eq!(
            plan.sites[0].disposition,
            ApiMigrationSiteDispositionV1::Unchanged
        );
        assert_eq!(
            plan.sites[0].reason,
            "protected wire discriminant remains byte-identical"
        );
        assert_eq!(
            plan.files[0].expected_content,
            plan.files[0].intended_content
        );
        let result = apply_api_migration_fixture(&graph, plan).await;
        assert!(result.success);
        assert_eq!(result.changed_sites, 0);
        assert_eq!(result.protected_values_verified, 1);
        assert!(result.changed_files.is_empty());
        assert_eq!(
            fs::read_to_string(project.path().join("src/lib.rs")).unwrap(),
            initial
        );
    }

    fn fixture_request() -> SourceEditEffectRequestV1 {
        let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
        let reconciliation_operation = source_edit_reconciliation_operation().unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.edit.fixture").unwrap(),
            RepositoryId::new("repository.edit.fixture").unwrap(),
            WorktreeId::new("worktree.edit.fixture").unwrap(),
            None,
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            tracedecay_application::CapabilityGrantId::new("grant.edit.fixture").unwrap(),
            1,
            digest(SHA256_A),
            ActorId::new("actor.edit.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                operation.capability_id().clone(),
                reconciliation_operation.capability_id().clone(),
            ]),
            BTreeSet::from([
                operation.use_case_id().clone(),
                reconciliation_operation.use_case_id().clone(),
            ]),
            DisclosureClass::Sensitive,
        )
        .unwrap();
        let context = RequestContext::new(
            ActorId::new("actor.edit.requester").unwrap(),
            scope,
            grant,
            RequestId::new("request.edit.fixture").unwrap(),
            Deadline::new(UtcMicros(900)).unwrap(),
            CancellationContext::active("cancel.edit.fixture").unwrap(),
        )
        .unwrap();
        let authority = AuthorityReceipt::from_context(
            &context,
            PolicyDecisionRef::new(
                "policy.edit.fixture",
                1,
                digest(SHA256_B),
                ComponentVersion::new("policy.edit.v1").unwrap(),
            )
            .unwrap(),
            UtcMicros(2),
        )
        .unwrap();
        SourceEditEffectRequestV1 {
            context,
            authority,
            edit: SourceEditRequest::StrReplace {
                path: "src/lib.rs".to_owned(),
                old_str: "old".to_owned(),
                new_str: "new".to_owned(),
                dry_run: false,
                verify: false,
            },
            idempotency_key: IdempotencyKey::new("source-edit.fixture").unwrap(),
            expected_state: digest(SHA256_A),
            proof: SourceEditEffectProofV1 {
                policy_digest: digest(SHA256_B),
                configuration_revision_id:
                    tracedecay_domain::configuration::ConfigurationRevisionId::new(
                        "configuration.edit.fixture.v1",
                    )
                    .unwrap(),
                configuration_digest: digest(SHA256_A),
                catalog_revision: 1,
                catalog_digest: digest(SHA256_A),
                privacy_domain_id: tracedecay_domain::PrivacyDomainId::new("privacy.edit.fixture")
                    .unwrap(),
                privacy_key_epoch: 1,
                privacy_digest: digest(SHA256_A),
                external_proof: None,
            },
            observed_at: UtcMicros(3),
        }
    }

    fn fixture_journal(
        request: &SourceEditEffectRequestV1,
        state: SourceEditJournalStateV1,
    ) -> SourceEditJournalV1 {
        let operation = source_edit_operation(request.edit.kind()).unwrap();
        let input_digest = request.input_digest().unwrap();
        SourceEditJournalV1 {
            version: JOURNAL_VERSION,
            effect_id: effect_id(&request.idempotency_key, &input_digest).unwrap(),
            input_digest,
            expected_state: request.expected_state.clone(),
            predicted_state: None,
            candidate_files: vec!["src/lib.rs".to_owned()],
            request: SourceEditDurableRequestV1 {
                operation: operation.use_case_id().clone(),
                request_id: request.context.request_id().clone(),
                actor: request.context.actor().clone(),
                scope: request.context.scope().clone(),
                authority: request.authority.clone(),
                authority_proof: request.proof.clone(),
                idempotency_key: request.idempotency_key.clone(),
                deadline: request.context.deadline().clone(),
                started_at: request.observed_at,
                dry_run: request.edit.dry_run(),
                verification_requested: request.edit.verify(),
            },
            state,
        }
    }

    fn fixture_reconciliation(
        request: &SourceEditEffectRequestV1,
        journal: &SourceEditJournalV1,
        disposition: SourceEditReconciliationDispositionV1,
    ) -> SourceEditReconciliationRequestV1 {
        SourceEditReconciliationRequestV1 {
            context: request.context.clone(),
            authority: request.authority.clone(),
            kind: request.edit.kind(),
            effect_id: journal.effect_id.clone(),
            idempotency_key: request.idempotency_key.clone(),
            attempt_idempotency_key: tracedecay_application::IdempotencyKey::new(
                "source-edit-reconciliation-attempt.fixture",
            )
            .unwrap(),
            input_digest: journal.input_digest.clone(),
            disposition,
            proof: request.proof.clone(),
            observed_at: UtcMicros(4),
        }
    }

    #[derive(Clone)]
    struct FixtureSourceEditAuthorization(
        tracedecay_application::SourceEditAuthorizationAdmissionV1,
    );

    fn fixture_authorization(
        request: &SourceEditEffectRequestV1,
    ) -> FixtureSourceEditAuthorization {
        FixtureSourceEditAuthorization(
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                request.authority.clone(),
                request.proof.clone(),
                request.context.scope(),
            )
            .unwrap(),
        )
    }

    impl SourceEditAuthorizationPort for FixtureSourceEditAuthorization {
        fn admit<'a>(
            &'a self,
            _context: &'a RequestContext,
            _operation: &'a ApplicationOperation,
            _observed_at: UtcMicros,
        ) -> SourceEditAuthorizationFuture<'a> {
            Box::pin(async move { Ok(self.0.clone()) })
        }

        fn recheck_effect<'a>(
            &'a self,
            _context: &'a RequestContext,
            _operation: &'a ApplicationOperation,
            _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
            _observed_at: UtcMicros,
        ) -> SourceEditAuthorizationFuture<'a> {
            Box::pin(async move { Ok(self.0.clone()) })
        }
    }

    struct CancelBeforeEffectAuthorization {
        admission: tracedecay_application::SourceEditAuthorizationAdmissionV1,
        cancellation: CancellationSignal,
        rechecks: AtomicUsize,
    }

    impl SourceEditAuthorizationPort for CancelBeforeEffectAuthorization {
        fn admit<'a>(
            &'a self,
            _context: &'a RequestContext,
            _operation: &'a ApplicationOperation,
            _observed_at: UtcMicros,
        ) -> SourceEditAuthorizationFuture<'a> {
            Box::pin(async move { Ok(self.admission.clone()) })
        }

        fn recheck_effect<'a>(
            &'a self,
            _context: &'a RequestContext,
            _operation: &'a ApplicationOperation,
            _admission: &'a tracedecay_application::SourceEditAuthorizationAdmissionV1,
            _observed_at: UtcMicros,
        ) -> SourceEditAuthorizationFuture<'a> {
            Box::pin(async move {
                if self.rechecks.fetch_add(1, Ordering::AcqRel) == 1 {
                    assert!(self.cancellation.cancel(UtcMicros(4)));
                }
                Ok(self.admission.clone())
            })
        }
    }

    #[tokio::test]
    async fn preview_apply_replay_and_expected_state_cas_preserve_exact_bytes() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        let initial = b"old\r\nunchanged \xE2\x98\x83\n";
        let applied = b"new\r\nunchanged \xE2\x98\x83\n";
        fs::write(project.path().join("src/lib.rs"), initial).unwrap();
        let graph = fixture_graph(project.path()).await;
        let operation = source_edit_operation(SourceEditKind::StrReplace).unwrap();
        let request = fixture_request();
        let authorization = fixture_authorization(&request);

        let mut preview_request = request.clone();
        preview_request.edit = preview_request.edit.clone().with_dry_run(true);
        let preview = execute_source_edit(&graph, &operation, preview_request, &authorization)
            .await
            .unwrap();
        assert!(preview.dry_run);
        assert!(preview.outcome.success());
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            initial
        );

        let mut apply_request = request;
        apply_request.idempotency_key = IdempotencyKey::new("source-edit.apply-fixture").unwrap();
        apply_request.expected_state = preview.expected_state.clone();
        let applied_result =
            execute_source_edit(&graph, &operation, apply_request.clone(), &authorization)
                .await
                .unwrap();
        assert!(applied_result.outcome.success());
        assert!(!applied_result.replayed);
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            applied
        );

        let replay = execute_source_edit(&graph, &operation, apply_request, &authorization)
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            applied
        );

        fs::write(project.path().join("src/lib.rs"), initial).unwrap();
        let expected_state = preview_source_edit_expected_state(&graph, fixture_request().edit)
            .await
            .unwrap();
        fs::write(
            project.path().join("src/lib.rs"),
            b"old\r\nconcurrent change\n",
        )
        .unwrap();
        let mut stale_request = fixture_request();
        stale_request.idempotency_key = IdempotencyKey::new("source-edit.stale-fixture").unwrap();
        stale_request.expected_state = expected_state;
        let stale = execute_source_edit(&graph, &operation, stale_request, &authorization)
            .await
            .unwrap();
        assert_eq!(
            stale.effect.unwrap().receipt.outcome,
            EffectTermination::Failed
        );
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            b"old\r\nconcurrent change\n"
        );
    }

    #[tokio::test]
    async fn dry_run_cancellation_before_admission_skips_preview() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"old").unwrap();
        let graph = fixture_graph(project.path()).await;
        let mut request = fixture_request();
        request.edit = request.edit.clone().with_dry_run(true);
        let operation = source_edit_operation(request.edit.kind()).unwrap();
        let authorization = fixture_authorization(&request);
        let cancellation = CancellationSignal::active("cancel.edit.preview").unwrap();
        assert!(cancellation.cancel(UtcMicros(4)));
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            cancellation,
        );

        let result =
            execute_source_edit_with_control(&graph, &operation, request, &authorization, &control)
                .await
                .unwrap();
        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::Cancelled
        );
        assert_eq!(fs::read(project.path().join("src/lib.rs")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn live_cancellation_before_effect_keeps_source_unchanged_and_is_durable() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"old").unwrap();
        let graph = fixture_graph(project.path()).await;
        let mut request = fixture_request();
        request.expected_state = preview_source_edit_expected_state(&graph, request.edit.clone())
            .await
            .unwrap();
        let operation = source_edit_operation(request.edit.kind()).unwrap();
        let cancellation = CancellationSignal::active("cancel.edit.live").unwrap();
        let authorization = CancelBeforeEffectAuthorization {
            admission: fixture_authorization(&request).0,
            cancellation: cancellation.clone(),
            rechecks: AtomicUsize::new(0),
        };
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            cancellation,
        );

        let result =
            execute_source_edit_with_control(&graph, &operation, request, &authorization, &control)
                .await
                .unwrap();
        let effect = result.effect.unwrap();

        assert_eq!(fs::read(project.path().join("src/lib.rs")).unwrap(), b"old");
        assert_eq!(effect.receipt.outcome, EffectTermination::Cancelled);
        assert_eq!(
            effect.execution.cancellation.unwrap().stage,
            CancellationStage::BeforeEffect
        );
    }

    #[test]
    fn committed_record_keeps_after_commit_cancellation_without_downgrade() {
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: None,
            },
        );
        let observation = tracedecay_application::CancellationObservation {
            stage: CancellationStage::AfterCommit,
            observed_at: UtcMicros(5),
        };

        let record = applied_record(
            &journal,
            &outcome,
            digest(SHA256_B),
            UtcMicros(5),
            Some(observation.clone()),
        )
        .unwrap();

        assert_eq!(record.effect.receipt.outcome, EffectTermination::Completed);
        assert_eq!(
            record.effect.execution.termination,
            OperationTermination::Completed
        );
        assert_eq!(record.effect.execution.cancellation, Some(observation));
    }

    #[test]
    fn verification_failures_are_typed_and_retained() {
        let unavailable = failed_edit_verification(TraceDecayError::Config {
            message: "diagnostics unavailable".to_owned(),
        });
        assert_eq!(
            unavailable.state,
            SourceEditVerificationStateV1::Unavailable
        );
        assert!(unavailable.message.is_some());

        let cancelled = failed_edit_verification(TraceDecayError::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "diagnostics cancelled",
        )));
        assert_eq!(cancelled.state, SourceEditVerificationStateV1::Cancelled);
        assert!(cancelled.message.is_some());

        let failed = failed_edit_verification(TraceDecayError::Config {
            message: "diagnostics failed".to_owned(),
        });
        assert_eq!(failed.state, SourceEditVerificationStateV1::Failed);
        assert!(failed.message.is_some());
    }

    #[test]
    fn requested_incomplete_verification_makes_committed_effect_partial() {
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let mut journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: Some(SourceEditVerificationStateV1::Failed),
            },
        );
        journal.request.verification_requested = true;

        let record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();

        assert_eq!(record.effect.receipt.outcome, EffectTermination::Partial);
        assert_eq!(
            record.effect.execution.termination,
            OperationTermination::Partial
        );
    }

    #[test]
    fn prepared_restart_is_durable_effect_unknown_and_not_replayed() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();

        let result = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::EffectUnknown
        );
        assert!(
            durability.load_journal().unwrap().is_some(),
            "an unknown prepared effect must retain its recovery evidence"
        );
    }

    #[test]
    fn applied_restart_finalizes_original_receipt_and_clears_journal() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: None,
            },
        );
        durability.persist_journal(&journal).unwrap();

        let result = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::Completed
        );
        assert!(durability.load_journal().unwrap().is_none());
        assert!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn replay_rejects_authority_drift_and_same_key_changed_input() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(
            &request,
            SourceEditJournalStateV1::Applied {
                outcome: SourceEditDurableOutcomeV1::from_live(
                    source_edit_operation(request.edit.kind())
                        .unwrap()
                        .use_case_id(),
                    &outcome,
                ),
                committed_state: digest(SHA256_B),
                ended_at: UtcMicros(4),
                control_observation: None,
                verification_state: None,
            },
        );
        let record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
        durability.persist_receipt(&record).unwrap();

        let replay = recover_or_replay(&durability, &request, &request.input_digest().unwrap())
            .unwrap()
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            replay.effect.unwrap().receipt.outcome,
            EffectTermination::Completed
        );

        let mut current_proof = request.clone();
        current_proof.proof.configuration_digest = digest(SHA256_B);
        current_proof.proof.configuration_revision_id =
            tracedecay_domain::configuration::ConfigurationRevisionId::new(
                "configuration.edit.fixture.v2",
            )
            .unwrap();
        current_proof.proof.catalog_revision = 2;
        current_proof.proof.catalog_digest = digest(SHA256_B);
        current_proof.proof.privacy_key_epoch = 2;
        current_proof.proof.privacy_digest = digest(SHA256_B);
        assert_eq!(
            current_proof.input_digest().unwrap(),
            request.input_digest().unwrap()
        );
        assert!(
            recover_or_replay(
                &durability,
                &current_proof,
                &current_proof.input_digest().unwrap(),
            )
            .is_err()
        );

        let mut conflict = request.clone();
        conflict.expected_state = digest(SHA256_B);
        assert!(
            recover_or_replay(&durability, &conflict, &conflict.input_digest().unwrap()).is_err()
        );
    }

    #[test]
    fn durable_receipt_rejects_unknown_version() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let outcome = SourceEditOutcome::Edit(EditResult {
            success: true,
            file_path: "src/lib.rs".to_owned(),
            message: "applied".to_owned(),
            ..EditResult::default()
        });
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        let mut record =
            applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
        record.version = JOURNAL_VERSION + 1;
        persist_record(
            &durability.receipt_path(&request.idempotency_key).unwrap(),
            "source-edit-receipt",
            &record,
        )
        .unwrap();

        assert!(durability.load_receipt(&request.idempotency_key).is_err());
    }

    #[test]
    fn cancelled_reconciliation_attempt_is_separate_replayable_and_conflict_safe() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();
        durability
            .persist_receipt(&unknown_record(&journal).unwrap())
            .unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        );
        let attempt_input = digest(SHA256_B);
        let operation = source_edit_reconciliation_operation().unwrap();
        let cancellation = CancellationSignal::active("cancel.reconcile.fixture").unwrap();
        assert!(cancellation.cancel(UtcMicros(5)));
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            cancellation,
        );
        let reconciliation_authority =
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                reconciliation.authority.clone(),
                reconciliation.proof.clone(),
                reconciliation.context.scope(),
            )
            .unwrap();
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &operation,
            authority: &reconciliation_authority,
            input_digest: &attempt_input,
            control: Some(&control),
        };
        let result = reconcile_prepared_source_edit_controlled(
            &durability,
            directory.path(),
            &source_edit_operation(request.edit.kind()).unwrap(),
            reconciliation.clone(),
            Some(attempt),
        )
        .unwrap();

        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::Cancelled
        );
        assert_eq!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .unwrap()
                .effect
                .receipt
                .outcome,
            EffectTermination::EffectUnknown
        );
        assert!(durability.load_journal().unwrap().is_some());
        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &attempt_input)
                .unwrap()
                .unwrap()
                .replayed
        );
        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &digest(SHA256_A))
                .is_err()
        );
    }

    #[tokio::test]
    async fn reconciliation_before_admission_cancellation_is_durable_and_replayable() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"unchanged").unwrap();
        let graph = fixture_graph(project.path()).await;
        let durability = SourceEditDurability::for_graph(&graph);
        let files = vec!["src/lib.rs".to_owned()];
        let mut request = fixture_request();
        request.expected_state = source_edit_state_digest(project.path(), &files).unwrap();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.candidate_files = files;
        durability.persist_journal(&journal).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        );
        let authorization = fixture_authorization(&request);
        let cancellation = CancellationSignal::active("cancel.reconcile.before-admission").unwrap();
        assert!(cancellation.cancel(UtcMicros(5)));
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            cancellation,
        );

        let result = reconcile_source_edit_effect_unknown_with_control(
            &graph,
            reconciliation.clone(),
            &authorization,
            &control,
        )
        .await
        .unwrap();
        assert_eq!(
            result.effect.as_ref().unwrap().receipt.outcome,
            EffectTermination::Cancelled
        );
        assert_eq!(
            result
                .effect
                .as_ref()
                .unwrap()
                .execution
                .cancellation
                .as_ref()
                .unwrap()
                .stage,
            CancellationStage::BeforeAdmission
        );
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            b"unchanged"
        );
        assert!(durability.load_journal().unwrap().is_some());

        let replay = reconcile_source_edit_effect_unknown_with_control(
            &graph,
            reconciliation,
            &authorization,
            &control,
        )
        .await
        .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            fs::read(project.path().join("src/lib.rs")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn completed_reconciliation_attempt_replay_clears_prepared_journal() {
        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let request = fixture_request();
        let journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        durability.persist_journal(&journal).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmRolledBack,
        );
        let original_outcome = SourceEditOutcome::Reconciled {
            success: false,
            message: "rolled back".to_owned(),
        };
        let original = durable_record(
            &journal,
            SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &original_outcome),
            None,
            UtcMicros(5),
            EffectTermination::Failed,
            ReconciliationState::Reconciled,
            None,
        )
        .unwrap();
        durability.persist_receipt(&original).unwrap();
        let attempt_input = digest(SHA256_B);
        let operation = source_edit_reconciliation_operation().unwrap();
        let reconciliation_authority =
            tracedecay_application::SourceEditAuthorizationAdmissionV1::new(
                reconciliation.authority.clone(),
                reconciliation.proof.clone(),
                reconciliation.context.scope(),
            )
            .unwrap();
        let attempt = SourceEditReconciliationAttemptV1 {
            operation: &operation,
            authority: &reconciliation_authority,
            input_digest: &attempt_input,
            control: None,
        };
        let attempt_outcome = SourceEditOutcome::Reconciled {
            success: true,
            message: "completed".to_owned(),
        };
        let completed = reconciliation_attempt_record(
            &journal,
            &reconciliation,
            &attempt,
            &attempt_outcome,
            None,
            UtcMicros(6),
            EffectTermination::Completed,
            None,
        )
        .unwrap();
        durability
            .persist_reconciliation_receipt(&completed)
            .unwrap();

        assert!(
            recover_reconciliation_attempt(&durability, &reconciliation, &attempt_input)
                .unwrap()
                .unwrap()
                .replayed
        );
        assert!(durability.load_journal().unwrap().is_none());
    }

    #[test]
    fn durable_journal_and_receipt_never_retain_edit_bodies() {
        const SENTINEL: &str = "SOURCE_EDIT_BODY_MUST_NOT_PERSIST_7b6398";

        let directory = tempdir().unwrap();
        let durability = SourceEditDurability {
            root: directory.path().to_path_buf(),
        };
        let mut request = fixture_request();
        request.edit = SourceEditRequest::StrReplace {
            path: "src/lib.rs".to_owned(),
            old_str: SENTINEL.to_owned(),
            new_str: SENTINEL.to_owned(),
            dry_run: false,
            verify: true,
        };
        let outcomes = vec![
            SourceEditOutcome::Edit(EditResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                matched_str: SENTINEL.to_owned(),
                new_str: SENTINEL.to_owned(),
                replaced_span: Some(SENTINEL.to_owned()),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..EditResult::default()
            }),
            SourceEditOutcome::MultiEdit(MultiEditResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                applied_count: 2,
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..MultiEditResult::default()
            }),
            SourceEditOutcome::Insert(InsertResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                anchor_line: 7,
                content: SENTINEL.to_owned(),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..InsertResult::default()
            }),
            SourceEditOutcome::AstGrep(AstGrepResult {
                success: true,
                file_path: "src/lib.rs".to_owned(),
                pattern: SENTINEL.to_owned(),
                rewrite: SENTINEL.to_owned(),
                diff: Some(SENTINEL.to_owned()),
                message: SENTINEL.to_owned(),
                ..AstGrepResult::default()
            }),
            SourceEditOutcome::Move(MoveResult {
                success: true,
                symbol: "fixture_symbol".to_owned(),
                source_file: "src/lib.rs".to_owned(),
                dest_file: "src/moved.rs".to_owned(),
                moved_span: Some(SENTINEL.to_owned()),
                diff: Some(SENTINEL.to_owned()),
                applied_imports: vec![SENTINEL.to_owned()],
                impact: vec![MoveHint {
                    kind: "dependency_broken".to_owned(),
                    file: "src/lib.rs".to_owned(),
                    line: Some(7),
                    detail: SENTINEL.to_owned(),
                    suggestion: Some(SENTINEL.to_owned()),
                }],
                message: SENTINEL.to_owned(),
                ..MoveResult::default()
            }),
        ];
        let verification = SourceEditVerificationV1 {
            state: SourceEditVerificationStateV1::Errors,
            verdict: "errors".to_owned(),
            error_count: 1,
            warning_count: 0,
            first_errors: vec![SourceEditDiagnosticV1 {
                line: 7,
                code: "fixture".to_owned(),
                message: SENTINEL.to_owned(),
            }],
            message: None,
        };
        let operation = source_edit_operation(request.edit.kind()).unwrap();

        for outcome in outcomes {
            let journal = fixture_journal(
                &request,
                SourceEditJournalStateV1::Applied {
                    outcome: SourceEditDurableOutcomeV1::from_live(
                        operation.use_case_id(),
                        &outcome,
                    ),
                    committed_state: digest(SHA256_B),
                    ended_at: UtcMicros(4),
                    control_observation: None,
                    verification_state: None,
                },
            );
            durability.persist_journal(&journal).unwrap();
            let journal_json = fs::read_to_string(durability.journal_path()).unwrap();
            assert!(!journal_json.contains(SENTINEL));

            let record =
                applied_record(&journal, &outcome, digest(SHA256_B), UtcMicros(4), None).unwrap();
            let live = record
                .clone()
                .into_live_application_result(outcome, Some(verification.clone()));
            assert!(live.value().to_string().contains(SENTINEL));

            durability.persist_receipt(&record).unwrap();
            let receipt_json =
                fs::read_to_string(durability.receipt_path(&request.idempotency_key).unwrap())
                    .unwrap();
            assert!(!receipt_json.contains(SENTINEL));
            for forbidden_key in [
                "matched_str",
                "new_str",
                "content",
                "pattern",
                "rewrite",
                "replaced_span",
                "moved_span",
                "diff",
                "applied_imports",
                "impact",
                "detail",
                "suggestion",
                "verification",
            ] {
                assert!(!journal_json.contains(&format!("\"{forbidden_key}\"")));
                assert!(!receipt_json.contains(&format!("\"{forbidden_key}\"")));
            }

            let replay = durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .unwrap()
                .into_application_result(true);
            let replay_value = replay.value();
            assert_eq!(replay_value["durable_metadata_only"], true);
            assert!(!replay_value.to_string().contains(SENTINEL));
        }
    }

    #[test]
    fn authorized_committed_reconciliation_replaces_unknown_and_unblocks_edits() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"before").unwrap();
        let durability = SourceEditDurability {
            root: project.path().join("durability"),
        };
        let files = vec!["src/lib.rs".to_owned()];
        let mut request = fixture_request();
        request.expected_state = source_edit_state_digest(project.path(), &files).unwrap();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.predicted_state = Some(
            planned_source_edit_state_digest(
                &files,
                &[crate::tracedecay::PlannedSourceEditFile {
                    relative_path: "src/lib.rs".to_owned(),
                    expected: Some("before".to_owned()),
                    intended: Some("after".to_owned()),
                }],
                true,
            )
            .unwrap(),
        );
        durability.persist_journal(&journal).unwrap();
        let unknown = reconcile_journal(
            &durability,
            durability.load_journal().unwrap().unwrap(),
            &request,
            &request.input_digest().unwrap(),
        )
        .unwrap();
        assert_eq!(
            unknown.effect.unwrap().receipt.outcome,
            EffectTermination::EffectUnknown
        );

        fs::write(project.path().join("src/lib.rs"), b"after").unwrap();
        let committed_state = source_edit_state_digest(project.path(), &files).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmCommitted {
                committed_state: committed_state.clone(),
            },
        );
        let operation = source_edit_operation(request.edit.kind()).unwrap();
        let resolved =
            reconcile_prepared_source_edit(&durability, project.path(), &operation, reconciliation)
                .unwrap();

        assert_eq!(resolved.predicted_state, Some(committed_state.clone()));
        assert_eq!(
            resolved.effect.unwrap().receipt.committed_state,
            Some(committed_state)
        );
        assert!(durability.load_journal().unwrap().is_none());
        assert!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn reconciliation_mismatch_retains_unknown_journal() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src")).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"before").unwrap();
        let durability = SourceEditDurability {
            root: project.path().join("durability"),
        };
        let files = vec!["src/lib.rs".to_owned()];
        let mut request = fixture_request();
        request.expected_state = source_edit_state_digest(project.path(), &files).unwrap();
        let mut journal = fixture_journal(&request, SourceEditJournalStateV1::Prepared);
        journal.predicted_state = Some(
            planned_source_edit_state_digest(
                &files,
                &[crate::tracedecay::PlannedSourceEditFile {
                    relative_path: "src/lib.rs".to_owned(),
                    expected: Some("before".to_owned()),
                    intended: Some("intended".to_owned()),
                }],
                true,
            )
            .unwrap(),
        );
        durability.persist_journal(&journal).unwrap();
        fs::write(project.path().join("src/lib.rs"), b"unrelated").unwrap();
        let unrelated_state = source_edit_state_digest(project.path(), &files).unwrap();
        let reconciliation = fixture_reconciliation(
            &request,
            &journal,
            SourceEditReconciliationDispositionV1::ConfirmCommitted {
                committed_state: unrelated_state,
            },
        );
        let operation = source_edit_operation(request.edit.kind()).unwrap();

        assert!(
            reconcile_prepared_source_edit(
                &durability,
                project.path(),
                &operation,
                reconciliation,
            )
            .is_err()
        );
        assert!(durability.load_journal().unwrap().is_some());
    }

    #[test]
    fn expected_state_digest_covers_content_and_missing_files() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("present.rs"), b"one").unwrap();
        let files = vec!["missing.rs".to_owned(), "present.rs".to_owned()];
        let before = source_edit_state_digest(directory.path(), &files).unwrap();

        fs::write(directory.path().join("present.rs"), b"two").unwrap();
        let after = source_edit_state_digest(directory.path(), &files).unwrap();

        assert_ne!(before, after);
    }

    #[test]
    fn api_migration_apply_requires_the_current_graph_backed_plan() {
        let supplied = ApiMigrationPlanV1 {
            family_id: "family".to_owned(),
            repository_revision: "revision".to_owned(),
            graph_revision: digest(SHA256_A),
            operations: Vec::new(),
            sites: Vec::new(),
            files: Vec::new(),
            blocked: false,
            plan_digest: digest(SHA256_B),
        };
        let mut replanned = supplied.clone();
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_ok());
        replanned.graph_revision = digest(SHA256_B);
        assert!(validate_replanned_api_migration(&supplied, &replanned).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn expected_state_digest_rejects_symlinked_candidate_parent() {
        use std::os::unix::fs::symlink;

        let project = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("lib.rs"), b"outside").unwrap();
        symlink(outside.path(), project.path().join("src")).unwrap();

        assert!(source_edit_state_digest(project.path(), &["src/lib.rs".to_owned()]).is_err());
        assert_eq!(fs::read(outside.path().join("lib.rs")).unwrap(), b"outside");
    }
}
