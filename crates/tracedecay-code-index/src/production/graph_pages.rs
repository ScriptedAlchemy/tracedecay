//! Per-file graph projection outputs sealed after canonical resolution.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, FileOccurrenceId, ManifestDigest,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::{GraphDbError, GraphEntityId, GraphNamespace, GraphSpillRowFootprint};

use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::graph_projection::{
    CodeGraphSymbolBindingV1, code_graph_projection_identity, code_graph_symbol_bindings,
    emit_persisted_code_graph_page,
};
use crate::lineage::LineageSymbolRecordV1;

use super::graph_page_store::CodeGraphPageBuildFootprintV1;
use super::resolution_view::ResolutionMemoryChangeV1;
use super::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedCodeGraphPageV1 {
    pub(crate) file: SanitizedCodeFileV1,
    pub(crate) imports: Vec<CodeIndexImportEvidenceV1>,
    pub(crate) symbols: Vec<Arc<LineageSymbolRecordV1>>,
    pub(crate) edges: Vec<CanonicalRelationEdgeV1>,
    pub(crate) bindings: BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
    pub(crate) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    /// The page that owns each edge target reached outside this page.
    pub(crate) target_files: BTreeMap<SymbolOccurrenceId, FileOccurrenceId>,
    /// Edge targets no file binds or describes.
    pub(crate) placeholder_targets: BTreeSet<SymbolOccurrenceId>,
    /// Unbound edge targets whose placeholder row this page owns.
    pub(crate) owned_placeholders: BTreeSet<SymbolOccurrenceId>,
}

#[derive(Clone, Debug)]
pub(super) struct SealedCodeGraphPageV1 {
    pub(super) file_key: u32,
    pub(super) file_occurrence_id: FileOccurrenceId,
    pub(super) logical_path: String,
    pub(super) page_digest: ManifestDigest,
    pub(super) encoded: Vec<u8>,
    pub(super) build_footprint: CodeGraphPageBuildFootprintV1,
}

