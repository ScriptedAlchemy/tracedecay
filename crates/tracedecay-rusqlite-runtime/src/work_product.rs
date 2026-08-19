//! The production Work product graph authority over the registered exact-SQL
//! channel.
//!
//! The application crate owns the Work product family as pure ports:
//! `WorkProductEventPortV1` atomically appends to an immutable journal and
//! records its verified graph version, while `WorkGraphReadPortV1` serves those
//! verified versions with every projection derived from the same version.
//! Until this module existed the only implementation of either authority was a
//! test double, so the whole family —
//! including the DAG, timeline, causal, workload, and critical-path projections
//! the Work views draw — had no producer at all.
//!
//! Three rules hold this adapter to what it can actually prove.
//!
//! 1. **Declared, never derived.** Item effort, causal candidates,
//!    `scheduled_at`, and `deadline` reach a projection only by having been
//!    written into a `WorkProductEventV1` payload by the caller that declared
//!    them. This module reads them back out of the journal and folds them; it
//!    never estimates one, never backfills one, and never reads one off an
//!    attempt row.
//! 2. **No cross-authority joins.** `work_attempts_v1` is scoped by
//!    [`WorkAuthority`](tracedecay_domain::WorkAuthority); the product journal
//!    is scoped by the registered profile owner. Nothing here correlates them,
//!    so a runtime reading for accepted attempts this authority cannot observe
//!    is reported as an explicit unavailable coverage rather than as a
//!    fabricated zero.
//! 3. **Only verified versions are readable.** The event and its recovered
//!    graph version commit in one transaction, so a caller can never observe an
//!    event without the graph authority that verified and digested it.

use tracedecay_application::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkGraphSelectionCoverageV1,
    WorkProductSelectionScopeV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkGraphVersionV1, WorkProductEventPayloadV1,
    WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1, canonical_sha256,
};

use crate::exact_sql::ExactSqlValue;
use crate::work::{RegisteredWorkQuery, exact_sql_integer, exact_sql_text, registered_work_query};

mod attempt_admission;
mod authorization;
mod events;
mod evidence;
mod history;
mod publication;
mod read;
mod rooted_evidence;

/// The digest domain separator for a recovered Work product graph.
pub(crate) const WORK_PRODUCT_GRAPH_DIGEST_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-graph.v1";

/// The digest domain separator for a minted Work product event identity.
pub(crate) const WORK_PRODUCT_EVENT_ID_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-event-id.v1";

/// One journal row: the durable sequence the port assigned, and the event.
#[derive(Clone, Debug)]
pub(crate) struct WorkProductJournalEntryV1 {
    pub(crate) sequence: WorkProductEventSequenceV1,
    pub(crate) event: WorkProductEventV1,
}

/// One published, verified graph version and the two instants that place it.
///
/// `valid_at` is the event's own `occurred_at` — when the change became true.
/// `observed_at` is when this authority verified and published it. They are
/// distinct on purpose: a forensic read asks about the second, an as-of read
/// about the first.
#[derive(Clone, Debug)]
pub(crate) struct WorkProductPublishedVersionV1 {
    pub(crate) graph_version: WorkGraphVersionV1,
    pub(crate) event_sequence: WorkProductEventSequenceV1,
    pub(crate) valid_at: UtcMicros,
    pub(crate) observed_at: UtcMicros,
    pub(crate) recovered_graph_digest: String,
}

pub(crate) fn owner_params(scope: &AuthorizedWorkProductScopeV1) -> Vec<ExactSqlValue> {
    vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
    ]
}

/// Whether this selection authorizes every relation scope the event was
/// admitted under — that is, whether this one event is inside the slice of work
/// the selection names.
///
/// `ProfileOwnedNoGit` is an explicit no-Git selection, so it covers exactly
/// the events that named no relation scope. A `Relations` selection covers any
/// event whose scopes it names, which includes the scope-free ones.
///
/// An event this returns `false` for is *outside* the selection. It is not a
/// defect in the journal and it does not invalidate the events that are inside:
/// see [`covered_prefix`] for what a reader does with it.
pub(crate) fn selection_covers(
    selection: &WorkProductSelectionScopeV1,
    event: &WorkProductEventV1,
) -> bool {
    match selection {
        WorkProductSelectionScopeV1::ProfileOwnedNoGit => {
            event.authorized_relation_scopes().is_empty()
        }
        WorkProductSelectionScopeV1::Relations { relation_scopes } => event
            .authorized_relation_scopes()
            .iter()
            .all(|scope| relation_scopes.contains(scope)),
    }
}

