//! Per-file graph projection outputs sealed after canonical resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, ManifestDigest, SanitizedCodeFileV1,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::{GraphDbError, GraphEntityId, GraphNamespace, GraphSpillRowFootprint};

use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::graph_projection::{
    CodeGraphSymbolBindingV1, code_graph_projection_identity, code_graph_symbol_bindings,
    emit_persisted_code_graph_page,
};
use crate::lineage::LineageSymbolRecordV1;

use super::graph_page_store::CodeGraphPageBuildFootprintV1;
use super::{CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1};

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
        let files_by_occurrence = snapshot_files
            .iter()
            .map(|(occurrence, (_, file))| (occurrence, *file))
            .collect::<BTreeMap<_, _>>();
        let artifacts_by_occurrence = self
            .files
            .iter()
            .map(|file| (&file.extraction.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();

        let check = || Ok::<(), GraphDbError>(());
        let mut owners = BTreeMap::<SymbolOccurrenceId, FileOccurrenceId>::new();
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
                match owners.insert(owned.clone(), occurrence.clone()) {
                    None => {}
                    Some(existing) if existing == *occurrence => {}
                    Some(_) => {
                        return Err(CodeIndexProductionErrorV1::Contract(
                            "one graph symbol occurrence belongs to multiple files".to_owned(),
                        ));
                    }
                }
            }
        }

        let mut edges = BTreeMap::<FileOccurrenceId, Vec<&CanonicalRelationEdgeV1>>::new();
        for edge in &self.edges {
            if let Some(owner) = owners.get(&edge.from_occurrence) {
                edges.entry(owner.clone()).or_default().push(edge);
            }
        }

        let mut unresolved =
            BTreeMap::<FileOccurrenceId, Vec<&CodeIndexUnresolvedReferenceV1>>::new();
        for reference in &self.unresolved_calls {
            if let Some(owner) = owners.get(&reference.from_occurrence) {
                unresolved.entry(owner.clone()).or_default().push(reference);
            }
        }

        let mut placeholder_owners = BTreeMap::<SymbolOccurrenceId, FileOccurrenceId>::new();
        for (file_occurrence_id, page_edges) in &edges {
            for edge in page_edges {
                if !owners.contains_key(&edge.to_occurrence) {
                    placeholder_owners
                        .entry(edge.to_occurrence.clone())
                        .and_modify(|owner| {
                            if file_occurrence_id < owner {
                                *owner = file_occurrence_id.clone();
                            }
                        })
                        .or_insert_with(|| file_occurrence_id.clone());
                }
            }
        }

        for (position, snapshot_file) in self.snapshot.files.iter().enumerate() {
            let file_key = u32::try_from(position).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "code graph page file key exceeds u32".to_owned(),
                )
            })?;
            let occurrence = &snapshot_file.file_occurrence_id;
            let page_edges = edges.remove(occurrence).unwrap_or_default();
            let target_files = page_edges
                .iter()
                .filter_map(|edge| {
                    owners
                        .get(&edge.to_occurrence)
                        .or_else(|| placeholder_owners.get(&edge.to_occurrence))
                        .filter(|target| *target != occurrence)
                        .map(|target| (edge.to_occurrence.clone(), target.clone()))
                })
                .collect();
            let owned_placeholders = placeholder_owners
                .iter()
                .filter(|(_, owner)| *owner == occurrence)
                .map(|(occurrence, _)| occurrence.clone())
                .collect();
            let placeholder_targets = page_edges
                .iter()
                .filter(|edge| !owners.contains_key(&edge.to_occurrence))
                .map(|edge| edge.to_occurrence.clone())
                .collect();
            let artifacts = artifacts_by_occurrence.get(occurrence).copied();
            let bindings = artifacts
                .map(|file| {
                    code_graph_symbol_bindings(
                        Some(&files_by_occurrence),
                        &self.manifest.generation_id,
                        &file.artifacts.chunks.chunks,
                        &check,
                    )
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
                })
                .transpose()?
                .unwrap_or_default();
            let page = PersistedCodeGraphPageV1 {
                file: snapshot_file.clone(),
                imports: artifacts
                    .map(|file| file.artifacts.imports.clone())
                    .unwrap_or_default(),
                symbols: artifacts
                    .map(|file| file.artifacts.symbols.clone())
                    .unwrap_or_default(),
                edges: page_edges.into_iter().cloned().collect(),
                bindings,
                unresolved_calls: unresolved
                    .remove(occurrence)
                    .unwrap_or_default()
                    .into_iter()
                    .cloned()
                    .collect(),
                target_files,
                placeholder_targets,
                owned_placeholders,
            };
            let encoded = serde_json::to_vec(&page).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "code graph page encoding failed: {error}"
                ))
            })?;
            let page_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&encoded))
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            let build_footprint =
                page_build_footprint(&page, &self.manifest.generation_id, encoded.len())?;
            visit(SealedCodeGraphPageV1 {
                file_key,
                file_occurrence_id: occurrence.clone(),
                logical_path: snapshot_file.logical_path.clone(),
                page_digest,
                encoded,
                build_footprint,
            })?;
        }
        Ok(())
    }
}
