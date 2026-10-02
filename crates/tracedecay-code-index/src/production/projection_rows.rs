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
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeChunkProjectionReceiptV1, CodeGenerationId,
    CodeSearchChunkId, CodeSearchChunkV1, ContentDigest, ManifestDigest, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionOperationV1, ProjectionOutcomeV1,
    ProjectionReplayReasonV1,
};

use super::CodeIndexProductionErrorV1;

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// Each file's chunks in chunk-id order, by the file's snapshot key. Encoding
/// and restore both derive it from the file artifacts, so positions agree.
pub(super) struct FileChunkRostersV1<'a> {
    files: BTreeMap<u32, Vec<&'a CodeSearchChunkV1>>,
}

impl<'a> FileChunkRostersV1<'a> {
    pub(super) fn new(files: impl Iterator<Item = (u32, &'a [Arc<CodeSearchChunkV1>])>) -> Self {
        Self {
            files: files
                .map(|(file_key, chunks)| {
                    let mut roster = chunks.iter().map(Arc::as_ref).collect::<Vec<_>>();
                    roster.sort_by(|left, right| left.id.cmp(&right.id));
                    (file_key, roster)
                })
                .collect(),
        }
    }

    fn chunk(&self, file_key: u32, position: u32) -> Result<&'a CodeSearchChunkV1, CodeIndexProductionErrorV1> {
        usize::try_from(position)
            .ok()
            .and_then(|position| self.files.get(&file_key)?.get(position))
            .copied()
            .ok_or_else(|| contract("sealed projection row names a chunk outside its file"))
    }
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
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut positions = HashMap::new();
        for (file_key, roster) in &rosters.files {
            for (position, chunk) in roster.iter().enumerate() {
                let position = u32::try_from(position)
                    .map_err(|_| contract("sealed file chunk roster exceeds u32"))?;
                positions.insert(&chunk.id, (*file_key, position, &chunk.content_digest));
            }
        }
        let changes = &request.changes;
        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut rows = Vec::new();
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
        changed.sort_by_key(|(file_key, position, _)| (*file_key, *position));
        let mut runs: Vec<AddedRunV1> = Vec::new();
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
        Ok(Self {
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
        })
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
                    chunk_id: chunk.id.clone(),
                    prior_digest: None,
                    current_digest: Some(chunk.content_digest.clone()),
                });
            }
        }
        for (file_key, position, prior) in changes.changed {
            let chunk = rosters.chunk(file_key, position)?;
            added_or_changed.push(ChangedCodeChunkV1 {
                chunk_id: chunk.id.clone(),
                prior_digest: Some(prior),
                current_digest: Some(chunk.content_digest.clone()),
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
    outcome: &'a ProjectionOutcomeV1,
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

/// The request's rows in the chunk order a receipt answers them in.
fn answered_rows(
    request: &ProjectionBatchRequestV1,
) -> BTreeMap<&CodeSearchChunkId, &ChangedCodeChunkV1> {
    request
        .changes
        .added_or_changed
        .iter()
        .chain(&request.changes.deleted)
        .map(|change| (&change.chunk_id, change))
        .collect()
}

/// The operation a receipt must record for `change`, with the outcome and
/// output a projector that applied it as requested records.
fn applied(change: &ChangedCodeChunkV1) -> ProjectionOperationV1 {
    match (&change.prior_digest, &change.current_digest) {
        (_, None) => ProjectionOperationV1::Deleted,
        (None, Some(_)) => ProjectionOperationV1::Added,
        (Some(_), Some(_)) => ProjectionOperationV1::Updated,
    }
}

impl<'a> PersistedBatchReceiptRefV1<'a> {
    pub(super) fn new(
        request: &ProjectionBatchRequestV1,
        receipt: &'a ProjectionBatchReceiptV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let rows = answered_rows(request);
        if receipt.receipts.len() != rows.len()
            || receipt
                .receipts
                .iter()
                .zip(rows.keys())
                .any(|(receipt, chunk_id)| &receipt.chunk_id != *chunk_id)
        {
            return Err(contract(
                "sealed projection receipt does not answer its request rows in chunk order",
            ));
        }
        let exceptions = receipt
            .receipts
            .iter()
            .zip(rows.values())
            .filter(|(receipt, change)| {
                receipt.operation != applied(change)
                    || receipt.outcome != ProjectionOutcomeV1::Applied
                    || receipt.output_digest != change.current_digest
            })
            .map(|(receipt, _)| PersistedChunkReceiptRefV1 {
                chunk_id: &receipt.chunk_id,
                operation: receipt.operation,
                outcome: &receipt.outcome,
                output_digest: receipt.output_digest.as_ref(),
            })
            .collect();
        Ok(Self {
            target_projection_key: &receipt.target_projection_key,
            request_digest: &receipt.request_digest,
            source_generation: &receipt.source_generation,
            source_manifest_digest: &receipt.source_manifest_digest,
            exceptions,
            reused_count: receipt.reused_count,
            publication_digest: &receipt.publication_digest,
        })
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
        let rows = answered_rows(request);
        let mut exceptions = self
            .exceptions
            .into_iter()
            .map(|exception| (exception.chunk_id.clone(), exception))
            .collect::<HashMap<_, _>>();
        let mut receipts = Vec::with_capacity(rows.len());
        for (chunk_id, change) in rows {
            let (operation, outcome, output_digest) = match exceptions.remove(chunk_id) {
                Some(exception) => (
                    exception.operation,
                    exception.outcome,
                    exception.output_digest,
                ),
                None => (
                    applied(change),
                    ProjectionOutcomeV1::Applied,
                    change.current_digest.clone(),
                ),
            };
            receipts.push(CodeChunkProjectionReceiptV1 {
                projection_key: self.target_projection_key.clone(),
                request_digest: self.request_digest.clone(),
                prior_generation: request.changes.from_generation.clone(),
                source_generation: self.source_generation.clone(),
                source_manifest_digest: self.source_manifest_digest.clone(),
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation,
                outcome,
                output_digest,
            });
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
