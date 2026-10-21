//! The production Work product graph authority over the registered exact-SQL
//! channel.
//!
//! The application crate owns the Work product family as pure ports:
//! `WorkProductEventPortV1` atomically appends to an immutable journal and
//! records its verified graph version, while `WorkGraphReadPortV1` serves those
//! verified versions with every projection derived from the same version.
//! This adapter is the producer for the whole family, including the DAG,
//! timeline, causal, workload, and critical-path projections the Work views
//! draw.
//!
//! Three rules hold this adapter to what it can actually prove.
//!
//! 1. **Declared, never derived.** Item effort, causal candidates,
//!    `scheduled_at`, and `deadline` reach a projection only by having been
//!    written into a `WorkProductEventV1` payload by the caller that declared
//!    them. This module reads them back out of the journal and folds them; it
//!    never estimates one, never backfills one, and never reads one off an
//!    attempt row.
//! 2. **Explicit runtime authority.** `work_attempts_v1` is scoped by
//!    [`WorkAuthority`](tracedecay_domain::WorkAuthority); the product journal
//!    is scoped by the registered profile owner. Plain product storage does not
//!    correlate them and reports unavailable coverage. A daemon route may bind
//!    its registered Work authority explicitly and then hydrates only attempts
//!    the product graph already accepted.
//! 3. **Only verified versions are readable.** The event and its recovered
//!    graph version commit in one transaction, so a caller can never observe an
//!    event without the graph authority that verified and digested it.

use tracedecay_contracts::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1, WorkGraphReadPortErrorV1, WorkGraphSelectionCoverageV1,
    WorkProductApplicationErrorV1, WorkProductAttemptAdmissionErrorV1, WorkProductEventPortErrorV1,
    WorkProductSelectionScopeV1, WorkStorageFailureV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkGraphVersionV1, WorkProductEventPayloadV1,
    WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1, canonical_sha256,
};

use crate::exact_sql::{ExactSqlError, ExactSqlReaderRefusalV1, ExactSqlValue};
use crate::work::{RegisteredWorkQuery, exact_sql_integer, exact_sql_text, registered_work_query};

mod attempt_admission;
pub(crate) use attempt_admission::{admit_retry, admit_synthesis};
mod authorization;
mod checkpoint;
mod events;
mod evidence;
mod history;
mod publication;
mod read;
mod rooted_evidence;

pub use read::AuthorizedWorkProductReadStorageV1;

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
/// `valid_at` is the event's own `occurred_at`, when the change became true.
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

