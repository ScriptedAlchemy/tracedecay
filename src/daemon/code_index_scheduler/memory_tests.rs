use std::fs;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_domain::{ProjectId, RelationEdgeKindV1, SanitizerRevision, WorktreeId};
use tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1;

use super::queries::relation_records_with_edge_probe;
use super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexPublicationStoreV1, ServingLane,
    SharedCodeIndexBytePoolV1,
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

fn realistic_corpus_fixture() -> TempDir {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "memory@test.invalid"],
    );
    git(root.path(), &["config", "user.name", "Memory Test"]);
    fs::create_dir_all(root.path().join("src/modules")).expect("create corpus directory");
    for module in 0..64 {
        let mut source = String::with_capacity(16 * 1024);
        for function in 0..64 {
            source.push_str(&format!(
                "pub fn module_{module}_function_{function}(value: u64) -> u64 {{ value.wrapping_mul({}).wrapping_add({}) }}\n",
                function + 1,
                module + 1,
            ));
        }
        fs::write(
            root.path().join(format!("src/modules/module_{module}.rs")),
            source,
        )
        .expect("write corpus module");
    }
    git(root.path(), &["add", "src/modules"]);
    git(root.path(), &["commit", "-q", "-m", "realistic corpus"]);
    root
}

fn relation_scale_fixture(disconnected_edges: usize) -> TempDir {
    let root = TempDir::new().expect("fixture root");
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "memory@test.invalid"],
    );
    git(root.path(), &["config", "user.name", "Memory Test"]);
    fs::create_dir_all(root.path().join("src")).expect("create source directory");
    let mut source = String::from("pub fn start() { target(); }\npub fn target() {}\n");
    for edge in 0..disconnected_edges {
        source.push_str(&format!(
            "pub fn noise_caller_{edge}() {{ noise_target_{edge}(); }}\n\
             pub fn noise_target_{edge}() {{}}\n"
        ));
    }
    fs::write(root.path().join("src/lib.rs"), source).expect("write relation corpus");
    git(root.path(), &["add", "src/lib.rs"]);
    git(root.path(), &["commit", "-q", "-m", "relation corpus"]);
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
            !charge
                .key
                .component
                .as_str()
                .starts_with("code_index.serving_")
        }),
        "cancelled warm must publish no derived lane reservation"
    );
}

#[test]
fn lane_admission_retains_only_independently_admitted_owners() {
    let project = fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(32 * 1024 * 1024).expect("resident limit"),
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
        latest.warm_serving_caches(),
        Err(tracedecay_query::retrieval::ports::RetrievalPortError::AuthorityUnavailable(_))
    ));
    assert!(!latest.query_owners_are_warm());
    assert!(
        resident_memory
            .snapshot()
            .charges
            .iter()
            .all(|charge| charge.bytes <= 32 * 1024 * 1024),
        "one denied lane must not publish a reservation beyond the process bound"
    );
}

#[test]
fn one_lane_warm_failure_never_poison_caches_or_blocks_other_lanes() {
    for lane in [
        ServingLane::RecordIndex,
        ServingLane::Exact,
        ServingLane::Lexical,
        ServingLane::Graph,
    ] {
        let project = fixture();
        let store = TempDir::new().expect("store root");
        let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
            NonZeroU64::new(1024 * 1024 * 1024).expect("resident limit"),
        ));
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
            ProjectId::new(format!("project.code-index-lane-{}", lane as u8)).expect("project id"),
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
            resident_memory,
        )
        .expect("scheduler");
        scheduler.reconcile_now().expect("generation");
        let latest = scheduler.latest_complete().expect("latest");
        let owners = latest.production_query_owners().expect("serving registry");
        owners.fail_next(lane);
        assert!(
            latest.warm_serving_caches().is_err(),
            "the injected lane failure must be reported"
        );

        match lane {
            ServingLane::RecordIndex => {
                assert!(latest.record_index().is_err());
                owners.exact().expect("exact remains independent");
                owners.lexical().expect("lexical remains independent");
                owners.graph().expect("graph remains independent");
            }
            ServingLane::Exact => {
                assert!(owners.exact().is_err());
                latest.record_index().expect("record remains independent");
                owners.lexical().expect("lexical remains independent");
                owners.graph().expect("graph remains independent");
            }
            ServingLane::Lexical => {
                assert!(owners.lexical().is_err());
                latest.record_index().expect("record remains independent");
                owners.exact().expect("exact remains independent");
                owners.graph().expect("graph remains independent");
            }
            ServingLane::Graph => {
                assert!(owners.graph().is_err());
                latest.record_index().expect("record remains independent");
                owners.exact().expect("exact remains independent");
                owners.lexical().expect("lexical remains independent");
            }
        }
        latest
            .warm_serving_caches()
            .expect("a failed lane retries because failure was not cached");
        latest.record_index().expect("record retry");
        owners.exact().expect("exact retry");
        owners.lexical().expect("lexical retry");
        owners.graph().expect("graph retry");
    }
}

