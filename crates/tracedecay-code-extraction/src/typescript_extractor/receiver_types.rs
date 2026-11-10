//! Parser-attested instance receiver facts. Unsupported values remain calls
//! with unresolved receivers; no method is selected from its spelling alone.

use tracedecay_domain::SourceSpan;
use tree_sitter::Node as TsNode;

use super::{ExtractionState, TypeScriptExtractor};
use crate::extraction_artifact::{
    ExtractedTypeScriptTypeRowV1, TypeScriptReceiverV1, TypeScriptTypeRowV1,
};

fn is_function(node: TsNode<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
    )
}

fn is_scope(node: TsNode<'_>) -> bool {
    is_function(node)
        || matches!(
            node.kind(),
            "program"
                | "statement_block"
                | "catch_clause"
                | "for_statement"
                | "for_in_statement"
                | "class"
                | "class_body"
                | "switch_body"
        )
}

fn contains_name(state: &ExtractionState<'_>, node: TsNode<'_>, name: &str) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern" | "type_identifier"
    ) && state.node_text(node) == name
    {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| contains_name(state, child, name))
}

/// Collect the declarations belonging to one lexical scope. Nested scopes
/// cannot supply a sibling's receiver. Function-scoped `var` is a blocker,
/// including a declaration nested under a block we otherwise do not enter.
fn bindings_in_scope<'t>(
    state: &ExtractionState<'_>,
    scope: TsNode<'t>,
    node: TsNode<'t>,
    name: &str,
    found: &mut Vec<TsNode<'t>>,
) {
    let binding = match node.kind() {
        "variable_declarator" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" | "rest_parameter" => {
            node.child_by_field_name("pattern")
        }
        "catch_clause" => node.child_by_field_name("parameter"),
        "import_specifier" => node
            .child_by_field_name("alias")
            .or_else(|| node.child_by_field_name("name")),
        "namespace_import" => node.named_child(0),
        "function_declaration"
        | "generator_function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
            if node != scope =>
        {
            node.child_by_field_name("name")
        }
        _ => None,
    };
    if binding.is_some_and(|binding| contains_name(state, binding, name)) {
        found.push(node);
    }
    if node != scope && is_scope(node) {
        return;
    }
    // Named function/class expressions bind their own name in their body.
    if node == scope
        && (is_function(node) || node.kind() == "class")
        && node
            .child_by_field_name("name")
            .is_some_and(|bound| contains_name(state, bound, name))
    {
        found.push(node);
    }
    if is_function(node) {
        if let Some(parameter) = node.child_by_field_name("parameter")
            && contains_name(state, parameter, name)
        {
            found.push(parameter);
        }
    }
    if node.kind() == "formal_parameters" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "identifier"
                    | "object_pattern"
                    | "array_pattern"
                    | "rest_pattern"
                    | "assignment_pattern"
            ) && contains_name(state, child, name)
            {
                found.push(child);
            }
        }
    }
    if node.kind() == "import_clause" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "identifier" && state.node_text(child) == name {
                found.push(child);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        bindings_in_scope(state, scope, child, name, found);
    }
}

fn has_var_binding(
    state: &ExtractionState<'_>,
    root: TsNode<'_>,
    node: TsNode<'_>,
    name: &str,
) -> bool {
    if node != root && is_function(node) {
        return false;
    }
    if node.kind() == "variable_declaration" {
        let mut cursor = node.walk();
        if node
            .named_children(&mut cursor)
            .filter_map(|child| child.child_by_field_name("name"))
            .any(|binding| contains_name(state, binding, name))
        {
            return true;
        }
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| has_var_binding(state, root, child, name))
}

