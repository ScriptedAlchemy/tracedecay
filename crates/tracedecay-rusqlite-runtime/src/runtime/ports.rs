use std::{error::Error, fmt};

use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::{
    CheckpointHandle, CheckpointPressure, CheckpointRequest, CheckpointStatus, CheckpointTicket,
    MaintenanceCheckpointRequest, PersistentWriter, WriterActorError, WriterState,
    WriterTelemetrySnapshot,
    reader::{ReaderPool, ReaderPoolSnapshot, ReaderQueryExecutor},
    watermark::CommitWatermarkSubscription,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePortError {
    pub component: &'static str,
    pub operation: &'static str,
}

impl RuntimePortError {
    pub const fn new(component: &'static str, operation: &'static str) -> Self {
        Self {
            component,
            operation,
        }
    }
}

impl fmt::Display for RuntimePortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed to {}", self.component, self.operation)
    }
}

impl Error for RuntimePortError {}

/// Common contract for a physical capability attached to one canonical shard.
pub trait RuntimeComponent {
    type Telemetry: Clone;

    fn binding(&self) -> &StoreRuntimeBindingV1;
    fn telemetry_snapshot(&self) -> Self::Telemetry;
}

pub trait RuntimeControl: RuntimeComponent {
    fn close(&mut self) -> Result<(), RuntimePortError>;
}

pub trait RuntimeWriter: RuntimeComponent {
    fn state(&self) -> WriterState;
    fn committed_watermarks(&self) -> CommitWatermarkSubscription;
    fn begin_drain(&self);
    fn shutdown_and_join(self) -> Result<(), RuntimePortError>;
}

impl RuntimeComponent for PersistentWriter {
    type Telemetry = WriterTelemetrySnapshot;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.binding()
    }

    fn telemetry_snapshot(&self) -> Self::Telemetry {
        self.telemetry_snapshot()
    }
}

impl RuntimeWriter for PersistentWriter {
    fn state(&self) -> WriterState {
        self.state()
    }

    fn committed_watermarks(&self) -> CommitWatermarkSubscription {
        self.commit_watermark_source()
    }

    fn begin_drain(&self) {
        self.begin_drain();
    }

    fn shutdown_and_join(self) -> Result<(), RuntimePortError> {
        self.shutdown_and_join()
            .map_err(|error| writer_error(&error))
    }
}

fn writer_error(_error: &WriterActorError) -> RuntimePortError {
    RuntimePortError::new("persistent writer", "shutdown and join")
}

pub trait RuntimeReaders: RuntimeComponent {
    fn begin_drain(&self);
    fn shutdown_and_join(self) -> Result<(), RuntimePortError>;
}

impl<E: ReaderQueryExecutor> RuntimeComponent for ReaderPool<E> {
    type Telemetry = ReaderPoolSnapshot;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.binding()
    }

    fn telemetry_snapshot(&self) -> Self::Telemetry {
        self.snapshot()
    }
}

impl<E: ReaderQueryExecutor> RuntimeReaders for ReaderPool<E> {
    fn begin_drain(&self) {
        ReaderPool::begin_drain(self);
    }

    fn shutdown_and_join(self) -> Result<(), RuntimePortError> {
        drop(self);
        Ok(())
    }
}

pub trait CheckpointControl: RuntimeControl {
    type Request;
    type Outcome;
    type MaintenanceRequest;
    type Status: Clone;
    type Pressure: Clone;

    fn checkpoint_status(&self) -> Self::Status;
    fn checkpoint_pressure(&self) -> Self::Pressure;
    fn trigger_checkpoint(
        &mut self,
        request: Self::Request,
    ) -> Result<Self::Outcome, RuntimePortError>;
    fn trigger_maintenance_checkpoint(
        &mut self,
        request: Self::MaintenanceRequest,
    ) -> Result<Self::Outcome, RuntimePortError>;
}

impl RuntimeComponent for CheckpointHandle {
    type Telemetry = CheckpointStatus;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.binding()
    }

    fn telemetry_snapshot(&self) -> Self::Telemetry {
        self.status()
    }
}

impl RuntimeControl for CheckpointHandle {
    fn close(&mut self) -> Result<(), RuntimePortError> {
        CheckpointHandle::close(self);
        Ok(())
    }
}

impl CheckpointControl for CheckpointHandle {
    type Request = CheckpointRequest;
    type Outcome = CheckpointTicket;
    type MaintenanceRequest = MaintenanceCheckpointRequest;
    type Status = CheckpointStatus;
    type Pressure = CheckpointPressure;

    fn checkpoint_status(&self) -> Self::Status {
        self.status()
    }

    fn checkpoint_pressure(&self) -> Self::Pressure {
        self.pressure()
    }

    fn trigger_checkpoint(
        &mut self,
        request: Self::Request,
    ) -> Result<Self::Outcome, RuntimePortError> {
        self.trigger(request)
            .map_err(|_| RuntimePortError::new("checkpoint control", "enqueue checkpoint"))
    }

    fn trigger_maintenance_checkpoint(
        &mut self,
        request: Self::MaintenanceRequest,
    ) -> Result<Self::Outcome, RuntimePortError> {
        self.trigger_maintenance(request).map_err(|_| {
            RuntimePortError::new("checkpoint control", "enqueue maintenance checkpoint")
        })
    }
}

macro_rules! capability_port {
    ($name:ident, $method:ident) => {
        pub trait $name: RuntimeControl {
            type Request;
            type Outcome;

            fn $method(
                &mut self,
                request: Self::Request,
            ) -> Result<Self::Outcome, RuntimePortError>;
        }
    };
}

capability_port!(MaintenanceControl, run_maintenance);
capability_port!(BackupControl, run_backup);
capability_port!(RepairControl, run_repair);

pub struct ShardRuntimeParts<W, R, C, M, B, P> {
    pub writer: W,
    pub readers: R,
    pub checkpoint: C,
    pub maintenance: M,
    pub backup: B,
    pub repair: P,
}

/// Driver-neutral daemon seam. The adapter may carry path/open configuration,
/// while the request carries only already-authorized canonical identity.
pub trait ShardRuntimeAttachment {
    type Writer: RuntimeWriter;
    type Readers: RuntimeReaders;
    type Checkpoint: CheckpointControl;
    type Maintenance: MaintenanceControl;
    type Backup: BackupControl;
    type Repair: RepairControl;
    type Error: Error + Send + Sync + 'static;

    #[allow(clippy::type_complexity)]
    fn attach(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        locator: &VerifiedStoreLocatorV1,
    ) -> Result<
        ShardRuntimeParts<
            Self::Writer,
            Self::Readers,
            Self::Checkpoint,
            Self::Maintenance,
            Self::Backup,
            Self::Repair,
        >,
        Self::Error,
    >;
}
