use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use rayon::prelude::*;

use crate::chunks::{
    CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1, published_symbol_spans,
    typescript_family_path,
};
use crate::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use crate::production::{
    CodeGraphFileBatchV1, CodeGraphResolutionV1, SealedGenerationFileWindowsV1,
    SealedGenerationSegmentReaderV1,
};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkV1, EdgeAuthorityV1,
    FileOccurrenceId, RelationEdgeKindV1, SanitizedCodeFileV1, SnapshotFileDispositionV1,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationRelation,
    GraphGenerationRowSpill, GraphLabel, GraphProjectionIdentity, GraphProjectorRevision,
    GraphPropertyName, GraphRelationId, GraphRelationKind, GraphWatermark, SpilledGraphGeneration,
};

use super::schema::{
    FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL, IMPORT_RECORD_PROPERTY,
    file_entity_id, file_import_relation_id_with, import_entity_id, record_property, serialize,
    stable_identity,
};
use super::{
    CodeGraphProjectionError, CodeGraphSymbolBindingV1, EDGE_LABEL, EDGE_RECORD_PROPERTY,
    FILE_SYMBOL_EDGE_KIND, SealedCodeGraphRowsError, SymbolRecordV1, TARGET_EDGE_KIND,
    code_graph_manifest_identity, compare_edges, current_generation_entity, projection,
    source_edge_kind, symbol_entity, symbol_entity_id, validate_edge,
};

/// Builds a sealed generation's code graph from its on-disk file segments and
/// spills the rows, never assembling the generation.
///
/// Two passes over the segments, one bounded window of files at a time:
/// 1. Resolution keeps only what cross-file resolution reads (symbols,
///    unresolved references, imports, and per-file edges) and derives the
///    cross-file edges, the bound symbol set, and the unresolved-call
///    limitations. Everything else a window decoded is dropped with it.
/// 2. Emission re-reads each window, emits its file, import, symbol, and
///    edge rows through the same emitter the whole-set build uses, and
///    pushes them to `spill`. The window's decoded segments are released
///    before the next is read.
///
/// The spill sorts and merges the rows on disk into the canonical order the
/// sealed store and the recovered digest require, so the result is
/// byte-identical to the manifest the whole generation would project.
#[hotpath::measure(label = "code_index.graph.build_rows")]
pub fn build_sealed_code_graph_rows(
    projection: GraphProjectionIdentity,
    source: &SealedGenerationFileWindowsV1,
    read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
    projector_revision: &GraphProjectorRevision,
    mut spill: GraphGenerationRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, SealedCodeGraphRowsError> {
    check()?;
    if projection.projection != self::projection()? {
        return Err(CodeGraphProjectionError::Contract(
            "code graph projection identity uses a foreign projector".to_owned(),
        )
        .into());
    }
    let generation = source.generation_id().clone();
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let resolution: CodeGraphResolutionV1 = hotpath::measure_block!(
        "code_index.graph.build_rows.resolve",
        source.resolve_code_graph(read_segment, check)
    )?;
    let unresolved_by_source = group_unresolved_calls(&resolution.unresolved_calls, check)?;
    let snapshot = source.snapshot();
    let files = snapshot
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, file))
        .collect::<BTreeMap<_, _>>();
    let context = CodeGraphRowContext {
        projection: &projection,
        generation: &generation,
        files: Some(&files),
        bound: &resolution.bound,
        unresolved_by_source: &unresolved_by_source,
    };
    hotpath::measure_block!("code_index.graph.build_rows.emit", {
        source.for_each_code_graph_batch(read_segment, &mut |batch: CodeGraphFileBatchV1<
            '_,
        >| {
            check()?;
            let rows = emit_code_graph_rows(
                &context,
                &CodeGraphRowBatch {
                    files: &batch.files,
                    imports: &batch.imports,
                    chunks: &batch.chunks,
                    symbols: &batch.symbols,
                    edges: &batch.edges,
                },
                check,
            )?;
            drop(batch);
            spill.push_batch(rows.entities, rows.relations, check)?;
            Ok::<(), SealedCodeGraphRowsError>(())
        })?;
        // The rows no window owns: snapshot files sealed without a segment
        // and the cross-file edges resolution derived.
        let unsegmented = snapshot
            .files
            .iter()
            .filter(|file| file.disposition != SnapshotFileDispositionV1::Present)
            .collect::<Vec<_>>();
        let rows = emit_code_graph_rows(
            &context,
            &CodeGraphRowBatch {
                files: &unsegmented,
                imports: &[],
                chunks: &[],
                symbols: &[],
                edges: &resolution.cross_file_edges,
            },
            check,
        )?;
        spill.push_batch(rows.entities, rows.relations, check)?;
        Ok::<(), SealedCodeGraphRowsError>(())
    })?;
    drop(resolution);
    // The generation marker counts every entity, itself included.
    let projection_node_count = spill.distinct_entities().checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    spill.push_batch(
        vec![current_generation_entity(
            &generation,
            projection_node_count,
        )?],
        Vec::new(),
        check,
    )?;
    let identity = code_graph_manifest_identity(projection, &generation, projector_revision)?;
    hotpath::measure_block!(
        "code_index.graph.build_rows.merge",
        spill.finish(identity, check)
    )
    .map_err(Into::into)
}

