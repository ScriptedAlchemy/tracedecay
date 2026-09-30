//! What a sealed generation's file segments contribute to its code graph,
//! read back one window at a time.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeSearchChunkV1, FileOccurrenceId, SanitizedCodeFileV1,
    SnapshotFileDispositionV1, SymbolOccurrenceId,
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

use super::graph_base_inputs::{
    CodeGraphBaseFileV1, CodeGraphBaseInputsWriterV1, read_code_graph_base_inputs,
};
use super::helpers::{resolve_cross_file_references, unresolved_import_calls};
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
    ///
    /// `inputs`, when given, records every file's resolution inputs and the
    /// resolution outputs, the base a later refresh layers over.
    pub(crate) fn resolve_code_graph<'a>(
        &'a self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        mut inputs: Option<CodeGraphBaseInputsWriterV1>,
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
        self.for_each_file_window(
            read_segment,
            |_, _| true,
            |window| {
                check()?;
                window_lengths.push(window.len());
                for (snapshot_file, segment_digest, page) in window {
                    let file_bindings = code_graph_symbol_bindings(
                        Some(&files_by_occurrence),
                        self.generation_id(),
                        &page.artifacts.chunks.chunks,
                        check,
                    )?;
                    bound.extend(file_bindings.keys().cloned());
                    let page = reduced_page(page);
                    if let Some(inputs) = inputs.as_mut() {
                        inputs.file(CodeGraphBaseFileV1 {
                            file_occurrence_id: snapshot_file.file_occurrence_id.clone(),
                            segment_digest: Some(segment_digest),
                            page: Some(page.clone()),
                            bindings: file_bindings.clone(),
                        })?;
                    }
                    snapshot_files.push(snapshot_file.file_occurrence_id.clone());
                    bindings.push(file_bindings);
                    files.push(resolution_file(page)?);
                }
                Ok::<(), SealedCodeGraphRowsError>(())
            },
        )?;
        if let Some(inputs) = inputs.as_mut() {
            inputs.unsegmented_files(self.snapshot())?;
        }
        bound.extend(
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter())
                .map(|symbol| symbol.occurrence.clone()),
        );
        let (cross_file_edges, unresolved_calls) = resolve_files(&files, check)?;
        if let Some(inputs) = inputs {
            inputs.finish(cross_file_edges.clone(), unresolved_calls.clone())?;
        }

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

/// A refresh's graph inputs relative to the base it layers over.
///
/// Resolution runs over every child file exactly as a cold build runs it,
/// but a file whose segment the base already sealed contributes the base's
/// recorded inputs instead of being decoded. Emission then needs only the
/// files the base does not carry, the base files the child dropped, and the
/// whole-generation resolution outputs of both sides.
pub(crate) struct CodeGraphLayeredResolutionV1<'a> {
    /// Child files the base does not carry, one batch per file.
    pub(crate) added: Vec<CodeGraphFileBatchV1<'a>>,
    /// Child files whose inputs the base recorded, one batch per file.
    pub(crate) unchanged: Vec<CodeGraphFileBatchV1<'a>>,
    /// Base files the child does not carry with the same inputs.
    pub(crate) removed: Vec<CodeGraphRemovedFileV1>,
    pub(crate) bound: HashSet<SymbolOccurrenceId>,
    pub(crate) base_bound: HashSet<SymbolOccurrenceId>,
    pub(crate) cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    pub(crate) base_cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    pub(crate) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    pub(crate) base_unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    /// Segments decoded because the base did not carry them.
    pub(crate) reextracted_files: usize,
    /// The code generation the base's inputs were recorded for.
    pub(crate) base_generation: tracedecay_domain::CodeGenerationId,
}

/// A base file the child no longer carries: the inputs its rows came from.
pub(crate) struct CodeGraphRemovedFileV1 {
    pub(crate) file_occurrence_id: FileOccurrenceId,
    pub(crate) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(crate) symbols: Vec<Arc<LineageSymbolRecordV1>>,
    pub(crate) edges: Vec<CanonicalRelationEdgeV1>,
    pub(crate) bindings: BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
}

