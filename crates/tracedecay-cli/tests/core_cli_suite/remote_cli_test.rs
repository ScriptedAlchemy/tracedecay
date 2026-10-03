//! CLI journeys for Remote Brain protocol commands. The blocking SDK client must
//! run off the async runtime so dropping it does not panic the CLI main task.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_contracts::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_contracts::remote::recovery::PromotionConfirmationV1;
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef, RequestId,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, ManifestDigest,
    ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
    canonical_sha256,
};

use crate::common::{
    apply_isolated_profile_env, canonical_existing_path, initialize_tracedecay_cli_project,
    tracedecay_bin, tracedecay_command_with_home,
};

const REMOTE_TLS_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost.crt.pem");
const REMOTE_TLS_PRIVATE_KEY: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost.key.pem");
const REMOTE_TLS_ROOT_CERTIFICATE: &[u8] =
    include_bytes!("../../../../tests/fixtures/remote_tls/localhost-root.crt.pem");

struct RemoteDaemon {
    child: Child,
}

impl Drop for RemoteDaemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill")
                .args(["-INT", &self.child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct RemoteCliFixture {
    _home: TempDir,
    _project: TempDir,
    _cert_dir: TempDir,
    _daemon: RemoteDaemon,
    home_path: PathBuf,
    project_path: PathBuf,
    remote_endpoint: std::net::SocketAddr,
    authority: Value,
    trust_root_path: PathBuf,
}

fn remote_cli_fixture() -> RemoteCliFixture {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let home_path = canonical_existing_path(home.path());
    let project_path = canonical_existing_path(project.path());
    fs::create_dir_all(project_path.join("src")).unwrap();
    fs::write(
        project_path.join("src/lib.rs"),
        "pub const REMOTE: bool = true;\n",
    )
    .unwrap();
    let git = Command::new(crate::common::git_program())
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(&project_path)
        .output()
        .expect("git init");
    assert!(git.status.success(), "git init failed");

    let profile = home_path.join(".tracedecay");
    let cert_dir = TempDir::new().unwrap();
    let certificate = cert_dir.path().join("localhost.crt.pem");
    let private_key = cert_dir.path().join("localhost.key.pem");
    fs::write(&certificate, REMOTE_TLS_CERTIFICATE).unwrap();
    fs::write(&private_key, REMOTE_TLS_PRIVATE_KEY).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let socket = profile.join("daemon.sock");
    let authority_path = profile.join("daemon-authority.json");
    let mut daemon_cmd = Command::new(tracedecay_bin());
    daemon_cmd
        .args(["daemon", "run", "--socket"])
        .arg(&socket)
        .args(["--remote-listen", "127.0.0.1:0"])
        .args(["--remote-tls-cert"])
        .arg(&certificate)
        .args(["--remote-tls-key"])
        .arg(&private_key)
        .current_dir(&project_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    apply_isolated_profile_env(&mut daemon_cmd, &home_path, &profile);
    let mut daemon = RemoteDaemon {
        child: daemon_cmd.spawn().expect("spawn remote daemon"),
    };
    let authority = wait_for_remote_authority(&mut daemon.child, &authority_path);
    let remote_endpoint = authority["remote_brain_tls_endpoint"]
        .as_str()
        .expect("remote endpoint")
        .parse::<std::net::SocketAddr>()
        .expect("parse remote endpoint");

    initialize_tracedecay_cli_project(&home_path, &project_path);
    let trust_root_path = write_bytes_fixture(
        &home_path,
        "root.crt.pem",
        REMOTE_TLS_ROOT_CERTIFICATE,
    );

    RemoteCliFixture {
        _home: home,
        _project: project,
        _cert_dir: cert_dir,
        _daemon: daemon,
        home_path,
        project_path,
        remote_endpoint,
        authority,
        trust_root_path,
    }
}

fn fixture_project_context(fixture: &RemoteCliFixture) -> Value {
    let context = tracedecay_command_with_home(&fixture.home_path)
        .current_dir(&fixture.project_path)
        .args([
            "projects",
            "context",
            &fixture.project_path.to_string_lossy(),
            "--json",
        ])
        .output()
        .expect("projects context");
    assert!(
        context.status.success(),
        "projects context failed: {}",
        String::from_utf8_lossy(&context.stderr)
    );
    serde_json::from_slice(&context.stdout).unwrap()
}

fn provision_remote_node(fixture: &RemoteCliFixture, grant: &EnrollmentGrantV1) {
    let local_endpoint = fixture.authority["http_application_endpoint"]
        .as_str()
        .expect("local endpoint");
    let local_token = fixture.authority["auth_token"].as_str().expect("auth token");
    let local_base = format!("http://{local_endpoint}");
    let admission = remote_admission(grant);
    let provisioned = reqwest::blocking::Client::new()
        .post(format!("{local_base}/remote-nodes/provision"))
        .bearer_auth(local_token)
        .header(reqwest::header::ORIGIN, &local_base)
        .json(&json!({"grant": grant, "admission": admission}))
        .send()
        .expect("provision remote node");
    assert_eq!(
        provisioned.status(),
        reqwest::StatusCode::NO_CONTENT,
        "{}",
        provisioned.text().unwrap_or_default()
    );
}

fn run_remote_enroll(
    fixture: &RemoteCliFixture,
    grant_path: &Path,
    enroll_path: &Path,
    request_path: &Path,
) -> std::process::Output {
    tracedecay_command_with_home(&fixture.home_path)
        .current_dir(&fixture.project_path)
        .args([
            "remote",
            "enroll",
            "--endpoint",
            &format!("https://{}/remote/", fixture.remote_endpoint),
            "--trust-root-file",
            &fixture.trust_root_path.to_string_lossy(),
            "--credential-file",
            &grant_path.to_string_lossy(),
            "--enrollment-credential-file",
            &enroll_path.to_string_lossy(),
            "--request-file",
            &request_path.to_string_lossy(),
            "--json",
        ])
        .output()
        .expect("tracedecay remote enroll")
}

#[test]
fn remote_enroll_runs_off_the_async_runtime_and_returns_a_protocol_response() {
    let fixture = remote_cli_fixture();
    let context = fixture_project_context(&fixture);
    let project_id = context["project"]["project_id"]
        .as_str()
        .expect("project id");

    let grant_credential = *b"0123456789abcdef0123456789abcdef";
    let enrollment_credential = *b"fedcba9876543210fedcba9876543210";
    let brain_id = BrainId::new(fixture.authority["brain_id"].as_str().expect("brain id"))
        .unwrap();
    let node_id = BrainNodeId::new("node.remote-cli-enroll").unwrap();
    let grant = remote_grant(brain_id, node_id, project_id, &grant_credential);
    provision_remote_node(&fixture, &grant);

    let request = enrollment_request(&grant);
    let request_path = write_json_fixture(&fixture.home_path, "enroll-request.json", &request);
    let grant_path = write_bytes_fixture(&fixture.home_path, "grant.bin", &grant_credential);
    let enroll_path =
        write_bytes_fixture(&fixture.home_path, "enroll.bin", &enrollment_credential);

    let output = run_remote_enroll(&fixture, &grant_path, &enroll_path, &request_path);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Cannot drop a runtime in a context where blocking is not allowed"),
        "remote enroll must not panic in async main:\n{stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "remote enroll must exit successfully\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("canonical remote enroll JSON");
    assert_eq!(
        response["request_id"].as_str(),
        Some("request.remote-cli-enroll"),
        "{response}"
    );
    assert!(
        response.get("result").is_some(),
        "enrollment must return a canonical protocol response: {response}"
    );
}

#[test]
fn remote_failover_json_refusal_prints_exactly_one_protocol_document() {
    let fixture = remote_cli_fixture();
    let context = fixture_project_context(&fixture);
    let project_id = context["project"]["project_id"]
        .as_str()
        .expect("project id");

    let grant_credential = *b"0123456789abcdef0123456789abcdef";
    let enrollment_credential = *b"fedcba9876543210fedcba9876543210";
    let brain_id = BrainId::new(fixture.authority["brain_id"].as_str().expect("brain id"))
        .unwrap();
    let node_id = BrainNodeId::new("node.remote-cli-failover").unwrap();
    let mut grant = remote_grant(brain_id.clone(), node_id.clone(), project_id, &grant_credential);
    grant.capabilities = [RemoteCapabilityV1::Promote].into_iter().collect();
    provision_remote_node(&fixture, &grant);

    let request = enrollment_request(&grant);
    let request_path = write_json_fixture(&fixture.home_path, "enroll-request.json", &request);
    let grant_path = write_bytes_fixture(&fixture.home_path, "grant.bin", &grant_credential);
    let enroll_path =
        write_bytes_fixture(&fixture.home_path, "enroll.bin", &enrollment_credential);

    let enrolled = run_remote_enroll(&fixture, &grant_path, &enroll_path, &request_path);
    assert_eq!(
        enrolled.status.code(),
        Some(0),
        "remote enroll must succeed before failover refusal\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&enrolled.stdout),
        String::from_utf8_lossy(&enrolled.stderr)
    );

    let sent_at = now_micros();
    let failover_request = RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-cli-failover").unwrap(),
        brain_id,
        node_id,
        1,
        Some(
            serde_json::from_value::<RemoteWriterFenceV1>(json!({
                "brain_id": fixture.authority["brain_id"].as_str().expect("brain id"),
                "shard_id": "shard.remote-cli-failover",
                "generation_id": "generation.remote-cli-failover",
                "placement_revision": 1,
                "authority_epoch": 1,
                "authority_node_id": "node.remote-cli-previous-writer"
            }))
            .unwrap(),
        ),
        sent_at,
        PromotionConfirmationV1 {
            preview_id: "promotion.remote-cli-failover".to_owned(),
            expected_authority_epoch: 1,
            expected_placement_revision: 1,
            expires_at_micros: sent_at.0.saturating_add(60_000_000),
        },
    )
    .unwrap();
    let failover_request_path =
        write_json_fixture(&fixture.home_path, "failover-request.json", &failover_request);

    let output = tracedecay_command_with_home(&fixture.home_path)
        .current_dir(&fixture.project_path)
        .args([
            "remote",
            "failover",
            "--endpoint",
            &format!("https://{}/remote/", fixture.remote_endpoint),
            "--trust-root-file",
            &fixture.trust_root_path.to_string_lossy(),
            "--credential-file",
            &enroll_path.to_string_lossy(),
            "--request-file",
            &failover_request_path.to_string_lossy(),
            "--json",
        ])
        .output()
        .expect("tracedecay remote failover");

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let document: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "remote failover --json refusal must be one JSON document ({error}):\nstdout:\n{}\nstderr:\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let problem = &document["result"]["Err"]["problem"];
    assert_eq!(
        problem["code"],
        json!("remote.writer_authority_unpublished")
    );
    assert_eq!(problem["kind"], json!("unavailable"));
    assert_eq!(
        problem["message"],
        json!("No writer authority has been published for this Remote Brain")
    );
}

fn write_json_fixture(home: &Path, name: &str, value: &impl serde::Serialize) -> PathBuf {
    let path = home.join(name);
    let bytes = serde_json::to_vec(value).expect("serialize fixture");
    fs::write(&path, bytes).expect("write fixture");
    path
}

fn write_bytes_fixture(home: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = home.join(name);
    fs::write(&path, bytes).expect("write fixture");
    path
}

fn wait_for_remote_authority(child: &mut Child, path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        assert!(
            child.try_wait().unwrap().is_none(),
            "daemon exited during startup"
        );
        if let Ok(contents) = fs::read(path)
            && let Ok(value) = serde_json::from_slice::<Value>(&contents)
            && value["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
            && value["http_application_endpoint"].as_str().is_some()
            && value["remote_brain_tls_endpoint"].as_str().is_some()
        {
            return value;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "timed out waiting for remote daemon authority at {}",
        path.display()
    );
}

fn now_micros() -> UtcMicros {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
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
        grant_id: EntityId::new("grant.remote-cli-enroll").unwrap(),
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
            repository_id: RepositoryId::new("repository.remote-cli-enroll").unwrap(),
            worktree_id: WorktreeId::new("worktree.remote-cli-enroll").unwrap(),
            reference: Some(RefId::new("refs/heads/remote-cli-enroll").unwrap()),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-cli-enroll").unwrap(),
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
                "policy.remote-cli-enroll",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote-cli-enroll.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: now,
        },
        ActorId::new("actor.remote-cli-enroll").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(600_000_000))).unwrap(),
    )
    .unwrap()
}

fn enrollment_request(grant: &EnrollmentGrantV1) -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let sent_at = now_micros();
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.remote-cli-enroll").unwrap(),
        grant.brain_id.clone(),
        grant.node_id.clone(),
        sent_at,
        EnrollmentRequestV1 {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.revision,
            enrollment_id: EntityId::new("enrollment.remote-cli-enroll").unwrap(),
            brain_id: grant.brain_id.clone(),
            node_id: grant.node_id.clone(),
            expires_at: grant.expires_at,
            capabilities: grant.capabilities.clone(),
            scope: grant.scope.clone(),
        },
    )
    .unwrap()
}
