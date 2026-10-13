use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::remote::capture::{RemoteCaptureSequenceV1, RemoteWriterAuthorityV1};
use tracedecay_contracts::remote::capture_protocol::RemoteCaptureRequestV1;
use tracedecay_contracts::remote::composition::ExpectedRemoteShardV1;
use tracedecay_contracts::remote::protocol::{CurrentAuthorityRequestV1, RemoteProtocolRequestV1};
use tracedecay_contracts::remote::query::{
    RemoteExactObservationResultV1, RemoteQueryOperationV1, RemoteQueryRequestV1,
};
use tracedecay_contracts::remote::recovery::PromotionConfirmationV1;
use tracedecay_contracts::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_contracts::{ApplicationProblem, RequestId};
use tracedecay_domain::{
    BrainId, BrainNodeId, ComponentVersion, DurableObservationV1, EnrollmentGrantV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProviderId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId,
};
use tracedecay_sdk::client::{Client, ClientError, ConnectionMode};
use tracedecay_sdk::operations::{TypedOperation, WorkflowListDefinitions};
use tracedecay_sdk::remote_client::EnrolledRemoteClient;

use super::production_daemon::{
    Daemon, REMOTE_TLS_CERTIFICATE, REMOTE_TLS_PRIVATE_KEY, REMOTE_TLS_ROOT_CERTIFICATE,
    enrollment_request, now_micros, query_policy, remote_admission, remote_grant, replay_policy,
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
    scratch: TempDir,
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
        let fixture = Self {
            daemon,
            brain_id: BrainId::new(authority["brain_id"].as_str().unwrap()).unwrap(),
            project_id,
            local_base: format!(
                "http://{}",
                authority["http_application_endpoint"].as_str().unwrap()
            ),
            local_token: authority["auth_token"].as_str().unwrap().to_owned(),
            remote,
            scratch,
        };
        fixture.wait_until_project_serves();
        fixture
    }

    /// The daemon mounts the project in the background and answers the typed
    /// retryable `runtime_mounting` problem until it serves.
    fn wait_until_project_serves(&self) {
        let client = Client::builder(ConnectionMode::local(
            &self.local_base,
            &self.project_id,
            &self.local_token,
        ))
        .build()
        .unwrap();
        let list = serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(
            json!({}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match client.execute::<WorkflowListDefinitions>(&list) {
                Err(ClientError::Problem(problem))
                    if problem.code == tracedecay_contracts::RUNTIME_MOUNTING_REASON_CODE
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                outcome => {
                    outcome.expect("the daemon serves the project");
                    break;
                }
            }
        }
    }

    fn home(&self) -> PathBuf {
        self.scratch.path().join("home")
    }

    fn profile(&self) -> PathBuf {
        self.home().join(".tracedecay")
    }

    fn project(&self) -> PathBuf {
        self.scratch.path().join("project")
    }

    /// Stops this daemon and starts a new one on the same profile. The new
    /// process gets fresh local and remote listeners.
    fn restart(&mut self) {
        stop_daemon(&mut self.daemon);
        fs::remove_file(self.profile().join("daemon-authority.json")).ok();
        let (daemon, authority, remote) = spawn_remote_daemon(
            &production_binary(),
            &self.home(),
            &self.profile(),
            &self.project(),
            &self.scratch.path().join("localhost.crt.pem"),
            &self.scratch.path().join("localhost.key.pem"),
        );
        self.daemon = daemon;
        assert_eq!(
            authority["brain_id"].as_str(),
            Some(self.brain_id.as_str()),
            "a restart keeps the profile's Brain"
        );
        self.local_base = format!(
            "http://{}",
            authority["http_application_endpoint"].as_str().unwrap()
        );
        self.local_token = authority["auth_token"].as_str().unwrap().to_owned();
        self.remote = remote;
        self.wait_until_project_serves();
    }

    /// Runs `tracedecay remote revoke` against this daemon, as its operator.
    fn revoke(&self, node_id: &BrainNodeId) -> Value {
        let mut command = Command::new(production_binary());
        command
            .args(["remote", "revoke", "--json", "--node-id", node_id.as_str()])
            .current_dir(self.project());
        isolated(&mut command, &self.home(), &self.profile());
        serde_json::from_slice(&run(&mut command)).unwrap()
    }

    fn remote_endpoint(&self) -> String {
        format!("https://{}/remote/", self.remote)
    }

    /// Runs the node operator `operation` (`join`, `drain`) on this daemon, as
    /// `node_id` of the Brain whose authority serves `endpoint`.
    fn node_operation(
        &self,
        operation: &str,
        endpoint: &str,
        brain_id: &BrainId,
        node_id: &BrainNodeId,
        scope: &RemoteRepositoryScopeV1,
    ) -> (reqwest::StatusCode, Value) {
        let discovery = RemoteProtocolRequestV1::new(
            RequestId::new("request.remote-journey-join").unwrap(),
            brain_id.clone(),
            node_id.clone(),
            1,
            None,
            now_micros(),
            CurrentAuthorityRequestV1 {
                scope: scope.clone(),
            },
        )
        .unwrap();
        let response = reqwest::blocking::Client::new()
            .post(format!("{}/remote-nodes/{operation}", self.local_base))
            .bearer_auth(&self.local_token)
            .header(reqwest::header::ORIGIN, &self.local_base)
            .header(
                REMOTE_ENROLLMENT_CREDENTIAL_HEADER,
                &ENROLLMENT_CREDENTIAL[..],
            )
            .json(&json!({
                "endpoint": endpoint,
                "trust_root_pem": std::str::from_utf8(REMOTE_TLS_ROOT_CERTIFICATE).unwrap(),
                "timeout_secs": 5,
                "discovery": discovery,
            }))
            .send()
            .unwrap();
        (response.status(), response.json().unwrap())
    }

    /// The node client served on this daemon's own loopback surface, where
    /// a joined node captures.
    fn local_client(&self) -> EnrolledRemoteClient {
        EnrolledRemoteClient::new_local_daemon(
            format!("{}/remote/", self.local_base),
            ENROLLMENT_CREDENTIAL,
            Duration::from_secs(30),
        )
        .unwrap()
    }

    fn status(&self) -> Value {
        let status: Value = reqwest::blocking::Client::new()
            .get(format!("{}/remote-status", self.local_base))
            .bearer_auth(&self.local_token)
            .header(reqwest::header::ORIGIN, &self.local_base)
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(status["kind"], "observed", "{status}");
        status["status"].clone()
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
        self.status()["authority"].clone()
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
                "query_policy": query_policy(&writer.scope),
            }),
        );
        assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{body}");
    }
}

