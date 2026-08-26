//! Generation-pinned symbol resolution for symbol-aware source edits.

use tracedecay_code_index::graph_projection::{CodeGraphProjectionError, CodeGraphSymbolSummaryV1};
use tracedecay_domain::{SourceSpan, SymbolOccurrenceId};
use tracedecay_usecases::graph::{map_code_graph_read_runtime_error, map_projection_error};
use tracedecay_usecases::tracedecay::SourceEditGraphReadV1;

use crate::errors::{Result, TraceDecayError};
use crate::types::{NodeKind, Visibility};

const MAX_EDIT_SYMBOL_MATCHES: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tracedecay) struct EditSymbolV1 {
    pub(in crate::tracedecay) occurrence: SymbolOccurrenceId,
    pub(in crate::tracedecay) kind: NodeKind,
    pub(in crate::tracedecay) name: String,
    pub(in crate::tracedecay) qualified_name: String,
    pub(in crate::tracedecay) file_path: String,
    pub(in crate::tracedecay) source_span: SourceSpan,
    pub(in crate::tracedecay) start_line: u32,
    pub(in crate::tracedecay) line_span: u32,
    pub(in crate::tracedecay) visibility: Visibility,
}

impl EditSymbolV1 {
    pub(in crate::tracedecay) fn line_bounds(&self, source: &str) -> Result<(usize, usize)> {
        let start = usize::try_from(self.source_span.start_byte).map_err(|error| {
            symbol_evidence_unavailable(format!("symbol start offset exceeds this host: {error}"))
        })?;
        let end = usize::try_from(self.source_span.end_byte).map_err(|error| {
            symbol_evidence_unavailable(format!("symbol end offset exceeds this host: {error}"))
        })?;
        if start >= end
            || end > source.len()
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(end)
        {
            return Err(symbol_evidence_unavailable(
                "symbol source span is not an exact UTF-8 range",
            ));
        }
        let start_line = source[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let end_inclusive = source[..end - 1]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        if u32::try_from(start_line).ok() != Some(self.start_line)
            || u32::try_from(end_inclusive.saturating_sub(start_line).saturating_add(1)).ok()
                != Some(self.line_span)
        {
            return Err(symbol_evidence_unavailable(
                "symbol source span disagrees with its extraction-attested line bounds",
            ));
        }
        Ok((start_line, end_inclusive))
    }
}

fn projection_error(error: CodeGraphProjectionError) -> TraceDecayError {
    map_code_graph_read_runtime_error(map_projection_error(error))
}

fn symbol_evidence_unavailable(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route(
        "source-edit-symbol-evidence-unavailable",
        false,
        detail.into(),
    )
}

pub(in crate::tracedecay) fn edit_symbol_from_summary(
    summary: &CodeGraphSymbolSummaryV1,
) -> Result<EditSymbolV1> {
    let metadata = summary
        .metadata
        .as_ref()
        .ok_or_else(|| symbol_evidence_unavailable("source-edit symbol has no lineage metadata"))?;
    let binding = summary
        .binding
        .as_ref()
        .ok_or_else(|| symbol_evidence_unavailable("source-edit symbol has no file binding"))?;
    let file_path = binding.logical_path.clone().ok_or_else(|| {
        symbol_evidence_unavailable("source-edit symbol has no logical file path")
    })?;
    let source_span = binding.source_span.ok_or_else(|| {
        symbol_evidence_unavailable("source-edit symbol has no extraction-attested source span")
    })?;
    let kind = NodeKind::from_str(&metadata.kind).ok_or_else(|| {
        symbol_evidence_unavailable(format!(
            "unknown source-edit symbol kind `{}`",
            metadata.kind
        ))
    })?;
    let visibility = Visibility::from_str(&metadata.visibility).ok_or_else(|| {
        symbol_evidence_unavailable(format!(
            "unknown source-edit symbol visibility `{}`",
            metadata.visibility
        ))
    })?;
    Ok(EditSymbolV1 {
        occurrence: summary.occurrence.clone(),
        kind,
        name: metadata.simple_name.clone(),
        qualified_name: metadata.qualified_name.clone(),
        file_path,
        source_span,
        start_line: metadata.start_line,
        line_span: metadata.line_span,
        visibility,
    })
}

/// Resolves a symbol against the immutable generation admitted for this edit.
pub(in crate::tracedecay) fn resolve_symbol_for_edit(
    graph: &SourceEditGraphReadV1,
    symbol: &str,
) -> Result<EditSymbolV1> {
    let cancellation = graph.cancellation();
    let mut matches = graph
        .reader()
        .resolve_qualified_name(
            symbol,
            None,
            MAX_EDIT_SYMBOL_MATCHES + 1,
            cancellation.clone(),
        )
        .map_err(projection_error)?;
    if matches.is_empty() && !symbol.contains("::") {
        matches = graph
            .reader()
            .resolve_simple_name(symbol, None, MAX_EDIT_SYMBOL_MATCHES + 1, cancellation)
            .map_err(projection_error)?;
    }
    if matches.len() > MAX_EDIT_SYMBOL_MATCHES {
        return Err(TraceDecayError::project_route(
            "source-edit-symbol-budget-exhausted",
            false,
            "source-edit symbol resolution exceeded 100 candidates",
        ));
    }
    let symbols = matches
        .iter()
        .map(edit_symbol_from_summary)
        .collect::<Result<Vec<_>>>()?;
    narrow_symbol_for_edit(symbol, symbols)
}

fn narrow_symbol_for_edit(symbol: &str, symbols: Vec<EditSymbolV1>) -> Result<EditSymbolV1> {
    let mut iter = symbols.into_iter();
    let Some(first) = iter.next() else {
        return Err(TraceDecayError::Config {
            message: format!("symbol '{symbol}' not found"),
        });
    };
    let rest = iter.collect::<Vec<_>>();
    if rest.is_empty() {
        return Ok(first);
    }
    let total = rest.len() + 1;
    let all = std::iter::once(first).chain(rest).collect::<Vec<_>>();
    if !symbol.contains("::") {
        let mut callables = all
            .iter()
            .filter(|candidate| is_callable_edit_kind(&candidate.kind))
            .cloned()
            .collect::<Vec<_>>();
        if callables.len() == 1 {
            return Ok(callables.remove(0));
        }
    }
    let mut declarations = all
        .into_iter()
        .filter(|candidate| !matches!(candidate.kind, NodeKind::Impl))
        .collect::<Vec<_>>();
    if declarations.len() == 1 {
        return Ok(declarations.remove(0));
    }
    let guidance = if symbol.contains("::") {
        "pass an exact stored qualified name"
    } else {
        "pass a fully qualified name"
    };
    Err(TraceDecayError::Config {
        message: format!("symbol '{symbol}' is ambiguous ({total} matches); {guidance}"),
    })
}

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
    use tracedecay_domain::{SourceSpan, SymbolOccurrenceId};