/// The unresolved receiver and import calls a graph discloses on their source
/// symbols, derived from every retained reference and edge of a generation.
///
/// A dotted Rust-style call stays a limitation unless the canonical resolver
/// bound its exact receiver site; TypeScript member calls are decided by the
/// module resolver and arrive in `typescript_unresolved`.
pub(crate) fn unresolved_call_limitations<'a>(
    references: &[(&str, &'a CodeIndexUnresolvedReferenceV1)],
    edges: impl Iterator<Item = &'a CanonicalRelationEdgeV1>,
    typescript_unresolved: Vec<CodeIndexUnresolvedReferenceV1>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, CodeGraphProjectionError> {
    let mut site_candidates = BTreeMap::new();
    for &(_, reference) in references {
        check()?;
        reference
            .validate()
            .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))?;
        if reference.kind == RelationEdgeKindV1::Calls && !reference.reference_name.contains('.') {
            site_candidates
                .entry((&reference.from_occurrence, reference.evidence_span))
                .and_modify(|candidate| *candidate = None)
                .or_insert(Some(reference.reference_name.as_str()));
        }
    }
    let mut resolved_sites = BTreeMap::new();
    for edge in edges {
        check()?;
        if edge.kind == RelationEdgeKindV1::Calls && edge.authority == EdgeAuthorityV1::NameResolved
        {
            let site = (&edge.from_occurrence, edge.evidence_span);
            // NameResolved is emitted only by the canonical retained-reference
            // resolver. A unique qualified candidate ties that edge to this
            // exact receiver site; bare or competing candidates cannot do so.
            if let Some(Some(candidate)) = site_candidates.get(&site)
                && let Some((owner, member)) = candidate.rsplit_once("::")
                && !owner.is_empty()
                && !member.is_empty()
            {
                resolved_sites.insert(site, member);
            }
        }
    }
    let mut unresolved_calls = Vec::new();
    for &(logical_path, reference) in references {
        check()?;
        // TypeScript member calls are retained only through an imported
        // namespace; the module resolver decides which are gaps.
        if typescript_family_path(logical_path) {
            continue;
        }
        // An enclosing-symbol fallback is not exact call-site proof, even
        // when another relation carries the same broad source span.
        let resolved_method_token =
            reference
                .reference_name
                .rsplit('.')
                .next()
                .is_some_and(|member| {
                    reference.evidence_span.len() == member.len() as u64
                        && resolved_sites
                            .get(&(&reference.from_occurrence, reference.evidence_span))
                            == Some(&member)
                });
        if reference.kind == RelationEdgeKindV1::Calls
            && reference.reference_name.contains('.')
            && !resolved_method_token
        {
            unresolved_calls.push(reference.clone());
        }
    }
    // A TypeScript call whose import names project code the seal could not
    // bind is the same kind of disclosed gap as an unresolved Rust receiver.
    check()?;
    unresolved_calls.extend(typescript_unresolved);
    unresolved_calls.sort();
    unresolved_calls.dedup();
    Ok(unresolved_calls)
}

