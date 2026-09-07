use thiserror::Error;
use tracedecay_domain::{
    TaskId, WorkAuthority, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSnapshotV1,
};

use crate::{ApplicationProblem, RequestContext};

pub const MAX_WORK_PROJECTION_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProjectionPortError {
    #[error("Work projection read authority is unavailable")]
    Unavailable,
    #[error("Work projection resume cursor is stale")]
    StaleCursor,
    #[error("Work projection does not exist or is not authorized")]
    NotFoundOrNotAuthorized,
}

pub trait WorkProjectionReadPort: Send + Sync {
    fn exact_snapshot(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError>;

    fn snapshot(
        &self,
        authority: &WorkAuthority,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError>;

    fn delta(
        &self,
        authority: &WorkAuthority,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError>;
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProjectionApplicationError {
    #[error("Work projection request was not admitted")]
    Admission(ApplicationProblem),
    #[error("Work projection page size must be between 1 and {MAX_WORK_PROJECTION_PAGE_SIZE}")]
    InvalidPageSize,
    #[error(transparent)]
    Port(#[from] WorkProjectionPortError),
}

pub struct WorkProjectionReadService<P> {
    port: P,
}

impl<P> WorkProjectionReadService<P>
where
    P: WorkProjectionReadPort,
{
    #[hotpath::skip]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[hotpath::measure(label = "application.work.read.exact_snapshot")]
    pub fn exact_snapshot(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionApplicationError> {
        let authority = super::work::work_authority(context)
            .map_err(WorkProjectionApplicationError::Admission)?;
        self.port
            .exact_snapshot(&authority, task_id)
            .map_err(Into::into)
    }

    #[hotpath::measure(label = "application.work.read.snapshot")]
    pub fn snapshot(
        &self,
        context: &RequestContext,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionApplicationError> {
        validate_page_size(page_size)?;
        let authority = super::work::work_authority(context)
            .map_err(WorkProjectionApplicationError::Admission)?;
        let snapshot = self
            .port
            .snapshot(&authority, page_size)
            .map_err(WorkProjectionApplicationError::from)?;
        // Page fill, distinct from the surrounding read latency: a slow read
        // of three rows and a fast read of a full page are different defects.
        hotpath::gauge!("application.work.read.snapshot.rows")
            .set(snapshot.projections().len() as u64);
        Ok(snapshot)
    }

    #[hotpath::measure(label = "application.work.read.delta")]
    pub fn delta(
        &self,
        context: &RequestContext,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionApplicationError> {
        validate_page_size(page_size)?;
        let authority = super::work::work_authority(context)
            .map_err(WorkProjectionApplicationError::Admission)?;
        let delta = self
            .port
            .delta(&authority, cursor, page_size)
            .map_err(WorkProjectionApplicationError::from)?;
        hotpath::gauge!("application.work.read.delta.rows")
            .set((delta.changed().len() + delta.removed().len()) as u64);
        Ok(delta)
    }
}

fn validate_page_size(page_size: u32) -> Result<(), WorkProjectionApplicationError> {
    if page_size == 0 || page_size > MAX_WORK_PROJECTION_PAGE_SIZE {
        Err(WorkProjectionApplicationError::InvalidPageSize)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_page_bounds_fail_before_the_port_runs() {
        assert_eq!(
            validate_page_size(0),
            Err(WorkProjectionApplicationError::InvalidPageSize)
        );
        assert_eq!(
            validate_page_size(MAX_WORK_PROJECTION_PAGE_SIZE + 1),
            Err(WorkProjectionApplicationError::InvalidPageSize)
        );
    }
}
