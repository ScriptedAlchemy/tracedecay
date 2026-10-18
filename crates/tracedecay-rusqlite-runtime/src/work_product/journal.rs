//! The owner's Work product journal as storage sees it: the head projection,
//! the verified version rows, the checkpoints, the scope-set registry, and
//! bounded reconstruction.
//!
//! Nothing here loads the chain. Every query is keyed by the owner and bounded:
//!
//! * the **head** is the current-state projection, written by every append, so a
//!   current read or the next append never replays history;
//! * a **checkpoint** is stored at every [`CHECKPOINT_INTERVAL`]th version, so
//!   reconstructing any older version folds fewer than that many events, read in
//!   pages of [`EVENT_PAGE_ROWS`];
//! * a version row is a point lookup, or a window of at most
//!   [`TIMELINE_WINDOW_ROWS`] ordered by the timeline key;
//! * the **scope registry** holds one row per distinct relation-scope set the
//!   journal has seen, so where a selection's covered prefix ends is a read of a
//!   handful of rows and not a scan of events.
//!
//! The event sequence equals the graph version it produces (`Created` is 1 and
//! every change adds 1). The schema enforces that equality and reconstruction
//! relies on it.

use tracedecay_contracts::{
    AuthorizedWorkProductScopeV1, MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1, VerifiedWorkGraphVersionV1,
    WorkGraphSelectionCoverageV1, WorkProductSelectionScopeV1,
};
use tracedecay_domain::{
    UtcMicros, WorkGraphVersionV1, WorkProductAuthorizedRelationScopeV1, WorkProductEventPayloadV1,
    WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1,
    WorkProductSourceWatermarkV1, canonical_sha256,
};

use super::{
    StoreFault, WorkProductJournalEntryV1, WorkProductPublishedVersionV1, owner_params,
    recovered_graph_digest, verified_version,
};
use crate::exact_sql::{ExactSqlError, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, RegisteredWorkWrite, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

/// A checkpoint is stored at every version divisible by this.
pub(crate) const CHECKPOINT_INTERVAL: u64 = 128;

/// Events read per query while folding forward from a checkpoint. A query of
/// this many of the largest legal events stays far under the exact-SQL result
/// cap, which is the point of paging.
pub(crate) const EVENT_PAGE_ROWS: usize = 16;

/// One more than a timeline page, so a page can tell it has a successor.
pub(crate) const TIMELINE_WINDOW_ROWS: usize = MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1 + 1;

const SCOPE_KEY_DOMAIN: &str = "tracedecay.rusqlite-runtime.work-product-scope-set.v1";

/// The table a database written before the head projection holds its chain in.
/// While it exists the journal is mid-migration.
pub(super) const LEGACY_EVENTS_TABLE: &str = "work_product_events_v1";

pub(crate) fn integer(value: u64) -> Result<ExactSqlValue, StoreFault> {
    i64::try_from(value)
        .map(ExactSqlValue::Integer)
        .map_err(|_| StoreFault::Corrupt("integer"))
}

fn decoded_u64(values: &[ExactSqlValue], index: usize) -> Option<u64> {
    exact_sql_integer(values, index).and_then(|value| u64::try_from(value).ok())
}

/// The canonical identity of a relation-scope set.
pub(crate) fn scope_key(
    scopes: &[WorkProductAuthorizedRelationScopeV1],
) -> Result<String, StoreFault> {
    let mut scopes = scopes.to_vec();
    scopes.sort();
    scopes.dedup();
    canonical_sha256(&(SCOPE_KEY_DOMAIN, &scopes))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| StoreFault::Corrupt("scope set key"))
}

/// Whether this selection authorizes every relation scope an event was admitted
/// under, that is, whether this one event is inside the slice of work the
/// selection names.
///
/// `ProfileOwnedNoGit` is an explicit no-Git selection, so it covers exactly
/// the events that named no relation scope. A `Relations` selection covers any
/// event whose scopes it names, which includes the scope-free ones.
pub(crate) fn selection_covers(
    selection: &WorkProductSelectionScopeV1,
    event_scopes: &[WorkProductAuthorizedRelationScopeV1],
) -> bool {
    match selection {
        WorkProductSelectionScopeV1::ProfileOwnedNoGit => event_scopes.is_empty(),
        WorkProductSelectionScopeV1::Relations { relation_scopes } => event_scopes
            .iter()
            .all(|scope| relation_scopes.contains(scope)),
    }
}

