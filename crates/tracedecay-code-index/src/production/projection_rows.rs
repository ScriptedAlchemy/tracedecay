//! Persisted projection request and receipt rows of one generation's evidence.
//!
//! A request's added-or-changed rows name chunks of the generation being
//! sealed, and each row's current digest is that chunk's content digest, so
//! the persisted form names those rows by their file's snapshot key and their
//! position among that file's chunks in chunk-id order (as runs when the
//! chunks are new) and keeps every other row whole. A row therefore decodes
//! against its own file, and a seal that wrote only some files' chunks names
//! them without the generation's whole chunk roster. A receipt answers
//! exactly the request's rows in chunk order, and a row the projector applied
//! as the request says is a pure function of that row, so only the other
//! decisions are persisted. Restore re-verifies the request, manifest, and
//! publication digests over the rebuilt rows, so any disagreement fails
//! closed.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeSearchChunkId,
    CodeSearchChunkV1, ContentDigest, ManifestDigest, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionOperationV1, ProjectionOutcomeV1,
    ProjectionReplayReasonV1,
};

use super::{CodeIndexProductionErrorV1, resident_bytes};
use crate::projection::ProjectionPublicationHandoffV1;
use crate::receipts::{applied_operation, applied_receipt, ordered_changes};

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// Each file's chunks in chunk-id order, by the file's snapshot key. Encoding
/// and restore both derive it from the file artifacts, so positions agree.
pub(super) struct FileChunkRostersV1<'a> {
    files: BTreeMap<u32, Vec<(&'a CodeSearchChunkId, &'a ContentDigest)>>,
}

impl<'a> FileChunkRostersV1<'a> {
    pub(super) fn new(files: impl Iterator<Item = (u32, &'a [Arc<CodeSearchChunkV1>])>) -> Self {
        Self {
            files: files
                .map(|(file_key, chunks)| {
                    let mut roster = chunks
                        .iter()
                        .map(|chunk| (&chunk.id, &chunk.content_digest))
                        .collect::<Vec<_>>();
                    roster.sort_by(|left, right| left.0.cmp(right.0));
                    (file_key, roster)
                })
                .collect(),
        }
    }

    /// Cold construction owns only authenticated change rows and their file keys.
    /// Admission precedes both a new tree entry and a roster reallocation.
    pub(super) fn from_changes(
        changes: &'a [ChangedCodeChunkV1],
        file_keys: &[u32],
        base: usize,
        retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        if changes.len() != file_keys.len() {
            return Err(contract("cold chunk membership and change rows disagree"));
        }
        let mut files = BTreeMap::<u32, Vec<_>>::new();
        let mut slots = 0_usize;
        for (change, key) in changes.iter().zip(file_keys) {
            let digest = change
                .current_digest
                .as_ref()
                .ok_or_else(|| contract("cold chunk has no content digest"))?;
            if !files.contains_key(key) {
                retain(
                    (base
                        .saturating_add(slots)
                        .saturating_add(roster_tree_bytes(files.len() + 1)))
                        as u64,
                )?;
            }
            let tree_bytes = roster_tree_bytes(files.len() + usize::from(!files.contains_key(key)));
            let roster = files.entry(*key).or_default();
            if roster.len() == roster.capacity() {
                let capacity = roster.capacity().saturating_mul(2).max(1);
                let new_bytes = capacity
                    .saturating_mul(std::mem::size_of::<(&CodeSearchChunkId, &ContentDigest)>());
                // The old allocation can remain live while realloc obtains the new one.
                retain(
                    (base
                        .saturating_add(tree_bytes)
                        .saturating_add(slots)
                        .saturating_add(new_bytes)) as u64,
                )?;
                let old_bytes = roster
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(&CodeSearchChunkId, &ContentDigest)>());
                roster
                    .try_reserve_exact(capacity - roster.len())
                    .map_err(|error| contract(&error.to_string()))?;
                slots =
                    slots.saturating_sub(old_bytes).saturating_add(
                        roster.capacity().saturating_mul(std::mem::size_of::<(
                            &CodeSearchChunkId,
                            &ContentDigest,
                        )>()),
                    );
            }
            roster.push((&change.chunk_id, digest));
        }
        let rosters = Self { files };
        retain((base.saturating_add(rosters.retained_bytes())) as u64)?;
        Ok(rosters)
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.files.values().fold(roster_tree_bytes(self.files.len()), |bytes, roster| {
            bytes.saturating_add(roster.capacity().saturating_mul(std::mem::size_of::<(&CodeSearchChunkId, &ContentDigest)>()))
        })
    }

