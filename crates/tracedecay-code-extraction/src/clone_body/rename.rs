use std::collections::HashMap;

use tree_sitter::Node as TreeSitterNode;

use super::{CallableSyntax, CloneBodyRenameIssueV1, CloneBodyRenameStatusV1, SyntaxPreorder};
use crate::traversal::visit_children_while;

type ByteSpan = (usize, usize);

pub(super) struct RenameNormalization {
    pub status: CloneBodyRenameStatusV1,
    pub issues: Vec<CloneBodyRenameIssueV1>,
    pub replacements: Option<HashMap<ByteSpan, String>>,
}

struct Binding {
    name: String,
    replacement: String,
    declaration: ByteSpan,
    scope: ByteSpan,
    active_from: usize,
}

struct Analyzer<'tree, 'source> {
    language: &'source str,
    source: &'source [u8],
    callables: Vec<CallableSyntax<'tree>>,
    bindings: Vec<Binding>,
    python_bindings: HashMap<(ByteSpan, String), String>,
    issues: Vec<CloneBodyRenameIssueV1>,
    next_argument: usize,
    next_local: usize,
}

pub(super) fn normalize(
    syntax: CallableSyntax<'_>,
    source: &str,
    language: &str,
) -> RenameNormalization {
    if !matches!(
        language,
        "rust" | "typescript" | "tsx" | "javascript" | "python"
    ) {
        return RenameNormalization {
            status: CloneBodyRenameStatusV1::UnsupportedLanguage,
            issues: Vec::new(),
            replacements: None,
        };
    }

    let mut analyzer = Analyzer {
        language,
        source: source.as_bytes(),
        callables: vec![syntax],
        bindings: Vec::new(),
        python_bindings: HashMap::new(),
        issues: Vec::new(),
        next_argument: 0,
        next_local: 0,
    };
    analyzer.collect_nested_callables();
    analyzer.collect_parameters();
    analyzer.collect_locals_and_issues();
    let replacements = analyzer.resolve_identifiers();
    analyzer.issues.sort();
    analyzer.issues.dedup();
    let status = if analyzer.issues.is_empty() {
        CloneBodyRenameStatusV1::Complete
    } else {
        CloneBodyRenameStatusV1::Partial
    };
    RenameNormalization {
        status,
        issues: analyzer.issues,
        replacements: Some(replacements),
    }
}

impl Analyzer<'_, '_> {
    fn collect_nested_callables(&mut self) {
        let root_body = self.callables[0].body;
        for node in SyntaxPreorder::new(root_body) {
            if is_callable(self.language, node.kind())
                && let Some(body) = node.child_by_field_name("body")
            {
                self.callables.push(CallableSyntax {
                    owner: node,
                    body,
                    body_boundary_complete: true,
                });
            }
        }
        self.callables
            .sort_by_key(|callable| callable.owner.start_byte());
    }

    fn collect_parameters(&mut self) {
        for index in 0..self.callables.len() {
            let callable = self.callables[index];
            if let Some(parameter) = callable.owner.child_by_field_name("parameter") {
                self.collect_pattern(
                    parameter,
                    span(callable.body),
                    callable.body.start_byte(),
                    BindingClass::Argument,
                );
            }
            let Some(parameters) = callable.owner.child_by_field_name("parameters") else {
                continue;
            };
            visit_children_while(parameters, |parameter| {
                if !parameter.is_named() {
                    return true;
                }
                let pattern = parameter
                    .child_by_field_name("pattern")
                    .or_else(|| parameter.child_by_field_name("name"))
                    .unwrap_or(parameter);
                self.collect_pattern(
                    pattern,
                    span(callable.body),
                    callable.body.start_byte(),
                    BindingClass::Argument,
                );
                true
            });
        }
    }

    fn collect_locals_and_issues(&mut self) {
        let root_body = self.callables[0].body;
        for node in SyntaxPreorder::new(root_body) {
            match self.language {
                "rust" => self.collect_rust_local(node, root_body),
                "typescript" | "tsx" | "javascript" => {
                    self.collect_typescript_local(node, root_body)
                }
                "python" => self.collect_python_local(node),
                _ => {}
            }
        }
    }

