//! Parser-attested instance receiver facts. Unsupported values remain calls
//! with unresolved receivers; no method is selected from its spelling alone.

use std::collections::{HashMap, HashSet};

use tracedecay_domain::SourceSpan;
use tree_sitter::Node as TsNode;

use super::{ExtractionState, TypeScriptExtractor};
use crate::extraction_artifact::{
    ExtractedTypeScriptTypeRowV1, TypeScriptReceiverV1, TypeScriptTypeRowV1,
};

/// Reuse facts about the exact retained tree while walking one owner's calls.
/// A declaration's position is checked separately at every call site, so an
/// earlier use cannot borrow a later call's proof. This cache never survives
/// extraction or crosses into another tree. Index every name together so
/// different receivers in one scope do not each trigger a full tree walk.
#[derive(Default)]
pub(super) struct ReceiverLookup<'t> {
    bindings: HashMap<usize, HashMap<String, ScopeBinding<'t>>>,
    var_bindings: HashMap<usize, HashSet<String>>,
    writes: HashMap<usize, HashSet<String>>,
}

#[derive(Clone, Copy)]
enum ScopeBinding<'t> {
    Missing,
    Blocked,
    Declared(TsNode<'t>),
}

impl<'t> ReceiverLookup<'t> {
    fn binding_in_scope(
        &mut self,
        state: &ExtractionState<'_>,
        scope: TsNode<'t>,
        name: &str,
    ) -> ScopeBinding<'t> {
        self.bindings
            .entry(scope.id())
            .or_insert_with(|| {
                let mut found = HashMap::new();
                bindings_in_scope(state, scope, scope, &mut found);
                found
            })
            .get(name)
            .copied()
            .unwrap_or(ScopeBinding::Missing)
    }

    fn has_var_binding(
        &mut self,
        state: &ExtractionState<'_>,
        scope: TsNode<'t>,
        name: &str,
    ) -> bool {
        self.var_bindings
            .entry(scope.id())
            .or_insert_with(|| {
                let mut found = HashSet::new();
                collect_var_bindings(state, scope, scope, &mut found);
                found
            })
            .contains(name)
    }

    fn has_write(&mut self, state: &ExtractionState<'_>, scope: TsNode<'t>, name: &str) -> bool {
        self.writes
            .entry(scope.id())
            .or_insert_with(|| {
                let mut found = HashSet::new();
                collect_writes(state, scope, &mut found);
                found
            })
            .contains(name)
    }
}

#[cfg(test)]
thread_local! {
    static SCOPE_LOOKUP_NODE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

fn visit_binding_names<'s>(
    state: &ExtractionState<'s>,
    node: TsNode<'_>,
    visit: &mut impl FnMut(&'s str),
) {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern" | "type_identifier"
    ) {
        visit(state.node_text(node));
        return;
    }
    // Defaults and property keys refer to values; only the pattern's
    // binding side declares or writes an identifier.
    let binding_field = match node.kind() {
        "assignment_pattern" | "object_assignment_pattern" => Some("left"),
        "pair_pattern" => Some("value"),
        _ => None,
    };
    if let Some(field) = binding_field {
        if let Some(binding) = node.child_by_field_name(field) {
            visit_binding_names(state, binding, visit);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_binding_names(state, child, visit);
    }
}

fn record_binding<'t>(
    state: &ExtractionState<'_>,
    pattern: TsNode<'t>,
    declaration: TsNode<'t>,
    found: &mut HashMap<String, ScopeBinding<'t>>,
) {
    let mut names = Vec::new();
    visit_binding_names(state, pattern, &mut |name| names.push(name));
    // One declaration may repeat a name inside its pattern. Only separate
    // declarations make the lexical binding ambiguous.
    names.sort_unstable();
    names.dedup();
    let binding = if declaration.has_error() {
        ScopeBinding::Blocked
    } else {
        ScopeBinding::Declared(declaration)
    };
    for name in names {
        found
            .entry(name.to_owned())
            .and_modify(|binding| *binding = ScopeBinding::Blocked)
            .or_insert(binding);
    }
}