impl SealedGenerationFileWindowsV1 {
    /// Resolves this generation's graph over the base whose inputs `base`
    /// holds, decoding only the file segments the base did not seal. `None`
    /// when those inputs were recorded for another projector or revision.
    #[hotpath::measure(label = "code_index.graph.layered.resolve")]
    pub(crate) fn resolve_layered_code_graph<'a>(
        &'a self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        base: &std::path::Path,
        projector_revision: &str,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Option<CodeGraphLayeredResolutionV1<'a>>, SealedCodeGraphRowsError> {
        check()?;
        let Some(mut base) = read_code_graph_base_inputs(base, projector_revision)? else {
            return Ok(None);
        };
        check()?;
        let base_bound = bound_symbols(base.files.values().filter_map(|file| {
            file.page
                .as_ref()
                .map(|page| (&file.bindings, page.artifacts.symbols.as_slice()))
        }));
        let mut decoded = self.decode_files_base_lacks(read_segment, &base.files, check)?;
        let reextracted_files = decoded.len();
        #[cfg(feature = "hotpath")]
        hotpath::gauge!("code_index.graph.layered.files_reextracted").inc(reextracted_files as u64);

        let mut files = Vec::new();
        let mut placed = Vec::new();
        for entry in self.segmented_files() {
            check()?;
            let (snapshot_file, digest) = entry?;
            let (page, bindings, reused) = match decoded.remove(&snapshot_file.file_occurrence_id) {
                Some((page, bindings)) => (page, bindings, false),
                None => {
                    let (page, bindings) = take_base_inputs(
                        &mut base.files,
                        &snapshot_file.file_occurrence_id,
                        digest,
                    )?;
                    (page, bindings, true)
                }
            };
            files.push(resolution_file(page)?);
            placed.push((snapshot_file, bindings, reused));
        }
        let unsegmented_added = self
            .snapshot()
            .files
            .iter()
            .filter(|file| file.disposition != SnapshotFileDispositionV1::Present)
            .filter(|file| {
                let unchanged = base
                    .files
                    .get(&file.file_occurrence_id)
                    .is_some_and(|base_file| base_file.segment_digest.is_none());
                if unchanged {
                    base.files.remove(&file.file_occurrence_id);
                }
                !unchanged
            })
            .collect::<Vec<_>>();
        let removed = base
            .files
            .into_values()
            .map(CodeGraphRemovedFileV1::from)
            .collect::<Vec<_>>();
        let bound = bound_symbols(
            placed
                .iter()
                .zip(&files)
                .map(|((_, bindings, _), file)| (bindings, file.artifacts.symbols.as_slice())),
        );
        let (cross_file_edges, unresolved_calls) = resolve_files(&files, check)?;

        let mut added = Vec::new();
        let mut unchanged = Vec::new();
        for (file, (snapshot_file, bindings, reused)) in files.into_iter().zip(placed) {
            let batch = CodeGraphFileBatchV1 {
                files: vec![snapshot_file],
                imports: file.artifacts.imports.clone(),
                chunks: Vec::new(),
                symbols: file.artifacts.symbols.clone(),
                edges: file.artifacts.edges.clone(),
                bindings,
            };
            if reused {
                unchanged.push(batch);
            } else {
                added.push(batch);
            }
        }
        added.extend(
            unsegmented_added
                .into_iter()
                .map(|file| CodeGraphFileBatchV1 {
                    files: vec![file],
                    imports: Vec::new(),
                    chunks: Vec::new(),
                    symbols: Vec::new(),
                    edges: Vec::new(),
                    bindings: BTreeMap::new(),
                }),
        );
        Ok(Some(CodeGraphLayeredResolutionV1 {
            added,
            unchanged,
            removed,
            bound,
            base_bound,
            cross_file_edges,
            base_cross_file_edges: base.cross_file_edges,
            unresolved_calls,
            base_unresolved_calls: base.unresolved_calls,
            reextracted_files,
            base_generation: base.generation,
        }))
    }

    /// Decodes every segment whose inputs `base_files` does not record under
    /// the same segment digest, reduced to its resolution inputs.
    fn decode_files_base_lacks(
        &self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        base_files: &BTreeMap<FileOccurrenceId, CodeGraphBaseFileV1>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<BTreeMap<FileOccurrenceId, FileResolutionInputsV1>, SealedCodeGraphRowsError> {
        let files_by_occurrence = self
            .snapshot()
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();
        let mut decoded = BTreeMap::new();
        self.for_each_file_window(
            read_segment,
            |occurrence, digest| {
                !base_files.get(occurrence).is_some_and(|file| {
                    file.page.is_some() && file.segment_digest.as_ref() == Some(digest)
                })
            },
            |window| {
                check()?;
                for (snapshot_file, _, page) in window {
                    let file_bindings = code_graph_symbol_bindings(
                        Some(&files_by_occurrence),
                        self.generation_id(),
                        &page.artifacts.chunks.chunks,
                        check,
                    )?;
                    decoded.insert(
                        snapshot_file.file_occurrence_id.clone(),
                        (reduced_page(page), file_bindings),
                    );
                }
                Ok::<(), SealedCodeGraphRowsError>(())
            },
        )?;
        Ok(decoded)
    }
}

