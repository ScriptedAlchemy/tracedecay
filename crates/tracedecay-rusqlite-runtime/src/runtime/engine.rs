use std::{error::Error, fmt};

use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::ports::*;
use crate::watermark::CommitWatermarkSubscription;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardRuntimeState {
    Ready,
    Draining,
    Closed,
}

#[derive(Clone, Debug)]
pub struct ShardRuntimeTelemetry<WT, RT, CT, MT, BT, PT> {
    pub state: ShardRuntimeState,
    pub writer: WT,
    pub readers: RT,
    pub checkpoint: CT,
    pub maintenance: MT,
    pub backup: BT,
    pub repair: PT,
}

#[derive(Debug)]
pub enum ShardRuntimeStartError<E> {
    LocatorBindingMismatch,
    Attachment(E),
    ComponentBindingMismatch { component: &'static str },
}

impl<E: fmt::Display> fmt::Display for ShardRuntimeStartError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocatorBindingMismatch => {
                formatter.write_str("verified locator does not match the canonical binding")
            }
            Self::Attachment(error) => write!(formatter, "runtime attachment failed: {error}"),
            Self::ComponentBindingMismatch { component } => {
                write!(
                    formatter,
                    "{component} attached to a different canonical binding"
                )
            }
        }
    }
}

impl<E: Error + 'static> Error for ShardRuntimeStartError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Attachment(error) => Some(error),
            _ => None,
        }
    }
}

pub struct ShardRuntimeEngine<W, R, C, M, B, P>
where
    W: RuntimeWriter,
    R: RuntimeReaders,
    C: CheckpointControl,
    M: MaintenanceControl,
    B: BackupControl,
    P: RepairControl,
{
    writer: Option<W>,
    readers: Option<R>,
    checkpoint: Option<C>,
    maintenance: Option<M>,
    backup: Option<B>,
    repair: Option<P>,
    state: ShardRuntimeState,
}