fn group_unresolved_calls<'a>(
    unresolved_calls: &'a [CodeIndexUnresolvedReferenceV1],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<BTreeMap<&'a SymbolOccurrenceId, Vec<CodeIndexUnresolvedReferenceV1>>, GraphDbError> {
    let mut by_source = BTreeMap::<_, Vec<_>>::new();
    for reference in unresolved_calls {
        check()?;
        by_source
            .entry(&reference.from_occurrence)
            .or_default()
            .push(reference.clone());
    }
    Ok(by_source)
}

pub(super) struct BuiltProjection {
    pub(super) watermark: GraphWatermark,
    pub(super) entities: Vec<GraphEntity>,
    pub(super) relations: Vec<GraphGenerationRelation>,
}

#[derive(Clone, Copy)]
pub(super) struct ProductionCodeGraphInputs<'a> {
    pub(super) files: &'a [SanitizedCodeFileV1],
    pub(super) symbols: &'a GenerationSymbolIndexV1,
    pub(super) imports: &'a [CodeIndexImportEvidenceV1],
    pub(super) unresolved_calls: &'a [CodeIndexUnresolvedReferenceV1],
}

fn collect_graph_rows_ordered<T, R>(
    items: &[T],
    operation: impl Fn(&T) -> Result<R, CodeGraphProjectionError> + Send + Sync,
) -> Result<Vec<R>, CodeGraphProjectionError>
where
    T: Sync,
    R: Send,
{
    crate::parallelism::install(|| {
        const ROWS_PER_WORK_UNIT: usize = 512;
        let run = |(chunk_index, chunk): (usize, &[T])| {
            crate::parallelism::with_background_cpu_permit(|| {
                catch_unwind(AssertUnwindSafe(|| {
                    chunk.iter().map(&operation).collect::<Result<Vec<_>, _>>()
                }))
                .unwrap_or_else(|payload| {
                    Err(CodeGraphProjectionError::Unavailable(
                        crate::parallelism::CodeIndexParallelismErrorV1::from_panic_payload(
                            chunk_index.saturating_mul(ROWS_PER_WORK_UNIT),
                            &*payload,
                        )
                        .to_string(),
                    ))
                })
            })
        };
        if items.len() < 2 || crate::parallelism::indexing_workers() < 2 {
            items.iter().map(operation).collect()
        } else {
            let chunks = items
                .par_chunks(ROWS_PER_WORK_UNIT)
                .enumerate()
                .map(&run)
                .collect::<Vec<_>>();
            let mut collected = Vec::with_capacity(items.len());
            for chunk in chunks {
                collected.extend(chunk?);
            }
            Ok(collected)
        }
    })
    .map_err(|error| CodeGraphProjectionError::Unavailable(error.to_string()))?
}

/// What the whole generation contributes to every batch's rows: the snapshot
/// files bindings and imports must belong to, the symbols some batch binds or
/// describes, and the unresolved calls each source symbol discloses.
struct CodeGraphRowContext<'a> {
    projection: &'a GraphProjectionIdentity,
    generation: &'a CodeGenerationId,
    /// `None` for a hermetic publish without a snapshot, whose chunks must
    /// name the serving generation instead.
    files: Option<&'a BTreeMap<&'a FileOccurrenceId, &'a SanitizedCodeFileV1>>,
    bound: &'a HashSet<SymbolOccurrenceId>,
    unresolved_by_source: &'a BTreeMap<&'a SymbolOccurrenceId, Vec<CodeIndexUnresolvedReferenceV1>>,
}

