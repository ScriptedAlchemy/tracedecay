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
//! 4. **Each scope set is its own version line.** An event carries the relation
//!    scopes it was admitted under, and its graph version belongs to the line
//!    named by that scope set. A reader under a selection folds exactly the
//!    events whose scopes the selection names, and only those, so an event
//!    admitted under a scope the selection does not name is never folded into a
//!    graph it returns. A writer pins the head of its own line, folded only from
//!    events it is authorized to see. An unrelated scoped event cannot freeze
//!    work the caller is authorized for, and no hidden event is ever replayed
//!    into a visible response.

use tracedecay_contracts::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1, WorkGraphReadPortErrorV1, WorkProductApplicationErrorV1,
    WorkProductAttemptAdmissionErrorV1, WorkProductEventPortErrorV1, WorkProductSelectionScopeV1,
    WorkStorageFailureV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkGraphVersionV1, WorkProductEventPayloadV1,
    WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1, canonical_sha256,
};

use std::collections::BTreeMap;

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

/// Whether this selection authorizes every relation scope the event was
/// admitted under, that is, whether this one event belongs to the selection's
/// aggregate.
///
/// `ProfileOwnedNoGit` is an explicit no-Git selection, so it covers exactly
/// the events that named no relation scope. A `Relations` selection covers any
/// event whose scopes it names, which includes the scope-free ones. The
/// selections therefore form a containment lattice rather than a partition: a
/// scope-free event belongs to every aggregate, a repository-scoped event only
/// to the aggregates naming that repository.
///
/// An event this returns `false` for is not a defect in the journal and does
/// not invalidate the events that are inside: the aggregate's fold simply skips
/// it and continues, on the aggregate's own version line.
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

/// Whether an event was admitted under exactly this selection's own scopes,
/// that is, whether it is native to this selection's version line.
///
/// This is the line's identity, and it is decided by the scope set alone. Graph
/// versions are per-line counters, so the no-Git line's second event and the
/// repository line's second event are both version 2; a version number can
/// never say which line an event belongs to. A `Relations` selection also
/// covers the scope-free events, because its own events are rooted in them: a
/// task created with no Git relation is the base a repository-scoped admission
/// is written against. Those are not native to it, and once it has folded an
/// event of its own they are siblings written against a head it has moved
/// past. See [`JournalFoldV1`] for the rule that decides when a narrower event
/// is still a base and when it is a sibling.
pub(crate) fn same_line(
    selection: &WorkProductSelectionScopeV1,
    event: &WorkProductEventV1,
) -> bool {
    match selection.relation_scopes() {
        None => event.authorized_relation_scopes().is_empty(),
        Some(scopes) => {
            event.authorized_relation_scopes().len() == scopes.len()
                && event
                    .authorized_relation_scopes()
                    .iter()
                    .all(|scope| scopes.contains(scope))
        }
    }
}

/// The canonical persisted identity of one version line: the scope set an
/// event was admitted under.
///
/// A graph version number is only meaningful together with the scope set it
/// belongs to, so the event and its published version both carry this digest.
/// It is the canonical digest of the scope set rather than the relation
/// identities, so the key is stable, fixed-width, and never carries a project
/// or repository id on its own.
pub(crate) fn scope_set_key(
    scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
) -> Result<String, StoreFault> {
    // The scopes are stored canonically sorted, so the sequence digest is
    // stable for one scope set.
    canonical_sha256(&scopes.to_vec())
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| StoreFault::Corrupt("scope set key"))
}

/// The canonical persisted identity of one reader's fold.
///
/// A checkpoint caches a fold, and a fold is a function of the selection it
/// was folded under, so checkpoints are keyed by the selection's own digest.
pub(crate) fn selection_key(selection: &WorkProductSelectionScopeV1) -> Result<String, StoreFault> {
    canonical_sha256(selection)
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| StoreFault::Corrupt("selection key"))
}

/// The leading parameters every owner-wide statement binds: the owner, in
/// column order. The event journal and the invalidation marker are per owner.
pub(crate) fn owner_params(scope: &AuthorizedWorkProductScopeV1) -> Vec<ExactSqlValue> {
    vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
    ]
}

