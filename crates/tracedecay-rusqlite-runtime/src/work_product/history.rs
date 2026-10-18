//! The owner's Work product history: the journaled events themselves, paged in
//! durable sequence order.
//!
//! History is the one Work product read that returns no projection at all. The
//! events are the record, so this authority hands back the stored
//! `WorkProductEventV1` rows unchanged, it does not summarise them, does not
//! reorder them, and never synthesises an event that was not appended.
//!
//! Three bounds shape what a page contains.
//!
//! 1. **The selection bounds the journal to its covered prefix.** An event
//!    records the relation scopes it was admitted under, and an event outside
//!    the selection falls outside the slice the read was authorized over, it
//!    does not poison the events inside it. So the page is served over the
//!    covered prefix (see [`Coverage`](super::journal::Coverage) for why the
//!    covered slice is always a prefix) and carries a
//!    [`WorkGraphSelectionCoverageV1`](tracedecay_contracts::WorkGraphSelectionCoverageV1)
//!    naming what lies beyond it. Filtering silently would falsify the
//!    authorized slice; disclosing the boundary is what makes serving it
//!    honest. Within that prefix the admitted scopes must still match the
//!    selection exactly, so an event a narrower selection would have to
//!    reinterpret is refused rather than reshaped.
//! 2. **Nothing later than the read instant is history yet.** Events whose
//!    `occurred_at` is after the request's `observed_at` are outside the read's
//!    own temporal bound and are not returned. `Complete` therefore means
//!    complete as of `observed_at`, which is the only completeness a
//!    point-in-time read can claim.
//! 3. **Pages resume by sequence, not by offset.** The continuation names the
//!    durable sequence the previous page ended on, so an event appended between
//!    two pages cannot shift a caller past an event it never saw. A page reads
//!    at most one row more than its limit, and stops early at a byte budget
//!    with a continuation, so no stored history makes a single result too big.

use tracedecay_contracts::{
    OpaqueCursor, WorkHistoryCoverageV1, WorkHistoryReadPortV1, WorkHistoryRequestV1,
    WorkHistoryV1, WorkProductApplicationErrorV1, WorkProductAuthorizedRelationScopeV1,
    WorkProductPortContextV1, WorkProductSelectionScopeV1,
};

use super::journal::{
    integer, journal_entry, load_coverage, load_head_version, require_current_journal,
};
use super::{FromStoreFault, owner_params};
use crate::exact_sql::ExactSqlValue;
use crate::work::{WorkSqliteStorage, exact_sql_text, registered_work_query};

type HistoryError = WorkProductApplicationErrorV1;

const HISTORY_CURSOR_PREFIX: &str = "work-product-event-sequence:";

/// Events read per query.
const HISTORY_QUERY_ROWS: usize = 16;

/// Stored payload bytes one page reads before it stops and continues, well under
/// the exact-SQL result cap, so history of any size pages without an oversized
/// result.
const HISTORY_PAGE_BYTES: usize = 8 * 1024 * 1024;

