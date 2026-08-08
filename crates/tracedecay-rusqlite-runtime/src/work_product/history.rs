//! The owner's Work product history: the journaled events themselves, paged in
//! durable sequence order.
//!
//! History is the one Work product read that returns no projection at all. The
//! events are the record, so this authority hands back the stored
//! `WorkProductEventV1` rows unchanged — it does not summarise them, does not
//! reorder them, and never synthesises an event that was not appended.
//!
//! Three bounds shape what a page contains.
//!
//! 1. **The selection must match the journal exactly.** An event records the
//!    relation scopes it was admitted under. A read whose selection is not that
//!    same set is refused rather than filtered: dropping the events a narrower
//!    selection misses would answer with a history that has holes in it and no
//!    way to say so, and the coverage type has no vocabulary for "some events
//!    were withheld".
//! 2. **Nothing later than the read instant is history yet.** Events whose
//!    `occurred_at` is after the request's `observed_at` are outside the read's
//!    own temporal bound and are not returned. `Complete` therefore means
//!    complete as of `observed_at`, which is the only completeness a
//!    point-in-time read can claim.
//! 3. **Pages resume by sequence, not by offset.** The continuation names the
//!    durable sequence the previous page ended on, so an event appended between
//!    two pages cannot shift a caller past an event it never saw.

use tracedecay_application::{
    OpaqueCursor, WorkHistoryCoverageV1, WorkHistoryReadPortV1, WorkHistoryRequestV1,
    WorkHistoryV1, WorkProductApplicationErrorV1, WorkProductPortContextV1,
    WorkProductSelectionScopeV1, WorkRelationScopeV1,
};

use super::load_journal;
use crate::work::WorkSqliteStorage;

type HistoryError = WorkProductApplicationErrorV1;

const HISTORY_CURSOR_PREFIX: &str = "work-product-event-sequence:";

impl WorkHistoryReadPortV1 for WorkSqliteStorage {
    fn read_history(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, HistoryError> {
        let scope = context.authorized_scope();
        let journal =
            load_journal(&self.handle, scope).ok_or(HistoryError::EventAuthorityUnavailable)?;
        let authorized = selected_relation_scopes(scope.selection());
        if journal
            .iter()
            .any(|entry| entry.event.authorized_relation_scopes() != authorized.as_slice())
        {
            return Err(HistoryError::NotFoundOrNotAuthorized);
        }
        let after = resume_from(request.continuation.as_ref())?;
        let mut events = journal
            .into_iter()
            .filter(|entry| {
                entry.sequence.get() > after && entry.event.occurred_at() <= request.observed_at
            })
            .map(|entry| entry.event)
            .collect::<Vec<_>>();
        let limit = usize::try_from(request.limit).map_err(|_| HistoryError::InvalidRequest)?;
        let coverage = if events.len() <= limit {
            WorkHistoryCoverageV1::Complete {
                returned: u32::try_from(events.len())
                    .map_err(|_| HistoryError::EventAuthorityUnavailable)?,
            }
        } else {
            events.truncate(limit);
            let last = events
                .last()
                .map(|event| event.sequence().get())
                .ok_or(HistoryError::EventAuthorityUnavailable)?;
            WorkHistoryCoverageV1::Partial {
                returned: request.limit,
                continuation: OpaqueCursor::new(format!("{HISTORY_CURSOR_PREFIX}{last}"))
                    .map_err(|_| HistoryError::EventAuthorityUnavailable)?,
            }
        };
        Ok(WorkHistoryV1 {
            authorized_scope: scope.clone(),
            events,
            coverage,
        })
    }
}

/// The exact relation scopes an event admitted under this selection must carry.
///
/// This mirrors the set the application re-derives when it checks the answer,
/// so an event that would fail that check is never returned in the first place.
fn selected_relation_scopes(selection: &WorkProductSelectionScopeV1) -> Vec<WorkRelationScopeV1> {
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