/// The leading parameters every per-line statement binds: the owner and the
/// scope set the line is named by, in column order.
pub(crate) fn line_params(
    scope: &AuthorizedWorkProductScopeV1,
    scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
) -> Result<Vec<ExactSqlValue>, StoreFault> {
    Ok(vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
        ExactSqlValue::Text(scope_set_key(scopes)?),
    ])
}

/// The leading parameters every per-selection statement binds: the owner and
/// the reader's selection, in column order.
pub(crate) fn selection_params(
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Vec<ExactSqlValue>, StoreFault> {
    Ok(vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
        ExactSqlValue::Text(selection_key(scope.selection())?),
    ])
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

/// Fold one selection's graph through `through_sequence` from the latest
/// checkpoint at or before it, verifying every covered event against the
/// publication on its own version line before accepting the new head.
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
    // Fold this selection's own chain first. A narrower event written after
    // this line took root is a sibling the fold skips, so it is not part of
    // this aggregate and its publication is not checked here.
    let mut folded = Vec::new();
    {
        let mut fold =
            JournalFoldV1::resume(scope.selection(), base_graph.clone(), base_sequence, &tail);
        for entry in &tail {
            fold.visit_through(entry.sequence)
                .ok_or(StoreFault::Corrupt("journal tail"))?;
            if fold.folded_through() == Some(entry.sequence) {
                folded.push(entry);
            }
        }
    }
    // Each folded event was published on the line its own scope set names, so
    // the publications are loaded per line and every event is checked against
    // the row that actually verified it. A line is loaded over the whole tail
    // range rather than only the sequences this fold used, because a sibling
    // the fold skipped was still published on that line and its row must not
    // be mistaken for the next folded event's.
    let mut published_by_line = BTreeMap::new();
    for entry in &folded {
        let key = scope_set_key(entry.event.authorized_relation_scopes())?;
        if published_by_line.contains_key(&key) {
            continue;
        }
        published_by_line.insert(
            key,
            load_line_versions_between(
                source,
                scope,
                entry.event.authorized_relation_scopes(),
                base_sequence,
                through_sequence.get(),
            )?,
        );
    }
    let mut fold = JournalFoldV1::resume(scope.selection(), base_graph, base_sequence, &tail);
    for entry in &folded {
        let graph = fold
            .advance_to(entry.sequence)
            .ok_or(StoreFault::Corrupt("folded event"))?;
        let line = published_by_line
            .get_mut(&scope_set_key(entry.event.authorized_relation_scopes())?)
            .ok_or(StoreFault::Corrupt("version line"))?;
        let line_head = line.last().map(|version| version.event_sequence);
        // The final event may still be in its append transaction without a
        // publication. Attempt admission also folds an already published
        // final event. Every earlier event must be published, including any
        // prefix whose cache was invalidated. Ordinary appends hash only the
        // recovered published head. A historical mutation requires every
        // affected publication to agree, even if a later event overwrote the
        // changed value and left the current graph unchanged.
        //
        // The publication is paired by its exact sequence, so a sibling this
        // fold skipped is passed over instead of being consumed as this
        // event's own row.
        let published = line
            .iter()
            .position(|version| version.event_sequence == entry.sequence);
        match published {
            None if entry.sequence == through_sequence => {}
            None => return Ok(None),
            Some(index) => {
                let version = &line[index];
                if version.graph_version != graph.version()
                    || verified_version(version, &entry.event).is_none()
                    || ((Some(entry.sequence) == line_head
                        || invalidated_from.is_some_and(|from| entry.sequence >= from))
                        && recovered_graph_digest(graph)
                            .as_ref()
                            .map(ManifestDigest::as_str)
                            != Some(version.recovered_graph_digest.as_str()))
                {
                    return Ok(None);
                }
                line.remove(index);
            }
        }
    }
    // On this selection's OWN line every publication must have been claimed by
    // the event it verified: a row left over is a published event the fold did
    // not reach, so the fold stopped short of the line's real head. A narrower
    // line legitimately keeps the rows of siblings this fold skipped, so it is
    // not held to that rule.
    let own_scopes = scope
        .selection()
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect());
    if published_by_line
        .get(&scope_set_key(&own_scopes)?)
        .is_some_and(|line| !line.is_empty())
    {
        return Ok(None);
    }
    let Some(graph) = fold.advance_to(through_sequence).cloned() else {
        return Ok(None);
    };
    // The head is the last event this selection folded, which is the event
    // being published, not the owner's last event.
    let last = folded.last().map(|entry| (*entry).clone());
    Ok(Some(CheckpointedFoldV1 {
        graph,
        base_sequence,
        invalidated_from,
        last,
    }))
}

