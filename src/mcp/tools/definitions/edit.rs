//! Edit/refactor tool definitions.

use serde_json::{Value, json};

use super::{def, def_rw};
use crate::mcp::tools::ToolDefinition;

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

pub(super) fn def_source_edit_reconcile() -> ToolDefinition {
    def_rw(
        "tracedecay_source_edit_reconcile",
        "Reconcile Source Edit",
        "ADMIN: conclude one retained source-edit EffectUnknown after independently inspecting the exact current candidate-file state. This never retries the edit. It requires the original durable effect identity and explicit confirmation.",
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
                    "description": "Explicit acknowledgement that this concludes a retained uncertain effect without retrying it."
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
        }),
    )
}

pub(super) fn def_str_replace() -> ToolDefinition {
    ToolDefinition {
        name: "tracedecay_str_replace".to_string(),
        description: "Replace a unique string in a file with new content. Fails if the old string is not found or matches more than once. This is the safest edit primitive — use this instead of sed/awk.".to_string(),
        input_schema: source_edit_schema(json!({
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
        })),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Edit File"
        })),
        meta: None,
    }
}

pub(super) fn def_multi_str_replace() -> ToolDefinition {
    ToolDefinition {
        name: "tracedecay_multi_str_replace".to_string(),
        description: "Apply multiple string replacements atomically in a single file. All replacements must match exactly once. If any replacement fails (0 or >1 matches), the entire operation is aborted and no changes are made.".to_string(),
        input_schema: source_edit_schema(json!({
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
        })),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Multi-Edit File"
        })),
        meta: None,
    }
}

pub(super) fn def_insert_at() -> ToolDefinition {
    ToolDefinition {
        name: "tracedecay_insert_at".to_string(),
        description: "Insert content before or after a unique anchor in a file. The anchor can be a unique string or a 1-indexed line number. Fails if the anchor matches more than one line.".to_string(),
        input_schema: source_edit_schema(json!({
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
        })),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Insert Into File"
        })),
        meta: None,
    }
}

pub(super) fn def_rename_preview() -> ToolDefinition {
    def(
        "tracedecay_rename_preview",
        "Rename Preview",
        "READ-ONLY preview of what a rename would touch. Given a symbol node and \
         an optional `new_name`, returns the declaration site plus every graph \
         reference site (from call/use/etc. edges), each with its current-text \
         line snippet, and a per-file count of literal textual occurrences of \
         the name that are NOT graph references ('text-only matches — review \
         manually'). It does NOT edit anything and does NOT rewrite occurrences \
         — pass the reported node identity to `tracedecay_rename_symbol` to \
         apply the rename. Graph call-edge coverage improves as the resolver \
         does; text-only counts catch what the graph misses (comments, \
         strings, dynamic dispatch, unresolved refs).",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID of the symbol to preview renaming"
                },
                "new_name": {
                    "type": "string",
                    "description": "Proposed new name. Optional — only used to label the preview; no text is rewritten."
                }
            },
            "required": ["node_id"]
        }),
    )
}

pub(super) fn def_rename_symbol() -> ToolDefinition {
    def_rw(
        "tracedecay_rename_symbol",
        "Rename Symbol",
        "Apply-grade rename of a graph-bound symbol across its declaration and \
         every graph reference site. Consumes the exact identity reported by \
         `tracedecay_rename_preview` (node_id, qualified_name, kind, file, \
         old_name) — a bare spelling is never sufficient — and rewrites \
         whole-identifier occurrences only on graph-attested lines. Text-only \
         matches (comments, strings, dynamic dispatch, unresolved refs) are \
         reported and never rewritten. Defaults to a DRY RUN returning the \
         per-file plan, diff, and the expected_state digest; applying is \
         opt-in via `dry_run: false` with a fresh `idempotency_key` and that \
         `expected_state`. The apply refuses stale identity or drifted files \
         before any write, and a partial failure restores every preimage.",
        {
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
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true (DEFAULT), bind every site and compute the full plan, per-file diff, and expected_state but write nothing. Set false to apply."
                    },
                    "verify": {
                        "type": "boolean",
                        "description": "If true, re-run file-scoped diagnostics after a real rename and include a compact verdict. Ignored for dry runs. Default: false."
                    }
                },
                "required": ["node_id", "qualified_name", "kind", "file", "old_name", "new_name"]
            }));
            // Like move_symbol, rename defaults to preview; only an explicit
            // dry_run=false selects the apply branch.
            schema["allOf"][0]["if"]["required"] = json!(["dry_run"]);
            schema["properties"]["dry_run"]["default"] = json!(true);
            schema
        },
    )
}

pub(super) fn def_ast_grep_search() -> ToolDefinition {
    def(
        "tracedecay_ast_grep_search",
        "AST Structural Search",
        "Structural (AST) code search: find call shapes, argument orders, and other \
         syntax-tree patterns that a text regex cannot express. Uses ast-grep's SGPattern \
         syntax (metavariables: `$X` one node, `$$$` many). Runs IN-PROCESS over the project \
         working tree using the bundled tree-sitter grammars — no external ast-grep binary, no \
         gating. Each hit resolves its enclosing symbol, so the natural next call is \
         tracedecay_body. Routing: use this when the pattern is structural (e.g. `foo($$$)`, \
         `if ($C) { $$$ }`); for a literal/regex string use tracedecay_grep; for a symbol name \
         use tracedecay_search. To rewrite a structural match, pair with tracedecay_ast_grep_rewrite.",
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "ast-grep structural pattern (SGPattern syntax), e.g. 'reserve_stock($$$)' or 'Result<$T, $E>'."
                },
                "lang": {
                    "type": "string",
                    "description": "Optional language key to force (e.g. 'rust', 'typescript', 'python'). Omit to auto-detect each file from its extension."
                },
                "path_glob": {
                    "type": "string",
                    "description": "Optional glob restricting which files are searched, matched against project-relative paths (e.g. 'src/**/*.rs')."
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum number of matches to return (default: 50, max: 200)."
                }
            },
            "required": ["pattern"]
        }),
    )
}

