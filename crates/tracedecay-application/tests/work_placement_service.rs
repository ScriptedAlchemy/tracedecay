//! Placement lowering contract: exclusivity of a managed root, blocked
//! admission, idempotent re-admission, and release that quarantines rather
//! than deletes.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "Placement, topology, and safe Git effects") requires linked and isolated
//! placements to be "canonical, exclusive, fenced ... and retained/quarantined
//! rather than cleaned when dirty, conflicted, unknown, or uniquely valuable",
//! and states that "retention expiry is eligibility for a fresh cleanup
//! preflight, not delete authority".

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AdmitWorkPlacementCommand, ApplicationProblem, ApplicationProblemKind, CancellationContext,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, ReleaseWorkPlacementCommand,
    RequestContext, RequestId, ResolvedScope, WorkPlacementPreflightRequestV1,
    WorkPlacementReadingV1, WorkPlacementService, WorkPlacementStatusRequestV1,
    WorkPlacementStorageError, WorkPlacementStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, UtcMicros, WorkAuthority,
    WorkPlacementBlockerV1, WorkPlacementIdentityV1, WorkPlacementKindV1,
    WorkPlacementObservationV1, WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

/// Platform-absolute fixture root: the placement contracts require
/// `Path::is_absolute`, which a bare `/...` literal fails on Windows.
fn fixture_root() -> String {
    if cfg!(windows) {
        "C:\\workspace\\linked-placement".to_owned()
    } else {
        "/workspace/linked-placement".to_owned()
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.placement"),
        id::<RepositoryId>("repository.placement"),
        id::<WorktreeId>("worktree.placement"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.admit_placement").unwrap();
    let use_case = UseCaseId::new("use-case.work.admit_placement").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.placement"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(100_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.placement.{actor}")).unwrap(),
        Deadline::new(UtcMicros(90_000)).unwrap(),
        CancellationContext::active(format!("cancel.placement.{actor}")).unwrap(),
    )
    .unwrap()
}

fn authority_of(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap()
}

fn linked() -> WorkPlacementTargetV1 {
    WorkPlacementTargetV1::new(
        WorkPlacementKindV1::LinkedWorktree,
        Some(fixture_root()),
        false,
        true,
    )
    .unwrap()
}

fn clean() -> WorkPlacementObservationV1 {
    WorkPlacementObservationV1 {
        dirty_tracked_paths: 0,
        untracked_paths: 0,
        unique_commits: Some(0),
        readable: true,
        active_holder: false,
        network_required: false,
        observed_at: UtcMicros(100),
    }
}

fn observer(
    observation: WorkPlacementObservationV1,
) -> impl FnOnce(&WorkPlacementTargetV1) -> Result<WorkPlacementObservationV1, ApplicationProblem> {
    move |_target| Ok(observation)
}

type PlacementKey = (WorkAuthority, WorkPlacementIdentityV1);

#[derive(Clone, Default)]
struct TestStore {
    placements: Arc<Mutex<BTreeMap<PlacementKey, WorkPlacementV1>>>,
}

impl WorkPlacementStoragePort for TestStore {
    fn load_placement(
        &self,
        authority: &WorkAuthority,
        identity: &WorkPlacementIdentityV1,
    ) -> Result<Option<WorkPlacementV1>, WorkPlacementStorageError> {
        Ok(self
            .placements
            .lock()
            .unwrap()
            .get(&(authority.clone(), identity.clone()))
            .cloned())
    }

    fn target_holder(
        &self,
        authority: &WorkAuthority,
        root: &str,
    ) -> Result<Option<WorkPlacementIdentityV1>, WorkPlacementStorageError> {
        Ok(self
            .placements
            .lock()
            .unwrap()
            .iter()
            .find(|((stored_authority, _), placement)| {
                stored_authority == authority
                    && placement.holds_target()
                    && placement.target().root() == Some(root)
            })
            .map(|((_, identity), _)| identity.clone()))
    }

    fn publish_placement(
        &self,
        authority: &WorkAuthority,
        expected: Option<u64>,
        next: &WorkPlacementV1,
    ) -> Result<(), WorkPlacementStorageError> {
        let mut placements = self.placements.lock().unwrap();
        let key = (authority.clone(), next.identity().clone());
        let current = placements.get(&key).map(WorkPlacementV1::authority_version);
        if current != expected {
            return Err(WorkPlacementStorageError::AuthorityConflict);
        }
        placements.insert(key, next.clone());
        Ok(())
    }
}

fn identity(run: &str) -> WorkPlacementIdentityV1 {
    WorkPlacementIdentityV1::new(id::<TaskId>("task.placement"), id::<RunId>(run))
}

fn admit_command(run: &str, at: i64) -> AdmitWorkPlacementCommand {
    AdmitWorkPlacementCommand {
        task_id: id::<TaskId>("task.placement"),
        run_id: id::<RunId>(run),
        target: linked(),
        retention_eligible_at: Some(UtcMicros(50_000)),
        occurred_at: UtcMicros(at),
    }
}

#[test]
fn a_run_with_no_placement_reads_absent_rather_than_an_empty_placement() {
    let service = WorkPlacementService::new(TestStore::default());
    let context = context("actor.placement.absent");
    let reading = service
        .status(
            &context,
            &WorkPlacementStatusRequestV1 {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.absent"),
            },
        )
        .expect("status");
    assert_eq!(reading, WorkPlacementReadingV1::Absent);
}

