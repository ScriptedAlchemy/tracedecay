#![allow(clippy::unwrap_used)]

use serde_json::json;
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::{ObservationReplayRequest, StoredObservation};
use tracedecay_usecases::host_admission::{HostAdmissionScope, HostAdmissionStatus};
use tracedecay_usecases::observation::{CaptureObservationRequest, ObservationCancellation};

const PROJECT_ID: &str = "project.cross-host-handoff";
const PROJECT_PATH: &str = "repo://cross-host-handoff";
const WORKTREE_PATH: &str = "worktree://feature/pr6";
const LOCATION_PROVENANCE: &str = "host-native-cwd";
const SECRET: &str = "sk-proj-cross-host-canary-1234567890";

#[tokio::test]
async fn codex_to_claude_handoff_preserves_identity_lineage_privacy_and_provenance() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project_root)
            .status()
            .unwrap()
            .success()
    );
    let project_id = ProjectId::new(PROJECT_ID).unwrap();
    assert!(
        tracedecay::storage::write_repository_identity_marker(&project_root, project_id.as_str())
            .unwrap()
    );
    let runtime =
        HostAdmissionTestRuntimeV1::project(&profile_root, &project_root, project_id.clone())
            .await
            .unwrap();
    let facade = runtime.facade();

    let parent = facade
        .capture(handoff_request(
            "codex",
            "session.handoff.parent",
            "codex.handoff.parent",
            "agent.handoff.parent",
            None,
        ))
        .await;
    assert_eq!(parent.status, HostAdmissionStatus::AcceptedForReplay);

    let child = facade
        .capture(handoff_request(
            "claude",
            "session.handoff.child",
            "claude.handoff.child",
            "agent.handoff.child",
            Some((
                "session.handoff.parent",
                "codex.handoff.parent",
                "agent.handoff.parent",
            )),
        ))
        .await;
    assert_eq!(child.status, HostAdmissionStatus::AcceptedForReplay);

    drop(facade);

    let rows = runtime
        .replay_observations(
            HostAdmissionScope::Project,
            ObservationReplayRequest::new(0, 10).unwrap(),
        )
        .await
        .unwrap();
    let profile_rows = runtime
        .replay_observations(
            HostAdmissionScope::Profile,
            ObservationReplayRequest::new(0, 10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        profile_rows.is_empty(),
        "project handoff observations must not fall back to the profile store"
    );
    assert!(
        !format!("{rows:?}").contains(SECRET),
        "private handoff input must not survive authoritative capture"
    );

    let codex = row_for_provider(&rows, "codex");
    let claude = row_for_provider(&rows, "claude");
    assert_handoff_identity(
        codex,
        "codex",
        "session.handoff.parent",
        "codex.handoff.parent",
        "agent.handoff.parent",
        1,
    );
    assert_handoff_identity(
        claude,
        "claude",
        "session.handoff.child",
        "claude.handoff.child",
        "agent.handoff.child",
        2,
    );

    let child_envelope = envelope(claude);
    assert_eq!(
        child_envelope.relations().parent_session_id(),
        Some(&SessionId::new("session.handoff.parent").unwrap())
    );
    assert_eq!(
        child_envelope.relations().parent_agent_id(),
        Some(&ObservationId::new("agent.handoff.parent").unwrap())
    );
    assert_eq!(
        serde_json::to_value(child_envelope).unwrap()["relations"]["parent_message_id"],
        "codex.handoff.parent"
    );
}

