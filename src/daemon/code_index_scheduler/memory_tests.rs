use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{ProjectId, SanitizerRevision, WorktreeId};
use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexPublicationStoreV1, SharedCodeIndexBytePoolV1,
};
use crate::code_index::production::{CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1};
use crate::privacy::CODE_SOURCE_SANITIZER_VERSION_V1;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> TempDir {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "memory@test.invalid"],
    );
    git(root.path(), &["config", "user.name", "Memory Test"]);
    fs::create_dir_all(root.path().join("src")).expect("create source directory");
    fs::write(
        root.path().join("src/lib.rs"),
        "pub fn retained_generation() -> u32 { 1 }\n",
    )
    .expect("write source");
    git(root.path(), &["add", "src/lib.rs"]);
    git(root.path(), &["commit", "-q", "-m", "fixture"]);
    root
}

#[test]
fn latest_complete_reuses_the_immutable_generation_allocation() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-memory").expect("valid project");
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(128 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        project_id.clone(),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("open scheduler");
    assert!(matches!(
        scheduler.reconcile_now().expect("publish generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));

    let first = scheduler.latest_complete().expect("first generation read");
    let second = scheduler.latest_complete().expect("second generation read");
    let charged = resident_memory.snapshot();
    assert!(charged.used_bytes > 0);
    let canonical = charged
        .charges
        .iter()
        .find(|charge| charge.key.component.as_str() == "code_index.canonical_generation.v1")
        .expect("canonical resident charge");
    let conservative = first
        .generation()
        .encoded_sealed_len()
        .and_then(
            crate::code_index::production::CodeIndexPublishedGenerationV1::sealed_resident_memory_upper_bound,
        )
        .expect("canonical sealed upper bound");
    assert_eq!(
        canonical.bytes, conservative,
        "publication must retain its conservative opaque-allocation overcharge"
    );

    assert!(
        std::ptr::eq(first.generation(), second.generation()),
        "readers must share the sealed generation instead of deep-cloning it"
    );
    assert!(!first.exact().expect("exact chunks").is_empty());
    let generation_id = first.generation().manifest().generation_id.clone();
    drop(scheduler);
    assert!(
        resident_memory.snapshot().used_bytes > 0,
        "in-flight generation handles must retain their canonical charge"
    );
    drop(first);
    drop(second);
    assert_eq!(
        resident_memory.snapshot().used_bytes,
        0,
        "the final generation handle must release its canonical charge"
    );

    let reopened = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        project_id,
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("reopen scheduler");
    assert_eq!(
        reopened
            .latest_complete()
            .expect("restored generation")
            .generation()
            .manifest()
            .generation_id,
        generation_id,
        "borrowed publication encoding must remain restart-compatible"
    );
    assert!(resident_memory.snapshot().used_bytes > 0);
    drop(reopened);
    assert_eq!(resident_memory.snapshot().used_bytes, 0);
}

#[test]
fn canonical_generation_admission_fails_typed_without_leaking_a_charge() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(NonZeroU64::MIN));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-memory-denial").expect("valid project"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("empty store opens before generation admission");

    let error = scheduler
        .reconcile_now()
        .expect_err("one-byte authority must deny a canonical generation");
    assert!(matches!(
        error,
        CodeIndexSchedulerErrorV1::Production(CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::ResidentMemoryUnavailable { limit_bytes: 1, .. }
        ))
    ));
    assert_eq!(resident_memory.snapshot().used_bytes, 0);
    assert!(
        !store.path().join("active-code-generation-v1.json").exists(),
        "denied publication must not advance the durable active pointer"
    );
}

#[test]
fn failed_canonical_decode_releases_its_preflight_reservation() {
    let project = fixture();
    let published_store = TempDir::new().expect("published store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(128 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-memory-corrupt").expect("valid project"),
        project.path(),
        published_store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("open scheduler");
    scheduler.reconcile_now().expect("publish generation");
    let latest = scheduler.latest_complete().expect("published generation");
    let sealed = latest
        .generation()
        .encode_sealed()
        .expect("sealed generation");
    drop(latest);
    drop(scheduler);
    assert_eq!(resident_memory.snapshot().used_bytes, 0);

    let mut corrupted: serde_json::Value = serde_json::from_slice(&sealed).expect("sealed JSON");
    corrupted["state_digest"] = serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
    let corrupted = serde_json::to_vec(&corrupted).expect("corrupted sealed JSON");
    let decode_store = TempDir::new().expect("decode store root");
    let publication = DaemonCodeIndexPublicationStoreV1::new_with_resident_memory(
        decode_store.path(),
        SanitizerRevision::new(CODE_SOURCE_SANITIZER_VERSION_V1).expect("sanitizer revision"),
        Arc::clone(&resident_memory),
        ProjectId::new("project.code-index-memory-corrupt").expect("valid project"),
        WorktreeId::new("worktree.code-index-memory-corrupt").expect("valid worktree"),
    )
    .expect("publication store");
    assert!(
        publication
            .decode_authenticated_generation(&corrupted)
            .is_err(),
        "corrupt decode must fail after preflight admission"
    );
    assert_eq!(
        resident_memory.snapshot().used_bytes,
        0,
        "decode failure must release the preflight reservation"
    );
}

