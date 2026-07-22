use std::{error::Error, fmt, time::Duration};

use tracedecay_store::{
    ConsistencyModeV1, RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeRequestProbeV1, StorageRuntimeContractErrorV1,
    StorageRuntimeErrorV1, StorageRuntimePortErrorV1, StorageRuntimePortFutureV1,
    StorageRuntimeReadPort, UnavailableReasonV1, WatermarkCoverageStatusV1,
};

use crate::{
    read_consistency::{ConsistencyClock, ReadConsistencyCoordinator},
    watermark::CommitWatermarkSubscription,
};

use super::{
    ReaderAcquireError, ReaderPool, ReaderQueryExecutor, RetainedExecution,
    SqliteRetainedSnapshotRegistry, unavailable_read,
};

/// The single production read path. Consistency resolution, reader admission,
/// SQLite snapshot ownership, and typed query execution cannot be invoked as
/// independent best-effort steps through this façade.
pub struct ReaderFacade<E, C>
where
    E: ReaderQueryExecutor,
    C: ConsistencyClock,
{
    pool: ReaderPool<E>,
    consistency: ReadConsistencyCoordinator<C>,
    commits: CommitWatermarkSubscription,
    snapshots: SqliteRetainedSnapshotRegistry<E>,
    acquisition_wait: Duration,
}

impl<E, C> ReaderFacade<E, C>
where
    E: ReaderQueryExecutor,
    C: ConsistencyClock,
{
    pub fn new(
        pool: ReaderPool<E>,
        consistency: ReadConsistencyCoordinator<C>,
        commits: CommitWatermarkSubscription,
        snapshots: SqliteRetainedSnapshotRegistry<E>,
        acquisition_wait: Duration,
    ) -> Self {
        Self {
            pool,
            consistency,
            commits,
            snapshots,
            acquisition_wait,
        }
    }

    pub fn retained_snapshots(&self) -> &SqliteRetainedSnapshotRegistry<E> {
        &self.snapshots
    }

    pub async fn read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderReadError> {
        request
            .validate()
            .map_err(ReaderReadError::InvalidRequest)?;
        if request.binding() != self.pool.binding() {
            return Err(ReaderReadError::BindingMismatch);
        }
        validate_probe(&request, probe)?;
        if let Some(reason) = interruption_reason(probe) {
            return unavailable_read(reason).map_err(ReaderReadError::Storage);
        }

        let coverage = self
            .consistency
            .resolve(
                request.binding(),
                request.consistency(),
                &self.commits,
                &self.snapshots,
                probe,
            )
            .await;
        if let Some(reason) = interruption_reason(probe) {
            return unavailable_read(reason).map_err(ReaderReadError::Storage);
        }
        if !coverage_allows_query(&coverage, &request) {
            return outcome(None, coverage, &request);
        }

        let raw = match request.consistency() {
            ConsistencyModeV1::ExactSnapshot { lease } => {
                let execution =
                    match self
                        .snapshots
                        .execute_exact(&lease.lease_id, request.clone(), probe)
                    {
                        Ok(execution) => execution,
                        Err(ReaderAcquireError::Interrupted { reason }) => {
                            return unavailable_read(reason).map_err(ReaderReadError::Storage);
                        }
                        Err(error) => return Err(ReaderReadError::Acquire(error)),
                    };
                match execution {
                    RetainedExecution::Outcome(outcome) => *outcome,
                    RetainedExecution::Unavailable(reason) => {
                        return unavailable_read(reason).map_err(ReaderReadError::Storage);
                    }
                }
            }
            _ => {
                let mut reader = match self.pool.acquire(&request, probe, self.acquisition_wait) {
                    Ok(reader) => reader,
                    Err(ReaderAcquireError::Interrupted { reason }) => {
                        return unavailable_read(reason).map_err(ReaderReadError::Storage);
                    }
                    Err(error) => return Err(ReaderReadError::Acquire(error)),
                };
                if let Err(error) = reader.begin_pinned_snapshot(probe) {
                    match error {
                        ReaderAcquireError::Interrupted { reason } => {
                            return unavailable_read(reason).map_err(ReaderReadError::Storage);
                        }
                        error => return Err(ReaderReadError::Acquire(error)),
                    }
                }
                match reader.execute_active_raw(request.clone(), probe) {
                    Ok(outcome) => outcome,
                    Err(ReaderAcquireError::Interrupted { reason }) => {
                        return unavailable_read(reason).map_err(ReaderReadError::Storage);
                    }
                    Err(error) => return Err(ReaderReadError::Acquire(error)),
                }
            }
        };
        if let Some(reason) = interruption_reason(probe) {
            return unavailable_read(reason).map_err(ReaderReadError::Storage);
        }
        outcome(raw.value().cloned(), coverage, &request)
    }
}

