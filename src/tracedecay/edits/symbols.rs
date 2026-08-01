//! Symbol resolution for symbol-aware edit primitives (`replace_symbol`,
//! `insert_at_symbol`, and `move_symbol` in the sibling module): exact
//! qualified-name match wins, with ambiguity narrowed first to callable
//! kinds and then to declaration kinds so a shared name never silently
//! clobbers the wrong site.

use crate::errors::{Result, TraceDecayError};
use crate::types::{Node, NodeKind};

use super::super::TraceDecay;

/// Resolves a symbol name to a single node suitable for symbol-aware editing.
///
/// Exact-qualified-name match wins. Bare-name ambiguity may narrow to callable
/// kinds (function/method/etc.); remaining ambiguity — bare or qualified —
/// narrows to declaration kinds, because a type's inherent `impl` blocks share
/// its qualified name but "edit `Foo`" means the `Foo` declaration (impl
/// blocks are separate spans the caller edits explicitly). Anything still
/// ambiguous refuses the edit — silently picking the wrong site is worse than
/// asking the caller to disambiguate.
pub(in crate::tracedecay) async fn resolve_symbol_for_edit(
    cg: &TraceDecay,
    symbol: &str,
) -> Result<Node> {
    let nodes = cg.get_nodes_by_qualified_name(symbol).await?;
    narrow_symbol_for_edit(symbol, nodes)
}

/// Pure narrowing behind [`resolve_symbol_for_edit`]; split out so the
/// ambiguity rules are unit-testable without a graph database.
fn narrow_symbol_for_edit(symbol: &str, nodes: Vec<Node>) -> Result<Node> {
    let mut iter = nodes.into_iter();
    let Some(first) = iter.next() else {
        return Err(TraceDecayError::Config {
            message: format!("symbol '{symbol}' not found"),
        });
    };
    let rest: Vec<Node> = iter.collect();
    if rest.is_empty() {
        return Ok(first);
    }
    let total = rest.len() + 1;
    let all: Vec<Node> = std::iter::once(first).chain(rest).collect();
    if !symbol.contains("::") {
        let mut callables: Vec<Node> = all
            .iter()
            .filter(|node| is_callable_edit_kind(&node.kind))
            .cloned()
            .collect();
        if callables.len() == 1 {
            return Ok(callables.remove(0));
        }
    }
    let mut declarations: Vec<Node> = all
        .into_iter()
        .filter(|node| !matches!(node.kind, NodeKind::Impl))
        .collect();
    if declarations.len() == 1 {
        return Ok(declarations.remove(0));
    }
    if symbol.contains("::") {
        return Err(TraceDecayError::Config {
            message: format!(
                "symbol '{symbol}' is ambiguous ({total} matches); pass an exact stored qualified name"
            ),
        });
    }
    Err(TraceDecayError::Config {
        message: format!(
            "symbol '{symbol}' is ambiguous ({total} matches); pass a fully qualified name"
        ),
    })
}

/// Kinds that win bare-name ambiguity: the callable definitions a caller
/// almost always means when naming `foo` without qualification.
fn is_callable_edit_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::StructMethod
            | NodeKind::Constructor
            | NodeKind::AbstractMethod
            | NodeKind::ArrowFunction
            | NodeKind::Procedure
    )
}

#[cfg(test)]
mod tests {
    use crate::types::{Node, NodeKind, Visibility};

    use super::narrow_symbol_for_edit;

    fn node(kind: NodeKind, name: &str) -> Node {
        Node {
            id: format!("{kind:?}:{name}"),
            kind,
            name: name.to_string(),
            qualified_name: format!("src/a.rs::{name}"),
            file_path: "src/a.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: None,
        }
    }

    #[test]
    fn narrow_symbol_prefers_declaration_over_impl_blocks() {
        let resolved = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Struct, "Widget"),
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Impl, "Widget"),
            ],
        )
        .expect("declaration should win over same-named impl blocks");
        assert_eq!(resolved.kind, NodeKind::Struct);
    }

    #[test]
    fn narrow_symbol_prefers_declaration_for_bare_names() {
        let resolved = narrow_symbol_for_edit(
            "Widget",
            vec![
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Enum, "Widget"),
            ],
        )
        .expect("bare name should narrow to the declaration");
        assert_eq!(resolved.kind, NodeKind::Enum);
    }

    #[test]
    fn narrow_symbol_keeps_callable_precedence_for_bare_names() {
        let resolved = narrow_symbol_for_edit(
            "run",
            vec![
                node(NodeKind::Module, "run"),
                node(NodeKind::Function, "run"),
            ],
        )
        .expect("bare name should keep the historical callable-wins rule");
        assert_eq!(resolved.kind, NodeKind::Function);
    }

    #[test]
    fn narrow_symbol_still_refuses_multiple_declarations() {
        let result = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Struct, "Widget"),
                node(NodeKind::Struct, "Widget"),
            ],
        );
        assert!(result.is_err(), "two declarations must stay ambiguous");
    }

    #[test]
    fn narrow_symbol_still_refuses_impl_only_matches() {
        let result = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                node(NodeKind::Impl, "Widget"),
                node(NodeKind::Impl, "Widget"),
            ],
        );
        assert!(result.is_err(), "impl blocks alone must stay ambiguous");
    }
}