/// `Some(None)` is a shadow or ambiguous binding, never permission to look
/// farther out. Both use-before-declaration and any write in the binding's
/// scope abstain, including writes in closures.
fn binding<'t>(
    state: &ExtractionState<'_>,
    site: TsNode<'t>,
    name: &str,
) -> Option<Option<(TsNode<'t>, TsNode<'t>)>> {
    let mut parent = site.parent();
    while let Some(scope) = parent {
        if is_scope(scope) {
            if (is_function(scope) || scope.kind() == "program")
                && has_var_binding(state, scope, scope, name)
            {
                return Some(None);
            }
            let mut found = Vec::new();
            bindings_in_scope(state, scope, scope, name, &mut found);
            if !found.is_empty() {
                return Some(match found.as_slice() {
                    [declaration] if !declaration.has_error() => Some((*declaration, scope)),
                    _ => None,
                });
            }
        }
        parent = scope.parent();
    }
    None
}

fn has_write(state: &ExtractionState<'_>, node: TsNode<'_>, name: &str) -> bool {
    let target = match node.kind() {
        "assignment_expression" | "augmented_assignment_expression" | "for_in_statement" => {
            node.child_by_field_name("left")
        }
        "update_expression" => node.child_by_field_name("argument"),
        _ => None,
    };
    // Writing x.field does not rebind x; destructuring assignments do.
    if target.is_some_and(|target| {
        target.kind() != "member_expression"
            && target.kind() != "subscript_expression"
            && contains_name(state, target, name)
    }) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| has_write(state, child, name))
}

fn named_type(state: &ExtractionState<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "type_annotation" | "parenthesized_type" => named_type(state, node.named_child(0)?),
        "type_identifier" | "identifier" => Some(state.node_text(node).to_owned()),
        "nested_type_identifier" | "member_expression" => {
            let left = node
                .child_by_field_name("module")
                .or_else(|| node.child_by_field_name("object"))?;
            let right = node
                .child_by_field_name("name")
                .or_else(|| node.child_by_field_name("property"))?;
            let left = named_type(state, left)?;
            if !matches!(
                right.kind(),
                "type_identifier" | "identifier" | "property_identifier"
            ) {
                return None;
            }
            Some(format!("{left}.{}", state.node_text(right)))
        }
        _ => None,
    }
}

/// This slice resolves module declarations/imports only. A local type,
/// value, or type parameter that shadows the head makes the fact unusable.
fn module_name(state: &ExtractionState<'_>, site: TsNode<'_>, name: String) -> Option<String> {
    let head = name.split('.').next()?;
    if let Some(bound) = binding(state, site, head) {
        let (_, scope) = bound?;
        if scope.kind() != "program" || has_write(state, scope, head) {
            return None;
        }
    }
    let mut parent = site.parent();
    while let Some(node) = parent {
        if let Some(parameters) = node.child_by_field_name("type_parameters")
            && contains_name(state, parameters, head)
        {
            return None;
        }
        parent = node.parent();
    }
    Some(name)
}

