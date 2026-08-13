#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, ORIGIN};
use serde_json::Value;
use tempfile::TempDir;
use tracedecay_sdk::application::{APPLICATION_REQUEST_ID_HEADER, RequestId};
use tracedecay_sdk::client::{
    Client, ClientError, ConnectionMode, OperationRequestOptions, TypedResponse,
};
use tracedecay_sdk::operations::ApplicationFactStoreCurate;

const VALID_REQUEST_ID: &str = "request.sdk.semantic-replay";

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
fn public_curator_client_replays_one_durable_effect_and_rejects_foreign_identities() {
    let scratch = TempDir::new().expect("temporary SDK journey root");
    let home = scratch.path().join("home");
    let profile = home.join(".tracedecay");
    let project = scratch.path().join("project");
    initialize_project(&home, &profile, &project);

    let binary = production_binary();
    let (daemon, authority) = spawn_daemon(&binary, &home, &profile, &project, None);
    let project_id = project_id(&binary, &home, &profile, &project);
    let client = sdk_client(&authority, &project_id);

    let request =
        tracedecay_sdk::application::retained_surfaces::FactStoreCurateRequestV1::default();
    let request_id = RequestId::new(VALID_REQUEST_ID).expect("canonical replay identity");

    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 0);
    let accepted = execute_curate(&client, &request, &request_id)
        .unwrap_or_else(|error| panic!("valid public curate request must succeed: {error}"));
    assert_eq!(accepted.request_id, VALID_REQUEST_ID);
    assert_eq!(accepted.result.run_id.as_str(), VALID_REQUEST_ID);
    assert!(accepted.result.matches_terminal());
    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 1);

    let first_daemon_epoch = authority["epoch"].as_u64().expect("daemon authority epoch");
    drop(client);
    drop(daemon);
    let (_restarted_daemon, authority) =
        spawn_daemon(&binary, &home, &profile, &project, Some(first_daemon_epoch));
    let endpoint = format!(
        "http://{}",
        authority["http_application_endpoint"]
            .as_str()
            .expect("HTTP application endpoint")
    );
    let token = authority["auth_token"]
        .as_str()
        .expect("daemon authorization token");
    let client = sdk_client(&authority, &project_id);

    let replay = execute_curate(&client, &request, &request_id)
        .unwrap_or_else(|error| panic!("same-identity replay must return its terminal: {error}"));
    assert_eq!(
        replay, accepted,
        "durable replay must return the exact settled response"
    );
    assert_eq!(
        application_run_record_count(&profile, VALID_REQUEST_ID),
        1,
        "replay must not execute the curator twice"
    );

    let mut different_request = request.clone();
    different_request.fact_review_limit += 1;
    let conflict = execute_curate(&client, &different_request, &request_id)
        .expect_err("one replay identity cannot name two request bodies");
    let ClientError::Problem(problem) = conflict else {
        panic!("request-body collision must remain a typed problem: {conflict}");
    };
    assert_eq!(problem.status, 409);
    assert_eq!(problem.kind, "conflict");
    assert_eq!(problem.envelope["request_id"], VALID_REQUEST_ID);
    assert_eq!(problem.envelope["problem"]["terminality"], "pre_admission");
    assert_eq!(
        problem.envelope["problem"]["committed_receipt"],
        Value::Null
    );
    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 1);

    let raw = reqwest::blocking::Client::new();
    let curate_route = "/retained/fact_store_curate";

    let mut malformed = HeaderMap::new();
    malformed.insert(APPLICATION_REQUEST_ID_HEADER, HeaderValue::from_static(""));
    let malformed =
        raw_application_request(&raw, &endpoint, &project_id, token, curate_route, malformed);
    assert_zero_effect_rejection(&malformed, &[""]);
    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 1);

    let mut duplicate = HeaderMap::new();
    duplicate.append(
        APPLICATION_REQUEST_ID_HEADER,
        HeaderValue::from_static(VALID_REQUEST_ID),
    );
    duplicate.append(
        APPLICATION_REQUEST_ID_HEADER,
        HeaderValue::from_static("request.sdk.semantic-replay.other"),
    );
    let duplicate =
        raw_application_request(&raw, &endpoint, &project_id, token, curate_route, duplicate);
    assert_zero_effect_rejection(
        &duplicate,
        &[VALID_REQUEST_ID, "request.sdk.semantic-replay.other"],
    );
    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 1);

    let mut disallowed = HeaderMap::new();
    disallowed.insert(
        APPLICATION_REQUEST_ID_HEADER,
        HeaderValue::from_static(VALID_REQUEST_ID),
    );
    let disallowed = raw_application_request(
        &raw,
        &endpoint,
        &project_id,
        token,
        "/workflow/list-definitions",
        disallowed,
    );
    assert_zero_effect_rejection(&disallowed, &[VALID_REQUEST_ID]);
    assert_eq!(
        application_run_record_count(&profile, VALID_REQUEST_ID),
        1,
        "malformed, duplicate, and disallowed identities must have zero effect"
    );

    let final_replay = execute_curate(&client, &request, &request_id)
        .unwrap_or_else(|error| panic!("rejected traffic must not disturb replay: {error}"));
    assert_eq!(final_replay, accepted);
    assert_eq!(application_run_record_count(&profile, VALID_REQUEST_ID), 1);
}

fn spawn_daemon(
    binary: &Path,
    home: &Path,
    profile: &Path,
    project: &Path,
    prior_epoch: Option<u64>,
) -> (Daemon, Value) {
    let socket = profile.join("daemon.sock");
    let mut command = Command::new(binary);
    command
        .args(["daemon", "run", "--socket"])
        .arg(&socket)
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    isolated(&mut command, home, profile);
    let mut daemon = Daemon {
        child: command.spawn().expect("spawn production daemon"),
    };
    let authority = wait_for_authority(
        &mut daemon.child,
        &profile.join("daemon-authority.json"),
        prior_epoch,
    );
    (daemon, authority)
}

