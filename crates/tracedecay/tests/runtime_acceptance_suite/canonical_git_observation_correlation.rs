use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_capture::codex::{
    CodexObservationLocation, codex_native_record_id, normalize_codex_observation_with_location,
};
use tracedecay_domain::{
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_global_db::GlobalDbGitCorrelationStore;
use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay_privacy::parse_normalized_observation_record_v1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_sessions::runtime::git_correlation::{
    CommitRelationFilter, GitRefFilter, SessionsForQuery, pending_git_evidence_publication_count,
};

fn run_git(project: &Path, args: &[&str]) -> String {
    let output = Command::new(crate::common::git_program())
        .args(args)
        .current_dir(project)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn canonical_codex_capture_publishes_admitted_git_evidence_for_sessions_for() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("source-4f73c2b16d6d47ddb4b99c394a35d3a8");
    std::fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init", "-b", "capture-branch"]);
    run_git(&project, &["config", "user.email", "test@test.invalid"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    std::fs::write(project.join("actual.txt"), "canonical source metadata\n").unwrap();
    run_git(&project, &["add", "actual.txt"]);
    run_git(&project, &["commit", "-m", "canonical source fixture"]);
    let commit_sha = run_git(&project, &["rev-parse", "HEAD"]);

    let project_id = ProjectId::new("project.canonical-git-capture").unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile"),
        &project,
        project_id.clone(),
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let marker = tracedecay_runtime_core::storage::read_repository_identity_marker(&project)
        .unwrap()
        .unwrap();
    let provenance = RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
        &project,
        &project_id,
        &marker,
    )
    .unwrap();
    let shard = &database.binding().shard_id;
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            project_id.clone(),
            database,
        )
        .with_repository_provenance(provenance)
        .with_background_cpu(runtime.background_cpu()),
    );

    let session_id = SessionId::new("019fc87d-2aca-7ef3-9236-ea4a42e0fa61").unwrap();
    let native = json!({
        "timestamp": "2026-08-03T16:37:54Z",
        "type": "session_meta",
        "payload": {
            "id": session_id.as_str(),
            "cwd": project.to_string_lossy(),
            "git": {
                "branch": "capture-branch",
                "commit_hash": commit_sha.clone(),
            }
        }
    });
    let encoded = serde_json::to_vec(&native).unwrap();
    let range = ObservationSourceRangeV1::new(0, encoded.len() as u64).unwrap();
    let record_id = codex_native_record_id(session_id.as_str(), &native).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::FileBytes,
        {
            let session_id = session_id.clone();
            let record_id = record_id.clone();
            let project = project.clone();
            move |value| {
                normalize_codex_observation_with_location(
                    &value,
                    session_id.as_str(),
                    Some(session_id.as_str()),
                    record_id.clone(),
                    range,
                    CodexObservationLocation {
                        project_path: Some(&project),
                        location_path: Some(&project),
                    },
                )
            }
        },
    )
    .unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        session_id.clone(),
    )
    .unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let request = CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope.clone(),
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::FileBytes,
            record_id,
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.canonical-git-capture").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap();
    let outcome = facade.capture_observation(request).await.unwrap();
    let sanitized_payload = match &outcome {
        CaptureObservationOutcome::Persisted {
            sanitized_record, ..
        }
        | CaptureObservationOutcome::AcceptedForReplay {
            sanitized_record, ..
        } => sanitized_record.payload(),
        other => panic!("canonical capture was not retained: {other:?}"),
    };
    let sanitized = serde_json::to_string(sanitized_payload).unwrap();
    assert!(!sanitized.contains(project.to_string_lossy().as_ref()));
    assert!(sanitized.contains("capture-branch"));
    assert!(sanitized.contains(&commit_sha));

    let store = GlobalDbGitCorrelationStore::new(database);
    assert_eq!(
        pending_git_evidence_publication_count(database)
            .await
            .unwrap(),
        1
    );
    assert!(
        store
            .sessions_for_with_relation(
                &SessionsForQuery {
                    git_ref: GitRefFilter::Branch("capture-branch".to_owned()),
                    since: None,
                    until: None,
                    limit: 10,
                },
                CommitRelationFilter::All,
            )
            .await
            .unwrap()
            .is_empty(),
        "capture stages evidence without publishing the Git graph inline"
    );
    facade
        .drain_projection_queue("codex", &scope, &ObservationCancellation::default(), 1)
        .await
        .unwrap();
    assert_eq!(
        pending_git_evidence_publication_count(database)
            .await
            .unwrap(),
        0
    );
    let branch_hits = store
        .sessions_for_with_relation(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("capture-branch".to_owned()),
                since: None,
                until: None,
                limit: 10,
            },
            CommitRelationFilter::All,
        )
        .await
        .unwrap();
    assert_eq!(branch_hits.len(), 1);
    assert_eq!(branch_hits[0].session_id, session_id.as_str());
    assert_eq!(branch_hits[0].worktree.as_deref(), project.to_str());

    let commit_hits = store
        .sessions_for_with_relation(
            &SessionsForQuery {
                git_ref: GitRefFilter::Commit(commit_sha.clone()),
                since: None,
                until: None,
                limit: 10,
            },
            CommitRelationFilter::All,
        )
        .await
        .unwrap();
    assert_eq!(commit_hits.len(), 1);
    assert_eq!(
        commit_hits[0].commit_sha.as_deref(),
        Some(commit_sha.as_str())
    );
}
