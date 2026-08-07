//! Verified Work product graph reads, with every projection derived from the
//! same version the caller asked for.
//!
//! ## The runtime coverage rule
//!
//! `WorkGraphVersionEntryV1` pairs a graph with a runtime projection, and the
//! domain validates that the attempts observed there are exactly the accepted
//! attempts the graph declares. This authority observes none: the durable
//! attempt rows live in `work_attempts_v1` under a `WorkAuthority`
//! (project/repository/worktree/actor/policy), and the product journal is keyed
//! by the registered profile owner. There is no recorded correspondence between
//! the two, so joining them would invent the very measurement Plan 11c forbids
//! estimating.
//!
//! So the coverage is reported, not guessed:
//!
//! * a graph that declares no accepted attempts gets `Complete` with zero
//!   attempts — a true and complete empty reading, not an absence;
//! * a graph that declares accepted attempts gets `Unavailable` — an explicit
//!   "this authority did not observe the runtime", which is what the Work views
//!   should draw as a named absence.
//!
//! When an executor authority that can prove the correspondence lands, it
//! supplies the observed attempts here and the coverage becomes `Complete`
//! without any other shape changing.
//!
//! ## The empty-journal rule
//!
//! An owner with no published version has no graph. `Current` and `AsOf` are
//! point reads of a version, and a version identity requires a non-zero event
//! sequence, so there is no representable "empty current graph": the absence is
//! typed as not-found-or-not-authorized. `Evolution` and `Forensic` are range
//! reads, and their explicit zero state *is* representable — an empty timeline
//! with `Complete { returned: 0 }` coverage — so that is what they answer.

use tracedecay_application::{
    MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1, OpaqueCursor, WorkGraphReadModeV1,
    WorkGraphReadPortErrorV1, WorkGraphReadPortV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkGraphTimelineV1, WorkGraphVersionEntryV1, WorkProductPortContextV1,
};
use tracedecay_domain::{
    ProjectionGenerationId, UtcMicros, WorkProductGraphV1, WorkProductProjectionBundleV1,
    WorkProjectionSequenceV1, WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1,
    canonical_sha256,
};

use super::{
    WorkProductJournalEntryV1, WorkProductPublishedVersionV1, fold_graph, load_journal,
    load_published_versions, selection_covers, verified_version,
};
use crate::work::WorkSqliteStorage;

type PortError = WorkGraphReadPortErrorV1;

/// The digest domain separator for a Work product projection generation.
const PROJECTION_GENERATION_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-projection-generation.v1";

impl WorkGraphReadPortV1 for WorkSqliteStorage {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, PortError> {
        let scope = context.authorized_scope();
        let journal = load_journal(&self.handle, scope).ok_or(PortError::Unavailable)?;
        // A selection that does not cover every event in the journal is refused
        // outright. Folding only the covered prefix would answer with a graph
        // that never existed, and answering "empty" would conceal a graph the
        // caller is simply not authorized for.
        if journal
            .iter()
            .any(|entry| !selection_covers(scope.selection(), &entry.event))
        {
            return Err(PortError::NotFoundOrNotAuthorized);
        }
        let published =
            load_published_versions(&self.handle, scope).ok_or(PortError::Unavailable)?;

        let entries = build_entries(&journal, &published, request.observed_at)?;
        match &request.mode {
            WorkGraphReadModeV1::Current => {
                let snapshot = entries
                    .into_iter()
                    .next_back()
                    .ok_or(PortError::NotFoundOrNotAuthorized)?;
                Ok(WorkGraphReadV1::Current {
                    authorized_scope: scope.clone(),
                    snapshot,
                })
            }
            WorkGraphReadModeV1::AsOf { valid_at } => {
                let snapshot = entries
                    .into_iter()
                    .filter(|entry| entry.valid_at() <= *valid_at)
                    .next_back()
                    .ok_or(PortError::NotFoundOrNotAuthorized)?;
                Ok(WorkGraphReadV1::AsOf {
                    authorized_scope: scope.clone(),
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
                    timeline: page(selected, request.continuation.as_ref())?,
                })
            }
        }
    }
}

/// Build one entry per published version, each carrying the graph folded to
/// that version and every projection derived from that same graph.
fn build_entries(
    journal: &[WorkProductJournalEntryV1],
    published: &[WorkProductPublishedVersionV1],
    projected_at: UtcMicros,
) -> Result<Vec<WorkGraphVersionEntryV1>, PortError> {
    published
        .iter()
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
            let runtime = runtime_projection(&graph, version, projected_at)?;
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
    graph: &WorkProductGraphV1,
    version: &WorkProductPublishedVersionV1,
    projected_at: UtcMicros,
) -> Result<WorkRuntimeProjectionV1, PortError> {
    let declares_accepted_attempts = graph
        .items()
        .iter()
        .any(|item| !item.accepted_attempts().is_empty());
    let coverage = if declares_accepted_attempts {
        WorkRuntimeProjectionCoverageV1::Unavailable
    } else {
        WorkRuntimeProjectionCoverageV1::Complete
    };
    let generation_id = canonical_sha256(&(
        PROJECTION_GENERATION_DOMAIN,
        version.graph_version.get(),
        version.event_sequence.get(),
    ))
    .ok()
    .and_then(|digest| ProjectionGenerationId::new(digest.as_str()).ok())
    .ok_or(PortError::Unavailable)?;
    WorkRuntimeProjectionV1::new(
        version.graph_version,
        generation_id,
        WorkProjectionSequenceV1::new(version.event_sequence.get()),
        projected_at,
        Vec::new(),
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
