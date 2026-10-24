//! Public request models for the source-edit transport surface.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::UseCaseId;

use super::RenamePreviewAcceptanceV1;
use super::effect_authorization::SourceEditReconciliationDispositionV1;

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
#[schemars(deny_unknown_fields, transform = close_reconciliation_variants)]
pub struct SourceEditReconcileSurfaceRequestV1 {
    pub original_operation: UseCaseId,
    pub effect_id: String,
    pub idempotency_key: String,
    pub attempt_idempotency_key: String,
    pub input_digest: String,
    #[serde(flatten)]
    pub disposition: SourceEditReconciliationDispositionV1,
    #[serde(default)]
    pub confirm: bool,
}

// Closing each complete variant preserves unknown-field rejection without
// requiring consumers to implement unevaluatedProperties across the union.
fn close_reconciliation_variants(schema: &mut schemars::Schema) {
    let Some(serde_json::Value::Array(variants)) = schema.get("oneOf") else {
        return;
    };
    let variants = variants
        .iter()
        .map(|variant| {
            let mut branch = schema.clone();
            branch.remove("oneOf");
            branch.remove("unevaluatedProperties");
            branch.insert("additionalProperties".to_owned(), false.into());
            if let Some(serde_json::Value::Object(properties)) = variant.get("properties")
                && let Some(serde_json::Value::Object(common)) = branch.get_mut("properties")
            {
                common.extend(properties.clone());
            }
            if let Some(serde_json::Value::Array(required)) = variant.get("required")
                && let Some(serde_json::Value::Array(common)) = branch.get_mut("required")
            {
                common.extend(required.clone());
            }
            if let Some(object) = variant.as_object() {
                for (key, value) in object {
                    if key != "properties" && key != "required" {
                        branch.insert(key.clone(), value.clone());
                    }
                }
            }
            branch.to_value()
        })
        .collect();
    schema.remove("properties");
    schema.remove("required");
    schema.remove("unevaluatedProperties");
    schema.insert("oneOf".to_owned(), serde_json::Value::Array(variants));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::SourceEditReconcileSurfaceRequestV1;

    #[test]
    fn reconciliation_deserialization_rejects_unknown_and_wrong_variant_fields() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for fields in [
            json!({ "disposition": "inspect" }),
            json!({ "disposition": "confirm_unchanged" }),
            json!({ "disposition": "confirm_committed", "committed_state": digest }),
            json!({ "disposition": "acknowledge_partial", "observed_state": digest }),
        ] {
            let mut request = json!({
                "original_operation": "use-case.application.source-edit.str-replace",
                "effect_id": "effect.source-edit.retained",
                "idempotency_key": "source-edit.original",
                "attempt_idempotency_key": "source-edit.reconciliation",
                "input_digest": digest,
            });
            request
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let disposition = fields["disposition"].as_str().unwrap();
            let decoded: SourceEditReconcileSurfaceRequestV1 =
                serde_json::from_value(request.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(decoded).unwrap()["disposition"],
                fields["disposition"],
                "{disposition} should round-trip"
            );

            let mut unknown = request.clone();
            unknown["unexpected"] = json!(true);
            assert!(
                serde_json::from_value::<SourceEditReconcileSurfaceRequestV1>(unknown).is_err(),
                "{disposition} should reject unknown request fields"
            );

            let wrong_field = if fields.get("committed_state").is_some() {
                "observed_state"
            } else {
                "committed_state"
            };
            request[wrong_field] = json!(digest);
            assert!(
                serde_json::from_value::<SourceEditReconcileSurfaceRequestV1>(request).is_err(),
                "{disposition} should reject the other variant's {wrong_field} field"
            );
        }
    }
}
