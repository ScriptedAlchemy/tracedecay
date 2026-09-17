//! Verified Work product graph reads, with every projection derived from the
//! same version the caller asked for.
//!
//! ## The runtime coverage rule
//!
//! `WorkGraphVersionEntryV1` pairs a graph with a runtime projection, and the
//! domain validates that the attempts observed there are exactly the accepted
//! attempts the graph declares. Durable attempt rows live in `work_attempts_v1`
//! under a `WorkAuthority` (project/repository/worktree/actor/policy), while the
//! product journal is keyed by the registered profile owner. Plain storage has
//! no correspondence between them. Daemon routes bind their registered
//! authority through [`AuthorizedWorkProductReadStorageV1`], which can hydrate
//! exactly the accepted attempt identities declared by each graph version.
//!
//! So the coverage is reported, not guessed:
//!
//! * a graph that declares no accepted attempts gets `Complete` with zero
//!   attempts — a true and complete empty reading, not an absence;
//! * a graph that declares accepted attempts gets `Unavailable` without an
//!   executor authority;
//! * an authority-bound read loads those exact attempts and reports complete or
//!   partial coverage without crossing into any other Work authority.
//!
//! ## The selection-coverage rule
//!
//! A selection names a slice of the owner's work, not the whole journal. An
//! event outside the selection falls outside the slice; it does not invalidate
//! the events inside it. So a read is answered over the journal's covered
//! prefix — see [`covered_prefix`](super::covered_prefix) for why the covered
//! slice is always a prefix — and carries a
//! [`WorkGraphSelectionCoverageV1`](tracedecay_contracts::WorkGraphSelectionCoverageV1)
//! that says how much lies outside it. Answering the slice silently would be
//! the real falsification; refusing the whole read because a later event was
//! admitted under a scope this selection does not name would discard work the
//! caller is plainly authorized for.
//!
//! Published versions are filtered by the same boundary: a version folded from
//! an event outside the selection is not readable under it at all.
//!
//! ## The empty-journal rule
//!
//! An owner with no published version has no graph. `Current` and `AsOf` are
//! point reads of a version, and a version identity requires a non-zero event
//! sequence, so there is no representable "empty current graph": the absence is
//! typed as not-found-or-not-authorized. `Evolution` and `Forensic` are range
//! reads, and their explicit zero state *is* representable — an empty timeline
//! with `Complete { returned: 0 }` coverage — so that is what they answer.

use tracedecay_contracts::{
    MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1, OpaqueCursor, VerifiedWorkEvidenceRootV1,
    VerifiedWorkGraphVersionV1, WorkAttemptReceiptReadErrorV1, WorkAttemptReceiptReadPortV1,
    WorkAttemptReceiptV1, WorkAttemptStoragePort, WorkEvidenceRootReadErrorV1,
    WorkEvidenceRootReadPortV1, WorkGraphReadModeV1, WorkGraphReadPortErrorV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkGraphTimelineV1, WorkGraphVersionEntryV1,
    WorkProductPortContextV1,
};
use tracedecay_domain::{
    ProjectionGenerationId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority,
    WorkProductGraphV1, WorkProductProjectionBundleV1, WorkProjectionSequenceV1,
    WorkRuntimeAttemptProjectionV1, WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1,
    canonical_sha256,
};

use super::{
    WorkProductJournalEntryV1, WorkProductPublishedVersionV1, fold_graph, load_covered_journal,
    verified_version,
};
use crate::work::WorkSqliteStorage;

type PortError = WorkGraphReadPortErrorV1;

/// The digest domain separator for a Work product projection generation.
const PROJECTION_GENERATION_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-projection-generation.v1";

#[derive(Clone)]
pub struct AuthorizedWorkProductReadStorageV1 {
    storage: WorkSqliteStorage,
    authority: WorkAuthority,
}

impl AuthorizedWorkProductReadStorageV1 {
    pub const fn new(storage: WorkSqliteStorage, authority: WorkAuthority) -> Self {
        Self { storage, authority }
    }
}

impl WorkGraphReadPortV1 for WorkSqliteStorage {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, PortError> {
        read_graph(self, None, context, request)
    }
}

impl WorkGraphReadPortV1 for AuthorizedWorkProductReadStorageV1 {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, PortError> {
        read_graph(&self.storage, Some(&self.authority), context, request)
    }
}

