//! Generation-bound read contracts for Work projection snapshots and deltas.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{ProjectionGenerationId, TaskId, WorkProjection};

pub const MAX_WORK_PROJECTION_READ_ITEMS: usize = 1_024;
pub const MAX_WORK_PROJECTION_CURSOR_BYTES: usize = 2_048;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProjectionReadError {
    #[error(
        "Work projection cursor must be canonical and at most {MAX_WORK_PROJECTION_CURSOR_BYTES} bytes"
    )]
    InvalidCursor,
    #[error("Work projection sequence range must increase")]
    InvalidSequenceRange,
    #[error("Work projection coverage counts are inconsistent")]
    InvalidCoverageCounts,
    #[error("Work projection coverage carries fields forbidden by its state")]
    InvalidCoverageShape,
    #[error("Work projection coverage range does not match the envelope sequence")]
    CoverageRangeMismatch,
    #[error("Work projection read item count exceeds {MAX_WORK_PROJECTION_READ_ITEMS}")]
    TooManyItems,
    #[error("Work projection read contains a duplicate task")]
    DuplicateTask,
    #[error("Work projection delta repeats a removed task")]
    DuplicateRemovedTask,
    #[error("Work projection delta changes and removes the same task")]
    ConflictingTaskChange,
    #[error("Work projection delta sequence must increase")]
    NonMonotonicSequence,
    #[error("Work projection generations do not match")]
    GenerationMismatch,
    #[error("Work projection delta does not continue the snapshot sequence")]
    SequenceMismatch,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkProjectionResumeCursorV1 {
    generation_id: ProjectionGenerationId,
    token: String,
}

impl WorkProjectionResumeCursorV1 {
    pub fn new(
        generation_id: ProjectionGenerationId,
        token: impl Into<String>,
    ) -> Result<Self, WorkProjectionReadError> {
        let token = token.into();
        if !crate::canonical_text::is_canonical_text_within(
            &token,
            MAX_WORK_PROJECTION_CURSOR_BYTES,
        ) {
            return Err(WorkProjectionReadError::InvalidCursor);
        }
        Ok(Self {
            generation_id,
            token,
        })
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl<'de> Deserialize<'de> for WorkProjectionResumeCursorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            generation_id: ProjectionGenerationId,
            token: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.generation_id, wire.token).map_err(serde::de::Error::custom)
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
#[schemars(title = "WorkProjectionSequenceV1")]
pub struct WorkProjectionSequenceV1(u64);

impl WorkProjectionSequenceV1 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkProjectionSequenceRangeV1 {
    start_exclusive: WorkProjectionSequenceV1,
    end_inclusive: WorkProjectionSequenceV1,
}

impl WorkProjectionSequenceRangeV1 {
    pub fn new(
        start_exclusive: WorkProjectionSequenceV1,
        end_inclusive: WorkProjectionSequenceV1,
    ) -> Result<Self, WorkProjectionReadError> {
        if start_exclusive >= end_inclusive {
            return Err(WorkProjectionReadError::InvalidSequenceRange);
        }
        Ok(Self {
            start_exclusive,
            end_inclusive,
        })
    }

    pub const fn start_exclusive(self) -> WorkProjectionSequenceV1 {
        self.start_exclusive
    }

    pub const fn end_inclusive(self) -> WorkProjectionSequenceV1 {
        self.end_inclusive
    }
}

impl<'de> Deserialize<'de> for WorkProjectionSequenceRangeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            start_exclusive: WorkProjectionSequenceV1,
            end_inclusive: WorkProjectionSequenceV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.start_exclusive, wire.end_inclusive).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkProjectionCoverageV1 {
    Complete {
        returned: u32,
        total: u32,
    },
    Partial {
        returned: u32,
        total: u32,
        range: WorkProjectionSequenceRangeV1,
        cursor: WorkProjectionResumeCursorV1,
    },
    Capped {
        returned: u32,
        total: u32,
        cap: u32,
        range: WorkProjectionSequenceRangeV1,
        cursor: WorkProjectionResumeCursorV1,
    },
}

impl WorkProjectionCoverageV1 {
    pub fn complete(returned: u32, total: u32) -> Result<Self, WorkProjectionReadError> {
        let coverage = Self::Complete { returned, total };
        coverage.validate()?;
        Ok(coverage)
    }

    pub fn partial(
        returned: u32,
        total: u32,
        range: WorkProjectionSequenceRangeV1,
        cursor: WorkProjectionResumeCursorV1,
    ) -> Result<Self, WorkProjectionReadError> {
        let coverage = Self::Partial {
            returned,
            total,
            range,
            cursor,
        };
        coverage.validate()?;
        Ok(coverage)
    }

