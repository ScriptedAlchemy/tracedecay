use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;
use tracedecay_tool_catalog::UseCaseId;

use crate::result::EffectResult;

use super::{
    AstGrepResult, EditResult, InsertResult, MoveResult, MultiEditResult, RenameResult,
    SourceEditVerificationV1,
};

/// Terminal payload when an edit is refused before it can publish an effect.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditFailedResultV1 {
    pub success: bool,
    pub failed: bool,
    pub message: String,
}

/// Terminal payload when cancellation is observed before an edit commits.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditCancelledResultV1 {
    pub success: bool,
    pub cancelled: bool,
    pub message: String,
}

/// Terminal payload when an edit deadline expires before commit.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditTimedOutResultV1 {
    pub success: bool,
    pub timed_out: bool,
    pub message: String,
}

/// Terminal payload when publication may have occurred and inspection is required.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditEffectUnknownResultV1 {
    pub success: bool,
    pub effect_unknown: bool,
    pub message: String,
}

/// Terminal payload returned by explicit reconciliation and rollback.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditReconciledResultV1 {
    pub success: bool,
    pub reconciled: bool,
    pub message: String,
}

/// Body-free source-edit evidence retained inside durable effect receipts.
///
/// This payload intentionally contains no caller-supplied edit text, preview
/// diff, moved span, import, diagnostic, or impact detail. It is also the
/// exact replay outcome, so replay never fabricates a live edit body.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEditDurableEffectPayloadV1 {
    pub operation: UseCaseId,
    pub success: bool,
    pub files: Vec<String>,
    pub change_count: Option<usize>,
    pub line: Option<u32>,
    pub before: Option<bool>,
    pub import_count: Option<usize>,
    pub finding_count: Option<usize>,
    pub failed: bool,
    pub cancelled: bool,
    pub timed_out: bool,
    pub effect_unknown: bool,
    pub reconciled: bool,
    pub durable_metadata_only: bool,
    pub message: String,
}

/// The single typed source-edit output union used by application, MCP, HTTP,
/// and generated SDK surfaces.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum SourceEditSurfaceOutcomeV1 {
    Edit(EditResult),
    MultiEdit(MultiEditResult),
    Insert(InsertResult),
    AstGrep(AstGrepResult),
    Move(MoveResult),
    Rename(RenameResult),
    Failed(SourceEditFailedResultV1),
    Cancelled(SourceEditCancelledResultV1),
    TimedOut(SourceEditTimedOutResultV1),
    EffectUnknown(SourceEditEffectUnknownResultV1),
    Reconciled(SourceEditReconciledResultV1),
    DurableMetadata(SourceEditDurableEffectPayloadV1),
}

impl SourceEditSurfaceOutcomeV1 {
    pub fn success(&self) -> bool {
        match self {
            Self::Edit(result) => result.success,
            Self::MultiEdit(result) => result.success,
            Self::Insert(result) => result.success,
            Self::AstGrep(result) => result.success,
            Self::Move(result) => result.success,
            Self::Rename(result) => result.success,
            Self::Failed(result) => result.success,
            Self::Cancelled(result) => result.success,
            Self::TimedOut(result) => result.success,
            Self::EffectUnknown(result) => result.success,
            Self::Reconciled(result) => result.success,
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
            Self::Rename(result) => &result.message,
            Self::Failed(result) => &result.message,
            Self::Cancelled(result) => &result.message,
            Self::TimedOut(result) => &result.message,
            Self::EffectUnknown(result) => &result.message,
            Self::Reconciled(result) => &result.message,
            Self::DurableMetadata(result) => &result.message,
        }
    }

    pub fn dry_run(&self) -> bool {
        match self {
            Self::Edit(result) => result.dry_run,
            Self::MultiEdit(result) => result.dry_run,
            Self::Insert(result) => result.dry_run,
            Self::AstGrep(result) => result.dry_run,
            Self::Move(result) => result.dry_run,
            Self::Rename(result) => result.dry_run,
            Self::Failed(_)
            | Self::Cancelled(_)
            | Self::TimedOut(_)
            | Self::EffectUnknown(_)
            | Self::Reconciled(_)
            | Self::DurableMetadata(_) => false,
        }
    }