// The bound runtime authority only licenses hydrating accepted attempt rows for
// the graph read. Evidence roots are scoped by the port context and attempt
// receipts by the authority the caller passes, so binding one here would either
// be ignored or override the caller's — both routes answer from the same
// storage.
impl WorkEvidenceRootReadPortV1 for AuthorizedWorkProductReadStorageV1 {
    fn read_evidence_root(
        &self,
        context: &WorkProductPortContextV1,
        task_id: &TaskId,
        requested: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1> {
        self.storage.read_evidence_root(context, task_id, requested)
    }
}

impl WorkAttemptReceiptReadPortV1 for AuthorizedWorkProductReadStorageV1 {
    fn attempt_receipt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptReceiptV1, WorkAttemptReceiptReadErrorV1> {
        self.storage.attempt_receipt(authority, identity)
    }
}

fn read_graph(
    storage: &WorkSqliteStorage,
    authority: Option<&WorkAuthority>,
    context: &WorkProductPortContextV1,
    request: &WorkGraphReadRequestV1,
) -> Result<WorkGraphReadV1, PortError> {
    let scope = context.authorized_scope();
    // Events outside the selection fall outside it; they do not poison the
    // ones inside. The read is answered over the covered prefix and carries
    // the coverage that says what was left out, so a caller can never
    // mistake a slice for the whole.
    let covered = load_covered_journal(storage.handle(), scope).ok_or(PortError::Unavailable)?;
    let selection_coverage = covered.coverage;

    let entries = build_entries(
        storage,
        authority,
        &covered.journal,
        &covered.published,
        request.observed_at,
    )?;
    match &request.mode {
        WorkGraphReadModeV1::Current => {
            let snapshot = entries
                .into_iter()
                .next_back()
                .ok_or(PortError::NotFoundOrNotAuthorized)?;
            Ok(WorkGraphReadV1::Current {
                authorized_scope: scope.clone(),
                selection_coverage,
                snapshot,
            })
        }
        WorkGraphReadModeV1::AsOf { valid_at } => {
            let snapshot = entries
                .into_iter()
                .rfind(|entry| entry.valid_at() <= *valid_at)
                .ok_or(PortError::NotFoundOrNotAuthorized)?;
            Ok(WorkGraphReadV1::AsOf {
                authorized_scope: scope.clone(),
                selection_coverage,
                snapshot,
            })
        }
        WorkGraphReadModeV1::Evolution {
            from_valid_at,
            through_valid_at,
        } => {
            let selected = entries
                .into_iter()
                .filter(|entry| {
                    entry.valid_at() >= *from_valid_at && entry.valid_at() <= *through_valid_at
                })
                .collect::<Vec<_>>();
            Ok(WorkGraphReadV1::Evolution {
                authorized_scope: scope.clone(),
                selection_coverage,
                timeline: page(selected, request.continuation.as_ref())?,
            })
        }
        WorkGraphReadModeV1::Forensic {
            from_observed_at,
            through_observed_at,
        } => {
            let selected = entries
                .into_iter()
                .filter(|entry| {
                    entry.observed_at() >= *from_observed_at
                        && entry.observed_at() <= *through_observed_at
                })
                .collect::<Vec<_>>();
            Ok(WorkGraphReadV1::Forensic {
                authorized_scope: scope.clone(),
                selection_coverage,
                timeline: page(selected, request.continuation.as_ref())?,
            })
        }
    }
}

/// Build one entry per published version, each carrying the graph folded to
/// that version and every projection derived from that same graph.
fn build_entries(
    storage: &WorkSqliteStorage,
    authority: Option<&WorkAuthority>,
    journal: &[WorkProductJournalEntryV1],
    published: &[WorkProductPublishedVersionV1],
    projected_at: UtcMicros,
) -> Result<Vec<WorkGraphVersionEntryV1>, PortError> {
    published
        .iter()
        // A read cannot include a version this authority had not observed at
        // the caller's observation instant. Besides preserving forensic
        // truth, this lets a prepared mutation read its former head and reach
        // the event journal's authoritative compare-and-swap conflict when a
        // later version has already committed.
        .filter(|version| version.observed_at <= projected_at)
        .map(|version| {
            let entry = journal
                .iter()
                .find(|entry| entry.sequence == version.event_sequence)
                .ok_or(PortError::Unavailable)?;
            let graph =
                fold_graph(journal, version.event_sequence).ok_or(PortError::Unavailable)?;
            if graph.version() != version.graph_version {
                return Err(PortError::Unavailable);
            }
            let verified = verified_version(version, &entry.event).ok_or(PortError::Unavailable)?;
            let runtime = runtime_projection(storage, authority, &graph, version, projected_at)?;
            let projections =
                WorkProductProjectionBundleV1::from_graph(&graph, &runtime, projected_at)
                    .map_err(|_| PortError::Unavailable)?;
            WorkGraphVersionEntryV1::new(
                version.valid_at,
                version.observed_at,
                projected_at,
                verified,
                graph,
                runtime,
                projections,
            )
            .map_err(|_| PortError::Unavailable)
        })
        .collect()
}

/// The runtime reading this authority can actually prove for one version.
///
/// See the module documentation for why an unobserved runtime is reported as
/// `Unavailable` instead of as zero attempts.
fn runtime_projection(
    storage: &WorkSqliteStorage,
    authority: Option<&WorkAuthority>,
    graph: &WorkProductGraphV1,
    version: &WorkProductPublishedVersionV1,
    projected_at: UtcMicros,
) -> Result<WorkRuntimeProjectionV1, PortError> {
    let accepted = graph
        .items()
        .iter()
        .flat_map(|item| item.accepted_attempts().iter())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut attempts = Vec::new();
    let mut unavailable = std::collections::BTreeSet::new();
    if let Some(authority) = authority {
        for identity in &accepted {
            match storage.load(authority, identity) {
                Ok(attempt) => attempts.push(WorkRuntimeAttemptProjectionV1 {
                    identity: attempt.identity().clone(),
                    state: attempt.state(),
                }),
                Err(_) => {
                    unavailable.insert(identity.clone());
                }
            }
        }
    } else {
        unavailable = accepted;
    }
    let coverage = if unavailable.is_empty() {
        WorkRuntimeProjectionCoverageV1::Complete
    } else if attempts.is_empty() {
        WorkRuntimeProjectionCoverageV1::Unavailable
    } else {
        WorkRuntimeProjectionCoverageV1::Partial {
            unavailable_attempts: unavailable,
        }
    };
    let generation_id = canonical_sha256(&(
        PROJECTION_GENERATION_DOMAIN,
        version.graph_version.get(),
        version.event_sequence.get(),
        &attempts,
        &coverage,
    ))
    .ok()
    .and_then(|digest| ProjectionGenerationId::new(digest.as_str()).ok())
    .ok_or(PortError::Unavailable)?;
    WorkRuntimeProjectionV1::new(
        version.graph_version,
        generation_id,
        WorkProjectionSequenceV1::new(version.event_sequence.get()),
        projected_at,
        attempts,
        coverage,
    )
    .map_err(|_| PortError::Unavailable)
}

/// Bound one timeline page, resuming from a continuation the previous page
/// issued.
///
/// The cursor names the graph version the previous page ended on, so resuming
/// is exact rather than offset-based: a version published between two pages
/// cannot shift a caller past an entry it never saw.
fn page(
    entries: Vec<WorkGraphVersionEntryV1>,
    continuation: Option<&OpaqueCursor>,
) -> Result<WorkGraphTimelineV1, PortError> {
    let remaining = match continuation {
        None => entries,
        Some(cursor) => {
            let after = cursor
                .as_str()
                .strip_prefix(TIMELINE_CURSOR_PREFIX)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(PortError::NotFoundOrNotAuthorized)?;
            entries
                .into_iter()
                .filter(|entry| entry.verified_version().graph_version().get() > after)
                .collect()
        }
    };
    if remaining.len() <= MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1 {
        return WorkGraphTimelineV1::complete(remaining).map_err(|_| PortError::Unavailable);
    }
    let mut page = remaining;
    page.truncate(MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1);
    let last = page
        .last()
        .map(|entry| entry.verified_version().graph_version().get())
        .ok_or(PortError::Unavailable)?;
    let cursor = OpaqueCursor::new(format!("{TIMELINE_CURSOR_PREFIX}{last}"))
        .map_err(|_| PortError::Unavailable)?;
    WorkGraphTimelineV1::partial(page, cursor).map_err(|_| PortError::Unavailable)
}

const TIMELINE_CURSOR_PREFIX: &str = "work-product-graph-version:";
