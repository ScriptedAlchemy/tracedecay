//! Edit/refactor tool definitions.

use serde_json::{Value, json};

fn source_edit_schema(mut schema: Value) -> Value {
    let Some(root) = schema.as_object_mut() else {
        unreachable!("source edit input schema must be an object");
    };
    let Some(properties) = root.get_mut("properties").and_then(Value::as_object_mut) else {
        unreachable!("source edit input schema must define object properties");
    };
    properties.insert(
        "idempotency_key".to_owned(),
        json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 512,
            "description": "Stable caller-provided key for this edit effect. Reusing the key reconciles retries with the original effect."
        }),
    );
    properties.insert(
        "expected_state".to_owned(),
        json!({
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$",
            "description": "Caller-observed digest of every file the edit may touch. Applying the edit is rejected if the current state differs."
        }),
    );
    if let Some(dry_run) = properties.get_mut("dry_run").and_then(Value::as_object_mut) {
        dry_run.insert("default".to_owned(), Value::Bool(false));
    }
    if let Some(verify) = properties.get_mut("verify").and_then(Value::as_object_mut) {
        verify.insert("default".to_owned(), Value::Bool(false));
    }
    root.insert("additionalProperties".to_owned(), Value::Bool(false));
    root.insert(
        "allOf".to_owned(),
        json!([
            {
                "if": {
                    "properties": {
                        "dry_run": {"const": false}
                    }
                },
                "then": {
                    "required": ["idempotency_key", "expected_state"]
                }
            }
        ]),
    );
    schema
}

pub(super) fn source_edit_reconcile_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": {
                "type": "string",
                "enum": [
                    "str_replace",
                    "multi_str_replace",
                    "insert_at",
                    "ast_grep_rewrite",
                    "replace_symbol",
                    "insert_at_symbol",
                    "move_symbol",
                    "rename_symbol"
                ],
                "description": "Original source-edit operation kind retained in the uncertain journal."
            },
            "effect_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "Exact effect ID from the EffectUnknown receipt."
            },
            "idempotency_key": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "Original caller-provided idempotency key."
            },
            "attempt_idempotency_key": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "A new idempotency key for this reconciliation attempt. It must differ from the original edit key; exact retries replay this attempt's receipt."
            },
            "input_digest": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$",
                "description": "Exact input digest from the EffectUnknown receipt."
            },
            "disposition": {
                "type": "string",
                "enum": ["confirm_committed", "confirm_rolled_back"],
                "description": "Independent inspection conclusion. Reconciliation verifies the live candidate-file digest."
            },
            "committed_state": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$",
                "description": "Exact independently observed committed-state digest; required only for confirm_committed."
            },
            "confirm": {
                "type": "boolean",
                "const": true,
                "description": "Caller attestation that this concludes a retained uncertain effect without retrying it. Set true after your own inspection; do not ask a human to confirm a reversible file-state conclusion."
            }
        },
        "required": [
            "kind",
            "effect_id",
            "idempotency_key",
            "attempt_idempotency_key",
            "input_digest",
            "disposition",
            "confirm"
        ],
        "allOf": [{
            "if": {
                "properties": {
                    "disposition": {"const": "confirm_committed"}
                },
                "required": ["disposition"]
            },
            "then": {"required": ["committed_state"]},
            "else": {"not": {"required": ["committed_state"]}}
        }]
    })
}

pub(super) fn source_edit_rollback_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "effect_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "Exact effect ID from the completed move_symbol receipt."
            },
            "original_idempotency_key": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "Original caller-provided move_symbol idempotency key."
            },
            "idempotency_key": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "description": "A fresh key for this rollback effect. Exact retries replay its durable receipt."
            },
            "original_input_digest": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$",
                "description": "Exact input digest from the completed move_symbol receipt."
            },
            "expected_state": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$",
                "description": "Exact committed-state digest from the completed move_symbol receipt. Rollback is refused unless the workspace still matches it."
            },
            "confirm": {
                "type": "boolean",
                "const": true,
                "description": "Caller attestation that this restores the retained preimages. Set true yourself; do not pause for a human when the receipt already authorizes the rollback."
            }
        },
        "required": [
            "effect_id",
            "original_idempotency_key",
            "idempotency_key",
            "original_input_digest",
            "expected_state",
            "confirm"
        ]
    })
}