fn page_build_footprint(
    page: &PersistedCodeGraphPageV1,
    generation: &tracedecay_domain::CodeGenerationId,
    encoded_bytes: usize,
) -> Result<CodeGraphPageBuildFootprintV1, CodeIndexProductionErrorV1> {
    let sizing_projection = code_graph_projection_identity(
        GraphNamespace::new("sizing")
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
    )
    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let rows = emit_persisted_code_graph_page(&sizing_projection, generation, page, &|| Ok(()))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let mut entity_spill = GraphSpillRowFootprint::default();
    let mut relation_spill = GraphSpillRowFootprint::default();
    let mut max_entity_spill = GraphSpillRowFootprint::default();
    let mut identity_bytes = 0_usize;
    for entity in &rows.entities {
        let footprint = GraphSpillRowFootprint::of_entity(entity)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        entity_spill.buffered = entity_spill.buffered.saturating_add(footprint.buffered);
        entity_spill.resident = entity_spill.resident.saturating_add(footprint.resident);
        if footprint.resident > max_entity_spill.resident {
            max_entity_spill = footprint;
        }
        identity_bytes = identity_bytes
            .saturating_add(size_of::<GraphEntityId>().saturating_mul(2))
            .saturating_add(entity.identity.as_str().len());
    }
    for relation in &rows.relations {
        let footprint = GraphSpillRowFootprint::of_relation(relation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        relation_spill.buffered = relation_spill.buffered.saturating_add(footprint.buffered);
        relation_spill.resident = relation_spill.resident.saturating_add(footprint.resident);
    }
    let own_entities = rows
        .entities
        .iter()
        .map(|entity| &entity.identity)
        .collect::<HashSet<_>>();
    let external_endpoints = rows
        .relations
        .iter()
        .flat_map(|relation| [&relation.from.identity, &relation.to.identity])
        .filter(|endpoint| !own_entities.contains(endpoint))
        .collect::<HashSet<_>>();
    let encoded_bytes = u64::try_from(encoded_bytes).unwrap_or(u64::MAX);
    let number = |value: usize| u64::try_from(value).unwrap_or(u64::MAX);
    Ok(CodeGraphPageBuildFootprintV1 {
        // The encoded page, its decoded allocations, and emitted rows cannot
        // overlap by more than this conservative duplicate of their owned
        // payload bytes.
        decode_bytes: encoded_bytes
            .saturating_mul(2)
            .saturating_add(number(entity_spill.resident))
            .saturating_add(number(relation_spill.resident)),
        entity_spill_buffered: number(entity_spill.buffered),
        entity_spill_resident: number(entity_spill.resident),
        relation_spill_buffered: number(relation_spill.buffered),
        relation_spill_resident: number(relation_spill.resident),
        identity_bytes: number(identity_bytes),
        entity_count: number(rows.entities.len()),
        relation_count: number(rows.relations.len()),
        external_endpoint_count: number(external_endpoints.len()),
        max_entity_spill_buffered: number(max_entity_spill.buffered),
        max_entity_spill_resident: number(max_entity_spill.resident),
    })
}

impl CodeIndexPublishedGenerationV1 {
    pub(super) fn for_each_sealed_code_graph_page(
        &self,
        mut visit: impl FnMut(SealedCodeGraphPageV1) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let snapshot_files = self
            .snapshot
            .files
            .iter()
            .enumerate()
            .map(|(key, file)| {
                u32::try_from(key)
                    .map(|key| (file.file_occurrence_id.clone(), (key, file)))
                    .map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "code graph page file key exceeds u32".to_owned(),
                        )
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let artifacts_by_occurrence = self
            .files
            .iter()
            .map(|file| (&file.extraction.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();

        let mut owners = BTreeMap::<&SymbolOccurrenceId, &FileOccurrenceId>::new();
        for file in &self.files {
            let occurrence = &file.extraction.file_occurrence_id;
            snapshot_files.get(occurrence).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "code graph page file is outside its snapshot".to_owned(),
                )
            })?;
            for owned in file
                .artifacts
                .chunks
                .chunks
                .iter()
                .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
                .chain(
                    file.artifacts
                        .symbols
                        .iter()
                        .map(|symbol| &symbol.occurrence),
                )
            {
                match owners.insert(owned, occurrence) {
                    None => {}
                    Some(existing) if existing == occurrence => {}
                    Some(_) => {
                        return Err(CodeIndexProductionErrorV1::Contract(
                            "one graph symbol occurrence belongs to multiple files".to_owned(),
                        ));
                    }
                }
            }
        }

        for_each_code_graph_page(
            &self.snapshot,
            &self.manifest.generation_id,
            &owners,
            &self.edges,
            &self.unresolved_calls,
            &mut |_| Ok(()),
            &mut |file| {
                Ok(artifacts_by_occurrence
                    .get(&file.file_occurrence_id)
                    .map(|artifacts| Arc::clone(artifacts)))
            },
            &mut visit,
        )?;
        Ok(())
    }
}

/// The symbol occurrences a file's graph page owns: its chunks' symbols and
/// its symbols.
pub(super) fn owned_occurrences(file: &FileGenerationArtifactsV1) -> BTreeSet<&SymbolOccurrenceId> {
    let occurrences = file
        .artifacts
        .chunks
        .chunks
        .iter()
        .filter_map(|chunk| chunk.anchor.symbol_occurrence_id.as_ref())
        .chain(
            file.artifacts
                .symbols
                .iter()
                .map(|symbol| &symbol.occurrence),
        );
    let mut owners = BTreeSet::new();
    for occurrence in occurrences {
        owners.insert(occurrence);
    }
    owners
}

/// Everything one file's graph page holds besides what its file says.
pub(super) struct CodeGraphPageInputsV1<'a> {
    pub(super) file_key: u32,
    pub(super) snapshot_file: &'a SanitizedCodeFileV1,
    pub(super) artifacts: Option<&'a FileGenerationArtifactsV1>,
    /// The file's own and cross-file edges in canonical edge order.
    pub(super) edges: Vec<CanonicalRelationEdgeV1>,
    /// The file's call limitations in sorted order.
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    pub(super) target_files: BTreeMap<SymbolOccurrenceId, FileOccurrenceId>,
    pub(super) placeholder_targets: BTreeSet<SymbolOccurrenceId>,
    pub(super) owned_placeholders: BTreeSet<SymbolOccurrenceId>,
}

/// Encode and content-address one file's graph page.
pub(super) fn seal_code_graph_page(
    inputs: CodeGraphPageInputsV1<'_>,
    files_by_occurrence: &BTreeMap<&FileOccurrenceId, &SanitizedCodeFileV1>,
    generation: &tracedecay_domain::CodeGenerationId,
) -> Result<SealedCodeGraphPageV1, CodeIndexProductionErrorV1> {
    seal_code_graph_page_charged(inputs, files_by_occurrence, generation, 0, &mut |_| Ok(()))
}