    pub fn capped(
        returned: u32,
        total: u32,
        cap: u32,
        range: WorkProjectionSequenceRangeV1,
        cursor: WorkProjectionResumeCursorV1,
    ) -> Result<Self, WorkProjectionReadError> {
        let coverage = Self::Capped {
            returned,
            total,
            cap,
            range,
            cursor,
        };
        coverage.validate()?;
        Ok(coverage)
    }

    pub const fn returned(&self) -> u32 {
        match self {
            Self::Complete { returned, .. }
            | Self::Partial { returned, .. }
            | Self::Capped { returned, .. } => *returned,
        }
    }

    pub const fn total(&self) -> u32 {
        match self {
            Self::Complete { total, .. }
            | Self::Partial { total, .. }
            | Self::Capped { total, .. } => *total,
        }
    }

    pub const fn range(&self) -> Option<WorkProjectionSequenceRangeV1> {
        match self {
            Self::Complete { .. } => None,
            Self::Partial { range, .. } | Self::Capped { range, .. } => Some(*range),
        }
    }

    pub fn resume_cursor(&self) -> Option<&WorkProjectionResumeCursorV1> {
        match self {
            Self::Complete { .. } => None,
            Self::Partial { cursor, .. } | Self::Capped { cursor, .. } => Some(cursor),
        }
    }

    fn validate(&self) -> Result<(), WorkProjectionReadError> {
        match self {
            Self::Complete { returned, total } if returned == total => Ok(()),
            Self::Partial {
                returned, total, ..
            } if *returned > 0 && returned < total => Ok(()),
            Self::Capped {
                returned,
                total,
                cap,
                ..
            } if *cap > 0 && returned == cap && returned < total => Ok(()),
            _ => Err(WorkProjectionReadError::InvalidCoverageCounts),
        }
    }

    fn validate_item_count(&self, item_count: usize) -> Result<(), WorkProjectionReadError> {
        if usize::try_from(self.returned()).ok() != Some(item_count) {
            return Err(WorkProjectionReadError::InvalidCoverageCounts);
        }
        self.validate()
    }

