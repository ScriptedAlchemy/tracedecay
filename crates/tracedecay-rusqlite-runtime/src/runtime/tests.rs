use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use tracedecay_store::{LocatorDigest, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::*;
use crate::{
    CheckpointPressure, CheckpointStatus, WriterState,
    watermark::{CommitWatermarkSubscription, CommittedWatermarkPublisher},
};

type Events = Arc<Mutex<Vec<&'static str>>>;

#[derive(Clone)]
struct FakeWriter {
    binding: StoreRuntimeBindingV1,
    events: Events,
    watermarks: CommitWatermarkSubscription,
}

impl RuntimeComponent for FakeWriter {
    type Telemetry = &'static str;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    fn telemetry_snapshot(&self) -> Self::Telemetry {
        "writer"
    }
}

impl RuntimeWriter for FakeWriter {
    fn state(&self) -> WriterState {
        WriterState::Ready
    }
    fn committed_watermarks(&self) -> CommitWatermarkSubscription {
        self.watermarks.clone()
    }
    fn begin_drain(&self) {
        self.events.lock().unwrap().push("writer_drain");
    }
    fn shutdown_and_join(self) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push("writer_join");
        Ok(())
    }
}

struct FakeReaders {
    binding: StoreRuntimeBindingV1,
    events: Events,
}

impl RuntimeComponent for FakeReaders {
    type Telemetry = &'static str;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    fn telemetry_snapshot(&self) -> Self::Telemetry {
        "readers"
    }
}

impl RuntimeReaders for FakeReaders {
    fn begin_drain(&self) {
        self.events.lock().unwrap().push("readers_drain");
    }
    fn shutdown_and_join(self) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push("readers_join");
        Ok(())
    }
}

struct FakeCapability {
    binding: StoreRuntimeBindingV1,
    events: Events,
    name: &'static str,
}

impl RuntimeComponent for FakeCapability {
    type Telemetry = &'static str;

    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    fn telemetry_snapshot(&self) -> Self::Telemetry {
        self.name
    }
}

impl RuntimeControl for FakeCapability {
    fn close(&mut self) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push(match self.name {
            "checkpoint" => "checkpoint_close",
            "maintenance" => "maintenance_close",
            "backup" => "backup_close",
            _ => "repair_close",
        });
        Ok(())
    }
}

impl CheckpointControl for FakeCapability {
    type Request = ();
    type Outcome = ();
    type MaintenanceRequest = ();
    type Status = CheckpointStatus;
    type Pressure = CheckpointPressure;

    fn checkpoint_status(&self) -> Self::Status {
        CheckpointStatus::default()
    }

    fn checkpoint_pressure(&self) -> Self::Pressure {
        CheckpointPressure::Open
    }

    fn trigger_checkpoint(&mut self, (): ()) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push("checkpoint_trigger");
        Ok(())
    }

    fn trigger_maintenance_checkpoint(&mut self, (): ()) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push("checkpoint_maintenance");
        Ok(())
    }
}

impl MaintenanceControl for FakeCapability {
    type Request = ();
    type Outcome = ();
    fn run_maintenance(&mut self, (): ()) -> Result<(), RuntimePortError> {
        self.events.lock().unwrap().push("maintenance_run");
        Ok(())
    }
}

impl BackupControl for FakeCapability {
    type Request = ();
    type Outcome = ();
    fn run_backup(&mut self, (): ()) -> Result<(), RuntimePortError> {
        Ok(())
    }
}

impl RepairControl for FakeCapability {
    type Request = ();
    type Outcome = ();
    fn run_repair(&mut self, (): ()) -> Result<(), RuntimePortError> {
        Ok(())
    }
}

struct FakeAttachment {
    events: Events,
}

impl ShardRuntimeAttachment for FakeAttachment {
    type Writer = FakeWriter;
    type Readers = FakeReaders;
    type Checkpoint = FakeCapability;
    type Maintenance = FakeCapability;
    type Backup = FakeCapability;
    type Repair = FakeCapability;
    type Error = Infallible;

    fn attach(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        _locator: &VerifiedStoreLocatorV1,
    ) -> Result<
        ShardRuntimeParts<
            FakeWriter,
            FakeReaders,
            FakeCapability,
            FakeCapability,
            FakeCapability,
            FakeCapability,
        >,
        Self::Error,
    > {
        self.events.lock().unwrap().push("attach");
        let capability = |name| FakeCapability {
            binding: binding.clone(),
            events: Arc::clone(&self.events),
            name,
        };
        Ok(ShardRuntimeParts {
            writer: FakeWriter {
                binding: binding.clone(),
                events: Arc::clone(&self.events),
                watermarks: CommittedWatermarkPublisher::new(binding.clone()).subscribe(),
            },
            readers: FakeReaders {
                binding: binding.clone(),
                events: Arc::clone(&self.events),
            },
            checkpoint: capability("checkpoint"),
            maintenance: capability("maintenance"),
            backup: capability("backup"),
            repair: capability("repair"),
        })
    }
}

type Engine = ShardRuntimeEngine<
    FakeWriter,
    FakeReaders,
    FakeCapability,
    FakeCapability,
    FakeCapability,
    FakeCapability,
>;

fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.runtime",
            "profile_id": "profile.runtime",
            "scope": { "kind": "project", "project_id": "project.runtime" }
        },
        "incarnation": 3,
        "authority_epoch": 8
    }))
    .unwrap()
}

fn start(events: Events) -> Engine {
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    );
    Engine::start(&mut FakeAttachment { events }, &binding, &locator).unwrap()
}

#[test]
fn start_owns_writer_and_reader_pool_and_reports_physical_telemetry() {
    let events = Events::default();
    let engine = start(Arc::clone(&events));
    assert_eq!(engine.state(), ShardRuntimeState::Ready);
    assert_eq!(engine.writer().binding(), engine.readers().binding());
    let telemetry = engine.telemetry_snapshot();
    assert_eq!(telemetry.writer, "writer");
    assert_eq!(telemetry.readers, "readers");
    assert_eq!(*events.lock().unwrap(), ["attach"]);
}

#[test]
fn checkpoint_trigger_is_routed_to_the_owned_control() {
    let events = Events::default();
    let mut engine = start(Arc::clone(&events));
    assert_eq!(engine.checkpoint_status(), CheckpointStatus::default());
    assert_eq!(engine.checkpoint_pressure(), CheckpointPressure::Open);
    engine.trigger_checkpoint(()).unwrap();
    engine.trigger_maintenance_checkpoint(()).unwrap();
    assert!(events.lock().unwrap().contains(&"checkpoint_trigger"));
    assert!(events.lock().unwrap().contains(&"checkpoint_maintenance"));
}

#[test]
fn maintenance_is_fenced_by_drain_before_action() {
    let events = Events::default();
    let mut engine = start(Arc::clone(&events));
    engine.run_maintenance(()).unwrap();
    assert_eq!(engine.state(), ShardRuntimeState::Draining);
    assert_eq!(
        &events.lock().unwrap()[1..],
        ["writer_drain", "readers_drain", "maintenance_run"]
    );
}

#[test]
fn shutdown_drains_joins_and_closes_every_owner_exactly_once() {
    let events = Events::default();
    start(Arc::clone(&events)).shutdown_and_join().unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            "attach",
            "writer_drain",
            "readers_drain",
            "readers_join",
            "checkpoint_close",
            "maintenance_close",
            "backup_close",
            "repair_close",
            "writer_join",
        ]
    );
}
