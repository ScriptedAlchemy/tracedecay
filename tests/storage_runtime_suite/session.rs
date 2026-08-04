//! Read-only parity coverage for registered profile and project session stores.
//!
//! The helper process receives only completed copies. Fixture admission and
//! legacy compatibility reads happen through production APIs before the copy
//! boundary; the copied snapshots are reopened only by the immutable helper.
//! Every helper request carries the shared sealed copied-snapshot provenance
//! captured from the test-owned canonical copy, and the helper's revalidated
//! snapshot identity is checked on every response. This test deliberately
//! contains no raw SQL, migration, or projection-refresh path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use tracedecay::{
    application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1},
    global_db::ProjectRegistryContext,
    sessions::{
        SessionMessageRecord, SessionRecord,
        codex::{
            CodexSource, try_admit_codex_jsonl_observations_for_profile_with_admission,
            try_admit_codex_jsonl_observations_for_project_with_admission,
        },
        lcm::{LcmLoadSessionMessage, LcmLoadSessionRequest, LcmRecentSession},
    },
};
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_sqlite_parity_protocol::{
    Command, CopiedDatabase, CopiedSnapshotProvenance, DatabaseKind, PROTOCOL_VERSION, Request,
    SnapshotFileIdentity, VerifiedCopiedSnapshot, validate_request,
};
use tracedecay_store::ObservationReplayRequest;

use crate::support::{
    DatabaseArtifactInventory, DatabaseArtifactKind, IsolatedTempRoot, assert_artifacts_unchanged,
    inventory_database_artifacts, invoke_rusqlite_parity, snapshot_content_digest,
};