/// Refuses every Work product operation while the pre-projection journal tables
/// still exist. A reader or writer that ran in that window would see an owner
/// with no journal and start a second chain beside the one being migrated.
pub(crate) fn require_current_journal(source: &impl RegisteredWorkQuery) -> Result<(), StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        vec![ExactSqlValue::Text(LEGACY_EVENTS_TABLE.to_owned())],
    )?;
    if rows.rows.is_empty() {
        Ok(())
    } else {
        Err(StoreFault::Sql(ExactSqlError::Busy))
    }
}

/// The journal's current state: its newest graph, as the last append left it.
#[derive(Clone, Debug)]
pub(crate) struct JournalHead {
    pub(crate) version: WorkGraphVersionV1,
    pub(crate) graph: WorkProductGraphV1,
}

pub(crate) fn load_head(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Option<JournalHead>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT head_version, head_graph FROM work_product_journal_head_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2",
        owner_params(scope),
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let corrupt = || StoreFault::Corrupt("journal head");
    let version = decoded_u64(&row.values, 0)
        .and_then(|value| WorkGraphVersionV1::new(value).ok())
        .ok_or_else(corrupt)?;
    let graph: WorkProductGraphV1 =
        serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?;
    if graph.version() != version {
        return Err(corrupt());
    }
    Ok(Some(JournalHead { version, graph }))
}

/// The head's version without its graph, for callers that only need to know how
/// far the journal has advanced.
pub(crate) fn load_head_version(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<Option<WorkGraphVersionV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT head_version FROM work_product_journal_head_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2",
        owner_params(scope),
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    decoded_u64(&row.values, 0)
        .and_then(|value| WorkGraphVersionV1::new(value).ok())
        .map(Some)
        .ok_or(StoreFault::Corrupt("journal head"))
}

/// The slice of the journal one selection covers: always a prefix.
///
/// A graph version is folded from every event up to its own sequence, so the
/// first event the selection does not cover ends the readable slice: any later
/// version would have to be folded across an event outside the selection to
/// exist at all, and that graph never existed under this selection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Coverage {
    /// Versions `1..=covered` are readable under the selection.
    pub(crate) covered: u64,
    /// The journal's length.
    pub(crate) total: u64,
    pub(crate) first_excluded: Option<WorkProductEventSequenceV1>,
}

impl Coverage {
    pub(crate) fn disclosure(self) -> Result<WorkGraphSelectionCoverageV1, StoreFault> {
        let narrow =
            |value: u64| u32::try_from(value).map_err(|_| StoreFault::Corrupt("coverage count"));
        Ok(match self.first_excluded {
            None => WorkGraphSelectionCoverageV1::Complete {
                covered_events: narrow(self.covered)?,
            },
            Some(first_excluded_sequence) => WorkGraphSelectionCoverageV1::Partial {
                covered_events: narrow(self.covered)?,
                excluded_events: narrow(self.total - self.covered)?,
                first_excluded_sequence,
            },
        })
    }
}

/// Where the selection's covered prefix ends, from the scope registry: one row
/// per distinct scope set the journal has seen, never a scan of events.
pub(crate) fn load_coverage(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    total: u64,
) -> Result<Coverage, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT relation_scopes, first_sequence FROM work_product_journal_scopes_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2",
        owner_params(scope),
    )?;
    let corrupt = || StoreFault::Corrupt("scope set row");
    let mut first_excluded: Option<u64> = None;
    for row in &rows.rows {
        let scopes: Vec<WorkProductAuthorizedRelationScopeV1> =
            serde_json::from_str(exact_sql_text(&row.values, 0).ok_or_else(corrupt)?)
                .map_err(|_| corrupt())?;
        if !selection_covers(scope.selection(), &scopes) {
            let first = decoded_u64(&row.values, 1).ok_or_else(corrupt)?;
            first_excluded = Some(first_excluded.map_or(first, |current| current.min(first)));
        }
    }
    Ok(Coverage {
        covered: first_excluded.map_or(total, |first| first - 1),
        total,
        first_excluded: first_excluded
            .map(|first| WorkProductEventSequenceV1::new(first).map_err(|_| corrupt()))
            .transpose()?,
    })
}

