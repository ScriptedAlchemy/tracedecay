use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::{ApiMigrationApplyResultV1, EffectResult, SourceEditVerificationV1};
use tracedecay_domain::ManifestDigest;

use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult,
};

/// Body-free outcome metadata retained after the live edit response is returned.
///
/// Receipts intentionally keep no caller-supplied edit text, preview diff,
/// moved/replaced span, import text, diagnostic text, or impact detail. The
/// active transaction journal separately retains bounded exact preimages until
/// commit or rollback so restart recovery can restore the accepted workspace.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditDurableOutcomeV1 {
    pub(super) operation: tracedecay_tool_catalog::UseCaseId,
    pub(super) success: bool,
    pub(super) files: Vec<String>,
    pub(super) change_count: Option<usize>,
    pub(super) line: Option<u32>,
    pub(super) before: Option<bool>,
    pub(super) import_count: Option<usize>,
    pub(super) finding_count: Option<usize>,
    #[serde(default)]
    pub(super) failed: bool,
    pub(super) cancelled: bool,
    pub(super) timed_out: bool,
    pub(super) effect_unknown: bool,
    pub(super) reconciled: bool,
}

impl SourceEditDurableOutcomeV1 {
    pub(super) fn from_live(
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

    pub(super) fn value(&self) -> Value {
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

    pub(super) fn candidate_files(&self) -> Vec<String> {
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

    pub(super) fn to_value(&self) -> Value {
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
