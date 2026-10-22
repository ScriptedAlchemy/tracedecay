//! Source-edit tool arguments decoded into their typed daemon invocations.
//!
//! The messages are the public argument diagnostics every transport reports.

use serde_json::{Value, json};
use tracedecay_contracts::{
    EffectId, IdempotencyKey, RenameSymbolBindingV1, RenameSymbolSurfaceRequestV1,
    SourceEditInvocationV1, SourceEditKind, SourceEditReconciliationDispositionV1,
    SourceEditReconciliationInvocationV1, SourceEditRequest,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, UseCaseId};

use super::ApplicationSurfaceRequest;

fn missing_required_param(name: &str) -> String {
    format!("missing required parameter: {name}")
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| missing_required_param(name))
}

fn required_string(args: &Value, name: &str) -> Result<String, String> {
    required_str(args, name).map(str::to_owned)
}

fn identity_error(error: impl std::fmt::Display) -> String {
    format!("invalid source edit effect identity: {error}")
}

fn flag(args: &Value, name: &str, default: bool) -> bool {
    args.get(name).and_then(Value::as_bool).unwrap_or(default)
}

fn optional_idempotency_key(args: &Value) -> Result<Option<IdempotencyKey>, String> {
    args.get("idempotency_key")
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| missing_required_param("idempotency_key"))?;
            IdempotencyKey::new(value).map_err(|error| format!("invalid idempotency_key: {error}"))
        })
        .transpose()
}

fn optional_expected_state(args: &Value) -> Result<Option<ManifestDigest>, String> {
    args.get("expected_state")
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| missing_required_param("expected_state"))?;
            ManifestDigest::new(value).map_err(|error| format!("invalid expected_state: {error}"))
        })
        .transpose()
}

fn edit_request(
    operation: ApplicationSurfaceOperation,
    args: &Value,
) -> Result<SourceEditRequest, String> {
    let dry_run = flag(args, "dry_run", false);
    let verify = flag(args, "verify", false);
    Ok(match operation {
        ApplicationSurfaceOperation::StrReplace => SourceEditRequest::StrReplace {
            path: required_string(args, "path")?,
            old_str: required_string(args, "old_str")?,
            new_str: required_string(args, "new_str")?,
            dry_run,
            verify,
        },
        ApplicationSurfaceOperation::MultiStrReplace => {
            let path = required_string(args, "path")?;
            let replacements = args
                .get("replacements")
                .and_then(Value::as_array)
                .ok_or_else(|| missing_required_param("replacements"))?;
            let parsed: Vec<(String, String)> = replacements
                .iter()
                .filter_map(|pair| match pair.as_array()?.as_slice() {
                    [old, new] => Some((old.as_str()?.to_owned(), new.as_str()?.to_owned())),
                    _ => None,
                })
                .collect();
            if parsed.len() != replacements.len() {
                return Err("each replacement must be an array of exactly 2 strings".to_owned());
            }
            SourceEditRequest::MultiStrReplace {
                path,
                replacements: parsed,
                dry_run,
                verify,
            }
        }
        ApplicationSurfaceOperation::InsertAt => SourceEditRequest::InsertAt {
            path: required_string(args, "path")?,
            anchor: required_string(args, "anchor")?,
            content: required_string(args, "content")?,
            before: flag(args, "before", false),
            dry_run,
            verify,
        },
        ApplicationSurfaceOperation::AstGrepRewrite => SourceEditRequest::AstGrepRewrite {
            path: required_string(args, "path")?,
            pattern: required_string(args, "pattern")?,
            rewrite: required_string(args, "rewrite")?,
            dry_run,
            verify,
        },
        ApplicationSurfaceOperation::ReplaceSymbol => SourceEditRequest::ReplaceSymbol {
            symbol: required_string(args, "symbol")?,
            new_source: required_string(args, "new_source")?,
            dry_run,
            verify,
        },
        ApplicationSurfaceOperation::InsertAtSymbol => SourceEditRequest::InsertAtSymbol {
            symbol: required_string(args, "symbol")?,
            content: required_string(args, "content")?,
            position: args
                .get("position")
                .and_then(Value::as_str)
                .unwrap_or("after")
                .to_owned(),
            dry_run,
            verify,
        },
        // The impact report is the product; applying is opt-in.
        ApplicationSurfaceOperation::MoveSymbol => SourceEditRequest::MoveSymbol {
            symbol: required_string(args, "symbol")?,
            dest_file: required_string(args, "dest_file")?,
            dry_run: flag(args, "dry_run", true),
            update_references: flag(args, "update_references", false),
        },
        ApplicationSurfaceOperation::RenameSymbol => {
            let mut input = args.clone();
            if let Some(object) = input.as_object_mut() {
                object.remove("format");
                object.remove("__mcp_request_id");
            }
            let request: RenameSymbolSurfaceRequestV1 = serde_json::from_value(input)
                .map_err(|error| format!("invalid source edit request: {error}"))?;
            SourceEditRequest::RenameSymbol {
                binding: RenameSymbolBindingV1 {
                    node_id: request.node_id,
                    qualified_name: request.qualified_name,
                    kind: request.kind,
                    file: request.file,
                    old_name: request.old_name,
                    accepted_preview: request.accepted_preview,
                },
                new_name: request.new_name,
                dry_run: request.dry_run,
                verify: request.verify,
            }
        }
        _ => return Err(format!("{} is not a source edit", operation.as_str())),
    })
}