/// Why a Work product storage read or write failed.
///
/// The exact SQL layer already separates busy, result-limit, expiry, authority,
/// and SQLite failures, and a row that does not decode is the one cause it
/// cannot see. This keeps both so each port can answer with the category the
/// caller needs instead of one unit `Unavailable`.
#[derive(Debug)]
pub(crate) enum StoreFault {
    /// Persisted bytes did not decode into the shape the store wrote. The
    /// label names the loader and never carries row content.
    Corrupt(&'static str),
    Sql(ExactSqlError),
}

impl std::fmt::Display for StoreFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(what) => write!(formatter, "stored {what} did not decode"),
            Self::Sql(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<ExactSqlError> for StoreFault {
    fn from(error: ExactSqlError) -> Self {
        Self::Sql(error)
    }
}

/// The answer a port gives for a [`StoreFault`], in the port's own vocabulary.
///
/// Only the expected-revision or uniqueness conflict is a `VersionConflict`,
/// and that is decided where the statement runs, never here: nothing in this
/// trait can produce a conflict, so a storage fault cannot be mistaken for
/// stale input.
pub(crate) trait FromStoreFault: Sized {
    fn storage(failure: WorkStorageFailureV1) -> Self;
    fn denied() -> Self;
    fn timed_out() -> Self;
    fn unavailable() -> Self;

    /// Classifies `fault`, and records its retained cause in tracing. The
    /// public answer carries the category only, so SQL parameters, paths, and
    /// foreign authority detail stay in the log.
    fn from_fault(fault: impl Into<StoreFault>) -> Self {
        let fault = fault.into();
        tracing::warn!(
            target: "tracedecay.work_product.storage",
            cause = %fault,
            "Work product storage failed"
        );
        match fault {
            StoreFault::Corrupt(_) => Self::storage(WorkStorageFailureV1::StoredDataInvalid),
            StoreFault::Sql(
                ExactSqlError::Busy
                | ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Saturated),
            ) => Self::storage(WorkStorageFailureV1::Busy),
            StoreFault::Sql(ExactSqlError::QueryLimitExceeded) => {
                Self::storage(WorkStorageFailureV1::ResultLimitExceeded)
            }
            StoreFault::Sql(ExactSqlError::TransactionExpired) => Self::timed_out(),
            StoreFault::Sql(
                ExactSqlError::AuthorityDenied(_) | ExactSqlError::AuthorityMismatch,
            ) => Self::denied(),
            StoreFault::Sql(_) => Self::unavailable(),
        }
    }
}

/// Classifies a failed insert into the journal or the published versions.
///
/// Only a primary-key or uniqueness violation is the expected-revision
/// conflict: another writer already holds the sequence, version, or command
/// this insert claimed. Every other failure, busy storage, a lost writer, an
/// I/O error, is the storage fault it is and never reads as stale input.
pub(crate) fn insert_fault<E: FromStoreFault>(error: ExactSqlError, conflict: E) -> E {
    const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
    const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;
    match error {
        ExactSqlError::Sqlite {
            extended_code: Some(SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE),
            ..
        } => conflict,
        error => E::from_fault(error),
    }
}

/// The ports that share the `Storage` / `NotFoundOrNotAuthorized` / `TimedOut`
/// / `Unavailable` vocabulary.
macro_rules! store_fault_port {
    ($($port:ident),+) => {$(
        impl FromStoreFault for $port {
            fn storage(failure: WorkStorageFailureV1) -> Self {
                Self::Storage(failure)
            }
            fn denied() -> Self {
                Self::NotFoundOrNotAuthorized
            }
            fn timed_out() -> Self {
                Self::TimedOut
            }
            fn unavailable() -> Self {
                Self::Unavailable
            }
        }
    )+};
}
store_fault_port!(
    WorkProductEventPortErrorV1,
    WorkGraphReadPortErrorV1,
    WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1,
    WorkProductAttemptAdmissionErrorV1
);

/// The history read answers in the application error directly.
impl FromStoreFault for WorkProductApplicationErrorV1 {
    fn storage(failure: WorkStorageFailureV1) -> Self {
        Self::Storage(failure)
    }
    fn denied() -> Self {
        Self::NotFoundOrNotAuthorized
    }
    fn timed_out() -> Self {
        Self::TimedOut
    }
    fn unavailable() -> Self {
        Self::EventAuthorityUnavailable
    }
}

pub(crate) fn owner_params(scope: &AuthorizedWorkProductScopeV1) -> Vec<ExactSqlValue> {
    vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
    ]
}

/// Whether this selection authorizes every relation scope the event was
/// admitted under, that is, whether this one event is inside the slice of work
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
/// covered slice rather than refused outright, with the caveat that a silent
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
/// Every read that folds a graph needs the same three things, the covered
/// events, the versions folded from them alone, and the coverage disclosure,
/// so they are resolved once here rather than re-derived at each reader.
pub(crate) struct CoveredJournalV1 {
    pub(crate) journal: Vec<WorkProductJournalEntryV1>,
    pub(crate) published: Vec<WorkProductPublishedVersionV1>,
    pub(crate) coverage: WorkGraphSelectionCoverageV1,
}