pub(super) const PUBLISHED_COLUMNS: &str = "graph_version, event_sequence, valid_at, observed_at, source_watermark, recovered_graph_digest";

pub(super) fn published_from_values(
    values: &[ExactSqlValue],
) -> Result<WorkProductPublishedVersionV1, StoreFault> {
    let corrupt = || StoreFault::Corrupt("published version row");
    Ok(WorkProductPublishedVersionV1 {
        graph_version: decoded_u64(values, 0)
            .and_then(|value| WorkGraphVersionV1::new(value).ok())
            .ok_or_else(corrupt)?,
        event_sequence: decoded_u64(values, 1)
            .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
            .ok_or_else(corrupt)?,
        valid_at: UtcMicros(exact_sql_integer(values, 2).ok_or_else(corrupt)?),
        observed_at: UtcMicros(exact_sql_integer(values, 3).ok_or_else(corrupt)?),
        source_watermark: serde_json::from_str::<WorkProductSourceWatermarkV1>(
            exact_sql_text(values, 4).ok_or_else(corrupt)?,
        )
        .map_err(|_| corrupt())?,
        recovered_graph_digest: exact_sql_text(values, 5).ok_or_else(corrupt)?.to_owned(),
    })
}

/// One published version, if it is inside the readable prefix.
pub(crate) fn load_published(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    covered: u64,
    version: WorkGraphVersionV1,
) -> Result<Option<WorkProductPublishedVersionV1>, StoreFault> {
    if version.get() > covered {
        return Ok(None);
    }
    let mut params = owner_params(scope);
    params.push(integer(version.get())?);
    let rows = registered_work_query(
        source,
        &format!(
            "SELECT {PUBLISHED_COLUMNS} FROM work_product_journal_versions_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND graph_version = ?3"
        ),
        params,
    )?;
    rows.rows
        .first()
        .map(|row| published_from_values(&row.values))
        .transpose()
}

/// Whether the readable prefix holds a version newer than `version`.
pub(crate) fn has_version_after(covered: u64, version: WorkGraphVersionV1) -> bool {
    covered > version.get()
}

/// The newest version in the readable prefix that this authority had observed
/// by `observed_at`, and, when `valid_at` is given, whose change was already
/// true then.
pub(crate) fn load_latest_published(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    covered: u64,
    observed_at: UtcMicros,
    valid_at: Option<UtcMicros>,
) -> Result<Option<WorkProductPublishedVersionV1>, StoreFault> {
    let mut params = owner_params(scope);
    params.extend([integer(covered)?, ExactSqlValue::Integer(observed_at.0)]);
    let valid_clause = if let Some(valid_at) = valid_at {
        params.push(ExactSqlValue::Integer(valid_at.0));
        "AND valid_at <= ?5"
    } else {
        ""
    };
    let rows = registered_work_query(
        source,
        &format!(
            "SELECT {PUBLISHED_COLUMNS} FROM work_product_journal_versions_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND graph_version <= ?3 AND observed_at <= ?4 {valid_clause}
             ORDER BY graph_version DESC
             LIMIT 1"
        ),
        params,
    )?;
    rows.rows
        .first()
        .map(|row| published_from_values(&row.values))
        .transpose()
}

/// Which instant a timeline window is cut by.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TimelineAxis {
    Valid,
    Observed,
}

/// The timeline position a continuation resumes after.
pub(crate) type TimelineKey = (UtcMicros, UtcMicros, u64);