    pub fn touched_files(&self) -> Vec<String> {
        if self.dry_run() || !self.success() {
            return Vec::new();
        }
        match self {
            Self::Edit(result) => vec![result.file_path.clone()],
            Self::MultiEdit(result) => vec![result.file_path.clone()],
            Self::Insert(result) => vec![result.file_path.clone()],
            Self::AstGrep(result) => vec![result.file_path.clone()],
            Self::Move(result) => vec![result.source_file.clone(), result.dest_file.clone()],
            Self::Rename(result) => result.files.iter().map(|file| file.file.clone()).collect(),
            Self::DurableMetadata(result) => result.files.clone(),
            Self::Failed(_)
            | Self::Cancelled(_)
            | Self::TimedOut(_)
            | Self::EffectUnknown(_)
            | Self::Reconciled(_) => Vec::new(),
        }
    }

    pub fn candidate_files(&self) -> Vec<String> {
        match self {
            Self::Edit(result) => vec![result.file_path.clone()],
            Self::MultiEdit(result) => vec![result.file_path.clone()],
            Self::Insert(result) => vec![result.file_path.clone()],
            Self::AstGrep(result) => vec![result.file_path.clone()],
            Self::Move(result) => vec![result.source_file.clone(), result.dest_file.clone()],
            Self::Rename(result) => result.files.iter().map(|file| file.file.clone()).collect(),
            Self::DurableMetadata(result) => result.files.clone(),
            Self::Failed(_)
            | Self::Cancelled(_)
            | Self::TimedOut(_)
            | Self::EffectUnknown(_)
            | Self::Reconciled(_) => Vec::new(),
        }
    }

    pub fn as_move(&self) -> Option<&MoveResult> {
        match self {
            Self::Move(result) => Some(result),
            _ => None,
        }
    }
}

impl SourceEditDurableEffectPayloadV1 {
    pub fn from_live(operation: &UseCaseId, outcome: &SourceEditSurfaceOutcomeV1) -> Self {
        let (change_count, line, before, import_count, finding_count) = match outcome {
            SourceEditSurfaceOutcomeV1::MultiEdit(result) => {
                (Some(result.applied_count), None, None, None, None)
            }
            SourceEditSurfaceOutcomeV1::Insert(result) => (
                None,
                Some(result.anchor_line),
                Some(result.before),
                None,
                None,
            ),
            SourceEditSurfaceOutcomeV1::Move(result) => (
                None,
                None,
                None,
                Some(result.applied_imports.len()),
                Some(result.impact.len()),
            ),
            SourceEditSurfaceOutcomeV1::Rename(result) => (
                Some(
                    result
                        .files
                        .iter()
                        .map(|file| file.replaced_count)
                        .sum::<usize>(),
                ),
                None,
                None,
                None,
                Some(result.hazards.len()),
            ),
            _ => (None, None, None, None, None),
        };
        let failed = matches!(outcome, SourceEditSurfaceOutcomeV1::Failed(_));
        let cancelled = matches!(outcome, SourceEditSurfaceOutcomeV1::Cancelled(_));
        let timed_out = matches!(outcome, SourceEditSurfaceOutcomeV1::TimedOut(_));
        let effect_unknown = matches!(outcome, SourceEditSurfaceOutcomeV1::EffectUnknown(_));
        let reconciled = matches!(outcome, SourceEditSurfaceOutcomeV1::Reconciled(_));
        let message = if failed {
            "source edit failed before the effect"
        } else if cancelled {
            "source edit was cancelled"
        } else if timed_out {
            "source edit timed out"
        } else if effect_unknown {
            "source edit effect is unknown and requires reconciliation"
        } else if reconciled {
            "source edit reconciliation completed"
        } else if outcome.success() {
            "source edit completed; detailed edit output was not retained"
        } else {
            "source edit failed; detailed edit output was not retained"
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
            failed,
            cancelled,
            timed_out,
            effect_unknown,
            reconciled,
            durable_metadata_only: true,
            message: message.to_owned(),
        }
    }
}