impl<E, C> StorageRuntimeReadPort for ReaderFacade<E, C>
where
    E: ReaderQueryExecutor,
    C: ConsistencyClock + Send + Sync,
{
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
        Box::pin(async move {
            self.read(request, probe)
                .await
                .map_err(ReaderReadError::into_port_error)
        })
    }
}

#[derive(Debug)]
pub enum ReaderReadError {
    InvalidRequest(StorageRuntimeContractErrorV1),
    BindingMismatch,
    Acquire(ReaderAcquireError),
    Storage(StorageRuntimeErrorV1),
    InvalidOutcome(StorageRuntimeContractErrorV1),
}

impl ReaderReadError {
    fn into_port_error(self) -> StorageRuntimePortErrorV1 {
        match self {
            Self::InvalidRequest(error) => StorageRuntimePortErrorV1::InvalidRequest(error),
            error => StorageRuntimePortErrorV1::Runtime(Box::new(
                StorageRuntimeErrorV1::Infrastructure {
                    operation: error.to_string(),
                },
            )),
        }
    }
}

impl fmt::Display for ReaderReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(formatter, "invalid read request: {error}"),
            Self::BindingMismatch => {
                formatter.write_str("read request does not bind to this reader façade")
            }
            Self::Acquire(error) => write!(formatter, "reader acquisition failed: {error}"),
            Self::Storage(error) => write!(formatter, "reader storage failed: {error}"),
            Self::InvalidOutcome(error) => write!(formatter, "invalid reader outcome: {error}"),
        }
    }
}

impl Error for ReaderReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequest(error) | Self::InvalidOutcome(error) => Some(error),
            Self::Acquire(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::BindingMismatch => None,
        }
    }
}

fn coverage_allows_query(coverage: &RuntimeReadCoverageV1, request: &RuntimeReadRequestV1) -> bool {
    match coverage {
        RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. } => true,
        RuntimeReadCoverageV1::Partial { coverage } => {
            coverage.status_for(&request.binding().shard_id) == WatermarkCoverageStatusV1::Satisfied
        }
        RuntimeReadCoverageV1::Stale { .. } | RuntimeReadCoverageV1::Unavailable { .. } => false,
    }
}

fn validate_probe(
    request: &RuntimeReadRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), ReaderReadError> {
    if probe.cancellation_identity() != &request.control().cancellation {
        return Err(ReaderReadError::Acquire(
            ReaderAcquireError::ProbeBindingMismatch {
                field: "cancellation identity",
            },
        ));
    }
    if probe.deadline_identity() != &request.control().deadline {
        return Err(ReaderReadError::Acquire(
            ReaderAcquireError::ProbeBindingMismatch {
                field: "deadline identity",
            },
        ));
    }
    Ok(())
}

fn interruption_reason(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    probe.interruption().map(|interruption| match interruption {
        RuntimeInterruptionV1::Cancelled => UnavailableReasonV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded => UnavailableReasonV1::DeadlineExceeded,
    })
}

fn outcome(
    value: Option<tracedecay_store::RuntimeReadResultV1>,
    coverage: RuntimeReadCoverageV1,
    request: &RuntimeReadRequestV1,
) -> Result<RuntimeReadOutcomeV1, ReaderReadError> {
    let outcome =
        RuntimeReadOutcomeV1::new(value, coverage).map_err(ReaderReadError::InvalidOutcome)?;
    outcome
        .validate_for(request)
        .map_err(ReaderReadError::InvalidOutcome)?;
    Ok(outcome)
}