    fn chunk(
        &self,
        file_key: u32,
        position: u32,
    ) -> Result<(&'a CodeSearchChunkId, &'a ContentDigest), CodeIndexProductionErrorV1> {
        usize::try_from(position)
            .ok()
            .and_then(|position| self.files.get(&file_key)?.get(position))
            .copied()
            .ok_or_else(|| contract("sealed projection row names a chunk outside its file"))
    }
}

fn roster_tree_bytes(files: usize) -> usize {
    resident_bytes::btree_bytes(
        files,
        std::mem::size_of::<(u32, Vec<(&CodeSearchChunkId, &ContentDigest)>)>(),
    )
}

#[derive(Serialize)]
pub(super) struct PersistedProjectionRequestRefV1<'a> {
    request_digest: &'a ManifestDigest,
    changes: PersistedChangeSetRefV1<'a>,
    previous_projection_key: &'a Option<ProjectionKeyV1>,
    target_projection_key: &'a ProjectionKeyV1,
    replay_reason: ProjectionReplayReasonV1,
}

/// `(file key, first position, count)`: consecutive new chunks of one file.
type AddedRunV1 = (u32, u32, u32);

#[derive(Serialize)]
struct PersistedChangeSetRefV1<'a> {
    from_generation: &'a Option<CodeGenerationId>,
    to_generation: &'a CodeGenerationId,
    manifest_digest: &'a ManifestDigest,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    added: Vec<AddedRunV1>,
    /// `(file key, position, prior content digest)`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed: Vec<(u32, u32, &'a ContentDigest)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rows: Vec<&'a ChangedCodeChunkV1>,
    deleted: &'a [ChangedCodeChunkV1],
    reused_count: u64,
    reused_digest: &'a ManifestDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedProjectionRequestV1 {
    request_digest: ManifestDigest,
    changes: PersistedChangeSetV1,
    previous_projection_key: Option<ProjectionKeyV1>,
    target_projection_key: ProjectionKeyV1,
    replay_reason: ProjectionReplayReasonV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedChangeSetV1 {
    from_generation: Option<CodeGenerationId>,
    to_generation: CodeGenerationId,
    manifest_digest: ManifestDigest,
    #[serde(default)]
    added: Vec<AddedRunV1>,
    #[serde(default)]
    changed: Vec<(u32, u32, ContentDigest)>,
    #[serde(default)]
    rows: Vec<ChangedCodeChunkV1>,
    deleted: Vec<ChangedCodeChunkV1>,
    reused_count: u64,
    reused_digest: ManifestDigest,
}

