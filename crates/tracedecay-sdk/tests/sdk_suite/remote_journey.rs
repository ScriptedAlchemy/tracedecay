use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::remote::capture::{RemoteCaptureSequenceV1, RemoteWriterAuthorityV1};
use tracedecay_contracts::remote::capture_protocol::RemoteCaptureRequestV1;
use tracedecay_contracts::remote::protocol::RemoteProtocolRequestV1;
use tracedecay_contracts::remote::recovery::PromotionConfirmationV1;
use tracedecay_contracts::{ApplicationProblem, RequestId};
use tracedecay_domain::{
    BrainId, BrainNodeId, ComponentVersion, DurableObservationV1, EnrollmentGrantV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProviderId, RemoteCapabilityV1, RemoteWriterFenceV1,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId,
};
use tracedecay_sdk::client::{Client, ConnectionMode};
use tracedecay_sdk::operations::{TypedOperation, WorkflowListDefinitions};
use tracedecay_sdk::remote_client::EnrolledRemoteClient;

use super::production_daemon::{
    Daemon, REMOTE_TLS_CERTIFICATE, REMOTE_TLS_PRIVATE_KEY, REMOTE_TLS_ROOT_CERTIFICATE,
    enrollment_request, now_micros, remote_admission, remote_grant, replay_policy,
    spawn_remote_daemon, stop_daemon,
};
use crate::isolated_profile::apply_isolated_profile_env as isolated;
use crate::{production_binary, run};

const GRANT_CREDENTIAL: [u8; 32] = *b"0123456789abcdef0123456789abcdef";
const ENROLLMENT_CREDENTIAL: [u8; 32] = *b"fedcba9876543210fedcba9876543210";

struct RemoteDaemonFixture {
    daemon: Daemon,
    project_id: String,
    brain_id: BrainId,
    local_base: String,
    local_token: String,
    remote: SocketAddr,
    _scratch: TempDir,
}

impl RemoteDaemonFixture {
    fn start(package: &str) -> Self {
        let scratch = TempDir::new().unwrap();
        let home = scratch.path().join("home");
        let profile = home.join(".tracedecay");
        let project = scratch.path().join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            format!("[package]\nname=\"{package}\"\nversion=\"0.0.0\"\nedition=\"2024\"\n"),
        )
        .unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "pub const FIXTURE: bool = true;\n",
        )
        .unwrap();
        run(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project));
        let certificate = scratch.path().join("localhost.crt.pem");
        let private_key = scratch.path().join("localhost.key.pem");
        fs::write(&certificate, REMOTE_TLS_CERTIFICATE).unwrap();
        fs::write(&private_key, REMOTE_TLS_PRIVATE_KEY).unwrap();
        fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();

        let binary = production_binary();
        let (daemon, authority, remote) = spawn_remote_daemon(
            &binary,
            &home,
            &profile,
            &project,
            &certificate,
            &private_key,
        );
        let mut init = Command::new(&binary);
        init.arg("init").current_dir(&project);
        isolated(&mut init, &home, &profile);
        run(&mut init);
        let mut context_command = Command::new(&binary);
        context_command
            .args(["projects", "context"])
            .arg(&project)
            .arg("--json")
            .current_dir(&project);
        isolated(&mut context_command, &home, &profile);
        let context: Value = serde_json::from_slice(&run(&mut context_command)).unwrap();
        let project_id = context["project"]["project_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let local_base = format!(
            "http://{}",
            authority["http_application_endpoint"].as_str().unwrap()
        );
        let local_token = authority["auth_token"].as_str().unwrap().to_owned();
        Client::builder(ConnectionMode::local(
            &local_base,
            &project_id,
            &local_token,
        ))
        .build()
        .unwrap()
        .execute::<WorkflowListDefinitions>(
            &serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!(
                {}
            ))
            .unwrap(),
        )
        .expect("the daemon serves the project before any remote node exists");
        Self {
            daemon,
            brain_id: BrainId::new(authority["brain_id"].as_str().unwrap()).unwrap(),
            project_id,
            local_base,
            local_token,
            remote,
            _scratch: scratch,
        }
    }

    fn local_post(&self, path: &str, body: &Value) -> (reqwest::StatusCode, String) {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}{path}", self.local_base))
            .bearer_auth(&self.local_token)
            .header(reqwest::header::ORIGIN, &self.local_base)
            .json(body)
            .send()
            .unwrap();
        let status = response.status();
        (status, response.text().unwrap())
    }

    fn status_authority(&self) -> Value {
        let status: Value = reqwest::blocking::Client::new()
            .get(format!("{}/remote-status", self.local_base))
            .bearer_auth(&self.local_token)
            .header(reqwest::header::ORIGIN, &self.local_base)
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(status["kind"], "observed", "{status}");
        status["status"]["authority"].clone()
    }

    fn client(&self, credential: [u8; 32]) -> EnrolledRemoteClient {
        EnrolledRemoteClient::new_with_root_certificate(
            format!("https://{}/remote/", self.remote),
            credential,
            Duration::from_secs(30),
            REMOTE_TLS_ROOT_CERTIFICATE,
        )
        .unwrap()
    }

    fn provision_and_enroll(&self, grant: &EnrollmentGrantV1) {
        let (status, body) = self.local_post(
            "/remote-nodes/provision",
            &json!({"grant": grant, "admission": remote_admission(grant)}),
        );
        assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{body}");
        self.client(GRANT_CREDENTIAL)
            .enroll(&enrollment_request(grant), ENROLLMENT_CREDENTIAL)
            .unwrap()
            .result
            .expect("the provisioned node enrolls");
    }

    fn publish_writer(&self, node_id: &BrainNodeId, writer: &RemoteWriterAuthorityV1) {
        let (status, body) = self.local_post(
            "/remote-nodes/writer-authority",
            &json!({
                "node_id": node_id,
                "writer": writer,
                "replay_policy": replay_policy(&writer.scope),
            }),
        );
        assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{body}");
    }
}

