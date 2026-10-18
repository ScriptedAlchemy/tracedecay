//! Lexical-scope evidence for rename collision hazards (JavaScript and TypeScript).
//!
//! A spelling coincidence is not a hazard. Each hazard here needs its own
//! evidence: a declaration of the new name in the target's binding scope
//! (collision), a declaration of the new name between a target reference and
//! the target's binding scope (capture), or a reference to the new name that
//! would start resolving to the renamed symbol (changed resolution).

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_code_extraction::ts_provider::try_language;
use tracedecay_contracts::source_edit::RenameHazardKindV1;
use tree_sitter::{Node, Parser};

pub(super) struct ScopeFinding {
    pub(super) kind: RenameHazardKindV1,
    pub(super) line: usize,
    pub(super) detail: String,
    /// Byte range of the renamed-symbol site the finding affects, when it has one.
    pub(super) site: Option<(usize, usize)>,
}

pub(super) enum ScopeAnalysis {
    Findings(Vec<ScopeFinding>),
    /// Scope cannot be established, so the caller must refuse conservatively.
    Unavailable(&'static str),
}

enum Role<'t> {
    Binding(Node<'t>),
    Use,
    NonLexical,
}

const LEXICAL_IDENTIFIERS: &[&str] = &[
    "identifier",
    "type_identifier",
    "shorthand_property_identifier",
    "shorthand_property_identifier_pattern",
];

fn grammar_key(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1;
    match extension {
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        _ => None,
    }
}

fn is_function_scope(kind: &str) -> bool {
    matches!(
        kind,
        "program"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
            | "class_static_block"
    )
}

fn is_scope(kind: &str) -> bool {
    is_function_scope(kind)
        || matches!(
            kind,
            "statement_block"
                | "for_statement"
                | "for_in_statement"
                | "catch_clause"
                | "switch_body"
                | "class"
        )
}

fn scope_above(node: Node<'_>, hoist: bool) -> Option<Node<'_>> {
    let mut cursor = node.parent();
    while let Some(candidate) = cursor {
        let kind = candidate.kind();
        if if hoist {
            is_function_scope(kind)
        } else {
            is_scope(kind)
        } {
            return Some(candidate);
        }
        cursor = candidate.parent();
    }
    None
}

fn is_field(parent: Node<'_>, field: &str, child: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|node| node.id() == child.id())
}

fn within(node: Node<'_>, ancestor: Node<'_>) -> bool {
    let mut cursor = Some(node);
    while let Some(candidate) = cursor {
        if candidate.id() == ancestor.id() {
            return true;
        }
        cursor = candidate.parent();
    }
    false
}

/// The role of an identifier-like node, or `None` when the node is not a
/// lexically scoped name (a property name, for example).
fn role(node: Node<'_>) -> Option<Role<'_>> {
    if !LEXICAL_IDENTIFIERS.contains(&node.kind()) {
        return None;
    }
    let mut child = node;
    let mut parent = node.parent()?;
    loop {
        let destructuring = match parent.kind() {
            "array_pattern" | "object_pattern" | "rest_pattern" => true,
            "pair_pattern" => is_field(parent, "value", child),
            "assignment_pattern" | "object_assignment_pattern" => is_field(parent, "left", child),
            _ => false,
        };
        if !destructuring {
            break;
        }
        child = parent;
        parent = parent.parent()?;
    }
    let binding = match parent.kind() {
        "variable_declarator" if is_field(parent, "name", child) => {
            let hoisted = parent
                .parent()
                .is_some_and(|declaration| declaration.kind() == "variable_declaration");
            scope_above(parent, hoisted)
        }
        "formal_parameters" => scope_above(parent, false),
        "required_parameter" | "optional_parameter" if is_field(parent, "pattern", child) => {
            scope_above(parent, false)
        }
        "arrow_function" if is_field(parent, "parameter", child) => Some(parent),
        "catch_clause" if is_field(parent, "parameter", child) => Some(parent),
        "function_declaration"
        | "generator_function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "enum_declaration"
        | "type_alias_declaration"
        | "interface_declaration"
        | "function_signature"
        | "type_parameter"
            if is_field(parent, "name", child) =>
        {
            scope_above(parent, false)
        }
        "function_expression" | "generator_function" | "class"
            if is_field(parent, "name", child) =>
        {
            Some(parent)
        }
        "import_specifier" => {
            if parent.child_by_field_name("alias").is_none() || is_field(parent, "alias", child) {
                scope_above(parent, false)
            } else {
                return Some(Role::NonLexical);
            }
        }
        "import_clause" | "namespace_import" => scope_above(parent, false),
        "for_in_statement"
            if is_field(parent, "left", child) && parent.child_by_field_name("kind").is_some() =>
        {
            Some(parent)
        }
        "export_specifier" if is_field(parent, "alias", child) => return Some(Role::NonLexical),
        _ => None,
    };
    Some(binding.map_or(Role::Use, Role::Binding))
}

fn collect_named<'t>(node: Node<'t>, source: &[u8], name: &str, out: &mut Vec<Node<'t>>) {
    if LEXICAL_IDENTIFIERS.contains(&node.kind()) && node.utf8_text(source) == Ok(name) {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_named(child, source, name, out);
    }
}

fn line_of(node: Node<'_>) -> usize {
    node.start_position().row + 1
}

/// Analyze every code occurrence of `new_name` in `source` against the scopes
/// of the renamed symbol's `targets` (byte ranges of its sites in this file).
pub(super) fn analyze(
    path: &str,
    source: &str,
    new_name: &str,
    targets: &[(usize, usize)],
) -> ScopeAnalysis {
    let Some(key) = grammar_key(path) else {
        return ScopeAnalysis::Unavailable("scope analysis is not implemented for this language");
    };
    let mut parser = Parser::new();
    let parsed = try_language(key)
        .ok()
        .filter(|language| parser.set_language(language).is_ok())
        .and_then(|_| parser.parse(source, None));
    let Some(tree) = parsed else {
        return ScopeAnalysis::Unavailable("the file could not be parsed");
    };
    let root = tree.root_node();
    if root.has_error() {
        return ScopeAnalysis::Unavailable("the file has syntax errors");
    }

    let mut target_scopes = Vec::new();
    let mut target_uses = Vec::new();
    for &(start, end) in targets {
        let Some(node) = root
            .descendant_for_byte_range(start, end)
            .filter(|node| node.start_byte() == start && node.end_byte() == end)
        else {
            return ScopeAnalysis::Unavailable("a renamed site is not a single syntax node");
        };
        match role(node) {
            Some(Role::Binding(scope)) => target_scopes.push(scope),
            Some(Role::Use) => target_uses.push(((start, end), node)),
            Some(Role::NonLexical) => {}
            None => {
                return ScopeAnalysis::Unavailable("a renamed site is not a lexically scoped name");
            }
        }
    }
    if target_scopes.is_empty() {
        target_scopes.push(root);
    }

    let mut occurrences = Vec::new();
    collect_named(root, source.as_bytes(), new_name, &mut occurrences);
    let roles: Vec<_> = occurrences
        .iter()
        .filter_map(|node| role(*node).map(|role| (*node, role)))
        .collect();
    let mut declared = BTreeMap::new();
    for (node, role) in &roles {
        if let Role::Binding(scope) = role {
            declared.entry(scope.id()).or_insert_with(|| line_of(*node));
        }
    }

    let mut findings = Vec::new();
    let mut captured = BTreeSet::new();
    for target in &target_scopes {
        if let Some(&line) = declared.get(&target.id()) {
            findings.push(ScopeFinding {
                kind: RenameHazardKindV1::NamespaceCollision,
                line,
                detail: format!(
                    "`{new_name}` is already declared in the scope that binds the renamed symbol"
                ),
                site: None,
            });
        }
        for (node, role) in &roles {
            match role {
                Role::Binding(scope) if scope.id() != target.id() && within(*scope, *target) => {
                    for &(range, reference) in &target_uses {
                        if within(reference, *scope) && captured.insert(range) {
                            findings.push(ScopeFinding {
                                kind: RenameHazardKindV1::Shadowing,
                                line: line_of(reference),
                                detail: format!(
                                    "renaming would make this reference resolve to the `{new_name}` declared at line {}",
                                    line_of(*node)
                                ),
                                site: Some(range),
                            });
                        }
                    }
                }
                Role::Use
                    if within(*node, *target) && resolves_to_scope(*node, *target, &declared) =>
                {
                    findings.push(ScopeFinding {
                        kind: RenameHazardKindV1::ChangedResolution,
                        line: line_of(*node),
                        detail: format!(
                            "this reference to `{new_name}` would resolve to the renamed symbol"
                        ),
                        site: None,
                    });
                }
                _ => {}
            }
        }
    }
    ScopeAnalysis::Findings(findings)
}

/// Whether a use of the new name would reach the renamed symbol's binding
/// scope without meeting an inner declaration. A declaration in the binding
/// scope itself is reported as a collision, not as a changed resolution.
fn resolves_to_scope(
    reference: Node<'_>,
    target: Node<'_>,
    declared: &BTreeMap<usize, usize>,
) -> bool {
    let mut cursor = reference.parent();
    while let Some(candidate) = cursor {
        if candidate.id() == target.id() {
            return !declared.contains_key(&candidate.id());
        }
        if declared.contains_key(&candidate.id()) {
            return false;
        }
        cursor = candidate.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use tracedecay_contracts::source_edit::RenameHazardKindV1::{
        ChangedResolution, NamespaceCollision, Shadowing,
    };

    use super::{ScopeAnalysis, analyze};

    fn kinds(source: &str) -> Vec<tracedecay_contracts::source_edit::RenameHazardKindV1> {
        let sites: Vec<_> = source
            .match_indices("oldName")
            .map(|(start, name)| (start, start + name.len()))
            .collect();
        match analyze("src/a.ts", source, "newName", &sites) {
            ScopeAnalysis::Findings(findings) => {
                findings.into_iter().map(|finding| finding.kind).collect()
            }
            ScopeAnalysis::Unavailable(reason) => panic!("unexpectedly unavailable: {reason}"),
        }
    }

    #[test]
    fn unrelated_nested_binding_is_not_a_hazard() {
        let source = "export function oldName() {}\n\
            export function unrelated() {\n  const newName = 1;\n  return newName;\n}\n";
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn same_scope_declaration_is_a_collision() {
        let source = "function oldName() {}\nconst newName = 1;\n";
        assert_eq!(kinds(source), vec![NamespaceCollision]);
    }

    #[test]
    fn inner_binding_around_a_reference_is_shadowing() {
        let source = "function oldName() {}\n\
            function caller() { const newName = 1; return oldName() + newName; }\n";
        assert_eq!(kinds(source), vec![Shadowing]);
    }

    #[test]
    fn unshadowed_reference_to_an_outer_name_changes_resolution() {
        let source = "const newName = 1;\n\
            function outer() { function oldName() {} return newName + oldName(); }\n";
        assert_eq!(kinds(source), vec![ChangedResolution]);
    }

    #[test]
    fn parameters_and_destructuring_bind_locally() {
        let source = "function oldName() {}\n\
            function a(newName) { return newName; }\n\
            function b({ x: newName }) { return newName; }\n\
            function c() { const [newName] = [1]; return newName; }\n";
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn member_names_and_strings_are_not_references() {
        let source = "function oldName() {}\n\
            const value = { newName: 1 }.newName + 'newName';\n";
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn unsupported_language_and_member_targets_are_unavailable() {
        assert!(matches!(
            analyze("src/a.rs", "fn old_name() {}", "new_name", &[(3, 11)]),
            ScopeAnalysis::Unavailable(_)
        ));
        let source = "class A { oldName() {} }";
        let start = source.find("oldName").unwrap();
        assert!(matches!(
            analyze("src/a.ts", source, "newName", &[(start, start + 7)]),
            ScopeAnalysis::Unavailable(_)
        ));
    }
}
