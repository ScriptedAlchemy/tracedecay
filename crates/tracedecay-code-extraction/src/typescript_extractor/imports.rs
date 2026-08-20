use tracedecay_domain::SourceSpan;
use tree_sitter::Node as TsNode;

use crate::extraction_artifact::{
    ExtractedImportEvidenceV1, ImportModuleKindV1, ImportNamespaceV1,
};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{Edge, EdgeKind, Node, NodeKind, UnresolvedRef, Visibility, generate_node_id};

use super::ExtractionState;

/// Preserve the statement-level graph row while collecting binding evidence
/// from the same parser node. Binding rows never become graph nodes.
pub(super) fn visit_import(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
    let text = state.node_text(node);
    let module_specifier = extract_module_specifier(state, node);
    let name = match &module_specifier {
        Some(module_specifier) => module_specifier.clone(),
        None => text.to_string(),
    };
    let start_line = node.start_position().row as u32;
    let end_line = node.end_position().row as u32;
    let start_column = node.start_position().column as u32;
    let end_column = node.end_position().column as u32;
    let qualified_name = format!("{}::{}", state.qualified_prefix(), name);
    let statement_identity = format!("{name}@{start_column}");
    let id = generate_node_id(
        &state.file_path,
        &NodeKind::Use,
        &statement_identity,
        start_line,
    );

    state.nodes.push(Node {
        id: id.clone(),
        kind: NodeKind::Use,
        name: name.clone(),
        qualified_name,
        file_path: state.file_path.clone(),
        start_line,
        attrs_start_line: start_line,
        end_line,
        start_column,
        end_column,
        signature: Some(text.trim().to_string()),
        docstring: None,
        visibility: Visibility::Private,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: state.timestamp,
        parent_id: None,
    });

    if let Some(parent_id) = state.parent_node_id() {
        state.edges.push(Edge {
            source: parent_id.to_string(),
            target: id.clone(),
            kind: EdgeKind::Contains,
            line: Some(start_line),
        });
    }

    state.unresolved_refs.push(UnresolvedRef {
        from_node_id: id,
        reference_name: name,
        reference_kind: EdgeKind::Uses,
        line: start_line,
        column: start_column,
        file_path: state.file_path.clone(),
    });

    let Some(module_specifier) = module_specifier else {
        return;
    };
    let module_kind = classify_module(&module_specifier);
    let statement_namespace =
        if has_unnamed_child_kind(node, "type") || has_unnamed_child_kind(node, "typeof") {
            ImportNamespaceV1::Type
        } else {
            ImportNamespaceV1::Value
        };

    let Some(clause) = find_direct_child_by_kind(node, "import_clause") else {
        push_evidence(
            state,
            &module_specifier,
            (None, None),
            ImportNamespaceV1::SideEffect,
            module_kind,
            node,
        );
        return;
    };

    let mut cursor = clause.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "identifier" => {
                let local_name = state.node_text(child).to_string();
                push_evidence(
                    state,
                    &module_specifier,
                    (Some("default".to_owned()), Some(local_name)),
                    statement_namespace,
                    module_kind,
                    child,
                );
            }
            "named_imports" => visit_named_imports(
                state,
                child,
                &module_specifier,
                statement_namespace,
                module_kind,
            ),
            "namespace_import" => visit_namespace_import(
                state,
                child,
                &module_specifier,
                statement_namespace,
                module_kind,
            ),
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn visit_named_imports(
    state: &mut ExtractionState<'_>,
    named_imports: TsNode<'_>,
    module_specifier: &str,
    statement_namespace: ImportNamespaceV1,
    module_kind: ImportModuleKindV1,
) {
    let mut cursor = named_imports.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let specifier = cursor.node();
        if specifier.kind() == "import_specifier"
            && let Some(name_node) = specifier.child_by_field_name("name")
        {
            let imported_name = binding_name(state, name_node);
            let local_name = match specifier.child_by_field_name("alias") {
                Some(alias) => binding_name(state, alias),
                None => imported_name.clone(),
            };
            let namespace = if statement_namespace == ImportNamespaceV1::Type
                || has_unnamed_child_kind(specifier, "type")
                || has_unnamed_child_kind(specifier, "typeof")
            {
                ImportNamespaceV1::Type
            } else {
                ImportNamespaceV1::Value
            };
            push_evidence(
                state,
                module_specifier,
                (Some(imported_name), Some(local_name)),
                namespace,
                module_kind,
                specifier,
            );
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn visit_namespace_import(
    state: &mut ExtractionState<'_>,
    namespace_import: TsNode<'_>,
    module_specifier: &str,
    namespace: ImportNamespaceV1,
    module_kind: ImportModuleKindV1,
) {
    let Some(local) = find_direct_child_by_kind(namespace_import, "identifier") else {
        return;
    };
    let local_name = state.node_text(local).to_string();
    push_evidence(
        state,
        module_specifier,
        (Some("*".to_owned()), Some(local_name)),
        namespace,
        module_kind,
        namespace_import,
    );
}

fn push_evidence(
    state: &mut ExtractionState<'_>,
    module_specifier: &str,
    names: (Option<String>, Option<String>),
    namespace: ImportNamespaceV1,
    module_kind: ImportModuleKindV1,
    evidence_node: TsNode<'_>,
) {
    let Some((span, start_line, start_column)) = evidence_location(state, evidence_node) else {
        return;
    };
    state.imports.push(ExtractedImportEvidenceV1 {
        logical_path: state.file_path.clone(),
        module_specifier: module_specifier.to_owned(),
        imported_name: names.0,
        local_name: names.1,
        namespace,
        module_kind,
        span,
        start_line,
        start_column,
    });
}

fn evidence_location(
    state: &mut ExtractionState<'_>,
    node: TsNode<'_>,
) -> Option<(SourceSpan, u32, u32)> {
    let start_byte = match u64::try_from(node.start_byte()) {
        Ok(value) => value,
        Err(_) => {
            state
                .errors
                .push("TypeScript import start byte exceeds canonical span width".to_owned());
            return None;
        }
    };
    let end_byte = match u64::try_from(node.end_byte()) {
        Ok(value) => value,
        Err(_) => {
            state
                .errors
                .push("TypeScript import end byte exceeds canonical span width".to_owned());
            return None;
        }
    };
    let start_line = match u32::try_from(node.start_position().row) {
        Ok(value) => value,
        Err(_) => {
            state
                .errors
                .push("TypeScript import row exceeds canonical coordinate width".to_owned());
            return None;
        }
    };
    let start_column = match u32::try_from(node.start_position().column) {
        Ok(value) => value,
        Err(_) => {
            state
                .errors
                .push("TypeScript import column exceeds canonical coordinate width".to_owned());
            return None;
        }
    };
    Some((
        SourceSpan {
            start_byte,
            end_byte,
        },
        start_line,
        start_column,
    ))
}

fn extract_module_specifier(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
    node.child_by_field_name("source")
        .and_then(|source| unquote(state.node_text(source)))
}

fn binding_name(state: &ExtractionState<'_>, node: TsNode<'_>) -> String {
    let text = state.node_text(node);
    match unquote(text) {
        Some(unquoted) => unquoted,
        None => text.to_string(),
    }
}

fn unquote(text: &str) -> Option<String> {
    text.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .filter(|inner| !inner.is_empty())
        .map(str::to_owned)
}

fn has_unnamed_child_kind(node: TsNode<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        if !child.is_named() && child.kind() == kind {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

fn classify_module(module_specifier: &str) -> ImportModuleKindV1 {
    if module_specifier.starts_with("./") || module_specifier.starts_with("../") {
        ImportModuleKindV1::ProjectRelative
    } else {
        ImportModuleKindV1::BareModule
    }
}
