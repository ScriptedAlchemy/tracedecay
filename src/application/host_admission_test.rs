use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, EvidenceAvailabilityV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProviderId, RepositoryId, RetentionClass, SessionId, WorktreeId,
};
use tracedecay_store::ObservationReplayRequest;

use crate::privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};

use super::*;

fn initialize_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let output = Command::new(crate::git::git_program())
        .args(["init", "-q", "-b", "main"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn host_capture_request(scope: ObservationScopeV1, record_id: &str) -> CaptureObservationRequest {
    // Host admission sanitizes through the single provider-neutral
    // observation path (RecordSanitizerV1::observation_v1), so the fixture
    // must present a canonical observation envelope rather than a raw
    // provider frame.
    let encoded = serde_json::to_vec(&json!({ "text": "host provenance fixture" })).unwrap();
    let range = ObservationSourceRangeV1::new(0, u64::try_from(encoded.len()).unwrap()).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    let session_id = "session.host-provenance".to_owned();
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
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
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

#[test]
fn probe_distinguishes_unknown_provider_and_missing_authority() {
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::default());
    assert_eq!(
        facade.probe("other", HostAdmissionScope::Project).status,
        HostAdmissionStatus::Unknown
    );
    assert_eq!(
        facade.probe("claude", HostAdmissionScope::Project).status,
        HostAdmissionStatus::Unavailable
    );
}

#[test]
fn all_production_provider_ids_are_supported() {
    for provider in crate::sessions::SessionProvider::ALL
        .into_iter()
        .filter(|provider| provider.supports_host_admission())
    {
        assert!(
            supported_provider(provider.id()),
            "unsupported provider {}",
            provider.id()
        );
    }
    assert!(!supported_provider("roo"));
    assert!(!supported_provider("vibe"));
}

#[test]
fn replay_statuses_serialize_without_provider_content() {
    for (outcome, expected_status) in [
        (
            HostAdmissionOutcome::replay_completed(false, false),
            "accepted_for_replay",
        ),
        (
            HostAdmissionOutcome::replay_completed(false, true),
            "exact_duplicate",
        ),
        (
            HostAdmissionOutcome::replay_completed(true, false),
            "committed",
        ),
    ] {
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            serde_json::json!({
                "status": expected_status,
                "retryable": false,
            })
        );
    }
}

#[test]
fn quarantine_outcomes_serialize_as_static_payload_free_dispositions() {
    for outcome in [
        HostAdmissionOutcome::quarantine_full(),
        HostAdmissionOutcome::quarantine_corrupted(),
        HostAdmissionOutcome::quarantine_recovery_required(),
    ] {
        let rendered = serde_json::to_string(&outcome).unwrap();
        assert!(rendered.contains("spool_quarantine_"));
        assert!(!rendered.contains("provider-private-payload"));
        assert!(!matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ));
    }
}

#[test]
fn application_errors_map_to_bounded_static_outcomes() {
    assert_eq!(
        classify_error(&ObservationApplicationError::Cancelled),
        HostAdmissionOutcome::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        )
    );
    assert_eq!(
        classify_error(&ObservationApplicationError::Store(
            ObservationStoreError::Storage {
                operation: "write",
                source: Box::new(std::io::Error::other("provider content must not escape",)),
            },
        )),
        HostAdmissionOutcome::new(
            HostAdmissionStatus::Unavailable,
            true,
            Some("authority_write_failed"),
        )
    );
}

#[tokio::test]
async fn host_ingress_binds_provenance_to_authoritative_project_and_replays_stably() {
    let root = TempDir::new().unwrap();
    let repository_root = root.path().join("repository");
    initialize_repository(&repository_root);
    let project_db = GlobalDb::open_at(&root.path().join("project.db"))
        .await
        .unwrap();
    let profile_db = GlobalDb::open_at(&root.path().join("profile.db"))
        .await
        .unwrap();
    let project_id = ProjectId::new("project.host-provenance").unwrap();
    let provenance = RepositoryProvenanceAdmissionContext::new(
        repository_root.clone(),
        project_id.clone(),
        RepositoryId::new("repository.host-provenance").unwrap(),
        Some(WorktreeId::new("worktree.host-provenance").unwrap()),
        [0x51; 32],
    );
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(&project_db, project_id.clone())
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
    let (initial_attachment, initial_generation) = match initial {
        CaptureObservationOutcome::Persisted { outcome, .. }
            if matches!(*outcome, ObservationPersistOutcome::Committed(_)) =>
        {
            let ObservationPersistOutcome::Committed(receipt) = *outcome else {
                unreachable!("guard matched committed persist outcome");
            };
            (
                receipt.repository_provenance_attachment().clone(),
                receipt.projection_generation().clone(),
            )
        }
        other => panic!("expected committed project observation, got {other:?}"),
    };
    let initial_provenance = initial_attachment.provenance().unwrap();
    assert_eq!(initial_provenance.capture().project_id(), Some(&project_id));
    assert_eq!(initial_provenance.generation_id(), &initial_generation);

    let remote = Command::new(crate::git::git_program())
        .args([
            "remote",
            "add",
            "origin",
            "https://example.invalid/changed.git",
        ])
        .current_dir(&repository_root)
        .output()
        .unwrap();
    assert!(remote.status.success());
    let replay = facade
        .capture_observation(host_capture_request(
            project_scope.clone(),
            "host.provenance",
        ))
        .await
        .unwrap();
    let replay_attachment = match replay {
        CaptureObservationOutcome::Persisted { outcome, .. }
            if matches!(*outcome, ObservationPersistOutcome::ExactDuplicate(_)) =>
        {
            let ObservationPersistOutcome::ExactDuplicate(receipt) = *outcome else {
                unreachable!("guard matched exact duplicate persist outcome");
            };
            receipt.repository_provenance_attachment().clone()
        }
        other => panic!("expected exact duplicate replay, got {other:?}"),
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

    let profile_facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_profile(&profile_db).with_repository_provenance(provenance),
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
        CaptureObservationOutcome::Persisted { outcome, .. }
            if matches!(*outcome, ObservationPersistOutcome::Committed(_)) =>
        {
            let ObservationPersistOutcome::Committed(receipt) = *outcome else {
                unreachable!("guard matched committed persist outcome");
            };
            receipt.repository_provenance_attachment().clone()
        }
        other => panic!("expected committed profile observation, got {other:?}"),
    };
    assert!(matches!(
        profile_attachment.availability(),
        EvidenceAvailabilityV1::Unavailable
    ));
    assert!(profile_attachment.anchor().is_none());

    let project_rows = GlobalDbObservationStore::new(&project_db)
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(project_rows.len(), 1);
}
