//! One bounded, persistent SQLite writer for one authorized shard.

mod backup;
mod request;
mod settlement;
#[cfg(test)]
mod tests;
mod transaction;
mod worker;

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc as std_mpsc,
    },
    thread::{self, JoinHandle},
};

use rusqlite::{Savepoint, Transaction};
use tokio::sync::{mpsc, oneshot, watch};
use tracedecay_store::{
    AdmissionConfigV1, IdempotencyIdentityV1, RuntimeCancellationStageV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StorageRuntimeContractErrorV1,
    StorageRuntimeErrorV1, StoreCommitReceiptV1, StoreRuntimeBindingV1, UnavailableReasonV1,
    VerifiedStoreLocatorV1,
};

use crate::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage,
    StorageOperationExecutor,
    admission::{
        Admission, Capacity, DEFAULT_RESERVED_HEALTH_BYTES, DEFAULT_RESERVED_HEALTH_OPERATIONS,
        Limits,
    },
    checkpoint::{
        CheckpointBlockers, CheckpointError, CheckpointOutcome, CheckpointPressure,
        CheckpointResult, CheckpointStatus, MaintenanceCheckpointMode, RusqliteCheckpointError,
    },
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    maintenance::ExclusiveMaintenancePermit,
    migration_sql::WriterCommand as MigrationSqlWriterCommand,
    persistence::RuntimeWriterPersistence,
    telemetry::{WriterTelemetry, WriterTelemetrySnapshot},
    watermark::{CommitWatermarkSubscription, CommittedWatermarkPublisher},
};

struct UnrestrictedRuntimeWriteAuthority;

impl RuntimeWriteAuthority for UnrestrictedRuntimeWriteAuthority {
    fn verify(&self, _stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        Ok(())
    }
}

use backup::{OnlineBackupCommand, validate_destination};
pub use backup::{OnlineBackupReceipt, WriterOnlineBackupError};
use request::{AcceptedRequest, CheckpointCommand, IncrementalVacuumCommand};
use worker::Worker;

#[derive(Clone)]
pub struct CheckpointHandle {
    binding: StoreRuntimeBindingV1,
    state: Arc<AtomicU8>,
    shutdown_requested: Arc<AtomicBool>,
    sender: Option<mpsc::Sender<CheckpointCommand>>,
    status: watch::Receiver<CheckpointStatus>,
    pressure: watch::Receiver<CheckpointPressure>,
}

impl CheckpointHandle {
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn status(&self) -> CheckpointStatus {
        self.status.borrow().clone()
    }

    pub fn status_subscription(&self) -> watch::Receiver<CheckpointStatus> {
        self.status.clone()
    }

    pub fn pressure(&self) -> CheckpointPressure {
        self.pressure.borrow().clone()
    }

    pub fn pressure_subscription(&self) -> watch::Receiver<CheckpointPressure> {
        self.pressure.clone()
    }

    pub fn trigger(
        &self,
        request: CheckpointRequest,
    ) -> Result<CheckpointTicket, CheckpointControlError> {
        self.trigger_authorized(request, Arc::new(UnrestrictedRuntimeWriteAuthority))
    }

    pub fn trigger_authorized(
        &self,
        request: CheckpointRequest,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<CheckpointTicket, CheckpointControlError> {
        let (reply, response) = oneshot::channel();
        let command = CheckpointCommand::new(request.blockers, request.probe, authority, reply);
        self.enqueue(command, response, WriterState::Ready)
    }

    pub fn trigger_maintenance(
        &self,
        request: MaintenanceCheckpointRequest,
    ) -> Result<CheckpointTicket, CheckpointControlError> {
        self.trigger_maintenance_authorized(request, Arc::new(UnrestrictedRuntimeWriteAuthority))
    }

    pub fn trigger_maintenance_authorized(
        &self,
        request: MaintenanceCheckpointRequest,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<CheckpointTicket, CheckpointControlError> {
        if request.permit.binding() != &self.binding {
            return Err(CheckpointControlError::BindingMismatch);
        }
        let (reply, response) = oneshot::channel();
        let command = CheckpointCommand::new_maintenance(
            request.blockers,
            request.mode,
            request.permit,
            authority,
            reply,
        );
        self.enqueue(command, response, WriterState::Draining)
    }

    fn enqueue(
        &self,
        command: CheckpointCommand,
        response: oneshot::Receiver<
            Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>>,
        >,
        required_state: WriterState,
    ) -> Result<CheckpointTicket, CheckpointControlError> {
        command
            .verify(RuntimeWriteAuthorityStage::BeforeAdmission)
            .map_err(checkpoint_control_error)?;
        if self.shutdown_requested.load(Ordering::Acquire)
            || WriterState::load(&self.state) != required_state
        {
            return Err(CheckpointControlError::Unavailable);
        }
        let sender = self
            .sender
            .as_ref()
            .ok_or(CheckpointControlError::Unavailable)?;
        sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => CheckpointControlError::Busy,
            mpsc::error::TrySendError::Closed(_) => CheckpointControlError::Unavailable,
        })?;
        Ok(CheckpointTicket { response })
    }
}

