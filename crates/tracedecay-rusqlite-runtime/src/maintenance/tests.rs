use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use tracedecay_store::{
    BrainId, ProjectId, RuntimeMaintenanceStateV1, RuntimePublicationIdV1, SnapshotLeaseIdV1,
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreRuntimeRegistryPublicationV1, StoreShardIdV1, UserProfileId,
};

use crate::checkpoint::{CheckpointBlocker, CheckpointBlockers};

use super::*;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn binding(incarnation: u64, epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            id::<BrainId>("brain.maintenance"),
            id::<UserProfileId>("profile.maintenance"),
            id::<ProjectId>("project.maintenance"),
        ),
        StoreIncarnationV1::new(incarnation).unwrap(),
        StoreAuthorityEpochV1::new(epoch).unwrap(),
    )
}

fn owner(value: u64) -> MaintenanceOwnerId {
    MaintenanceOwnerId::new(value).unwrap()
}

#[test]
fn maintenance_errors_are_reportable_without_losing_driver_codes() {
    assert_eq!(
        MaintenanceError::WrongOwner.to_string(),
        "maintenance request belongs to a different owner"
    );
    assert_eq!(
        MaintenanceError::Driver(DriverMaintenanceError {
            code: "checkpoint_busy",
            retryable: true,
        })
        .to_string(),
        "maintenance driver failed: checkpoint_busy (retryable)"
    );
}

fn publication(sequence: u64, incarnation: u64, epoch: u64) -> StoreRuntimeRegistryPublicationV1 {
    let publication_id = RuntimePublicationIdV1::new(format!("publication.{sequence}")).unwrap();
    serde_json::from_value(serde_json::json!({
        "publication_id": publication_id,
        "binding": binding(incarnation, epoch),
        "published_at": sequence,
    }))
    .unwrap()
}

fn request(
    owner: MaintenanceOwnerId,
    expected: StoreRuntimeRegistryPublicationV1,
) -> MaintenanceRequest {
    MaintenanceRequest {
        owner,
        expected,
        action: MaintenanceAction::Migration {
            plan: MigrationPlanId::new("migration.v2").unwrap(),
        },
    }
}

fn request_for(
    owner: MaintenanceOwnerId,
    expected: StoreRuntimeRegistryPublicationV1,
    action: MaintenanceAction,
) -> MaintenanceRequest {
    MaintenanceRequest {
        owner,
        expected,
        action,
    }
}

struct FakeState {
    publication: StoreRuntimeRegistryPublicationV1,
    state: RuntimeMaintenanceStateV1,
    blockers: DrainBlockers,
    events: Vec<&'static str>,
    fail_begin_reopen: bool,
    fail_finish_reopen: bool,
}

struct FakeLifecycle {
    inner: Mutex<FakeState>,
}

impl FakeLifecycle {
    fn ready(publication: StoreRuntimeRegistryPublicationV1) -> Self {
        Self {
            inner: Mutex::new(FakeState {
                publication,
                state: RuntimeMaintenanceStateV1::Ready,
                blockers: DrainBlockers::default(),
                events: Vec::new(),
                fail_begin_reopen: false,
                fail_finish_reopen: false,
            }),
        }
    }

    fn set_blockers(&self, blockers: DrainBlockers) {
        self.inner.lock().unwrap().blockers = blockers;
    }

    fn events(&self) -> Vec<&'static str> {
        self.inner.lock().unwrap().events.clone()
    }
}

impl MaintenanceLifecycle for FakeLifecycle {
    fn publication(&self) -> StoreRuntimeRegistryPublicationV1 {
        self.inner.lock().unwrap().publication.clone()
    }

    fn state(&self) -> RuntimeMaintenanceStateV1 {
        self.inner.lock().unwrap().state
    }

