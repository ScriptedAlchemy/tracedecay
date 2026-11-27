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
//!    scopes it was admitted under and belongs to the line that scope set
//!    names. A line's events extend one another; a line with no event yet is
//!    rooted on its widest narrower line, so its first event extends exactly
//!    one head. Ancestry is decided by journal sequence and scope-set
//!    containment, never by a version number, because two lines both publish
//!    version 2. A reader folds the chain its own line ends at, which only
//!    ever holds events its selection covers, so no hidden event is replayed
//!    into a visible response; a writer fences on the exact head it read and
//!    folds only the events since that line's checkpoint.

use tracedecay_contracts::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1, WorkGraphReadPortErrorV1, WorkProductApplicationErrorV1,
    WorkProductAttemptAdmissionErrorV1, WorkProductEventPortErrorV1, WorkProductSelectionScopeV1,
    WorkStorageFailureV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkGraphVersionV1, WorkProductAuthorizedRelationScopeV1,
    WorkProductEventPayloadV1, WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1,
    canonical_sha256,
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

/// Whether this selection authorizes every relation scope the event was
/// admitted under.
///
/// `ProfileOwnedNoGit` is an explicit no-Git selection, so it covers exactly
/// the events that named no relation scope. A `Relations` selection covers any
/// event whose scopes it names, which includes the scope-free ones. An event
/// this returns `false` for is hidden from the selection: it is never folded
/// into a graph, paired with a publication, or replayed under it.
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

/// The canonical persisted identity of one version line: the scope set an
/// event was admitted under.
///
/// A graph version number is only meaningful together with the scope set it
/// belongs to, so the event, its published version and the line's checkpoints
/// all carry this digest. It is the canonical digest of the scope set rather
/// than the relation identities, so the key is stable, fixed-width, and never
/// carries a project or repository id on its own.
pub(crate) fn scope_set_key(
    scopes: &[WorkProductAuthorizedRelationScopeV1],
) -> Result<String, StoreFault> {
    // The scopes are stored canonically sorted, so the sequence digest is
    // stable for one scope set.
    canonical_sha256(&scopes.to_vec())
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| StoreFault::Corrupt("scope set key"))
}

/// A selection names at most the request's project and its repository, so a
/// line has at most two scopes and three narrower lines. Anything wider is not
/// a scope set this authority admits, and enumerating its narrower lines is
/// refused rather than attempted.
const MAX_LINE_SCOPES: usize = 4;

/// One version line: the exact scope set its events were admitted under.
///
/// Every event belongs to exactly one line, the one its own scope set names,
/// and a selection's own line is the one its scope set names. Graph versions
/// are per-line counters, so a version number never says which line, or which
/// head on a line, an event extends. Ancestry is decided by journal sequence
/// and scope-set containment alone.
#[derive(Clone, Debug)]
pub(crate) struct VersionLineV1 {
    scopes: Vec<WorkProductAuthorizedRelationScopeV1>,
    key: String,
}

impl VersionLineV1 {
    fn of_scopes(scopes: Vec<WorkProductAuthorizedRelationScopeV1>) -> Result<Self, StoreFault> {
        let key = scope_set_key(&scopes)?;
        Ok(Self { scopes, key })
    }

    /// The line a selection writes to and reads from.
    pub(crate) fn of_selection(
        selection: &WorkProductSelectionScopeV1,
    ) -> Result<Self, StoreFault> {
        Self::of_scopes(
            selection
                .relation_scopes()
                .map_or_else(Vec::new, |relations| relations.iter().cloned().collect()),
        )
    }

    /// The line an event was admitted on.
    pub(crate) fn of_event(event: &WorkProductEventV1) -> Result<Self, StoreFault> {
        Self::of_scopes(event.authorized_relation_scopes().to_vec())
    }