fn authority_without_observed_at(mut authority: Value) -> Value {
    if authority["state"] != "available" {
        authority["value"]
            .as_object_mut()
            .expect("an unavailable authority carries its reason")
            .remove("observed_at");
    }
    authority
}

fn writer_fence(brain_id: &BrainId, epoch: u64, node: &str) -> RemoteWriterFenceV1 {
    serde_json::from_value(json!({
        "brain_id": brain_id.as_str(),
        "shard_id": "shard.remote-sdk-production",
        "generation_id": "generation.remote-sdk-production",
        "placement_revision": 1,
        "authority_epoch": epoch,
        "authority_node_id": node,
    }))
    .unwrap()
}

fn observation(project_id: &str, observation_id: &str) -> DurableObservationV1 {
    let payload = json!({"kind": "assistant_message", "body": "remote journey observation"});
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.remote-journey").unwrap(),
            ComponentVersion::new("sanitizer.remote-journey.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.remote-journey").unwrap(),
                SessionId::new("session.remote-journey").unwrap(),
            )
            .unwrap(),
            ObservationScopeV1::Project {
                project_id: ProjectId::new(project_id).unwrap(),
            },
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new(observation_id).unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.remote-journey").unwrap(),
        payload,
    )
    .unwrap()
}

fn capture_request(
    request_id: &str,
    node_id: &BrainNodeId,
    writer: &RemoteWriterAuthorityV1,
    sequence: RemoteCaptureSequenceV1,
    observation: DurableObservationV1,
) -> RemoteProtocolRequestV1<RemoteCaptureRequestV1> {
    RemoteProtocolRequestV1::new(
        RequestId::new(request_id).unwrap(),
        writer.authority.fence.brain_id.clone(),
        node_id.clone(),
        1,
        None,
        now_micros(),
        RemoteCaptureRequestV1 {
            writer: writer.clone(),
            policy_revision: 1,
            sequence,
            observation,
        },
    )
    .unwrap()
}

