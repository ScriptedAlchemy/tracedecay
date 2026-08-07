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
        let task_ids = events
            .iter()
            .map(|event| event.task_id().clone())
            .collect::<BTreeSet<_>>();
        let total =
            u32::try_from(task_ids.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let selected = task_ids
            .into_iter()
            .take(page_size as usize)
            .collect::<BTreeSet<_>>();
        let projections = rebuild_selected(&events, &selected)?;
        let returned =
            u32::try_from(projections.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let generation = projection_generation(authority)?;
        let current = sequence(events.len())?;
        let coverage = if returned == total {
            WorkProjectionCoverageV1::complete(returned, total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            WorkProjectionCoverageV1::capped(
                returned,
                total,
                page_size,
                WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), current)
                    .map_err(|_| WorkProjectionPortError::Unavailable)?,
                projection_cursor(generation.clone(), current)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionSnapshotV1::new(generation, current, projections, coverage)
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
        let mut selected = BTreeSet::new();
        let mut to = current;
        for (offset, event) in events.iter().enumerate().skip(from as usize) {
            if !selected.contains(event.task_id()) && selected.len() == page_size as usize {
                to = u64::try_from(offset).map_err(|_| WorkProjectionPortError::Unavailable)?;
                break;
            }
            selected.insert(event.task_id().clone());
        }
        let bounded_events =
            &events[..usize::try_from(to).map_err(|_| WorkProjectionPortError::Unavailable)?];
        let changed = rebuild_selected(bounded_events, &selected)?;
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