fn seal_code_graph_page_charged(
    inputs: CodeGraphPageInputsV1<'_>,
    files_by_occurrence: &BTreeMap<&FileOccurrenceId, &SanitizedCodeFileV1>,
    generation: &CodeGenerationId,
    base: usize,
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<SealedCodeGraphPageV1, CodeIndexProductionErrorV1> {
    let check = || Ok::<(), GraphDbError>(());
    let CodeGraphPageInputsV1 {
        file_key,
        snapshot_file,
        artifacts,
        edges,
        unresolved_calls,
        target_files,
        placeholder_targets,
        owned_placeholders,
    } = inputs;
    let bindings = artifacts
        .map(|file| {
            code_graph_symbol_bindings(
                Some(files_by_occurrence),
                generation,
                &file.artifacts.chunks.chunks,
                &check,
            )
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
        })
        .transpose()?
        .unwrap_or_default();
    let page = PersistedCodeGraphPageV1 {
        file: snapshot_file.clone(),
        imports: artifacts.map_or_default(|file| file.artifacts.imports.clone()),
        symbols: artifacts.map_or_default(|file| file.artifacts.symbols.clone()),
        edges,
        bindings,
        unresolved_calls,
        target_files,
        placeholder_targets,
        owned_placeholders,
    };
    let encoded = super::partitioned_codec::encode_json_rows_charged(&page, base, retain)?;
    let page_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&encoded))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let build_footprint = page_build_footprint(&page, generation, encoded.len())?;
    Ok(SealedCodeGraphPageV1 {
        file_key,
        file_occurrence_id: snapshot_file.file_occurrence_id.clone(),
        logical_path: snapshot_file.logical_path.clone(),
        page_digest,
        encoded,
        build_footprint,
    })
}

