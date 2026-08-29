use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_domain::{BrainId, ProjectId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use tracedecay_usecases::host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::ingest::test_support::{
    IngestPassBounds, USER_INGEST_PROVIDER_FRONTIER_KEY,
    ingest_project_sources_for_provider_without_registered_authority,
    ingest_user_global_sources_for_provider_with_roots_bounded,
    ingest_user_global_sources_for_provider_with_roots_without_registered_authority,
    ingest_user_global_sources_for_startup_with_db_without_registered_authority,
};
use tracedecay_sessions::runtime::ingest::{
    IngestPassCoverage, ingest_project_sources_for_provider,
    ingest_user_global_sources_for_startup_with_db, registered_project_roots_from,
};
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
    database: RegisteredGlobalDbLeaseV1,
    _registry: DaemonSessionRuntimeRegistryV1,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _profile: tempfile::TempDir,
}

impl IngestTestRuntime {
    fn authority(&self) -> GlobalDbSessionIngestAuthority<RegisteredGlobalDbLeaseV1> {
        GlobalDbSessionIngestAuthority::new(self.database.clone())
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
    let (_, scope, registry) = open_registry_at(&profile.path().join("profile"), purpose).await;
    (profile, scope, registry)
}

async fn open_registry_at(
    profile_root: &Path,
    purpose: &str,
) -> (
    crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    tracedecay_runtime_core::db::DaemonDatabaseScope,
    DaemonSessionRuntimeRegistryV1,
) {
    let identity = crate::daemon::profile_identity::load_or_create(profile_root).unwrap();
    let nonce = INGEST_TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        nonce,
        &format!("{purpose}-{nonce}"),
    )
    .unwrap();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .unwrap();
    (identity, scope, registry)
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
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        project_root,
        project_id.as_str(),
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
async fn foreign_brain_profile_session_authority_fails_before_store_mutation() {
    let source_root = tempfile::tempdir().unwrap();
    let rollout_dir = source_root.path().join(".codex/sessions/2026/08/22");
    std::fs::create_dir_all(&rollout_dir).unwrap();
    let rollout = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-08-22T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": "foreign-profile-session",
                "cwd": source_root.path(),
                "model": "gpt-5.6",
            },
        }),
        serde_json::json!({
            "timestamp": "2026-08-22T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "must not persist"},
        }),
    );
    std::fs::write(
        rollout_dir.join("rollout-foreign-profile-session.jsonl"),
        rollout,
    )
    .unwrap();
    let runtime = profile_test_runtime().await;
    let authority = runtime.authority();
    let shard = &runtime.database.binding().shard_id;
    let foreign_brain = BrainId::new("brain.foreign-profile-session").unwrap();
    let before_frontier = runtime
        .database
        .get_parse_offset_result(USER_INGEST_PROVIDER_FRONTIER_KEY)
        .await
        .unwrap();
    let before_messages = runtime.database.session_message_count().await.unwrap();

    let outcome = ingest_user_global_sources_for_provider_with_roots_bounded(
        (&foreign_brain, &shard.profile_id, &authority),
        source_root.path(),
        Some(SessionProvider::Codex),
        Vec::new(),
        TEST_INGEST_BOUNDS,
        &ObservationCancellation::default(),
    )
    .await;

    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        outcome.failures[0].reason_code,
        "profile_sessions_authority_mismatch"
    );
    assert!(!outcome.failures[0].retryable);
    assert_eq!(outcome.stats, TranscriptIngestStats::default());
    assert!(!outcome.scheduling_state_written);
    assert_eq!(outcome.units_admitted, 0);
    assert_eq!(
        runtime
            .database
            .get_parse_offset_result(USER_INGEST_PROVIDER_FRONTIER_KEY)
            .await
            .unwrap(),
        before_frontier,
        "foreign identity must not advance the user-ingest frontier"
    );
    assert_eq!(
        runtime.database.session_message_count().await.unwrap(),
        before_messages,
        "foreign identity must not persist transcript messages"
    );
}

#[tokio::test]
async fn distinct_profile_roots_publish_distinct_production_session_identities() {
    let temporary = tempfile::tempdir().unwrap();
    let first_root = temporary.path().join("first-profile");
    let second_root = temporary.path().join("second-profile");

    let (first_identity, first_scope, first_registry) =
        open_registry_at(&first_root, "distinct-profile-first").await;
    let first_database = first_registry.profile_sessions().await.unwrap();
    let first_shard = first_database.binding().shard_id.clone();
    assert_eq!(&first_shard.brain_id, first_identity.brain_id());
    assert_eq!(&first_shard.profile_id, first_identity.profile_id());
    drop(first_database);
    drop(first_registry);
    drop(first_scope);

    let (second_identity, second_scope, second_registry) =
        open_registry_at(&second_root, "distinct-profile-second").await;
    let second_database = second_registry.profile_sessions().await.unwrap();
    let second_shard = second_database.binding().shard_id.clone();
    assert_eq!(&second_shard.brain_id, second_identity.brain_id());
    assert_eq!(&second_shard.profile_id, second_identity.profile_id());

    assert_ne!(first_identity.brain_id(), second_identity.brain_id());
    assert_ne!(first_identity.profile_id(), second_identity.profile_id());
    assert_ne!(first_shard, second_shard);
    drop((second_database, second_registry, second_scope));
}

#[tokio::test]
async fn one_profile_root_reopens_with_its_persisted_session_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let profile_root = temporary.path().join("stable-profile");

    let (first_identity, first_scope, first_registry) =
        open_registry_at(&profile_root, "stable-profile-first").await;
    let first_database = first_registry.profile_sessions().await.unwrap();
    let first_shard = first_database.binding().shard_id.clone();
    drop(first_database);
    drop(first_registry);
    drop(first_scope);

    let (second_identity, second_scope, second_registry) =
        open_registry_at(&profile_root, "stable-profile-second").await;
    let second_database = second_registry.profile_sessions().await.unwrap();

    assert_eq!(first_identity, second_identity);
    assert_eq!(first_shard, second_database.binding().shard_id);
    drop((second_database, second_registry, second_scope));
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
        IngestPassCoverage::Partial { deferred_units: 11 }
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
