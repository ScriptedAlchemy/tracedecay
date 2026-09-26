//! What a sealed generation's file segments contribute to its code graph,
//! read back one window at a time.

use std::collections::HashSet;
use std::sync::Arc;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeSearchChunkV1, SanitizedCodeFileV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::GraphDbError;

use crate::chunks::{
    CodeFileChunksV1, CodeFileIndexArtifactsV1, CodeIndexImportEvidenceV1,
    CodeIndexUnresolvedReferenceV1, ExactExtractionAuthorityV1,
};
use crate::graph_projection::{SealedCodeGraphRowsError, unresolved_call_limitations};
use crate::lineage::LineageSymbolRecordV1;

use super::helpers::{resolve_cross_file_references, unresolved_typescript_import_calls};
use super::partitioned_codec::{SealedGenerationFileWindowsV1, SealedGenerationSegmentReaderV1};
use super::sealed_codec::PersistedFileGenerationArtifactsV1;
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};

/// The whole-generation inputs cross-file resolution derives, the only state
/// resident across the row-emission pass.
pub(crate) struct CodeGraphResolutionV1 {
    /// Every symbol occurrence some file binds through a chunk or describes
    /// with metadata; only edges from these are retained.
    pub(crate) bound: HashSet<SymbolOccurrenceId>,
    /// The edges sealing derives across files; no file segment carries them.
    pub(crate) cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    /// The call limitations each source symbol discloses, canonically ordered.
    pub(crate) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
}

/// The rows one window of sealed files owns.
pub(crate) struct CodeGraphFileBatchV1<'a> {
    pub(crate) files: Vec<&'a SanitizedCodeFileV1>,
    pub(crate) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(crate) chunks: Vec<Arc<CodeSearchChunkV1>>,
    pub(crate) symbols: Vec<Arc<LineageSymbolRecordV1>>,
    pub(crate) edges: Vec<CanonicalRelationEdgeV1>,
}

impl SealedGenerationFileWindowsV1 {
    /// Reads every segment once and derives the cross-file graph inputs.
    ///
    /// Each file is reduced to what resolution reads, its symbols, imports,
    /// edges, unresolved references, and document, as its window decodes;
    /// chunk rows and clone streams are dropped with the window.
    pub(crate) fn resolve_code_graph(
        &self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<CodeGraphResolutionV1, SealedCodeGraphRowsError> {
        let mut bound = HashSet::new();
        let mut files = Vec::new();
        self.for_each_file_window(read_segment, |window| {
            check()?;
            for (_, page) in window {
                bound.extend(
                    page.artifacts
                        .chunks
                        .chunks
                        .iter()
                        .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.clone()),
                );
                files.push(resolution_file(page)?);
            }
            Ok::<(), SealedCodeGraphRowsError>(())
        })?;
        bound.extend(
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter())
                .map(|symbol| symbol.occurrence.clone()),
        );
        check()?;
        let cross_file_edges = resolve_cross_file_references(&files)?;
        check()?;
        let typescript_unresolved = unresolved_typescript_import_calls(&files);
        let references = files
            .iter()
            .flat_map(|file| {
                file.artifacts
                    .unresolved_references
                    .iter()
                    .map(|reference| (file.authority.logical_path.as_str(), reference))
            })
            .collect::<Vec<_>>();
        let unresolved_calls = unresolved_call_limitations(
            &references,
            files
                .iter()
                .flat_map(|file| file.artifacts.edges.iter())
                .chain(&cross_file_edges),
            typescript_unresolved,
            check,
        )?;
        Ok(CodeGraphResolutionV1 {
            bound,
            cross_file_edges,
            unresolved_calls,
        })
    }

    /// Reads every segment again and hands each window's graph rows to
    /// `visit`, which owns them until it returns.
    pub(crate) fn for_each_code_graph_batch<E>(
        &self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        visit: &mut dyn FnMut(CodeGraphFileBatchV1<'_>) -> Result<(), E>,
    ) -> Result<(), E>
    where
        E: From<CodeIndexProductionErrorV1>,
    {
        self.for_each_file_window(read_segment, |window| {
            let mut batch = CodeGraphFileBatchV1 {
                files: Vec::with_capacity(window.len()),
                imports: Vec::new(),
                chunks: Vec::new(),
                symbols: Vec::new(),
                edges: Vec::new(),
            };
            for (file, page) in window {
                let CodeFileIndexArtifactsV1 {
                    chunks,
                    symbols,
                    edges,
                    imports,
                    ..
                } = page.artifacts;
                batch.files.push(file);
                batch.imports.extend(imports);
                batch.chunks.extend(chunks.chunks);
                batch.symbols.extend(symbols);
                batch.edges.extend(edges);
            }
            visit(batch)
        })
    }
}

/// A file reduced to the fields cross-file resolution reads. Its document
/// and exact authority describe the chunk rows it keeps, which are none.
fn resolution_file(
    page: PersistedFileGenerationArtifactsV1,
) -> Result<Arc<FileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
    let PersistedFileGenerationArtifactsV1 {
        authority,
        extraction,
        artifacts,
    } = page;
    let CodeFileIndexArtifactsV1 {
        chunks,
        symbols,
        edges,
        imports,
        unresolved_references,
        ..
    } = artifacts;
    let mut document = chunks.document;
    document.chunk_ids = Vec::new();
    let chunks = CodeFileChunksV1 {
        document,
        chunks: Vec::new(),
    };
    let exact_authority =
        ExactExtractionAuthorityV1::restore(&chunks).map_err(CodeIndexProductionErrorV1::Chunk)?;
    Ok(Arc::new(FileGenerationArtifactsV1 {
        authority,
        extraction,
        artifacts: CodeFileIndexArtifactsV1 {
            chunks,
            symbols,
            edges,
            edge_abstentions: Vec::new(),
            imports,
            clone_bodies: Vec::new(),
            schema_evidence: None,
            unresolved_references,
        },
        exact_authority,
    }))
}