    pub(crate) fn scopes(&self) -> &[WorkProductAuthorizedRelationScopeV1] {
        &self.scopes
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Every line whose scope set is a proper subset of this one's: the lines
    /// this line can be rooted on, and the only other lines a reader under it
    /// is authorized to see.
    fn narrower(&self) -> Result<Vec<Self>, StoreFault> {
        let count = self.scopes.len();
        if count > MAX_LINE_SCOPES {
            return Err(StoreFault::Corrupt("version line scope set"));
        }
        (0_u32..(1_u32 << count) - 1)
            .map(|mask| {
                Self::of_scopes(
                    self.scopes
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| mask & (1 << index) != 0)
                        .map(|(_, scope)| scope.clone())
                        .collect(),
                )
            })
            .collect()
    }
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
/// line's scope-set key, in column order.
pub(crate) fn line_params(
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
) -> Vec<ExactSqlValue> {
    owner_params(scope)
        .into_iter()
        .chain([ExactSqlValue::Text(line.key.clone())])
        .collect()
}

/// Rows one journal query may return. Payload bytes are bounded separately:
/// a legal event can be much larger than the average journal row.
const JOURNAL_PAGE_ROWS: usize = 256;
const JOURNAL_PAGE_BYTES: usize = crate::exact_sql::MAX_QUERY_BYTES / 2;

/// Rows one published-version query may return.
const PUBLISHED_PAGE_ROWS: usize = 512;

/// A line's own events between graph checkpoints. A checkpoint is written
/// when an append would otherwise fold at least this many of the line's own
/// events past its base, so an append folds at most this many.
pub(crate) const CHECKPOINT_INTERVAL: u64 = 256;

/// The largest sequence an exact-SQL integer parameter can carry.
const MAX_BOUND_SEQUENCE: u64 = i64::MAX.unsigned_abs();

fn cursor_value(cursor: u64, what: &'static str) -> Result<ExactSqlValue, StoreFault> {
    i64::try_from(cursor)
        .map(ExactSqlValue::Integer)
        .map_err(|_| StoreFault::Corrupt(what))
}

fn stored_sequence(
    value: Option<i64>,
    what: &'static str,
) -> Result<WorkProductEventSequenceV1, StoreFault> {
    value
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
        .ok_or(StoreFault::Corrupt(what))
}

/// Load the owner's whole journal in canonical sequence order.
///
/// A [`StoreFault`] means the rows could not be read or decoded, which every
/// caller turns into a typed failure rather than into an empty journal.
pub(crate) fn load_journal(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Vec<WorkProductJournalEntryV1>, StoreFault> {
    load_journal_between(source, scope, None, 0, MAX_BOUND_SEQUENCE)
}

/// Load the journal events with `after < sequence <= through` in canonical
/// order, one bounded page at a time, either the owner's whole journal or
/// only the events of one line.
///
/// An event whose stored line key does not name its own decoded scope set is
/// corrupt: the line is read by the key, so a payload that disagrees with it
/// would otherwise be folded into a line it was never admitted on.
fn load_journal_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: Option<&VersionLineV1>,
    after: u64,
    through: u64,
) -> Result<Vec<WorkProductJournalEntryV1>, StoreFault> {
    let mut journal = Vec::new();
    let mut cursor = after;
    let filter = if line.is_some() {
        "AND selection_key = ?7"
    } else {
        "AND ?7 IS NULL"
    };
    loop {
        let rows = registered_work_query(
            source,
            &format!(
                "WITH candidates AS (
                    SELECT sequence, length(CAST(event_payload AS BLOB)) AS payload_bytes
                    FROM work_product_events_v1
                    WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
                      AND sequence > ?3 AND sequence <= ?4 {filter}
                    ORDER BY sequence LIMIT ?5
                 ), sized AS (
                    SELECT sequence, sum(payload_bytes) OVER (ORDER BY sequence) AS page_bytes
                    FROM candidates
                 )
                 SELECT event.sequence, event.event_payload, event.selection_key
                 FROM sized JOIN work_product_events_v1 AS event
                   ON event.owner_brain_id = ?1 AND event.owner_profile_id = ?2
                  AND event.sequence = sized.sequence
                 WHERE sized.page_bytes <= ?6 OR sized.sequence = (SELECT min(sequence) FROM sized)
                 ORDER BY event.sequence"
            ),
            owner_params(scope)
                .into_iter()
                .chain([
                    cursor_value(cursor, "journal cursor")?,
                    cursor_value(through, "journal bound")?,
                    cursor_value(JOURNAL_PAGE_ROWS as u64, "journal page")?,
                    cursor_value(JOURNAL_PAGE_BYTES as u64, "journal byte page")?,
                    line.map_or(ExactSqlValue::Null, |line| {
                        ExactSqlValue::Text(line.key.clone())
                    }),
                ])
                .collect(),
        )?;
        if rows.rows.is_empty() {
            return Ok(journal);
        }
        for row in rows.rows {
            let corrupt = || StoreFault::Corrupt("journal row");
            let sequence = stored_sequence(exact_sql_integer(&row.values, 0), "journal row")?;
            let event: WorkProductEventV1 =
                serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
                    .map_err(|_| corrupt())?;
            if exact_sql_text(&row.values, 2) != Some(VersionLineV1::of_event(&event)?.key()) {
                return Err(StoreFault::Corrupt("journal row line"));
            }
            cursor = sequence.get();
            journal.push(WorkProductJournalEntryV1 { sequence, event });
        }
    }
}