    use crate::types::{NodeKind, Visibility};

    use super::{EditSymbolV1, narrow_symbol_for_edit};

    fn symbol(kind: NodeKind, name: &str) -> EditSymbolV1 {
        EditSymbolV1 {
            occurrence: SymbolOccurrenceId::new(format!("occurrence:{name}:{kind:?}"))
                .expect("occurrence"),
            kind,
            name: name.to_owned(),
            qualified_name: format!("src/a.rs::{name}"),
            file_path: "src/a.rs".to_owned(),
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
            start_line: 0,
            line_span: 1,
            visibility: Visibility::Pub,
        }
    }

    #[test]
    fn narrowing_prefers_declaration_over_impl_blocks() {
        let resolved = narrow_symbol_for_edit(
            "src/a.rs::Widget",
            vec![
                symbol(NodeKind::Struct, "Widget"),
                symbol(NodeKind::Impl, "Widget"),
                symbol(NodeKind::Impl, "Widget"),
            ],
        )
        .expect("declaration should win");
        assert_eq!(resolved.kind, NodeKind::Struct);
    }

    #[test]
    fn narrowing_keeps_callable_precedence_for_bare_names() {
        let resolved = narrow_symbol_for_edit(
            "run",
            vec![
                symbol(NodeKind::Module, "run"),
                symbol(NodeKind::Function, "run"),
            ],
        )
        .expect("callable should win");
        assert_eq!(resolved.kind, NodeKind::Function);
    }

    #[test]
    fn narrowing_refuses_multiple_declarations() {
        assert!(
            narrow_symbol_for_edit(
                "src/a.rs::Widget",
                vec![
                    symbol(NodeKind::Struct, "Widget"),
                    symbol(NodeKind::Struct, "Widget"),
                ],
            )
            .is_err()
        );
    }
}