#[test]
fn a_clean_preflight_admits_and_re_admission_of_the_same_target_replays() {
    let store = TestStore::default();
    let service = WorkPlacementService::new(store.clone());
    let context = context("actor.placement.admit");

    let preflight = service
        .preflight(
            &context,
            WorkPlacementPreflightRequestV1 {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                target: linked(),
                occurred_at: UtcMicros(100),
            },
            observer(clean()),
        )
        .expect("preflight");
    assert!(preflight.is_admissible());

    let placement = service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 200),
            observer(clean()),
        )
        .expect("admit");
    assert_eq!(placement.state(), WorkPlacementStateV1::Admitted);
    assert_eq!(placement.authority_version(), 1);

    // Re-admitting the same target is a replay, not a second row.
    let replayed = service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 300),
            observer(clean()),
        )
        .expect("replay");
    assert_eq!(replayed, placement);
    assert_eq!(store.placements.lock().unwrap().len(), 1);
}

#[test]
fn a_second_run_cannot_take_a_root_an_admitted_placement_already_holds() {
    let store = TestStore::default();
    let service = WorkPlacementService::new(store);
    let context = context("actor.placement.exclusive");
    service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 200),
            observer(clean()),
        )
        .expect("first admission");

    // The observation is clean; exclusivity is the service's own reading of
    // storage, so a caller cannot observe its way past it.
    let problem = service
        .admit_placement(
            &context,
            admit_command("run.placement.b", 300),
            observer(clean()),
        )
        .expect_err("a held root is exclusive");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);

    // The holder's own re-preflight is still admissible: a run is not its own
    // blocker.
    let preflight = service
        .preflight(
            &context,
            WorkPlacementPreflightRequestV1 {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                target: linked(),
                occurred_at: UtcMicros(400),
            },
            observer(clean()),
        )
        .expect("holder preflight");
    assert!(preflight.is_admissible());
}

#[test]
fn an_unreadable_target_blocks_admission_and_names_the_reason() {
    let service = WorkPlacementService::new(TestStore::default());
    let context = context("actor.placement.unreadable");
    let unreadable = WorkPlacementObservationV1 {
        readable: false,
        ..clean()
    };
    let preflight = service
        .preflight(
            &context,
            WorkPlacementPreflightRequestV1 {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                target: linked(),
                occurred_at: UtcMicros(100),
            },
            observer(unreadable),
        )
        .expect("preflight");
    assert_eq!(
        preflight.blockers,
        BTreeSet::from([WorkPlacementBlockerV1::TargetUnreadable])
    );
    let problem = service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 200),
            observer(unreadable),
        )
        .expect_err("a blocked target is not admitted");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
}

#[test]
fn release_quarantines_uniquely_valuable_bytes_and_frees_the_root_only_when_clean() {
    let store = TestStore::default();
    let service = WorkPlacementService::new(store.clone());
    let context = context("actor.placement.release");
    let admitted = service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 200),
            observer(clean()),
        )
        .expect("admit");

    // An unmeasured reachability is "unknown", which Plan 32 forbids cleaning.
    let unmeasured = WorkPlacementObservationV1 {
        unique_commits: None,
        ..clean()
    };
    let quarantined = service
        .release(
            &context,
            ReleaseWorkPlacementCommand {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                expected_authority_version: admitted.authority_version(),
                occurred_at: UtcMicros(400),
            },
            observer(unmeasured),
        )
        .expect("release");
    assert_eq!(quarantined.state(), WorkPlacementStateV1::Quarantined);
    assert_eq!(
        quarantined.blockers(),
        &BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits])
    );
    // Quarantine still holds the root, so nobody else may take it.
    let problem = service
        .admit_placement(
            &context,
            admit_command("run.placement.b", 500),
            observer(clean()),
        )
        .expect_err("a quarantined root is still held");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);

    // A fresh cleanup preflight that proves the target is worthless releases it.
    let released = service
        .release(
            &context,
            ReleaseWorkPlacementCommand {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                expected_authority_version: quarantined.authority_version(),
                occurred_at: UtcMicros(600),
            },
            observer(clean()),
        )
        .expect("second release");
    assert_eq!(released.state(), WorkPlacementStateV1::Released);
    // Only now is the root free for another run.
    service
        .admit_placement(
            &context,
            admit_command("run.placement.b", 700),
            observer(clean()),
        )
        .expect("the released root is available");
    assert_eq!(
        store
            .load_placement(&authority_of(&context), &identity("run.placement.b"))
            .unwrap()
            .expect("second placement")
            .state(),
        WorkPlacementStateV1::Admitted
    );
}

#[test]
fn a_stale_release_version_conflicts_instead_of_republishing() {
    let store = TestStore::default();
    let service = WorkPlacementService::new(store);
    let context = context("actor.placement.stale");
    let admitted = service
        .admit_placement(
            &context,
            admit_command("run.placement.a", 200),
            observer(clean()),
        )
        .expect("admit");
    let problem = service
        .release(
            &context,
            ReleaseWorkPlacementCommand {
                task_id: id::<TaskId>("task.placement"),
                run_id: id::<RunId>("run.placement.a"),
                expected_authority_version: admitted.authority_version() + 5,
                occurred_at: UtcMicros(400),
            },
            observer(clean()),
        )
        .expect_err("stale release");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
}