pub(super) fn def_ast_grep_rewrite() -> ToolDefinition {
    ToolDefinition {
        name: "tracedecay_ast_grep_rewrite".to_string(),
        description: "Perform structural code rewrite using the host ast-grep CLI. The pattern and rewrite use ast-grep's SGPattern syntax. This tool is advertised only when that CLI is available.".to_string(),
        input_schema: source_edit_schema(json!({
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
                    "description": "If true, preview the rewrite without applying it: ast-grep runs without --update-all (or the built-in literal fallback computes a diff) and the response returns the would-be change, writing nothing (default: false)."
                },
                "verify": {
                    "type": "boolean",
                    "description": "If true, re-run file-scoped diagnostics after a real rewrite and include a compact verdict (clean / N new errors) in the response. Ignored for dry runs. Default: false to keep edits fast."
                }
            },
            "required": ["path", "pattern", "rewrite"]
        })),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "AST Structural Rewrite"
        })),
        meta: None,
    }
}

pub(super) fn def_replace_symbol() -> ToolDefinition {
    def_rw(
        "tracedecay_replace_symbol",
        "Replace Symbol Source",
        "Replace the full source of a named symbol (function, method, struct, \
         enum, etc.) with new source text. Resolves the symbol via exact \
         qualified-name match; on ambiguity, callable kinds win, and if \
         still ambiguous the edit is refused. The replaced span covers the \
         item's LEADING doc-comment / attribute block (e.g. `///` docs, `#[...]` \
         attributes) as well as its body, so `new_source` must itself include \
         any docs/attributes you want to keep — otherwise they are dropped. \
         The result's `replaced_span` returns the exact text that was swapped \
         out (docs/attrs included) so you can recover them; a `message` note \
         flags when the old span had docs/attrs the replacement appears to omit. \
         Preserves the surrounding file untouched and reindexes after writing.",
        source_edit_schema(json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name. Prefer a fully qualified name for disambiguation."
                },
                "new_source": {
                    "type": "string",
                    "description": "Full replacement source — must include the symbol's own declaration line, plus any leading doc-comments/attributes to preserve (the replaced span includes the old ones)."
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
        })),
    )
}

pub(super) fn def_move_symbol() -> ToolDefinition {
    def_rw(
        "tracedecay_move_symbol",
        "Move Symbol Across Files",
        "Move a function (Rust-first) from its file to a destination file, and \
         — the centerpiece — report the full IMPACT of the move: every caller \
         whose reference breaks, every dependency the body loses at the \
         destination, visibility that must be escalated, destination collisions, \
         and missing module declarations. Each finding is an evidence-based hint \
         { kind, file, line, detail, suggestion } derived from the code graph \
         (callers/callees) and parse-level facts (identifiers, `use` lines), not \
         regex guessing. The moved span includes the item's leading \
         doc-comment / attribute block. Defaults to a DRY RUN — the report and a \
         combined source+destination preview diff are the product; applying is \
         opt-in via `dry_run: false`, which removes the span from the source, \
         inserts it at the destination, and auto-inserts unambiguous needed \
         imports (returned in `applied_imports`). Caller references are never \
         auto-edited in v1; the exact change rides in each hint.",
        {
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
        },
    )
}

pub(super) fn def_insert_at_symbol() -> ToolDefinition {
    def_rw(
        "tracedecay_insert_at_symbol",
        "Insert Near Symbol",
        "Insert content immediately before or after a named symbol's source \
         range. Same resolution semantics as `tracedecay_replace_symbol`. \
         Use `position=\"before\"` or `position=\"after\"` (default: after).",
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
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::SourceEditKind;

    fn source_edit_definitions() -> [(ToolDefinition, SourceEditKind); 8] {
        [
            (def_str_replace(), SourceEditKind::StrReplace),
            (def_multi_str_replace(), SourceEditKind::MultiStrReplace),
            (def_insert_at(), SourceEditKind::InsertAt),
            (def_ast_grep_rewrite(), SourceEditKind::AstGrepRewrite),
            (def_replace_symbol(), SourceEditKind::ReplaceSymbol),
            (def_insert_at_symbol(), SourceEditKind::InsertAtSymbol),
            (def_move_symbol(), SourceEditKind::MoveSymbol),
            (def_rename_symbol(), SourceEditKind::RenameSymbol),
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
            def_str_replace(),
            def_multi_str_replace(),
            def_insert_at(),
            def_ast_grep_rewrite(),
            def_replace_symbol(),
            def_insert_at_symbol(),
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
        for definition in [def_move_symbol(), def_rename_symbol()] {
            let schema = definition.input_schema;
            assert_eq!(schema["allOf"][0]["if"]["required"], json!(["dry_run"]));
            assert_eq!(
                schema["allOf"][0]["then"]["required"],
                json!(["idempotency_key", "expected_state"])
            );
        }
    }

    #[test]
    fn source_edit_reconciliation_requires_exact_identity_and_confirmation() {
        let schema = def_source_edit_reconcile().input_schema;
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
}