/// One batch of a generation's rows: the files it owns and the chunks,
/// symbols, imports, and edges those files produced.
struct CodeGraphRowBatch<'a> {
    files: &'a [&'a SanitizedCodeFileV1],
    imports: &'a [CodeIndexImportEvidenceV1],
    chunks: &'a [Arc<CodeSearchChunkV1>],
    symbols: &'a [Arc<LineageSymbolRecordV1>],
    edges: &'a [CanonicalRelationEdgeV1],
}

struct EmittedRows {
    entities: Vec<GraphEntity>,
    relations: Vec<GraphGenerationRelation>,
}

pub(super) fn build_projection(
    projection: &GraphProjectionIdentity,
    generation: &CodeGenerationId,
    edges: &[CanonicalRelationEdgeV1],
    chunks: &[Arc<CodeSearchChunkV1>],
    production: Option<ProductionCodeGraphInputs<'_>>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<BuiltProjection, CodeGraphProjectionError> {
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let unresolved_by_source = group_unresolved_calls(
        production.map_or(&[], |inputs| inputs.unresolved_calls),
        check,
    )?;
    let files = production.map(|inputs| {
        inputs
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>()
    });
    let symbols = production.map_or(&[][..], |inputs| inputs.symbols.symbols.as_slice());
    // The whole set is one batch, so every symbol it binds or describes is
    // bound for edge retention exactly as the batch itself sees it.
    let bound = chunks
        .iter()
        .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.clone())
        .chain(symbols.iter().map(|symbol| symbol.occurrence.clone()))
        .collect::<HashSet<_>>();
    let file_rows = files
        .as_ref()
        .map(|files| files.values().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    let context = CodeGraphRowContext {
        projection,
        generation,
        files: files.as_ref(),
        bound: &bound,
        unresolved_by_source: &unresolved_by_source,
    };
    let EmittedRows {
        mut entities,
        relations,
    } = emit_code_graph_rows(
        &context,
        &CodeGraphRowBatch {
            files: &file_rows,
            imports: production.map_or(&[], |inputs| inputs.imports),
            chunks,
            symbols,
            edges,
        },
        check,
    )?;
    let projection_node_count = entities.len().checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    entities.push(current_generation_entity(
        generation,
        projection_node_count,
    )?);
    Ok(BuiltProjection {
        watermark: GraphWatermark::new(stable_identity("watermark", generation.as_str()))?,
        entities,
        relations,
    })
}