/// At most [`TIMELINE_WINDOW_ROWS`] versions of the readable prefix inside
/// `[from, through]` on `axis` that were observed by `observed_at`, in timeline
/// order and after `resume`.
pub(crate) fn load_timeline_window(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    covered: u64,
    axis: TimelineAxis,
    (from, through): (UtcMicros, UtcMicros),
    observed_at: UtcMicros,
    resume: Option<TimelineKey>,
) -> Result<Vec<WorkProductPublishedVersionV1>, StoreFault> {
    let column = match axis {
        TimelineAxis::Valid => "valid_at",
        TimelineAxis::Observed => "observed_at",
    };
    let mut params = owner_params(scope);
    params.extend([
        integer(covered)?,
        ExactSqlValue::Integer(observed_at.0),
        ExactSqlValue::Integer(from.0),
        ExactSqlValue::Integer(through.0),
    ]);
    let resume_clause = if let Some((valid, observed, version)) = resume {
        params.extend([
            ExactSqlValue::Integer(valid.0),
            ExactSqlValue::Integer(observed.0),
            integer(version)?,
        ]);
        "AND (valid_at, observed_at, graph_version) > (?7, ?8, ?9)"
    } else {
        ""
    };
    let rows = registered_work_query(
        source,
        &format!(
            "SELECT {PUBLISHED_COLUMNS} FROM work_product_journal_versions_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND graph_version <= ?3 AND observed_at <= ?4
               AND {column} >= ?5 AND {column} <= ?6 {resume_clause}
             ORDER BY valid_at, observed_at, graph_version
             LIMIT {TIMELINE_WINDOW_ROWS}"
        ),
        params,
    )?;
    rows.rows
        .iter()
        .map(|row| published_from_values(&row.values))
        .collect()
}

/// The events with `after < sequence <= through`, oldest first, at most `limit`.
pub(crate) fn load_events_between(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    after: u64,
    through: u64,
    limit: usize,
) -> Result<Vec<WorkProductJournalEntryV1>, StoreFault> {
    let mut params = owner_params(scope);
    params.extend([integer(after)?, integer(through)?]);
    let rows = registered_work_query(
        source,
        &format!(
            "SELECT sequence, event_payload FROM work_product_journal_events_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND sequence > ?3 AND sequence <= ?4
             ORDER BY sequence
             LIMIT {limit}"
        ),
        params,
    )?;
    rows.rows
        .iter()
        .map(|row| journal_entry(&row.values))
        .collect()
}

