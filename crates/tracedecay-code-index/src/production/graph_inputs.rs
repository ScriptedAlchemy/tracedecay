//! What a sealed generation's file segments contribute to its code graph,
//! read back one window at a time.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeSearchChunkV1, SanitizedCodeFileV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::GraphDbError;

use crate::chunks::{
    CodeFileChunksV1, CodeFileIndexArtifactsV1, CodeIndexImportEvidenceV1,
    CodeIndexUnresolvedReferenceV1, ExactExtractionAuthorityV1,
};
use crate::graph_projection::{
    CodeGraphProjectionError, CodeGraphSymbolBindingV1, SealedCodeGraphRowsError,
    code_graph_symbol_bindings, unresolved_call_limitations,
};
use crate::lineage::LineageSymbolRecordV1;

use super::helpers::{resolve_cross_file_references, unresolved_typescript_import_calls};
use super::partitioned_codec::{SealedGenerationFileWindowsV1, SealedGenerationSegmentReaderV1};
use super::sealed_codec::PersistedFileGenerationArtifactsV1;
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};

/// The whole-generation inputs cross-file resolution derives, the only state
/// resident across the row-emission pass.
pub(crate) struct CodeGraphResolutionV1<'a> {
    /// Every symbol occurrence some file binds through a chunk or describes
    /// with metadata; only edges from these are retained.
    pub(crate) bound: HashSet<SymbolOccurrenceId>,
    /// The edges sealing derives across files; no file segment carries them.
    pub(crate) cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    /// The call limitations each source symbol discloses, canonically ordered.
    pub(crate) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    /// Graph-only file batches retained from the authenticated decode.
    pub(crate) batches: Vec<CodeGraphFileBatchV1<'a>>,
}

/// The rows one window of sealed files owns.
pub(crate) struct CodeGraphFileBatchV1<'a> {
    pub(crate) files: Vec<&'a SanitizedCodeFileV1>,
    pub(crate) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(crate) chunks: Vec<Arc<CodeSearchChunkV1>>,
    pub(crate) symbols: Vec<Arc<LineageSymbolRecordV1>>,
    pub(crate) edges: Vec<CanonicalRelationEdgeV1>,
    pub(crate) bindings: BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
}

impl SealedGenerationFileWindowsV1 {
    /// Reads every segment once and derives the cross-file graph inputs and
    /// compact row-emission batches.
    ///
    /// Each file is reduced to what resolution reads, its symbols, imports,
    /// edges, unresolved references, and document, as its window decodes;
    /// chunk text and clone streams are dropped with the window.
    pub(crate) fn resolve_code_graph<'a>(
        &'a self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<CodeGraphResolutionV1<'a>, SealedCodeGraphRowsError> {
        let mut bound = HashSet::new();
        let mut files = Vec::new();
        let mut snapshot_files = Vec::new();
        let mut bindings = Vec::new();
        let mut window_lengths = Vec::new();
        let files_by_occurrence = self
            .snapshot()
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();
        self.for_each_file_window(read_segment, |window| {
            check()?;
            window_lengths.push(window.len());
            for (snapshot_file, page) in window {
                let file_bindings = code_graph_symbol_bindings(
                    Some(&files_by_occurrence),
                    self.generation_id(),
                    &page.artifacts.chunks.chunks,
                    check,
                )?;
                bound.extend(file_bindings.keys().cloned());
                snapshot_files.push(snapshot_file.file_occurrence_id.clone());
                bindings.push(file_bindings);
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
        drop(references);

        let mut files = files.into_iter();
        let mut snapshot_files = snapshot_files.into_iter();
        let mut bindings = bindings.into_iter();
        let mut batches = Vec::with_capacity(window_lengths.len());
        for window_len in window_lengths {
            let mut batch = CodeGraphFileBatchV1 {
                files: Vec::with_capacity(window_len),
                imports: Vec::new(),
                chunks: Vec::new(),
                symbols: Vec::new(),
                edges: Vec::new(),
                bindings: BTreeMap::new(),
            };
            for _ in 0..window_len {
                let file = files.next().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed graph batch is missing a decoded file".to_owned(),
                    )
                })?;
                let snapshot_file_id = snapshot_files.next().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed graph batch is missing its snapshot file".to_owned(),
                    )
                })?;
                let snapshot_file = files_by_occurrence
                    .get(&snapshot_file_id)
                    .copied()
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed graph batch names a file outside its snapshot".to_owned(),
                        )
                    })?;
                let file_bindings = bindings.next().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed graph batch is missing its symbol bindings".to_owned(),
                    )
                })?;
                batch.files.push(snapshot_file);
                batch.imports.extend(file.artifacts.imports.iter().cloned());
                batch.symbols.extend(file.artifacts.symbols.iter().cloned());
                batch.edges.extend(file.artifacts.edges.iter().cloned());
                for (occurrence, binding) in file_bindings {
                    match batch.bindings.entry(occurrence) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(binding);
                        }
                        std::collections::btree_map::Entry::Occupied(entry)
                            if entry.get() == &binding => {}
                        std::collections::btree_map::Entry::Occupied(_) => {
                            return Err(CodeGraphProjectionError::Contract(
                                "one symbol occurrence has conflicting graph candidate bindings"
                                    .to_owned(),
                            )
                            .into());
                        }
                    }
                }
            }
            batches.push(batch);
        }
        Ok(CodeGraphResolutionV1 {
            bound,
            cross_file_edges,
            unresolved_calls,
            batches,
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
