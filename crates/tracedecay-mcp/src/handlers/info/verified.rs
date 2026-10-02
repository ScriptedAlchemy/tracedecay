//! Generation-pinned code-graph evidence shared by project-info handlers.

use std::collections::HashSet;
use std::path::Path;

use tracedecay_code_index::graph_projection::{
    CodeGraphFileSymbolCountV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_code_index::lineage::LineageSymbolRecordV1;
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

pub(super) const INFO_RELATION_LIMIT: usize = 2_000_000;

#[derive(Debug)]
pub(super) struct IndexedFileSummary {
    pub(super) path: String,
    pub(super) node_count: u32,
    pub(super) size: u64,
}

/// Every file that binds at least one symbol, with its symbol count, read
/// from the generation census rather than by paging its symbols.
pub(super) fn indexed_file_counts(
    graph: &VerifiedGraphQuery,
) -> Result<Vec<CodeGraphFileSymbolCountV1>> {
    Ok(graph.census(usize::MAX)?.largest_files)
}

/// The symbols bound to `files`, in canonical occurrence order. The census
/// counts size the read exactly, so it touches only these files' symbols.
pub(super) fn symbols_in_files(
    graph: &VerifiedGraphQuery,
    files: &[CodeGraphFileSymbolCountV1],
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    let total = files.iter().try_fold(0_usize, |total, file| {
        usize::try_from(file.symbols)
            .ok()
            .and_then(|symbols| total.checked_add(symbols))
            .ok_or_else(|| {
                info_graph_error(
                    "verified-info-file-count-overflow",
                    "the selected files contain more symbols than one read can address",
                )
            })
    })?;
    if total == 0 {
        return Ok(Vec::new());
    }
    let paths = files
        .iter()
        .map(|file| file.logical_path.clone())
        .collect::<HashSet<_>>();
    let page = graph.symbols_in_logical_files_page(&paths, None, total, total)?;
    for symbol in &page.symbols {
        required_symbol_parts(symbol)?;
    }
    Ok(page.symbols)
}

pub(super) async fn indexed_files(
    project_root: &Path,
    graph: &VerifiedGraphQuery,
) -> Result<Vec<IndexedFileSummary>> {
    let counts = indexed_file_counts(graph)?;
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut files = counts
            .into_iter()
            .map(|file| {
                let node_count = u32::try_from(file.symbols).map_err(|_| {
                    info_graph_error(
                        "verified-info-file-count-overflow",
                        "an indexed file contains more symbols than the file listing can represent",
                    )
                })?;
                let path = file.logical_path;
                let project_path = tracedecay_runtime_core::storage::ProjectPath::resolve(
                    &project_root,
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
    })
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("indexed file metadata scan failed to join: {join_error}"),
    })?
}

pub(super) fn symbols_in_dir(
    graph: &VerifiedGraphQuery,
    directory: &str,
    kinds: &[NodeKind],
) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
    // Trim every trailing slash first. `repository_path_matches_scope` treats
    // a leftover `/` as a literal character, so `src/` would miss `src/lib.rs`.
    let prefix = directory.trim_end_matches('/');
    let files = indexed_file_counts(graph)?
        .into_iter()
        .filter(|file| {
            tracedecay_domain::repository_path_matches_scope(&file.logical_path, Some(prefix))
        })
        .collect::<Vec<_>>();
    let mut selected = Vec::new();
    for symbol in symbols_in_files(graph, &files)? {
        let metadata = required_metadata(&symbol)?;
        if NodeKind::from_str(&metadata.kind).is_some_and(|kind| kinds.contains(&kind)) {
            selected.push(symbol);
        }
    }
    Ok(selected)
}

pub(super) fn required_symbol_parts(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<(&LineageSymbolRecordV1, &str)> {
    Ok((required_metadata(symbol)?, required_file_path(symbol)?))
}

pub(super) fn required_metadata(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<&LineageSymbolRecordV1> {
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

pub(super) fn end_line(metadata: &LineageSymbolRecordV1) -> Result<u32> {
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