const PROVIDER: &str = "codex";
const PROJECT_ID: &str = "project.storage-runtime-session";
const RUSQLITE_PARITY_PROTOCOL_VERSION: u16 = PROTOCOL_VERSION;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservationMetadata {
    sequence: u64,
    observation_id: String,
    source_provider: String,
    source_session_id: String,
    scope: String,
    ordering_domain: String,
    projection_status: String,
    retrieval_anchor_id: String,
    projection_generation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionMetadata {
    record: SessionRecord,
    ordered_messages: Vec<SessionMessageRecord>,
    temporal_messages: Vec<LcmLoadSessionMessage>,
    recent_sessions: Vec<LcmRecentSession>,
    lcm_schema_version: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionStoreMetadata {
    observations: Vec<ObservationMetadata>,
    session: SessionMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectStoreMetadata {
    registry: ProjectRegistryContext,
    observations: Vec<ObservationMetadata>,
}

#[derive(Clone, Debug)]
struct CopiedStore {
    source_path: PathBuf,
    source_artifacts: DatabaseArtifactInventory,
    snapshot_path: PathBuf,
    snapshot_artifacts: DatabaseArtifactInventory,
    provenance: CopiedSnapshotProvenance,
}

#[derive(Clone, Debug, PartialEq)]
struct HelperProbe {
    metadata: Value,
    schema: Value,
    foreign_keys: Value,
    page_size: Value,
    journal_mode: Value,
    quick_check: Value,
    full_check: Value,
    allowlisted_count: Value,
    session_store: BTreeMap<String, SessionTableProbe>,
}

#[derive(Clone, Debug, PartialEq)]
struct SessionTableProbe {
    count: Value,
    schema: Value,
    rows: Vec<Value>,
}

#[derive(Clone, Copy)]
struct SessionTableSpec {
    family: &'static str,
    table: &'static str,
    order_columns: &'static [&'static str],
}

const SESSION_TABLES: &[SessionTableSpec] = &[
    SessionTableSpec {
        family: "observation",
        table: "observations",
        order_columns: &["sequence"],
    },
    SessionTableSpec {
        family: "transcript",
        table: "sessions",
        order_columns: &["provider", "session_id"],
    },
    SessionTableSpec {
        family: "transcript",
        table: "session_messages",
        order_columns: &["provider", "session_id", "ordinal", "message_id"],
    },
    SessionTableSpec {
        family: "lcm",
        table: "session_schema_migrations",
        order_columns: &["name"],
    },
    SessionTableSpec {
        family: "lcm",
        table: "lcm_raw_messages",
        order_columns: &["store_id"],
    },
    SessionTableSpec {
        family: "temporal",
        table: "session_temporal_schema_migrations",
        order_columns: &["name"],
    },
    SessionTableSpec {
        family: "temporal",
        table: "session_temporal_generations",
        order_columns: &["session_id", "generation"],
    },
    SessionTableSpec {
        family: "temporal",
        table: "session_temporal_observation_effects",
        order_columns: &["observation_sequence"],
    },
];

#[tokio::test]
async fn profile_project_and_session_snapshots_have_read_only_rusqlite_parity() {
    let root = IsolatedTempRoot::new("session");
    let fixture_session_id = fixture_session_id();

    let (profile_path, profile_before) = seed_profile_store(root.path(), &fixture_session_id).await;
    let (project_path, project_before) = seed_project_store(root.path(), &fixture_session_id).await;
    let (session_path, session_before) = seed_session_store(root.path(), &fixture_session_id).await;
    assert_eq!(profile_before.len(), 3);
    assert_eq!(project_before.registry.project.project_id, PROJECT_ID);
    assert_session_metadata_is_ordered_and_typed(&session_before.session, &fixture_session_id);

    let profile_copy = copy_checkpointed_store(&profile_path, root.path(), "profile");
    let project_copy = copy_checkpointed_store(&project_path, root.path(), "project");
    let session_copy = copy_checkpointed_store(&session_path, root.path(), "session");

    let profile_probe = probe_with_rusqlite_helper(&profile_copy, "profile");
    let project_probe = probe_with_rusqlite_helper(&project_copy, "project");
    let session_probe = probe_with_rusqlite_helper(&session_copy, "session");
    assert_helper_probes_agree(&[&profile_probe, &project_probe, &session_probe]);
    assert_observation_helper_parity(&profile_before, &profile_probe, "profile");
    assert_observation_helper_parity(&project_before.observations, &project_probe, "project");
    assert_session_helper_parity(&session_before, &session_probe, &fixture_session_id);
    assert_session_store_schema_contract(&session_probe);

    for copied in [&profile_copy, &project_copy, &session_copy] {
        assert_artifacts_unchanged(
            &copied.snapshot_artifacts,
            &inventory_database_artifacts(&copied.snapshot_path),
            "the first complete typed rusqlite helper probe",
        );
    }

    // Every typed command starts a fresh process; repeat the complete probe to
    // make the immutable reopen explicit before checking all DB/WAL/SHM bytes.
    assert_eq!(
        probe_with_rusqlite_helper(&profile_copy, "profile"),
        profile_probe
    );
    assert_eq!(
        probe_with_rusqlite_helper(&project_copy, "project"),
        project_probe
    );
    assert_eq!(
        probe_with_rusqlite_helper(&session_copy, "session"),
        session_probe
    );

    for copied in [&profile_copy, &project_copy, &session_copy] {
        assert_artifacts_unchanged(
            &copied.snapshot_artifacts,
            &inventory_database_artifacts(&copied.snapshot_path),
            "the isolated rusqlite helper and its immutable reopen",
        );
        assert_artifacts_unchanged(
            &copied.source_artifacts,
            &inventory_database_artifacts(&copied.source_path),
            "the snapshot-copy and helper path",
        );
    }
}

async fn seed_profile_store(root: &Path, session_id: &str) -> (PathBuf, Vec<ObservationMetadata>) {
    let runtime = HostAdmissionTestRuntimeV1::profile(root.join("authoritative/profile-runtime"))
        .await
        .expect("open isolated registered profile store");
    let transcript = write_admission_fixture(root, "profile", None);
    let expected_bytes = fs::metadata(&transcript)
        .expect("profile fixture metadata")
        .len();

    let admission = runtime.facade();
    let progress = try_admit_codex_jsonl_observations_for_profile_with_admission(
        &transcript,
        Some(session_id),
        &[],
        &admission,
        None,
    )
    .await
    .expect("checked-in Codex fixture must pass profile production admission");
    assert_eq!(progress.bytes_consumed, expected_bytes);
    assert!(!progress.source_deferred);

    let metadata = read_observation_metadata(&runtime, HostAdmissionScope::Profile).await;
    assert_normalized_observations(&metadata, session_id, "profile");
    runtime
        .checkpoint_session_database_for_test(HostAdmissionScope::Profile)
        .await
        .expect("checkpoint registered profile session store");
    let snapshot_path = root.join("checkpointed-sources/profile.db");
    fs::create_dir_all(snapshot_path.parent().expect("profile snapshot parent"))
        .expect("create profile snapshot directory");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Profile, &snapshot_path)
        .await
        .expect("snapshot registered profile session store");
    drop(runtime);
    (snapshot_path, metadata)
}

async fn seed_project_store(root: &Path, session_id: &str) -> (PathBuf, ProjectStoreMetadata) {
    let profile_root = root.join("authoritative/project-profile");
    let project_root = root.join("project-root");
    fs::create_dir_all(&project_root).expect("create isolated project root");
    let transcript = write_admission_fixture(root, "project", Some(&project_root));
    let expected_bytes = fs::metadata(&transcript)
        .expect("project fixture metadata")
        .len();
    let project_id = ProjectId::new(PROJECT_ID).expect("valid project identity");
    let runtime =
        HostAdmissionTestRuntimeV1::project(&profile_root, &project_root, project_id.clone())
            .await
            .expect("open isolated registered project store");
    assert!(
        runtime
            .upsert_code_project(project_id.as_str(), &project_root, None, None, Some("main"))
            .await
            .is_some(),
        "register the authoritative project before project-scoped admission"
    );

    let admission = runtime.facade();
    let progress = try_admit_codex_jsonl_observations_for_project_with_admission(
        &transcript,
        &project_root,
        project_id,
        &admission,
        None,
    )
    .await
    .expect("checked-in Codex fixture must pass project production admission");
    assert_eq!(progress.bytes_consumed, expected_bytes);
    assert!(!progress.source_deferred);

    let metadata = read_project_store_metadata(&runtime).await;
    assert_eq!(metadata.registry.project.project_id, PROJECT_ID);
    assert_normalized_observations(
        &metadata.observations,
        session_id,
        &format!("project:{PROJECT_ID}"),
    );
    runtime
        .checkpoint_session_database_for_test(HostAdmissionScope::Project)
        .await
        .expect("checkpoint registered project session store");
    let snapshot_path = root.join("checkpointed-sources/project.db");
    fs::create_dir_all(snapshot_path.parent().expect("project snapshot parent"))
        .expect("create project snapshot directory");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Project, &snapshot_path)
        .await
        .expect("snapshot registered project session store");
    drop(runtime);
    (snapshot_path, metadata)
}