/// The events one selection's own chain folds, and the publication that
/// verified each of them, in sequence order.
///
/// This is the one membership rule every reader shares, and it is the one
/// [`JournalFoldV1`] applies: an event outside the selection, or a narrower
/// event written after this line took root, is a sibling branch, so it is left
/// out together with its publication. Every folded event must have exactly the
/// publication that verified it, or the read fails closed.
pub(crate) fn folded_publications(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    journal: &[WorkProductJournalEntryV1],
) -> Result<
    Option<(
        Vec<WorkProductJournalEntryV1>,
        Vec<WorkProductPublishedVersionV1>,
    )>,
    StoreFault,
> {
    let mut fold = JournalFoldV1::new(scope.selection(), journal);
    let mut folded = Vec::new();
    for entry in journal {
        // A chain that cannot be followed through the journal is unverifiable,
        // the same answer a missing publication gets below.
        if fold.visit_through(entry.sequence).is_none() {
            return Ok(None);
        }
        if fold.folded_through() == Some(entry.sequence) {
            folded.push(entry.clone());
        }
    }
    let mut published_by_line = BTreeMap::new();
    for entry in &folded {
        let key = scope_set_key(entry.event.authorized_relation_scopes())?;
        if !published_by_line.contains_key(&key) {
            published_by_line.insert(
                key,
                load_line_versions(source, scope, entry.event.authorized_relation_scopes())?,
            );
        }
    }
    let mut published = Vec::with_capacity(folded.len());
    for entry in &folded {
        let line = published_by_line
            .get_mut(&scope_set_key(entry.event.authorized_relation_scopes())?)
            .ok_or(StoreFault::Corrupt("version line"))?;
        // The publication is paired by its exact sequence, so a sibling this
        // fold skipped is passed over rather than consumed as this event's
        // row. A folded event without the publication that verified it is a
        // chain this authority cannot vouch for, not a decode failure: the
        // caller answers it as unavailability, the same as any other
        // unverifiable history.
        let Some(index) = line
            .iter()
            .position(|version| version.event_sequence == entry.sequence)
        else {
            return Ok(None);
        };
        published.push(line.remove(index));
    }
    Ok(Some((folded, published)))
}

/// What a new append under one selection must follow.
pub(crate) struct JournalTailV1 {
    /// The owner's last journal sequence. The journal is one physical sequence
    /// per owner, so the next event takes `last_sequence + 1` whether or not
    /// the last event belongs to this selection's line.
    pub(crate) last_sequence: Option<WorkProductEventSequenceV1>,
    /// The result version of the last event this selection covers, which is
    /// the head of its own version line and the version a new event must
    /// expect. `None` is a selection that has folded no event yet.
    pub(crate) line_head: Option<WorkGraphVersionV1>,
}