pub struct CheckpointRequest {
    blockers: CheckpointBlockers,
    probe: Arc<dyn RuntimeRequestProbeV1>,
}

impl CheckpointRequest {
    pub fn new(blockers: CheckpointBlockers, probe: Arc<dyn RuntimeRequestProbeV1>) -> Self {
        Self { blockers, probe }
    }

    pub fn blockers(&self) -> &CheckpointBlockers {
        &self.blockers
    }
}

pub struct MaintenanceCheckpointRequest {
    mode: MaintenanceCheckpointMode,
    permit: ExclusiveMaintenancePermit,
    blockers: CheckpointBlockers,
}

impl MaintenanceCheckpointRequest {
    pub fn new(
        mode: MaintenanceCheckpointMode,
        permit: ExclusiveMaintenancePermit,
        blockers: CheckpointBlockers,
    ) -> Self {
        Self {
            mode,
            permit,
            blockers,
        }
    }

    pub const fn mode(&self) -> MaintenanceCheckpointMode {
        self.mode
    }

    pub fn blockers(&self) -> &CheckpointBlockers {
        &self.blockers
    }
}

pub struct CheckpointTicket {
    response: oneshot::Receiver<Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>>>,
}

impl CheckpointTicket {
    pub async fn wait(self) -> Result<CheckpointOutcome, CheckpointControlError> {
        let result = self
            .response
            .await
            .map_err(|_| CheckpointControlError::Unavailable)?
            .map_err(checkpoint_control_error)?;
        Ok(CheckpointOutcome::from_internal(result))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointControlError {
    Busy,
    Unavailable,
    BindingMismatch,
    AuthorityDenied { stage: RuntimeWriteAuthorityStage },
    Blocked(CheckpointBlockers),
    Driver(String),
}

impl fmt::Display for CheckpointControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("checkpoint control is busy"),
            Self::Unavailable => formatter.write_str("checkpoint control is unavailable"),
            Self::BindingMismatch => {
                formatter.write_str("maintenance permit belongs to another shard")
            }
            Self::AuthorityDenied { stage } => {
                write!(formatter, "runtime write authority denied at {stage:?}")
            }
            Self::Blocked(blockers) => {
                write!(
                    formatter,
                    "checkpoint is blocked by {} readers",
                    blockers.count()
                )
            }
            Self::Driver(message) => write!(formatter, "checkpoint driver failed: {message}"),
        }
    }
}

impl Error for CheckpointControlError {}

fn checkpoint_control_error(
    error: CheckpointError<RusqliteCheckpointError>,
) -> CheckpointControlError {
    match error {
        CheckpointError::Driver(error) => CheckpointControlError::Driver(error.to_string()),
        CheckpointError::MaintenanceStillDraining(blockers) => {
            CheckpointControlError::Blocked(blockers)
        }
        CheckpointError::AuthorityDenied(stage) => {
            CheckpointControlError::AuthorityDenied { stage }
        }
        CheckpointError::InvalidConfig(_) => CheckpointControlError::Unavailable,
    }
}

#[derive(Clone, Debug)]
pub struct ExistingWriterLocator {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    opened_database: Option<Arc<OpenedDatabaseFile>>,
}

