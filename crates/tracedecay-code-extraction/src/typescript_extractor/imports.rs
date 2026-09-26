use tracedecay_domain::SourceSpan;
use tree_sitter::Node as TsNode;

use crate::common::local_node_id;
use crate::extraction_artifact::{
    ExtractedImportEvidenceV1, ImportModuleKindV1, ImportNamespaceV1, import_module_kind,
};
use crate::traversal::find_direct_child_by_kind;
use crate::types::{
    ComplexityAnalysisV1, Edge, EdgeKind, Node, NodeKind, UnresolvedRef, Visibility,
};

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
    let id = local_node_id(&state.file_path, state.source, &NodeKind::Use, &name, node);

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
        complexity_analysis: ComplexityAnalysisV1::Complete,
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
    let Some(module_kind) = import_module_kind("typescript", &module_specifier) else {
        return;
    };
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
            BindingShape::IMPORT,
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
                    BindingShape::IMPORT,
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

/// `export … from "m"` forwards another module's bindings without binding
/// them locally. Each forwarded name is public import evidence so the seal's
/// re-export walk can follow a barrel to the defining file: `export { a as b }
/// from` keeps `a` as the imported name and `b` as the exported (local) name,
/// `export * from` is a public glob, and `export * as ns from` exports the
/// namespace under `ns`.
///
/// A same-module clause (`function a() {}; export { a as b }`) forwards this
/// module's own binding, so its rows name the declaring file itself
/// (`./util.ts`): the seal resolves that specifier back to this file and
/// looks `a` up among its declarations and local imports.
pub(super) fn visit_reexport(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
    let module_specifier = match extract_module_specifier(state, node) {
        Some(module_specifier) => module_specifier,
        None if find_direct_child_by_kind(node, "export_clause").is_some() => {
            let file_name = state.file_path.rsplit('/').next().unwrap_or_default();
            if file_name.is_empty() {
                return;
            }
            format!("./{file_name}")
        }
        None => return,
    };
    let Some(module_kind) = import_module_kind("typescript", &module_specifier) else {
        return;
    };
    let namespace = if has_unnamed_child_kind(node, "type") {
        ImportNamespaceV1::Type
    } else {
        ImportNamespaceV1::Value
    };
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "export_clause" => {
                let mut specifiers = child.walk();
                if specifiers.goto_first_child() {
                    loop {
                        let specifier = specifiers.node();
                        if specifier.kind() == "export_specifier"
                            && let Some(name_node) = specifier.child_by_field_name("name")
                        {
                            let imported_name = binding_name(state, name_node);
                            let exported_name = match specifier.child_by_field_name("alias") {
                                Some(alias) => binding_name(state, alias),
                                None => imported_name.clone(),
                            };
                            let namespace = if namespace == ImportNamespaceV1::Type
                                || has_unnamed_child_kind(specifier, "type")
                            {
                                ImportNamespaceV1::Type
                            } else {
                                ImportNamespaceV1::Value
                            };
                            push_evidence(
                                state,
                                &module_specifier,
                                (Some(imported_name), Some(exported_name)),
                                BindingShape::REEXPORT,
                                namespace,
                                module_kind,
                                specifier,
                            );
                        }
                        if !specifiers.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "namespace_export" => {
                if let Some(local) = find_direct_child_by_kind(child, "identifier") {
                    let exported_name = state.node_text(local).to_string();
                    push_evidence(
                        state,
                        &module_specifier,
                        (Some("*".to_owned()), Some(exported_name)),
                        BindingShape::REEXPORT,
                        namespace,
                        module_kind,
                        child,
                    );
                }
            }
            "*" => push_evidence(
                state,
                &module_specifier,
                (Some("*".to_owned()), None),
                BindingShape::REEXPORT_GLOB,
                namespace,
                module_kind,
                node,
            ),
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// `export default <name>` and `export default function|class <name>` export
/// a module-scope binding under the name `default`. Like a same-module export
/// clause, the row names the declaring file itself (`./util.ts`), so the seal
/// resolves `default` back to that binding: a declaration here or a local
/// import. An anonymous default export names no binding and records nothing.
pub(super) fn visit_default_export(state: &mut ExtractionState<'_>, node: TsNode<'_>) {
    if node.child_by_field_name("source").is_some() || !has_unnamed_child_kind(node, "default") {
        return;
    }
    let declaration = node.child_by_field_name("declaration");
    let name_node = match declaration {
        Some(declaration) => declaration.child_by_field_name("name"),
        None => node
            .child_by_field_name("value")
            .filter(|value| value.kind() == "identifier"),
    };
    let Some(name_node) = name_node else {
        return;
    };
    let namespace = if declaration.is_some_and(|declaration| {
        matches!(
            declaration.kind(),
            "interface_declaration" | "type_alias_declaration"
        )
    }) {
        ImportNamespaceV1::Type
    } else {
        ImportNamespaceV1::Value
    };
    let file_name = state.file_path.rsplit('/').next().unwrap_or_default();
    if file_name.is_empty() {
        return;
    }
    let module_specifier = format!("./{file_name}");
    let Some(module_kind) = import_module_kind("typescript", &module_specifier) else {
        return;
    };
    let local_name = state.node_text(name_node).to_string();
    push_evidence(
        state,
        &module_specifier,
        (Some(local_name), Some("default".to_owned())),
        BindingShape::REEXPORT,
        namespace,
        module_kind,
        name_node,
    );
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
                BindingShape::IMPORT,
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
        BindingShape::IMPORT,
        namespace,
        module_kind,
        namespace_import,
    );
}

/// Whether a binding row is a local import or a forwarded re-export, and
/// whether it names every export of its module.
#[derive(Clone, Copy)]
struct BindingShape {
    is_public: bool,
    is_glob: bool,
}

impl BindingShape {
    const IMPORT: Self = Self {
        is_public: false,
        is_glob: false,
    };
    const REEXPORT: Self = Self {
        is_public: true,
        is_glob: false,
    };
    const REEXPORT_GLOB: Self = Self {
        is_public: true,
        is_glob: true,
    };
}

fn push_evidence(
    state: &mut ExtractionState<'_>,
    module_specifier: &str,
    names: (Option<String>, Option<String>),
    shape: BindingShape,
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
        is_public: shape.is_public,
        reexport_scope: None,
        is_glob: shape.is_glob,
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