async fn seed_session_store(root: &Path, session_id: &str) -> (PathBuf, SessionStoreMetadata) {
    let profile_root = root.join("authoritative/session-profile");
    let project_root = root.join("session-project-root");
    let home = root.join("session-home");
    fs::create_dir_all(&project_root).expect("create isolated session project root");
    let transcript = write_legacy_rollout(&home, &project_root);
    let expected_bytes = fs::metadata(&transcript)
        .expect("session fixture metadata")
        .len();
    let project_id = ProjectId::new(PROJECT_ID).expect("valid project identity");
    let runtime =
        HostAdmissionTestRuntimeV1::project(&profile_root, &project_root, project_id.clone())
            .await
            .expect("open isolated registered session store");
    assert!(
        runtime
            .upsert_code_project(project_id.as_str(), &project_root, None, None, Some("main"))
            .await
            .is_some(),
        "register the authoritative project before session fixture admission"
    );

    let admission_facade = runtime.facade();
    let admission = try_admit_codex_jsonl_observations_for_project_with_admission(
        &transcript,
        &project_root,
        project_id,
        &admission_facade,
        None,
    )
    .await
    .expect("checked-in Codex fixture must pass session-store production admission");
    assert_eq!(admission.bytes_consumed, expected_bytes);
    assert!(!admission.source_deferred);

    let ingest = runtime
        .ingest_project_transcript_source_for_test(
            &CodexSource::with_home(&home),
            &project_root,
            None,
        )
        .await
        .expect("checked-in Codex fixture must pass legacy production ingestion");
    assert_eq!(ingest.sessions_upserted, 1);
    assert!(
        ingest.messages_upserted > 0,
        "the real fixture must produce at least one legacy session message"
    );

    let metadata = read_session_store_metadata(&runtime, session_id).await;
    assert_normalized_observations(
        &metadata.observations,
        session_id,
        &format!("project:{PROJECT_ID}"),
    );
    assert_session_metadata_is_ordered_and_typed(&metadata.session, session_id);
    runtime
        .checkpoint_session_database_for_test(HostAdmissionScope::Project)
        .await
        .expect("checkpoint registered project session store");
    let snapshot_path = root.join("checkpointed-sources/session.db");
    fs::create_dir_all(snapshot_path.parent().expect("session snapshot parent"))
        .expect("create session snapshot directory");
    runtime
        .snapshot_session_database_for_test(HostAdmissionScope::Project, &snapshot_path)
        .await
        .expect("snapshot registered session store");
    drop(runtime);
    (snapshot_path, metadata)
}

fn copy_checkpointed_store(source_path: &Path, root: &Path, label: &str) -> CopiedStore {
    let source_artifacts = inventory_database_artifacts(source_path);
    assert_checkpoint_has_no_live_sidecars(&source_artifacts, label);

    let snapshots = root.join("copied-snapshots");
    fs::create_dir_all(&snapshots).expect("create copied-snapshot directory");
    let snapshot_path = snapshots.join(format!("{label}.db"));
    fs::copy(source_path, &snapshot_path).expect("copy checkpointed database snapshot");
    let staging_root = snapshots
        .canonicalize()
        .expect("canonicalize copied-snapshot staging root");
    let canonical_path = snapshot_path
        .canonicalize()
        .expect("canonicalize copied snapshot");
    assert!(
        canonical_path.starts_with(&staging_root),
        "the copied {label} snapshot must stay inside its private staging root"
    );
    let snapshot_artifacts = inventory_database_artifacts(&canonical_path);
    assert_checkpoint_has_no_live_sidecars(&snapshot_artifacts, label);
    assert_eq!(
        source_artifacts
            .artifacts
            .get(&DatabaseArtifactKind::Database),
        snapshot_artifacts
            .artifacts
            .get(&DatabaseArtifactKind::Database),
        "the copied {label} snapshot must exactly match its checkpointed authority database"
    );

    let metadata =
        fs::metadata(&canonical_path).expect("read sealed copied-snapshot metadata for provenance");
    let provenance = CopiedSnapshotProvenance {
        authority_identity: format!("storage-runtime-suite:session-store-authority:{label}"),
        staging_root,
        canonical_path: canonical_path.clone(),
        byte_len: metadata.len(),
        content_digest: snapshot_content_digest(&canonical_path),
        file_identity: SnapshotFileIdentity::from_metadata(&metadata),
    };

    CopiedStore {
        source_path: source_path.to_path_buf(),
        source_artifacts,
        snapshot_path: canonical_path,
        snapshot_artifacts,
        provenance,
    }
}

fn assert_checkpoint_has_no_live_sidecars(inventory: &DatabaseArtifactInventory, label: &str) {
    for artifact in [DatabaseArtifactKind::Wal, DatabaseArtifactKind::Shm] {
        assert!(
            inventory
                .artifacts
                .get(&artifact)
                .is_some_and(Option::is_none),
            "checkpointed {label} store retained a live {artifact:?} sidecar: {inventory:#?}"
        );
    }
}