fn unwrap_expression(mut node: TsNode<'_>) -> Option<TsNode<'_>> {
    while node.kind() == "parenthesized_expression" {
        node = node.named_child(0)?;
    }
    Some(node)
}

fn initializer_receiver(
    state: &ExtractionState<'_>,
    node: TsNode<'_>,
) -> Option<TypeScriptReceiverV1> {
    let node = unwrap_expression(node)?;
    match node.kind() {
        "new_expression" => {
            let constructor = node.child_by_field_name("constructor")?;
            let name = module_name(state, constructor, named_type(state, constructor)?)?;
            Some(TypeScriptReceiverV1::Named {
                name,
                constructor: true,
            })
        }
        "await_expression" => {
            let call = unwrap_expression(node.named_child(0)?)?;
            if call.kind() != "call_expression" {
                return None;
            }
            let callee = call
                .child_by_field_name("function")
                .or_else(|| call.named_child(0))?;
            if callee.kind() != "identifier" {
                return None;
            }
            let name = module_name(state, callee, state.node_text(callee).to_owned())?;
            Some(TypeScriptReceiverV1::AwaitedFactory { name })
        }
        _ => None,
    }
}

fn receiver(
    state: &ExtractionState<'_>,
    object: TsNode<'_>,
    site: TsNode<'_>,
) -> Option<TypeScriptReceiverV1> {
    let object = unwrap_expression(object)?;
    if object.kind() != "identifier" {
        return initializer_receiver(state, object);
    }
    let name = state.node_text(object);
    let (declaration, scope) = binding(state, site, name)??;
    if has_write(state, scope, name) {
        return None;
    }
    let pattern = match declaration.kind() {
        "variable_declarator" => declaration.child_by_field_name("name")?,
        "required_parameter" => declaration.child_by_field_name("pattern")?,
        _ => return None,
    };
    if pattern.kind() != "identifier" {
        return None;
    }
    if declaration.kind() == "variable_declarator"
        && declaration
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration")
    {
        return None;
    }
    if declaration.kind() == "variable_declarator" && declaration.end_byte() > site.start_byte() {
        return None;
    }
    if let Some(annotation) = declaration.child_by_field_name("type") {
        let name = module_name(state, annotation, named_type(state, annotation)?)?;
        return Some(TypeScriptReceiverV1::Named {
            name,
            constructor: false,
        });
    }
    // Initializer inference requires const; a mutable value without a
    // declared type does not attest its type throughout a captured closure.
    if declaration.parent()?.kind() != "lexical_declaration"
        || !TypeScriptExtractor::has_child_kind(declaration.parent()?, "const")
    {
        return None;
    }
    initializer_receiver(state, declaration.child_by_field_name("value")?)
}

pub(super) fn record_call(state: &mut ExtractionState<'_>, callee: TsNode<'_>, owner: &str) {
    if callee.kind() != "member_expression"
        || callee.has_error()
        || TypeScriptExtractor::has_child_kind(callee, "optional_chain")
    {
        return;
    }
    let Some(property) = callee.child_by_field_name("property") else {
        return;
    };
    if property.kind() != "property_identifier" {
        return;
    }
    let Some(object) = callee.child_by_field_name("object") else {
        return;
    };
    let Some(receiver) = receiver(state, object, callee) else {
        return;
    };
    // Computed receiver references are retained at the property token by
    // the chunker; plain identifier receivers retain the full callee span.
    let evidence = if object.kind() == "identifier" {
        callee
    } else {
        property
    };
    state.typescript_types.push(ExtractedTypeScriptTypeRowV1 {
        node_id: owner.to_owned(),
        row: TypeScriptTypeRowV1::ReceiverCall {
            span: SourceSpan {
                start_byte: evidence.start_byte() as u64,
                end_byte: evidence.end_byte() as u64,
            },
            receiver,
        },
    });
}

pub(super) fn record_callable(
    state: &mut ExtractionState<'_>,
    node: TsNode<'_>,
    owner: &str,
    instance: bool,
) {
    if node.has_error() {
        return;
    }
    if instance
        && !["static", "get", "set"]
            .iter()
            .any(|kind| TypeScriptExtractor::has_child_kind(node, kind))
    {
        state.typescript_types.push(ExtractedTypeScriptTypeRowV1 {
            node_id: owner.to_owned(),
            row: TypeScriptTypeRowV1::InstanceMethod,
        });
    }
    let Some(annotation) = node.child_by_field_name("return_type") else {
        return;
    };
    let Some(mut ty) = annotation.named_child(0) else {
        return;
    };
    let promise = ty.kind() == "generic_type"
        && ty
            .child_by_field_name("name")
            .is_some_and(|name| state.node_text(name) == "Promise");
    if promise {
        // A project declaration called Promise is not the built-in wrapper.
        if binding(state, ty, "Promise").is_some()
            || module_name(state, ty, "Promise".to_owned()).is_none()
        {
            return;
        }
        let Some(arguments) = ty.child_by_field_name("type_arguments") else {
            return;
        };
        if arguments.named_child_count() != 1 {
            return;
        }
        let Some(inner) = arguments.named_child(0) else {
            return;
        };
        ty = inner;
    }
    let Some(name) = named_type(state, ty).and_then(|name| module_name(state, ty, name)) else {
        return;
    };
    state.typescript_types.push(ExtractedTypeScriptTypeRowV1 {
        node_id: owner.to_owned(),
        row: TypeScriptTypeRowV1::CallableReturn { name, promise },
    });
}