/// Load the covered slice of the owner's journal and the versions readable from
/// it. A [`StoreFault`] is a store that could not be read or decoded, which
/// every caller turns into its typed failure rather than into an empty graph.
pub(crate) fn load_covered_journal(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<CoveredJournalV1, StoreFault> {
    let (journal, coverage) = covered_prefix(scope.selection(), load_journal(source, scope)?)
        .ok_or(StoreFault::Corrupt("journal coverage"))?;
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
    Ok(CoveredJournalV1 {
        journal,
        published,
        coverage,
    })
}

/// Rows one journal query may return. Payload bytes are bounded separately:
/// a legal event can be much larger than the average journal row.
const JOURNAL_PAGE_ROWS: usize = 256;
const JOURNAL_PAGE_BYTES: usize = crate::exact_sql::MAX_QUERY_BYTES / 2;

/// Rows one published-version query may return.
const PUBLISHED_PAGE_ROWS: usize = 512;

/// Events between graph checkpoints. A checkpoint is written when an append
/// would otherwise fold at least this many events past its base.
pub(crate) const CHECKPOINT_INTERVAL: u64 = 256;

/// The largest sequence an exact-SQL integer parameter can carry.
const MAX_BOUND_SEQUENCE: u64 = i64::MAX.unsigned_abs();

fn cursor_value(cursor: u64, what: &'static str) -> Result<ExactSqlValue, StoreFault> {
    i64::try_from(cursor)
        .map(ExactSqlValue::Integer)
        .map_err(|_| StoreFault::Corrupt(what))
}

/// Load the owner's whole journal in canonical sequence order.
///
/// A [`StoreFault`] means the rows could not be read or decoded, which every
/// caller turns into a typed failure rather than into an empty journal.
pub(crate) fn load_journal(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Vec<WorkProductJournalEntryV1>, StoreFault> {
    load_journal_between(source, scope, 0, MAX_BOUND_SEQUENCE)
}

/// Load the journal events with `after < sequence <= through` in canonical
/// order, one bounded page at a time.
pub(crate) fn load_journal_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    after: u64,
    through: u64,
) -> Result<Vec<WorkProductJournalEntryV1>, StoreFault> {
    let mut journal = Vec::new();
    let mut cursor = after;
    loop {
        let rows = registered_work_query(
            source,
            "WITH candidates AS (
                SELECT sequence, length(CAST(event_payload AS BLOB)) AS payload_bytes
                FROM work_product_events_v1
                WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
                  AND sequence > ?3 AND sequence <= ?4
                ORDER BY sequence LIMIT ?5
             ), sized AS (
                SELECT sequence, sum(payload_bytes) OVER (ORDER BY sequence) AS page_bytes
                FROM candidates
             )
             SELECT event.sequence, event.event_payload
             FROM sized JOIN work_product_events_v1 AS event
               ON event.owner_brain_id = ?1 AND event.owner_profile_id = ?2
              AND event.sequence = sized.sequence
             WHERE sized.page_bytes <= ?6 OR sized.sequence = (SELECT min(sequence) FROM sized)
             ORDER BY event.sequence",
            owner_params(scope)
                .into_iter()
                .chain([
                    cursor_value(cursor, "journal cursor")?,
                    cursor_value(through, "journal bound")?,
                    cursor_value(JOURNAL_PAGE_ROWS as u64, "journal page")?,
                    cursor_value(JOURNAL_PAGE_BYTES as u64, "journal byte page")?,
                ])
                .collect(),
        )?;
        if rows.rows.is_empty() {
            return Ok(journal);
        }
        for row in rows.rows {
            let corrupt = || StoreFault::Corrupt("journal row");
            let sequence = exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
                .ok_or_else(corrupt)?;
            let event: WorkProductEventV1 =
                serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
                    .map_err(|_| corrupt())?;
            cursor = sequence.get();
            journal.push(WorkProductJournalEntryV1 { sequence, event });
        }
    }
}