    fn collect_rust_local(&mut self, node: TreeSitterNode<'_>, root_body: TreeSitterNode<'_>) {
        match node.kind() {
            "let_declaration" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.collect_pattern(
                        pattern,
                        self.lexical_scope(node, root_body),
                        node.end_byte(),
                        BindingClass::Local,
                    );
                }
            }
            "for_expression" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let active_from = node
                        .child_by_field_name("body")
                        .map_or(node.end_byte(), |body| body.start_byte());
                    self.collect_pattern(
                        pattern,
                        node.child_by_field_name("body")
                            .map_or_else(|| self.lexical_scope(node, root_body), span),
                        active_from,
                        BindingClass::Local,
                    );
                }
            }
            "macro_invocation" | "match_arm" => {
                self.issues
                    .push(CloneBodyRenameIssueV1::UnsupportedBindingSyntax);
            }
            _ => {}
        }
    }

    fn collect_typescript_local(
        &mut self,
        node: TreeSitterNode<'_>,
        root_body: TreeSitterNode<'_>,
    ) {
        match node.kind() {
            "variable_declarator" => {
                if let Some(pattern) = node.child_by_field_name("name") {
                    let scope = if node
                        .parent()
                        .is_some_and(|parent| parent.kind() == "variable_declaration")
                    {
                        self.callable_scope(node)
                    } else {
                        self.lexical_scope(node, root_body)
                    };
                    self.collect_pattern(pattern, scope, scope.0, BindingClass::Local);
                }
            }
            "catch_clause" => {
                if let (Some(pattern), Some(body)) = (
                    node.child_by_field_name("parameter"),
                    node.child_by_field_name("body"),
                ) {
                    self.collect_pattern(
                        pattern,
                        span(body),
                        body.start_byte(),
                        BindingClass::Local,
                    );
                }
            }
            "call_expression" if self.call_name(node) == Some("eval") => {
                self.issues.push(CloneBodyRenameIssueV1::DynamicBinding);
            }
            _ => {}
        }
    }

    fn collect_python_local(&mut self, node: TreeSitterNode<'_>) {
        match node.kind() {
            "assignment" | "named_expression" => {
                if let Some(pattern) = node
                    .child_by_field_name("left")
                    .or_else(|| node.child_by_field_name("name"))
                {
                    self.collect_python_assignment(pattern);
                }
            }
            "for_statement" => {
                if let Some(pattern) = node.child_by_field_name("left") {
                    self.collect_python_assignment(pattern);
                }
            }
            "global_statement"
            | "nonlocal_statement"
            | "import_statement"
            | "import_from_statement"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => {
                self.issues
                    .push(CloneBodyRenameIssueV1::UnsupportedBindingSyntax);
            }
            "call"
                if matches!(
                    self.call_name(node),
                    Some("eval" | "exec" | "locals" | "globals")
                ) =>
            {
                self.issues.push(CloneBodyRenameIssueV1::DynamicBinding);
            }
            _ => {}
        }
    }

    fn collect_python_assignment(&mut self, pattern: TreeSitterNode<'_>) {
        if matches!(pattern.kind(), "attribute" | "subscript") {
            return;
        }
        let scope = self.callable_scope(pattern);
        self.collect_pattern(pattern, scope, scope.0, BindingClass::Local);
    }

    fn collect_pattern(
        &mut self,
        pattern: TreeSitterNode<'_>,
        scope: ByteSpan,
        active_from: usize,
        class: BindingClass,
    ) {
        match pattern.kind() {
            "identifier" => self.add_binding(pattern, scope, active_from, class),
            "mut_pattern"
            | "ref_pattern"
            | "reference_pattern"
            | "captured_pattern"
            | "rest_pattern"
            | "assignment_pattern"
            | "default_parameter"
            | "typed_parameter"
            | "typed_default_parameter"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => {
                if let Some(inner) = pattern
                    .child_by_field_name("pattern")
                    .or_else(|| pattern.child_by_field_name("name"))
                    .or_else(|| pattern.child_by_field_name("left"))
                {
                    self.collect_pattern(inner, scope, active_from, class);
                } else {
                    self.collect_pattern_children(pattern, scope, active_from, class);
                }
            }
            "tuple_pattern" | "slice_pattern" | "list_pattern" | "array_pattern"
            | "pattern_list" => {
                self.collect_pattern_children(pattern, scope, active_from, class);
            }
            "self" | "this" | "positional_separator" | "keyword_separator" => {}
            "object_pattern"
            | "struct_pattern"
            | "tuple_struct_pattern"
            | "or_pattern"
            | "macro_invocation" => {
                self.issues
                    .push(CloneBodyRenameIssueV1::UnsupportedBindingSyntax);
            }
            _ => {
                if pattern.is_named() {
                    self.issues
                        .push(CloneBodyRenameIssueV1::UnsupportedBindingSyntax);
                }
            }
        }
    }

    fn collect_pattern_children(
        &mut self,
        pattern: TreeSitterNode<'_>,
        scope: ByteSpan,
        active_from: usize,
        class: BindingClass,
    ) {
        visit_children_while(pattern, |child| {
            if child.is_named() {
                self.collect_pattern(child, scope, active_from, class);
            }
            true
        });
    }

    fn add_binding(
        &mut self,
        identifier: TreeSitterNode<'_>,
        scope: ByteSpan,
        active_from: usize,
        class: BindingClass,
    ) {
        let Some(name) = identifier.utf8_text(self.source).ok() else {
            self.issues
                .push(CloneBodyRenameIssueV1::UnsupportedBindingSyntax);
            return;
        };
        let replacement = match class {
            BindingClass::Argument => {
                let replacement = format!("arg_{}", self.next_argument);
                self.next_argument += 1;
                replacement
            }
            BindingClass::Local => {
                if self.language == "python"
                    && let Some(replacement) = self.python_bindings.get(&(scope, name.to_owned()))
                {
                    replacement.clone()
                } else {
                    let replacement = format!("local_{}", self.next_local);
                    self.next_local += 1;
                    if self.language == "python" {
                        self.python_bindings
                            .insert((scope, name.to_owned()), replacement.clone());
                    }
                    replacement
                }
            }
        };
        self.bindings.push(Binding {
            name: name.to_owned(),
            replacement,
            declaration: span(identifier),
            scope,
            active_from,
        });
    }

    fn resolve_identifiers(&self) -> HashMap<ByteSpan, String> {
        let mut replacements = HashMap::new();
        for identifier in SyntaxPreorder::new(self.callables[0].body)
            .filter(|node| node.kind() == "identifier" && !is_preserved_identifier(*node))
        {
            let Some(name) = identifier.utf8_text(self.source).ok() else {
                continue;
            };
            let point = span(identifier);
            let binding = self
                .bindings
                .iter()
                .filter(|binding| {
                    binding.name == name
                        && (binding.declaration == point
                            || contains(binding.scope, point) && point.0 >= binding.active_from)
                })
                .min_by_key(|binding| {
                    (
                        binding.scope.1.saturating_sub(binding.scope.0),
                        usize::MAX.saturating_sub(binding.declaration.0),
                    )
                });
            if let Some(binding) = binding {
                replacements.insert(point, binding.replacement.clone());
            }
        }
        replacements
    }

    fn lexical_scope(&self, node: TreeSitterNode<'_>, root_body: TreeSitterNode<'_>) -> ByteSpan {
        let mut parent = node.parent();
        while let Some(candidate) = parent {
            if matches!(
                candidate.kind(),
                "block" | "statement_block" | "catch_clause"
            ) {
                return span(candidate);
            }
            if candidate.id() == root_body.id() {
                break;
            }
            parent = candidate.parent();
        }
        self.callable_scope(node)
    }

    fn callable_scope(&self, node: TreeSitterNode<'_>) -> ByteSpan {
        let node_span = span(node);
        self.callables
            .iter()
            .map(|callable| span(callable.body))
            .filter(|scope| contains(*scope, node_span))
            .min_by_key(|scope| scope.1.saturating_sub(scope.0))
            .unwrap_or_else(|| span(self.callables[0].body))
    }

    fn call_name(&self, call: TreeSitterNode<'_>) -> Option<&str> {
        call.child_by_field_name("function")
            .and_then(|function| (function.kind() == "identifier").then_some(function))
            .and_then(|function| function.utf8_text(self.source).ok())
    }
}

