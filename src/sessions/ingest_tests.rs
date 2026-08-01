use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tracedecay_domain::ProjectId;
use tracedecay_global_db::RegisteredGlobalDb;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::sessions::ingest::test_support::{
    IngestPassBounds, IngestPassCoverage, USER_INGEST_PROVIDER_FRONTIER_KEY,
    ingest_project_sources_for_provider_without_registered_authority,
    ingest_user_global_sources_for_provider_with_roots_bounded,
    ingest_user_global_sources_for_provider_with_roots_without_registered_authority,
    ingest_user_global_sources_for_startup_with_db_without_registered_authority,
};
use crate::sessions::ingest::{
    ingest_project_sources_for_provider, ingest_user_global_sources_for_startup_with_db,
    registered_project_roots_from,
};
use crate::store::GlobalDbSessionIngestAuthority;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::{SessionProvider, TranscriptIngestStats};

static INGEST_TEST_NONCE: AtomicU64 = AtomicU64::new(1);

const TEST_INGEST_BOUNDS: IngestPassBounds = IngestPassBounds {
    discovered_units: 16,
    units_per_pass: 8,
    units_per_source: 8,
    queue_depth: 8,
    bytes_per_unit: 1024,
    bytes_per_pass: 4096,
    retries: 0,
};

struct IngestTestRuntime {
    database: Arc<RegisteredGlobalDb>,
    _registry: DaemonSessionRuntimeRegistryV1,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _profile: tempfile::TempDir,
}

impl IngestTestRuntime {
    fn authority(&self) -> GlobalDbSessionIngestAuthority<Arc<RegisteredGlobalDb>> {
        GlobalDbSessionIngestAuthority::new(Arc::clone(&self.database))
    }
}

async fn open_registry(
    purpose: &str,
) -> (
    tempfile::TempDir,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
    DaemonSessionRuntimeRegistryV1,
) {
    let profile = tempfile::tempdir().unwrap();
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile.path().join("profile")).unwrap();
    let nonce = INGEST_TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        nonce,
        &format!("{purpose}-{nonce}"),
    )
    .unwrap();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .unwrap();
    (profile, scope, registry)
}

async fn profile_test_runtime() -> IngestTestRuntime {
    let (profile, scope, registry) = open_registry("sessions-ingest-profile-test").await;
    let database = registry.profile_sessions().await.unwrap();
    IngestTestRuntime {
        database,
        _registry: registry,
        _scope: scope,
        _profile: profile,
    }
}

async fn project_test_runtime(project_root: &Path, project_id: ProjectId) -> IngestTestRuntime {
    let (profile, scope, registry) = open_registry("sessions-ingest-project-test").await;
    tracedecay_runtime_core::storage::write_enrollment_marker(
        project_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let database = registry
        .project_sessions(project_id, [project_root.to_path_buf()])
        .await
        .unwrap();
    IngestTestRuntime {
        database,
        _registry: registry,
        _scope: scope,
        _profile: profile,
    }
}

fn scheduler_test_project_id() -> ProjectId {
    ProjectId::new(format!(
        "scheduler-test-{}",
        INGEST_TEST_NONCE.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

#[tokio::test]
async fn missing_project_identity_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();

    let outcome = ingest_project_sources_for_provider_without_registered_authority(
        &authority,
        temp.path(),
        None,
        None,
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].reason_code, "project_identity_missing");
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
}

#[tokio::test]
async fn unregistered_project_authority_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(temp.path(), project_id.clone()).await;
    let authority = runtime.authority();

    let outcome = ingest_project_sources_for_provider_without_registered_authority(
        &authority,
        temp.path(),
        Some(project_id),
        Some(SessionProvider::Vibe),
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
}

#[tokio::test]
async fn mismatched_project_id_fails_before_provider_catch_up() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mounted_project_id = scheduler_test_project_id();
    let requested_project_id = scheduler_test_project_id();
    let runtime = project_test_runtime(&project, mounted_project_id.clone()).await;
    let authority = runtime.authority();
    let shard = &runtime.database.binding().shard_id;

    let outcome = ingest_project_sources_for_provider(
        &shard.brain_id,
        &shard.profile_id,
        &authority,
        &project,
        Some(requested_project_id),
        None,
        true,
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "project_sessions_authority_mismatch"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert_eq!(
        runtime.database.binding().shard_id.scope,
        tracedecay_store::StoreShardScopeV1::ProjectSessions {
            project_id: mounted_project_id
        }
    );
}

#[tokio::test]
async fn unregistered_profile_authority_fails_before_ingest_writes() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();

    let outcome = ingest_user_global_sources_for_provider_with_roots_without_registered_authority(
        &authority,
        temp.path(),
        Some(SessionProvider::Codex),
        Vec::new(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        runtime
            .database
            .get_parse_offset_result(USER_INGEST_PROVIDER_FRONTIER_KEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cancelled_user_pass_reports_partial_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();
    let shard = &runtime.database.binding().shard_id;
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome = ingest_user_global_sources_for_provider_with_roots_bounded(
        (&shard.brain_id, &shard.profile_id, &authority),
        temp.path(),
        None,
        Vec::new(),
        TEST_INGEST_BOUNDS,
        &cancellation,
    )
    .await;

    assert_eq!(outcome.units_admitted, 0);
    assert_eq!(
        outcome.coverage,
        IngestPassCoverage::Partial { deferred_units: 9 }
    );
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
}

#[tokio::test]
async fn cancelled_startup_user_ingest_stops_before_registry_reads() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();
    let shard = &runtime.database.binding().shard_id;
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome = ingest_user_global_sources_for_startup_with_db(
        &shard.brain_id,
        &shard.profile_id,
        &authority,
        &authority,
        temp.path(),
        &cancellation,
    )
    .await;

    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(
        outcome
            .failures
            .iter()
            .any(|failure| failure.reason_code == "ingest_pass_cancelled")
    );
}

#[tokio::test]
async fn unregistered_startup_authority_fails_before_registry_reads() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();

    let outcome = ingest_user_global_sources_for_startup_with_db_without_registered_authority(
        &authority,
        &authority,
        temp.path(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "registered_authority_unavailable"
    );
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
}

#[tokio::test]
async fn registered_project_roots_include_modern_registry_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = temp.path().join("repo");
    let worktree = temp.path().join("repo-worktree");
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let canonical = std::fs::canonicalize(canonical).unwrap();
    let worktree = std::fs::canonicalize(worktree).unwrap();
    let runtime = profile_test_runtime().await;
    runtime
        .database
        .upsert_code_project("project-1", &canonical, None, None, None)
        .await
        .unwrap();
    runtime
        .database
        .upsert_project_alias(&worktree, "project-1")
        .await
        .unwrap();
    let authority = runtime.authority();
    let roots = registered_project_roots_from(&authority).await.unwrap();

    assert!(
        roots.contains(&canonical),
        "missing {canonical:?} from {roots:?}"
    );
    assert!(
        roots.contains(&worktree),
        "missing {worktree:?} from {roots:?}"
    );
}

// macOS filesystems reject invalid UTF-8 path components with EILSEQ.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn registered_project_roots_preserve_non_unicode_current_root() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let runtime = profile_test_runtime().await;
    runtime
        .database
        .upsert_code_project("project-native", &root, None, None, None)
        .await
        .unwrap();
    let authority = runtime.authority();
    let roots = registered_project_roots_from(&authority).await.unwrap();

    assert!(roots.contains(&root));
}