fn reconcile(args: &Value) -> Result<SourceEditReconciliationInvocationV1, String> {
    if args.get("disposition").and_then(Value::as_str) != Some("inspect")
        && args.get("confirm").and_then(Value::as_bool) != Some(true)
    {
        return Err("source edit reconciliation requires confirm=true from the caller after it inspects the file; do not pause for a human".to_owned());
    }
    let original_operation =
        UseCaseId::new(required_str(args, "original_operation")?).map_err(identity_error)?;
    let effect_id = EffectId::new(required_str(args, "effect_id")?).map_err(identity_error)?;
    let idempotency_key =
        IdempotencyKey::new(required_str(args, "idempotency_key")?).map_err(identity_error)?;
    let attempt_idempotency_key =
        IdempotencyKey::new(required_str(args, "attempt_idempotency_key")?)
            .map_err(identity_error)?;
    if attempt_idempotency_key == idempotency_key {
        return Err(
            "reconciliation attempt idempotency key must differ from the original edit key"
                .to_owned(),
        );
    }
    let input_digest =
        ManifestDigest::new(required_str(args, "input_digest")?).map_err(identity_error)?;
    let disposition = match required_str(args, "disposition")? {
        "inspect" => {
            if args.get("committed_state").is_some() || args.get("observed_state").is_some() {
                return Err("inspection does not accept a caller-supplied state digest".to_owned());
            }
            SourceEditReconciliationDispositionV1::Inspect
        }
        "confirm_committed" => {
            if args.get("observed_state").is_some() {
                return Err(
                    "observed_state is only valid when disposition is acknowledge_partial"
                        .to_owned(),
                );
            }
            SourceEditReconciliationDispositionV1::ConfirmCommitted {
                committed_state: ManifestDigest::new(required_str(args, "committed_state")?)
                    .map_err(identity_error)?,
            }
        }
        "confirm_unchanged" => {
            if args.get("committed_state").is_some() || args.get("observed_state").is_some() {
                return Err(
                    "committed_state and observed_state do not apply to confirm_unchanged"
                        .to_owned(),
                );
            }
            SourceEditReconciliationDispositionV1::ConfirmUnchanged
        }
        "acknowledge_partial" => {
            if args.get("committed_state").is_some() {
                return Err(
                    "committed_state is only valid when disposition is confirm_committed"
                        .to_owned(),
                );
            }
            SourceEditReconciliationDispositionV1::AcknowledgePartial {
                observed_state: ManifestDigest::new(required_str(args, "observed_state")?)
                    .map_err(identity_error)?,
            }
        }
        value => {
            return Err(format!(
                "invalid source edit reconciliation disposition: {value}"
            ));
        }
    };
    Ok(SourceEditReconciliationInvocationV1 {
        original_operation,
        effect_id,
        idempotency_key,
        attempt_idempotency_key,
        input_digest,
        disposition,
    })
}

/// Whether `operation` is one of the source-edit tools.
pub fn is_source_edit_operation(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::StrReplace
            | ApplicationSurfaceOperation::MultiStrReplace
            | ApplicationSurfaceOperation::InsertAt
            | ApplicationSurfaceOperation::AstGrepRewrite
            | ApplicationSurfaceOperation::ReplaceSymbol
            | ApplicationSurfaceOperation::InsertAtSymbol
            | ApplicationSurfaceOperation::MoveSymbol
            | ApplicationSurfaceOperation::RenameSymbol
            | ApplicationSurfaceOperation::SourceEditReconcile
    )
}

/// Decode one source-edit tool's arguments; `Err` is the public diagnostic.
pub fn parse_source_edit_arguments(
    operation: ApplicationSurfaceOperation,
    args: &Value,
) -> Result<ApplicationSurfaceRequest, String> {
    match operation {
        ApplicationSurfaceOperation::SourceEditReconcile => {
            reconcile(args).map(ApplicationSurfaceRequest::SourceEditReconcile)
        }
        _ => {
            let edit = edit_request(operation, args)?;
            let idempotency_key = optional_idempotency_key(args)?;
            let expected_state = optional_expected_state(args)?;
            if !edit.dry_run() && (idempotency_key.is_none() || expected_state.is_none()) {
                return Err("source edit apply requires a fresh idempotency_key and the expected_state returned by a preview".to_owned());
            }
            Ok(ApplicationSurfaceRequest::SourceEdit(
                SourceEditInvocationV1 {
                    edit,
                    idempotency_key,
                    expected_state,
                },
            ))
        }
    }
}