/// Emits one batch's rows. Every row a generation projects belongs to exactly
/// one batch, except that an edge target no batch binds or describes is
/// emitted by each batch whose edges reach it, identically, so the union of
/// all batches, sorted and deduplicated, is the whole-set projection.
fn emit_code_graph_rows(
    context: &CodeGraphRowContext<'_>,
    batch: &CodeGraphRowBatch<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<EmittedRows, CodeGraphProjectionError> {
    let projection = context.projection;
    let (symbol_metadata, bindings, retained_edges, occurrences) =
        hotpath::measure_block!("code_index.seal.collect.bind", {
            let symbol_metadata = batch
                .symbols
                .iter()
                .map(|symbol| (symbol.occurrence.clone(), symbol))
                .collect::<BTreeMap<_, _>>();
            for import in batch.imports {
                check()?;
                import
                    .validate()
                    .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
                let file = context
                    .files
                    .and_then(|files| files.get(&import.file_occurrence_id))
                    .ok_or_else(|| {
                        CodeGraphProjectionError::Contract(
                            "code graph import refers to a file outside its immutable snapshot"
                                .to_owned(),
                        )
                    })?;
                if file.logical_path != import.logical_path {
                    return Err(CodeGraphProjectionError::Contract(
                        "code graph import logical path does not match its file occurrence"
                            .to_owned(),
                    ));
                }
            }
            let mut bindings = BTreeMap::<SymbolOccurrenceId, CodeGraphSymbolBindingV1>::new();
            let symbol_spans = published_symbol_spans(batch.chunks.iter().map(AsRef::as_ref));
            for chunk in batch.chunks {
                check()?;
                chunk
                    .validate()
                    .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
                // With production inputs, membership in the immutable snapshot is
                // the serving binding and file-page generation_id is extraction
                // provenance. Hermetic publishes without a snapshot still require
                // the chunk to name the serving generation.
                let logical_path = match context.files {
                    Some(files) => {
                        let Some(file) = files.get(&chunk.anchor.file_occurrence_id) else {
                            return Err(CodeGraphProjectionError::Contract(
                                "code graph chunk refers to a file outside its immutable snapshot"
                                    .to_owned(),
                            ));
                        };
                        Some(file.logical_path.clone())
                    }
                    None => {
                        if chunk.anchor.generation_id != *context.generation {
                            return Err(CodeGraphProjectionError::GenerationMismatch);
                        }
                        None
                    }
                };
                let Some(symbol) = chunk.anchor.symbol_occurrence_id.clone() else {
                    continue;
                };
                let candidate = CodeGraphSymbolBindingV1 {
                    file: chunk.anchor.file_occurrence_id.clone(),
                    logical_path,
                    source_span: symbol_spans.get(&symbol).copied(),
                    chunk: Some(chunk.id.clone()),
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                };
                match bindings.entry(symbol) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let current = entry.get_mut();
                        if current.file != candidate.file
                            || current.logical_path != candidate.logical_path
                            || current.language_descriptor_revision
                                != candidate.language_descriptor_revision
                        {
                            return Err(CodeGraphProjectionError::Contract(
                                "one symbol occurrence has conflicting graph candidate bindings"
                                    .to_owned(),
                            ));
                        }
                        if candidate.chunk < current.chunk {
                            current.chunk = candidate.chunk;
                        }
                    }
                }
            }

            let mut retained_edges = Vec::new();
            for edge in batch.edges {
                check()?;
                validate_edge(edge)?;
                if context.bound.contains(&edge.from_occurrence) {
                    retained_edges.push(edge.clone());
                }
            }
            retained_edges.sort_by(compare_edges);
            retained_edges.dedup();

            let mut occurrences = bindings
                .keys()
                .chain(symbol_metadata.keys())
                .cloned()
                .collect::<Vec<_>>();
            for edge in &retained_edges {
                if !context.bound.contains(&edge.to_occurrence) {
                    occurrences.push(edge.to_occurrence.clone());
                }
            }
            occurrences.sort();
            occurrences.dedup();
            Ok::<_, CodeGraphProjectionError>((
                symbol_metadata,
                bindings,
                retained_edges,
                occurrences,
            ))
        })?;
    // Chunks bind symbols to files and spans above; they are not graph rows.
    // No reader addresses a chunk through the graph, traversal alternates
    // symbol and edge-evidence entities, and a symbol's binding already
    // names its chunk, so projecting one entity plus one relation per chunk
    // only multiplied every graph artifact by the chunk count.
    hotpath::measure_block!("code_index.seal.collect.emit", {
        let mut entities = Vec::with_capacity(
            batch
                .files
                .len()
                .saturating_add(batch.imports.len())
                .saturating_add(occurrences.len())
                .saturating_add(retained_edges.len()),
        );
        let mut relations = Vec::with_capacity(
            retained_edges
                .len()
                .saturating_mul(2)
                .saturating_add(bindings.len())
                .saturating_add(batch.imports.len()),
        );

        for file in batch.files {
            check()?;
            entities.push(file_entity(
                file_entity_id(&file.file_occurrence_id)?,
                file,
            )?);
        }
        for import in batch.imports {
            check()?;
            let identity = import_entity_id(import)?;
            let file_id = file_entity_id(&import.file_occurrence_id)?;
            relations.push(file_import_relation(
                projection, import, file_id, &identity,
            )?);
            entities.push(import_entity(identity, import)?);
        }

        // Every stable identity below is a serialize-and-hash; each symbol's
        // is computed once and reused by its entity and every relation that
        // names it. An edge target another batch owns is derived on use.
        let mut symbol_ids = BTreeMap::<SymbolOccurrenceId, GraphEntityId>::new();
        let row_window = crate::parallelism::indexing_workers()
            .max(1)
            .saturating_mul(512);
        hotpath::gauge!("code_index.seal.collect.emit.effective_workers")
            .set(crate::parallelism::indexing_workers());
        for window in occurrences.chunks(row_window) {
            check()?;
            let identities = collect_graph_rows_ordered(window, symbol_entity_id)?;
            symbol_ids.extend(window.iter().cloned().zip(identities));
        }
        for window in occurrences.chunks(row_window) {
            check()?;
            entities.extend(collect_graph_rows_ordered(window, |occurrence| {
                let identity = require_symbol_id(&symbol_ids, occurrence)?.clone();
                let record = SymbolRecordV1 {
                    binding: bindings.get(occurrence).cloned(),
                    metadata: symbol_metadata
                        .get(occurrence)
                        .map(|record| LineageSymbolRecordV1::clone(record)),
                    occurrence: occurrence.clone(),
                    unresolved_calls: context
                        .unresolved_by_source
                        .get(occurrence)
                        .cloned()
                        .unwrap_or_default(),
                };
                symbol_entity(identity, record)
            })?);
        }
        if context.files.is_some() {
            let binding_rows = bindings.iter().collect::<Vec<_>>();
            for window in binding_rows.chunks(row_window) {
                check()?;
                relations.extend(collect_graph_rows_ordered(
                    window,
                    |&(occurrence, binding)| {
                        let file_id = file_entity_id(&binding.file)?;
                        let symbol_id = require_symbol_id(&symbol_ids, occurrence)?;
                        file_symbol_relation(projection, binding, file_id, occurrence, symbol_id)
                    },
                )?);
            }
        }
        for window in retained_edges.chunks(row_window) {
            check()?;
            for (entity, source, target) in collect_graph_rows_ordered(window, |edge| {
                edge_artifacts(projection, edge, &symbol_ids)
            })? {
                entities.push(entity);
                relations.push(source);
                relations.push(target);
            }
        }
        Ok(EmittedRows {
            entities,
            relations,
        })
    })
}