impl<'a> PersistedProjectionRequestRefV1<'a> {
    /// `rosters` must hold every file whose chunks the request adds or
    /// changes; a row naming a chunk of no roster is kept whole.
    pub(super) fn new(
        request: &'a ProjectionBatchRequestV1,
        rosters: &FileChunkRostersV1<'_>,
        base: usize,
        retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let count = rosters.files.values().map(Vec::len).sum::<usize>();
        let table_bytes = resident_bytes::hash_table_bytes::<(
            &CodeSearchChunkId,
            (u32, u32, &ContentDigest),
        )>(super::helpers::resolution_hash_capacity(count));
        retain((base.saturating_add(table_bytes)) as u64)?;
        let mut positions = HashMap::with_capacity(count);
        for (file_key, roster) in &rosters.files {
            for (position, chunk) in roster.iter().enumerate() {
                let position = u32::try_from(position)
                    .map_err(|_| contract("sealed file chunk roster exceeds u32"))?;
                positions.insert(chunk.0, (*file_key, position, chunk.1));
            }
        }
        let changes = &request.changes;
        let mut counts = [0_usize; 3];
        for change in &changes.added_or_changed {
            let located = positions
                .get(&change.chunk_id)
                .is_some_and(|(_, _, digest)| change.current_digest.as_ref() == Some(*digest));
            counts[if !located {
                2
            } else if change.prior_digest.is_some() {
                1
            } else {
                0
            }] += 1;
        }
        let vectors = counts[0]
            .saturating_mul(std::mem::size_of::<(u32, u32)>() + std::mem::size_of::<AddedRunV1>())
            .saturating_add(
                counts[1].saturating_mul(std::mem::size_of::<(u32, u32, &ContentDigest)>()),
            )
            .saturating_add(counts[2].saturating_mul(std::mem::size_of::<&ChangedCodeChunkV1>()));
        retain((base.saturating_add(table_bytes).saturating_add(vectors)) as u64)?;
        let mut added = Vec::with_capacity(counts[0]);
        let mut changed = Vec::with_capacity(counts[1]);
        let mut rows = Vec::with_capacity(counts[2]);
        for change in &changes.added_or_changed {
            let located = positions
                .get(&change.chunk_id)
                .filter(|(_, _, digest)| change.current_digest.as_ref() == Some(*digest));
            match (located, &change.prior_digest) {
                (Some((file_key, position, _)), None) => added.push((*file_key, *position)),
                (Some((file_key, position, _)), Some(prior)) => {
                    changed.push((*file_key, *position, prior));
                }
                (None, _) => rows.push(change),
            }
        }
        added.sort_unstable();
        changed.sort_unstable_by_key(|(file_key, position, _)| (*file_key, *position));
        let mut runs: Vec<AddedRunV1> = Vec::with_capacity(counts[0]);
        for (file_key, position) in added {
            match runs.last_mut() {
                Some((run_file, start, count))
                    if *run_file == file_key && start.checked_add(*count) == Some(position) =>
                {
                    *count += 1;
                }
                _ => runs.push((file_key, position, 1)),
            }
        }
        drop(positions);
        let result = Self {
            request_digest: &request.request_digest,
            changes: PersistedChangeSetRefV1 {
                from_generation: &changes.from_generation,
                to_generation: &changes.to_generation,
                manifest_digest: &changes.manifest_digest,
                added: runs,
                changed,
                rows,
                deleted: &changes.deleted,
                reused_count: changes.reused_count,
                reused_digest: &changes.reused_digest,
            },
            previous_projection_key: &request.previous_projection_key,
            target_projection_key: &request.target_projection_key,
            replay_reason: request.replay_reason,
        };
        retain((base.saturating_add(result.retained_bytes())) as u64)?;
        Ok(result)
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.changes
            .added
            .capacity()
            .saturating_mul(std::mem::size_of::<AddedRunV1>())
            .saturating_add(
                self.changes
                    .changed
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u32, u32, &ContentDigest)>()),
            )
            .saturating_add(
                self.changes
                    .rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<&ChangedCodeChunkV1>()),
            )
    }
}

impl PersistedProjectionRequestV1 {
    pub(super) fn target_projection_key(&self) -> &ProjectionKeyV1 {
        &self.target_projection_key
    }

    pub(super) fn expand(
        self,
        rosters: &FileChunkRostersV1<'_>,
    ) -> Result<ProjectionBatchRequestV1, CodeIndexProductionErrorV1> {
        let changes = self.changes;
        let mut added_or_changed = Vec::new();
        for (file_key, start, count) in changes.added {
            let end = start
                .checked_add(count)
                .ok_or_else(|| contract("sealed projection run exceeds u32"))?;
            for position in start..end {
                let chunk = rosters.chunk(file_key, position)?;
                added_or_changed.push(ChangedCodeChunkV1 {
                    chunk_id: chunk.0.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.1.clone()),
                });
            }
        }
        for (file_key, position, prior) in changes.changed {
            let chunk = rosters.chunk(file_key, position)?;
            added_or_changed.push(ChangedCodeChunkV1 {
                chunk_id: chunk.0.clone(),
                prior_digest: Some(prior),
                current_digest: Some(chunk.1.clone()),
            });
        }
        added_or_changed.extend(changes.rows);
        added_or_changed.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        Ok(ProjectionBatchRequestV1 {
            request_digest: self.request_digest,
            changes: ChangedCodeChunkSetV1 {
                from_generation: changes.from_generation,
                to_generation: changes.to_generation,
                manifest_digest: changes.manifest_digest,
                added_or_changed,
                deleted: changes.deleted,
                reused_count: changes.reused_count,
                reused_digest: changes.reused_digest,
            },
            previous_projection_key: self.previous_projection_key,
            target_projection_key: self.target_projection_key,
            replay_reason: self.replay_reason,
        })
    }
}