/// One file's resolution inputs: its reduced page and its symbol bindings.
type FileResolutionInputsV1 = (
    PersistedFileGenerationArtifactsV1,
    BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
);

/// Every symbol occurrence the files bind through a chunk or describe.
fn bound_symbols<'b>(
    files: impl Iterator<
        Item = (
            &'b BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
            &'b [Arc<LineageSymbolRecordV1>],
        ),
    >,
) -> HashSet<SymbolOccurrenceId> {
    let mut bound = HashSet::new();
    for (bindings, symbols) in files {
        bound.extend(bindings.keys().cloned());
        bound.extend(symbols.iter().map(|symbol| symbol.occurrence.clone()));
    }
    bound
}

/// The base's recorded inputs for a file the refresh reuses unchanged.
fn take_base_inputs(
    base_files: &mut BTreeMap<FileOccurrenceId, CodeGraphBaseFileV1>,
    occurrence: &FileOccurrenceId,
    digest: &tracedecay_domain::ManifestDigest,
) -> Result<FileResolutionInputsV1, CodeIndexProductionErrorV1> {
    let file = base_files
        .remove(occurrence)
        .filter(|file| file.segment_digest.as_ref() == Some(digest))
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "a refresh lost the base inputs it chose to reuse".to_owned(),
            )
        })?;
    let page = file.page.ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("reused base inputs carry no page".to_owned())
    })?;
    Ok((page, file.bindings))
}

impl From<CodeGraphBaseFileV1> for CodeGraphRemovedFileV1 {
    fn from(file: CodeGraphBaseFileV1) -> Self {
        let (imports, symbols, edges) = match file.page {
            Some(page) => (
                page.artifacts.imports,
                page.artifacts.symbols,
                page.artifacts.edges,
            ),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        Self {
            file_occurrence_id: file.file_occurrence_id,
            imports,
            symbols,
            edges,
            bindings: file.bindings,
        }
    }
}

/// Whole-generation resolution over every file's inputs: the cross-file
/// edges and the call limitations each source symbol discloses.
fn resolve_files(
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
    check()?;
    let import_unresolved = unresolved_import_calls(files);
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
        import_unresolved,
        check,
    )?;
    Ok((cross_file_edges, unresolved_calls))
}

/// A file page reduced to the fields cross-file resolution reads. Its
/// document and exact authority describe the chunk rows it keeps, which are
/// none.
fn reduced_page(page: PersistedFileGenerationArtifactsV1) -> PersistedFileGenerationArtifactsV1 {
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
        callable_arities,
        ..
    } = artifacts;
    let mut document = chunks.document;
    document.chunk_ids = Vec::new();
    PersistedFileGenerationArtifactsV1 {
        authority,
        extraction,
        artifacts: CodeFileIndexArtifactsV1 {
            chunks: CodeFileChunksV1 {
                document,
                chunks: Vec::new(),
            },
            symbols,
            edges,
            edge_abstentions: Vec::new(),
            imports,
            clone_bodies: Vec::new(),
            schema_evidence: None,
            unresolved_references,
            callable_arities,
        },
    }
}

/// A reduced page as the resolution input it is.
fn resolution_file(
    page: PersistedFileGenerationArtifactsV1,
) -> Result<Arc<FileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
    let PersistedFileGenerationArtifactsV1 {
        authority,
        extraction,
        artifacts,
    } = reduced_page(page);
    let exact_authority = ExactExtractionAuthorityV1::restore(&artifacts.chunks)
        .map_err(CodeIndexProductionErrorV1::Chunk)?;
    Ok(Arc::new(FileGenerationArtifactsV1 {
        authority,
        extraction,
        artifacts,
        exact_authority,
    }))
}