fn require_symbol_id<'ids>(
    symbol_ids: &'ids BTreeMap<SymbolOccurrenceId, GraphEntityId>,
    occurrence: &SymbolOccurrenceId,
) -> Result<&'ids GraphEntityId, CodeGraphProjectionError> {
    symbol_ids.get(occurrence).ok_or_else(|| {
        CodeGraphProjectionError::Contract(
            "code graph relation names a symbol occurrence with no entity".to_owned(),
        )
    })
}

/// A batch's own symbol identity, or the derived identity of an edge target
/// another batch emits.
fn endpoint_symbol_id(
    symbol_ids: &BTreeMap<SymbolOccurrenceId, GraphEntityId>,
    occurrence: &SymbolOccurrenceId,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    match symbol_ids.get(occurrence) {
        Some(identity) => Ok(identity.clone()),
        None => symbol_entity_id(occurrence),
    }
}

/// One retained edge's entity plus both endpoint relations, sharing a single
/// serialization and identity derivation of the edge payload.
fn edge_artifacts(
    projection: &GraphProjectionIdentity,
    edge: &CanonicalRelationEdgeV1,
    symbol_ids: &BTreeMap<SymbolOccurrenceId, GraphEntityId>,
) -> Result<
    (
        GraphEntity,
        GraphGenerationRelation,
        GraphGenerationRelation,
    ),
    CodeGraphProjectionError,