#[test]
fn relation_traversal_examines_only_reachable_adjacency_in_a_large_generation() {
    const DISCONNECTED_EDGES: usize = 1_000;

    let project = relation_scale_fixture(DISCONNECTED_EDGES);
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(2 * 1024 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-relation-scale").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        resident_memory,
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("generation");
    let latest = scheduler.latest_complete().expect("latest");
    latest
        .warm_serving_caches()
        .expect("warm independent serving lanes");
    let start = latest
        .generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| {
            symbol.qualified_name == "start" || symbol.qualified_name.ends_with("::start")
        })
        .expect("start symbol")
        .occurrence
        .clone();
    let scope = tracedecay_application::CodeQueryScope::new(
        latest.generation.manifest().generation_id.clone(),
        None,
    )
    .expect("query scope");

    let (records, examined_edges) = relation_records_with_edge_probe(
        &latest,
        &start,
        &[RelationEdgeKindV1::Calls],
        false,
        1,
        &scope,
    )
    .expect("relation traversal");

    assert_eq!(records.len(), 1, "only start -> target is reachable");
    assert_eq!(
        examined_edges, 1,
        "disconnected edges must not contribute to traversal work"
    );
    assert!(
        latest.generation.edges().len() >= DISCONNECTED_EDGES,
        "fixture must retain the large disconnected edge population"
    );
}

#[tokio::test]
async fn mount_publishes_a_large_generation_before_bounded_lane_warming() {
    let project = realistic_corpus_fixture();
    let store = TempDir::new().expect("store root");
    let project_id = ProjectId::new("project.code-index-mount-publication").expect("project id");
    let scoped_store = super::scoped_code_index_store_root(store.path(), project.path());
    {
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
            project_id.clone(),
            project.path(),
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
            Arc::new(ProcessResidentMemoryV1::new(
                NonZeroU64::new(2 * 1024 * 1024 * 1024).expect("resident limit"),
            )),
        )
        .expect("scheduler");
        scheduler.reconcile_now().expect("sealed generation");
    }

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(2, 0);
    let started = Instant::now();
    registry
        .mount_worktree(project_id, project.path(), store.path().to_path_buf(), None)
        .await
        .expect("mount retained generation");
    let mount_elapsed = started.elapsed();
    let canonical_root = project.path().canonicalize().expect("canonical root");
    let (latest, scheduler) = {
        let mounted = registry.mounted.lock().await;
        let mounted = mounted.get(&canonical_root).expect("mounted worktree");
        (
            mounted
                .serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .expect("published generation"),
            Arc::clone(&mounted.scheduler),
        )
    };

    assert!(
        mount_elapsed < Duration::from_secs(30),
        "mount decode took {mount_elapsed:?}; lane warming must not be charged to mount"
    );
    assert!(
        !latest.serving_lanes_are_ready(),
        "zero warm permits prove publication happened before any lane warm"
    );
    assert!(
        registry.mounted.try_lock().is_ok(),
        "publication must release the mount registry lock before warming"
    );
    assert!(
        scheduler.try_lock().is_ok(),
        "publication must release the scheduler writer before warming"
    );

    registry.background_reconcile_admission().add_permits(1);
    tokio::time::timeout(Duration::from_secs(60), async {
        while !latest.serving_lanes_are_ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bounded lane warming completed");
    registry.shutdown().await;
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
        resident_memory.snapshot().charges.iter().any(|charge| {
            charge
                .key
                .component
                .as_str()
                .starts_with("code_index.serving_")
        }),
        "in-flight lane owner must retain its reservation"
    );
    drop(owners);
    assert_eq!(resident_memory.snapshot().used_bytes, 0);
}

#[test]
fn realistic_corpus_retains_every_serving_component_within_authority() {
    let project = realistic_corpus_fixture();
    let store = TempDir::new().expect("store root");
    let resident_memory = Arc::new(ProcessResidentMemoryV1::new(
        NonZeroU64::new(2 * 1024 * 1024 * 1024).expect("resident limit"),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_resident_memory(
        ProjectId::new("project.code-index-realistic-corpus").expect("project id"),
        project.path(),
        store.path().to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        Arc::clone(&resident_memory),
    )
    .expect("scheduler");
    scheduler.reconcile_now().expect("generation");
    let latest = scheduler.latest_complete().expect("latest");
    latest
        .warm_serving_caches()
        .expect("warm serving components");

    let snapshot = resident_memory.snapshot();
    assert!(snapshot.used_bytes <= snapshot.limit_bytes);
    for component in [
        "code_index.capture_working_set.v1",
        "code_index.canonical_generation.v1",
        "code_index.serving_record_index",
        "code_index.serving_exact_lexical",
        "code_index.serving_graph",
    ] {
        assert!(
            snapshot
                .charges
                .iter()
                .any(|charge| charge.key.component.as_str() == component),
            "missing resident charge for {component}"
        );
    }

    drop(latest);
    drop(scheduler);
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