/// The owner's last journal sequence. The journal is one physical sequence
/// per owner, so the next event takes the one after it whichever line it is on.
pub(crate) fn last_owner_sequence(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Option<WorkProductEventSequenceV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT sequence FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
         ORDER BY sequence DESC LIMIT 1",
        owner_params(scope),
    )?;
    rows.rows
        .first()
        .map(|row| stored_sequence(exact_sql_integer(&row.values, 0), "journal tail"))
        .transpose()
}

/// The event a chain ends at: its line, its journal sequence, and the graph
/// version it recorded.
#[derive(Clone, Debug)]
pub(crate) struct ChainHeadV1 {
    pub(crate) line: VersionLineV1,
    pub(crate) sequence: WorkProductEventSequenceV1,
    pub(crate) graph_version: WorkGraphVersionV1,
}

/// The last event `line` holds before `before`, read from the line's index.
fn last_on_line(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
    before: u64,
) -> Result<Option<ChainHeadV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT sequence, result_graph_version FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND selection_key = ?3
           AND sequence < ?4
         ORDER BY sequence DESC LIMIT 1",
        line_params(scope, line)
            .into_iter()
            .chain([cursor_value(before, "line head bound")?])
            .collect(),
    )?;
    rows.rows
        .first()
        .map(|row| decode_chain_head(line, &row.values))
        .transpose()
}

fn decode_chain_head(
    line: &VersionLineV1,
    values: &[ExactSqlValue],
) -> Result<ChainHeadV1, StoreFault> {
    Ok(ChainHeadV1 {
        line: line.clone(),
        sequence: stored_sequence(exact_sql_integer(values, 0), "line head")?,
        graph_version: exact_sql_integer(values, 1)
            .and_then(|value| u64::try_from(value).ok())
            .and_then(|value| WorkGraphVersionV1::new(value).ok())
            .ok_or(StoreFault::Corrupt("line head"))?,
    })
}

/// A reader's head must have been published by its observation instant. An
/// exact-version read also stops at the requested event, inclusively, so a
/// later sibling with the same observation timestamp cannot replace it.
fn last_observed_on_line(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
    observed_at: UtcMicros,
    through: u64,
) -> Result<Option<ChainHeadV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT event.sequence, event.result_graph_version
         FROM work_product_events_v1 AS event
         LEFT JOIN work_product_graph_versions_v1 AS published
           ON published.owner_brain_id = event.owner_brain_id
          AND published.owner_profile_id = event.owner_profile_id
          AND published.selection_key = event.selection_key
          AND published.event_sequence = event.sequence
         WHERE event.owner_brain_id = ?1 AND event.owner_profile_id = ?2
           AND event.selection_key = ?3 AND event.sequence <= ?4
           AND (published.observed_at IS NULL OR published.observed_at <= ?5)
         ORDER BY event.sequence DESC LIMIT 1",
        line_params(scope, line)
            .into_iter()
            .chain([
                cursor_value(through, "observed line head bound")?,
                ExactSqlValue::Integer(observed_at.0),
            ])
            .collect(),
    )?;
    // A missing publication remains a candidate: load_chain must refuse the
    // unverifiable event instead of silently returning an older graph.
    rows.rows
        .first()
        .map(|row| decode_chain_head(line, &row.values))
        .transpose()
}

