//! A refresh's code graph as a delta over the sealed generation it replaces.
//!
//! A cold build's rows are, file by file, a pure function of that file's
//! inputs, except for what whole-generation resolution decides: which
//! symbols are bound (and so which edge targets are placeholder entities),
//! which calls each source symbol discloses as unresolved, and the
//! cross-file edges. The delta therefore carries the rows of files the base
//! does not hold, re-emits rows of unchanged files only where those
//! decisions moved, hides every base row that no longer exists, and never
//! touches the rest. The base's own row digests plus this delta give exactly
//! the digest a cold build of the same tree records.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use tracedecay_domain::{CanonicalRelationEdgeV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphDbError, GraphEntityId, GraphLayeredRowSpill, GraphProjectionIdentity,
    GraphProjectorRevision, GraphRelationId, LayeredGraphGeneration,
};

use super::builder::{
    CodeGraphRowBatch, CodeGraphRowContext, edge_relation_id, emit_code_graph_rows,
    file_symbol_relation_id, group_unresolved_calls,
};
use super::schema::{file_entity_id, file_import_relation_id_with, import_entity_id};
use super::{
    CURRENT_GENERATION_ENTITY, CodeGraphProjectionError, SealedCodeGraphRowsError, SymbolRecordV1,
    code_graph_generation_id, code_graph_manifest_identity, current_generation_entity, projection,
    symbol_entity, symbol_entity_id,
};
use crate::production::{
    CodeGraphLayeredResolutionV1, CodeGraphRemovedFileV1, SealedGenerationFileWindowsV1,
    SealedGenerationSegmentReaderV1,
};

/// What a layered build did, beside the rows it sealed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CodeGraphLayeredReportV1 {
    /// File segments decoded because the base did not carry them.
    pub reextracted_files: usize,
    /// Child files whose resolution inputs came from the base.
    pub reused_files: usize,
    /// Base files the child dropped or changed.
    pub removed_files: usize,
    /// `(entities, relations)` the delta container encodes.
    pub delta_rows: (usize, usize),
}

/// A layered graph generation and how it was built.
pub struct CodeGraphLayeredBuildV1 {
    pub generation: LayeredGraphGeneration,
    pub report: CodeGraphLayeredReportV1,
}

/// Builds a sealed code generation's graph as a delta over `spill`'s base.
///
/// `Ok(None)` when the base carries no resolution inputs this projector can
/// read, which a cold build answers.
#[hotpath::measure(label = "code_index.graph.build_layered_rows")]
pub fn build_layered_code_graph_rows(
    projection_identity: GraphProjectionIdentity,
    source: &SealedGenerationFileWindowsV1,
    read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
    projector_revision: &GraphProjectorRevision,
    mut spill: GraphLayeredRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<CodeGraphLayeredBuildV1>, SealedCodeGraphRowsError> {
    check()?;
    if projection_identity.projection != projection()? {
        return Err(CodeGraphProjectionError::Contract(
            "code graph projection identity uses a foreign projector".to_owned(),
        )
        .into());
    }
    let generation = source.generation_id().clone();
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let Some(inputs) = spill.base_attachment() else {
        return Ok(None);
    };
    let Some(resolution) = source.resolve_layered_code_graph(
        read_segment,
        &inputs,
        projector_revision.as_str(),
        check,
    )?
    else {
        return Ok(None);
    };
    if code_graph_generation_id(&resolution.base_generation, projector_revision)?
        != *spill.base().generation()
    {
        return Err(CodeGraphProjectionError::Contract(
            "layered base inputs belong to a different graph generation".to_owned(),
        )
        .into());
    }
    let report = hotpath::measure_block!(
        "code_index.graph.build_layered_rows.emit",
        emit_delta(
            &projection_identity,
            source,
            &generation,
            &resolution,
            &mut spill,
            check,
        )
    )?;
    let identity =
        code_graph_manifest_identity(projection_identity, &generation, projector_revision)?;
    let generation = hotpath::measure_block!(
        "code_index.graph.build_layered_rows.finish",
        spill.finish(identity, check)
    )?;
    let report = CodeGraphLayeredReportV1 {
        delta_rows: generation.delta_row_counts(),
        ..report
    };
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index.graph.layered.delta_entities").inc(report.delta_rows.0 as u64);
        hotpath::gauge!("code_index.graph.layered.delta_relations").inc(report.delta_rows.1 as u64);
        hotpath::gauge!("code_index.graph.layered.files_reused").inc(report.reused_files as u64);
    }
    Ok(Some(CodeGraphLayeredBuildV1 { generation, report }))
}

