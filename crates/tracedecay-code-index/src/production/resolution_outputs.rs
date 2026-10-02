//! Canonical graph resolution outputs derived while a generation seals.

use std::sync::Arc;

use tracedecay_domain::CanonicalRelationEdgeV1;
use tracedecay_graph_db::GraphDbError;

use crate::chunks::CodeIndexUnresolvedReferenceV1;
#[cfg(test)]
use crate::graph_projection::SealedCodeGraphRowsError;
use crate::graph_projection::unresolved_call_limitations;

#[cfg(test)]
use super::helpers::resolve_cross_file_references;
use super::helpers::unresolved_import_calls;
use super::resolution_view::FileSymbolsByNameV1;
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};

#[cfg(test)]
pub(super) fn resolve_files(
    files: &[Arc<FileGenerationArtifactsV1>],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<
    (
        Vec<CanonicalRelationEdgeV1>,
        Vec<CodeIndexUnresolvedReferenceV1>,
    ),
    SealedCodeGraphRowsError,
> {
    check()?;
    let cross_file_edges = resolve_cross_file_references(files)?;
    let mut edges = files
        .iter()
        .flat_map(|file| file.artifacts.edges.iter())
        .cloned()
        .collect::<Vec<_>>();
    edges.extend(cross_file_edges.iter().cloned());
    let unresolved_calls = unresolved_calls_for_edges(files, &edges, check)?;
    Ok((cross_file_edges, unresolved_calls))
}

pub(super) fn unresolved_calls_for_edges(
    files: &[Arc<FileGenerationArtifactsV1>],
    edges: &[CanonicalRelationEdgeV1],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, CodeIndexProductionErrorV1> {
    check().map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let import_unresolved = unresolved_import_calls(files, &FileSymbolsByNameV1::new(files), None);
    let references = files
        .iter()
        .flat_map(|file| {
            file.artifacts
                .unresolved_references
                .iter()
                .map(|reference| (file.authority.logical_path.as_str(), reference))
        })
        .collect::<Vec<_>>();
    unresolved_call_limitations(&references, edges.iter(), import_unresolved, check)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}