    fn validate_generation(
        &self,
        generation_id: &ProjectionGenerationId,
    ) -> Result<(), WorkProjectionReadError> {
        if let Some(cursor) = self.resume_cursor()
            && cursor.generation_id() != generation_id
        {
            return Err(WorkProjectionReadError::GenerationMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkProjectionCoverageV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "snake_case")]
        enum Wire {
            Complete {
                returned: u32,
                total: u32,
                cursor: Option<WorkProjectionResumeCursorV1>,
                range: Option<WorkProjectionSequenceRangeV1>,
                cap: Option<u32>,
            },
            Partial {
                returned: u32,
                total: u32,
                range: WorkProjectionSequenceRangeV1,
                cursor: WorkProjectionResumeCursorV1,
                cap: Option<u32>,
            },
            Capped {
                returned: u32,
                total: u32,
                cap: u32,
                range: WorkProjectionSequenceRangeV1,
                cursor: WorkProjectionResumeCursorV1,
            },
        }

        let coverage = match Wire::deserialize(deserializer)? {
            Wire::Complete {
                returned,
                total,
                cursor,
                range,
                cap,
            } => {
                if cursor.is_some() || range.is_some() || cap.is_some() {
                    Err(WorkProjectionReadError::InvalidCoverageShape)
                } else {
                    Self::complete(returned, total)
                }
            }
            Wire::Partial {
                returned,
                total,
                range,
                cursor,
                cap,
            } => {
                if cap.is_some() {
                    Err(WorkProjectionReadError::InvalidCoverageShape)
                } else {
                    Self::partial(returned, total, range, cursor)
                }
            }
            Wire::Capped {
                returned,
                total,
                cap,
                range,
                cursor,
            } => Self::capped(returned, total, cap, range, cursor),
        };
        coverage.map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkProjectionSnapshotV1 {
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
    projections: Vec<WorkProjection>,
    coverage: WorkProjectionCoverageV1,
}

impl WorkProjectionSnapshotV1 {
    pub fn new(
        generation_id: ProjectionGenerationId,
        sequence: WorkProjectionSequenceV1,
        mut projections: Vec<WorkProjection>,
        coverage: WorkProjectionCoverageV1,
    ) -> Result<Self, WorkProjectionReadError> {
        canonicalize_projections(&mut projections)?;
        coverage.validate_item_count(projections.len())?;
        coverage.validate_generation(&generation_id)?;
        if let Some(range) = coverage.range()
            && range.end_inclusive() != sequence
        {
            return Err(WorkProjectionReadError::CoverageRangeMismatch);
        }
        Ok(Self {
            generation_id,
            sequence,
            projections,
            coverage,
        })
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub const fn sequence(&self) -> WorkProjectionSequenceV1 {
        self.sequence
    }

    pub fn projections(&self) -> &[WorkProjection] {
        &self.projections
    }

    pub fn coverage(&self) -> &WorkProjectionCoverageV1 {
        &self.coverage
    }
}

impl<'de> Deserialize<'de> for WorkProjectionSnapshotV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            generation_id: ProjectionGenerationId,
            sequence: WorkProjectionSequenceV1,
            projections: Vec<WorkProjection>,
            coverage: WorkProjectionCoverageV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.generation_id,
            wire.sequence,
            wire.projections,
            wire.coverage,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkProjectionDeltaV1 {
    generation_id: ProjectionGenerationId,
    from_sequence: WorkProjectionSequenceV1,
    to_sequence: WorkProjectionSequenceV1,
    changed: Vec<WorkProjection>,
    removed: BTreeSet<TaskId>,
    coverage: WorkProjectionCoverageV1,
}

impl WorkProjectionDeltaV1 {
    pub fn new(
        generation_id: ProjectionGenerationId,
        from_sequence: WorkProjectionSequenceV1,
        to_sequence: WorkProjectionSequenceV1,
        mut changed: Vec<WorkProjection>,
        removed: BTreeSet<TaskId>,
        coverage: WorkProjectionCoverageV1,
    ) -> Result<Self, WorkProjectionReadError> {
        if from_sequence >= to_sequence {
            return Err(WorkProjectionReadError::NonMonotonicSequence);
        }
        canonicalize_projections(&mut changed)?;
        if changed
            .iter()
            .any(|projection| removed.contains(projection.task_id()))
        {
            return Err(WorkProjectionReadError::ConflictingTaskChange);
        }
        let item_count = changed
            .len()
            .checked_add(removed.len())
            .ok_or(WorkProjectionReadError::TooManyItems)?;
        if item_count > MAX_WORK_PROJECTION_READ_ITEMS {
            return Err(WorkProjectionReadError::TooManyItems);
        }
        coverage.validate_item_count(item_count)?;
        coverage.validate_generation(&generation_id)?;
        if let Some(range) = coverage.range()
            && (range.start_exclusive() != from_sequence || range.end_inclusive() != to_sequence)
        {
            return Err(WorkProjectionReadError::CoverageRangeMismatch);
        }
        Ok(Self {
            generation_id,
            from_sequence,
            to_sequence,
            changed,
            removed,
            coverage,
        })
    }

    pub fn validate_after(
        &self,
        snapshot: &WorkProjectionSnapshotV1,
    ) -> Result<(), WorkProjectionReadError> {
        if self.generation_id != snapshot.generation_id {
            return Err(WorkProjectionReadError::GenerationMismatch);
        }
        if self.from_sequence != snapshot.sequence {
            return Err(WorkProjectionReadError::SequenceMismatch);
        }
        Ok(())
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub const fn from_sequence(&self) -> WorkProjectionSequenceV1 {
        self.from_sequence
    }

    pub const fn to_sequence(&self) -> WorkProjectionSequenceV1 {
        self.to_sequence
    }

    pub fn changed(&self) -> &[WorkProjection] {
        &self.changed
    }

    pub fn removed(&self) -> &BTreeSet<TaskId> {
        &self.removed
    }

    pub fn coverage(&self) -> &WorkProjectionCoverageV1 {
        &self.coverage
    }
}

impl<'de> Deserialize<'de> for WorkProjectionDeltaV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            generation_id: ProjectionGenerationId,
            from_sequence: WorkProjectionSequenceV1,
            to_sequence: WorkProjectionSequenceV1,
            changed: Vec<WorkProjection>,
            removed: Vec<TaskId>,
            coverage: WorkProjectionCoverageV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let removed = wire.removed.iter().cloned().collect::<BTreeSet<_>>();
        if removed.len() != wire.removed.len() {
            return Err(serde::de::Error::custom(
                WorkProjectionReadError::DuplicateRemovedTask,
            ));
        }
        Self::new(
            wire.generation_id,
            wire.from_sequence,
            wire.to_sequence,
            wire.changed,
            removed,
            wire.coverage,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn canonicalize_projections(
    projections: &mut [WorkProjection],
) -> Result<(), WorkProjectionReadError> {
    if projections.len() > MAX_WORK_PROJECTION_READ_ITEMS {
        return Err(WorkProjectionReadError::TooManyItems);
    }
    projections.sort_by(|left, right| left.task_id().cmp(right.task_id()));
    if projections
        .windows(2)
        .any(|pair| pair[0].task_id() == pair[1].task_id())
    {
        return Err(WorkProjectionReadError::DuplicateTask);
    }
    Ok(())
}