/// Decodes a `(sequence, event_payload)` pair, refusing a payload whose own
/// sequence disagrees with the column it was stored under.
pub(crate) fn journal_entry(
    values: &[ExactSqlValue],
) -> Result<WorkProductJournalEntryV1, StoreFault> {
    let corrupt = || StoreFault::Corrupt("journal row");
    let sequence = decoded_u64(values, 0)
        .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
        .ok_or_else(corrupt)?;
    let event: WorkProductEventV1 =
        serde_json::from_str(exact_sql_text(values, 1).ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?;
    if event.sequence() != sequence || event.result_graph_version().get() != sequence.get() {
        return Err(corrupt());
    }
    Ok(WorkProductJournalEntryV1 { sequence, event })
}

/// The graph one event produces from the graph before it. `None` is a chain
/// that is not one canonical progression: a `Created` over an existing graph, a
/// change with nothing to change, a change the graph refuses, or a result whose
/// version is not the version the event recorded.
pub(crate) fn fold_event(
    prior: Option<WorkProductGraphV1>,
    event: &WorkProductEventV1,
) -> Option<WorkProductGraphV1> {
    let folded = match (prior, event.payload()) {
        (None, WorkProductEventPayloadV1::Created { graph }) => graph.clone(),
        (Some(current), WorkProductEventPayloadV1::Changed { change }) => {
            current.apply(change.as_ref().clone()).ok()?
        }
        _ => return None,
    };
    (folded.version() == event.result_graph_version()).then_some(folded)
}

fn checkpoint_before(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    target: u64,
) -> Result<Option<(u64, WorkProductGraphV1)>, StoreFault> {
    let mut params = owner_params(scope);
    params.push(integer(target)?);
    let rows = registered_work_query(
        source,
        "SELECT graph_version, graph_payload FROM work_product_journal_checkpoints_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND graph_version <= ?3
         ORDER BY graph_version DESC
         LIMIT 1",
        params,
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let corrupt = || StoreFault::Corrupt("checkpoint row");
    let version = decoded_u64(&row.values, 0).ok_or_else(corrupt)?;
    let graph: WorkProductGraphV1 =
        serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?;
    if graph.version().get() != version {
        return Err(corrupt());
    }
    Ok(Some((version, graph)))
}

/// Reconstructs graphs of the journal at ascending versions, sharing the work
/// between nearby versions.
///
/// A target is reached from the nearest of: the head, the graph already
/// reconstructed, and the newest checkpoint at or before it. Whichever it
/// starts from, it folds fewer than [`CHECKPOINT_INTERVAL`] events.
pub(crate) struct GraphReplay<'a, S: RegisteredWorkQuery> {
    source: &'a S,
    scope: &'a AuthorizedWorkProductScopeV1,
    head: Option<&'a JournalHead>,
    current: Option<(u64, WorkProductGraphV1)>,
}

impl<'a, S: RegisteredWorkQuery> GraphReplay<'a, S> {
    pub(crate) const fn new(
        source: &'a S,
        scope: &'a AuthorizedWorkProductScopeV1,
        head: Option<&'a JournalHead>,
    ) -> Self {
        Self {
            source,
            scope,
            head,
            current: None,
        }
    }

    /// The graph at `target`, verified only against the chain: the caller
    /// checks it against the version's recorded digest.
    pub(crate) fn graph_at(&mut self, target: u64) -> Result<&WorkProductGraphV1, StoreFault> {
        if let Some(head) = self.head
            && head.version.get() == target
        {
            self.current = Some((target, head.graph.clone()));
        } else {
            let hint = target - target % CHECKPOINT_INTERVAL;
            let reusable = self
                .current
                .as_ref()
                .is_some_and(|(version, _)| *version <= target && *version >= hint);
            if !reusable {
                self.current = checkpoint_before(self.source, self.scope, target)?;
            }
            self.fold_to(target)?;
        }
        self.current
            .as_ref()
            .map(|(_, graph)| graph)
            .ok_or(StoreFault::Corrupt("journal chain"))
    }

    fn fold_to(&mut self, target: u64) -> Result<(), StoreFault> {
        let broken = || StoreFault::Corrupt("journal chain");
        let mut at = self.current.as_ref().map_or(0, |(version, _)| *version);
        while at < target {
            let page = load_events_between(self.source, self.scope, at, target, EVENT_PAGE_ROWS)?;
            if page.is_empty() {
                return Err(broken());
            }
            for entry in page {
                if entry.sequence.get() != at + 1 {
                    return Err(broken());
                }
                let prior = self.current.take().map(|(_, graph)| graph);
                let graph = fold_event(prior, &entry.event).ok_or_else(broken)?;
                at = entry.sequence.get();
                self.current = Some((at, graph));
            }
        }
        if at != target {
            return Err(broken());
        }
        Ok(())
    }
}

/// The graph a published version recorded, reconstructed and checked against
/// that version's digest, so a damaged chain, checkpoint or projection fails
/// closed instead of being served.
pub(crate) fn verified_graph<S: RegisteredWorkQuery>(
    replay: &mut GraphReplay<'_, S>,
    published: &WorkProductPublishedVersionV1,
) -> Result<WorkProductGraphV1, StoreFault> {
    let graph = replay.graph_at(published.graph_version.get())?;
    let digest = recovered_graph_digest(graph).ok_or(StoreFault::Corrupt("recovered graph"))?;
    if digest.as_str() != published.recovered_graph_digest
        || graph.version() != published.graph_version
    {
        return Err(StoreFault::Corrupt("recovered graph"));
    }
    Ok(graph.clone())
}

/// One verified version of the readable prefix and its graph, or `None` when
/// the prefix never published that version.
pub(crate) fn load_verified_graph(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    covered: u64,
    version: WorkGraphVersionV1,
) -> Result<Option<(WorkProductPublishedVersionV1, WorkProductGraphV1)>, StoreFault> {
    let Some(published) = load_published(source, scope, covered, version)? else {
        return Ok(None);
    };
    let head = if load_head_version(source, scope)? == Some(version) {
        load_head(source, scope)?
    } else {
        None
    };
    let mut replay = GraphReplay::new(source, scope, head.as_ref());
    let graph = verified_graph(&mut replay, &published)?;
    Ok(Some((published, graph)))
}

/// What a caller-named verified version resolves to under the selection.
pub(crate) enum NamedVersionV1 {
    /// The journal published exactly this identity; the graph is the one that
    /// identity digests.
    Found(Box<(VerifiedWorkGraphVersionV1, WorkProductGraphV1)>),
    /// The journal never published that version and nothing newer exists.
    Absent,
    /// The caller is behind: a newer version exists, or the same version number
    /// carries a different verified identity, which is a different reading of
    /// history and is honoured exactly rather than reconciled.
    Stale,
}

pub(crate) fn resolve_named_version(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    covered: u64,
    requested: &VerifiedWorkGraphVersionV1,
) -> Result<NamedVersionV1, StoreFault> {
    let version = requested.graph_version();
    let Some((published, graph)) = load_verified_graph(source, scope, covered, version)? else {
        return Ok(if has_version_after(covered, version) {
            NamedVersionV1::Stale
        } else {
            NamedVersionV1::Absent
        });
    };
    let verified =
        verified_version(&published).ok_or(StoreFault::Corrupt("published version row"))?;
    Ok(if verified == *requested {
        NamedVersionV1::Found(Box::new((verified, graph)))
    } else {
        NamedVersionV1::Stale
    })
}

/// Why a row write into the journal did not happen.
pub(crate) enum WriteFault {
    /// The primary key or a uniqueness constraint already holds what the write
    /// claimed: another writer owns the sequence, version or command.
    Conflict,
    Store(StoreFault),
}

impl From<StoreFault> for WriteFault {
    fn from(fault: StoreFault) -> Self {
        Self::Store(fault)
    }
}

impl From<ExactSqlError> for WriteFault {
    fn from(error: ExactSqlError) -> Self {
        const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
        const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;
        match error {
            ExactSqlError::Sqlite {
                extended_code: Some(SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE),
                ..
            } => Self::Conflict,
            error => Self::Store(StoreFault::Sql(error)),
        }
    }
}

fn execute(
    transaction: &impl RegisteredWorkWrite,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> Result<usize, WriteFault> {
    Ok(transaction
        .work_execute(exact_sql_statement(sql, params)?)?
        .changed_rows)
}

fn with(
    mut params: Vec<ExactSqlValue>,
    rest: impl IntoIterator<Item = ExactSqlValue>,
) -> Vec<ExactSqlValue> {
    params.extend(rest);
    params
}

/// `owner` is the owner brain and profile parameters, `?1` and `?2`.
pub(crate) fn insert_event_row(
    transaction: &impl RegisteredWorkWrite,
    owner: &[ExactSqlValue],
    event: &WorkProductEventV1,
) -> Result<(), WriteFault> {
    let payload = serde_json::to_string(event).map_err(|_| StoreFault::Corrupt("event"))?;
    let expected = match event.expected_graph_version() {
        Some(version) => integer(version.get())?,
        None => ExactSqlValue::Null,
    };
    execute(
        transaction,
        "INSERT INTO work_product_journal_events_v1 (
            owner_brain_id, owner_profile_id, sequence, scope_key, event_id, command_id,
            canonical_input_digest, expected_graph_version, result_graph_version,
            occurred_at, event_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        with(
            owner.to_vec(),
            [
                integer(event.sequence().get())?,
                ExactSqlValue::Text(scope_key(event.authorized_relation_scopes())?),
                ExactSqlValue::Text(event.event_id().as_str().to_owned()),
                ExactSqlValue::Text(event.command_id().as_str().to_owned()),
                ExactSqlValue::Text(event.canonical_input_digest().as_str().to_owned()),
                expected,
                integer(event.result_graph_version().get())?,
                ExactSqlValue::Integer(event.occurred_at().0),
                ExactSqlValue::Text(payload),
            ],
        ),
    )
    .map(drop)
}

/// Records the event's relation-scope set the first time the journal sees it.
pub(crate) fn register_scope_set(
    transaction: &impl RegisteredWorkWrite,
    owner: &[ExactSqlValue],
    event: &WorkProductEventV1,
) -> Result<(), WriteFault> {
    let scopes = event.authorized_relation_scopes();
    execute(
        transaction,
        "INSERT OR IGNORE INTO work_product_journal_scopes_v1 (
            owner_brain_id, owner_profile_id, scope_key, relation_scopes, first_sequence
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        with(
            owner.to_vec(),
            [
                ExactSqlValue::Text(scope_key(scopes)?),
                ExactSqlValue::Text(
                    serde_json::to_string(scopes).map_err(|_| StoreFault::Corrupt("scope set"))?,
                ),
                integer(event.sequence().get())?,
            ],
        ),
    )
    .map(drop)
}

pub(crate) fn insert_version_row(
    transaction: &impl RegisteredWorkWrite,
    owner: &[ExactSqlValue],
    published: &WorkProductPublishedVersionV1,
) -> Result<(), WriteFault> {
    execute(
        transaction,
        "INSERT INTO work_product_journal_versions_v1 (
            owner_brain_id, owner_profile_id, graph_version, event_sequence,
            valid_at, observed_at, source_watermark, recovered_graph_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        with(
            owner.to_vec(),
            [
                integer(published.graph_version.get())?,
                integer(published.event_sequence.get())?,
                ExactSqlValue::Integer(published.valid_at.0),
                ExactSqlValue::Integer(published.observed_at.0),
                ExactSqlValue::Text(
                    serde_json::to_string(&published.source_watermark)
                        .map_err(|_| StoreFault::Corrupt("source watermark"))?,
                ),
                ExactSqlValue::Text(published.recovered_graph_digest.clone()),
            ],
        ),
    )
    .map(drop)
}

/// Moves the head to `graph`. An update only applies over the head the caller
/// read, so a head that moved in between is a conflict and not a lost update.
pub(crate) fn write_head_row(
    transaction: &impl RegisteredWorkWrite,
    owner: &[ExactSqlValue],
    prior: Option<WorkGraphVersionV1>,
    version: WorkGraphVersionV1,
    graph_json: &str,
) -> Result<(), WriteFault> {
    let changed = if let Some(prior) = prior {
        execute(
            transaction,
            "UPDATE work_product_journal_head_v1 SET head_version = ?3, head_graph = ?4
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND head_version = ?5",
            with(
                owner.to_vec(),
                [
                    integer(version.get())?,
                    ExactSqlValue::Text(graph_json.to_owned()),
                    integer(prior.get())?,
                ],
            ),
        )?
    } else {
        execute(
            transaction,
            "INSERT INTO work_product_journal_head_v1 (
                owner_brain_id, owner_profile_id, head_version, head_graph
             ) VALUES (?1, ?2, ?3, ?4)",
            with(
                owner.to_vec(),
                [
                    integer(version.get())?,
                    ExactSqlValue::Text(graph_json.to_owned()),
                ],
            ),
        )?
    };
    if changed == 1 {
        Ok(())
    } else {
        Err(WriteFault::Conflict)
    }
}

pub(crate) fn insert_checkpoint_row(
    transaction: &impl RegisteredWorkWrite,
    owner: &[ExactSqlValue],
    version: WorkGraphVersionV1,
    graph_json: &str,
) -> Result<(), WriteFault> {
    execute(
        transaction,
        "INSERT INTO work_product_journal_checkpoints_v1 (
            owner_brain_id, owner_profile_id, graph_version, graph_payload
         ) VALUES (?1, ?2, ?3, ?4)",
        with(
            owner.to_vec(),
            [
                integer(version.get())?,
                ExactSqlValue::Text(graph_json.to_owned()),
            ],
        ),
    )
    .map(drop)
}

/// How many versions of the journal this selection can read: the length of its
/// covered prefix.
pub(crate) fn covered_prefix_len(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
) -> Result<u64, StoreFault> {
    let total = load_head_version(source, scope)?.map_or(0, WorkGraphVersionV1::get);
    Ok(load_coverage(source, scope, total)?.covered)
}
