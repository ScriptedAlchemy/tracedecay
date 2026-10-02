//! Per-file graph projection outputs sealed after canonical resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, FileOccurrenceId, ManifestDigest, SanitizedCodeFileV1,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::GraphDbError;

use crate::chunks::{CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1};
use crate::graph_projection::{CodeGraphSymbolBindingV1, code_graph_symbol_bindings};
use crate::lineage::LineageSymbolRecordV1;

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
            let artifacts = self
                .files
                .iter()
                .find(|file| file.extraction.file_occurrence_id == *occurrence);
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
            visit(SealedCodeGraphPageV1 {
                file_key,
                file_occurrence_id: occurrence.clone(),
                logical_path: snapshot_file.logical_path.clone(),
                page_digest,
                encoded,
            })?;
        }
        Ok(())
    }
}