impl<W, R, C, M, B, P> ShardRuntimeEngine<W, R, C, M, B, P>
where
    W: RuntimeWriter,
    R: RuntimeReaders,
    C: CheckpointControl,
    M: MaintenanceControl,
    B: BackupControl,
    P: RepairControl,
{
    pub fn start<A>(
        attachment: &mut A,
        binding: &StoreRuntimeBindingV1,
        locator: &VerifiedStoreLocatorV1,
    ) -> Result<Self, ShardRuntimeStartError<A::Error>>
    where
        A: ShardRuntimeAttachment<
                Writer = W,
                Readers = R,
                Checkpoint = C,
                Maintenance = M,
                Backup = B,
                Repair = P,
            >,
    {
        if locator.shard_id != binding.shard_id || locator.incarnation != binding.incarnation {
            return Err(ShardRuntimeStartError::LocatorBindingMismatch);
        }
        let parts = attachment
            .attach(binding, locator)
            .map_err(ShardRuntimeStartError::Attachment)?;
        validate_binding(binding, "writer", parts.writer.binding())?;
        validate_binding(binding, "reader pool", parts.readers.binding())?;
        validate_binding(binding, "checkpoint control", parts.checkpoint.binding())?;
        validate_binding(binding, "maintenance control", parts.maintenance.binding())?;
        validate_binding(binding, "backup control", parts.backup.binding())?;
        validate_binding(binding, "repair control", parts.repair.binding())?;
        Ok(Self {
            writer: Some(parts.writer),
            readers: Some(parts.readers),
            checkpoint: Some(parts.checkpoint),
            maintenance: Some(parts.maintenance),
            backup: Some(parts.backup),
            repair: Some(parts.repair),
            state: ShardRuntimeState::Ready,
        })
    }

    pub const fn state(&self) -> ShardRuntimeState {
        self.state
    }

    pub fn writer(&self) -> &W {
        self.writer.as_ref().expect("writer exists until close")
    }

    pub fn readers(&self) -> &R {
        self.readers.as_ref().expect("readers exist until close")
    }

    pub fn committed_watermarks(&self) -> CommitWatermarkSubscription {
        self.writer().committed_watermarks()
    }

    pub fn checkpoint_status(&self) -> C::Status {
        self.checkpoint
            .as_ref()
            .expect("checkpoint exists until close")
            .checkpoint_status()
    }

    pub fn checkpoint_pressure(&self) -> C::Pressure {
        self.checkpoint
            .as_ref()
            .expect("checkpoint exists until close")
            .checkpoint_pressure()
    }

    pub fn trigger_checkpoint(
        &mut self,
        request: C::Request,
    ) -> Result<C::Outcome, RuntimePortError> {
        self.checkpoint
            .as_mut()
            .expect("checkpoint exists until close")
            .trigger_checkpoint(request)
    }

    pub fn trigger_maintenance_checkpoint(
        &mut self,
        request: C::MaintenanceRequest,
    ) -> Result<C::Outcome, RuntimePortError> {
        self.checkpoint
            .as_mut()
            .expect("checkpoint exists until close")
            .trigger_maintenance_checkpoint(request)
    }

    pub fn run_backup(&mut self, request: B::Request) -> Result<B::Outcome, RuntimePortError> {
        self.backup
            .as_mut()
            .expect("backup exists until close")
            .run_backup(request)
    }

    pub fn run_maintenance(&mut self, request: M::Request) -> Result<M::Outcome, RuntimePortError> {
        self.begin_drain();
        self.maintenance
            .as_mut()
            .expect("maintenance exists until close")
            .run_maintenance(request)
    }

    pub fn run_repair(&mut self, request: P::Request) -> Result<P::Outcome, RuntimePortError> {
        self.begin_drain();
        self.repair
            .as_mut()
            .expect("repair exists until close")
            .run_repair(request)
    }

    pub fn begin_drain(&mut self) {
        if self.state != ShardRuntimeState::Ready {
            return;
        }
        self.state = ShardRuntimeState::Draining;
        self.writer().begin_drain();
        self.readers().begin_drain();
    }

    #[allow(clippy::type_complexity)]
    pub fn telemetry_snapshot(
        &self,
    ) -> ShardRuntimeTelemetry<
        W::Telemetry,
        R::Telemetry,
        C::Telemetry,
        M::Telemetry,
        B::Telemetry,
        P::Telemetry,
    > {
        ShardRuntimeTelemetry {
            state: self.state,
            writer: self.writer().telemetry_snapshot(),
            readers: self.readers().telemetry_snapshot(),
            checkpoint: self.checkpoint.as_ref().unwrap().telemetry_snapshot(),
            maintenance: self.maintenance.as_ref().unwrap().telemetry_snapshot(),
            backup: self.backup.as_ref().unwrap().telemetry_snapshot(),
            repair: self.repair.as_ref().unwrap().telemetry_snapshot(),
        }
    }

    pub fn shutdown_and_join(mut self) -> Result<(), RuntimePortError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), RuntimePortError> {
        self.begin_drain();
        let mut first_error = None;
        if let Some(readers) = self.readers.take() {
            record_error(&mut first_error, readers.shutdown_and_join());
        }
        close_component(&mut self.checkpoint, &mut first_error);
        close_component(&mut self.maintenance, &mut first_error);
        close_component(&mut self.backup, &mut first_error);
        close_component(&mut self.repair, &mut first_error);
        if let Some(writer) = self.writer.take() {
            record_error(&mut first_error, writer.shutdown_and_join());
        }
        self.state = ShardRuntimeState::Closed;
        first_error.map_or(Ok(()), Err)
    }
}

impl<W, R, C, M, B, P> Drop for ShardRuntimeEngine<W, R, C, M, B, P>
where
    W: RuntimeWriter,
    R: RuntimeReaders,
    C: CheckpointControl,
    M: MaintenanceControl,
    B: BackupControl,
    P: RepairControl,
{
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

fn validate_binding<E>(
    expected: &StoreRuntimeBindingV1,
    component: &'static str,
    actual: &StoreRuntimeBindingV1,
) -> Result<(), ShardRuntimeStartError<E>> {
    (expected == actual)
        .then_some(())
        .ok_or(ShardRuntimeStartError::ComponentBindingMismatch { component })
}

fn close_component<T: RuntimeControl>(
    component: &mut Option<T>,
    first_error: &mut Option<RuntimePortError>,
) {
    if let Some(mut component) = component.take() {
        record_error(first_error, component.close());
    }
}

fn record_error(first: &mut Option<RuntimePortError>, result: Result<(), RuntimePortError>) {
    if first.is_none() {
        *first = result.err();
    }
}