impl ExistingWriterLocator {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
    ) -> Result<Self, WriterStartError> {
        if locator.shard_id != binding.shard_id || locator.incarnation != binding.incarnation {
            return Err(WriterStartError::LocatorBindingMismatch);
        }
        if !path.is_absolute() {
            return Err(WriterStartError::LocatorPathIsNotAbsolute);
        }
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Self {
                binding,
                locator,
                path,
                opened_database: None,
            }),
            Ok(_) => Err(WriterStartError::LocatorPathIsNotFile),
            Err(_) => Err(WriterStartError::LocatorPathMissing),
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }
    pub(crate) fn with_opened_database(mut self, opened_database: OpenedDatabaseFile) -> Self {
        self.opened_database = Some(Arc::new(opened_database));
        self
    }
    pub(crate) fn expected_file_identity(&self) -> Option<u64> {
        self.opened_database
            .as_ref()
            .map(|opened| opened.identity())
    }
    fn worker_open_path(&self) -> Result<PathBuf, WriterStartError> {
        self.opened_database.as_ref().map_or_else(
            || Ok(self.path.clone()),
            |opened| {
                opened
                    .writer_open_path(&self.path)
                    .map_err(WriterStartError::OpenedDatabaseIdentity)
            },
        )
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub enum WriterStartError {
    InvalidAdmission(StorageRuntimeContractErrorV1),
    InvalidAdmissionLimits,
    LocatorBindingMismatch,
    LocatorPathIsNotAbsolute,
    LocatorPathMissing,
    LocatorPathIsNotFile,
    ThreadSpawn(std::io::Error),
    StartupChannelClosed,
    OpenFailed,
    /// Carries the connection-policy stage that actually failed. Collapsing
    /// every non-open policy error into one label hid which pragma rejected the
    /// connection, which is the only thing that identifies a platform-specific
    /// SQLite difference.
    ConnectionPolicyFailed(String),
    CheckpointSetupFailed,
    CheckpointSchedulerSetupFailed,
    OpenedDatabaseIdentity(OpenedDatabaseFileError),
    OpenedDatabaseIdentityMismatch {
        expected: u64,
        actual: u64,
    },
    OpenedDatabasePathMismatch,
}

impl fmt::Display for WriterStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAdmission(error) => write!(f, "invalid writer admission: {error}"),
            Self::InvalidAdmissionLimits => f.write_str("invalid writer admission limits"),
            Self::LocatorBindingMismatch => {
                f.write_str("verified SQLite locator does not bind to the runtime")
            }
            Self::LocatorPathIsNotAbsolute => {
                f.write_str("writer requires an explicit absolute SQLite path")
            }
            Self::LocatorPathMissing => f.write_str("verified SQLite path is missing"),
            Self::LocatorPathIsNotFile => f.write_str("verified SQLite path is not a regular file"),
            Self::ThreadSpawn(error) => write!(f, "failed to start SQLite writer thread: {error}"),
            Self::StartupChannelClosed => {
                f.write_str("SQLite writer thread exited before reporting startup")
            }
            Self::OpenFailed => f.write_str("failed to open verified SQLite store"),
            Self::ConnectionPolicyFailed(detail) => {
                write!(
                    f,
                    "failed to apply SQLite writer connection policy: {detail}"
                )
            }
            Self::CheckpointSetupFailed => {
                f.write_str("failed to initialize SQLite writer checkpoint policy")
            }
            Self::CheckpointSchedulerSetupFailed => {
                f.write_str("failed to initialize SQLite writer checkpoint scheduler")
            }
            Self::OpenedDatabaseIdentity(error) => {
                write!(
                    f,
                    "failed to identify opened SQLite writer database: {error}"
                )
            }
            Self::OpenedDatabaseIdentityMismatch { expected, actual } => write!(
                f,
                "SQLite writer opened file identity {actual}, expected {expected}"
            ),
            Self::OpenedDatabasePathMismatch => {
                f.write_str("SQLite writer opened a displaced pinned database path")
            }
        }
    }
}