/// The source-edit kind a surface operation names, when it names one.
pub(super) const fn source_edit_kind(
    operation: ApplicationSurfaceOperation,
) -> Option<SourceEditKind> {
    Some(match operation {
        ApplicationSurfaceOperation::StrReplace => SourceEditKind::StrReplace,
        ApplicationSurfaceOperation::MultiStrReplace => SourceEditKind::MultiStrReplace,
        ApplicationSurfaceOperation::InsertAt => SourceEditKind::InsertAt,
        ApplicationSurfaceOperation::AstGrepRewrite => SourceEditKind::AstGrepRewrite,
        ApplicationSurfaceOperation::ReplaceSymbol => SourceEditKind::ReplaceSymbol,
        ApplicationSurfaceOperation::InsertAtSymbol => SourceEditKind::InsertAtSymbol,
        ApplicationSurfaceOperation::MoveSymbol => SourceEditKind::MoveSymbol,
        ApplicationSurfaceOperation::RenameSymbol => SourceEditKind::RenameSymbol,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tracedecay_tool_catalog::ApplicationSurfaceOperation;

    use super::parse_source_edit_arguments;
    use crate::application_surface::ApplicationSurfaceRequest;

    #[test]
    fn apply_without_preview_identity_is_refused_with_the_public_message() {
        let error = parse_source_edit_arguments(
            ApplicationSurfaceOperation::StrReplace,
            &json!({"path": "src/lib.rs", "old_str": "a", "new_str": "b"}),
        )
        .expect_err("apply requires preview identity");
        assert_eq!(
            error,
            "source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
        );
    }

    #[test]
    fn move_symbol_defaults_to_preview_and_keeps_update_references() {
        let request = parse_source_edit_arguments(
            ApplicationSurfaceOperation::MoveSymbol,
            &json!({"symbol": "a", "dest_file": "src/b.rs", "update_references": true}),
        )
        .expect("move preview");
        let ApplicationSurfaceRequest::SourceEdit(invocation) = request else {
            panic!("move is a source edit");
        };
        assert!(invocation.edit.dry_run());
        assert!(matches!(
            invocation.edit,
            tracedecay_contracts::SourceEditRequest::MoveSymbol {
                update_references: true,
                ..
            }
        ));
    }

    #[test]
    fn reconcile_accepts_committed_unchanged_and_partial_observations() {
        let base = json!({
            "original_operation": "use-case.application.source-edit.str-replace",
            "effect_id": "effect.source-edit.reconcile",
            "idempotency_key": "source-edit.original",
            "attempt_idempotency_key": "source-edit.reconcile",
            "input_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "confirm": true,
        });
        for (disposition, digest_field) in [
            ("confirm_committed", "committed_state"),
            ("acknowledge_partial", "observed_state"),
        ] {
            let mut args = base.clone();
            args["disposition"] = Value::String(disposition.to_owned());
            args[digest_field] = Value::String(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_owned(),
            );
            assert!(matches!(
                parse_source_edit_arguments(
                    ApplicationSurfaceOperation::SourceEditReconcile,
                    &args
                )
                .expect("valid observed state"),
                ApplicationSurfaceRequest::SourceEditReconcile(_)
            ));
        }
        let mut unchanged = base;
        unchanged["disposition"] = Value::String("confirm_unchanged".to_owned());
        assert!(matches!(
            parse_source_edit_arguments(
                ApplicationSurfaceOperation::SourceEditReconcile,
                &unchanged
            )
            .expect("valid unchanged state"),
            ApplicationSurfaceRequest::SourceEditReconcile(_)
        ));
    }

    #[test]
    fn inspection_requires_identity_but_no_conclusion_or_caller_digest() {
        let mut args = json!({
            "original_operation": "use-case.application.source-edit.rollback",
            "effect_id": "effect.source-edit.retained",
            "idempotency_key": "source-edit.original",
            "attempt_idempotency_key": "source-edit.inspect",
            "input_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "disposition": "inspect",
        });
        assert!(matches!(
            parse_source_edit_arguments(ApplicationSurfaceOperation::SourceEditReconcile, &args)
                .unwrap(),
            ApplicationSurfaceRequest::SourceEditReconcile(SourceEditReconciliationInvocationV1 {
                disposition: SourceEditReconciliationDispositionV1::Inspect,
                ..
            })
        ));
        args["observed_state"] = args["input_digest"].clone();
        assert!(
            parse_source_edit_arguments(ApplicationSurfaceOperation::SourceEditReconcile, &args)
                .is_err()
        );
        args.as_object_mut().unwrap().remove("observed_state");
        args["disposition"] = json!("confirm_unchanged");
        assert!(
            parse_source_edit_arguments(ApplicationSurfaceOperation::SourceEditReconcile, &args)
                .is_err()
        );
    }

    #[test]
    fn missing_replacement_pair_names_the_exact_shape() {
        assert_eq!(
            parse_source_edit_arguments(
                ApplicationSurfaceOperation::MultiStrReplace,
                &json!({"path": "src/lib.rs", "replacements": [["only"]], "dry_run": true}),
            )
            .expect_err("pairs are exact"),
            "each replacement must be an array of exactly 2 strings"
        );
    }
}
