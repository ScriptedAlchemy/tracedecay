use std::fs;
use std::path::Path;
use std::sync::Arc;
use tracedecay_runtime_core::path_safety::{plain_git_args, plain_host_path};

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, EvidenceAvailabilityV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId, UserProfileId,
};
use tracedecay_global_db::{GlobalDbObservationStore, RegisteredGlobalDb};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestRuntimeProfileIdentityV1,
};
use tracedecay_sessions::admission::HostAdmissionStatus;
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_store::{ObservationPersistOutcome, ObservationReplayRequest, ObservationStore};

use crate::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ExternalSourceProjectionStateV1,
    ObservationCancellation,
};

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
}

fn enroll_project(path: &Path, project_id: &ProjectId) {
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(path, project_id.as_str())
        .unwrap();
}

fn observation_store(database: &RegisteredGlobalDb) -> GlobalDbObservationStore {
    database.observation_store()
}

fn host_capture_request(scope: ObservationScopeV1, record_id: &str) -> CaptureObservationRequest {
    // Host admission sanitizes through the single provider-neutral
    // observation path (RecordSanitizerV1::observation_v1), so the fixture
    // must present a canonical observation envelope rather than a raw
    // provider frame.
    let encoded = serde_json::to_vec(&json!({ "text": "host provenance fixture" })).unwrap();
    let range = ObservationSourceRangeV1::new(0, u64::try_from(encoded.len()).unwrap()).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    // One source lane per record. Every fixture frame starts at offset 0 with
    // no expected cursor, so sharing a session would make the second distinct
    // record collide with the first record's advanced cursor and short-circuit
    // as `cursor_conflict` before the authority is ever consulted. Replays
    // still resolve as exact duplicates because they reuse `record_id`.
    let session_id = format!("session.{record_id}");
    let envelope_session = session_id.clone();
    let envelope_record = record_id.to_owned();
    let parsed =
        parse_normalized_observation_record_v1(&encoded, range, ordering_domain, move |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new("claude").unwrap(),
                "message",
                ObservationId::new(envelope_record.clone()).unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new(envelope_session.clone()).unwrap(),
                )
                .with_message_id(ObservationId::new(envelope_record.clone()).unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::User,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(ordering_domain, range),
            )
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("claude").unwrap(),
                SessionId::new(session_id).unwrap(),
            )
            .unwrap(),
            scope,
            ObservationSourceGenerationV1::new(41).unwrap(),
            range,
            ordering_domain,
            ObservationId::new(record_id).unwrap(),
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.host-provenance-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap()
}

fn facade_with_background_cpu<'a>(
    background_cpu: &Arc<ProcessBackgroundCpuV1>,
    authorities: HostAdmissionAuthorities<'a>,
) -> HostAdmissionFacade<'a> {
    HostAdmissionFacade::new(authorities.with_background_cpu(Arc::clone(background_cpu)))
}

async fn registered_database_for_test(
    path: &Path,
    identity: &tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1,
    scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
) -> (
    tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    tracedecay_global_db::RegisteredGlobalDbOwnerV1,
) {
    tracedecay_global_db::register_registered_schema_installer();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let authority =
        DatabaseAuthority::for_owned_runtime(path, "host-admission registered test runtime")
            .unwrap();
    let fixture = Database::publish_registered_daemon_test_runtime_with_retirement_control_for_profile_identity(
        path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestRuntimeProfileIdentityV1::new(identity.brain_id().clone(), identity.profile_id().clone()),
        scope,
    ).await.unwrap();
    let (database, _runtime, _retirement) = fixture.into_parts();
    let owner = tracedecay_global_db::RegisteredGlobalDbOwnerV1::admit_and_attach(database)
        .await
        .unwrap();
    let lease = owner.issue_lease().unwrap();
    tracedecay_global_db::tests::harness::bind_test_session_relation_graph(&lease).unwrap();
    (lease, owner)
}