/// Canonical result returned directly by every source-edit use case.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SourceEditSurfaceResultV1 {
    #[serde(flatten)]
    pub outcome: SourceEditSurfaceOutcomeV1,
    pub expected_state: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_state: Option<ManifestDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<SourceEditVerificationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectResult<SourceEditDurableEffectPayloadV1>>,
    pub replayed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_edit::{RenameFileEditV1, RenameHazardKindV1, RenameHazardV1};

    const EXPECTED_STATE: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn durable_rename_metadata_counts_current_hazards_and_changes() {
        let outcome = SourceEditSurfaceOutcomeV1::Rename(RenameResult {
            success: true,
            files: vec![
                RenameFileEditV1 {
                    file: "src/lib.rs".to_owned(),
                    replaced_count: 2,
                },
                RenameFileEditV1 {
                    file: "src/caller.rs".to_owned(),
                    replaced_count: 3,
                },
            ],
            hazards: vec![
                RenameHazardV1 {
                    kind: RenameHazardKindV1::Shadowing,
                    blocking: false,
                    message: "shadowing requires review".to_owned(),
                    site_id: Some("site.shadowing".to_owned()),
                },
                RenameHazardV1 {
                    kind: RenameHazardKindV1::ChangedResolution,
                    blocking: true,
                    message: "resolution would change".to_owned(),
                    site_id: Some("site.resolution".to_owned()),
                },
            ],
            message: "rename planned".to_owned(),
            ..RenameResult::default()
        });
        let operation = UseCaseId::new("use-case.application.rename-symbol")
            .expect("rename operation identity");

        let durable = SourceEditDurableEffectPayloadV1::from_live(&operation, &outcome);

        assert_eq!(durable.change_count, Some(5));
        assert_eq!(durable.finding_count, Some(2));
        assert_eq!(
            durable.files,
            ["src/lib.rs".to_owned(), "src/caller.rs".to_owned()]
        );
    }

    #[test]
    fn source_edit_surface_wire_types_are_deserialize_owned() {
        fn assert_deserialize_owned<T: serde::de::DeserializeOwned>() {}

        assert_deserialize_owned::<SourceEditSurfaceOutcomeV1>();
        assert_deserialize_owned::<SourceEditSurfaceResultV1>();
    }

    #[test]
    fn source_edit_surface_result_round_trips_success() {
        let expected = serde_json::json!({
            "success": true,
            "file_path": "src/lib.rs",
            "matched_str": "old_name",
            "new_str": "new_name",
            "message": "replacement completed",
            "expected_state": EXPECTED_STATE,
            "replayed": false,
        });

        let decoded: SourceEditSurfaceResultV1 =
            serde_json::from_value(expected.clone()).expect("deserialize success result");

        assert!(matches!(
            &decoded.outcome,
            SourceEditSurfaceOutcomeV1::Edit(_)
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize success result"),
            expected
        );
    }

    #[test]
    fn source_edit_surface_result_round_trips_failure() {
        let expected = serde_json::json!({
            "success": false,
            "failed": true,
            "message": "edit was denied",
            "expected_state": EXPECTED_STATE,
            "replayed": false,
        });

        let decoded: SourceEditSurfaceResultV1 =
            serde_json::from_value(expected.clone()).expect("deserialize failure result");

        assert!(matches!(
            &decoded.outcome,
            SourceEditSurfaceOutcomeV1::Failed(_)
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize failure result"),
            expected
        );
    }

    #[test]
    fn source_edit_surface_result_round_trips_reconciled_outcome() {
        let expected = serde_json::json!({
            "success": true,
            "reconciled": true,
            "message": "the edit was confirmed committed",
            "expected_state": EXPECTED_STATE,
            "replayed": true,
        });

        let decoded: SourceEditSurfaceResultV1 =
            serde_json::from_value(expected.clone()).expect("deserialize reconciled result");

        assert!(matches!(
            &decoded.outcome,
            SourceEditSurfaceOutcomeV1::Reconciled(_)
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize reconciled result"),
            expected
        );
    }

    #[test]
    fn source_edit_surface_result_rejects_a_malformed_outcome() {
        let malformed = serde_json::json!({
            "success": false,
            "failed": true,
            "expected_state": EXPECTED_STATE,
            "replayed": false,
        });

        assert!(serde_json::from_value::<SourceEditSurfaceResultV1>(malformed).is_err());
    }

    #[test]
    fn source_edit_surface_result_rejects_an_unknown_outcome_shape() {
        let unknown = serde_json::json!({
            "success": false,
            "unknown_outcome": true,
            "message": "unrecognized result",
            "expected_state": EXPECTED_STATE,
            "replayed": false,
        });

        assert!(serde_json::from_value::<SourceEditSurfaceResultV1>(unknown).is_err());
    }
}