    fn stop_admissions_and_begin_drain(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<(), MaintenanceError> {
        let mut state = self.inner.lock().unwrap();
        assert_eq!(&state.publication, expected);
        assert_eq!(state.state, RuntimeMaintenanceStateV1::Ready);
        state.state = RuntimeMaintenanceStateV1::Draining;
        state.events.push("draining");
        Ok(())
    }

    fn drain_blockers(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
    ) -> Result<DrainBlockers, MaintenanceError> {
        let state = self.inner.lock().unwrap();
        assert_eq!(&state.publication, expected);
        Ok(state.blockers.clone())
    }

    fn enter_exclusive(
        &self,
        expected: &StoreRuntimeRegistryPublicationV1,
        _owner: MaintenanceOwnerId,
    ) -> Result<(), MaintenanceError> {
        let mut state = self.inner.lock().unwrap();
        assert!(state.blockers.is_clear());
        assert_eq!(state.state, RuntimeMaintenanceStateV1::Draining);
        assert_eq!(&state.publication, expected);
        state.state = RuntimeMaintenanceStateV1::ExclusiveMaintenance;
        state.events.push("exclusive");
        Ok(())
    }

    fn reopen(
        &self,
        permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        let mut state = self.inner.lock().unwrap();
        assert_eq!(permit.publication(), &state.publication);
        if state.fail_begin_reopen {
            return Err(MaintenanceError::Lifecycle {
                stage: "begin reopen",
            });
        }
        assert_eq!(state.state, RuntimeMaintenanceStateV1::ExclusiveMaintenance);
        state.publication = receipt.publication.clone();
        state.state = RuntimeMaintenanceStateV1::Reopening;
        state.events.push("reopening");
        if state.fail_finish_reopen {
            return Err(MaintenanceError::Lifecycle {
                stage: "finish reopen",
            });
        }
        assert_eq!(state.state, RuntimeMaintenanceStateV1::Reopening);
        state.state = RuntimeMaintenanceStateV1::Ready;
        state.events.push("ready");
        Ok(receipt)
    }

    fn fault(
        &self,
        permit: ExclusiveMaintenancePermit,
        receipt: ReplacementPublicationReceipt,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        let mut state = self.inner.lock().unwrap();
        assert_eq!(
            permit.binding().shard_id,
            receipt.publication.binding.shard_id
        );
        state.publication = receipt.publication.clone();
        state.state = RuntimeMaintenanceStateV1::Faulted;
        state.events.push("faulted");
        Ok(receipt)
    }
}

struct FakeCanonicalAuthority {
    publications: Mutex<Vec<StoreRuntimeRegistryPublicationV1>>,
    requests: Mutex<Vec<ReplacementPublicationRequest>>,
}

impl FakeCanonicalAuthority {
    fn with_replacements(mut publications: Vec<StoreRuntimeRegistryPublicationV1>) -> Arc<Self> {
        publications.reverse();
        Arc::new(Self {
            publications: Mutex::new(publications),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl CanonicalRegistryAuthority for FakeCanonicalAuthority {
    fn request_replacement(
        &self,
        request: &ReplacementPublicationRequest,
    ) -> Result<ReplacementPublicationReceipt, MaintenanceError> {
        self.requests.lock().unwrap().push(request.clone());
        let publication = self.publications.lock().unwrap().pop().ok_or(
            MaintenanceError::CanonicalAuthority {
                stage: "fake publication queue exhausted",
            },
        )?;
        Ok(ReplacementPublicationReceipt {
            request: request.clone(),
            publication,
        })
    }
}

fn coordinator(replacements: Vec<StoreRuntimeRegistryPublicationV1>) -> MaintenanceCoordinator {
    MaintenanceCoordinator::new(FakeCanonicalAuthority::with_replacements(replacements))
}

#[derive(Default)]
struct FakeDriver {
    calls: Vec<&'static str>,
    fail: bool,
}

impl FakeDriver {
    fn result(&mut self, call: &'static str) -> Result<(), DriverMaintenanceError> {
        self.calls.push(call);
        if self.fail {
            Err(DriverMaintenanceError {
                code: "driver_failed",
                retryable: false,
            })
        } else {
            Ok(())
        }
    }
}

impl MaintenanceDriver for FakeDriver {
    fn migrate(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _plan: &MigrationPlanId,
    ) -> Result<(), DriverMaintenanceError> {
        self.result("migration")
    }

    fn rebuild_fts(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _index: &FtsIndexId,
    ) -> Result<(), DriverMaintenanceError> {
        self.result("fts")
    }

    fn restore(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        self.result("restore")
    }

    fn compact(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _mode: CompactionMode,
    ) -> Result<(), DriverMaintenanceError> {
        self.result("compaction")
    }

    fn replace_shard(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        _artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        self.result("replacement")
    }
}

#[test]
fn legal_transition_sequence_reopens_with_stronger_fence() {
    let old = publication(1, 4, 9);
    let replacement = publication(2, 8, 20);
    let lifecycle = FakeLifecycle::ready(old.clone());
    let coordinator = coordinator(vec![replacement.clone()]);
    let owner = owner(1);
    assert_eq!(
        coordinator.start(&lifecycle, request(owner, old), &|_| false),
        Ok(MaintenanceStart::Started)
    );

    let mut driver = FakeDriver::default();
    let progress = coordinator
        .advance(owner, &lifecycle, &mut driver, &|_| false)
        .unwrap();
    let MaintenanceProgress::Reopened {
        publication,
        action_performed,
    } = progress
    else {
        panic!("maintenance did not reopen")
    };
    assert!(action_performed);
    assert_eq!(publication.publication, replacement);
    assert_eq!(driver.calls, ["migration"]);
    assert_eq!(
        lifecycle.events(),
        ["draining", "exclusive", "reopening", "ready"]
    );
}

#[test]
fn drain_blockers_retain_the_single_owner_and_prevent_driver_work() {
    let old = publication(1, 1, 1);
    let lifecycle = FakeLifecycle::ready(old.clone());
    lifecycle.set_blockers(DrainBlockers {
        admissions: 2,
        readers: 3,
        snapshots: CheckpointBlockers {
            blockers: vec![CheckpointBlocker {
                lease_id: SnapshotLeaseIdV1::try_from("lease.maintenance".to_owned()).unwrap(),
                age: std::time::Duration::from_secs(1),
            }],
            omitted: 0,
        },
        writer_active: true,
    });
    let coordinator = coordinator(vec![publication(2, 2, 2)]);
    let first = owner(1);
    let second = owner(2);
    coordinator
        .start(&lifecycle, request(first, old.clone()), &|_| false)
        .unwrap();
    assert!(matches!(
        coordinator.start(&lifecycle, request(second, old), &|_| false),
        Err(MaintenanceError::AlreadyOwned { owner }) if owner == first
    ));

    let mut driver = FakeDriver::default();
    assert!(matches!(
        coordinator.advance(first, &lifecycle, &mut driver, &|boundary| {
            boundary == CancellationBoundary::AwaitingDrain
        }),
        Ok(MaintenanceProgress::Blocked {
            blockers,
            cancellation_recorded: true,
        }) if !blockers.is_clear()
    ));
    assert!(driver.calls.is_empty());
    assert_eq!(lifecycle.events(), ["draining"]);

    lifecycle.set_blockers(DrainBlockers::default());
    assert!(matches!(
        coordinator.advance(first, &lifecycle, &mut driver, &|_| false),
        Ok(MaintenanceProgress::Reopened {
            action_performed: false,
            ..
        })
    ));
}

#[test]
fn driver_and_reopen_failures_publish_faulted_without_deletion_fallback() {
    let old = publication(1, 1, 2);
    let lifecycle = FakeLifecycle::ready(old.clone());
    let faulted = publication(2, 4, 9);
    let maintenance = coordinator(vec![faulted.clone()]);
    let owner = owner(7);
    maintenance
        .start(&lifecycle, request(owner, old), &|_| false)
        .unwrap();
    let mut driver = FakeDriver {
        fail: true,
        ..FakeDriver::default()
    };
    assert!(matches!(
        maintenance.advance(owner, &lifecycle, &mut driver, &|_| false),
        Ok(MaintenanceProgress::Faulted {
            error: MaintenanceError::Driver(_),
            publication,
        }) if matches!(
            publication.as_ref(),
            PublicationAttempt {
                receipt: Some(_),
                ..
            }
        )
    ));
    assert_eq!(lifecycle.state(), RuntimeMaintenanceStateV1::Faulted);
    assert_eq!(lifecycle.publication(), faulted);
    assert_eq!(lifecycle.events(), ["draining", "exclusive", "faulted"]);
}

#[test]
fn cancellation_is_observed_only_at_safe_boundaries() {
    let old = publication(1, 1, 1);
    let lifecycle = FakeLifecycle::ready(old.clone());
    let coordinator = coordinator(vec![publication(2, 2, 2)]);
    assert_eq!(
        coordinator.start(&lifecycle, request(owner(1), old.clone()), &|boundary| {
            boundary == CancellationBoundary::BeforeDrain
        }),
        Ok(MaintenanceStart::Cancelled)
    );
    assert!(lifecycle.events().is_empty());

    coordinator
        .start(&lifecycle, request(owner(1), old), &|_| false)
        .unwrap();
    let mut driver = FakeDriver::default();
    let progress = coordinator
        .advance(owner(1), &lifecycle, &mut driver, &|boundary| {
            boundary == CancellationBoundary::BeforeAction
        })
        .unwrap();
    assert!(matches!(
        progress,
        MaintenanceProgress::Reopened {
            action_performed: false,
            ..
        }
    ));
    assert!(driver.calls.is_empty());
}

#[test]
fn permit_requires_clear_drain_proof_and_cannot_be_duplicated() {
    let current = publication(1, 1, 1);
    let lifecycle = FakeLifecycle::ready(current.clone());
    lifecycle.stop_admissions_and_begin_drain(&current).unwrap();
    lifecycle.enter_exclusive(&current, owner(1)).unwrap();
    let drained = DrainedStateProof::observe(current.clone(), DrainBlockers::default()).unwrap();
    let permit =
        ExclusiveMaintenancePermit::issue_after_drain(owner(1), current.clone(), drained).unwrap();
    assert_eq!(permit.publication(), &current);

    fn consume_once(_: ExclusiveMaintenancePermit) {}
    consume_once(permit);

    assert!(
        DrainedStateProof::observe(
            current,
            DrainBlockers {
                readers: 1,
                ..DrainBlockers::default()
            }
        )
        .is_err()
    );
}

#[test]
fn closed_driver_menu_routes_every_maintenance_action() {
    let shard = binding(1, 1).shard_id;
    let artifact = VerifiedMaintenanceArtifact {
        artifact_id: "artifact.maintenance".to_owned(),
        shard_id: shard,
        generation: 1,
    };
    let actions = [
        (
            MaintenanceAction::FtsRebuild {
                index: FtsIndexId::new("fts.code").unwrap(),
            },
            "fts",
        ),
        (
            MaintenanceAction::Restore {
                artifact: artifact.clone(),
            },
            "restore",
        ),
        (
            MaintenanceAction::Compaction {
                mode: CompactionMode::Incremental,
            },
            "compaction",
        ),
        (
            MaintenanceAction::Compaction {
                mode: CompactionMode::Full,
            },
            "compaction",
        ),
        (
            MaintenanceAction::ShardReplacement { artifact },
            "replacement",
        ),
    ];

    for (sequence, (action, expected_call)) in actions.into_iter().enumerate() {
        let sequence = u64::try_from(sequence).unwrap() * 2 + 1;
        let current = publication(sequence, sequence, sequence);
        let replacement = publication(sequence + 1, sequence + 1, sequence + 1);
        let lifecycle = FakeLifecycle::ready(current.clone());
        let maintenance = coordinator(vec![replacement]);
        let owner = owner(sequence);
        maintenance
            .start(&lifecycle, request_for(owner, current, action), &|_| false)
            .unwrap();
        let mut driver = FakeDriver::default();

        assert!(matches!(
            maintenance.advance(owner, &lifecycle, &mut driver, &|_| false),
            Ok(MaintenanceProgress::Reopened {
                action_performed: true,
                ..
            })
        ));
        assert_eq!(driver.calls, [expected_call]);
    }
}