const REMOTE_ENROLLMENT_CREDENTIAL_HEADER: &str = "x-tracedecay-enrollment-credential";

/// An authority endpoint on a loopback port nothing listens on, as a node
/// cut off from its authority sees it.
fn unreachable_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("https://{}/remote/", listener.local_addr().unwrap())
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
    observation_at(project_id, observation_id, 0)
}

/// An observation of the one journey source session at byte `start`. The
/// project store advances that source's cursor on each commit, so every later
/// observation of the session must start where the previous one ended.
fn observation_at(project_id: &str, observation_id: &str, start: u64) -> DurableObservationV1 {
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
            ObservationSourceRangeV1::new(start, start + 1).unwrap(),
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
        RemoteCapabilityV1::ServeAuthority,
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

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn a_joined_node_captures_offline_only_while_its_authority_is_unreachable() {
    let desktop = RemoteDaemonFixture::start("remote-offline-desktop");
    let laptop = RemoteDaemonFixture::start("remote-offline-laptop");
    let node_id = BrainNodeId::new("node.remote-offline-laptop").unwrap();
    let mut grant = remote_grant(
        desktop.brain_id.clone(),
        node_id.clone(),
        &desktop.project_id,
        &GRANT_CREDENTIAL,
    );
    grant.capabilities = [
        RemoteCapabilityV1::CaptureOffline,
        RemoteCapabilityV1::DiscoverAuthority,
    ]
    .into_iter()
    .collect();
    desktop.provision_and_enroll(&grant);
    let writer: RemoteWriterAuthorityV1 = serde_json::from_value(json!({
        "project_id": desktop.project_id,
        "scope": grant.scope,
        "authority": {
            "fence": writer_fence(&desktop.brain_id, 1, node_id.as_str()),
            "credential_revision": 1,
            "observed_at": now_micros(),
        },
    }))
    .unwrap();
    desktop.publish_writer(&node_id, &writer);
    let join = |endpoint: &str| {
        laptop.node_operation("join", endpoint, &desktop.brain_id, &node_id, &grant.scope)
    };
    let (status, joined) = join(&desktop.remote_endpoint());
    assert_eq!(status, reqwest::StatusCode::OK, "{joined}");
    let project_id = desktop.project_id.clone();
    let capture = |request_id: &str, writer: &RemoteWriterAuthorityV1, sequence| {
        laptop
            .local_client()
            .capture(&capture_request(
                request_id,
                &node_id,
                writer,
                sequence,
                observation(&project_id, "observation.remote-offline"),
            ))
            .expect("the laptop answers the node credential with a protocol response")
    };
    let first = RemoteCaptureSequenceV1 {
        sequence: 1,
        previous_event_id: None,
    };
    let online = capture("request.remote-offline-online", &writer, first.clone());
    assert!(
        matches!(
            &online.result,
            Err(problem) if matches!(
                problem.problem.source(),
                ApplicationProblem::Conflict { diagnostic, .. }
                    if diagnostic.code == "remote.authority_reachable"
            )
        ),
        "capture is refused while the authority answers: {:?}",
        online.result
    );

    let (status, refused) = join(&unreachable_endpoint());
    assert_eq!(
        (status, refused),
        (
            reqwest::StatusCode::BAD_GATEWAY,
            json!({"code": "authority_unreachable"})
        )
    );
    let offline_status = laptop.status();
    assert_eq!(
        (
            &offline_status["authority"]["state"],
            &offline_status["authority"]["value"]["reason"],
        ),
        (&json!("unavailable"), &json!("authority_unreachable")),
        "{offline_status}"
    );

    let mut moved_writer = writer.clone();
    moved_writer.authority.fence = writer_fence(&desktop.brain_id, 2, node_id.as_str());
    let unfenced = capture(
        "request.remote-offline-unfenced",
        &moved_writer,
        first.clone(),
    );
    assert!(
        unfenced.result.is_err(),
        "offline capture runs only under the writer the authority last published"
    );
    let offline = capture("request.remote-offline-captured", &writer, first);
    let receipt = offline
        .result
        .expect("offline capture is admitted while the authority is unreachable")
        .outcome
        .payload()
        .cloned()
        .expect("an admitted capture returns its receipt");
    assert_eq!(receipt.sequence, 1);
    assert_eq!(
        serde_json::to_value(&offline.authority).unwrap()["state"],
        json!("unavailable")
    );
    assert_eq!(
        laptop.status()["spool"]["pending_count"],
        json!(1),
        "the frame waits in the laptop's spool"
    );

    let (status, rejoined) = join(&desktop.remote_endpoint());
    assert_eq!(status, reqwest::StatusCode::OK, "{rejoined}");
    assert_eq!(
        laptop.status_authority(),
        json!({"state": "available", "value": writer.authority}),
        "a successful discovery records the authority as reachable again"
    );
    let reachable = capture(
        "request.remote-offline-reachable",
        &writer,
        RemoteCaptureSequenceV1 {
            sequence: 2,
            previous_event_id: Some(receipt.event_id),
        },
    );
    assert!(
        matches!(
            &reachable.result,
            Err(problem) if matches!(
                problem.problem.source(),
                ApplicationProblem::Conflict { diagnostic, .. }
                    if diagnostic.code == "remote.authority_reachable"
            )
        ),
        "capture is refused again once the authority answers: {:?}",
        reachable.result
    );

    for fixture in [laptop, desktop] {
        let mut daemon = fixture.daemon;
        stop_daemon(&mut daemon);
    }
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn a_joined_node_drains_offline_frames_in_order_to_its_authority() {
    let desktop = RemoteDaemonFixture::start("remote-drain-desktop");
    let laptop = RemoteDaemonFixture::start("remote-drain-laptop");
    let node_id = BrainNodeId::new("node.remote-drain-laptop").unwrap();
    let mut grant = remote_grant(
        desktop.brain_id.clone(),
        node_id.clone(),
        &desktop.project_id,
        &GRANT_CREDENTIAL,
    );
    grant.capabilities = [
        RemoteCapabilityV1::CaptureOffline,
        RemoteCapabilityV1::DiscoverAuthority,
        RemoteCapabilityV1::TransferFrame,
        RemoteCapabilityV1::Replay,
        RemoteCapabilityV1::ServeAuthority,
    ]
    .into_iter()
    .collect();
    desktop.provision_and_enroll(&grant);
    let writer: RemoteWriterAuthorityV1 = serde_json::from_value(json!({
        "project_id": desktop.project_id,
        "scope": grant.scope,
        "authority": {
            "fence": writer_fence(&desktop.brain_id, 1, node_id.as_str()),
            "credential_revision": 1,
            "observed_at": now_micros(),
        },
    }))
    .unwrap();
    desktop.publish_writer(&node_id, &writer);
    let operate = |operation: &str, endpoint: &str| {
        laptop.node_operation(
            operation,
            endpoint,
            &desktop.brain_id,
            &node_id,
            &grant.scope,
        )
    };
    let (status, joined) = operate("join", &desktop.remote_endpoint());
    assert_eq!(status, reqwest::StatusCode::OK, "{joined}");
    let (status, refused) = operate("join", &unreachable_endpoint());
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{refused}");
    let mut previous_event_id = None;
    let mut captured = Vec::new();
    for (sequence, observation_id) in [
        (1, "observation.remote-drain-first"),
        (2, "observation.remote-drain-second"),
    ] {
        let receipt = laptop
            .local_client()
            .capture(&capture_request(
                &format!("request.remote-drain-capture-{sequence}"),
                &node_id,
                &writer,
                RemoteCaptureSequenceV1 {
                    sequence,
                    previous_event_id: previous_event_id.clone(),
                },
                observation(&desktop.project_id, observation_id),
            ))
            .unwrap()
            .result
            .expect("offline capture is admitted while the authority is unreachable")
            .outcome
            .payload()
            .cloned()
            .expect("an admitted capture returns its receipt");
        previous_event_id = Some(receipt.event_id.clone());
        captured.push(json!({
            "event_id": receipt.event_id,
            "sequence": sequence,
            "disposition": "transferred_pending",
        }));
    }
    let (status, unreachable) = operate("drain", &unreachable_endpoint());
    assert_eq!(
        (status, unreachable),
        (
            reqwest::StatusCode::BAD_GATEWAY,
            json!({"code": "authority_unreachable"})
        )
    );
    assert_eq!(
        laptop.status()["spool"]["pending_count"],
        json!(2),
        "frames stay pending until the authority acknowledges them"
    );

    let drain = || operate("drain", &desktop.remote_endpoint());
    let (status, drained) = drain();
    assert_eq!(status, reqwest::StatusCode::OK, "{drained}");
    assert_eq!(
        drained,
        json!({"transferred": captured}),
        "the authority acknowledges every frame once, in sequence order"
    );
    assert_eq!(
        laptop.status()["spool"]["pending_count"],
        json!(0),
        "acknowledged frames leave the laptop's pending spool"
    );
    assert_eq!(
        desktop.status()["spool"]["pending_count"],
        json!(2),
        "the authority holds both frames pending replay"
    );

    let (status, again) = drain();
    assert_eq!(
        (status, again),
        (reqwest::StatusCode::OK, json!({"transferred": []})),
        "a second drain sends nothing"
    );
    assert_eq!(desktop.status()["spool"]["pending_count"], json!(2));

    // Both observations start at byte 0 of one source, so the project store
    // refuses the second one's source cursor once the first commits.
    let node = desktop.client(ENROLLMENT_CREDENTIAL);
    let fence = &writer.authority.fence;
    let replay = |request_id: &str, frame: &Value| {
        node.replay(&replay_request(
            request_id,
            &node_id,
            fence,
            frame["event_id"].as_str().unwrap(),
        ))
        .unwrap()
        .result
        .map(|envelope| envelope.outcome.payload().cloned().unwrap())
    };
    let first = replay("request.remote-drain-replay-first", &captured[0]);
    assert!(
        matches!(first, Ok(RemoteReplayOutcomeV1::Acknowledged { .. })),
        "{first:?}"
    );
    let refused = replay("request.remote-drain-replay-refused", &captured[1]);
    assert!(
        matches!(refused, Ok(RemoteReplayOutcomeV1::Rejected { .. })),
        "a frame the store refuses is rejected, not left retryable: {refused:?}"
    );
    assert_eq!(
        desktop.status()["spool"]["pending_count"],
        json!(0),
        "a rejected frame leaves the authority's pending spool"
    );

    for fixture in [laptop, desktop] {
        let mut daemon = fixture.daemon;
        stop_daemon(&mut daemon);
    }
}

/// The #3656 Remote Brain job end to end on two spawned daemons: a laptop joins
/// a desktop Brain, captures while the desktop is unreachable, keeps those
/// frames across restarts of both daemons, drains them in order to the one
/// fenced writer, replays and queries them there, and loses access when the
/// desktop's operator revokes it without its pending frames being dropped.
#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn a_laptop_captures_offline_and_its_frames_reach_one_fenced_authority_until_revoked() {
    let mut desktop = RemoteDaemonFixture::start("remote-qualification-desktop");
    let mut laptop = RemoteDaemonFixture::start("remote-qualification-laptop");
    let node_id = BrainNodeId::new("node.remote-qualification-laptop").unwrap();
    let mut grant = remote_grant(
        desktop.brain_id.clone(),
        node_id.clone(),
        &desktop.project_id,
        &GRANT_CREDENTIAL,
    );
    grant.capabilities = [
        RemoteCapabilityV1::CaptureOffline,
        RemoteCapabilityV1::DiscoverAuthority,
        RemoteCapabilityV1::TransferFrame,
        RemoteCapabilityV1::Replay,
        RemoteCapabilityV1::Query,
        // The published fence names this node as the writer, and replay
        // admits frames only under a writer enrolled to serve authority.
        RemoteCapabilityV1::ServeAuthority,
    ]
    .into_iter()
    .collect();
    desktop.provision_and_enroll(&grant);
    let fence = writer_fence(&desktop.brain_id, 1, node_id.as_str());
    let writer: RemoteWriterAuthorityV1 = serde_json::from_value(json!({
        "project_id": desktop.project_id,
        "scope": grant.scope,
        "authority": {"fence": fence, "credential_revision": 1, "observed_at": now_micros()},
    }))
    .unwrap();
    desktop.publish_writer(&node_id, &writer);
    let brain_id = desktop.brain_id.clone();
    let operate = |laptop: &RemoteDaemonFixture, operation: &str, endpoint: &str| {
        laptop.node_operation(operation, endpoint, &brain_id, &node_id, &grant.scope)
    };
    let (status, joined) = operate(&laptop, "join", &desktop.remote_endpoint());
    assert_eq!(status, reqwest::StatusCode::OK, "{joined}");
    let (status, refused) = operate(&laptop, "join", &unreachable_endpoint());
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{refused}");

    let observations = [
        observation_at(
            &desktop.project_id,
            "observation.remote-qualification-first",
            0,
        ),
        observation_at(
            &desktop.project_id,
            "observation.remote-qualification-second",
            1,
        ),
    ];
    let mut previous_event_id = None;
    let mut event_ids = Vec::new();
    for (sequence, observation) in (1..).zip(&observations) {
        let receipt = laptop
            .local_client()
            .capture(&capture_request(
                &format!("request.remote-qualification-capture-{sequence}"),
                &node_id,
                &writer,
                RemoteCaptureSequenceV1 {
                    sequence,
                    previous_event_id: previous_event_id.clone(),
                },
                observation.clone(),
            ))
            .unwrap()
            .result
            .expect("offline capture is admitted while the authority is unreachable")
            .outcome
            .payload()
            .cloned()
            .expect("an admitted capture returns its receipt");
        previous_event_id = Some(receipt.event_id.clone());
        event_ids.push(receipt.event_id);
    }

    laptop.restart();
    assert_eq!(
        laptop.status()["spool"]["pending_count"],
        json!(2),
        "the laptop keeps its offline frames across a restart"
    );
    desktop.restart();
    let (status, drained) = operate(&laptop, "drain", &desktop.remote_endpoint());
    assert_eq!(status, reqwest::StatusCode::OK, "{drained}");
    assert_eq!(
        drained["transferred"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| (frame["event_id"].clone(), frame["sequence"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (json!(event_ids[0]), json!(1)),
            (json!(event_ids[1]), json!(2))
        ],
        "the restarted authority acknowledges both frames in capture order"
    );
    assert_eq!(laptop.status()["spool"]["pending_count"], json!(0));

    let node = desktop.client(ENROLLMENT_CREDENTIAL);
    let replay = |request_id: &str, event_id: &str| {
        node.replay(&replay_request(request_id, &node_id, &fence, event_id))
            .unwrap()
    };
    for (index, event_id) in event_ids.iter().enumerate() {
        let replayed = replay(
            &format!("request.remote-qualification-replay-{index}"),
            event_id,
        );
        let outcome = replayed
            .result
            .unwrap_or_else(|problem| {
                panic!("the authority replays drained frame {index}: {problem:?}")
            })
            .outcome
            .payload()
            .cloned()
            .expect("a replay returns its outcome");
        let RemoteReplayOutcomeV1::Acknowledged { receipt, .. } = outcome else {
            panic!("drained frame {index} is acknowledged: {outcome:?}");
        };
        assert_eq!(
            (&receipt.event_id, &receipt.writer_fence),
            (event_id, &fence),
            "the frame commits under the one published writer fence"
        );
    }
    assert_eq!(
        desktop.status()["spool"]["pending_count"],
        json!(0),
        "replayed frames leave the authority's pending spool"
    );

    for observation in &observations {
        let queried = node
            .query(&query_request(&node_id, &grant.scope, &fence, observation))
            .unwrap()
            .result
            .unwrap_or_else(|problem| panic!("the authority answers the query: {problem:?}"))
            .outcome
            .payload()
            .cloned()
            .expect("a query returns its result");
        let RemoteExactObservationResultV1::Found(found) = queried.observation else {
            panic!("the replayed observation is found: {queried:?}");
        };
        assert_eq!(
            &found.observation, observation,
            "the authority serves the exact observation the laptop captured offline"
        );
    }

    let revocation = desktop.revoke(&node_id);
    assert_eq!(
        revocation["revocations"][0]["node_id"],
        json!(node_id.as_str()),
        "{revocation}"
    );
    let refused_as_revoked =
        |authority: Value| authority["value"]["reason"] == "enrollment_revoked";
    let replayed = replay("request.remote-qualification-replay-revoked", &event_ids[0]);
    assert!(replayed.result.is_err());
    assert!(
        refused_as_revoked(serde_json::to_value(&replayed.authority).unwrap()),
        "a revoked node cannot replay"
    );
    let queried = node
        .query(&query_request(
            &node_id,
            &grant.scope,
            &fence,
            &observations[0],
        ))
        .unwrap();
    assert!(queried.result.is_err());
    assert!(
        refused_as_revoked(serde_json::to_value(&queried.authority).unwrap()),
        "a revoked node cannot query"
    );

    let (status, refused) = operate(&laptop, "join", &unreachable_endpoint());
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{refused}");
    laptop
        .local_client()
        .capture(&capture_request(
            "request.remote-qualification-capture-3",
            &node_id,
            &writer,
            RemoteCaptureSequenceV1 {
                sequence: 3,
                previous_event_id,
            },
            observation_at(
                &desktop.project_id,
                "observation.remote-qualification-third",
                2,
            ),
        ))
        .unwrap()
        .result
        .expect("the laptop still captures offline after the authority revokes it");
    let (status, refused) = operate(&laptop, "drain", &desktop.remote_endpoint());
    assert_ne!(status, reqwest::StatusCode::OK, "{refused}");
    assert_eq!(
        laptop.status()["spool"]["pending_count"],
        json!(1),
        "a revoked drain keeps the frame pending instead of dropping it"
    );

    for fixture in [laptop, desktop] {
        let mut daemon = fixture.daemon;
        stop_daemon(&mut daemon);
    }
}

fn replay_request(
    request_id: &str,
    node_id: &BrainNodeId,
    fence: &RemoteWriterFenceV1,
    event_id: &str,
) -> RemoteProtocolRequestV1<RemoteReplayRequestV1> {
    RemoteProtocolRequestV1::new(
        RequestId::new(request_id).unwrap(),
        fence.brain_id.clone(),
        node_id.clone(),
        1,
        Some(fence.clone()),
        now_micros(),
        RemoteReplayRequestV1 {
            event_id: event_id.to_owned(),
        },
    )
    .unwrap()
}

fn query_request(
    node_id: &BrainNodeId,
    scope: &RemoteRepositoryScopeV1,
    fence: &RemoteWriterFenceV1,
    observation: &DurableObservationV1,
) -> RemoteProtocolRequestV1<RemoteQueryRequestV1> {
    RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-qualification-query").unwrap(),
        fence.brain_id.clone(),
        node_id.clone(),
        1,
        Some(fence.clone()),
        now_micros(),
        RemoteQueryRequestV1 {
            schema_revision: 1,
            scope: scope.clone(),
            expected_shards: vec![ExpectedRemoteShardV1 {
                brain_id: fence.brain_id.as_str().to_owned(),
                shard_id: fence.shard_id.as_str().to_owned(),
                generation_id: fence.generation_id.as_str().to_owned(),
            }],
            expected_authority: fence.clone(),
            operation: RemoteQueryOperationV1::ExactObservation {
                observation_id: observation.observation_id().clone(),
            },
        },
    )
    .unwrap()
}
