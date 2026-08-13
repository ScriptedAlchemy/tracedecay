use std::collections::BTreeSet;

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use tracedecay_application::{
    AcceptTaskCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand, Deadline,
    DisclosureClass, RequestContext, RequestId, ResolvedScope, WorkReadiness, WorkService,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkCommandId,
    WorkVersion, WorktreeId, canonical_sha256,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const DEPENDENCY_COUNT: usize = 64;
const READY_DEPENDENCY_COUNT: usize = DEPENDENCY_COUNT / 2;

struct WorkReadinessFixture {
    service: WorkService<WorkSqliteStorage>,
    context: RequestContext,
    ready_target: TaskId,
    blocked_target: TaskId,
    _runtime: RegisteredGlobalDbTestRuntime,
    _project: tempfile::TempDir,
    _profile: tempfile::TempDir,
}

fn id<T>(value: impl Into<String>) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.into()).expect("benchmark identity is valid")
}

fn digest(label: &str) -> ManifestDigest {
    canonical_sha256(&("tracedecay.work-readiness-benchmark.v1", label))
        .expect("benchmark digest is valid")
}

fn context(project_id: ProjectId) -> RequestContext {
    let scope = ResolvedScope::new(
        project_id,
        id::<RepositoryId>("repository.work-readiness-benchmark"),
        id::<WorktreeId>("worktree.work-readiness-benchmark"),
        None,
    )
    .expect("benchmark scope is valid");
    let capability = CapabilityId::new("capability.work-readiness-benchmark")
        .expect("benchmark capability is valid");
    let use_case =
        UseCaseId::new("use-case.work-readiness-benchmark").expect("benchmark use case is valid");
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work-readiness-benchmark"),
        1,
        digest("grant"),
        id::<ActorId>("actor.work-readiness-issuer"),
        UtcMicros(1),
        UtcMicros(1_000_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .expect("benchmark grant is valid");
    RequestContext::new(
        id::<ActorId>("actor.work-readiness-owner"),
        scope,
        grant,
        RequestId::new("request.work-readiness-benchmark").expect("benchmark request is valid"),
        Deadline::new(UtcMicros(900_000)).expect("benchmark deadline is valid"),
        CancellationContext::active("cancel.work-readiness-benchmark")
            .expect("benchmark cancellation is valid"),
    )
    .expect("benchmark request context is valid")
}

fn dependency(index: usize) -> TaskId {
    id(format!("task.work-readiness.dependency.{index:02}"))
}

fn create(
    service: &WorkService<WorkSqliteStorage>,
    context: &RequestContext,
    task_id: TaskId,
    dependencies: BTreeSet<TaskId>,
    sequence: usize,
) {
    service
        .create(
            context,
            CreateWorkCommand {
                title: format!("Readiness fixture for {}", task_id.as_str()),
                task_id,
                dependencies,
                command_id: id::<WorkCommandId>(format!(
                    "command.work-readiness.create.{sequence:03}"
                )),
                occurred_at: UtcMicros(i64::try_from(sequence).expect("benchmark timestamp fits")),
            },
        )
        .expect("benchmark Work task is created");
}

async fn fixture() -> WorkReadinessFixture {
    let profile = tempfile::tempdir().expect("temporary benchmark profile exists");
    let project = tempfile::tempdir().expect("temporary benchmark project exists");
    let project_id = id::<ProjectId>("project.work-readiness-benchmark");
    let runtime =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id.clone())
            .await
            .expect("production registered Work store opens");
    // This is the exact concrete service/storage pairing constructed by
    // `RegisteredGlobalDb::work_application_services`; mounting that larger
    // bundle would additionally require the unrelated project-graph runtime.
    let service = WorkService::new(
        runtime
            .project_database()
            .expect("registered project database is mounted")
            .work_storage()
            .expect("production Work storage attaches"),
    );
    let context = context(project_id);
    let mut accepted_dependencies = BTreeSet::new();
    let mut blocked_dependencies = BTreeSet::new();
    let mut sequence = 10;

    for index in 0..DEPENDENCY_COUNT {
        let task_id = dependency(index);
        create(
            &service,
            &context,
            task_id.clone(),
            BTreeSet::new(),
            sequence,
        );
        sequence += 1;
        if index % 2 == 0 {
            service
                .accept_task(
                    &context,
                    AcceptTaskCommand {
                        task_id: task_id.clone(),
                        expected_version: WorkVersion::initial(),
                        command_id: id::<WorkCommandId>(format!(
                            "command.work-readiness.accept.{index:02}"
                        )),
                        occurred_at: UtcMicros(
                            i64::try_from(sequence).expect("benchmark timestamp fits"),
                        ),
                    },
                )
                .expect("benchmark dependency is accepted");
            sequence += 1;
            accepted_dependencies.insert(task_id);
        } else {
            blocked_dependencies.insert(task_id);
        }
    }
    assert_eq!(accepted_dependencies.len(), READY_DEPENDENCY_COUNT);
    assert_eq!(blocked_dependencies.len(), READY_DEPENDENCY_COUNT);

    let ready_target = id::<TaskId>("task.work-readiness.ready-target");
    create(
        &service,
        &context,
        ready_target.clone(),
        accepted_dependencies,
        sequence,
    );
    sequence += 1;
    let blocked_target = id::<TaskId>("task.work-readiness.blocked-target");
    let all_dependencies = (0..DEPENDENCY_COUNT)
        .map(dependency)
        .collect::<BTreeSet<_>>();
    assert_eq!(all_dependencies.len(), DEPENDENCY_COUNT);
    create(
        &service,
        &context,
        blocked_target.clone(),
        all_dependencies,
        sequence,
    );

    assert_eq!(
        service
            .readiness(&context, &ready_target)
            .expect("ready Work state is readable"),
        WorkReadiness::Ready
    );
    assert_eq!(
        service
            .readiness(&context, &blocked_target)
            .expect("blocked Work state is readable"),
        WorkReadiness::Blocked {
            active_dependencies: blocked_dependencies,
        }
    );

    WorkReadinessFixture {
        service,
        context,
        ready_target,
        blocked_target,
        _runtime: runtime,
        _project: project,
        _profile: profile,
    }
}

fn work_readiness(criterion: &mut Criterion) {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime builds");
    let fixture = tokio.block_on(fixture());
    let mut group = criterion.benchmark_group("registered_work_readiness");

    group.throughput(Throughput::Elements(READY_DEPENDENCY_COUNT as u64));
    group.bench_function("ready_32", |bencher| {
        bencher.iter(|| {
            let readiness = fixture
                .service
                .readiness(
                    black_box(&fixture.context),
                    black_box(&fixture.ready_target),
                )
                .expect("ready Work state remains readable");
            black_box(readiness)
        });
    });

    group.throughput(Throughput::Elements(DEPENDENCY_COUNT as u64));
    group.bench_function("blocked_64", |bencher| {
        bencher.iter(|| {
            let readiness = fixture
                .service
                .readiness(
                    black_box(&fixture.context),
                    black_box(&fixture.blocked_target),
                )
                .expect("blocked Work state remains readable");
            black_box(readiness)
        });
    });
    group.finish();
}

criterion_group!(benches, work_readiness);
criterion_main!(benches);