/// The readable slice of an owner's journal under one selection, and the
/// disclosure that says how much was left out.
///
/// A selection names a slice of the owner's work. Events outside it fall
/// outside the slice; they do not poison it. So the read is answered over the
/// covered slice rather than refused outright — with the caveat that a silent
/// covered slice would be worse than a refusal, which is why the coverage comes
/// back with it and every mounted read carries it through.
///
/// The slice is the journal's covered *prefix*, and that follows from folding
/// rather than from convenience. A graph version is folded from every event up
/// to its own sequence, so the first uncovered event ends the readable slice:
/// any later version would have to be folded across an event outside the
/// selection to exist at all, and that graph never existed under this
/// selection. Every event from the first uncovered one onward is therefore
/// counted as excluded, whatever scopes it named itself.
pub(crate) fn covered_prefix(
    selection: &WorkProductSelectionScopeV1,
    mut journal: Vec<WorkProductJournalEntryV1>,
) -> Option<(Vec<WorkProductJournalEntryV1>, WorkGraphSelectionCoverageV1)> {
    let total = journal.len();
    let covered = journal
        .iter()
        .position(|entry| !selection_covers(selection, &entry.event))
        .unwrap_or(total);
    let covered_events = u32::try_from(covered).ok()?;
    let Some(first_excluded_sequence) = journal.get(covered).map(|entry| entry.sequence) else {
        return Some((
            journal,
            WorkGraphSelectionCoverageV1::Complete { covered_events },
        ));
    };
    journal.truncate(covered);
    Some((
        journal,
        WorkGraphSelectionCoverageV1::Partial {
            covered_events,
            excluded_events: u32::try_from(total - covered).ok()?,
            first_excluded_sequence,
        },
    ))
}

/// One owner's journal and published versions, both bounded to the slice the
/// selection covers.
///
/// Every read that folds a graph needs the same three things — the covered
/// events, the versions folded from them alone, and the coverage disclosure —
/// so they are resolved once here rather than re-derived at each reader.
pub(crate) struct CoveredJournalV1 {
    pub(crate) journal: Vec<WorkProductJournalEntryV1>,
    pub(crate) published: Vec<WorkProductPublishedVersionV1>,
    pub(crate) coverage: WorkGraphSelectionCoverageV1,
}

/// Load the covered slice of the owner's journal and the versions readable from
/// it. `None` is an undecodable store, which every caller turns into a typed
/// unavailability rather than into an empty graph.
pub(crate) fn load_covered_journal(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Option<CoveredJournalV1> {
    let (journal, coverage) = covered_prefix(scope.selection(), load_journal(source, scope)?)?;
    let published = load_published_versions(source, scope)?
        .into_iter()
        // A version folded from an event outside the selection is not readable
        // under it: that graph never existed under this selection.
        .filter(|version| {
            coverage
                .first_excluded_sequence()
                .is_none_or(|excluded| version.event_sequence < excluded)
        })
        .collect();
    Some(CoveredJournalV1 {
        journal,
        published,
        coverage,
    })
}

/// Load the owner's whole journal in canonical sequence order.
///
/// `None` means the stored rows could not be decoded at all, which every caller
/// turns into a typed unavailability rather than into an empty journal.
pub(crate) fn load_journal(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Option<Vec<WorkProductJournalEntryV1>> {
    let rows = registered_work_query(
        source,
        "SELECT sequence, event_payload FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
         ORDER BY sequence",
        owner_params(scope),
    )
    .ok()?;
    rows.rows
        .into_iter()
        .map(|row| {
            let sequence = exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| WorkProductEventSequenceV1::new(value).ok())?;
            let event: WorkProductEventV1 =
                serde_json::from_str(exact_sql_text(&row.values, 1)?).ok()?;
            Some(WorkProductJournalEntryV1 { sequence, event })
        })
        .collect()
}

/// The owner's journal tail: the sequence and result version a new append must
/// follow. `Some(None)` is an owner with no journal at all.
#[allow(clippy::type_complexity)]
pub(crate) fn load_journal_tail(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Option<Option<(WorkProductEventSequenceV1, WorkGraphVersionV1)>> {
    let rows = registered_work_query(
        source,
        "SELECT sequence, result_graph_version FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
         ORDER BY sequence DESC LIMIT 1",
        owner_params(scope),
    )
    .ok()?;
    let Some(row) = rows.rows.first() else {
        return Some(None);
    };
    let sequence = exact_sql_integer(&row.values, 0)
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| WorkProductEventSequenceV1::new(value).ok())?;
    let version = exact_sql_integer(&row.values, 1)
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| WorkGraphVersionV1::new(value).ok())?;
    Some(Some((sequence, version)))
}