/// The receipt's batch header and the decisions that are not the default
/// for their request row.
#[derive(Serialize)]
pub(super) struct PersistedBatchReceiptRefV1<'a> {
    target_projection_key: &'a ProjectionKeyV1,
    request_digest: &'a ManifestDigest,
    source_generation: &'a CodeGenerationId,
    source_manifest_digest: &'a ManifestDigest,
    exceptions: Vec<PersistedChunkReceiptRefV1<'a>>,
    reused_count: u64,
    publication_digest: &'a ManifestDigest,
}

#[derive(Serialize)]
struct PersistedChunkReceiptRefV1<'a> {
    chunk_id: &'a CodeSearchChunkId,
    operation: ProjectionOperationV1,
    outcome: ProjectionOutcomeV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_digest: Option<&'a ContentDigest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedBatchReceiptV1 {
    target_projection_key: ProjectionKeyV1,
    request_digest: ManifestDigest,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    exceptions: Vec<PersistedChunkReceiptV1>,
    reused_count: u64,
    publication_digest: ManifestDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedChunkReceiptV1 {
    chunk_id: CodeSearchChunkId,
    operation: ProjectionOperationV1,
    outcome: ProjectionOutcomeV1,
    #[serde(default)]
    output_digest: Option<ContentDigest>,
}