/// The first event `line` holds at or before `through`.
fn first_on_line(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
    through: u64,
) -> Result<Option<WorkProductEventSequenceV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT sequence FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND selection_key = ?3
           AND sequence <= ?4
         ORDER BY sequence LIMIT 1",
        line_params(scope, line)
            .into_iter()
            .chain([cursor_value(through, "line root bound")?])
            .collect(),
    )?;
    rows.rows
        .first()
        .map(|row| stored_sequence(exact_sql_integer(&row.values, 0), "line root"))
        .transpose()
}

/// The head `line`'s chain ends at among the events before `before`.
///
/// A line that holds an event ends at its own last event: its events extend
/// one another, and a narrower event written after it took root is a sibling
/// written against a head it has moved past. A line that holds no event yet is
/// rooted on its WIDEST narrower line that holds one, the newest of equally
/// wide ones: a repository line extends the no-Git line it covers, and a
/// project-and-repository line extends the repository line, never a fold of
/// two siblings that each expected the same base. `None` is a line with no
/// covered event at all.
///
/// This one rule is both what a writer's compare-and-swap fences on and what
/// every reader reconstructs, so the base a first write extended is always
/// exactly the chain readers fold beneath it.
pub(crate) fn chain_head(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
    before: u64,
) -> Result<Option<ChainHeadV1>, StoreFault> {
    select_chain_head(line, |candidate| {
        last_on_line(source, scope, candidate, before)
    })
}

fn select_chain_head(
    line: &VersionLineV1,
    mut last: impl FnMut(&VersionLineV1) -> Result<Option<ChainHeadV1>, StoreFault>,
) -> Result<Option<ChainHeadV1>, StoreFault> {
    if let Some(own) = last(line)? {
        return Ok(Some(own));
    }
    let mut root: Option<ChainHeadV1> = None;
    for narrower in line.narrower()? {
        if let Some(head) = last(&narrower)? {
            let rank = |head: &ChainHeadV1| (head.line.scopes.len(), head.sequence.get());
            if root
                .as_ref()
                .is_none_or(|current| rank(&head) > rank(current))
            {
                root = Some(head);
            }
        }
    }
    Ok(root)
}

/// The events one chain folds, in sequence order, each paired with the
/// publication that verified it, and the checkpoint it resumes from.
pub(crate) struct ChainV1 {
    /// The verified graph a writer's fold resumes from. A reader always folds
    /// from the chain's root and never carries one.
    pub(crate) base: Option<checkpoint::GraphCheckpoint>,
    pub(crate) entries: Vec<WorkProductJournalEntryV1>,
    pub(crate) published: Vec<WorkProductPublishedVersionV1>,
}

impl ChainV1 {
    const fn empty() -> Self {
        Self {
            base: None,
            entries: Vec::new(),
            published: Vec::new(),
        }
    }
}

/// Load the chain that ends at `head`, walking down from each line's first
/// event to the head it was rooted on.
///
/// With `resume_through`, the walk stops at the newest checkpoint of a line at
/// or before that sequence, so a writer reads only the events since it. A
/// reader passes `None` and loads the chain from its root.
///
/// `Ok(None)` is a chain this authority cannot vouch for: a line whose
/// publications in the walked range are not exactly its events.
fn load_chain(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    head: &ChainHeadV1,
    resume_through: Option<u64>,
) -> Result<Option<ChainV1>, StoreFault> {
    let mut segments = Vec::new();
    let mut base = None;
    let mut current = head.clone();
    loop {
        let through = current.sequence.get();
        if let Some(bound) = resume_through
            && let Some(checkpoint) =
                checkpoint::load(source, scope, &current.line, through.min(bound))?
        {
            segments.push((current.line, checkpoint.sequence, through));
            base = Some(checkpoint);
            break;
        }
        let root = first_on_line(source, scope, &current.line, through)?
            .ok_or(StoreFault::Corrupt("version line root"))?;
        segments.push((current.line.clone(), root.get() - 1, through));
        match chain_head(source, scope, &current.line, root.get())? {
            Some(rooted_on) => current = rooted_on,
            None => break,
        }
    }
    let mut chain = ChainV1 {
        base,
        ..ChainV1::empty()
    };
    for (line, after, through) in segments.into_iter().rev() {
        let entries = load_journal_between(source, scope, Some(&line), after, through)?;
        let published = load_line_versions_between(source, scope, &line, after, through)?;
        // A line's publications in this range are exactly its events in it:
        // a missing row is an event nobody verified, and a spare row is a
        // published event the walk did not reach.
        if entries.len() != published.len()
            || entries
                .iter()
                .zip(&published)
                .any(|(entry, version)| entry.sequence != version.event_sequence)
        {
            return Ok(None);
        }
        chain.entries.extend(entries);
        chain.published.extend(published);
    }
    Ok(Some(chain))
}