/// A graph folded through `last`, resumed from the nearest checkpoint.
pub(crate) struct CheckpointedFoldV1 {
    pub(crate) graph: WorkProductGraphV1,
    /// The checkpoint sequence the fold resumed from; `0` means the journal
    /// head.
    pub(crate) base_sequence: u64,
    /// An affected history this fold verified before accepting the new head.
    pub(crate) invalidated_from: Option<WorkProductEventSequenceV1>,
    /// The last event folded, `None` only when the checkpoint itself is the
    /// requested sequence.
    pub(crate) last: Option<WorkProductJournalEntryV1>,
}

/// Fold the owner's graph through `through_sequence` from the latest
/// checkpoint at or before it, verifying the events after that checkpoint
/// against their publications before accepting the new event.
///
/// `Ok(None)` is a broken chain, exactly as for [`fold_graph`]. A checkpoint
/// whose payload does not recompute to its recorded version and digest is a
/// corrupt store, never a base to fold from.
pub(crate) fn fold_graph_checkpointed(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    through_sequence: WorkProductEventSequenceV1,
) -> Result<Option<CheckpointedFoldV1>, StoreFault> {
    let invalidated_from = checkpoint::invalidated_from(source, scope, through_sequence)?;
    let checkpoint_bound =
        invalidated_from.map_or(through_sequence.get(), |sequence| sequence.get() - 1);
    let (base_sequence, base_graph) = checkpoint::load(source, scope, checkpoint_bound)?
        .map_or((0, None), |checkpoint| {
            (checkpoint.sequence, Some(checkpoint.graph))
        });
    let tail = load_journal_between(source, scope, base_sequence, through_sequence.get())?;
    let published =
        load_published_versions_between(source, scope, base_sequence, through_sequence.get())?;
    let published_head = published.last().map(|version| version.event_sequence);
    let mut published = published.iter();
    let mut fold = JournalFoldV1::resume(base_graph, base_sequence, &tail);
    for entry in &tail {
        let Some(graph) = fold.advance_to(entry.sequence) else {
            return Ok(None);
        };
        // The final event may still be in its append transaction without a
        // publication. Attempt admission also folds an already published
        // final event. Every earlier event must be published, including any
        // prefix whose cache was invalidated. Ordinary appends hash only the
        // recovered published head. A historical mutation requires every
        // affected publication to agree, even if a later event overwrote the
        // changed value and left the current graph unchanged.
        match published.next() {
            None if entry.sequence == through_sequence => {}
            None => return Ok(None),
            Some(version) => {
                if version.event_sequence != entry.sequence
                    || version.graph_version != graph.version()
                    || verified_version(version, &entry.event).is_none()
                    || ((Some(entry.sequence) == published_head
                        || invalidated_from.is_some_and(|from| entry.sequence >= from))
                        && recovered_graph_digest(graph)
                            .as_ref()
                            .map(ManifestDigest::as_str)
                            != Some(version.recovered_graph_digest.as_str()))
                {
                    return Ok(None);
                }
            }
        }
    }
    if published.next().is_some() {
        return Ok(None);
    }
    let Some(graph) = fold.advance_to(through_sequence).cloned() else {
        return Ok(None);
    };
    Ok(Some(CheckpointedFoldV1 {
        graph,
        base_sequence,
        invalidated_from,
        last: tail.last().cloned(),
    }))
}

/// The owner's journal tail: the sequence and result version a new append must
/// follow. `Ok(None)` is an owner with no journal at all.
#[allow(clippy::type_complexity)]
pub(crate) fn load_journal_tail(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Option<(WorkProductEventSequenceV1, WorkGraphVersionV1)>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT sequence, result_graph_version FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
         ORDER BY sequence DESC LIMIT 1",
        owner_params(scope),
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let corrupt = || StoreFault::Corrupt("journal tail");
    let sequence = exact_sql_integer(&row.values, 0)
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
        .ok_or_else(corrupt)?;
    let version = exact_sql_integer(&row.values, 1)
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| WorkGraphVersionV1::new(value).ok())
        .ok_or_else(corrupt)?;
    Ok(Some((sequence, version)))
}