/// The owner's journal tail and this selection's line head within it.
///
/// The two are deliberately separate. An event appended under another
/// selection advances the owner's sequence without advancing this line, so the
/// next sequence and the expected version come from different events.
pub(crate) fn load_journal_tail(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Option<JournalTailV1>, StoreFault> {
    let journal = load_journal(source, scope)?;
    // The line head is where this selection's own chain currently ends, which
    // is the fold's result, not the last covered event's: a scope-free event
    // written after this line moved past its base is a sibling the fold skips,
    // and pinning its version would blame a stale head for a sibling's sake.
    // A fold that cannot reach the journal's end is a broken chain, the same
    // `None` the checkpointed fold answers, never a stale revision.
    let mut fold = JournalFoldV1::new(scope.selection(), &journal);
    let line_head = match journal.last() {
        None => None,
        Some(last) => {
            if fold.visit_through(last.sequence).is_none() {
                return Ok(None);
            }
            fold.graph().map(WorkProductGraphV1::version)
        }
    };
    Ok(Some(JournalTailV1 {
        last_sequence: journal.last().map(|entry| entry.sequence),
        line_head,
    }))
}

/// Load every verified graph version one scope-set line has published, oldest
/// first, one bounded page at a time.
pub(crate) fn load_line_versions(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
) -> Result<Vec<WorkProductPublishedVersionV1>, StoreFault> {
    load_line_versions_between(source, scope, scopes, 0, MAX_BOUND_SEQUENCE)
}

fn load_line_versions_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
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
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND selection_key = ?3
               AND event_sequence > ?4 AND event_sequence <= ?5
             ORDER BY event_sequence LIMIT ?6",
            line_params(scope, scopes)?
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

/// Fold one selection's aggregate into the graph at `through_sequence`.
///
/// Returns `None` when the events on this selection's own line are not one
/// canonical progression, a missing `Created` head, or a change whose folded
/// result version does not match the version the event recorded. A narrower
/// event written after this line took root is a sibling branch and is
/// skipped, not an error. A broken chain is never repaired here and never
/// partially folded: the caller turns it into a typed unavailability, because a
/// graph folded from part of its history is a falsified graph, not a degraded
/// one.
pub(crate) fn fold_graph(
    selection: &WorkProductSelectionScopeV1,
    journal: &[WorkProductJournalEntryV1],
    through_sequence: WorkProductEventSequenceV1,
) -> Option<WorkProductGraphV1> {
    let mut fold = JournalFoldV1::new(selection, journal);
    fold.advance_to(through_sequence)?;
    fold.graph
}

/// One forward pass over an owner's journal that yields one selection's
/// aggregate at each requested sequence, so reading several versions folds
/// every covered event once instead of refolding the whole prefix per version.
///
/// Events the selection does not cover are skipped, not folded: the aggregate's
/// version line runs through the covered events alone, so an event admitted
/// under another scope neither advances it nor breaks its chain. A narrower
/// event is folded while it is still this line's base, and is a sibling
/// branch once this line has folded an event of its own; a sibling is skipped
/// the same way. Only an event native to this line is version-checked, and
/// only such an event that fails to match is a broken chain.
pub(crate) struct JournalFoldV1<'a> {
    selection: &'a WorkProductSelectionScopeV1,
    remaining: &'a [WorkProductJournalEntryV1],
    graph: Option<WorkProductGraphV1>,
    /// The journal sequence of the last event this aggregate folded.
    last_sequence: u64,
    /// The journal sequence through which every event has been visited,
    /// whether folded or skipped as a sibling branch.
    visited_through: u64,
    /// Whether this line has folded an event admitted under its own scopes.
    ///
    /// Until it has, the narrower events it covers are still its base. Once it
    /// has, they are siblings written against a head it has moved past.
    rooted: bool,
}

impl<'a> JournalFoldV1<'a> {
    pub(crate) const fn new(
        selection: &'a WorkProductSelectionScopeV1,
        journal: &'a [WorkProductJournalEntryV1],
    ) -> Self {
        Self {
            selection,
            remaining: journal,
            graph: None,
            last_sequence: 0,
            visited_through: 0,
            rooted: false,
        }
    }

    /// A fold that continues from `graph`, the verified graph a checkpoint
    /// recorded at `base_sequence`, over the events that follow it.
    ///
    /// A checkpoint is written only for an event this line published, so a
    /// resumed fold is rooted whenever it carries a graph.
    pub(crate) const fn resume(
        selection: &'a WorkProductSelectionScopeV1,
        graph: Option<WorkProductGraphV1>,
        base_sequence: u64,
        journal: &'a [WorkProductJournalEntryV1],
    ) -> Self {
        Self {
            selection,
            remaining: journal,
            rooted: graph.is_some(),
            graph,
            last_sequence: base_sequence,
            visited_through: base_sequence,
        }
    }