/// Revalidates the sealed provenance against the live filesystem before any
/// helper request is issued for the copy, so a replaced or mutated snapshot is
/// caught by the test authority before the process boundary is crossed.
fn revalidate_copied_snapshot(copied: &CopiedStore, label: &str) {
    let canonical_path = copied
        .snapshot_path
        .canonicalize()
        .expect("re-canonicalize the copied snapshot before helper requests");
    assert_eq!(
        canonical_path, copied.provenance.canonical_path,
        "the copied {label} snapshot moved after its provenance was sealed"
    );
    assert!(
        canonical_path.starts_with(&copied.provenance.staging_root),
        "the copied {label} snapshot escaped its sealed staging root"
    );
    let metadata = fs::metadata(&canonical_path)
        .expect("re-read the copied snapshot metadata before helper requests");
    assert_eq!(
        metadata.len(),
        copied.provenance.byte_len,
        "the copied {label} snapshot changed length after its provenance was sealed"
    );
    assert_eq!(
        SnapshotFileIdentity::from_metadata(&metadata),
        copied.provenance.file_identity,
        "the copied {label} snapshot was replaced after its provenance was sealed"
    );
    assert_eq!(
        snapshot_content_digest(&canonical_path),
        copied.provenance.content_digest,
        "the copied {label} snapshot content changed after its provenance was sealed"
    );
    assert_artifacts_unchanged(
        &copied.snapshot_artifacts,
        &inventory_database_artifacts(&canonical_path),
        "the window between copied-snapshot sealing and helper requests",
    );
}

fn fixture_session_id() -> String {
    let session_meta: Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/codex/session_meta.input.json"
    ))
    .expect("checked-in Codex session metadata fixture");
    session_meta
        .pointer("/payload/id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .expect("checked-in Codex session metadata must carry payload.id")
}

fn fixture_records(project_root: Option<&Path>) -> Vec<Value> {
    let session_meta =
        include_str!("../fixtures/provider_normalization/codex/session_meta.input.json");
    let agent_message =
        include_str!("../fixtures/provider_normalization/codex/agent_message.input.json");
    let function_call =
        include_str!("../fixtures/provider_normalization/codex/function_call.input.json");
    assert!(
        [session_meta, agent_message, function_call]
            .iter()
            .all(|fixture| fixture.ends_with('\n')),
        "checked-in provider-normalization records must retain JSONL terminators"
    );

    let mut session_meta: Value =
        serde_json::from_str(session_meta).expect("checked-in Codex session metadata fixture");
    if let Some(project_root) = project_root {
        let payload = session_meta
            .get_mut("payload")
            .and_then(Value::as_object_mut)
            .expect("checked-in Codex session metadata must contain a payload object");
        assert!(
            payload.contains_key("cwd"),
            "checked-in Codex session metadata must contain its native cwd field"
        );
        payload.insert(
            "cwd".to_owned(),
            Value::String(project_root.to_string_lossy().into_owned()),
        );
    }

    vec![
        session_meta,
        serde_json::from_str(agent_message).expect("checked-in Codex agent-message fixture"),
        serde_json::from_str(function_call).expect("checked-in Codex function-call fixture"),
    ]
}

fn write_admission_fixture(root: &Path, label: &str, project_root: Option<&Path>) -> PathBuf {
    let directory = root.join("fixture-input").join(label);
    fs::create_dir_all(&directory).expect("create fixture-input directory");
    let path = directory.join("rollout-codex-golden-session.jsonl");
    write_jsonl(&path, fixture_records(project_root));
    path
}

fn write_legacy_rollout(home: &Path, project_root: &Path) -> PathBuf {
    let directory = home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&directory).expect("create isolated Codex rollout directory");
    let path = directory.join("rollout-2026-01-01T00-00-00-codex-golden-session.jsonl");
    write_jsonl(&path, fixture_records(Some(project_root)));
    path
}

fn write_jsonl(path: &Path, records: Vec<Value>) {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).expect("encode checked-in JSONL record");
        bytes.push(b'\n');
    }
    fs::write(path, bytes).expect("write checked-in JSONL fixture copy");
}

async fn read_observation_metadata(
    runtime: &HostAdmissionTestRuntimeV1,
    scope: HostAdmissionScope,
) -> Vec<ObservationMetadata> {
    runtime
        .replay_observations(
            scope,
            ObservationReplayRequest::new(0, 64).expect("bounded replay request"),
        )
        .await
        .expect("read normalized observations")
        .into_iter()
        .map(|stored| {
            let observation = stored.observation();
            ObservationMetadata {
                sequence: stored.sequence(),
                observation_id: observation.observation_id().as_str().to_owned(),
                source_provider: observation.source().provider().as_str().to_owned(),
                source_session_id: observation.source().session_id().as_str().to_owned(),
                scope: normalized_scope(observation.scope()),
                ordering_domain: observation.identity().ordering_domain().as_str().to_owned(),
                projection_status: format!("{:?}", stored.projection_status()),
                retrieval_anchor_id: stored.retrieval_anchor_id().as_str().to_owned(),
                projection_generation: stored.projection_generation().as_str().to_owned(),
            }
        })
        .collect()
}