fn emit_delta(
    projection_identity: &GraphProjectionIdentity,
    source: &SealedGenerationFileWindowsV1,
    generation: &tracedecay_domain::CodeGenerationId,
    resolution: &CodeGraphLayeredResolutionV1<'_>,
    spill: &mut GraphLayeredRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<CodeGraphLayeredReportV1, SealedCodeGraphRowsError> {
    let unresolved_by_source = group_unresolved_calls(&resolution.unresolved_calls, check)?;
    let base_unresolved_by_source =
        group_unresolved_calls(&resolution.base_unresolved_calls, check)?;
    let files = source
        .snapshot()
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, file))
        .collect::<BTreeMap<_, _>>();
    let context = CodeGraphRowContext {
        projection: projection_identity,
        generation,
        files: Some(&files),
        bound: &resolution.bound,
        unresolved_by_source: &unresolved_by_source,
    };
    let no_bindings = BTreeMap::new();
    let emit = |spill: &mut GraphLayeredRowSpill,
                batch: CodeGraphRowBatch<'_>,
                with_relations: bool|
     -> Result<(), SealedCodeGraphRowsError> {
        let rows = emit_code_graph_rows(&context, &batch, check)?;
        let relations = if with_relations {
            rows.relations
        } else {
            Vec::new()
        };
        spill.push_batch(rows.entities, relations, check)?;
        Ok(())
    };

    // Files the base does not hold: every row they own.
    for batch in &resolution.added {
        check()?;
        emit(
            spill,
            CodeGraphRowBatch {
                files: &batch.files,
                imports: &batch.imports,
                chunks: &batch.chunks,
                symbols: &batch.symbols,
                edges: &batch.edges,
                bindings: Some(&batch.bindings),
            },
            true,
        )?;
    }
    // Base files the child does not hold: every row they owned.
    for removed in &resolution.removed {
        check()?;
        let (entities, relations) = removed_file_rows(removed)?;
        spill.hide(entities, relations);
    }
    // Unchanged files: only where whole-generation resolution moved.
    let mut retention_gained = Vec::new();
    let mut retention_lost = Vec::new();
    for batch in &resolution.unchanged {
        check()?;
        let occurrences = batch
            .bindings
            .keys()
            .chain(batch.symbols.iter().map(|symbol| &symbol.occurrence))
            .collect::<BTreeSet<_>>();
        let moved = occurrences
            .into_iter()
            .filter(|occurrence| {
                unresolved_by_source.get(occurrence) != base_unresolved_by_source.get(occurrence)
            })
            .collect::<BTreeSet<_>>();
        if !moved.is_empty() {
            let symbols = batch
                .symbols
                .iter()
                .filter(|symbol| moved.contains(&symbol.occurrence))
                .cloned()
                .collect::<Vec<_>>();
            let bindings = batch
                .bindings
                .iter()
                .filter(|(occurrence, _)| moved.contains(occurrence))
                .map(|(occurrence, binding)| (occurrence.clone(), binding.clone()))
                .collect::<BTreeMap<_, _>>();
            emit(
                spill,
                CodeGraphRowBatch {
                    files: &[],
                    imports: &[],
                    chunks: &[],
                    symbols: &symbols,
                    edges: &[],
                    bindings: Some(&bindings),
                },
                false,
            )?;
        }
        for edge in &batch.edges {
            match (
                resolution.base_bound.contains(&edge.from_occurrence),
                resolution.bound.contains(&edge.from_occurrence),
            ) {
                (false, true) => retention_gained.push(edge.clone()),
                (true, false) => retention_lost.push(edge_relation_id(edge)?),
                _ => {}
            }
        }
    }
    // Cross-file edges resolution derives anew, keyed by the relation each
    // projects to, a digest of the whole edge.
    let cross = retained_by_relation(&resolution.cross_file_edges, &resolution.bound)?;
    let base_cross =
        retained_by_relation(&resolution.base_cross_file_edges, &resolution.base_bound)?;
    for (identity, edge) in &cross {
        if !base_cross.contains_key(identity) {
            retention_gained.push((*edge).clone());
        }
    }
    retention_lost.extend(
        base_cross
            .keys()
            .filter(|identity| !cross.contains_key(*identity))
            .cloned(),
    );
    emit(
        spill,
        CodeGraphRowBatch {
            files: &[],
            imports: &[],
            chunks: &[],
            symbols: &[],
            edges: &retention_gained,
            bindings: Some(&no_bindings),
        },
        true,
    )?;
    spill.hide(Vec::new(), retention_lost);
    // Edge targets no file binds: placeholder entities on either side.
    let placeholders = |edges: &mut dyn Iterator<Item = &CanonicalRelationEdgeV1>,
                        bound: &HashSet<SymbolOccurrenceId>| {
        edges
            .filter(|edge| {
                bound.contains(&edge.from_occurrence) && !bound.contains(&edge.to_occurrence)
            })
            .map(|edge| edge.to_occurrence.clone())
            .collect::<BTreeSet<_>>()
    };
    let unchanged_edges = || {
        resolution
            .unchanged
            .iter()
            .flat_map(|batch| batch.edges.iter())
    };
    let child_placeholders = placeholders(
        &mut unchanged_edges()
            .chain(resolution.added.iter().flat_map(|batch| batch.edges.iter()))
            .chain(&resolution.cross_file_edges),
        &resolution.bound,
    );
    let base_placeholders = placeholders(
        &mut unchanged_edges()
            .chain(resolution.removed.iter().flat_map(|file| file.edges.iter()))
            .chain(&resolution.base_cross_file_edges),
        &resolution.base_bound,
    );
    let mut appeared = Vec::new();
    for occurrence in child_placeholders.difference(&base_placeholders) {
        check()?;
        appeared.push(symbol_entity(
            symbol_entity_id(occurrence)?,
            SymbolRecordV1 {
                occurrence: occurrence.clone(),
                binding: None,
                metadata: None,
                unresolved_calls: unresolved_by_source
                    .get(occurrence)
                    .cloned()
                    .unwrap_or_default(),
            },
        )?);
    }
    spill.push_batch(appeared, Vec::new(), check)?;
    spill.hide(
        base_placeholders
            .difference(&child_placeholders)
            .map(symbol_entity_id)
            .collect::<Result<Vec<_>, _>>()?,
        Vec::new(),
    );
    // The generation marker counts every entity, itself included.
    let marker = GraphEntityId::new(CURRENT_GENERATION_ENTITY)?;
    spill.hide([marker], Vec::new());
    let entities = spill.entity_count(check)?;
    let projection_node_count = entities.checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    spill.push_batch(
        vec![current_generation_entity(
            generation,
            projection_node_count,
        )?],
        Vec::new(),
        check,
    )?;
    Ok(CodeGraphLayeredReportV1 {
        reextracted_files: resolution.reextracted_files,
        reused_files: resolution.unchanged.len(),
        removed_files: resolution.removed.len(),
        delta_rows: (0, 0),
    })
}