fn sdk_client(authority: &Value, project_id: &str) -> Client {
    let endpoint = format!(
        "http://{}",
        authority["http_application_endpoint"]
            .as_str()
            .expect("HTTP application endpoint")
    );
    let token = authority["auth_token"]
        .as_str()
        .expect("daemon authorization token");
    Client::builder(ConnectionMode::local(&endpoint, project_id, token))
        .origin(
            reqwest::Url::parse(&endpoint)
                .expect("daemon endpoint URL")
                .origin()
                .ascii_serialization(),
        )
        .build()
        .expect("public SDK client")
}

fn execute_curate(
    client: &Client,
    request: &tracedecay_sdk::application::retained_surfaces::FactStoreCurateRequestV1,
    request_id: &RequestId,
) -> Result<
    TypedResponse<tracedecay_sdk::application::retained_surfaces::AutomationRunResultV1>,
    ClientError,
> {
    client.execute_with_options::<ApplicationFactStoreCurate>(
        request,
        OperationRequestOptions {
            request_id: Some(request_id.clone()),
            ..OperationRequestOptions::default()
        },
    )
}

fn raw_application_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    project_id: &str,
    token: &str,
    route: &str,
    headers: HeaderMap,
) -> (u16, Value) {
    let response = client
        .post(format!(
            "{endpoint}/projects/{project_id}/application{route}"
        ))
        .bearer_auth(token)
        .header(ORIGIN, endpoint)
        .header(CONTENT_TYPE, "application/json")
        .headers(headers)
        .body("{}")
        .send()
        .expect("raw production-daemon request");
    let status = response.status().as_u16();
    let body = response.json().expect("canonical JSON problem");
    (status, body)
}

fn assert_zero_effect_rejection(response: &(u16, Value), rejected_request_ids: &[&str]) {
    let (status, body) = response;
    assert_eq!(*status, 400);
    assert_eq!(body["kind"], "problem");
    assert_eq!(body["value"]["problem"]["kind"], "invalid_request");
    assert_eq!(body["value"]["problem"]["terminality"], "pre_admission");
    assert_eq!(body["value"]["problem"]["committed_receipt"], Value::Null);
    let minted_request_id = body["value"]["request_id"]
        .as_str()
        .expect("server-owned rejection identity");
    RequestId::new(minted_request_id).expect("canonical server-owned rejection identity");
    assert!(
        !rejected_request_ids.contains(&minted_request_id),
        "a rejected caller identity must not become the effect identity"
    );
    assert_eq!(
        body["value"]["problem"]["request_id"], minted_request_id,
        "the rejection problem must be bound to its server-owned identity"
    );
}

fn initialize_project(home: &Path, profile: &Path, project: &Path) {
    fs::create_dir_all(home).expect("create isolated home");
    fs::create_dir_all(project.join("src")).expect("create project source root");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"sdk-replay-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    )
    .expect("write project manifest");
    fs::write(
        project.join("src/lib.rs"),
        "pub const SDK_REPLAY_FIXTURE: bool = true;\n",
    )
    .expect("write project source");
    run(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(project));

    let binary = production_binary();
    let mut init = Command::new(binary);
    init.arg("init").current_dir(project);
    isolated(&mut init, home, profile);
    run(&mut init);
}

fn project_id(binary: &Path, home: &Path, profile: &Path, project: &Path) -> String {
    let mut context = Command::new(binary);
    context
        .args(["projects", "context"])
        .arg(project)
        .arg("--json")
        .current_dir(project);
    isolated(&mut context, home, profile);
    let payload: Value = serde_json::from_slice(&run(&mut context)).expect("project context JSON");
    payload["project"]["project_id"]
        .as_str()
        .expect("registered project identity")
        .to_owned()
}

fn application_run_record_count(profile: &Path, run_id: &str) -> usize {
    let projects = profile.join("projects");
    let Ok(entries) = fs::read_dir(projects) else {
        return 0;
    };
    entries
        .map(|entry| {
            entry
                .expect("project store directory")
                .path()
                .join("dashboard/automation_runs.jsonl")
        })
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|contents| {
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    serde_json::from_str::<Value>(line).expect("complete automation ledger record")
                })
                .collect::<Vec<_>>()
        })
        .filter(|record| {
            record["run_id"] == run_id
                && record["trigger"] == "application"
                && record["task"] == "memory_curator"
        })
        .count()
}

fn production_binary() -> PathBuf {
    let path = std::env::var_os("TRACEDECAY_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../../target/debug/tracedecay"));
    fs::canonicalize(&path)
        .unwrap_or_else(|error| panic!("missing production daemon {}: {error}", path.display()))
}

fn isolated(command: &mut Command, home: &Path, profile: &Path) {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TRACEDECAY_DATA_DIR", profile)
        .env("TRACEDECAY_GLOBAL_DB", profile.join("global.db"))
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1");
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().expect("run subprocess");
    assert!(
        output.status.success(),
        "command failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn wait_for_authority(child: &mut Child, path: &Path, prior_epoch: Option<u64>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        assert!(
            child.try_wait().expect("daemon process status").is_none(),
            "production daemon exited during startup"
        );
        if let Ok(contents) = fs::read(path)
            && let Ok(value) = serde_json::from_slice::<Value>(&contents)
            && value["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
            && value["http_application_endpoint"].as_str().is_some()
            && prior_epoch.is_none_or(|epoch| {
                value["epoch"]
                    .as_u64()
                    .is_some_and(|current| current > epoch)
            })
        {
            return value;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "timed out waiting for daemon authority at {}",
        path.display()
    );
}
