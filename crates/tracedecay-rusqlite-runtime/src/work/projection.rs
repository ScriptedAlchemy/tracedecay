//! Event-replayed Work projection reads over the canonical journal.
//!
//! Every read here is positioned by the journal's durable append order, the
//! `owner_sequence` each event committed with. Snapshot and delta pages are cut
//! at an append position, and a resume cursor names one, so a follower that
//! resumes at `to` sees exactly the events appended after the page it holds:
//! appends to tasks that sort earlier cannot move rows past the cursor the way
//! an offset into a `task_id, version` ordering let them.

use std::collections::BTreeSet;

use tracedecay_application::{WorkProjectionPortError, WorkProjectionReadPort};
use tracedecay_domain::{
    ProjectionGenerationId, TaskId, WorkAuthority, WorkEvent, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
};

use super::WorkSqliteStorage;
use super::events::{load_registered_events_in_append_order, load_registered_owner_frontier};

const CURSOR_TOKEN_PREFIX: &str = "work-projection-append-sequence.v1:";

impl WorkProjectionReadPort for WorkSqliteStorage {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let frontier =
            load_registered_owner_frontier(self.handle(), authority).map_err(unavailable)?;
        let events = load_registered_events_in_append_order(self.handle(), authority, 0, frontier)
            .map_err(unavailable)?;
        let task_events = events
            .iter()
            .filter(|(_, event)| event.task_id() == task_id)
            .map(|(_, event)| event.clone())
            .collect::<Vec<_>>();
        let projection = WorkProjection::rebuild(&task_events)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionSnapshotV1::new(
            projection_generation(authority)?,
            WorkProjectionSequenceV1::new(frontier),
            vec![projection],
            WorkProjectionCoverageV1::complete(1, 1)
                .map_err(|_| WorkProjectionPortError::Unavailable)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)
    }

    fn snapshot(
        &self,
        authority: &WorkAuthority,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let frontier =
            load_registered_owner_frontier(self.handle(), authority).map_err(unavailable)?;
        let events = load_registered_events_in_append_order(self.handle(), authority, 0, frontier)
            .map_err(unavailable)?;
        let total = count(
            events
                .iter()
                .map(|(_, event)| event.task_id())
                .collect::<BTreeSet<_>>()
                .len(),
        )?;
        // A capped page is cut at an append position, never at a task count:
        // the tasks it returns are exactly the tasks the journal prefix
        // `(0, to]` introduced, so `delta` resumed at `to` reaches every task
        // this page left out.
        let page = page_tasks(&events, 0, frontier, page_size);
        let projections = rebuild_selected(page.events(&events), &page.selected)?;
        let returned = count(projections.len())?;
        let generation = projection_generation(authority)?;
        let to_sequence = WorkProjectionSequenceV1::new(page.to);
        let coverage = if page.to == frontier {
            WorkProjectionCoverageV1::complete(returned, total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            WorkProjectionCoverageV1::capped(
                returned,
                total,
                page_size,
                WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), to_sequence)
                    .map_err(|_| WorkProjectionPortError::Unavailable)?,
                projection_cursor(generation.clone(), to_sequence)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionSnapshotV1::new(generation, to_sequence, projections, coverage)
            .map_err(|_| WorkProjectionPortError::Unavailable)
    }

    fn delta(
        &self,
        authority: &WorkAuthority,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
        let generation = projection_generation(authority)?;
        if cursor.generation_id() != &generation {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let from = parse_projection_cursor(cursor)?;
        let frontier =
            load_registered_owner_frontier(self.handle(), authority).map_err(unavailable)?;
        if from >= frontier {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let events = load_registered_events_in_append_order(self.handle(), authority, 0, frontier)
            .map_err(unavailable)?;
        let total = count(
            events
                .iter()
                .filter(|(sequence, _)| *sequence > from)
                .map(|(_, event)| event.task_id())
                .collect::<BTreeSet<_>>()
                .len(),
        )?;
        let page = page_tasks(&events, from, frontier, page_size);
        let changed = rebuild_selected(page.events(&events), &page.selected)?;
        let returned = count(changed.len())?;
        let from_sequence = WorkProjectionSequenceV1::new(from);
        let to_sequence = WorkProjectionSequenceV1::new(page.to);
        let coverage = if page.to == frontier {
            WorkProjectionCoverageV1::complete(returned, total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            WorkProjectionCoverageV1::capped(
                returned,
                total,
                page_size,
                WorkProjectionSequenceRangeV1::new(from_sequence, to_sequence)
                    .map_err(|_| WorkProjectionPortError::Unavailable)?,
                projection_cursor(generation.clone(), to_sequence)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionDeltaV1::new(
            generation,
            from_sequence,
            to_sequence,
            changed,
            BTreeSet::new(),
            coverage,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)
    }
}

/// One page of the task walk: the tasks it covers and the inclusive append
/// position it stops at.
///
/// `to` is the page's resume point in both directions — the prefix `(0, to]`
/// is what the returned projections replay, and a walk resumed after `to`
/// yields the tasks this page could not fit. Keeping the two in one value is
/// what makes a capped page resumable: a cursor minted anywhere else would
/// name a position whose continuation does not contain the missing tasks.
struct TaskPage {
    selected: BTreeSet<TaskId>,
    to: u64,
}

impl TaskPage {
    /// The journal prefix the page's projections are rebuilt from.
    fn events<'a>(&self, events: &'a [(u64, WorkEvent)]) -> &'a [(u64, WorkEvent)] {
        &events[..events.partition_point(|(sequence, _)| *sequence <= self.to)]
    }
}

/// Walks the events appended after `from` and admits tasks until one more
/// distinct task would exceed `page_size`, stopping just before that event.
///
/// The cut is the position preceding the event that introduces the
/// overflowing task, so a later read resumed there starts on exactly that
/// event and loses none of the tasks past the cap. A walk that never
/// overflows covers the whole frontier.
fn page_tasks(events: &[(u64, WorkEvent)], from: u64, frontier: u64, page_size: u32) -> TaskPage {
    let mut selected = BTreeSet::new();
    let mut to = frontier;
    for (sequence, event) in events.iter().filter(|(sequence, _)| *sequence > from) {
        if !selected.contains(event.task_id()) && selected.len() == page_size as usize {
            to = sequence - 1;
            break;
        }
        selected.insert(event.task_id().clone());
    }
    TaskPage { selected, to }
}

fn rebuild_selected(
    events: &[(u64, WorkEvent)],
    selected: &BTreeSet<TaskId>,
) -> Result<Vec<WorkProjection>, WorkProjectionPortError> {
    selected
        .iter()
        .map(|task_id| {
            let history = events
                .iter()
                .filter(|(_, event)| event.task_id() == task_id)
                .map(|(_, event)| event.clone())
                .collect::<Vec<_>>();
            WorkProjection::rebuild(&history).map_err(|_| WorkProjectionPortError::Unavailable)
        })
        .collect()
}

fn projection_generation(
    authority: &WorkAuthority,
) -> Result<ProjectionGenerationId, WorkProjectionPortError> {
    authority
        .projection_generation_id()
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(super) fn projection_cursor(
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
    WorkProjectionResumeCursorV1::new(
        generation_id,
        format!("{CURSOR_TOKEN_PREFIX}{}", sequence.get()),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn parse_projection_cursor(
    cursor: &WorkProjectionResumeCursorV1,
) -> Result<u64, WorkProjectionPortError> {
    cursor
        .token()
        .strip_prefix(CURSOR_TOKEN_PREFIX)
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .ok_or(WorkProjectionPortError::StaleCursor)
}

fn count(value: usize) -> Result<u32, WorkProjectionPortError> {
    u32::try_from(value).map_err(|_| WorkProjectionPortError::Unavailable)
}

fn unavailable(_: tracedecay_application::WorkStorageError) -> WorkProjectionPortError {
    WorkProjectionPortError::Unavailable
}