/// Emit the same canonical graph pages from either decoded files or a staged
/// source. The loader's one file stays alive only until its page is sealed.
#[allow(clippy::too_many_arguments)]
pub(super) fn for_each_code_graph_page(
    snapshot: &SanitizedCodeSnapshotV1,
    generation: &CodeGenerationId,
    owners: &BTreeMap<&SymbolOccurrenceId, &FileOccurrenceId>,
    all_edges: &[CanonicalRelationEdgeV1],
    unresolved_calls: &[CodeIndexUnresolvedReferenceV1],
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    load: &mut impl FnMut(
        &SanitizedCodeFileV1,
    )
        -> Result<Option<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1>,
    visit: &mut impl FnMut(SealedCodeGraphPageV1) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let mut routing = super::resident_bytes::btree_bytes(
        snapshot.files.len(),
        size_of::<(&FileOccurrenceId, &SanitizedCodeFileV1)>(),
    );
    retain(u64::try_from(routing).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("graph routing charge exceeds u64".to_owned())
    })?)?;
    let mut charge = |bytes: ResolutionMemoryChangeV1| {
        bytes.apply_with(&mut routing, |routing| {
            retain(u64::try_from(routing).map_err(|_| {
                CodeIndexProductionErrorV1::Contract("graph routing charge exceeds u64".to_owned())
            })?)
        })
    };
    let files_by_occurrence = snapshot
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, file))
        .collect::<BTreeMap<_, _>>();
    let mut edges = BTreeMap::<&FileOccurrenceId, Vec<&CanonicalRelationEdgeV1>>::new();
    for edge in all_edges {
        if let Some(owner) = owners.get(&edge.from_occurrence) {
            if !edges.contains_key(*owner) {
                let before = super::resident_bytes::btree_bytes(
                    edges.len(),
                    size_of::<(&FileOccurrenceId, Vec<&CanonicalRelationEdgeV1>)>(),
                );
                let after = super::resident_bytes::btree_bytes(
                    edges.len().saturating_add(1),
                    size_of::<(&FileOccurrenceId, Vec<&CanonicalRelationEdgeV1>)>(),
                );
                charge(ResolutionMemoryChangeV1::Retain(
                    after.saturating_sub(before),
                ))?;
            }
            let rows = edges.entry(*owner).or_default();
            super::resolution_view::reserve_resolution_vec(rows, 1, &mut charge)?;
            rows.push(edge);
        }
    }

    let mut unresolved = BTreeMap::<&FileOccurrenceId, Vec<&CodeIndexUnresolvedReferenceV1>>::new();
    for reference in unresolved_calls {
        if let Some(owner) = owners.get(&reference.from_occurrence) {
            if !unresolved.contains_key(*owner) {
                let before = super::resident_bytes::btree_bytes(
                    unresolved.len(),
                    size_of::<(&FileOccurrenceId, Vec<&CodeIndexUnresolvedReferenceV1>)>(),
                );
                let after = super::resident_bytes::btree_bytes(
                    unresolved.len().saturating_add(1),
                    size_of::<(&FileOccurrenceId, Vec<&CodeIndexUnresolvedReferenceV1>)>(),
                );
                charge(ResolutionMemoryChangeV1::Retain(
                    after.saturating_sub(before),
                ))?;
            }
            let rows = unresolved.entry(*owner).or_default();
            super::resolution_view::reserve_resolution_vec(rows, 1, &mut charge)?;
            rows.push(reference);
        }
    }

    let mut placeholder_owners = BTreeMap::<&SymbolOccurrenceId, &FileOccurrenceId>::new();
    for (file_occurrence_id, page_edges) in &edges {
        for edge in page_edges {
            if !owners.contains_key(&edge.to_occurrence) {
                if !placeholder_owners.contains_key(&edge.to_occurrence) {
                    let before = super::resident_bytes::btree_bytes(
                        placeholder_owners.len(),
                        size_of::<(&SymbolOccurrenceId, &FileOccurrenceId)>(),
                    );
                    let after = super::resident_bytes::btree_bytes(
                        placeholder_owners.len().saturating_add(1),
                        size_of::<(&SymbolOccurrenceId, &FileOccurrenceId)>(),
                    );
                    charge(ResolutionMemoryChangeV1::Retain(
                        after.saturating_sub(before),
                    ))?;
                }
                placeholder_owners
                    .entry(&edge.to_occurrence)
                    .and_modify(|owner| {
                        if *file_occurrence_id < *owner {
                            *owner = *file_occurrence_id;
                        }
                    })
                    .or_insert(*file_occurrence_id);
            }
        }
    }

    for (position, snapshot_file) in snapshot.files.iter().enumerate() {
        let file_key = u32::try_from(position).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("code graph page file key exceeds u32".to_owned())
        })?;
        let occurrence = &snapshot_file.file_occurrence_id;
        let page_edges = edges.remove(occurrence).unwrap_or_default();
        let page_calls = unresolved.remove(occurrence).unwrap_or_default();
        let mut page_bytes = routing
            .saturating_add(super::resident_bytes::vec_bytes(
                &page_edges,
                page_edges.len(),
                |edge| super::resident_bytes::edge_heap_bytes(edge),
            ))
            .saturating_add(page_edges.len().saturating_mul(
                size_of::<CanonicalRelationEdgeV1>() - size_of::<&CanonicalRelationEdgeV1>(),
            ))
            .saturating_add(super::resident_bytes::vec_bytes(
                &page_calls,
                page_calls.len(),
                |call| super::resident_bytes::unresolved_heap_bytes(call),
            ))
            .saturating_add(page_calls.len().saturating_mul(
                size_of::<CodeIndexUnresolvedReferenceV1>()
                    - size_of::<&CodeIndexUnresolvedReferenceV1>(),
            ));
        let mut charge = |bytes: ResolutionMemoryChangeV1| {
            bytes.apply_with(&mut page_bytes, |page_bytes| {
                retain(u64::try_from(page_bytes).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract("graph page charge exceeds u64".to_owned())
                })?)
            })
        };
        let mut target_files = BTreeMap::new();
        let mut placeholder_targets = BTreeSet::new();
        for edge in &page_edges {
            if let Some(target) = owners
                .get(&edge.to_occurrence)
                .or_else(|| placeholder_owners.get(&edge.to_occurrence))
                && *target != occurrence
                && !target_files.contains_key(&edge.to_occurrence)
            {
                let before = super::resident_bytes::btree_bytes(
                    target_files.len(),
                    size_of::<(SymbolOccurrenceId, FileOccurrenceId)>(),
                );
                let after = super::resident_bytes::btree_bytes(
                    target_files.len().saturating_add(1),
                    size_of::<(SymbolOccurrenceId, FileOccurrenceId)>(),
                );
                charge(ResolutionMemoryChangeV1::Retain(
                    after
                        .saturating_sub(before)
                        .saturating_add(edge.to_occurrence.as_str().len())
                        .saturating_add(target.as_str().len()),
                ))?;
                target_files.insert(edge.to_occurrence.clone(), (*target).clone());
            }
            if !owners.contains_key(&edge.to_occurrence)
                && !placeholder_targets.contains(&edge.to_occurrence)
            {
                let before = super::resident_bytes::btree_bytes(
                    placeholder_targets.len(),
                    size_of::<SymbolOccurrenceId>(),
                );
                let after = super::resident_bytes::btree_bytes(
                    placeholder_targets.len().saturating_add(1),
                    size_of::<SymbolOccurrenceId>(),
                );
                charge(ResolutionMemoryChangeV1::Retain(
                    after
                        .saturating_sub(before)
                        .saturating_add(edge.to_occurrence.as_str().len()),
                ))?;
                placeholder_targets.insert(edge.to_occurrence.clone());
            }
        }
        let mut owned_placeholders = BTreeSet::new();
        for (placeholder, owner) in &placeholder_owners {
            if *owner != occurrence {
                continue;
            }
            let before = super::resident_bytes::btree_bytes(
                owned_placeholders.len(),
                size_of::<SymbolOccurrenceId>(),
            );
            let after = super::resident_bytes::btree_bytes(
                owned_placeholders.len().saturating_add(1),
                size_of::<SymbolOccurrenceId>(),
            );
            charge(ResolutionMemoryChangeV1::Retain(
                after
                    .saturating_sub(before)
                    .saturating_add(placeholder.as_str().len()),
            ))?;
            owned_placeholders.insert((*placeholder).clone());
        }
        let artifacts = load(snapshot_file)?;
        if let Some(file) = &artifacts {
            page_bytes = page_bytes
                .saturating_add(super::resident_bytes::file_bytes(file))
                .saturating_add(
                    file.artifacts
                        .chunks
                        .chunks
                        .iter()
                        .map(|chunk| super::resident_bytes::chunk_bytes(chunk))
                        .sum::<usize>(),
                )
                .saturating_add(
                    file.artifacts
                        .symbols
                        .iter()
                        .map(|symbol| super::resident_bytes::symbol_bytes(symbol))
                        .sum::<usize>(),
                )
                .saturating_add(super::resident_bytes::vec_bytes(
                    &file.artifacts.imports,
                    file.artifacts.imports.len(),
                    super::resident_bytes::import_heap_bytes,
                ))
                .saturating_add(
                    file.artifacts
                        .symbols
                        .len()
                        .saturating_mul(size_of::<Arc<LineageSymbolRecordV1>>()),
                );
            page_bytes = page_bytes.saturating_add(super::resident_bytes::btree_bytes(
                file.artifacts.chunks.chunks.len(),
                size_of::<(SymbolOccurrenceId, CodeGraphSymbolBindingV1)>(),
            ));
            for chunk in &file.artifacts.chunks.chunks {
                page_bytes = page_bytes
                    .saturating_add(
                        chunk
                            .anchor
                            .symbol_occurrence_id
                            .as_ref()
                            .map_or(0, |id| id.as_str().len()),
                    )
                    .saturating_add(chunk.anchor.file_occurrence_id.as_str().len())
                    .saturating_add(chunk.id.as_str().len())
                    .saturating_add(chunk.language_descriptor_revision.as_str().len())
                    .saturating_add(snapshot_file.logical_path.len());
            }
        }
        page_bytes = page_bytes
            .saturating_add(size_of::<SanitizedCodeFileV1>())
            .saturating_add(super::resident_bytes::snapshot_file_heap_bytes(
                snapshot_file,
            ));
        retain(u64::try_from(page_bytes).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("graph page charge exceeds u64".to_owned())
        })?)?;
        visit(seal_code_graph_page_charged(
            CodeGraphPageInputsV1 {
                file_key,
                snapshot_file,
                artifacts: artifacts.as_deref(),
                edges: page_edges.into_iter().cloned().collect(),
                unresolved_calls: page_calls.into_iter().cloned().collect(),
                target_files,
                placeholder_targets,
                owned_placeholders,
            },
            &files_by_occurrence,
            generation,
            page_bytes,
            retain,
        )?)?;
    }
    Ok(())
}