#[test]
fn sealed_restore_denial_happens_before_the_first_generation_read() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        ProjectId::new("project.code-index-zero-read-seed").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("seed scheduler");
    scheduler.reconcile_now().expect("seed generation");
    drop(scheduler);

    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(NonZeroU64::MIN));
    let publication = DaemonCodeIndexPublicationStoreV1::new_with_resident_memory(
        store.path(),
        SanitizerRevision::new(CODE_SOURCE_SANITIZER_VERSION_V1).expect("sanitizer revision"),
        resident_memory,
        ProjectId::new("project.code-index-zero-read").expect("project id"),
        WorktreeId::new("worktree.code-index-zero-read").expect("worktree id"),
    )
    .expect("publication store");
    assert!(matches!(
        publication.load_active_shared(),
        Err(CodeIndexPublicationStoreErrorV1::ResidentMemoryUnavailable { limit_bytes: 1, .. })
    ));
    assert_eq!(
        publication.sealed_read_attempt_count(),
        0,
        "admission must precede File::open/read"
    );
}

#[test]
fn cancelled_lane_warm_releases_every_unpublished_lane_charge() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(1024 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-cancel-warm").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("generation");
    let latest = scheduler.latest_complete().expect("latest");
    latest.warm_control.cancel();
    assert!(matches!(
        latest.production_query_owners(),
        Err(tracedecay_query::retrieval::ports::RetrievalPortError::Cancelled)
    ));
    assert!(
        resident_memory.snapshot().charges.iter().all(|charge| {
            !matches!(
                charge.key.component.as_str(),
                "code_index.record_index.v1"
                    | "code_index.exact_lexical.v1"
                    | "code_index.graph.v1"
            )
        }),
        "cancelled warm must publish no derived lane reservation"
    );
}

#[test]
fn lane_admission_denies_before_publishing_any_derived_owner() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(256 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-lane-denial").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("generation");
    let latest = scheduler.latest_complete().expect("latest");
    assert!(matches!(
        latest.production_query_owners(),
        Err(tracedecay_query::retrieval::ports::RetrievalPortError::AuthorityUnavailable(_))
    ));
    assert!(!latest.query_owners_are_warm());
    assert!(
        resident_memory.snapshot().charges.iter().all(|charge| {
            !matches!(
                charge.key.component.as_str(),
                "code_index.exact_lexical.v1" | "code_index.graph.v1"
            )
        }),
        "denied lane build must retain no partial reservation"
    );
}

#[test]
fn in_flight_lane_arc_retains_charge_until_its_final_reader_drops() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(1024 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-lane-reader").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("generation");
    let latest = scheduler.latest_complete().expect("latest");
    latest.warm_serving_caches().expect("warm lanes");
    let owners = latest.production_query_owners().expect("ready owners");
    drop(latest);
    drop(scheduler);
    assert!(
        resident_memory
            .snapshot()
            .charges
            .iter()
            .any(|charge| { charge.key.component.as_str() == "code_index.exact_lexical.v1" }),
        "in-flight lane owner must retain its reservation"
    );
    drop(owners);
    assert_eq!(resident_memory.snapshot().used_bytes, 0);
}

#[tokio::test]
async fn registry_reports_retained_generation_bytes_without_scheduler_locks() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-memory").expect("valid project");
    let store = TempDir::new().expect("store root");
    let scoped_store = super::scoped_code_index_store_root(store.path(), project.path());
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        project.path(),
        scoped_store,
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open seed scheduler");
    assert!(matches!(
        scheduler.reconcile_now().expect("seed retained generation"),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    drop(scheduler);

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(project_id, project.path(), store.path().to_path_buf(), None)
        .await
        .expect("mount worktree");

    let stats = registry.memory_stats().await;

    assert_eq!(stats.mounted_worktrees, 1);
    assert_eq!(stats.reconciling_worktrees, 0);
    assert!(stats.retained_generation_encoded_bytes > 0);
    registry.shutdown().await;
}

#[tokio::test]
async fn unmount_cancels_workers_and_releases_code_index_reservations() {
    let project = fixture();
    let project_id = ProjectId::new("project.code-index-unmount").expect("project id");
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(project_id, project.path(), store.path().to_path_buf(), None)
        .await
        .expect("mount");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if registry
                .latest_generation_id(project.path())
                .await
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("generation becomes ready");
    assert!(registry.resident_memory().snapshot().used_bytes > 0);
    assert!(registry.unmount_worktree(project.path()).await);
    assert_eq!(registry.resident_memory().snapshot().used_bytes, 0);
    assert!(!registry.is_worktree_mounted(project.path()).await);
}