fn handoff_request(
    provider: &str,
    session_id: &str,
    record_id: &str,
    agent_id: &str,
    parent: Option<(&str, &str, &str)>,
) -> CaptureObservationRequest {
    let provider_id = ProviderId::new(provider).unwrap();
    let canonical_session_id = SessionId::new(session_id).unwrap();
    let record_id = ObservationId::new(record_id).unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(canonical_session_id.clone())
        .with_message_id(record_id.clone())
        .with_agent_id(ObservationId::new(agent_id).unwrap());
    if let Some((parent_session_id, parent_message_id, parent_agent_id)) = parent {
        relations = relations
            .with_parent_session_id(SessionId::new(parent_session_id).unwrap())
            .with_parent_message_id(ObservationId::new(parent_message_id).unwrap())
            .with_parent_agent_id(ObservationId::new(parent_agent_id).unwrap());
    }

    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::DaemonSequence;
    let native_sequence = if provider == "codex" { 1 } else { 2 };
    let encoded = serde_json::to_vec(&json!({
        "text": "safe cross-host handoff body",
        "api_key": SECRET,
    }))
    .unwrap();
    let envelope_provider = provider_id.clone();
    let envelope_record_id = record_id.clone();
    let native_source = provider.to_string();
    let parsed =
        parse_normalized_observation_record_v1(&encoded, range, ordering_domain, move |native| {
            CanonicalObservationEnvelopeV1::new(
                envelope_provider,
                "cross_host_handoff",
                envelope_record_id,
                relations,
                vec![
                    CanonicalObservationFactV1::Session {
                        project_path: Some(PROJECT_PATH.to_string()),
                        location_path: Some(WORKTREE_PATH.to_string()),
                        transcript_path: None,
                        title: Some("Cross-host handoff".to_string()),
                        started_at: None,
                        ended_at: None,
                        source: Some("native-host-event".to_string()),
                        native_source: Some(native_source),
                        profile: None,
                        location_provenance: Some(LOCATION_PROVENANCE.to_string()),
                    },
                    CanonicalObservationFactV1::Message {
                        role: CanonicalMessageRoleV1::Assistant,
                        content: native,
                        model: None,
                        timestamp: None,
                    },
                ],
                CanonicalObservationEvidenceV1::new(ordering_domain, range)
                    .with_native_sequence(native_sequence),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider_id, canonical_session_id).unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Project {
                project_id: ProjectId::new(PROJECT_ID).unwrap(),
            },
            ObservationSourceGenerationV1::new(17).unwrap(),
            range,
            ordering_domain,
            record_id,
        )
        .unwrap(),
        None,
        RetentionClass::new("retention.cross-host-handoff-test").unwrap(),
        ObservationCancellation::default(),
    )
    .unwrap()
}

fn row_for_provider<'a>(rows: &'a [StoredObservation], provider: &str) -> &'a StoredObservation {
    rows.iter()
        .find(|row| row.observation().source().provider().as_str() == provider)
        .unwrap_or_else(|| panic!("missing {provider} handoff row"))
}

fn envelope(row: &StoredObservation) -> CanonicalObservationEnvelopeV1 {
    serde_json::from_value(row.observation().payload().clone()).unwrap()
}

fn assert_handoff_identity(
    row: &StoredObservation,
    provider: &str,
    session_id: &str,
    record_id: &str,
    agent_id: &str,
    native_sequence: u64,
) {
    let observation = row.observation();
    let identity = observation.identity();
    assert_eq!(identity.source().provider().as_str(), provider);
    assert_eq!(identity.source().session_id().as_str(), session_id);
    assert_eq!(
        identity.scope(),
        &ObservationScopeV1::Project {
            project_id: ProjectId::new(PROJECT_ID).unwrap(),
        }
    );
    assert_eq!(
        identity.generation(),
        ObservationSourceGenerationV1::new(17).unwrap()
    );
    assert_eq!(
        identity.position(),
        ObservationSourceRangeV1::new(0, 1).unwrap()
    );
    assert_eq!(
        identity.ordering_domain(),
        ObservationOrderingDomainV1::DaemonSequence
    );
    assert_eq!(
        identity.native_record_id(),
        Some(&ObservationId::new(record_id).unwrap())
    );

    let envelope = envelope(row);
    assert_eq!(envelope.provider().as_str(), provider);
    assert_eq!(envelope.relations().session_id().as_str(), session_id);
    assert_eq!(
        envelope.relations().message_id(),
        Some(&ObservationId::new(record_id).unwrap())
    );
    assert_eq!(
        envelope.relations().agent_id(),
        Some(&ObservationId::new(agent_id).unwrap())
    );
    assert_eq!(
        envelope.evidence().ordering_domain(),
        ObservationOrderingDomainV1::DaemonSequence
    );
    assert_eq!(
        envelope.evidence().range(),
        ObservationSourceRangeV1::new(0, 1).unwrap()
    );
    assert_eq!(envelope.evidence().native_sequence(), Some(native_sequence));

    let session = envelope
        .facts()
        .iter()
        .find_map(|fact| match fact {
            CanonicalObservationFactV1::Session {
                project_path,
                location_path,
                location_provenance,
                ..
            } => Some((project_path, location_path, location_provenance)),
            _ => None,
        })
        .expect("session handoff fact");
    assert_eq!(session.0.as_deref(), Some(PROJECT_PATH));
    assert_eq!(session.1.as_deref(), Some(WORKTREE_PATH));
    assert_eq!(session.2.as_deref(), Some(LOCATION_PROVENANCE));
}