    /// The graph folded so far, `None` while the aggregate has folded nothing.
    pub(crate) const fn graph(&self) -> Option<&WorkProductGraphV1> {
        self.graph.as_ref()
    }

    /// The journal sequence of the last event this aggregate actually folded,
    /// as opposed to one it visited and skipped as a sibling branch.
    pub(crate) fn folded_through(&self) -> Option<WorkProductEventSequenceV1> {
        WorkProductEventSequenceV1::new(self.last_sequence).ok()
    }

    /// Visit every event through `through_sequence`, folding the ones on this
    /// aggregate's chain and skipping the rest. `None` is a broken chain, the
    /// same answer [`Self::advance_to`] gives; `Some(())` means the fold
    /// reached the sequence whether or not it has folded a graph yet.
    pub(crate) fn visit_through(
        &mut self,
        through_sequence: WorkProductEventSequenceV1,
    ) -> Option<()> {
        self.step_through(through_sequence)
            .filter(|reached| *reached)
            .map(|_| ())
    }

    /// Folds forward through `through_sequence` with the same chain rules as
    /// [`fold_graph`]. Requests must ascend and identify an exact journal row.
    ///
    /// A narrower event is folded while it is still this line's base. Once
    /// this line has folded an event of its own, a later narrower event was
    /// written against a head this line already moved past: it is a sibling
    /// branch, skipped and not an error, so the two lines advance
    /// independently. An event native to this line that does not match is a
    /// broken chain and fails closed.
    pub(crate) fn advance_to(
        &mut self,
        through_sequence: WorkProductEventSequenceV1,
    ) -> Option<&WorkProductGraphV1> {
        if !self.step_through(through_sequence)? {
            return None;
        }
        self.graph.as_ref()
    }

    /// The shared forward pass. `None` is a broken chain; `Some(false)` means
    /// the journal ended before `through_sequence`, so the fold never reached
    /// it; `Some(true)` means every event through it has been visited.
    ///
    /// Membership is decided by the event's own scope set, never by its
    /// version number. Graph versions are per-line counters, so the no-Git
    /// line's second event and the repository line's second event are both
    /// version 2; a version match therefore proves nothing about which line an
    /// event belongs to.
    ///
    /// A wider selection is rooted in the narrower events it covers: a
    /// repository-scoped admission is written against the no-Git graph. So a
    /// narrower event is folded while it is still this line's base, which is
    /// until this line has folded an event of its own. Once it has, the
    /// narrower line's later events were written against a head this line has
    /// moved past: they are siblings, skipped and never version-compared. Only
    /// an event admitted under exactly this selection's scopes is on its chain
    /// after that, and only such an event is version-checked.
    fn step_through(&mut self, through_sequence: WorkProductEventSequenceV1) -> Option<bool> {
        while let Some((entry, rest)) = self.remaining.split_first() {
            if entry.sequence.get() > through_sequence.get() {
                break;
            }
            self.remaining = rest;
            self.visited_through = entry.sequence.get();
            // An event outside the selection is never this line's. A narrower
            // event is its base only while this line has folded nothing of its
            // own.
            let own = same_line(self.selection, &entry.event);
            if !selection_covers(self.selection, &entry.event) || (!own && self.rooted) {
                continue;
            }
            if entry.event.sequence() != entry.sequence {
                return None;
            }
            if entry.event.expected_graph_version()
                != self.graph.as_ref().map(WorkProductGraphV1::version)
            {
                return None;
            }
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
            // Only an event admitted under this selection's own scopes takes
            // root. Folding a narrower base event leaves the line unrooted, so
            // the narrower line's next event is still its base too.
            self.rooted |= own;
        }
        // The fold is complete through the requested sequence once every event
        // up to it has been visited, whether folded or skipped as a sibling.
        Some(self.visited_through == through_sequence.get())
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