/// Load every verified graph version this owner has published, oldest first.
pub(crate) fn load_published_versions(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Option<Vec<WorkProductPublishedVersionV1>> {
    let rows = registered_work_query(
        source,
        "SELECT graph_version, event_sequence, valid_at, observed_at, recovered_graph_digest
         FROM work_product_graph_versions_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
         ORDER BY graph_version",
        owner_params(scope),
    )
    .ok()?;
    rows.rows
        .into_iter()
        .map(|row| {
            Some(WorkProductPublishedVersionV1 {
                graph_version: exact_sql_integer(&row.values, 0)
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| WorkGraphVersionV1::new(value).ok())?,
                event_sequence: exact_sql_integer(&row.values, 1)
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| WorkProductEventSequenceV1::new(value).ok())?,
                valid_at: UtcMicros(exact_sql_integer(&row.values, 2)?),
                observed_at: UtcMicros(exact_sql_integer(&row.values, 3)?),
                recovered_graph_digest: exact_sql_text(&row.values, 4)?.to_owned(),
            })
        })
        .collect()
}

/// Fold the journal into the graph at `through_sequence`.
///
/// Returns `None` when the stored chain is not one canonical progression — a
/// missing `Created` head, a gap, or a change whose folded result version does
/// not match the version the event recorded. A broken chain is never repaired
/// here and never partially folded: the caller turns it into a typed
/// unavailability, because a graph folded from part of its history is a
/// falsified graph, not a degraded one.
pub(crate) fn fold_graph(
    journal: &[WorkProductJournalEntryV1],
    through_sequence: WorkProductEventSequenceV1,
) -> Option<WorkProductGraphV1> {
    let mut graph: Option<WorkProductGraphV1> = None;
    for entry in journal {
        if entry.sequence.get() > through_sequence.get() {
            break;
        }
        let folded = match (graph.take(), entry.event.payload()) {
            (None, WorkProductEventPayloadV1::Created { graph }) => graph.clone(),
            (Some(current), WorkProductEventPayloadV1::Changed { change }) => {
                current.apply(change.as_ref().clone()).ok()?
            }
            _ => return None,
        };
        if folded.version() != entry.event.result_graph_version() {
            return None;
        }
        graph = Some(folded);
    }
    graph
}

/// The exact digest a verified version records for a folded graph.
pub(crate) fn recovered_graph_digest(graph: &WorkProductGraphV1) -> Option<ManifestDigest> {
    canonical_sha256(&(WORK_PRODUCT_GRAPH_DIGEST_DOMAIN, graph)).ok()
}

/// Rebuild the verified version identity for one published row.
pub(crate) fn verified_version(
    published: &WorkProductPublishedVersionV1,
    event: &WorkProductEventV1,
) -> Option<VerifiedWorkGraphVersionV1> {
    VerifiedWorkGraphVersionV1::new(
        published.graph_version,
        published.event_sequence,
        event.source_watermark().clone(),
        ManifestDigest::new(published.recovered_graph_digest.clone()).ok()?,
    )
    .ok()
}