async fn read_session_store_metadata(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &str,
) -> SessionStoreMetadata {
    let observations = read_observation_metadata(runtime, HostAdmissionScope::Project).await;
    let record = runtime
        .session_for_test(HostAdmissionScope::Project, PROVIDER, session_id)
        .await
        .expect("read registered session record")
        .expect("legacy production ingestion must retain the fixture session");
    let temporal_messages = load_all_temporal_messages(runtime, session_id).await;
    let mut ordered_messages = Vec::with_capacity(temporal_messages.len());
    for message in &temporal_messages {
        ordered_messages.push(
            runtime
                .session_message_for_test(
                    HostAdmissionScope::Project,
                    PROVIDER,
                    &message.message_id,
                )
                .await
                .expect("read registered session message")
                .expect("each ordered raw message must retain its legacy projection"),
        );
    }
    let recent_sessions = runtime
        .lcm_recent_sessions_for_test(Some(PROVIDER), 16)
        .await
        .expect("read temporal session metadata");
    let lcm_schema_version = runtime
        .lcm_schema_migration_version_for_test(HostAdmissionScope::Project)
        .await
        .expect("read registered LCM schema version");

    SessionStoreMetadata {
        observations,
        session: SessionMetadata {
            record,
            ordered_messages,
            temporal_messages,
            recent_sessions,
            lcm_schema_version,
        },
    }
}

async fn read_project_store_metadata(runtime: &HostAdmissionTestRuntimeV1) -> ProjectStoreMetadata {
    let registry = runtime
        .project_registry_context_by_id(PROJECT_ID)
        .await
        .expect("the seeded project registry row must be readable");
    let observations = read_observation_metadata(runtime, HostAdmissionScope::Project).await;
    ProjectStoreMetadata {
        registry,
        observations,
    }
}

async fn load_all_temporal_messages(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &str,
) -> Vec<LcmLoadSessionMessage> {
    let mut after_store_id = None;
    let mut messages = Vec::new();
    loop {
        let page = runtime
            .lcm_load_session_for_test(LcmLoadSessionRequest {
                provider: PROVIDER.to_owned(),
                session_id: session_id.to_owned(),
                after_store_id,
                limit: 100,
                roles: Vec::new(),
                start_time: None,
                end_time: None,
                content_slice: None,
            })
            .await
            .expect("load ordered temporal session page");
        messages.extend(page.messages);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        after_store_id = Some(
            cursor
                .parse()
                .expect("temporal page cursor must contain an ordered store id"),
        );
    }
    messages
}

fn normalized_scope(scope: &ObservationScopeV1) -> String {
    match scope {
        ObservationScopeV1::Profile => "profile".to_owned(),
        ObservationScopeV1::Project { project_id } => format!("project:{}", project_id.as_str()),
    }
}