impl<'a> PersistedBatchReceiptRefV1<'a> {
    pub(super) fn allocation_bytes(projection: &ProjectionPublicationHandoffV1) -> usize {
        projection
            .output_overrides()
            .len()
            .saturating_mul(std::mem::size_of::<PersistedChunkReceiptRefV1<'_>>())
    }

    pub(super) fn allocation_bytes_held(&self) -> usize {
        self.exceptions
            .capacity()
            .saturating_mul(std::mem::size_of::<PersistedChunkReceiptRefV1<'_>>())
    }

    pub(super) fn new(projection: &'a ProjectionPublicationHandoffV1) -> Self {
        let mut overrides = projection.output_overrides().iter().peekable();
        let mut exceptions = Vec::with_capacity(projection.output_overrides().len());
        exceptions.extend(
            ordered_changes(&projection.request().changes)
                .enumerate()
                .filter_map(|(index, change)| {
                    overrides
                        .next_if(|(position, _)| *position == index)
                        .map(|(_, output)| PersistedChunkReceiptRefV1 {
                            chunk_id: &change.chunk_id,
                            operation: applied_operation(change),
                            outcome: ProjectionOutcomeV1::Applied,
                            output_digest: Some(output),
                        })
                }),
        );
        Self {
            target_projection_key: projection.target_projection_key(),
            request_digest: projection.request_digest(),
            source_generation: projection.source_generation(),
            source_manifest_digest: projection.source_manifest_digest(),
            exceptions,
            reused_count: projection.reused_count(),
            publication_digest: projection.publication_digest(),
        }
    }
}

impl PersistedBatchReceiptV1 {
    /// Rebuild the full receipt rows from the request. Every field restored
    /// here is one the receipt verifier requires to equal the request, so the
    /// batch's publication digest recomputes over the same bytes it sealed.
    pub(super) fn expand(
        self,
        request: &ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, CodeIndexProductionErrorV1> {
        let mut exceptions = HashMap::new();
        for exception in self.exceptions {
            if exceptions
                .insert(exception.chunk_id.clone(), exception)
                .is_some()
            {
                return Err(contract(
                    "sealed generation receipt repeats a chunk exception",
                ));
            }
        }
        let rows = ordered_changes(&request.changes);
        let mut receipts = Vec::with_capacity(rows.len());
        for change in rows {
            let mut receipt = applied_receipt(request, change);
            receipt.projection_key = self.target_projection_key.clone();
            receipt.request_digest = self.request_digest.clone();
            receipt.source_generation = self.source_generation.clone();
            receipt.source_manifest_digest = self.source_manifest_digest.clone();
            if let Some(exception) = exceptions.remove(&change.chunk_id) {
                receipt.operation = exception.operation;
                receipt.outcome = exception.outcome;
                receipt.output_digest = exception.output_digest;
            }
            receipts.push(receipt);
        }
        if !exceptions.is_empty() {
            return Err(contract(
                "sealed generation receipt names a chunk outside its projection request",
            ));
        }
        Ok(ProjectionBatchReceiptV1 {
            target_projection_key: self.target_projection_key,
            request_digest: self.request_digest,
            source_generation: self.source_generation,
            source_manifest_digest: self.source_manifest_digest,
            receipts,
            reused_count: self.reused_count,
            publication_digest: self.publication_digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PersistedBatchReceiptRefV1, PersistedBatchReceiptV1};
    use crate::projection::{ProjectionPublicationHandoffV1, project_for_publication};
    use crate::receipts::{
        ChunkProjectionDecisionV1, build_batch_receipt, expected_request_digest,
    };
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, ContentDigest, ProjectionBatchRequestV1, ProjectionOperationV1,
        ProjectionOutcomeV1,
    };

    fn request() -> ProjectionBatchRequestV1 {
        let digest = format!("sha256:{}", "a".repeat(64));
        let mut request: ProjectionBatchRequestV1 = serde_json::from_value(serde_json::json!({
            "request_digest": digest,
            "changes": {
                "from_generation": null,
                "to_generation": "generation.v1.aaaaaaaa.00000001",
                "manifest_digest": digest,
                "added_or_changed": [{"chunk_id": "chunk.v1.added", "prior_digest": null, "current_digest": digest}],
                "deleted": [], "reused_count": 0, "reused_digest": digest
            },
            "previous_projection_key": null,
            "target_projection_key": {"kind": "lexical", "schema_revision": "lexical.v1", "profile_digest": digest},
            "replay_reason": "initial_projection"
        })).expect("request fixture");
        request.changes.reused_digest = ChangedCodeChunkSetV1::seal_reused_partition(&[])
            .expect("empty reuse")
            .1;
        request.changes.seal().expect("changes seal");
        request.request_digest = expected_request_digest(&request).expect("request seal");
        request
    }

    #[test]
    fn compact_receipt_wire_restores_nondefault_outputs_and_rejects_exception_corruption() {
        let request = request();
        let change = &request.changes.added_or_changed[0];
        let output =
            ContentDigest::new(format!("sha256:{}", "f".repeat(64))).expect("output digest");
        let expanded = build_batch_receipt(
            &request,
            &[ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: change.current_digest.clone(),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output),
            }],
        )
        .expect("expanded evidence");
        let handoff = ProjectionPublicationHandoffV1::restore(request.clone(), expanded.clone())
            .expect("verified handoff");
        let wire = serde_json::to_value(PersistedBatchReceiptRefV1::new(&handoff))
            .expect("persist compact receipt");
        let decoded: PersistedBatchReceiptV1 =
            serde_json::from_value(wire.clone()).expect("decode");
        assert_eq!(decoded.expand(&request).expect("restore rows"), expanded);

        let mut duplicate = wire.clone();
        let exceptions = duplicate["exceptions"].as_array_mut().expect("exceptions");
        assert_eq!(exceptions.len(), 1);
        exceptions.push(exceptions[0].clone());
        let duplicate: PersistedBatchReceiptV1 =
            serde_json::from_value(duplicate).expect("duplicate wire");
        assert!(duplicate.expand(&request).is_err());

        let mut extra = wire;
        extra["exceptions"][0]["chunk_id"] = serde_json::json!("chunk.v1.foreign");
        let extra: PersistedBatchReceiptV1 = serde_json::from_value(extra).expect("foreign wire");
        assert!(extra.expand(&request).is_err());

        let default = project_for_publication(request).expect("default publication");
        let default_wire = serde_json::to_value(PersistedBatchReceiptRefV1::new(&default))
            .expect("default compact receipt");
        assert!(
            default_wire["exceptions"]
                .as_array()
                .expect("default exceptions")
                .is_empty()
        );
    }
}