/// Load every verified graph version this owner has published, oldest first,
/// one bounded page at a time.
pub(crate) fn load_published_versions(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Vec<WorkProductPublishedVersionV1>, StoreFault> {
    load_published_versions_between(source, scope, 0, MAX_BOUND_SEQUENCE)
}

fn load_published_versions_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    after: u64,
    through: u64,
) -> Result<Vec<WorkProductPublishedVersionV1>, StoreFault> {
    let mut versions: Vec<WorkProductPublishedVersionV1> = Vec::new();
    loop {
        let cursor = versions
            .last()
            .map_or(after, |version| version.event_sequence.get());
        let rows = registered_work_query(
            source,
            "SELECT graph_version, event_sequence, valid_at, observed_at, recovered_graph_digest
             FROM work_product_graph_versions_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND event_sequence > ?3 AND event_sequence <= ?4
             ORDER BY event_sequence LIMIT ?5",
            owner_params(scope)
                .into_iter()
                .chain([
                    cursor_value(cursor, "published version cursor")?,
                    cursor_value(through, "published version bound")?,
                    cursor_value(PUBLISHED_PAGE_ROWS as u64, "published version page")?,
                ])
                .collect(),
        )?;
        let page_len = rows.rows.len();
        for row in rows.rows {
            let corrupt = || StoreFault::Corrupt("published version row");
            versions.push(WorkProductPublishedVersionV1 {
                graph_version: exact_sql_integer(&row.values, 0)
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| WorkGraphVersionV1::new(value).ok())
                    .ok_or_else(corrupt)?,
                event_sequence: exact_sql_integer(&row.values, 1)
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
                    .ok_or_else(corrupt)?,
                valid_at: UtcMicros(exact_sql_integer(&row.values, 2).ok_or_else(corrupt)?),
                observed_at: UtcMicros(exact_sql_integer(&row.values, 3).ok_or_else(corrupt)?),
                recovered_graph_digest: exact_sql_text(&row.values, 4)
                    .ok_or_else(corrupt)?
                    .to_owned(),
            });
        }
        if page_len < PUBLISHED_PAGE_ROWS {
            return Ok(versions);
        }
    }
}

/// Fold the journal into the graph at `through_sequence`.
///
/// Returns `None` when the stored chain is not one canonical progression, a
/// missing `Created` head, a gap, or a change whose folded result version does
/// not match the version the event recorded. A broken chain is never repaired
/// here and never partially folded: the caller turns it into a typed
/// unavailability, because a graph folded from part of its history is a
/// falsified graph, not a degraded one.
pub(crate) fn fold_graph(
    journal: &[WorkProductJournalEntryV1],
    through_sequence: WorkProductEventSequenceV1,
) -> Option<WorkProductGraphV1> {
    let mut fold = JournalFoldV1::new(journal);
    fold.advance_to(through_sequence)?;
    fold.graph
}

/// One forward pass over a journal that yields the graph at each requested
/// sequence, so reading several versions folds every event once instead of
/// refolding the whole prefix per version.
pub(crate) struct JournalFoldV1<'a> {
    remaining: &'a [WorkProductJournalEntryV1],
    graph: Option<WorkProductGraphV1>,
    last_sequence: u64,
}

impl<'a> JournalFoldV1<'a> {
    pub(crate) const fn new(journal: &'a [WorkProductJournalEntryV1]) -> Self {
        Self {
            remaining: journal,
            graph: None,
            last_sequence: 0,
        }
    }

    /// A fold that continues from `graph`, the verified graph a checkpoint
    /// recorded, over the events that follow it.
    pub(crate) const fn resume(
        graph: Option<WorkProductGraphV1>,
        base_sequence: u64,
        journal: &'a [WorkProductJournalEntryV1],
    ) -> Self {
        Self {
            remaining: journal,
            graph,
            last_sequence: base_sequence,
        }
    }