fn assert_normalized_observations(
    observations: &[ObservationMetadata],
    session_id: &str,
    expected_scope: &str,
) {
    assert_eq!(
        observations.len(),
        3,
        "the checked-in session_meta, agent_message, and function_call fixtures must all persist"
    );
    assert_eq!(
        observations
            .iter()
            .map(|observation| observation.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "production admission must retain deterministic observation order"
    );
    assert!(observations.iter().all(|observation| {
        observation.source_provider == PROVIDER
            && observation.source_session_id == session_id
            && observation.scope == expected_scope
            && observation.ordering_domain == "file_bytes"
    }));
    assert!(
        observations
            .windows(2)
            .all(|pair| pair[0].observation_id != pair[1].observation_id),
        "normalized observation identities must stay distinct in source order"
    );
}

fn assert_session_metadata_is_ordered_and_typed(metadata: &SessionMetadata, session_id: &str) {
    assert_eq!(metadata.record.provider, PROVIDER);
    assert_eq!(metadata.record.session_id, session_id);
    assert!(!metadata.ordered_messages.is_empty());
    assert_eq!(
        metadata
            .ordered_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        metadata
            .temporal_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        "legacy and temporal projections must expose identical ordered message ids"
    );
    assert!(
        metadata
            .temporal_messages
            .windows(2)
            .all(|pair| pair[0].store_id < pair[1].store_id)
    );
    assert_eq!(metadata.recent_sessions.len(), 1);
    assert_eq!(metadata.recent_sessions[0].provider, PROVIDER);
    assert_eq!(metadata.recent_sessions[0].session_id, session_id);
    assert_eq!(
        metadata.recent_sessions[0].message_count,
        i64::try_from(metadata.temporal_messages.len()).expect("message count fits i64")
    );
    assert_eq!(
        metadata.recent_sessions[0].last_store_id,
        metadata
            .temporal_messages
            .last()
            .expect("nonempty temporal message list")
            .store_id
    );
    assert!(
        metadata.lcm_schema_version.is_some(),
        "the session store must retain typed temporal schema metadata"
    );
}

fn assert_observation_helper_parity(
    legacy: &[ObservationMetadata],
    helper: &HelperProbe,
    label: &str,
) {
    let probe = helper_table(helper, "observations");
    assert_eq!(
        probe.count["row_count"].as_u64(),
        Some(u64::try_from(legacy.len()).expect("legacy observation count fits u64")),
        "{label} observation count differs across the registered runtime and rusqlite"
    );
    let legacy_keys = legacy
        .iter()
        .map(|observation| (observation.sequence, observation.observation_id.as_str()))
        .collect::<Vec<_>>();
    let helper_keys = probe
        .rows
        .iter()
        .map(|row| {
            (
                row["sequence"]
                    .as_u64()
                    .expect("helper observation sequence"),
                row["observation_id"]
                    .as_str()
                    .expect("helper observation id"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(helper_keys, legacy_keys, "{label} observation key order");
}

fn assert_session_helper_parity(
    legacy: &SessionStoreMetadata,
    helper: &HelperProbe,
    session_id: &str,
) {
    assert_observation_helper_parity(&legacy.observations, helper, "session-store");

    let sessions = helper_table(helper, "sessions");
    assert_eq!(sessions.count["row_count"], 1);
    assert_eq!(sessions.rows.len(), 1);
    assert_eq!(sessions.rows[0]["provider"].as_str(), Some(PROVIDER));
    assert_eq!(sessions.rows[0]["session_id"].as_str(), Some(session_id));

    let legacy_message_keys = legacy
        .session
        .ordered_messages
        .iter()
        .map(|message| {
            (
                message.provider.as_str(),
                message.session_id.as_str(),
                message.ordinal,
                message.message_id.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let messages = helper_table(helper, "session_messages");
    assert_eq!(
        messages.count["row_count"].as_u64(),
        Some(u64::try_from(legacy_message_keys.len()).expect("legacy message count fits u64"))
    );
    let helper_message_keys = messages
        .rows
        .iter()
        .map(|row| {
            (
                row["provider"].as_str().expect("helper message provider"),
                row["session_id"]
                    .as_str()
                    .expect("helper message session id"),
                row["ordinal"].as_i64().expect("helper message ordinal"),
                row["message_id"].as_str().expect("helper message id"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        helper_message_keys, legacy_message_keys,
        "legacy transcript and helper keyset ordering must agree"
    );

    let legacy_raw_keys = legacy
        .session
        .temporal_messages
        .iter()
        .map(|message| {
            (
                message.store_id,
                message.provider.as_str(),
                message.session_id.as_str(),
                message.ordinal,
                message.message_id.as_str(),
                message.content_hash.as_str(),
            )
        })
        .collect::<Vec<_>>();
    let raw_messages = helper_table(helper, "lcm_raw_messages");
    assert_eq!(
        raw_messages.count["row_count"].as_u64(),
        Some(u64::try_from(legacy_raw_keys.len()).expect("legacy raw-message count fits u64"))
    );
    let helper_raw_keys = raw_messages
        .rows
        .iter()
        .map(|row| {
            (
                row["store_id"].as_i64().expect("helper raw store id"),
                row["provider"].as_str().expect("helper raw provider"),
                row["session_id"].as_str().expect("helper raw session id"),
                row["ordinal"].as_i64().expect("helper raw ordinal"),
                row["message_id"].as_str().expect("helper raw message id"),
                row["content_hash"]
                    .as_str()
                    .expect("helper raw content hash"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        helper_raw_keys, legacy_raw_keys,
        "legacy temporal reads and helper store-id ordering must agree"
    );
    assert!(
        helper_raw_keys.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "helper raw-message keyset pages must remain strictly ordered"
    );

    let helper_lcm_versions = helper_table(helper, "session_schema_migrations")
        .rows
        .iter()
        .map(|row| row["version"].as_i64().expect("helper LCM schema version"))
        .collect::<Vec<_>>();
    assert!(
        legacy
            .session
            .lcm_schema_version
            .is_some_and(|version| helper_lcm_versions.contains(&version)),
        "registered runtime and helper must report the same persisted LCM schema version"
    );
}

fn assert_session_store_schema_contract(helper: &HelperProbe) {
    let expected_columns: &[(&str, &[&str])] = &[
        (
            "observations",
            &[
                "sequence",
                "observation_id",
                "payload_digest",
                "receipt_id",
                "observation_json",
                "committed_cursor_json",
            ],
        ),
        (
            "sessions",
            &[
                "provider",
                "session_id",
                "project_key",
                "project_path",
                "title",
                "started_at",
                "ended_at",
                "transcript_path",
                "metadata_json",
                "parent_session_id",
                "is_subagent",
                "agent_id",
                "parent_tool_use_id",
            ],
        ),
        (
            "session_messages",
            &[
                "provider",
                "message_id",
                "session_id",
                "role",
                "timestamp",
                "ordinal",
                "text",
                "kind",
                "model",
                "tool_names",
                "source_path",
                "source_offset",
                "metadata_json",
            ],
        ),
        (
            "session_schema_migrations",
            &["name", "version", "applied_at"],
        ),
        (
            "lcm_raw_messages",
            &[
                "provider",
                "message_id",
                "session_id",
                "store_id",
                "role",
                "ordinal",
                "timestamp",
                "content",
                "content_hash",
                "storage_kind",
                "payload_ref",
                "snippet_text",
                "index_text",
                "legacy_source",
                "legacy_truncated",
                "metadata_json",
            ],
        ),
        (
            "session_temporal_schema_migrations",
            &["name", "version", "applied_at"],
        ),
        (
            "session_temporal_generations",
            &[
                "session_id",
                "generation",
                "state",
                "frozen_watermarks_json",
                "created_at",
                "ready_at",
                "activated_at",
                "completed_at",
            ],
        ),
        (
            "session_temporal_observation_effects",
            &[
                "observation_id",
                "observation_sequence",
                "session_id",
                "receipt_id",
                "effect_digest",
                "output_count",
                "recorded_at",
            ],
        ),
    ];
    for (table, expected) in expected_columns {
        let observed = helper_table(helper, table).schema["columns"]
            .as_array()
            .expect("typed schema columns")
            .iter()
            .map(|column| column["name"].as_str().expect("typed schema column name"))
            .collect::<Vec<_>>();
        assert_eq!(observed, *expected, "schema columns for {table}");
    }

    assert_foreign_key_contract(
        helper,
        "observations",
        &["receipt_id->sanitization_receipts.receipt_id"],
    );
    assert_foreign_key_contract(helper, "sessions", &[]);
    assert_foreign_key_contract(
        helper,
        "session_messages",
        &[
            "provider->sessions.provider",
            "session_id->sessions.session_id",
        ],
    );
    assert_foreign_key_contract(helper, "session_schema_migrations", &[]);
    assert_foreign_key_contract(
        helper,
        "lcm_raw_messages",
        &[
            "provider->sessions.provider",
            "session_id->sessions.session_id",
        ],
    );
    assert_foreign_key_contract(helper, "session_temporal_schema_migrations", &[]);
    assert_foreign_key_contract(helper, "session_temporal_generations", &[]);
    assert_foreign_key_contract(
        helper,
        "session_temporal_observation_effects",
        &[
            "observation_id->observations.observation_id",
            "receipt_id->sanitization_receipts.receipt_id",
        ],
    );
}

fn assert_foreign_key_contract(helper: &HelperProbe, table: &str, expected: &[&str]) {
    let mut observed = helper_table(helper, table).schema["foreign_keys"]
        .as_array()
        .expect("typed foreign-key metadata")
        .iter()
        .map(|key| {
            format!(
                "{}->{}.{}",
                key["from_column"].as_str().expect("foreign-key source"),
                key["referenced_table"]
                    .as_str()
                    .expect("foreign-key target table"),
                key["to_column"]
                    .as_str()
                    .expect("foreign-key target column")
            )
        })
        .collect::<Vec<_>>();
    observed.sort();
    let mut expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(observed, expected, "foreign-key contract for {table}");
}

fn helper_table<'a>(helper: &'a HelperProbe, table: &str) -> &'a SessionTableProbe {
    helper
        .session_store
        .get(table)
        .unwrap_or_else(|| panic!("missing helper probe for {table}"))
}

fn probe_with_rusqlite_helper(copied: &CopiedStore, label: &str) -> HelperProbe {
    revalidate_copied_snapshot(copied, label);
    let metadata = helper_command(copied, label, "metadata", json!({ "type": "metadata" }));
    assert_eq!(metadata["query_only"].as_bool(), Some(true));
    assert_eq!(metadata["immutable"].as_bool(), Some(true));
    assert_eq!(
        metadata["canonical_path"].as_str(),
        Some(copied.provenance.canonical_path.to_string_lossy().as_ref())
    );
    assert!(metadata["sqlite_version"].as_str().is_some());
    assert!(metadata["compile_options"].as_array().is_some());

    let schema = helper_command(copied, label, "schema", json!({ "type": "schema" }));
    let table_names = schema["objects"]
        .as_array()
        .expect("typed schema objects")
        .iter()
        .filter(|object| object["kind"].as_str() == Some("table"))
        .filter_map(|object| object["name"].as_str())
        .collect::<BTreeSet<_>>();
    for table in [
        "observations",
        "sessions",
        "session_messages",
        "session_schema_migrations",
        "lcm_raw_messages",
        "session_temporal_schema_migrations",
        "session_temporal_generations",
        "session_temporal_observation_effects",
    ] {
        assert!(
            table_names.contains(table),
            "typed schema output omitted required session-store table {table:?}"
        );
    }

    let foreign_keys = helper_command(
        copied,
        label,
        "foreign-keys",
        json!({ "type": "foreign_keys" }),
    );
    assert!(foreign_keys["enabled"].as_bool().is_some());
    let page_size = helper_command(copied, label, "page-size", json!({ "type": "page_size" }));
    assert!(page_size["bytes"].as_u64().is_some_and(|bytes| bytes > 0));
    let journal_mode = helper_command(
        copied,
        label,
        "journal-mode",
        json!({ "type": "journal_mode" }),
    );
    assert!(journal_mode["mode"].as_str().is_some());
    let quick_check = helper_command(
        copied,
        label,
        "quick-check",
        json!({ "type": "integrity", "check": "quick" }),
    );
    assert_eq!(quick_check["findings"], json!(["ok"]));
    let full_check = helper_command(
        copied,
        label,
        "full-check",
        json!({ "type": "integrity", "check": "full" }),
    );
    assert_eq!(full_check["findings"], json!(["ok"]));

    let session_store = SESSION_TABLES
        .iter()
        .map(|spec| {
            (
                spec.table.to_owned(),
                probe_session_table(copied, label, *spec),
            )
        })
        .collect();

    HelperProbe {
        metadata,
        schema,
        foreign_keys,
        page_size,
        journal_mode,
        quick_check,
        full_check,
        allowlisted_count,
        session_store,
    }
}

fn probe_session_table(
    copied: &CopiedStore,
    label: &str,
    spec: SessionTableSpec,
) -> SessionTableProbe {
    let count = helper_command(
        copied,
        label,
        &format!("{}-count", spec.table),
        json!({
            "type": "session_store_count",
            "family": spec.family,
            "table": spec.table,
        }),
    );
    assert_eq!(count["family"].as_str(), Some(spec.family));
    assert_eq!(count["table"].as_str(), Some(spec.table));

    let schema = helper_command(
        copied,
        label,
        &format!("{}-schema", spec.table),
        json!({
            "type": "session_store_schema",
            "family": spec.family,
            "table": spec.table,
        }),
    );
    assert_eq!(schema["exists"].as_bool(), Some(true));
    assert_eq!(schema["family"].as_str(), Some(spec.family));
    assert_eq!(schema["table"].as_str(), Some(spec.table));

    let mut cursor = Value::Null;
    let mut rows = Vec::new();
    for page_ordinal in 0..128 {
        let page = helper_command(
            copied,
            label,
            &format!("{}-page-{page_ordinal}", spec.table),
            json!({
                "type": "session_store_page",
                "family": spec.family,
                "table": spec.table,
                "cursor": cursor,
                "limit": 2,
            }),
        );
        assert_eq!(page["family"].as_str(), Some(spec.family));
        assert_eq!(page["table"].as_str(), Some(spec.table));
        assert_eq!(page["order_columns"], json!(spec.order_columns));
        assert_eq!(page["digest_algorithm"].as_str(), Some("sha256-v1"));
        let page_rows = page["rows"]
            .as_array()
            .expect("typed session-store page rows");
        assert!(page_rows.len() <= 2);
        assert!(page_rows.iter().all(|row| {
            row["table"].as_str() == Some(spec.table)
                && row["row_digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))
        }));
        rows.extend(page_rows.iter().cloned());
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            let row_count = count["row_count"]
                .as_u64()
                .expect("required session-store table count");
            assert_eq!(
                u64::try_from(rows.len()).expect("row page count fits u64"),
                row_count,
                "keyset pages must cover the complete {:?} table exactly once",
                spec.table
            );
            return SessionTableProbe {
                count,
                schema,
                rows,
            };
        }
    }
    panic!(
        "session-store keyset pagination for {:?} exceeded its bounded page budget",
        spec.table
    );
}

fn helper_command(copied: &CopiedStore, label: &str, command_name: &str, command: Value) -> Value {
    let command: Command = serde_json::from_value(command)
        .expect("session-suite commands must decode as shared typed protocol commands");
    let request = Request {
        protocol_version: RUSQLITE_PARITY_PROTOCOL_VERSION,
        request_id: format!("session-{label}-{command_name}"),
        database: CopiedDatabase {
            path: copied.provenance.canonical_path.clone(),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: copied.provenance.clone(),
        },
        command,
    };
    validate_request(&request)
        .expect("revalidate the typed parity request before invoking the helper");
    let expected_verified = VerifiedCopiedSnapshot {
        authority_identity: request.database.provenance.authority_identity.clone(),
        canonical_path: request.database.provenance.canonical_path.clone(),
        byte_len: request.database.provenance.byte_len,
        content_digest: request.database.provenance.content_digest.clone(),
        file_identity: request.database.provenance.file_identity.clone(),
    };
    let response = invoke_rusqlite_parity(&request);
    assert_eq!(
        response["protocol_version"].as_u64(),
        Some(u64::from(RUSQLITE_PARITY_PROTOCOL_VERSION))
    );
    assert_eq!(
        response["request_id"].as_str(),
        Some(request.request_id.as_str())
    );
    assert_eq!(
        response["status"].as_str(),
        Some("ok"),
        "typed helper command {command_name:?} failed: {response:#}"
    );
    let verified: VerifiedCopiedSnapshot =
        serde_json::from_value(response["verified_snapshot"].clone())
            .expect("typed helper response must carry a revalidated copied-snapshot identity");
    assert_eq!(
        verified, expected_verified,
        "the helper must revalidate the sealed provenance before honoring {command_name:?}"
    );
    response["output"].clone()
}

fn assert_helper_probes_agree(probes: &[&HelperProbe]) {
    let (first, rest) = probes
        .split_first()
        .expect("profile, project, and session helper probes");
    for probe in rest {
        assert_eq!(
            metadata_without_path(&probe.metadata),
            metadata_without_path(&first.metadata),
            "the bundled SQLite metadata must agree across copied registered stores"
        );
        assert_eq!(probe.schema, first.schema);
        assert_eq!(probe.foreign_keys, first.foreign_keys);
        assert_eq!(probe.page_size, first.page_size);
        assert_eq!(probe.journal_mode, first.journal_mode);
        assert_eq!(probe.quick_check, first.quick_check);
        assert_eq!(probe.full_check, first.full_check);
        assert_eq!(probe.allowlisted_count, first.allowlisted_count);
        for spec in SESSION_TABLES {
            assert_eq!(
                helper_table(probe, spec.table).schema,
                helper_table(first, spec.table).schema,
                "typed session-store schema differs for {}",
                spec.table
            );
        }
    }
}

fn metadata_without_path(metadata: &Value) -> Value {
    let mut metadata = metadata.clone();
    metadata
        .as_object_mut()
        .expect("typed metadata output object")
        .remove("canonical_path");
    metadata
}