pub(super) fn str_replace_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute or project-relative file path"
            },
            "old_str": {
                "type": "string",
                "description": "Exact string to find and replace. Must match exactly once in the file."
            },
            "new_str": {
                "type": "string",
                "description": "Replacement string"
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, validate and compute the edit but write nothing; the response includes a bounded preview diff of the would-be change (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real edit and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["path", "old_str", "new_str"]
    }))
}

pub(super) fn multi_str_replace_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute or project-relative file path"
            },
            "replacements": {
                "type": "array",
                "description": "Array of [old_str, new_str] pairs to replace",
                "items": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 2,
                    "maxItems": 2
                }
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, validate and compute all replacements but write nothing; the response includes a bounded preview diff of the would-be change (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real edit and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["path", "replacements"]
    }))
}

pub(super) fn insert_at_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute or project-relative file path"
            },
            "anchor": {
                "type": "string",
                "description": "Unique string or line number (1-indexed) to insert at"
            },
            "content": {
                "type": "string",
                "description": "Content to insert"
            },
            "before": {
                "type": "boolean",
                "default": false,
                "description": "If true, insert before the anchor line; if false, insert after (default: false)"
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, resolve the anchor and compute the insertion but write nothing; the response includes a bounded preview diff of the would-be change (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real edit and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["path", "anchor", "content"]
    }))
}

pub(super) fn rename_symbol_schema() -> Value {
    let mut schema = source_edit_schema(json!({
        "type": "object",
        "properties": {
            "node_id": {
                "type": "string",
                "description": "Exact node ID of the symbol, as reported by tracedecay_rename_preview."
            },
            "qualified_name": {
                "type": "string",
                "description": "Exact qualified name the preview reported for the node."
            },
            "kind": {
                "type": "string",
                "description": "Exact symbol kind the preview reported (e.g. 'function')."
            },
            "file": {
                "type": "string",
                "description": "Exact project-relative defining file the preview reported."
            },
            "old_name": {
                "type": "string",
                "description": "Exact current name the preview reported. Apply refuses if the live symbol drifted."
            },
            "new_name": {
                "type": "string",
                "description": "New identifier. Must be a valid identifier and must not already occur in any touched file."
            },
            "accepted_preview": {
                "type": "object",
                "additionalProperties": false,
                "description": "Exact preview manifest returned by the accepted dry run. Required only when applying.",
                "properties": {
                    "preview_id": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                    "preview_digest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                    "plan_digest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"},
                    "repository_revision": {
                        "type": ["string", "null"],
                        "description": "Exact repository_revision returned by the accepted preview; copy the string or null value verbatim."
                    },
                    "graph_revision": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
                },
                "required": ["preview_id", "preview_digest", "plan_digest", "repository_revision", "graph_revision"]
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true (DEFAULT), bind every site and compute the full plan, per-file diff, and expected_state but write nothing. Set false to apply."
            },
            "verify": {
                "type": "boolean",
                "description": "If true (DEFAULT), re-run file-scoped diagnostics after a real rename and include a compact verdict. Ignored for dry runs."
            }
        },
        "required": ["node_id", "qualified_name", "kind", "file", "old_name", "new_name"]
    }));
    // Like move_symbol, rename defaults to preview; only an explicit
    // dry_run=false selects the apply branch.
    schema["properties"]["dry_run"]["default"] = json!(true);
    schema["properties"]["verify"]["default"] = json!(true);
    schema["allOf"] = json!([
        {
            "if": {
                "properties": {"dry_run": {"const": false}},
                "required": ["dry_run"]
            },
            "then": {"required": ["idempotency_key", "expected_state"]}
        },
        {
            "if": {
                "properties": {"dry_run": {"const": false}},
                "required": ["dry_run"]
            },
            "then": {"required": ["accepted_preview"]}
        }
    ]);
    schema
}

pub(super) fn ast_grep_rewrite_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Absolute or project-relative file path"
            },
            "pattern": {
                "type": "string",
                "description": "ast-grep search pattern (SGPattern syntax)"
            },
            "rewrite": {
                "type": "string",
                "description": "ast-grep rewrite rule"
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, preview the rewrite without applying it: the response returns the would-be change and writes nothing (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real rewrite and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["path", "pattern", "rewrite"]
    }))
}