> {
    let payload = serialize(edge)?;
    let identity = GraphEntityId::new(stable_identity("edge", &hex::encode(&payload)))?;
    let entity = GraphEntity::new(
        identity.clone(),
        BTreeSet::from([GraphLabel::new(EDGE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(EDGE_RECORD_PROPERTY)?,
            record_property(payload)?,
        )]),
    )?;
    let from = endpoint_symbol_id(symbol_ids, &edge.from_occurrence)?;
    let to = endpoint_symbol_id(symbol_ids, &edge.to_occurrence)?;
    let source = GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity("source", identity.as_str()))?,
        GraphEntityRef::new(projection.clone(), from),
        GraphEntityRef::new(projection.clone(), identity.clone()),
        GraphRelationKind::new(source_edge_kind(edge.kind))?,
        BTreeMap::new(),
    )?;
    let target = GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity("target", identity.as_str()))?,
        GraphEntityRef::new(projection.clone(), identity),
        GraphEntityRef::new(projection.clone(), to),
        GraphRelationKind::new(TARGET_EDGE_KIND)?,
        BTreeMap::new(),
    )?;
    Ok((entity, source, target))
}

fn file_entity(
    identity: GraphEntityId,
    file: &SanitizedCodeFileV1,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    file.validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    GraphEntity::new(
        identity,
        BTreeSet::from([GraphLabel::new(FILE_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(FILE_RECORD_PROPERTY)?,
            record_property(serialize(file)?)?,
        )]),
    )
    .map_err(Into::into)
}

fn import_entity(
    identity: GraphEntityId,
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphEntity, CodeGraphProjectionError> {
    GraphEntity::new(
        identity,
        BTreeSet::from([GraphLabel::new(IMPORT_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(IMPORT_RECORD_PROPERTY)?,
            record_property(serialize(import)?)?,
        )]),
    )
    .map_err(Into::into)
}

fn file_symbol_relation(
    projection: &GraphProjectionIdentity,
    binding: &CodeGraphSymbolBindingV1,
    file_id: GraphEntityId,
    occurrence: &SymbolOccurrenceId,
    symbol_id: &GraphEntityId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "file-symbol",
            &format!("{}\0{}", binding.file.as_str(), occurrence.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), file_id),
        GraphEntityRef::new(projection.clone(), symbol_id.clone()),
        GraphRelationKind::new(FILE_SYMBOL_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn file_import_relation(
    projection: &GraphProjectionIdentity,
    import: &CodeIndexImportEvidenceV1,
    file_id: GraphEntityId,
    import_id: &GraphEntityId,
) -> Result<GraphGenerationRelation, CodeGraphProjectionError> {
    GraphGenerationRelation::new(
        file_import_relation_id_with(import, import_id)?,
        GraphEntityRef::new(projection.clone(), file_id),
        GraphEntityRef::new(projection.clone(), import_id.clone()),
        GraphRelationKind::new(FILE_IMPORT_EDGE_KIND)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

pub(super) fn validate_symbol_metadata(
    metadata: &LineageSymbolRecordV1,
    occurrence: &SymbolOccurrenceId,
) -> Result<(), CodeGraphProjectionError> {
    if metadata.occurrence != *occurrence {
        return Err(CodeGraphProjectionError::Contract(
            "code graph symbol metadata names a different occurrence".to_owned(),
        ));
    }
    metadata
        .identity
        .validate()
        .and_then(|()| metadata.file_identity.validate())
        .and_then(|()| metadata.content_digest.validate())
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    if metadata.qualified_name.is_empty() || metadata.kind.is_empty() {
        return Err(CodeGraphProjectionError::Contract(
            "code graph symbol metadata is incomplete".to_owned(),
        ));
    }
    Ok(())
}
