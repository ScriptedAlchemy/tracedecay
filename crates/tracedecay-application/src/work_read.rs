use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    WorkAuthority, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1, WorkProjectionSnapshotV1,
};

use crate::{ApplicationProblem, RequestContext};

pub const MAX_WORK_PROJECTION_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProjectionSnapshotRequestV1 {
    pub page_size: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProjectionDeltaRequestV1 {
    pub cursor: WorkProjectionResumeCursorV1,
    pub page_size: u32,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProjectionPortError {
    #[error("Work projection read authority is unavailable")]
    Unavailable,
    #[error("Work projection resume cursor is stale")]
    StaleCursor,
}

pub trait WorkProjectionReadPort: Send + Sync {
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
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    pub fn snapshot(
        &self,
        context: &RequestContext,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionApplicationError> {
        validate_page_size(page_size)?;
        let authority = super::work::work_authority(context)
            .map_err(WorkProjectionApplicationError::Admission)?;
        self.port
            .snapshot(&authority, page_size)
            .map_err(Into::into)
    }

    pub fn delta(
        &self,
        context: &RequestContext,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionApplicationError> {
        validate_page_size(page_size)?;
        let authority = super::work::work_authority(context)
            .map_err(WorkProjectionApplicationError::Admission)?;
        self.port
            .delta(&authority, cursor, page_size)
            .map_err(Into::into)
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
    use schemars::schema_for;

    use super::*;

    #[test]
    fn public_work_read_schemas_have_only_canonical_domain_names() {
        let snapshot = serde_json::to_string(&schema_for!(WorkProjectionSnapshotV1)).unwrap();
        let delta = serde_json::to_string(&schema_for!(WorkProjectionDeltaV1)).unwrap();
        assert!(snapshot.contains("WorkProjectionSnapshotV1"));
        assert!(delta.contains("WorkProjectionDeltaV1"));
        assert!(!snapshot.contains("\"WorkSnapshotV1\""));
        assert!(!delta.contains("\"WorkDeltaV1\""));
    }

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
