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
//! exactly the accepted attempt identities declared by each graph version, in
//! bounded batches.
//!
//! So the coverage is reported, not guessed:
//!
//! * a graph that declares no accepted attempts gets `Complete` with zero
//!   attempts, a true and complete empty reading, not an absence;
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
//! prefix, see [`Coverage`](super::journal::Coverage) for why the covered slice
//! is always a prefix, and carries a
//! [`WorkGraphSelectionCoverageV1`](tracedecay_contracts::WorkGraphSelectionCoverageV1)
//! that says how much lies outside it. Answering the slice silently would be
//! the real falsification; refusing the whole read because a later event was
//! admitted under a scope this selection does not name would discard work the
//! caller is plainly authorized for.
//!
//! The prefix's end comes from the scope registry, one row per distinct scope
//! set, so no read scans events to find it. Published versions are bounded by
//! the same prefix: a version folded from an event outside the selection is not
//! readable under it at all.
//!
//! * `Current` is the newest version of the prefix observed by the caller's
//!   instant. It is served from the journal's head when that is the newest, so a
//!   current read over a fully covered journal touches no event.
//! * `AsOf`, `Evolution` and `Forensic` are historical. They select version
//!   rows by their own instants and reconstruct each graph from the nearest
//!   checkpoint, and every reconstructed graph must reproduce its recorded
//!   digest.
//!
//! ## The empty-journal rule
//!
//! An owner with no published version has no graph. `Current` and `AsOf` are
//! point reads of a version, and a version identity requires a non-zero event
//! sequence, so there is no representable "empty current graph": the absence is
//! answered as `WorkGraphReadV1::Absent` under the already-authorized scope,
//! never as a denial. `Evolution` and `Forensic` are range reads, and their
//! explicit zero state *is* representable, an empty timeline with
//! `Complete { returned: 0 }` coverage, so that is what they answer. A covered
//! prefix holding events but no published version cannot be committed by the
//! atomic writer, so it is reported unavailable rather than absent.

use tracedecay_contracts::{
    VerifiedWorkEvidenceRootV1, VerifiedWorkGraphVersionV1, WorkAttemptReceiptReadErrorV1,
    WorkAttemptReceiptReadPortV1, WorkAttemptReceiptV1, WorkEvidenceRootReadErrorV1,
    WorkEvidenceRootReadPortV1, WorkGraphReadModeV1, WorkGraphReadPortErrorV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkGraphTimelineV1, WorkGraphVersionEntryV1,
    WorkProductPortContextV1,
};
use tracedecay_domain::{
    ProjectionGenerationId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority,
    WorkGraphVersionV1, WorkProductGraphV1, WorkProductProjectionBundleV1,
    WorkProjectionSequenceV1, WorkRuntimeAttemptProjectionV1, WorkRuntimeProjectionCoverageV1,
    WorkRuntimeProjectionV1, canonical_sha256,
};

