//! Event-replayed Work projection reads over the canonical journal.
//!
//! Every read here is positioned by the journal's durable append order, the
//! `owner_sequence` each event committed with. Snapshot and delta pages are cut
//! at an append position, and a resume cursor names one, so a follower that
//! resumes at `to` sees exactly the events appended after the page it holds:
//! appends to tasks that sort earlier cannot move rows past the cursor the way
//! an offset into a `task_id, version` ordering let them.
//!
//! A read first captures the owner frontier, then reads only the rows it
//! answers with, bounded by that frontier: an exact read visits one task's
//! history, and a page discovers its changed tasks from positions alone before
//! decoding just those tasks' histories.

use std::collections::BTreeSet;

use tracedecay_application::{WorkProjectionPortError, WorkProjectionReadPort, WorkStorageError};
use tracedecay_domain::{
    ProjectionGenerationId, TaskId, WorkAuthority, WorkEvent, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
};

use super::WorkSqliteStorage;
use super::events::{
    load_registered_appended_positions, load_registered_changed_histories, load_registered_history,
    load_registered_owner_frontier,
};

const CURSOR_TOKEN_PREFIX: &str = "work-projection-append-sequence.v1:";

impl WorkProjectionReadPort for WorkSqliteStorage {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let frontier =
            load_registered_owner_frontier(self.handle(), authority).map_err(port_error)?;
        let history = load_registered_history(self.handle(), authority, task_id, Some(frontier))
            .map_err(port_error)?;
        let projection =
            WorkProjection::rebuild(&history).map_err(|_| WorkProjectionPortError::Unavailable)?;
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
            load_registered_owner_frontier(self.handle(), authority).map_err(port_error)?;
        let page = self.read_page(authority, 0, frontier, page_size)?;
        let generation = projection_generation(authority)?;
        let to_sequence = WorkProjectionSequenceV1::new(page.to);
        let coverage = if page.to == frontier {
            WorkProjectionCoverageV1::complete(page.returned()?, page.total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            WorkProjectionCoverageV1::capped(
                page.returned()?,
                page.total,
                page_size,
                WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), to_sequence)
                    .map_err(|_| WorkProjectionPortError::Unavailable)?,
                projection_cursor(generation.clone(), to_sequence)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionSnapshotV1::new(generation, to_sequence, page.projections, coverage)
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
            load_registered_owner_frontier(self.handle(), authority).map_err(port_error)?;
        if from >= frontier {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let page = self.read_page(authority, from, frontier, page_size)?;
        let from_sequence = WorkProjectionSequenceV1::new(from);
        let to_sequence = WorkProjectionSequenceV1::new(page.to);
        let coverage = if page.to == frontier {
            WorkProjectionCoverageV1::complete(page.returned()?, page.total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            WorkProjectionCoverageV1::capped(
                page.returned()?,
                page.total,
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
            page.projections,
            BTreeSet::new(),
            coverage,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)
    }
}

/// One page of the task walk after `from`: the projections it carries, the
/// inclusive append position it stops at, and how many tasks changed in the
/// whole `(from, frontier]` range.
///
/// `to` is the page's resume point in both directions — the prefix `(0, to]`
/// is what the returned projections replay, and a walk resumed after `to`
/// yields the tasks this page could not fit. Keeping the two in one value is
/// what makes a capped page resumable: a cursor minted anywhere else would
/// name a position whose continuation does not contain the missing tasks.
struct TaskPage {
    projections: Vec<WorkProjection>,
    to: u64,
    total: u32,
}

impl TaskPage {
    fn returned(&self) -> Result<u32, WorkProjectionPortError> {
        count(self.projections.len())
    }
}

impl WorkSqliteStorage {
    /// Discovers the tasks changed after `from` from append positions alone,
    /// cuts the page just before the first event of the task that would
    /// exceed `page_size`, then reads and folds only the selected tasks'
    /// histories as of that cut.
    fn read_page(
        &self,
        authority: &WorkAuthority,
        from: u64,
        frontier: u64,
        page_size: u32,
    ) -> Result<TaskPage, WorkProjectionPortError> {
        let positions =
            load_registered_appended_positions(self.handle(), authority, from, frontier)
                .map_err(port_error)?;
        let mut changed = BTreeSet::new();
        let mut to = frontier;
        for (sequence, task_id) in positions {
            // The first event of one task too many is where the page stops;
            // the walk continues only to count every changed task.
            if changed.insert(task_id) && changed.len() == page_size as usize + 1 {
                to = sequence.saturating_sub(1);
            }
        }
        let total = count(changed.len())?;
        let histories = load_registered_changed_histories(self.handle(), authority, from, to)
            .map_err(port_error)?;
        let projections = fold_histories(&histories)?;
        Ok(TaskPage {
            projections,
            to,
            total,
        })
    }
}

/// Folds histories already grouped by task and ordered by version, each task
/// exactly once.
fn fold_histories(histories: &[WorkEvent]) -> Result<Vec<WorkProjection>, WorkProjectionPortError> {
    histories
        .chunk_by(|previous, next| previous.task_id() == next.task_id())
        .map(|history| {
            WorkProjection::rebuild(history).map_err(|_| WorkProjectionPortError::Unavailable)
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

fn port_error(error: WorkStorageError) -> WorkProjectionPortError {
    match error {
        WorkStorageError::NotFoundOrNotAuthorized => {
            WorkProjectionPortError::NotFoundOrNotAuthorized
        }
        _ => WorkProjectionPortError::Unavailable,
    }
}
