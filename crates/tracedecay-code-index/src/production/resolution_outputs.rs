//! Canonical graph resolution outputs derived while a generation seals.

use std::sync::Arc;

use tracedecay_domain::CanonicalRelationEdgeV1;
use tracedecay_graph_db::GraphDbError;

use crate::chunks::CodeIndexUnresolvedReferenceV1;
use crate::graph_projection::unresolved_call_limitations;

use super::helpers::unresolved_import_calls;
use super::resolution_view::FileSymbolsByNameV1;
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};

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