/// The edges a generation retains, those whose source it binds, by the
/// relation identity each projects to.
fn retained_by_relation<'e>(
    edges: &'e [CanonicalRelationEdgeV1],
    bound: &HashSet<SymbolOccurrenceId>,
) -> Result<BTreeMap<GraphRelationId, &'e CanonicalRelationEdgeV1>, CodeGraphProjectionError> {
    edges
        .iter()
        .filter(|edge| bound.contains(&edge.from_occurrence))
        .map(|edge| Ok((edge_relation_id(edge)?, edge)))
        .collect()
}

/// Every identity a base file's rows took, exactly as its cold emission
/// derived them; placeholder targets are decided generation-wide.
fn removed_file_rows(
    file: &CodeGraphRemovedFileV1,
) -> Result<(Vec<GraphEntityId>, Vec<GraphRelationId>), CodeGraphProjectionError> {
    let mut entities = vec![file_entity_id(&file.file_occurrence_id)?];
    let mut relations = Vec::new();
    for import in &file.imports {
        let identity = import_entity_id(import)?;
        relations.push(file_import_relation_id_with(import, &identity)?);
        entities.push(identity);
    }
    for occurrence in file
        .bindings
        .keys()
        .chain(file.symbols.iter().map(|symbol| &symbol.occurrence))
        .collect::<BTreeSet<_>>()
    {
        entities.push(symbol_entity_id(occurrence)?);
    }
    for (occurrence, binding) in &file.bindings {
        relations.push(file_symbol_relation_id(binding, occurrence)?);
    }
    for edge in &file.edges {
        relations.push(edge_relation_id(edge)?);
    }
    Ok((entities, relations))
}
