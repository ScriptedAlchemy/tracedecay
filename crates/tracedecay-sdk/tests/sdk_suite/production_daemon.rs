#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustls::pki_types::pem::PemObject;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_contracts::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_contracts::remote::recovery::PromotionConfirmationV1;
use tracedecay_contracts::remote::replay::{
    RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidenceV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, LegalAction, PolicyDecisionRef,
    RequestId, ResolvedScope, RetryDirective,
};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, ManifestDigest,
    ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_sdk::client::{
    CancellationStatus, Client, ClientError, ConnectionMode, StreamOptions, StreamResume,
};
use tracedecay_sdk::operations::{TypedOperation, WorkflowGetDefinition, WorkflowListDefinitions};
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};

const REMOTE_TLS_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost.crt.pem");
const REMOTE_TLS_PRIVATE_KEY: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost.key.pem");
const REMOTE_TLS_ROOT_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost-root.crt.pem");
const REMOTE_TLS_ALTERNATE_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/alternate.crt.pem");
const REMOTE_TLS_ALTERNATE_PRIVATE_KEY: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/alternate.key.pem");
const REMOTE_TLS_ALTERNATE_ROOT_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/alternate-root.crt.pem");

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill")
                .args(["-INT", &self.child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn installed_rust_client_requires_workflow_reads_and_exact_lifecycle_capability() {
    let scratch = TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let profile = home.join(".tracedecay");
    let project = scratch.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"sdk-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    fs::create_dir(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub const FIXTURE: bool = true;\n",
    )
    .unwrap();
    run(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project));
    let binary = production_binary();
    let socket = profile.join("daemon.sock");
    let authority_path = profile.join("daemon-authority.json");
    let mut daemon_command = Command::new(&binary);
    daemon_command
        .args(["daemon", "run", "--socket"])
        .arg(&socket)
        .current_dir(&project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    isolated(&mut daemon_command, &home, &profile);
    let mut daemon = Daemon {
        child: daemon_command.spawn().unwrap(),
    };
    let authority = wait_for_authority(&mut daemon.child, &authority_path);
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
    let project_id = context["project"]["project_id"].as_str().unwrap();
    let endpoint = format!(
        "http://{}",
        authority["http_application_endpoint"].as_str().unwrap()
    );
    let token = authority["auth_token"].as_str().unwrap();

    let mode = ConnectionMode::local(&endpoint, project_id, token);
    let client = Client::builder(mode)
        .origin(
            reqwest::Url::parse(&endpoint)
                .unwrap()
                .origin()
                .ascii_serialization(),
        )
        .build()
        .unwrap();
    assert_workflow_get_definition_route_conceals_missing_definition(&client);
    let request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();
    let request_id = client
        .execute::<WorkflowListDefinitions>(&request)
        .unwrap_or_else(|error| panic!("WorkflowListDefinitions must succeed: {error}"))
        .request_id;
    match client.stream_operation(&request_id, StreamOptions::default()) {
        Ok(mut initial) => {
            let open = initial.next().unwrap().unwrap();
            assert_eq!(open.event, "open");
            let frontier = &open.data["data"]["frontier"];
            let resume = StreamResume {
                token: frontier["resume_token"].as_str().unwrap().to_owned(),
                next_sequence: frontier["next_sequence"].as_u64().unwrap(),
            };
            drop(initial);
            let resumed = client
                .stream_operation(
                    &request_id,
                    StreamOptions {
                        resume: Some(resume),
                        max_reconnects: 0,
                    },
                )
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(resumed.last().is_some_and(|event| event.terminal()));
        }
        Err(ClientError::Problem(problem)) if problem.code == "operation_event.unavailable" => {
            let resumed = client.stream_operation(
                &request_id,
                StreamOptions {
                    resume: Some(StreamResume {
                        token: "resume.unavailable".to_owned(),
                        next_sequence: 1,
                    }),
                    max_reconnects: 0,
                },
            );
            assert!(matches!(
                resumed,
                Err(ClientError::Problem(problem))
                    if problem.code == "operation_event.resume_expired"
            ));
        }
        Err(error) => panic!("production stream failed unexpectedly: {error}"),
    }
    match client.cancel_operation(&request_id) {
        Ok(cancellation) => assert!(matches!(
            cancellation.status,
            CancellationStatus::Requested
                | CancellationStatus::AlreadyRequested
                | CancellationStatus::AlreadyTerminal
        )),
        Err(ClientError::Problem(problem)) => {
            assert_eq!(problem.code, "operation_event.unavailable");
        }
        Err(error) => panic!("production cancellation failed unexpectedly: {error}"),
    }
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn enrolled_remote_client_rejects_an_untrusted_private_authority_and_isolates_enrollment() {
    let scratch = TempDir::new().unwrap();
    let first_home = scratch.path().join("first-home");
    let second_home = scratch.path().join("second-home");
    let first_profile = first_home.join(".tracedecay");
    let second_profile = second_home.join(".tracedecay");
    let project = scratch.path().join("project");
    fs::create_dir_all(&first_home).unwrap();
    fs::create_dir_all(&second_home).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"remote-sdk-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    fs::create_dir(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub const FIXTURE: bool = true;\n",
    )
    .unwrap();
    run(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project));

    let binary = production_binary();
    let first_certificate = scratch.path().join("first-localhost.crt.pem");
    let first_private_key = scratch.path().join("first-localhost.key.pem");
    let second_certificate = scratch.path().join("second-localhost.crt.pem");
    let second_private_key = scratch.path().join("second-localhost.key.pem");
    fs::write(&first_certificate, REMOTE_TLS_CERTIFICATE).unwrap();
    fs::write(&first_private_key, REMOTE_TLS_PRIVATE_KEY).unwrap();
    fs::write(&second_certificate, REMOTE_TLS_ALTERNATE_CERTIFICATE).unwrap();
    fs::write(&second_private_key, REMOTE_TLS_ALTERNATE_PRIVATE_KEY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&first_private_key, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&second_private_key, fs::Permissions::from_mode(0o600)).unwrap();

    let (mut first, first_authority, first_remote) = spawn_remote_daemon(
        &binary,
        &first_home,
        &first_profile,
        &project,
        &first_certificate,
        &first_private_key,
    );
    let (mut second, _, second_remote) = spawn_remote_daemon(
        &binary,
        &second_home,
        &second_profile,
        &project,
        &second_certificate,
        &second_private_key,
    );
    assert_ne!(first_remote, second_remote);

    for (home, profile) in [
        (&first_home, &first_profile),
        (&second_home, &second_profile),
    ] {
        let mut init = Command::new(&binary);
        init.arg("init").current_dir(&project);
        isolated(&mut init, home, profile);
        run(&mut init);
    }

    let mut context_command = Command::new(&binary);
    context_command
        .args(["projects", "context"])
        .arg(&project)
        .arg("--json")
        .current_dir(&project);
    isolated(&mut context_command, &first_home, &first_profile);
    let context: Value = serde_json::from_slice(&run(&mut context_command)).unwrap();
    let project_id = context["project"]["project_id"].as_str().unwrap();
    let local_endpoint = first_authority["http_application_endpoint"]
        .as_str()
        .unwrap();
    let local_token = first_authority["auth_token"].as_str().unwrap();
    let local_base = format!("http://{local_endpoint}");
    let local_client = Client::builder(ConnectionMode::local(&local_base, project_id, local_token))
        .build()
        .unwrap();
    let list_request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();
    local_client
        .execute::<WorkflowListDefinitions>(&list_request)
        .expect("the exact local project route must be mounted and admitted");

    let grant_credential = *b"0123456789abcdef0123456789abcdef";
    let enrollment_credential = *b"fedcba9876543210fedcba9876543210";
    let brain_id = BrainId::new(first_authority["brain_id"].as_str().unwrap()).unwrap();
    let node_id = BrainNodeId::new("node.remote-sdk-production").unwrap();
    let grant = remote_grant(brain_id, node_id, project_id, &grant_credential);
    let admission = remote_admission(&grant);
    let provisioned = reqwest::blocking::Client::new()
        .post(format!("{local_base}/remote-nodes/provision"))
        .bearer_auth(local_token)
        .header(reqwest::header::ORIGIN, &local_base)
        .json(&json!({"grant": grant, "admission": admission}))
        .send()
        .unwrap();
    let provision_status = provisioned.status();
    let provision_body = provisioned.text().unwrap_or_default();
    assert_eq!(
        provision_status,
        reqwest::StatusCode::NO_CONTENT,
        "{provision_body}"
    );

    let request = enrollment_request(&grant);
    let untrusted_authority = EnrolledRemoteClient::new_with_root_certificate(
        format!("https://{second_remote}/remote/"),
        grant_credential,
        Duration::from_secs(5),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap();
    assert!(matches!(
        untrusted_authority.enroll(&request, enrollment_credential),
        Err(RemoteClientError::Transport(_))
    ));
    let other_authority = EnrolledRemoteClient::new_with_root_certificate(
        format!("https://{second_remote}/remote/"),
        grant_credential,
        Duration::from_secs(5),
        REMOTE_TLS_ALTERNATE_ROOT_CERTIFICATE,
    )
    .unwrap();
    assert!(matches!(
        other_authority.enroll(&request, enrollment_credential),
        Err(RemoteClientError::Protocol(_))
    ));

    let enrolled = EnrolledRemoteClient::new_with_root_certificate(
        format!("https://{first_remote}/remote/"),
        grant_credential,
        Duration::from_secs(5),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap()
    .enroll(&request, enrollment_credential)
    .expect("the SDK must join /remote/ and trust the configured private root");
    let envelope = enrolled.result.as_ref().expect("enrollment is admitted");
    assert_eq!(
        envelope.request_id.as_str(),
        "request.remote-sdk-production"
    );
    let record = envelope
        .outcome
        .payload()
        .expect("admitted enrollment carries its credential record");
    assert_eq!(
        record.enrollment_id.as_str(),
        "enrollment.remote-sdk-production"
    );
    assert_eq!(record.brain_id, grant.brain_id);
    assert_eq!(record.node_id, grant.node_id);
    assert_eq!(record.expires_at, grant.expires_at);
    assert_eq!(record.revoked_at, None);
    assert_eq!(record.capabilities, grant.capabilities);
    assert_eq!(record.scope, grant.scope);

    let local_route_response = tls_http11_request(
        first_remote,
        REMOTE_TLS_ROOT_CERTIFICATE,
        &format!(
            "POST /projects/{project_id}/application/workflow/list-definitions HTTP/1.1\r\nHost: {first_remote}\r\nAuthorization: Bearer {local_token}\r\nOrigin: {local_base}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ),
    );
    assert!(
        local_route_response.starts_with("HTTP/1.1 404"),
        "the TLS listener exposed a route proven live on the local listener: {local_route_response}"
    );

    stop_daemon(&mut first);
    stop_daemon(&mut second);
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn enrolled_remote_failover_without_a_published_writer_is_a_typed_refusal() {
    let scratch = TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let profile = home.join(".tracedecay");
    let project = scratch.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"remote-failover-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
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
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();

    let binary = production_binary();
    let (mut daemon, authority, remote) = spawn_remote_daemon(
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
    let project_id = context["project"]["project_id"].as_str().unwrap();
    let local_base = format!(
        "http://{}",
        authority["http_application_endpoint"].as_str().unwrap()
    );
    let local_token = authority["auth_token"].as_str().unwrap();

    let grant_credential = *b"0123456789abcdef0123456789abcdef";
    let enrollment_credential = *b"fedcba9876543210fedcba9876543210";
    let brain_id = BrainId::new(authority["brain_id"].as_str().unwrap()).unwrap();
    let node_id = BrainNodeId::new("node.remote-sdk-production").unwrap();
    let mut grant = remote_grant(
        brain_id.clone(),
        node_id.clone(),
        project_id,
        &grant_credential,
    );
    grant.capabilities = [RemoteCapabilityV1::Promote].into_iter().collect();
    let provisioned = reqwest::blocking::Client::new()
        .post(format!("{local_base}/remote-nodes/provision"))
        .bearer_auth(local_token)
        .header(reqwest::header::ORIGIN, &local_base)
        .json(&json!({"grant": grant, "admission": remote_admission(&grant)}))
        .send()
        .unwrap();
    assert_eq!(provisioned.status(), reqwest::StatusCode::NO_CONTENT);
    let endpoint = format!("https://{remote}/remote/");
    EnrolledRemoteClient::new_with_root_certificate(
        &endpoint,
        grant_credential,
        Duration::from_secs(5),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap()
    .enroll(&enrollment_request(&grant), enrollment_credential)
    .unwrap()
    .result
    .expect("the node is enrolled before it asks for failover");

    let believed_writer: RemoteWriterFenceV1 = serde_json::from_value(json!({
        "brain_id": brain_id.as_str(),
        "shard_id": "shard.remote-sdk-production",
        "generation_id": "generation.remote-sdk-production",
        "placement_revision": 1,
        "authority_epoch": 1,
        "authority_node_id": "node.remote-sdk-previous-writer"
    }))
    .unwrap();
    let sent_at = now_micros();
    let failover = RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-sdk-failover").unwrap(),
        brain_id,
        node_id,
        1,
        Some(believed_writer),
        sent_at,
        PromotionConfirmationV1 {
            preview_id: "promotion.remote-sdk-failover".to_owned(),
            expected_authority_epoch: 1,
            expected_placement_revision: 1,
            expires_at_micros: sent_at.0.saturating_add(60_000_000),
        },
    )
    .unwrap();
    let response = EnrolledRemoteClient::new_with_root_certificate(
        &endpoint,
        enrollment_credential,
        Duration::from_secs(30),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap()
    .failover(&failover)
    .expect("a refused failover is still a canonical protocol response");

    let problem = response
        .result
        .as_ref()
        .expect_err("failover cannot promote over a writer that was never published");
    assert_eq!(
        (
            problem.problem.code.as_str(),
            problem.problem.retry,
            problem.problem.legal_actions.as_slice(),
        ),
        (
            "remote.writer_authority_unpublished",
            RetryDirective::Never,
            [LegalAction::ContactAdministrator].as_slice(),
        )
    );
    let authority_state = serde_json::to_value(&response.authority).unwrap();
    assert_eq!(
        (
            &authority_state["state"],
            &authority_state["value"]["reason"]
        ),
        (
            &json!("unavailable"),
            &json!("writer_authority_unpublished")
        ),
        "{authority_state}"
    );

    stop_daemon(&mut daemon);
}

#[test]
#[ignore = "requires a prebuilt production tracedecay daemon"]
fn enrolled_node_fails_over_after_the_operator_publishes_the_first_writer() {
    let scratch = TempDir::new().unwrap();
    let home = scratch.path().join("home");
    let profile = home.join(".tracedecay");
    let project = scratch.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"remote-publication-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
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
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();

    let binary = production_binary();
    let (mut daemon, authority, remote) = spawn_remote_daemon(
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
    let project_id = context["project"]["project_id"].as_str().unwrap();
    let local_base = format!(
        "http://{}",
        authority["http_application_endpoint"].as_str().unwrap()
    );
    let local_token = authority["auth_token"].as_str().unwrap();
    Client::builder(ConnectionMode::local(&local_base, project_id, local_token))
        .build()
        .unwrap()
        .execute::<WorkflowListDefinitions>(
            &serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!(
                {}
            ))
            .unwrap(),
        )
        .expect("the daemon serves the project before its writer is published");

    let grant_credential = *b"0123456789abcdef0123456789abcdef";
    let enrollment_credential = *b"fedcba9876543210fedcba9876543210";
    let brain_id = BrainId::new(authority["brain_id"].as_str().unwrap()).unwrap();
    let node_id = BrainNodeId::new("node.remote-sdk-production").unwrap();
    let mut grant = remote_grant(
        brain_id.clone(),
        node_id.clone(),
        project_id,
        &grant_credential,
    );
    grant.capabilities = [RemoteCapabilityV1::Promote].into_iter().collect();
    let operator = reqwest::blocking::Client::new();
    let local_post = |path: &str, body: &Value| {
        let response = operator
            .post(format!("{local_base}{path}"))
            .bearer_auth(local_token)
            .header(reqwest::header::ORIGIN, &local_base)
            .json(body)
            .send()
            .unwrap();
        let status = response.status();
        let body = response.text().unwrap();
        (status, body)
    };
    let (status, body) = local_post(
        "/remote-nodes/provision",
        &json!({"grant": grant, "admission": remote_admission(&grant)}),
    );
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{body}");
    let endpoint = format!("https://{remote}/remote/");
    EnrolledRemoteClient::new_with_root_certificate(
        &endpoint,
        grant_credential,
        Duration::from_secs(5),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap()
    .enroll(&enrollment_request(&grant), enrollment_credential)
    .unwrap()
    .result
    .expect("the node is enrolled before its writer is published");

    let previous_writer: RemoteWriterFenceV1 = serde_json::from_value(json!({
        "brain_id": brain_id.as_str(),
        "shard_id": "shard.remote-sdk-production",
        "generation_id": "generation.remote-sdk-production",
        "placement_revision": 1,
        "authority_epoch": 1,
        "authority_node_id": "node.remote-sdk-previous-writer"
    }))
    .unwrap();
    let writer = json!({
        "project_id": project_id,
        "scope": grant.scope,
        "authority": {
            "fence": previous_writer,
            "credential_revision": 1,
            "observed_at": now_micros(),
        },
    });
    let publication = json!({
        "node_id": node_id,
        "writer": writer,
        "replay_policy": replay_policy(&grant.scope),
    });

    let mut unregistered = publication.clone();
    unregistered["node_id"] = json!("node.remote-sdk-unregistered");
    assert_eq!(
        local_post("/remote-nodes/writer-authority", &unregistered),
        (
            reqwest::StatusCode::NOT_FOUND,
            json!({"code": "node_not_registered"}).to_string()
        )
    );
    let mut unprovisioned_scope = grant.scope.clone();
    unprovisioned_scope.repository_id = RepositoryId::new("repository.remote-sdk-other").unwrap();
    let mut unprovisioned = publication.clone();
    unprovisioned["writer"]["scope"] = json!(unprovisioned_scope);
    unprovisioned["replay_policy"] = json!(replay_policy(&unprovisioned_scope));
    assert_eq!(
        local_post("/remote-nodes/writer-authority", &unprovisioned),
        (
            reqwest::StatusCode::FORBIDDEN,
            json!({"code": "scope_not_provisioned"}).to_string()
        )
    );
    for attempt in ["publication", "exact replay"] {
        let (status, body) = local_post("/remote-nodes/writer-authority", &publication);
        assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{attempt}: {body}");
    }

    let sent_at = now_micros();
    let failover = RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-sdk-failover").unwrap(),
        brain_id,
        node_id.clone(),
        1,
        Some(previous_writer),
        sent_at,
        PromotionConfirmationV1 {
            preview_id: "promotion.remote-sdk-failover".to_owned(),
            expected_authority_epoch: 1,
            expected_placement_revision: 1,
            expires_at_micros: sent_at.0.saturating_add(60_000_000),
        },
    )
    .unwrap();
    let response = EnrolledRemoteClient::new_with_root_certificate(
        &endpoint,
        enrollment_credential,
        Duration::from_secs(30),
        REMOTE_TLS_ROOT_CERTIFICATE,
    )
    .unwrap()
    .failover(&failover)
    .expect("failover over a published writer is a canonical protocol response");
    let receipt = response
        .result
        .as_ref()
        .expect("the enrolled node is promoted over the published writer")
        .outcome
        .payload()
        .expect("an admitted promotion carries its compare-and-swap receipt");
    assert_eq!(
        (
            receipt.previous_epoch,
            receipt.installed_epoch,
            receipt.old_authority_fenced
        ),
        (1, 2, true)
    );
    let promoted = serde_json::to_value(&response.authority).unwrap();
    assert_eq!(
        (
            &promoted["state"],
            &promoted["value"]["fence"]["authority_node_id"],
            &promoted["value"]["fence"]["authority_epoch"],
        ),
        (&json!("available"), &json!(node_id.as_str()), &json!(2)),
        "{promoted}"
    );

    assert_eq!(
        local_post("/remote-nodes/writer-authority", &publication),
        (
            reqwest::StatusCode::CONFLICT,
            json!({"code": "conflict"}).to_string()
        ),
        "publication must never roll a promoted lineage back to its first writer"
    );

    stop_daemon(&mut daemon);
}

fn spawn_remote_daemon(
    binary: &Path,
    home: &Path,
    profile: &Path,
    project: &Path,
    certificate: &Path,
    private_key: &Path,
) -> (Daemon, Value, SocketAddr) {
    let socket = profile.join("daemon.sock");
    let authority_path = profile.join("daemon-authority.json");
    let mut command = Command::new(binary);
    command
        .args(["daemon", "run", "--socket"])
        .arg(socket)
        .args(["--remote-listen", "127.0.0.1:0"])
        .args(["--remote-tls-cert"])
        .arg(certificate)
        .args(["--remote-tls-key"])
        .arg(private_key)
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    isolated(&mut command, home, profile);
    let mut daemon = Daemon {
        child: command.spawn().unwrap(),
    };
    let authority = wait_for_remote_authority(&mut daemon.child, &authority_path);
    let remote_endpoint = authority["remote_brain_tls_endpoint"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (daemon, authority, remote_endpoint)
}

fn stop_daemon(daemon: &mut Daemon) {
    if let Some(status) = daemon.child.try_wait().unwrap() {
        assert!(status.success(), "production daemon exited with {status}");
        return;
    }
    let signal = Command::new("kill")
        .args(["-INT", &daemon.child.id().to_string()])
        .status()
        .unwrap();
    assert!(signal.success(), "failed to request daemon shutdown");
    // The daemon's own shutdown contract is DAEMON_SHUTDOWN_DEADLINE (45s):
    // every owner phase gets a typed deadline and the process exits with a
    // receipt even when an owner times out. The assertion here is that
    // shutdown COMPLETES within that contract, a tighter local SLA turned
    // loaded CI runners into false failures.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "production daemon shutdown returned {status}"
            );
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("production Remote Brain daemon did not complete shutdown after SIGINT");
}

fn now_micros() -> UtcMicros {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    UtcMicros(i64::try_from(elapsed.as_micros()).unwrap())
}

fn remote_grant(
    brain_id: BrainId,
    node_id: BrainNodeId,
    project_id: &str,
    secret: &[u8],
) -> EnrollmentGrantV1 {
    let now = now_micros();
    EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote-sdk-production").unwrap(),
        brain_id,
        node_id,
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
        revision: 1,
        issued_at: UtcMicros(now.0.saturating_sub(60_000_000)),
        expires_at: UtcMicros(now.0.saturating_add(600_000_000)),
        revoked_at: None,
        capabilities: [RemoteCapabilityV1::Query].into_iter().collect(),
        scope: RemoteRepositoryScopeV1 {
            project_id: ProjectId::new(project_id).unwrap(),
            repository_id: RepositoryId::new("repository.remote-sdk-production").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote-sdk-production").unwrap(),
            reference: Some(RefId::new("refs/heads/remote-sdk-production").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-sdk-production").unwrap(),
        },
    }
}

fn remote_admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let now = now_micros();
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .unwrap();
    let grant_digest = canonical_sha256(grant).unwrap();
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote-sdk-production",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote-sdk-production.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: now,
        },
        ActorId::new("actor.remote-sdk-production").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(600_000_000))).unwrap(),
    )
    .unwrap()
}

fn replay_policy(scope: &RemoteRepositoryScopeV1) -> RemoteReplayPolicyEvidenceV1 {
    let digest = ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap();
    RemoteReplayPolicyEvidenceV1 {
        scope: ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            scope.reference.clone(),
        )
        .unwrap(),
        repository_scope: scope.clone(),
        policy_revision: 1,
        decision: RemoteReplayPolicyDecisionV1::Admit,
        policy: PolicyDecisionRef::new(
            "policy.remote-sdk-production.replay",
            1,
            digest.clone(),
            ComponentVersion::new("policy.remote-sdk-production.replay.v1").unwrap(),
        )
        .unwrap(),
        configuration_digest: digest.clone(),
        catalog_digest: digest.clone(),
        privacy_digest: digest,
        revalidated_at: now_micros(),
    }
}