#[tokio::test]
async fn host_ingress_binds_provenance_to_authoritative_project_and_replays_stably() {
    let background_cpu = super::host_admission_batch_test::background_cpu_for_host_admission_test();
    let root = TempDir::new().unwrap();
    let repository_root = root.path().join("repository");
    initialize_repository(&repository_root);
    let project_id = ProjectId::new("project.host-provenance").unwrap();
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&root.path().join("profile"))
            .unwrap();
    let _daemon_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "host-provenance-authority-test",
    )
    .unwrap();
    enroll_project(&repository_root, &project_id);
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            &repository_root,
            project_id.as_str(),
        )
        .unwrap()
    );
    let marker =
        tracedecay_runtime_core::storage::read_repository_identity_marker(&repository_root)
            .unwrap()
            .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &repository_root,
        identity.profile_root(),
        project_id.as_str(),
    )
    .unwrap();
    let (project_registered, _project_owner) = registered_database_for_test(
        &layout.sessions_db_path,
        &identity,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await;
    let (profile_registered, _profile_owner) = registered_database_for_test(
        &tracedecay_sessions::runtime::user_sessions_db_path(identity.profile_root()),
        &identity,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await;
    let provenance = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
        &repository_root,
        &project_id,
        &marker,
    )
    .unwrap();
    let facade = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
            project_registered.as_ref(),
        )
        .with_repository_provenance(provenance.clone()),
    );
    let project_scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };

    let initial = facade
        .capture_observation(host_capture_request(
            project_scope.clone(),
            "host.provenance",
        ))
        .await
        .unwrap();
    // Host ingress commits the observation durably but leaves the
    // external-source projection queued, so the truthful outcome is
    // `AcceptedForReplay` carrying the underlying committed persist outcome
    // with a `Pending` projection state, never a fake immediate persist.
    let (initial_attachment, initial_generation) = match initial {
        CaptureObservationOutcome::AcceptedForReplay {
            projection_state,
            outcome,
            ..
        } if matches!(*outcome, ObservationPersistOutcome::Committed(_)) => {
            assert_eq!(projection_state, ExternalSourceProjectionStateV1::Pending);
            let ObservationPersistOutcome::Committed(receipt) = *outcome else {
                unreachable!("guard matched committed persist outcome");
            };
            (
                receipt.repository_provenance_attachment().clone(),
                receipt.projection_generation().clone(),
            )
        }
        other => {
            panic!("expected committed project observation accepted for replay, got {other:?}")
        }
    };
    let initial_provenance = initial_attachment.provenance().unwrap();
    assert_eq!(initial_provenance.capture().project_id(), Some(&project_id));
    assert_eq!(initial_provenance.generation_id(), &initial_generation);

    git(
        &repository_root,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/changed.git",
        ],
    );
    let replay = facade
        .capture_observation(host_capture_request(
            project_scope.clone(),
            "host.provenance",
        ))
        .await
        .unwrap();
    // The replay is an exact duplicate of the committed observation, and the
    // queued external-source projection is still pending, so it too arrives as
    // `AcceptedForReplay` around the underlying `ExactDuplicate` outcome.
    let replay_attachment = match replay {
        CaptureObservationOutcome::AcceptedForReplay {
            projection_state,
            outcome,
            ..
        } if matches!(*outcome, ObservationPersistOutcome::ExactDuplicate(_)) => {
            assert_eq!(projection_state, ExternalSourceProjectionStateV1::Pending);
            let ObservationPersistOutcome::ExactDuplicate(receipt) = *outcome else {
                unreachable!("guard matched exact duplicate persist outcome");
            };
            receipt.repository_provenance_attachment().clone()
        }
        other => panic!("expected exact duplicate replay accepted for replay, got {other:?}"),
    };
    assert_eq!(replay_attachment, initial_attachment);

    let mismatched = facade
        .capture(host_capture_request(
            ObservationScopeV1::Project {
                project_id: ProjectId::new("project.host-provenance-other").unwrap(),
            },
            "host.provenance.mismatched",
        ))
        .await;
    assert_eq!(mismatched.status, HostAdmissionStatus::Unavailable);
    assert_eq!(mismatched.reason_code, Some("project_authority_mismatch"));

    let profile_facade = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_profile(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            profile_registered.as_ref(),
        )
        .with_repository_provenance(provenance),
    );
    let profile_project = profile_facade
        .capture(host_capture_request(
            project_scope,
            "host.provenance.profile-project",
        ))
        .await;
    assert_eq!(profile_project.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        profile_project.reason_code,
        Some("project_authority_unbound")
    );
    let profile = profile_facade
        .capture_observation(host_capture_request(
            ObservationScopeV1::Profile,
            "host.provenance.profile",
        ))
        .await
        .unwrap();
    let profile_attachment = match profile {
        CaptureObservationOutcome::AcceptedForReplay {
            projection_state,
            outcome,
            ..
        } if matches!(*outcome, ObservationPersistOutcome::Committed(_)) => {
            assert_eq!(projection_state, ExternalSourceProjectionStateV1::Pending);
            let ObservationPersistOutcome::Committed(receipt) = *outcome else {
                unreachable!("guard matched committed persist outcome");
            };
            receipt.repository_provenance_attachment().clone()
        }
        other => {
            panic!("expected committed profile observation accepted for replay, got {other:?}")
        }
    };
    assert!(matches!(
        profile_attachment.availability(),
        EvidenceAvailabilityV1::Unavailable
    ));
    assert!(profile_attachment.anchor().is_none());

    let project_rows = observation_store(project_registered.as_ref())
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(project_rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_profile_runtime_is_required_and_mismatch_never_falls_back() {
    let background_cpu = super::host_admission_batch_test::background_cpu_for_host_admission_test();
    let temporary = TempDir::new().unwrap();
    let profile_root = temporary.path().join("profile");
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root).unwrap();
    let daemon_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "host-admission-authority-test",
    )
    .unwrap();
    let (registered, _profile_owner) = registered_database_for_test(
        &tracedecay_sessions::runtime::user_sessions_db_path(identity.profile_root()),
        &identity,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await;

    let unavailable = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::unavailable_for_profile(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Profile,
        "host.registered.missing",
    ))
    .await;
    assert_eq!(unavailable.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        unavailable.reason_code,
        Some("registered_authority_unavailable")
    );
    assert!(unavailable.retryable);
    assert!(
        observation_store(registered.as_ref())
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .is_empty(),
        "missing registered authority must not fall back to a direct write"
    );

    let authoritative = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_profile(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            registered.as_ref(),
        ),
    )
    .capture_observation(host_capture_request(
        ObservationScopeV1::Profile,
        "host.registered.committed",
    ))
    .await
    .unwrap();
    assert!(matches!(
        authoritative,
        CaptureObservationOutcome::AcceptedForReplay {
            projection_state: ExternalSourceProjectionStateV1::Pending,
            outcome,
            ..
        } if matches!(*outcome, ObservationPersistOutcome::Committed(_))
    ));

    let mismatch = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_profile(
            identity.brain_id().clone(),
            UserProfileId::new("profile.other").unwrap(),
            registered.as_ref(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Profile,
        "host.registered.mismatch",
    ))
    .await;
    assert_eq!(mismatch.status, HostAdmissionStatus::Unavailable);
    assert_eq!(mismatch.reason_code, Some("project_authority_mismatch"));
    assert!(!mismatch.retryable);
    assert_eq!(
        observation_store(registered.as_ref())
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        1,
        "mismatched registered authority must not fall back to a direct write"
    );

    drop(daemon_scope);
    let revoked = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_profile(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            registered.as_ref(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Profile,
        "host.registered.revoked",
    ))
    .await;
    assert_eq!(revoked.status, HostAdmissionStatus::Unavailable);
    assert_eq!(revoked.reason_code, Some("authority_write_failed"));
    let _inspection_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "host-admission-authority-test",
    )
    .unwrap();
    assert_eq!(
        observation_store(registered.as_ref())
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        1,
        "revoked daemon write authority must be rechecked by the runtime writer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_project_runtime_is_exact_and_revocation_never_falls_back() {
    let background_cpu = super::host_admission_batch_test::background_cpu_for_host_admission_test();
    let temporary = TempDir::new().unwrap();
    let profile_root = temporary.path().join("profile");
    let project_root = temporary.path().join("project");
    initialize_repository(&project_root);
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root).unwrap();
    let daemon_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "host-admission-project-authority-test",
    )
    .unwrap();
    let project_id = ProjectId::new("project.registered.exact").unwrap();
    enroll_project(&project_root, &project_id);
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        identity.profile_root(),
        project_id.as_str(),
    )
    .unwrap();
    let (registered, _project_owner) = registered_database_for_test(
        &layout.sessions_db_path,
        &identity,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await;
    let (profile_registered, _profile_owner) = registered_database_for_test(
        &tracedecay_sessions::runtime::user_sessions_db_path(identity.profile_root()),
        &identity,
        tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await;
    let unavailable = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::unavailable_for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        "host.project.registered.missing",
    ))
    .await;
    assert_eq!(unavailable.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        unavailable.reason_code,
        Some("registered_authority_unavailable")
    );
    assert!(unavailable.retryable);

    let other_project_id = ProjectId::new("project.registered.other").unwrap();
    let mismatch = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            other_project_id.clone(),
            registered.as_ref(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Project {
            project_id: other_project_id,
        },
        "host.project.registered.mismatch",
    ))
    .await;
    assert_eq!(mismatch.status, HostAdmissionStatus::Unavailable);
    assert_eq!(mismatch.reason_code, Some("project_authority_mismatch"));
    assert!(!mismatch.retryable);

    let wrong_shard = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
            profile_registered.as_ref(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        "host.project.registered.wrong-shard",
    ))
    .await;
    assert_eq!(wrong_shard.status, HostAdmissionStatus::Unavailable);
    assert_eq!(wrong_shard.reason_code, Some("project_authority_mismatch"));
    assert!(!wrong_shard.retryable);
    assert!(
        observation_store(registered.as_ref())
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .is_empty(),
        "missing or mismatched ProjectSessions authority must not use a path fallback"
    );

    let committed = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
            registered.as_ref(),
        ),
    )
    .capture_observation(host_capture_request(
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        "host.project.registered.committed",
    ))
    .await
    .unwrap();
    assert!(matches!(
        committed,
        CaptureObservationOutcome::AcceptedForReplay {
            projection_state: ExternalSourceProjectionStateV1::Pending,
            outcome,
            ..
        } if matches!(*outcome, ObservationPersistOutcome::Committed(_))
    ));

    drop(daemon_scope);
    let revoked = facade_with_background_cpu(
        &background_cpu,
        HostAdmissionAuthorities::for_project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
            registered.as_ref(),
        ),
    )
    .capture(host_capture_request(
        ObservationScopeV1::Project { project_id },
        "host.project.registered.revoked",
    ))
    .await;
    assert_eq!(revoked.status, HostAdmissionStatus::Unavailable);
    assert_eq!(revoked.reason_code, Some("authority_write_failed"));
    let _inspection_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        1,
        "host-admission-project-authority-test",
    )
    .unwrap();
    assert_eq!(
        observation_store(registered.as_ref())
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap()
            .len(),
        1,
        "revoked ProjectSessions authority must be rechecked at actor time"
    );
}

const GIT_FIXTURE_CONFIG: [&str; 10] = [
    "-c",
    "core.hooksPath=.git/no-hooks",
    "-c",
    "gc.auto=0",
    "-c",
    "user.name=TraceDecay Test",
    "-c",
    "user.email=tracedecay-test@example.com",
    "-c",
    "commit.gpgsign=false",
];

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(plain_host_path(root))
    .args(GIT_FIXTURE_CONFIG)
    .args(plain_git_args(args))
    .output()
    .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
}