/// Collect the declarations belonging to one lexical scope. Nested scopes
/// cannot supply a sibling's receiver. Function-scoped `var` is a blocker,
/// including a declaration nested under a block we otherwise do not enter.
fn bindings_in_scope<'t>(
    state: &ExtractionState<'_>,
    scope: TsNode<'t>,
    node: TsNode<'t>,
    found: &mut HashMap<String, ScopeBinding<'t>>,
) {
    #[cfg(test)]
    SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.set(visits.get() + 1));
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
    if let Some(binding) = binding {
        record_binding(state, binding, node, found);
    }
    if node != scope && is_scope(node) {
        return;
    }
    // Named function/class expressions bind their own name in their body.
    if node == scope
        && (is_function(node) || node.kind() == "class")
        && let Some(bound) = node.child_by_field_name("name")
    {
        record_binding(state, bound, node, found);
    }
    if is_function(node)
        && let Some(parameter) = node.child_by_field_name("parameter")
    {
        record_binding(state, parameter, parameter, found);
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
            ) {
                record_binding(state, child, child, found);
            }
        }
    }
    if node.kind() == "import_clause" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "identifier" {
                record_binding(state, child, child, found);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        bindings_in_scope(state, scope, child, found);
    }
}

fn collect_var_bindings(
    state: &ExtractionState<'_>,
    root: TsNode<'_>,
    node: TsNode<'_>,
    found: &mut HashSet<String>,
) {
    #[cfg(test)]
    SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.set(visits.get() + 1));
    if node != root && is_function(node) {
        return;
    }
    if node.kind() == "variable_declaration" {
        let mut cursor = node.walk();
        for binding in node
            .named_children(&mut cursor)
            .filter_map(|child| child.child_by_field_name("name"))
        {
            visit_binding_names(state, binding, &mut |name| {
                found.insert(name.to_owned());
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_var_bindings(state, root, child, found);
    }
}

/// `Some(None)` is a shadow or ambiguous binding, never permission to look
/// farther out. Both use-before-declaration and any write in the binding's
/// scope abstain, including writes in closures.
fn binding<'t>(
    state: &ExtractionState<'_>,
    site: TsNode<'t>,
    name: &str,
    lookup: &mut ReceiverLookup<'t>,
) -> Option<Option<(TsNode<'t>, TsNode<'t>)>> {
    let mut parent = site.parent();
    while let Some(scope) = parent {
        if is_scope(scope) {
            // Parameter initializers have an outer environment: hoisted
            // body variables are not visible until the body is entered.
            let body_scope = is_function(scope)
                && scope.child_by_field_name("body").is_some_and(|body| {
                    body.start_byte() <= site.start_byte() && site.end_byte() <= body.end_byte()
                });
            if (body_scope || scope.kind() == "program")
                && lookup.has_var_binding(state, scope, name)
            {
                return Some(None);
            }
            match lookup.binding_in_scope(state, scope, name) {
                ScopeBinding::Missing => {}
                ScopeBinding::Blocked => return Some(None),
                ScopeBinding::Declared(declaration) => return Some(Some((declaration, scope))),
            }
        }
        parent = scope.parent();
    }
    None
}

fn collect_writes(state: &ExtractionState<'_>, node: TsNode<'_>, found: &mut HashSet<String>) {
    #[cfg(test)]
    SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.set(visits.get() + 1));
    let target = match node.kind() {
        "assignment_expression" | "augmented_assignment_expression" | "for_in_statement" => {
            node.child_by_field_name("left")
        }
        "update_expression" => node.child_by_field_name("argument"),
        _ => None,
    };
    // Writing x.field does not rebind x; destructuring assignments do.
    if let Some(target) = target
        && target.kind() != "member_expression"
        && target.kind() != "subscript_expression"
    {
        visit_binding_names(state, target, &mut |name| {
            found.insert(name.to_owned());
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_writes(state, child, found);
    }
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
fn module_name<'t>(
    state: &ExtractionState<'_>,
    site: TsNode<'t>,
    name: String,
    lookup: &mut ReceiverLookup<'t>,
) -> Option<String> {
    let head = name.split('.').next()?;
    if let Some(bound) = binding(state, site, head, lookup) {
        let (_, scope) = bound?;
        if scope.kind() != "program" || lookup.has_write(state, scope, head) {
            return None;
        }
    }
    let mut parent = site.parent();
    while let Some(node) = parent {
        if let Some(parameters) = node.child_by_field_name("type_parameters") {
            let mut cursor = parameters.walk();
            if parameters
                .named_children(&mut cursor)
                .filter_map(|parameter| parameter.child_by_field_name("name"))
                .any(|name| state.node_text(name) == head)
            {
                return None;
            }
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

fn initializer_receiver<'t>(
    state: &ExtractionState<'_>,
    node: TsNode<'t>,
    lookup: &mut ReceiverLookup<'t>,
) -> Option<TypeScriptReceiverV1> {
    let node = unwrap_expression(node)?;
    match node.kind() {
        "new_expression" => {
            let constructor = node.child_by_field_name("constructor")?;
            let name = module_name(state, constructor, named_type(state, constructor)?, lookup)?;
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
            let name = module_name(state, callee, state.node_text(callee).to_owned(), lookup)?;
            Some(TypeScriptReceiverV1::AwaitedFactory { name })
        }
        _ => None,
    }
}

fn receiver<'t>(
    state: &ExtractionState<'_>,
    mut object: TsNode<'t>,
    mut site: TsNode<'t>,
    lookup: &mut ReceiverLookup<'t>,
) -> Option<TypeScriptReceiverV1> {
    loop {
        object = unwrap_expression(object)?;
        if object.kind() != "identifier" {
            return initializer_receiver(state, object, lookup);
        }
        let name = state.node_text(object);
        let (declaration, scope) = binding(state, site, name, lookup)??;
        if lookup.has_write(state, scope, name) {
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
        if declaration.kind() == "variable_declarator" && declaration.end_byte() > site.start_byte()
        {
            return None;
        }
        if let Some(annotation) = declaration.child_by_field_name("type") {
            let name = module_name(state, annotation, named_type(state, annotation)?, lookup)?;
            return Some(TypeScriptReceiverV1::Named {
                name,
                constructor: false,
            });
        }
        // Resolve an immutable alias at its declaration, preserving its
        // captured binding through inner shadows. Each step precedes its
        // use in source, so cycles abstain without recursion or a depth cap.
        if declaration.parent()?.kind() != "lexical_declaration"
            || !TypeScriptExtractor::has_child_kind(declaration.parent()?, "const")
        {
            return None;
        }
        object = declaration.child_by_field_name("value")?;
        site = object;
    }
}

pub(super) fn record_call<'t>(
    state: &mut ExtractionState<'_>,
    callee: TsNode<'t>,
    owner: &str,
    lookup: &mut ReceiverLookup<'t>,
) {
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
    let Some(receiver) = receiver(state, object, callee, lookup) else {
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
    let mut lookup = ReceiverLookup::default();
    let promise = ty.kind() == "generic_type"
        && ty
            .child_by_field_name("name")
            .is_some_and(|name| state.node_text(name) == "Promise");
    if promise {
        // A project declaration called Promise is not the built-in wrapper.
        if binding(state, ty, "Promise", &mut lookup).is_some()
            || module_name(state, ty, "Promise".to_owned(), &mut lookup).is_none()
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
    let Some(name) =
        named_type(state, ty).and_then(|name| module_name(state, ty, name, &mut lookup))
    else {
        return;
    };
    state.typescript_types.push(ExtractedTypeScriptTypeRowV1 {
        node_id: owner.to_owned(),
        row: TypeScriptTypeRowV1::CallableReturn { name, promise },
    });
}

#[cfg(test)]
mod tests {
    use super::{SCOPE_LOOKUP_NODE_VISITS, TypeScriptExtractor, TypeScriptTypeRowV1};

    fn repeated_receiver_walk(calls: usize, written: bool) -> usize {
        let mut source =
            String::from("class Worker { push(value: number) {} }\nexport function collect() { ");
        source.push_str(if written {
            "let receiver: Worker = new Worker(); "
        } else {
            "const receiver = new Worker(); "
        });
        for _ in 0..calls {
            source.push_str("if (true) { receiver.push(1); }");
        }
        if written {
            source.push_str("receiver = new Worker();");
        }
        source.push_str("}\n");
        SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.set(0));
        let artifact = TypeScriptExtractor::extract_typescript_artifact("repeated.ts", &source);
        let visits = SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.replace(0));
        assert!(
            artifact.result.errors.is_empty(),
            "{:?}",
            artifact.result.errors
        );
        assert_eq!(
            artifact
                .result
                .unresolved_refs
                .iter()
                .filter(|reference| reference.reference_name == "receiver.push")
                .count(),
            calls,
            "the extraction retains every source call site",
        );
        assert_eq!(
            artifact
                .typescript_types
                .iter()
                .filter(|row| matches!(row.row, TypeScriptTypeRowV1::ReceiverCall { .. }))
                .count(),
            if written { 0 } else { calls },
            "a write anywhere in the binding scope still refuses every receiver proof",
        );
        visits
    }

    #[test]
    fn repeated_receiver_scope_walks_grow_with_source_size() {
        for written in [false, true] {
            let small = repeated_receiver_walk(128, written);
            let large = repeated_receiver_walk(256, written);
            assert!(small > 0);
            assert!(
                large <= small * 3,
                "doubling receiver sites must not rescan the whole scope per call: {small} -> {large}, written={written}",
            );
        }
    }

    fn distinct_receiver_walk(calls: usize, written: bool) -> usize {
        let mut source =
            String::from("class Worker { push(value: number) {} }\nexport function collect() { ");
        for index in 0..calls {
            if written && index == 0 {
                source.push_str("let receiver0: Worker = new Worker(); receiver0.push(1);");
            } else {
                source.push_str(&format!(
                    "const receiver{index} = new Worker(); receiver{index}.push(1);"
                ));
            }
        }
        if written {
            source.push_str("receiver0 = new Worker();");
        }
        source.push_str("}\n");
        SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.set(0));
        let artifact = TypeScriptExtractor::extract_typescript_artifact("distinct.ts", &source);
        let visits = SCOPE_LOOKUP_NODE_VISITS.with(|visits| visits.replace(0));
        assert!(
            artifact.result.errors.is_empty(),
            "{:?}",
            artifact.result.errors
        );
        assert_eq!(
            artifact
                .result
                .unresolved_refs
                .iter()
                .filter(|reference| reference.reference_name.ends_with(".push"))
                .count(),
            calls,
        );
        assert_eq!(
            artifact
                .typescript_types
                .iter()
                .filter(|row| matches!(row.row, TypeScriptTypeRowV1::ReceiverCall { .. }))
                .count(),
            calls - usize::from(written),
            "a write to one name must not erase proof for its peers",
        );
        visits
    }

    #[test]
    fn distinct_receiver_scope_walks_grow_with_source_size() {
        for written in [false, true] {
            let small = distinct_receiver_walk(128, written);
            let large = distinct_receiver_walk(256, written);
            assert!(small > 0);
            assert!(
                large <= small * 3,
                "doubling receiver names must not rescan the whole scope per name: {small} -> {large}, written={written}",
            );
        }
    }

    #[test]
    fn cached_receiver_scope_keeps_call_site_order_and_inner_shadows() {
        let source = "class Worker { push() {} }\nfunction run() { receiver.push(); const receiver = new Worker(); receiver.push(); { const receiver = unknownValue; receiver.push(); } receiver.push(); }";
        let artifact = TypeScriptExtractor::extract_typescript_artifact("scopes.ts", source);
        assert!(artifact.result.errors.is_empty());
        let sites = artifact
            .typescript_types
            .iter()
            .filter_map(|row| match row.row {
                TypeScriptTypeRowV1::ReceiverCall { span, .. } => Some(span.start_byte as usize),
                _ => None,
            })
            .collect::<Vec<_>>();
        let calls = source
            .match_indices("receiver.push")
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 4);
        assert_eq!(sites, vec![calls[1], calls[3]]);
    }
}
