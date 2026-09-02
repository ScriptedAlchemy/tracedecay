//! Public request models for the source-edit transport surface.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::effect_authorization::SourceEditReconciliationDispositionV1;
use super::{RenamePreviewAcceptanceV1, SourceEditKind};

/// Public control fields shared by the source-edit MCP effects.
///
/// Preview calls intentionally omit the effect identity; an apply must carry
/// both values and the daemon enforces that relationship before it enters the
/// durable source-edit owner. Keeping the fields optional here preserves the
/// actual preview wire form rather than manufacturing an SDK-only variant.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEditApplyControlV1 {
    pub idempotency_key: Option<String>,
    /// Exact `preview_digest`/`expected_state` returned by the dry run. Apply
    /// re-resolves the typed plan and rejects any candidate-state drift.
    pub expected_state: Option<String>,
}

/// Exact public input accepted by `tracedecay_str_replace`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StrReplaceSurfaceRequestV1 {
    pub path: String,
    pub old_str: String,
    pub new_str: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_multi_str_replace`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiStrReplaceSurfaceRequestV1 {
    pub path: String,
    /// Ordered `[old, new]` pairs, matching the existing MCP tool wire form.
    pub replacements: Vec<(String, String)>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_insert_at`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InsertAtSurfaceRequestV1 {
    pub path: String,
    pub anchor: String,
    pub content: String,
    #[serde(default)]
    pub before: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_ast_grep_rewrite`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AstGrepRewriteSurfaceRequestV1 {
    pub path: String,
    pub pattern: String,
    pub rewrite: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_replace_symbol`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplaceSymbolSurfaceRequestV1 {
    pub symbol: String,
    pub new_source: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_insert_at_symbol`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InsertAtSymbolSurfaceRequestV1 {
    pub symbol: String,
    pub content: String,
    #[serde(default = "default_insert_after")]
    pub position: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

fn default_insert_after() -> String {
    "after".to_owned()
}

/// Exact public input accepted by `tracedecay_move_symbol`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MoveSymbolSurfaceRequestV1 {
    pub symbol: String,
    pub dest_file: String,
    #[serde(default = "default_preview")]
    pub dry_run: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

fn default_preview() -> bool {
    true
}

fn default_verify() -> bool {
    true
}

/// Exact public input accepted by the read-only `tracedecay_rename_preview`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewSurfaceRequestV1 {
    /// Canonical `SymbolOccurrenceId` from the verified code graph.
    pub node_id: String,
}

/// Exact public input accepted by `tracedecay_rename_symbol`.
///
/// The five identity fields consume the preview's exact symbol identity; the
/// flattened control consumes its exact candidate-state digest.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameSymbolSurfaceRequestV1 {
    /// Canonical `SymbolOccurrenceId` returned by `tracedecay_rename_preview`.
    pub node_id: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub old_name: String,
    pub new_name: String,
    /// Exact output identity from the accepted dry-run preview. Required when
    /// `dry_run=false` and omitted when computing a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_preview: Option<RenamePreviewAcceptanceV1>,
    #[serde(default = "default_preview")]
    pub dry_run: bool,
    #[serde(default = "default_verify")]
    pub verify: bool,
    #[serde(flatten)]
    pub control: SourceEditApplyControlV1,
}

/// Exact public input accepted by `tracedecay_source_edit_reconcile`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEditReconcileSurfaceRequestV1 {
    pub kind: SourceEditKind,
    pub effect_id: String,
    pub idempotency_key: String,
    pub attempt_idempotency_key: String,
    pub input_digest: String,
    pub disposition: SourceEditReconciliationDispositionV1,
    pub confirm: bool,
}

/// Exact public input accepted by `tracedecay_source_edit_rollback`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceEditRollbackSurfaceRequestV1 {
    pub effect_id: String,
    pub original_idempotency_key: String,
    pub idempotency_key: String,
    pub original_input_digest: String,
    pub expected_state: String,
    pub confirm: bool,
}
