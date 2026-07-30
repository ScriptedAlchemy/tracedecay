#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_sdk::client::{
    CancellationStatus, Client, ClientError, ConnectionMode, RequestOptions, StreamOptions,
    StreamResume,
};
use tracedecay_sdk::operations::{TypedOperation, WorkSnapshot};

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
fn installed_rust_client_requires_work_snapshot_and_exact_lifecycle_capability() {
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
    let mut init = Command::new(&binary);
    init.arg("init").current_dir(&project);
    isolated(&mut init, &home, &profile);
    run(&mut init);

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

    for mode in [ConnectionMode::local(&endpoint, project_id, token)] {
        let client = Client::builder(mode)
            .origin(
                reqwest::Url::parse(&endpoint)
                    .unwrap()
                    .origin()
                    .ascii_serialization(),
            )
            .build()
            .unwrap();
        let request = serde_json::from_value::<<WorkSnapshot as TypedOperation>::Request>(
            json!({"page_size": 1}),
        )
        .unwrap();
        let request_id = client
            .execute::<WorkSnapshot>(&request, RequestOptions)
            .unwrap_or_else(|error| panic!("WorkSnapshot must succeed: {error}"))
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
        match client.cancel_operation(&request_id, None) {
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
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn wait_for_authority(child: &mut Child, path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
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