impl Error for WriterStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidAdmission(error) => Some(error),
            Self::ThreadSpawn(error) => Some(error),
            Self::OpenedDatabaseIdentity(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum WriterActorError {
    InvalidRequest(StorageRuntimeContractErrorV1),
    ProbeBindingMismatch { field: &'static str },
    AuthorityDenied { stage: RuntimeWriteAuthorityStage },
    ReplyDropped,
    StorageFailure(StorageRuntimeErrorV1),
    IncrementalVacuumFailed(String),
    OnlineBackupFailed(WriterOnlineBackupError),
    InvalidWorkerOutcome(StorageRuntimeContractErrorV1),
    ThreadPanicked,
}

impl fmt::Display for WriterActorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(f, "invalid writer request: {error}"),
            Self::ProbeBindingMismatch { field } => {
                write!(f, "writer request probe does not match {field}")
            }
            Self::AuthorityDenied { stage } => {
                write!(f, "runtime write authority denied at {stage:?}")
            }
            Self::ReplyDropped => f.write_str("SQLite writer stopped before replying"),
            Self::StorageFailure(error) => write!(f, "SQLite writer failed: {error}"),
            Self::IncrementalVacuumFailed(message) => {
                write!(f, "SQLite incremental vacuum failed: {message}")
            }
            Self::OnlineBackupFailed(error) => write!(f, "{error}"),
            Self::InvalidWorkerOutcome(error) => {
                write!(f, "SQLite writer returned an invalid outcome: {error}")
            }
            Self::ThreadPanicked => f.write_str("SQLite writer thread panicked"),
        }
    }
}