fn enrollment_request(grant: &EnrollmentGrantV1) -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let sent_at = now_micros();
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.remote-sdk-production").unwrap(),
        grant.brain_id.clone(),
        grant.node_id.clone(),
        sent_at,
        EnrollmentRequestV1 {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.revision,
            enrollment_id: EntityId::new("enrollment.remote-sdk-production").unwrap(),
            brain_id: grant.brain_id.clone(),
            node_id: grant.node_id.clone(),
            expires_at: grant.expires_at,
            capabilities: grant.capabilities.clone(),
            scope: grant.scope.clone(),
        },
    )
    .unwrap()
}

fn tls_http11_request(endpoint: SocketAddr, certificate_pem: &[u8], request: &str) -> String {
    let certificate = rustls::pki_types::CertificateDer::from_pem_slice(certificate_pem).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).unwrap();
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connection = rustls::ClientConnection::new(
        Arc::new(config),
        rustls::pki_types::ServerName::from(endpoint.ip()),
    )
    .unwrap();
    let socket = TcpStream::connect(endpoint).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut stream = rustls::StreamOwned::new(connection, socket);
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert_eq!(stream.conn.alpn_protocol(), Some(b"http/1.1".as_slice()));
    response
}

fn assert_workflow_get_definition_route_conceals_missing_definition(client: &Client) {
    let request =
        serde_json::from_value::<<WorkflowGetDefinition as TypedOperation>::Request>(json!({
            "definition_id": "workflow.sdk.missing",
            "definition_version": 1
        }))
        .expect("canonical missing-definition get request");
    let error = client
        .execute::<WorkflowGetDefinition>(&request)
        .expect_err("a nonexistent workflow definition must not resolve");
    let ClientError::Problem(problem) = error else {
        panic!("mounted get-definition route must return a typed Workflow problem: {error}");
    };

    assert_eq!(problem.status, 404);
    assert_eq!(problem.kind, "not_found_or_not_authorized");
    assert_eq!(problem.code, "not_found_or_not_authorized");
    assert_eq!(problem.retry, "never");
    // Concealment doctrine: a not-found/not-authorized problem must NOT
    // confirm the probed route exists, so the envelope omits binding_id
    // (pinned by concealed_http_problem_omits_binding in tracedecay-api).
    assert_eq!(problem.envelope["binding_id"], serde_json::Value::Null);
    assert_eq!(
        problem.envelope["contract"]["schema_id"],
        json!(WorkflowGetDefinition::RESULT_SCHEMA_ID)
    );
    assert_eq!(
        problem.envelope["contract"]["schema_revision"],
        json!(WorkflowGetDefinition::RESULT_SCHEMA_REVISION)
    );
}

use crate::isolated_profile::apply_isolated_profile_env as isolated;
use crate::{production_binary, run};

fn wait_for_authority(child: &mut Child, path: &Path) -> Value {
    wait_for_authority_record(child, path, false)
}

fn wait_for_remote_authority(child: &mut Child, path: &Path) -> Value {
    wait_for_authority_record(child, path, true)
}

fn wait_for_authority_record(child: &mut Child, path: &Path, require_remote: bool) -> Value {
    // Generous on purpose: daemon boot converges registered schemas before the
    // authority record carries every endpoint, and on a loaded runner (or a
    // second daemon booting behind a first) that legitimately takes tens of
    // seconds. The assertion is that the authority appears, not that the
    // machine was idle.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "production daemon exited during startup"
        );
        if let Ok(contents) = fs::read(path)
            && let Ok(value) = serde_json::from_slice::<Value>(&contents)
            && value["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
            && value["http_application_endpoint"].as_str().is_some()
            && (!require_remote || value["remote_brain_tls_endpoint"].as_str().is_some())
        {
            return value;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "timed out waiting for{} daemon authority at {}",
        if require_remote { " Remote Brain" } else { "" },
        path.display()
    );
}