#[derive(Clone, Copy)]
enum BindingClass {
    Argument,
    Local,
}

fn is_callable(language: &str, kind: &str) -> bool {
    match language {
        "rust" => kind == "closure_expression",
        "typescript" | "tsx" | "javascript" => matches!(
            kind,
            "arrow_function"
                | "function_declaration"
                | "function_expression"
                | "generator_function"
                | "generator_function_declaration"
        ),
        "python" => matches!(kind, "function_definition" | "lambda"),
        _ => false,
    }
}

fn is_preserved_identifier(identifier: TreeSitterNode<'_>) -> bool {
    let Some(parent) = identifier.parent() else {
        return false;
    };
    for field in ["field", "property", "attribute"] {
        if parent
            .child_by_field_name(field)
            .is_some_and(|child| child.id() == identifier.id())
        {
            return true;
        }
    }
    (matches!(
        parent.kind(),
        "function_item"
            | "function_definition"
            | "function_declaration"
            | "function_expression"
            | "generator_function"
            | "generator_function_declaration"
            | "method_definition"
    ) || parent.kind() == "class_definition")
        && parent
            .child_by_field_name("name")
            .is_some_and(|child| child.id() == identifier.id())
        || parent.kind() == "keyword_argument"
            && parent
                .child_by_field_name("name")
                .is_some_and(|child| child.id() == identifier.id())
}

fn span(node: TreeSitterNode<'_>) -> ByteSpan {
    (node.start_byte(), node.end_byte())
}

fn contains(outer: ByteSpan, inner: ByteSpan) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}