impl Error for WriterActorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequest(error) | Self::InvalidWorkerOutcome(error) => Some(error),
            Self::StorageFailure(error) => Some(error),
            Self::OnlineBackupFailed(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) trait WriterPersistence: Send + 'static {
    fn lookup_idempotency(
        &mut self,
        transaction: &Transaction<'_>,
        binding: &StoreRuntimeBindingV1,
        idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1>;

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1>;
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterState {
    Ready = 1,
    Draining = 2,
    Closed = 3,
    Faulted = 4,
}

impl WriterState {
    fn load(state: &AtomicU8) -> Self {
        match state.load(Ordering::Acquire) {
            1 => Self::Ready,
            2 => Self::Draining,
            4 => Self::Faulted,
            _ => Self::Closed,
        }
    }

    fn unavailable_reason(self) -> UnavailableReasonV1 {
        match self {
            Self::Ready => UnavailableReasonV1::Opening,
            Self::Draining => UnavailableReasonV1::Draining,
            Self::Closed => UnavailableReasonV1::Closed,
            Self::Faulted => UnavailableReasonV1::Faulted,
        }
    }
}

pub struct PersistentWriter {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    state: Arc<AtomicU8>,
    shutdown_requested: Arc<AtomicBool>,
    sender: Mutex<Option<mpsc::Sender<AcceptedRequest>>>,
    migration_sql_sender: Mutex<Option<mpsc::Sender<MigrationSqlWriterCommand>>>,
    incremental_vacuum_sender: Mutex<Option<mpsc::Sender<IncrementalVacuumCommand>>>,
    online_backup_sender: Mutex<Option<mpsc::Sender<OnlineBackupCommand>>>,
    checkpoint_sender: Mutex<Option<mpsc::Sender<CheckpointCommand>>>,
    shutdown_sender: Option<mpsc::UnboundedSender<()>>,
    join: Option<JoinHandle<()>>,
    admission: Admission,
    telemetry: WriterTelemetry,
    watermark_source: CommitWatermarkSubscription,
    checkpoint_status: watch::Receiver<CheckpointStatus>,
    checkpoint_pressure: watch::Receiver<CheckpointPressure>,
    opened_file_identity: Option<u64>,
}

impl PersistentWriter {
    pub fn start<E>(
        locator: ExistingWriterLocator,
        admission: AdmissionConfigV1,
        executor: E,
    ) -> Result<Self, WriterStartError>
    where
        E: StorageOperationExecutor + Send + 'static,
    {
        Self::start_with_persistence(
            locator,
            admission,
            Box::new(RuntimeWriterPersistence::new(executor)),
        )
    }

    pub(crate) fn start_with_persistence(
        locator: ExistingWriterLocator,
        config: AdmissionConfigV1,
        persistence: Box<dyn WriterPersistence>,
    ) -> Result<Self, WriterStartError> {
        config
            .validate()
            .map_err(WriterStartError::InvalidAdmission)?;
        let limits = admission_limits(&config)?;
        let capacity = limits
            .general
            .operations
            .saturating_add(limits.health.operations) as usize;
        let admission = Admission::new(limits);
        let telemetry = WriterTelemetry::default();
        let state = Arc::new(AtomicU8::new(WriterState::Closed as u8));
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let binding = locator.binding().clone();
        let verified_locator = locator.verified_locator().clone();
        let path = locator.path().to_owned();
        let worker_open_path = locator.worker_open_path()?;
        let expected_file_identity = locator.expected_file_identity();
        let opened_database = locator.opened_database;
        let watermark_publisher = CommittedWatermarkPublisher::new(binding.clone());
        let watermark_source = watermark_publisher.subscribe();
        let (sender, receiver) = mpsc::channel(capacity);
        // Migration-SQL transactions are serialized by the writer actor. Keep
        // the same bounded admission depth as ordinary writes so a second
        // transaction can queue behind the active one instead of observing a
        // spurious Busy error from the transport's single-slot channel.
        let (migration_sql_sender, migration_sql_receiver) = mpsc::channel(capacity);
        let (incremental_vacuum_sender, incremental_vacuum_receiver) = mpsc::channel(1);
        let (online_backup_sender, online_backup_receiver) = mpsc::channel(1);
        let (checkpoint_sender, checkpoint_receiver) = mpsc::channel(1);
        let (shutdown_sender, shutdown_receiver) = mpsc::unbounded_channel();
        let (checkpoint_status_tx, checkpoint_status) = watch::channel(CheckpointStatus::default());
        let (checkpoint_pressure_tx, checkpoint_pressure) =
            watch::channel(CheckpointPressure::Open);
        let (started_tx, started_rx) = std_mpsc::sync_channel(1);
        let worker = Worker {
            path: worker_open_path,
            #[cfg(unix)]
            canonical_path: path.clone(),
            expected_file_identity,
            _opened_database: opened_database,
            binding: binding.clone(),
            config,
            receiver,
            migration_sql_receiver,
            incremental_vacuum_receiver,
            online_backup_receiver,
            checkpoint_receiver,
            shutdown_receiver,
            persistence,
            state: Arc::clone(&state),
            shutdown_requested: Arc::clone(&shutdown_requested),
            telemetry: telemetry.clone(),
            watermark_publisher,
            checkpoint_status: checkpoint_status_tx,
            checkpoint_pressure: checkpoint_pressure_tx,
            started: started_tx,
        };
        let join = thread::Builder::new()
            .name("tracedecay-rusqlite-writer".to_owned())
            .spawn(move || worker.run())
            .map_err(WriterStartError::ThreadSpawn)?;
        match started_rx.recv() {
            Ok(Ok(opened_file_identity)) => Ok(Self {
                binding,
                verified_locator,
                path,
                state,
                shutdown_requested,
                sender: Mutex::new(Some(sender)),
                migration_sql_sender: Mutex::new(Some(migration_sql_sender)),
                incremental_vacuum_sender: Mutex::new(Some(incremental_vacuum_sender)),
                online_backup_sender: Mutex::new(Some(online_backup_sender)),
                checkpoint_sender: Mutex::new(Some(checkpoint_sender)),
                shutdown_sender: Some(shutdown_sender),
                join: Some(join),
                admission,
                telemetry,
                watermark_source,
                checkpoint_status,
                checkpoint_pressure,
                opened_file_identity,
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(WriterStartError::StartupChannelClosed)
            }
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    pub(crate) fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }
    pub(crate) fn migration_sql_sender(&self) -> Option<mpsc::Sender<MigrationSqlWriterCommand>> {
        if self.state() != WriterState::Ready {
            return None;
        }
        self.migration_sql_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub fn opened_file_identity(&self) -> Option<u64> {
        self.opened_file_identity
    }
    pub fn state(&self) -> WriterState {
        WriterState::load(&self.state)
    }
    pub fn telemetry_snapshot(&self) -> WriterTelemetrySnapshot {
        self.telemetry.snapshot()
    }

    /// Returns a read-only view of this writer's committed watermark.
    pub fn commit_watermark_source(&self) -> CommitWatermarkSubscription {
        self.watermark_source.clone()
    }

    pub fn checkpoint_handle(&self) -> CheckpointHandle {
        CheckpointHandle {
            binding: self.binding.clone(),
            state: Arc::clone(&self.state),
            shutdown_requested: Arc::clone(&self.shutdown_requested),
            sender: self
                .checkpoint_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            status: self.checkpoint_status.clone(),
            pressure: self.checkpoint_pressure.clone(),
        }
    }

    pub async fn submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Result<RuntimeSubmitOutcomeV1, WriterActorError> {
        self.submit_authorized(request, probe, Arc::new(UnrestrictedRuntimeWriteAuthority))
            .await
    }

    pub async fn submit_authorized(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<RuntimeSubmitOutcomeV1, WriterActorError> {
        let request = Arc::new(request);
        request
            .validate()
            .map_err(WriterActorError::InvalidRequest)?;
        settlement::validate_probe(&request, probe.as_ref())?;
        if let Some(outcome) = settlement::interruption_outcome(
            &request,
            probe.as_ref(),
            RuntimeCancellationStageV1::BeforeAdmission,
        ) {
            return Ok(outcome);
        }
        if let Some(outcome) = settlement::binding_outcome(&self.binding, &request) {
            return Ok(outcome);
        }
        if authority
            .verify(RuntimeWriteAuthorityStage::BeforeAdmission)
            .is_err()
        {
            return Ok(settlement::missing_authority());
        }
        if self.state() != WriterState::Ready {
            return Ok(self.unavailable());
        }

        self.telemetry.offered();
        let permit = match self.admission.reserve(&request.envelope().metadata) {
            Ok(permit) => permit,
            Err(scope) => {
                self.telemetry.shed();
                return Ok(settlement::saturation(&request, scope));
            }
        };
        let bytes = request.envelope().metadata.admission_bytes;
        self.telemetry.admitted(bytes);
        let (reply, response) = oneshot::channel();
        let accepted = AcceptedRequest::new(request.clone(), probe, authority, reply, permit);
        let send_result = {
            let sender = self
                .sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.state() != WriterState::Ready {
                Err((accepted, false))
            } else if let Some(sender) = sender.as_ref() {
                sender.try_send(accepted).map_err(|error| match error {
                    mpsc::error::TrySendError::Full(item) => (item, true),
                    mpsc::error::TrySendError::Closed(item) => (item, false),
                })
            } else {
                Err((accepted, false))
            }
        };
        if let Err((accepted, saturated)) = send_result {
            self.telemetry.released(1, bytes);
            let outcome = if saturated {
                settlement::saturation(
                    &request,
                    tracedecay_store::SaturationScopeV1::ShardOperations,
                )
            } else {
                self.unavailable()
            };
            self.telemetry.completed(&Ok(outcome.clone()));
            drop(accepted);
            return Ok(outcome);
        }
        let outcome = response
            .await
            .map_err(|_| WriterActorError::ReplyDropped)?
            .map_err(WriterActorError::StorageFailure)?;
        outcome
            .validate_for(&request)
            .map_err(WriterActorError::InvalidWorkerOutcome)?;
        Ok(outcome)
    }

    pub async fn bounded_incremental_vacuum(
        &self,
        max_pages: u32,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<(), WriterActorError> {
        authority
            .verify(RuntimeWriteAuthorityStage::BeforeAdmission)
            .map_err(|_| WriterActorError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeAdmission,
            })?;
        if self.state() != WriterState::Ready {
            return Err(WriterActorError::IncrementalVacuumFailed(
                "writer is unavailable".to_owned(),
            ));
        }
        let (reply, response) = oneshot::channel();
        let command = IncrementalVacuumCommand::new(max_pages, authority, reply);
        let send_result = {
            let sender = self
                .incremental_vacuum_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.state() != WriterState::Ready {
                Err(mpsc::error::TrySendError::Closed(command))
            } else if let Some(sender) = sender.as_ref() {
                sender.try_send(command)
            } else {
                Err(mpsc::error::TrySendError::Closed(command))
            }
        };
        send_result.map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                WriterActorError::IncrementalVacuumFailed("command channel is busy".to_owned())
            }
            mpsc::error::TrySendError::Closed(_) => {
                WriterActorError::IncrementalVacuumFailed("writer is unavailable".to_owned())
            }
        })?;
        response.await.map_err(|_| WriterActorError::ReplyDropped)?
    }

    pub async fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<OnlineBackupReceipt, WriterActorError> {
        self.enqueue_online_backup(destination, None, authority)
            .await
    }

    pub async fn snapshot_to_interruptible(
        &self,
        destination: PathBuf,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<OnlineBackupReceipt, WriterActorError> {
        self.enqueue_online_backup(destination, Some(probe), authority)
            .await
    }

    async fn enqueue_online_backup(
        &self,
        destination: PathBuf,
        probe: Option<Arc<dyn RuntimeRequestProbeV1>>,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<OnlineBackupReceipt, WriterActorError> {
        authority
            .verify(RuntimeWriteAuthorityStage::BeforeAdmission)
            .map_err(|_| WriterActorError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeAdmission,
            })?;
        if let Some(interruption) = probe.as_ref().and_then(|probe| probe.interruption()) {
            return Err(WriterActorError::OnlineBackupFailed(match interruption {
                tracedecay_store::RuntimeInterruptionV1::Cancelled => {
                    WriterOnlineBackupError::Cancelled
                }
                tracedecay_store::RuntimeInterruptionV1::DeadlineExceeded => {
                    WriterOnlineBackupError::DeadlineExceeded
                }
            }));
        }
        let destination = validate_destination(&self.path, &destination)
            .map_err(WriterActorError::OnlineBackupFailed)?;
        if self.state() != WriterState::Ready {
            return Err(WriterActorError::OnlineBackupFailed(
                WriterOnlineBackupError::WriterShuttingDown,
            ));
        }
        let (reply, response) = oneshot::channel();
        let command = OnlineBackupCommand::new(destination, probe, authority, reply);
        let send_result = {
            let sender = self
                .online_backup_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.state() != WriterState::Ready {
                Err(mpsc::error::TrySendError::Closed(command))
            } else if let Some(sender) = sender.as_ref() {
                sender.try_send(command)
            } else {
                Err(mpsc::error::TrySendError::Closed(command))
            }
        };
        send_result.map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Busy)
            }
            mpsc::error::TrySendError::Closed(_) => {
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::WriterShuttingDown)
            }
        })?;
        response.await.map_err(|_| WriterActorError::ReplyDropped)?
    }

    fn unavailable(&self) -> RuntimeSubmitOutcomeV1 {
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: self.state().unavailable_reason(),
        }
    }

    pub fn begin_drain(&self) {
        let _ = self.state.compare_exchange(
            WriterState::Ready as u8,
            WriterState::Draining as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.migration_sql_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.incremental_vacuum_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.online_backup_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    pub fn shutdown_and_join(mut self) -> Result<(), WriterActorError> {
        self.begin_drain();
        self.request_shutdown();
        self.join_worker()
    }

    fn request_shutdown(&mut self) {
        self.shutdown_requested.store(true, Ordering::Release);
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        self.checkpoint_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    fn join_worker(&mut self) -> Result<(), WriterActorError> {
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| WriterActorError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for PersistentWriter {
    fn drop(&mut self) {
        self.begin_drain();
        self.request_shutdown();
        let _ = self.join_worker();
    }
}

fn admission_limits(config: &AdmissionConfigV1) -> Result<Limits, WriterStartError> {
    Limits::new(
        Capacity {
            operations: config.per_shard_queue.max_operations,
            bytes: config.per_shard_queue.max_bytes,
        },
        Capacity {
            operations: DEFAULT_RESERVED_HEALTH_OPERATIONS,
            bytes: DEFAULT_RESERVED_HEALTH_BYTES,
        },
        config.foreground_batch.max_bytes,
        config.background_batch.max_bytes,
    )
    .ok_or(WriterStartError::InvalidAdmissionLimits)
}