pub(super) fn replace_symbol_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "symbol": {
                "type": "string",
                "description": "Symbol name. Prefer a fully qualified name for disambiguation."
            },
            "new_source": {
                "type": "string",
                "description": "Full replacement source, must include the symbol's own declaration line, plus any leading doc-comments/attributes to preserve (the replaced span includes the old ones)."
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, resolve the symbol span and compute the replacement but write nothing; the response includes a bounded preview diff and the replaced_span (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real edit and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["symbol", "new_source"]
    }))
}

pub(super) fn move_symbol_schema() -> Value {
    let mut schema = source_edit_schema(json!({
        "type": "object",
        "properties": {
            "symbol": {
                "type": "string",
                "description": "Symbol to move. Prefer a fully qualified name for disambiguation; on ambiguity callable kinds win, else the move is refused."
            },
            "dest_file": {
                "type": "string",
                "description": "Destination file (project-relative or absolute, within the project). May be a new module file; parent directories are created on apply."
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true (DEFAULT), compute the move, the combined preview diff, and the impact report but write nothing. Set false to apply the move."
            },
            "update_references": {
                "type": "boolean",
                "description": "Reserved for a future version. In v1 callers are never auto-edited; the exact change is reported as a hint instead (default: false)."
            }
        },
        "required": ["symbol", "dest_file"]
    }));
    // Unlike the other edit tools, move_symbol defaults to preview.
    // Only an explicit dry_run=false therefore selects the apply branch.
    schema["allOf"][0]["if"]["required"] = json!(["dry_run"]);
    schema["properties"]["dry_run"]["default"] = json!(true);
    schema["properties"]["update_references"]["default"] = json!(false);
    schema
}

