use super::*;

fn assert_reconcile_did_not_publish_invalid_roster(
    served: &LatestCompleteCodeIndexV1,
    incumbent: &CodeGenerationId,
) {
    let generation = served.generation();
    if &generation.manifest().generation_id == incumbent {
        assert_eq!(
            roster_paths(served),
            ["node_modules/pkg/index.d.ts"],
            "the unchanged incumbent may remain serving while re-admission is required"
        );
    } else {
        assert!(
            generation.ignored_source_admissions().is_empty(),
            "a successor may not retain a path whose ignored-source proof is invalid"
        );
    }
}

async fn admit_package(
    fixture: &GitFixture,
    store: &TempDir,
) -> (
    CodeIndexSchedulerRegistryV1,
    CodeIndexIgnoredDependencyRequestV1,
    CodeGenerationId,
) {
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface PublicWidget { value: string }\n",
    );
    let registry = mount(fixture.path(), store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let request = request_for(&baseline, "pkg");
    let outcome = index_dependency(
        &registry,
        fixture.path(),
        request.clone(),
        StaticControl::active(),
    )
    .await
    .expect("admit ignored dependency");
    (registry, request, outcome.generation_id)
}

async fn restart_after_one_worker_pass(
    fixture: &GitFixture,
    store: &TempDir,
) -> CodeIndexSchedulerRegistryV1 {
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold restart activation");
    registry
        .mount_worktree(
            project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
        )
        .await
        .expect("remount retained ignored-source store");
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal restart scheduler hold");
        release_rx.recv().expect("release restart scheduler hold");
    });
    held_rx.recv().expect("restart scheduler is held");
    drop(admission);
    wait_for_reconciling(&registry, 1).await;
    release_tx.send(()).expect("release restart scheduler");
    lock_thread.join().expect("restart scheduler holder joins");
    wait_for_reconciling(&registry, 0).await;
    registry
}

async fn assert_restart_requires_readmission(
    registry: &CodeIndexSchedulerRegistryV1,
    fixture: &GitFixture,
    prior_request: CodeIndexIgnoredDependencyRequestV1,
    expected: CodeIndexIgnoredDependencyRefusalV1,
) {
    let error = if registry
        .latest_generation_id(fixture.path())
        .await
        .is_some()
    {
        let restored = latest(registry, fixture.path()).await;
        assert!(
            restored.generation().ignored_source_admissions().is_empty(),
            "restart cannot activate a generation with an invalid ignored-source roster"
        );
        index_dependency(
            registry,
            fixture.path(),
            request_for(&restored, "pkg"),
            StaticControl::active(),
        )
        .await
        .expect_err("the changed entrypoint requires typed re-admission")
    } else {
        index_dependency(
            registry,
            fixture.path(),
            prior_request,
            StaticControl::active(),
        )
        .await
        .expect_err("unverified restart requires typed re-admission")
    };
    assert!(
        matches!(
            error,
            CodeIndexSchedulerErrorV1::IgnoredDependency(refusal)
                if refusal == expected
                    || refusal == CodeIndexIgnoredDependencyRefusalV1::StaleGeneration
        ),
        "restart must expose the exact validation refusal or a stale-generation re-admission fence"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracked_dependency_roster_is_rejected_by_reconcile_and_restart() {
    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor(value: PublicWidget) { return value; }
"#,
    );
    let store = TempDir::new().expect("store root");
    let (registry, request, incumbent) = admit_package(&fixture, &store).await;

    git(
        fixture.path(),
        &["add", "-f", "node_modules/pkg/index.d.ts"],
    );
    git(
        fixture.path(),
        &["commit", "-qm", "track dependency source"],
    );
    reconcile_through_worker(&registry, fixture.path(), &request.scope).await;
    let served = latest(&registry, fixture.path()).await;
    assert_reconcile_did_not_publish_invalid_roster(&served, &incumbent);

    registry.shutdown().await;
    let restarted = restart_after_one_worker_pass(&fixture, &store).await;
    assert_restart_requires_readmission(
        &restarted,
        &fixture,
        request,
        CodeIndexIgnoredDependencyRefusalV1::NotIgnored,
    )
    .await;
    restarted.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retargeted_dependency_symlink_is_rejected_by_reconcile_and_restart() {
    use std::os::unix::fs::symlink;

    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor(value: PublicWidget) { return value; }
"#,
    );
    fixture.write(
        "src/retargeted.d.ts",
        b"export interface RetargetedWidget { outsidePackage: true }\n",
    );
    git(fixture.path(), &["add", "src/retargeted.d.ts"]);
    git(fixture.path(), &["commit", "-qm", "retarget fixture"]);
    let store = TempDir::new().expect("store root");
    let (registry, request, incumbent) = admit_package(&fixture, &store).await;

    std::fs::remove_file(fixture.path().join("node_modules/pkg/index.d.ts"))
        .expect("remove admitted dependency file");
    symlink(
        "../../src/retargeted.d.ts",
        fixture.path().join("node_modules/pkg/index.d.ts"),
    )
    .expect("retarget dependency inside project but outside package");
    reconcile_through_worker(&registry, fixture.path(), &request.scope).await;
    let served = latest(&registry, fixture.path()).await;
    assert_reconcile_did_not_publish_invalid_roster(&served, &incumbent);

    registry.shutdown().await;
    let restarted = restart_after_one_worker_pass(&fixture, &store).await;
    assert_restart_requires_readmission(
        &restarted,
        &fixture,
        request,
        CodeIndexIgnoredDependencyRefusalV1::SymlinkEscape,
    )
    .await;
    restarted.shutdown().await;
}
