//! Persisted projection request and receipt rows of one generation's evidence.
//!
//! A request's added-or-changed rows name chunks of the generation being
//! sealed, and each row's current digest is that chunk's content digest, so
//! the persisted form names those rows by position in the generation's
//! chunk-id-ordered roster (as runs when the chunk is new) and keeps every
//! other row whole. A receipt answers exactly the request's rows in chunk
//! order, and a row the projector applied as the request says is a pure
//! function of that row, so only the other decisions are persisted.
//! Restore re-verifies the request, manifest, and publication digests over
//! the rebuilt rows, so any disagreement fails closed.

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

/// A generation's chunks in canonical chunk-id order. Encoding and restore
/// both derive it from the file artifacts, so positions agree.
pub(super) fn chunk_roster<'a>(
    chunks: impl Iterator<Item = &'a Arc<CodeSearchChunkV1>>,
) -> Vec<&'a CodeSearchChunkV1> {
    let mut roster = chunks.map(Arc::as_ref).collect::<Vec<_>>();
    roster.sort_by(|left, right| left.id.cmp(&right.id));
    roster
}

#[derive(Serialize)]
pub(super) struct PersistedProjectionRequestRefV1<'a> {
    request_digest: &'a ManifestDigest,
    changes: PersistedChangeSetRefV1<'a>,
    previous_projection_key: &'a Option<ProjectionKeyV1>,
    target_projection_key: &'a ProjectionKeyV1,
    replay_reason: ProjectionReplayReasonV1,
}

#[derive(Serialize)]
struct PersistedChangeSetRefV1<'a> {
    from_generation: &'a Option<CodeGenerationId>,
    to_generation: &'a CodeGenerationId,
    manifest_digest: &'a ManifestDigest,
    added_or_changed: Vec<PersistedChangeRowRefV1<'a>>,
    deleted: &'a [ChangedCodeChunkV1],
    reused_count: u64,
    reused_digest: &'a ManifestDigest,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum PersistedChangeRowRefV1<'a> {
    Added {
        start: u32,
        count: u32,
    },
    Changed {
        current: u32,
        prior: &'a ContentDigest,
    },
    Row(&'a ChangedCodeChunkV1),
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
    added_or_changed: Vec<PersistedChangeRowV1>,
    deleted: Vec<ChangedCodeChunkV1>,
    reused_count: u64,
    reused_digest: ManifestDigest,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum PersistedChangeRowV1 {
    /// Roster chunks `start..start + count`, each new in this generation.
    Added {
        start: u32,
        count: u32,
    },
    /// Roster chunk `current`, changed from content digest `prior`.
    Changed {
        current: u32,
        prior: ContentDigest,
    },
    Row(ChangedCodeChunkV1),
}

impl<'a> PersistedProjectionRequestRefV1<'a> {
    pub(super) fn new(
        request: &'a ProjectionBatchRequestV1,
        roster: &[&CodeSearchChunkV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let positions = roster
            .iter()
            .enumerate()
            .map(|(position, chunk)| {
                u32::try_from(position)
                    .map(|position| (&chunk.id, (position, &chunk.content_digest)))
                    .map_err(|_| contract("sealed chunk roster exceeds u32"))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let changes = &request.changes;
        let mut rows = Vec::new();
        for change in &changes.added_or_changed {
            let position = positions
                .get(&change.chunk_id)
                .filter(|(_, digest)| change.current_digest.as_ref() == Some(*digest))
                .map(|(position, _)| *position);
            match (position, &change.prior_digest) {
                (Some(position), None) => match rows.last_mut() {
                    Some(PersistedChangeRowRefV1::Added { start, count })
                        if start.checked_add(*count) == Some(position) =>
                    {
                        *count += 1;
                    }
                    _ => rows.push(PersistedChangeRowRefV1::Added {
                        start: position,
                        count: 1,
                    }),
                },
                (Some(position), Some(prior)) => rows.push(PersistedChangeRowRefV1::Changed {
                    current: position,
                    prior,
                }),
                (None, _) => rows.push(PersistedChangeRowRefV1::Row(change)),
            }
        }
        Ok(Self {
            request_digest: &request.request_digest,
            changes: PersistedChangeSetRefV1 {
                from_generation: &changes.from_generation,
                to_generation: &changes.to_generation,
                manifest_digest: &changes.manifest_digest,
                added_or_changed: rows,
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
    pub(super) fn expand(
        self,
        roster: &[&CodeSearchChunkV1],
    ) -> Result<ProjectionBatchRequestV1, CodeIndexProductionErrorV1> {
        let chunk = |position: u32| {
            usize::try_from(position)
                .ok()
                .and_then(|position| roster.get(position))
                .copied()
                .ok_or_else(|| contract("sealed projection row names a chunk outside its roster"))
        };
        let changes = self.changes;
        let mut added_or_changed = Vec::new();
        for row in changes.added_or_changed {
            match row {
                PersistedChangeRowV1::Added { start, count } => {
                    let end = start
                        .checked_add(count)
                        .ok_or_else(|| contract("sealed projection run exceeds u32"))?;
                    for position in start..end {
                        let chunk = chunk(position)?;
                        added_or_changed.push(ChangedCodeChunkV1 {
                            chunk_id: chunk.id.clone(),
                            prior_digest: None,
                            current_digest: Some(chunk.content_digest.clone()),
                        });
                    }
                }
                PersistedChangeRowV1::Changed { current, prior } => {
                    let chunk = chunk(current)?;
                    added_or_changed.push(ChangedCodeChunkV1 {
                        chunk_id: chunk.id.clone(),
                        prior_digest: Some(prior),
                        current_digest: Some(chunk.content_digest.clone()),
                    });
                }
                PersistedChangeRowV1::Row(change) => added_or_changed.push(change),
            }
        }
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