/// The chain a reader under this selection folds: every event of its own
/// line, and beneath them the chain it was rooted on. An event this selection
/// does not cover is never on it, so no hidden event is ever folded into, or
/// paired with a publication for, a response under this selection.
/// The observation and optional exact-event bound choose only the top-level
/// head; its ancestry remains fixed by the sequence bounds used by writers.
pub(crate) fn selection_chain(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    observed_at: UtcMicros,
    through: Option<WorkProductEventSequenceV1>,
) -> Result<Option<ChainV1>, StoreFault> {
    let line = VersionLineV1::of_selection(scope.selection())?;
    let through = through.map_or(MAX_BOUND_SEQUENCE, |sequence| sequence.get());
    let head = select_chain_head(&line, |candidate| {
        last_observed_on_line(source, scope, candidate, observed_at, through)
    })?;
    match head {
        None => Ok(Some(ChainV1::empty())),
        Some(head) => load_chain(source, scope, &head, None),
    }
}

/// A graph folded through one chain head, resumed from the nearest checkpoint.
pub(crate) struct CheckpointedFoldV1 {
    pub(crate) graph: WorkProductGraphV1,
    /// How many of the head line's own events the fold applied past its
    /// checkpoint, or past the line's root when no checkpoint covers it.
    pub(crate) own_since_base: u64,
    /// The digest the head's publication recorded, which the fold recomputed.
    pub(crate) head_digest: String,
}

/// Fold the chain ending at `head` from the newest checkpoint before any
/// invalidated history, verifying every folded event against the publication
/// on its own line. A writer reads only the events since that checkpoint, so
/// its cost is bounded by the checkpoint interval and not by the retained
/// journal.
///
/// `Ok(None)` is a broken or unverifiable chain. A checkpoint whose payload
/// does not recompute to its recorded version and digest is a corrupt store,
/// never a base to fold from.
pub(crate) fn fold_chain_checkpointed(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    head: &ChainHeadV1,
    invalidated_from: Option<WorkProductEventSequenceV1>,
) -> Result<Option<CheckpointedFoldV1>, StoreFault> {
    let resume_through = invalidated_from.map_or(MAX_BOUND_SEQUENCE, |sequence| sequence.get() - 1);
    let Some(chain) = load_chain(source, scope, head, Some(resume_through))? else {
        return Ok(None);
    };
    let own_since_base = chain
        .entries
        .iter()
        .filter(|entry| entry.event.authorized_relation_scopes() == head.line.scopes())
        .count() as u64;
    let mut base_digest = None;
    let base = match chain.base {
        Some(checkpoint) => {
            base_digest = Some(checkpoint.digest);
            Some((
                WorkProductEventSequenceV1::new(checkpoint.sequence)
                    .map_err(|_| StoreFault::Corrupt("graph checkpoint row"))?,
                checkpoint.graph,
            ))
        }
        None => None,
    };
    let mut fold = JournalFoldV1::resume(base, &chain.entries);
    for (entry, version) in chain.entries.iter().zip(&chain.published) {
        let Some(graph) = fold.advance_to(entry.sequence) else {
            return Ok(None);
        };
        // Ordinary appends hash only the recovered published head. A
        // historical mutation requires every affected publication to agree,
        // even if a later event overwrote the changed value and left the
        // current graph unchanged.
        if version.graph_version != graph.version()
            || verified_version(version, &entry.event).is_none()
            || ((entry.sequence == head.sequence
                || invalidated_from.is_some_and(|from| entry.sequence >= from))
                && recovered_graph_digest(graph)
                    .as_ref()
                    .map(ManifestDigest::as_str)
                    != Some(version.recovered_graph_digest.as_str()))
        {
            return Ok(None);
        }
    }
    let head_digest = match chain.published.last() {
        Some(version) => version.recovered_graph_digest.clone(),
        // The checkpoint is the head itself, and its digest was checked
        // against the head's publication when it was loaded.
        None => base_digest.ok_or(StoreFault::Corrupt("chain head"))?,
    };
    let Some(graph) = fold.advance_to(head.sequence).cloned() else {
        return Ok(None);
    };
    if graph.version() != head.graph_version {
        return Ok(None);
    }
    Ok(Some(CheckpointedFoldV1 {
        graph,
        own_since_base,
        head_digest,
    }))
}