use super::journal::{
    GraphReplay, TimelineAxis, load_coverage, load_head, load_head_version, load_latest_published,
    load_published, load_timeline_window, require_current_journal, verified_graph,
};
use super::{FromStoreFault, WorkProductPublishedVersionV1, verified_version};
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
// be ignored or override the caller's, both routes answer from the same
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
    let source = storage.handle();
    require_current_journal(source).map_err(PortError::from_fault)?;
    // Events outside the selection fall outside it; they do not poison the
    // ones inside. The read is answered over the covered prefix and carries
    // the coverage that says what was left out, so a caller can never
    // mistake a slice for the whole.
    let total = load_head_version(source, scope)
        .map_err(PortError::from_fault)?
        .map_or(0, WorkGraphVersionV1::get);
    let coverage = load_coverage(source, scope, total).map_err(PortError::from_fault)?;
    let selection_coverage = coverage.disclosure().map_err(PortError::from_fault)?;
    if let Ok(covered_head) = WorkGraphVersionV1::new(coverage.covered) {
        // A covered prefix with no verified head version is a state the atomic
        // writer cannot produce, so it is damage and not an empty journal.
        load_published(source, scope, coverage.covered, covered_head)
            .map_err(PortError::from_fault)?
            .ok_or(PortError::Unavailable)?;
    }
    // The journal's head is the covered prefix's head only when nothing is
    // excluded; a narrower prefix reconstructs its own head from a checkpoint.
    let head = if coverage.first_excluded.is_none() {
        load_head(source, scope).map_err(PortError::from_fault)?
    } else {
        None
    };
    let covered = coverage.covered;
    let mut replay = GraphReplay::new(source, scope, head.as_ref());
    let mut entries = |published: Vec<WorkProductPublishedVersionV1>| {
        build_entries(
            storage,
            authority,
            &mut replay,
            published,
            request.observed_at,
        )
    };
    let absent = || WorkGraphReadV1::Absent {
        authorized_scope: scope.clone(),
        selection_coverage: selection_coverage.clone(),
    };
    // A read cannot include a version this authority had not observed at the
    // caller's observation instant. Besides preserving forensic truth, this
    // lets a prepared mutation read its former head and reach the event
    // journal's authoritative compare-and-swap conflict when a later version
    // has already committed.
    match &request.mode {
        WorkGraphReadModeV1::Current => {
            let latest = load_latest_published(source, scope, covered, request.observed_at, None)
                .map_err(PortError::from_fault)?;
            Ok(match entries(latest.into_iter().collect())?.pop() {
                Some(snapshot) => WorkGraphReadV1::Current {
                    authorized_scope: scope.clone(),
                    selection_coverage,
                    snapshot,
                },
                None => absent(),
            })
        }
        WorkGraphReadModeV1::AsOf { valid_at } => {
            let latest =
                load_latest_published(source, scope, covered, request.observed_at, Some(*valid_at))
                    .map_err(PortError::from_fault)?;
            Ok(match entries(latest.into_iter().collect())?.pop() {
                Some(snapshot) => WorkGraphReadV1::AsOf {
                    authorized_scope: scope.clone(),
                    selection_coverage,
                    snapshot,
                },
                None => absent(),
            })
        }
        WorkGraphReadModeV1::Evolution {
            from_valid_at,
            through_valid_at,
        } => {
            let window = load_timeline_window(
                source,
                scope,
                covered,
                TimelineAxis::Valid,
                (*from_valid_at, *through_valid_at),
                request.observed_at,
                WorkGraphTimelineV1::resume_key(request)?,
            )
            .map_err(PortError::from_fault)?;
            Ok(WorkGraphReadV1::Evolution {
                authorized_scope: scope.clone(),
                selection_coverage,
                timeline: WorkGraphTimelineV1::page(request, entries(window)?)?,
            })
        }
        WorkGraphReadModeV1::Forensic {
            from_observed_at,
            through_observed_at,
        } => {
            let window = load_timeline_window(
                source,
                scope,
                covered,
                TimelineAxis::Observed,
                (*from_observed_at, *through_observed_at),
                request.observed_at,
                WorkGraphTimelineV1::resume_key(request)?,
            )
            .map_err(PortError::from_fault)?;
            Ok(WorkGraphReadV1::Forensic {
                authorized_scope: scope.clone(),
                selection_coverage,
                timeline: WorkGraphTimelineV1::page(request, entries(window)?)?,
            })
        }
    }
}

/// Reconstruct and verify each selected version, ascending so neighbouring
/// versions share a fold, and build the entry carrying the graph and every
/// projection derived from that same graph.
fn build_entries(
    storage: &WorkSqliteStorage,
    authority: Option<&WorkAuthority>,
    replay: &mut GraphReplay<'_, crate::exact_sql::ExactSqlHandle>,
    mut selected: Vec<WorkProductPublishedVersionV1>,
    projected_at: UtcMicros,
) -> Result<Vec<WorkGraphVersionEntryV1>, PortError> {
    selected.sort_by_key(|version| version.graph_version);
    let mut entries = Vec::with_capacity(selected.len());
    for version in &selected {
        let graph = verified_graph(replay, version).map_err(PortError::from_fault)?;
        let verified = verified_version(version).ok_or(PortError::Unavailable)?;
        let runtime = runtime_projection(storage, authority, &graph, version, projected_at)?;
        let projections = WorkProductProjectionBundleV1::from_graph(&graph, &runtime, projected_at)
            .map_err(|_| PortError::Unavailable)?;
        entries.push(
            WorkGraphVersionEntryV1::new(
                version.valid_at,
                version.observed_at,
                projected_at,
                verified,
                graph,
                runtime,
                projections,
            )
            .map_err(|_| PortError::Unavailable)?,
        );
    }
    Ok(entries)
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
        let mut loaded = crate::work_attempt::load_attempts(storage.handle(), authority, &accepted);
        for identity in &accepted {
            match loaded.remove(identity) {
                Some(attempt) => attempts.push(WorkRuntimeAttemptProjectionV1 {
                    identity: attempt.identity().clone(),
                    state: attempt.state(),
                }),
                None => {
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
