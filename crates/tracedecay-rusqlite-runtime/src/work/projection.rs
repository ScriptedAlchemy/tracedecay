//! Event-replayed Work projection reads over the canonical journal.

use std::collections::BTreeSet;

use tracedecay_application::{WorkProjectionPortError, WorkProjectionReadPort};
use tracedecay_domain::{
    ProjectionGenerationId, TaskId, WorkAuthority, WorkEvent, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1,
    canonical_sha256,
};

use super::WorkSqliteStorage;

impl WorkProjectionReadPort for WorkSqliteStorage {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let events = self.load_authority_events(authority).map_err(unavailable)?;
        let task_events = events
            .iter()
            .filter(|event| event.task_id() == task_id)
            .cloned()
            .collect::<Vec<_>>();
        let projection = WorkProjection::rebuild(&task_events)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionSnapshotV1::new(
            projection_generation(authority)?,
            sequence(events.len())?,
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
        let events = self.load_authority_events(authority).map_err(unavailable)?;
        let total = u32::try_from(
            events
                .iter()
                .map(WorkEvent::task_id)
                .collect::<BTreeSet<_>>()
                .len(),
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let current = sequence(events.len())?;
        // A capped page must be cut at an event boundary, never at a task
        // count. The resume cursor is an event sequence, so the page is only
        // resumable when the tasks it returns are exactly the tasks the
        // journal prefix `[0, to)` introduced: `delta` then continues the same
        // walk from `to` and reaches every task this page left out. Capping by
        // task count while pointing the cursor at the journal head instead
        // named a sequence with nothing after it, so the remainder was
        // unreachable.
        let page = page_tasks(&events, 0, page_size)?;
        let projections = rebuild_selected(page.events(&events)?, &page.selected)?;
        let returned =
            u32::try_from(projections.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let generation = projection_generation(authority)?;
        let to_sequence = WorkProjectionSequenceV1::new(page.to);
        let coverage = if page.to == current.get() {
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
        let events = self.load_authority_events(authority).map_err(unavailable)?;
        let current =
            u64::try_from(events.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        if from >= current {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let all_changed = events
            .iter()
            .skip(from as usize)
            .map(|event| event.task_id().clone())
            .collect::<BTreeSet<_>>();
        let total =
            u32::try_from(all_changed.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let page = page_tasks(&events, from, page_size)?;
        let to = page.to;
        let changed = rebuild_selected(page.events(&events)?, &page.selected)?;
        let returned =
            u32::try_from(changed.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let from_sequence = WorkProjectionSequenceV1::new(from);
        let to_sequence = WorkProjectionSequenceV1::new(to);
        let coverage = if to == current {
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

/// One page of the task walk: the tasks it covers and the exclusive event
/// sequence it stops at.
///
/// `to` is the page's resume point in both directions — the prefix `[0, to)`
/// is what the returned projections replay, and a walk restarted at `to`
/// yields the tasks this page could not fit. Keeping the two in one value is
/// what makes a capped page resumable: a cursor minted anywhere else would
/// name a sequence whose continuation does not contain the missing tasks.
struct TaskPage {
    selected: BTreeSet<TaskId>,
    to: u64,
}

impl TaskPage {
    /// The journal prefix the page's projections are rebuilt from.
    fn events<'a>(
        &self,
        events: &'a [WorkEvent],
    ) -> Result<&'a [WorkEvent], WorkProjectionPortError> {
        events
            .get(..usize::try_from(self.to).map_err(|_| WorkProjectionPortError::Unavailable)?)
            .ok_or(WorkProjectionPortError::Unavailable)
    }
}

/// Walks `events` from `from` and admits tasks until one more distinct task
/// would exceed `page_size`, stopping at that event's offset.
///
/// The cut is on the event that introduces the overflowing task, so the page
/// boundary is a sequence a later read can resume from without either
/// re-deriving the task order or losing the tasks past the cap.
fn page_tasks(
    events: &[WorkEvent],
    from: u64,
    page_size: u32,
) -> Result<TaskPage, WorkProjectionPortError> {
    let mut selected = BTreeSet::new();
    let mut to = u64::try_from(events.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
    for (offset, event) in events.iter().enumerate().skip(from as usize) {
        if !selected.contains(event.task_id()) && selected.len() == page_size as usize {
            to = u64::try_from(offset).map_err(|_| WorkProjectionPortError::Unavailable)?;
            break;
        }
        selected.insert(event.task_id().clone());
    }
    Ok(TaskPage { selected, to })
}

fn rebuild_selected(
    events: &[WorkEvent],
    selected: &BTreeSet<TaskId>,
) -> Result<Vec<WorkProjection>, WorkProjectionPortError> {
    selected
        .iter()
        .map(|task_id| {
            let history = events
                .iter()
                .filter(|event| event.task_id() == task_id)
                .cloned()
                .collect::<Vec<_>>();
            WorkProjection::rebuild(&history).map_err(|_| WorkProjectionPortError::Unavailable)
        })
        .collect()
}

fn projection_generation(
    authority: &WorkAuthority,
) -> Result<ProjectionGenerationId, WorkProjectionPortError> {
    let digest = canonical_sha256(&("tracedecay.work.projection.generation.v1", authority))
        .map_err(|_| WorkProjectionPortError::Unavailable)?;
    ProjectionGenerationId::try_from(format!(
        "generation.work.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

pub(super) fn projection_cursor(
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
    WorkProjectionResumeCursorV1::new(
        generation_id,
        format!("work-projection-sequence.v1:{}", sequence.get()),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn parse_projection_cursor(
    cursor: &WorkProjectionResumeCursorV1,
) -> Result<u64, WorkProjectionPortError> {
    cursor
        .token()
        .strip_prefix("work-projection-sequence.v1:")
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .ok_or(WorkProjectionPortError::StaleCursor)
}

fn sequence(value: usize) -> Result<WorkProjectionSequenceV1, WorkProjectionPortError> {
    u64::try_from(value)
        .map(WorkProjectionSequenceV1::new)
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn unavailable(_: tracedecay_application::WorkStorageError) -> WorkProjectionPortError {
    WorkProjectionPortError::Unavailable
}