pub(super) fn insert_at_symbol_schema() -> Value {
    source_edit_schema(json!({
        "type": "object",
        "properties": {
            "symbol": {
                "type": "string",
                "description": "Symbol name. Prefer a fully qualified name for disambiguation."
            },
            "content": {
                "type": "string",
                "description": "Source text to insert. Newlines are preserved as-is."
            },
            "position": {
                "type": "string",
                "enum": ["before", "after"],
                "default": "after",
                "description": "Where to insert relative to the symbol's range. Default: after."
            },
            "dry_run": {
                "type": "boolean",
                "description": "If true, resolve the insertion point and compute the change but write nothing; the response includes a bounded preview diff of the would-be change (default: false)."
            },
            "verify": {
                "type": "boolean",
                "description": "If true, re-run file-scoped diagnostics after a real edit and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
            }
        },
        "required": ["symbol", "content"]
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::SourceEditKind;

    use crate::ToolDefinition;
    use crate::definitions::tests::handwritten;

    fn source_edit_definitions() -> [(ToolDefinition, SourceEditKind); 8] {
        [
            (
                handwritten("tracedecay_str_replace"),
                SourceEditKind::StrReplace,
            ),
            (
                handwritten("tracedecay_multi_str_replace"),
                SourceEditKind::MultiStrReplace,
            ),
            (
                handwritten("tracedecay_insert_at"),
                SourceEditKind::InsertAt,
            ),
            (
                handwritten("tracedecay_ast_grep_rewrite"),
                SourceEditKind::AstGrepRewrite,
            ),
            (
                handwritten("tracedecay_replace_symbol"),
                SourceEditKind::ReplaceSymbol,
            ),
            (
                handwritten("tracedecay_insert_at_symbol"),
                SourceEditKind::InsertAtSymbol,
            ),
            (
                handwritten("tracedecay_move_symbol"),
                SourceEditKind::MoveSymbol,
            ),
            (
                handwritten("tracedecay_rename_symbol"),
                SourceEditKind::RenameSymbol,
            ),
        ]
    }

    #[test]
    fn source_edit_schemas_are_strict_defaulted_and_exactly_paired() {
        for (definition, kind) in source_edit_definitions() {
            let schema = definition.input_schema;
            assert_eq!(
                definition.name,
                format!("tracedecay_{}", kind.operation_name())
            );
            assert_eq!(schema["additionalProperties"], json!(false));
            assert_eq!(schema["properties"]["idempotency_key"]["minLength"], 1);
            assert_eq!(schema["properties"]["idempotency_key"]["maxLength"], 512);
            assert_eq!(
                schema["properties"]["expected_state"]["pattern"],
                "^sha256:[0-9a-f]{64}$"
            );
            assert_eq!(
                schema["properties"]["dry_run"]["default"],
                json!(matches!(
                    kind,
                    SourceEditKind::MoveSymbol | SourceEditKind::RenameSymbol
                ))
            );
        }
    }

    #[test]
    fn source_edit_effect_inputs_are_required_only_for_apply() {
        for definition in [
            handwritten("tracedecay_str_replace"),
            handwritten("tracedecay_multi_str_replace"),
            handwritten("tracedecay_insert_at"),
            handwritten("tracedecay_ast_grep_rewrite"),
            handwritten("tracedecay_replace_symbol"),
            handwritten("tracedecay_insert_at_symbol"),
        ] {
            let schema = definition.input_schema;
            assert!(schema["properties"]["idempotency_key"].is_object());
            assert!(schema["properties"]["expected_state"].is_object());
            assert_eq!(
                schema["allOf"][0]["then"]["required"],
                json!(["idempotency_key", "expected_state"])
            );
            assert_eq!(
                schema["allOf"][0]["if"]["properties"]["dry_run"]["const"],
                json!(false)
            );
        }
        for definition in [
            handwritten("tracedecay_move_symbol"),
            handwritten("tracedecay_rename_symbol"),
        ] {
            let schema = definition.input_schema;
            assert_eq!(schema["allOf"][0]["if"]["required"], json!(["dry_run"]));
            assert_eq!(
                schema["allOf"][0]["then"]["required"],
                json!(["idempotency_key", "expected_state"])
            );
        }
    }

    #[test]
    fn rename_acceptance_requires_the_exact_preview_repository_revision() {
        let definition = handwritten("tracedecay_rename_symbol");
        assert!(
            definition.input_schema["properties"]["accepted_preview"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("repository_revision"))
        );

        let preview = serde_json::to_value(tracedecay_contracts::RenameResult::default()).unwrap();
        assert_eq!(
            preview.get("repository_revision"),
            Some(&serde_json::Value::Null)
        );
        let acceptance: tracedecay_contracts::RenamePreviewAcceptanceV1 =
            serde_json::from_value(json!({
                "preview_id": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "preview_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "plan_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "graph_revision": "sha256:3333333333333333333333333333333333333333333333333333333333333333"
            }))
            .unwrap();
        let acceptance = serde_json::to_value(acceptance).unwrap();
        assert_eq!(
            acceptance.get("repository_revision"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn source_edit_reconciliation_requires_exact_identity_and_confirmation() {
        let schema = handwritten("tracedecay_source_edit_reconcile").input_schema;
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["required"],
            json!([
                "kind",
                "effect_id",
                "idempotency_key",
                "attempt_idempotency_key",
                "input_digest",
                "disposition",
                "confirm"
            ])
        );
        assert_eq!(schema["properties"]["confirm"]["const"], json!(true));
        assert_eq!(
            schema["allOf"][0]["then"]["required"],
            json!(["committed_state"])
        );
        assert_eq!(
            schema["allOf"][0]["if"]["properties"]["disposition"]["const"],
            json!("confirm_committed")
        );
        assert_eq!(
            schema["allOf"][0]["else"]["not"]["required"],
            json!(["committed_state"])
        );
    }

    #[test]
    fn source_edit_rollback_requires_original_and_fresh_effect_identity() {
        let schema = handwritten("tracedecay_source_edit_rollback").input_schema;
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["required"],
            json!([
                "effect_id",
                "original_idempotency_key",
                "idempotency_key",
                "original_input_digest",
                "expected_state",
                "confirm"
            ])
        );
        assert_eq!(schema["properties"]["confirm"]["const"], json!(true));
        assert_eq!(
            schema["properties"]["original_input_digest"]["pattern"],
            "^sha256:[0-9a-f]{64}$"
        );
        assert_eq!(
            schema["properties"]["expected_state"]["pattern"],
            "^sha256:[0-9a-f]{64}$"
        );
    }
}
