//! Generation-pinned code-graph evidence shared by project-info handlers.

use std::collections::HashMap;

use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::queries::graph::VerifiedGraphQuery;
use crate::types::NodeKind;

pub(super) const INFO_SYMBOL_CENSUS_LIMIT: usize = 500_000;
pub(super) const INFO_RELATION_LIMIT: usize = 2_000_000;

#[derive(Debug)]
pub(super) struct IndexedFileSummary {
    pub(super) path: String,
    pub(super) node_count: u32,
    pub(super) size: u64,
}

pub(super) fn all_symbols(graph: &VerifiedGraphQuery) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let page = graph.symbols_page(None, INFO_SYMBOL_CENSUS_LIMIT)?;
    if page.has_more {
        return Err(info_graph_error(
            "verified-info-symbol-budget-exhausted",
            "the indexed symbol census exceeds the project-info analytical budget",
        ));
    }
    for symbol in &page.symbols {
        required_symbol_parts(symbol)?;
    }
    Ok(page.symbols)
}

pub(super) fn indexed_files(
    cg: &crate::tracedecay::TraceDecay,
    graph: &VerifiedGraphQuery,
) -> Result<Vec<IndexedFileSummary>> {
    let mut counts = HashMap::<String, u32>::new();
    for symbol in all_symbols(graph)? {
        let path = required_file_path(&symbol)?;
        let count = counts.entry(path.to_owned()).or_default();
        *count = count.checked_add(1).ok_or_else(|| {
            info_graph_error(
                "verified-info-file-count-overflow",
                "an indexed file contains more symbols than the file listing can represent",
            )
        })?;
    }
    let mut files = counts
        .into_iter()
        .map(|(path, node_count)| {
            let project_path = crate::storage::ProjectPath::resolve(
                cg.project_root(),
                std::path::Path::new(&path),
            )?;
            let size = std::fs::metadata(project_path.absolute_path())
                .map_err(|error| TraceDecayError::Config {
                    message: format!("cannot read indexed file metadata for '{path}': {error}"),
                })?
                .len();
            Ok(IndexedFileSummary {
                path,
                node_count,
                size,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

pub(super) fn symbols_in_dir(
    graph: &VerifiedGraphQuery,
    directory: &str,
    kinds: &[NodeKind],
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let prefix = directory.trim_end_matches('/');
    let mut selected = Vec::new();
    for symbol in all_symbols(graph)? {
        let (metadata, path) = required_symbol_parts(&symbol)?;
        let path_matches = path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'));
        let kind_matches =
            NodeKind::from_str(&metadata.kind).is_some_and(|kind| kinds.contains(&kind));
        if path_matches && kind_matches {
            selected.push(symbol);
        }
    }
    Ok(selected)
}

pub(super) fn required_symbol_parts(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<(&tracedecay_code_index::lineage::LineageSymbolRecordV1, &str)> {
    Ok((required_metadata(symbol)?, required_file_path(symbol)?))
}

pub(super) fn required_metadata(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<&tracedecay_code_index::lineage::LineageSymbolRecordV1> {
    symbol.metadata.as_ref().ok_or_else(|| {
        info_graph_error(
            "verified-info-symbol-metadata-incomplete",
            &format!(
                "verified graph symbol '{}' has no extraction metadata",
                symbol.occurrence.as_str()
            ),
        )
    })
}

pub(super) fn required_file_path(symbol: &CodeGraphSymbolSummaryV1) -> Result<&str> {
    symbol
        .binding
        .as_ref()
        .and_then(|binding| binding.logical_path.as_deref())
        .ok_or_else(|| {
            info_graph_error(
                "verified-info-symbol-binding-incomplete",
                &format!(
                    "verified graph symbol '{}' has no logical file binding",
                    symbol.occurrence.as_str()
                ),
            )
        })
}

pub(super) fn end_line(
    metadata: &tracedecay_code_index::lineage::LineageSymbolRecordV1,
) -> Result<u32> {
    if metadata.line_span == 0 {
        return Err(info_graph_error(
            "verified-info-symbol-span-invalid",
            &format!(
                "verified graph symbol '{}' has an empty line span",
                metadata.occurrence.as_str()
            ),
        ));
    }
    metadata
        .start_line
        .checked_add(metadata.line_span - 1)
        .ok_or_else(|| {
            info_graph_error(
                "verified-info-symbol-span-invalid",
                &format!(
                    "verified graph symbol '{}' line span overflows",
                    metadata.occurrence.as_str()
                ),
            )
        })
}

pub(super) fn info_graph_error(reason_code: &str, detail: &str) -> TraceDecayError {
    TraceDecayError::project_route(reason_code, false, detail)
}