/// Load the verified graph versions one line published with
/// `after < event_sequence <= through`, oldest first, one bounded page at a
/// time.
pub(crate) fn load_line_versions_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    line: &VersionLineV1,
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
            line_params(scope, line)
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
                event_sequence: stored_sequence(
                    exact_sql_integer(&row.values, 1),
                    "published version row",
                )?,
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

/// Fold one chain into its graph at `through_sequence`, `None` when the chain
/// is not one canonical progression: a missing `Created` root, or a change
/// whose expected or folded result version does not match the version the
/// event recorded. A broken chain is never repaired here and never partially
/// folded: the caller turns it into a typed unavailability, because a graph
/// folded from part of its history is a falsified graph, not a degraded one.
pub(crate) fn fold_graph(
    chain: &[WorkProductJournalEntryV1],
    through_sequence: WorkProductEventSequenceV1,
) -> Option<WorkProductGraphV1> {
    let mut fold = JournalFoldV1::new(chain);
    fold.advance_to(through_sequence).cloned()
}

/// One forward pass over a chain that yields its graph at each requested
/// sequence, so reading several versions folds every event once instead of
/// refolding the whole prefix per version.
///
/// Every event on a chain extends the one before it, so every event is
/// version-checked against it. Which events are on the chain is decided
/// before folding, by journal sequence and scope set; the fold never infers
/// membership from a version number.
pub(crate) struct JournalFoldV1<'a> {
    remaining: &'a [WorkProductJournalEntryV1],
    graph: Option<WorkProductGraphV1>,
    /// The journal sequence of the last event folded, or of the checkpoint
    /// the fold resumed from.
    last_sequence: Option<WorkProductEventSequenceV1>,
}

impl<'a> JournalFoldV1<'a> {
    /// A fold over `chain` from its root, the chain's `Created` event.
    pub(crate) const fn new(chain: &'a [WorkProductJournalEntryV1]) -> Self {
        Self {
            remaining: chain,
            graph: None,
            last_sequence: None,
        }
    }

    /// A fold over `chain` that continues from `base`, the verified graph a
    /// checkpoint recorded at the given sequence, just before the chain's
    /// first event.
    pub(crate) fn resume(
        base: Option<(WorkProductEventSequenceV1, WorkProductGraphV1)>,
        chain: &'a [WorkProductJournalEntryV1],
    ) -> Self {
        let (last_sequence, graph) = base.map_or((None, None), |(sequence, graph)| {
            (Some(sequence), Some(graph))
        });
        Self {
            remaining: chain,
            graph,
            last_sequence,
        }
    }

    /// Folds forward through `through_sequence`, which must be an exact chain
    /// event, or the checkpoint the fold resumed from, at or after the last
    /// one folded. `None` is a broken chain or a sequence the chain does not
    /// hold.
    pub(crate) fn advance_to(
        &mut self,
        through_sequence: WorkProductEventSequenceV1,
    ) -> Option<&WorkProductGraphV1> {
        while let Some((entry, rest)) = self.remaining.split_first() {
            if entry.sequence.get() > through_sequence.get() {
                break;
            }
            self.remaining = rest;
            if entry.event.sequence() != entry.sequence
                || entry.event.expected_graph_version()
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
            self.last_sequence = Some(entry.sequence);
        }
        if self.last_sequence != Some(through_sequence) {
            return None;
        }
        self.graph.as_ref()
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