fn failover_request(
    node_id: &BrainNodeId,
    previous: &RemoteWriterFenceV1,
) -> RemoteProtocolRequestV1<PromotionConfirmationV1> {
    let sent_at = now_micros();
    RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-journey-failover").unwrap(),
        previous.brain_id.clone(),
        node_id.clone(),
        1,
        Some(previous.clone()),
        sent_at,
        PromotionConfirmationV1 {
            preview_id: "promotion.remote-journey-failover".to_owned(),
            expected_authority_epoch: previous.authority_epoch.0,
            expected_placement_revision: 1,
            expires_at_micros: sent_at.0.saturating_add(60_000_000),
        },
    )
    .unwrap()
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn protocol_envelopes_report_the_status_authority_through_publication_and_failover() {
    let fixture = RemoteDaemonFixture::start("remote-authority-truth-fixture");
    let node_id = BrainNodeId::new("node.remote-sdk-production").unwrap();
    let mut grant = remote_grant(
        fixture.brain_id.clone(),
        node_id.clone(),
        &fixture.project_id,
        &GRANT_CREDENTIAL,
    );
    grant.capabilities = [
        RemoteCapabilityV1::CaptureOffline,
        RemoteCapabilityV1::Promote,
    ]
    .into_iter()
    .collect();
    fixture.provision_and_enroll(&grant);
    let node = fixture.client(ENROLLMENT_CREDENTIAL);
    let previous = writer_fence(&fixture.brain_id, 1, "node.remote-sdk-previous-writer");
    let writer: RemoteWriterAuthorityV1 = serde_json::from_value(json!({
        "project_id": fixture.project_id,
        "scope": grant.scope,
        "authority": {"fence": previous, "credential_revision": 1, "observed_at": now_micros()},
    }))
    .unwrap();
    let capture_authority = |request_id: &str| {
        let response = node
            .capture(&capture_request(
                request_id,
                &node_id,
                &writer,
                RemoteCaptureSequenceV1 {
                    sequence: 1,
                    previous_event_id: None,
                },
                observation(&fixture.project_id, "observation.remote-journey"),
            ))
            .expect("capture is a canonical protocol response");
        assert!(
            response.result.is_err(),
            "capture is refused while the writer is unpublished or reachable"
        );
        serde_json::to_value(&response.authority).unwrap()
    };

    let unpublished = fixture.status_authority();
    assert_eq!(
        (&unpublished["state"], &unpublished["value"]["reason"]),
        (
            &json!("unavailable"),
            &json!("writer_authority_unpublished")
        ),
        "{unpublished}"
    );
    assert_eq!(
        authority_without_observed_at(capture_authority(
            "request.remote-journey-capture-unpublished"
        )),
        authority_without_observed_at(unpublished.clone()),
        "capture before publication reports the status authority"
    );
    let refused_failover = node
        .failover(&failover_request(&node_id, &previous))
        .unwrap();
    assert!(refused_failover.result.is_err());
    assert_eq!(
        authority_without_observed_at(serde_json::to_value(&refused_failover.authority).unwrap()),
        authority_without_observed_at(unpublished),
        "failover before publication reports the status authority"
    );

    fixture.publish_writer(&node_id, &writer);
    let published = fixture.status_authority();
    assert_eq!(
        published,
        json!({"state": "available", "value": writer.authority}),
        "/remote-status reports the published writer"
    );
    let reachable = node
        .capture(&capture_request(
            "request.remote-journey-capture-published",
            &node_id,
            &writer,
            RemoteCaptureSequenceV1 {
                sequence: 1,
                previous_event_id: None,
            },
            observation(&fixture.project_id, "observation.remote-journey"),
        ))
        .unwrap();
    let Err(problem) = &reachable.result else {
        panic!("capture is refused while the published writer is reachable");
    };
    assert!(
        matches!(
            problem.problem.source(),
            ApplicationProblem::Conflict { diagnostic, .. }
                if diagnostic.code == "remote.authority_reachable"
        ),
        "{problem:?}"
    );
    assert_eq!(
        serde_json::to_value(&reachable.authority).unwrap(),
        published,
        "the reachable refusal reports the published writer it read"
    );

    let promoted = node
        .failover(&failover_request(&node_id, &previous))
        .unwrap();
    assert!(promoted.result.is_ok(), "the enrolled node is promoted");
    let after_failover = fixture.status_authority();
    assert_eq!(
        (
            &after_failover["value"]["fence"]["authority_node_id"],
            &after_failover["value"]["fence"]["authority_epoch"],
        ),
        (&json!(node_id.as_str()), &json!(2)),
        "{after_failover}"
    );
    assert_eq!(
        serde_json::to_value(&promoted.authority).unwrap(),
        after_failover,
        "failover reports the promoted authority /remote-status reads"
    );
    assert_eq!(
        capture_authority("request.remote-journey-capture-promoted"),
        after_failover,
        "capture after failover reports the promoted authority"
    );

    let mut daemon = fixture.daemon;
    stop_daemon(&mut daemon);
}