    /// Folds forward through `through_sequence` with the same chain rules as
    /// [`fold_graph`]. Requests must ascend and identify an exact journal row.
    pub(crate) fn advance_to(
        &mut self,
        through_sequence: WorkProductEventSequenceV1,
    ) -> Option<&WorkProductGraphV1> {
        while let Some((entry, rest)) = self.remaining.split_first() {
            if entry.sequence.get() > through_sequence.get() {
                break;
            }
            if self.last_sequence.checked_add(1) != Some(entry.sequence.get())
                || entry.event.sequence() != entry.sequence
                || entry.event.expected_graph_version()
                    != self.graph.as_ref().map(WorkProductGraphV1::version)
            {
                return None;
            }
            self.remaining = rest;
            let folded = match (self.graph.take(), entry.event.payload()) {
                (None, WorkProductEventPayloadV1::Created { graph }) => graph.clone(),
                (Some(current), WorkProductEventPayloadV1::Changed { change }) => {
                    current.apply(change.as_ref().clone()).ok()?
                }
                _ => return None,
            };
            if folded.version() != entry.event.result_graph_version() {
                return None;
            }
            self.graph = Some(folded);
            self.last_sequence = entry.sequence.get();
        }
        (self.last_sequence == through_sequence.get())
            .then_some(self.graph.as_ref())
            .flatten()
    }
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
    if published.event_sequence != event.sequence()
        || published.graph_version != event.result_graph_version()
        || published.valid_at != event.occurred_at()
        || published.observed_at < published.valid_at
    {
        return None;
    }
    VerifiedWorkGraphVersionV1::new(
        published.graph_version,
        published.event_sequence,
        event.source_watermark().clone(),
        ManifestDigest::new(published.recovered_graph_digest.clone()).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod store_fault_tests {
    use super::*;

    type Port = WorkProductEventPortErrorV1;

    fn sqlite(extended_code: i32) -> ExactSqlError {
        ExactSqlError::Sqlite {
            operation: "execute",
            code: Some(extended_code & 0xff),
            extended_code: Some(extended_code),
            message: "injected".to_owned(),
        }
    }

    #[test]
    fn only_a_primary_key_or_unique_violation_is_a_version_conflict() {
        for conflict in [1555, 2067] {
            assert_eq!(
                insert_fault(sqlite(conflict), Port::VersionConflict),
                Port::VersionConflict
            );
        }
        // CHECK, trigger abort, I/O error, busy, and a lost writer all fail
        // the insert without the revision having moved.
        for other in [275, 1811, 778, 5] {
            assert_eq!(
                insert_fault(sqlite(other), Port::VersionConflict),
                Port::Unavailable,
                "SQLite code {other} must not read as a stale revision"
            );
        }
        assert_eq!(
            insert_fault(ExactSqlError::Busy, Port::VersionConflict),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            insert_fault(ExactSqlError::WriterUnavailable, Port::VersionConflict),
            Port::Unavailable
        );
    }

    #[test]
    fn each_exact_sql_category_keeps_its_own_answer() {
        let classify = |fault: StoreFault| Port::from_fault(fault);
        assert_eq!(
            classify(ExactSqlError::Busy.into()),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            classify(ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Saturated).into()),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            classify(ExactSqlError::QueryLimitExceeded.into()),
            Port::Storage(WorkStorageFailureV1::ResultLimitExceeded)
        );
        assert_eq!(
            classify(StoreFault::Corrupt("journal row")),
            Port::Storage(WorkStorageFailureV1::StoredDataInvalid)
        );
        assert_eq!(
            classify(ExactSqlError::TransactionExpired.into()),
            Port::TimedOut
        );
        assert_eq!(
            classify(ExactSqlError::AuthorityDenied("secret".to_owned()).into()),
            Port::NotFoundOrNotAuthorized
        );
        assert_eq!(
            classify(ExactSqlError::WriterUnavailable.into()),
            Port::Unavailable
        );
        // Draining admits nothing further, so it is not capacity to wait out.
        assert_eq!(
            classify(ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Draining).into()),
            Port::Unavailable
        );
    }
}