impl WorkHistoryReadPortV1 for WorkSqliteStorage {
    fn read_history(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, HistoryError> {
        let scope = context.authorized_scope();
        let source = self.handle();
        require_current_journal(source).map_err(HistoryError::from_fault)?;
        let total = load_head_version(source, scope)
            .map_err(HistoryError::from_fault)?
            .map_or(0, tracedecay_domain::WorkGraphVersionV1::get);
        // Events outside the selection fall outside it; they do not poison the
        // ones inside. The page is served over the covered prefix and carries
        // the disclosure that says what was left out, so a caller can never
        // mistake a slice of the journal for the whole of it.
        let coverage = load_coverage(source, scope, total).map_err(HistoryError::from_fault)?;
        let selection_coverage = coverage.disclosure().map_err(HistoryError::from_fault)?;
        let mut cursor = resume_from(request.continuation.as_ref())?;
        let limit = usize::try_from(request.limit).map_err(|_| HistoryError::InvalidRequest)?;
        let mut events = Vec::new();
        let mut payload_bytes = 0;
        let mut exhausted = false;
        // One more event than the limit is read, so the page knows whether a
        // successor exists, in queries of a few rows and no more bytes than the
        // page budget, so no stored history can make a single result too big.
        while events.len() <= limit && payload_bytes < HISTORY_PAGE_BYTES {
            let mut params = owner_params(scope);
            params.extend([
                integer(cursor).map_err(HistoryError::from_fault)?,
                integer(coverage.covered).map_err(HistoryError::from_fault)?,
                ExactSqlValue::Integer(request.observed_at.0),
            ]);
            let rows = registered_work_query(
                source,
                &format!(
                    "SELECT sequence, event_payload FROM work_product_journal_events_v1
                     WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
                       AND sequence > ?3 AND sequence <= ?4 AND occurred_at <= ?5
                     ORDER BY sequence
                     LIMIT {}",
                    HISTORY_QUERY_ROWS.min(limit + 1 - events.len())
                ),
                params,
            )
            .map_err(HistoryError::from_fault)?;
            if rows.rows.is_empty() {
                exhausted = true;
                break;
            }
            for row in &rows.rows {
                let entry = journal_entry(&row.values).map_err(HistoryError::from_fault)?;
                payload_bytes += exact_sql_text(&row.values, 1).map_or(0, str::len);
                cursor = entry.sequence.get();
                events.push(entry.event);
            }
        }
        // Inside the prefix the admitted scopes must still be exactly this
        // selection's. A covering selection admits an event whose scopes it
        // merely contains, but history returns the stored event unchanged, and
        // an event admitted under fewer scopes than the read claims is not one
        // this authority may re-present under them.
        let authorized = selected_relation_scopes(scope.selection());
        if events
            .iter()
            .any(|event| event.authorized_relation_scopes() != authorized.as_slice())
        {
            return Err(HistoryError::NotFoundOrNotAuthorized);
        }
        // Complete only when every covered row was consumed and fits the limit.
        // A page that stopped on its byte budget cannot know that nothing
        // follows, so it hands back a continuation, and the next page may be
        // empty.
        let complete = exhausted && events.len() <= limit;
        events.truncate(limit);
        let coverage = if complete {
            WorkHistoryCoverageV1::Complete {
                returned: u32::try_from(events.len())
                    .map_err(|_| HistoryError::EventAuthorityUnavailable)?,
            }
        } else {
            let last = events
                .last()
                .map(|event| event.sequence().get())
                .ok_or(HistoryError::EventAuthorityUnavailable)?;
            WorkHistoryCoverageV1::Partial {
                returned: u32::try_from(events.len())
                    .map_err(|_| HistoryError::EventAuthorityUnavailable)?,
                continuation: OpaqueCursor::new(format!("{HISTORY_CURSOR_PREFIX}{last}"))
                    .map_err(|_| HistoryError::EventAuthorityUnavailable)?,
            }
        };
        Ok(WorkHistoryV1 {
            authorized_scope: scope.clone(),
            events,
            coverage,
            selection_coverage,
        })
    }
}

/// The exact relation scopes an event admitted under this selection must carry.
///
/// This mirrors the set the application re-derives when it checks the answer,
/// so an event that would fail that check is never returned in the first place.
fn selected_relation_scopes(
    selection: &WorkProductSelectionScopeV1,
) -> Vec<WorkProductAuthorizedRelationScopeV1> {
    selection
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect())
}

/// The durable sequence a continuation resumes after. A cursor this authority
/// did not mint is refused rather than treated as "start from the beginning".
fn resume_from(continuation: Option<&OpaqueCursor>) -> Result<u64, HistoryError> {
    match continuation {
        None => Ok(0),
        Some(cursor) => cursor
            .as_str()
            .strip_prefix(HISTORY_CURSOR_PREFIX)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(HistoryError::NotFoundOrNotAuthorized),
    }
}
