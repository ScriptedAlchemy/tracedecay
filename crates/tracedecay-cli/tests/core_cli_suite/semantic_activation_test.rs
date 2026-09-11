use std::cell::RefCell;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::configuration::ConfigurationSetRequestV1;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationValueV1,
    SEMANTIC_RUNTIME_SETTING_KEY, SettingKey,
};
use tracedecay_semantic_contracts::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticModelLifecycleStateV1,
    SemanticModelLifecycleStatusV1,
};

use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

use crate::common::{self, TestChildProcess, canonical_existing_path};

const PROFILE_ID: &str = "hybrid-conservative";
const PROBE_SYMBOL: &str = "semantic_cli_activation_probe";
const CLI_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

fn git(project: &Path, args: &[&str]) {
    let output = Command::new(common::git_program())
        .args(args)
        .current_dir(project)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn initialize_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        format!("pub fn {PROBE_SYMBOL}() -> &'static str {{ \"semantic cli activation\" }}\n"),
    )
    .expect("source fixture");
    git(project, &["init", "--quiet", "--initial-branch=main"]);
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed semantic CLI activation journey",
        ],
    );
}

// Import genuine catalog members into the profile's shared verified artifact store
// before the daemon starts. Project selection and readiness belong to the daemon.
fn install_profile_semantic_fixture(home: &Path, fixture_root: &Path) -> String {
    common::create_runtime().block_on(async {
        let profile = home.join(".tracedecay");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile)
            .expect("isolated profile root");
        let lifecycle_root = tracedecay_semantic::default_lifecycle_root_in(&profile);
        let identity = profile_identity::load_or_create(&profile)
            .expect("canonical isolated profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("isolated profile runtime registry");
        let owner = registry
            .profile_semantic_lifecycle()
            .await
            .expect("canonical profile artifact owner");
        let model = owner
            .catalog()
            .get(DEFAULT_FASTEMBED_MODEL_ID)
            .expect("production catalog contains default model");
        let repository_root = lifecycle_root
            .join("hf-hub-cache")
            .join(format!("models--{}", model.model_code.replace('/', "--")));
        let snapshot = repository_root
            .join("snapshots")
            .join(&model.source.revision);
        for member in model.members.values() {
            let destination = snapshot.join(&member.upstream_path);
            std::fs::create_dir_all(destination.parent().expect("model member parent"))
                .expect("cached model member directory");
            std::fs::copy(fixture_root.join(&member.path), destination)
                .expect("copy byte-pinned model member");
        }
        let reference = repository_root.join("refs").join(&model.source.revision);
        std::fs::create_dir_all(reference.parent().expect("revision reference parent"))
            .expect("revision reference directory");
        std::fs::write(reference, &model.source.revision).expect("revision reference");
        owner
            .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
            .expect("select production semantic model");
        owner
            .acquire_blocking_for_tests()
            .expect("install verified distribution fixture");
        let digest = match owner.status().state.expect("installed model state") {
            SemanticModelLifecycleStateV1::Installed {
                artifact_digest, ..
            }
            | SemanticModelLifecycleStateV1::Ready {
                artifact_digest, ..
            } => artifact_digest,
            state => panic!("expected installed production model, got {state:?}"),
        };
        drop(owner);
        registry
            .shutdown_terminal_tasks()
            .await
            .expect("join profile import workers");
        registry
            .close_retained_graph_runtimes_for_shutdown()
            .await
            .expect("close profile import runtime");
        digest
    })
}

fn select_project_semantic_model(binary: &Path, home: &Path, project: &Path) {
    let observed = configuration_tool(binary, home, project, "configuration_observed_state", "{}");
    let project_id = serde_json::from_value(observed["scope"]["project_id"].clone())
        .expect("registered configuration project identity");
    let expected_revision = serde_json::from_value(
        observed["outcome"]["value"]["payload"][0]["desired_revision_id"].clone(),
    )
    .expect("current project configuration revision");
    let request = ConfigurationSetRequestV1 {
        layer: ConfigurationLayerIdV1::Project { project_id },
        key: SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY).expect("semantic runtime setting key"),
        value: ConfigurationValueV1::Text(
            serde_json::to_string(&SemanticConfig {
                selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
                ..SemanticConfig::default()
            })
            .expect("semantic runtime configuration"),
        ),
        expected_revision,
        idempotency_key: ConfigurationIdempotencyKey::new(
            "configuration.idempotency.cli-semantic-model".to_owned(),
        )
        .expect("semantic configuration idempotency key"),
    };
    let committed = configuration_tool(
        binary,
        home,
        project,
        "configuration_set",
        &serde_json::to_string(&request).expect("semantic model selection request"),
    );
    assert_eq!(
        committed["scope"]["project_id"],
        observed["scope"]["project_id"]
    );
    assert_eq!(committed["outcome"]["outcome"], "effect", "{committed}");
}

fn wait_for_project_semantic_material(
    binary: &Path,
    home: &Path,
    project: &Path,
    artifact_digest: &str,
) {
    let last_observation = RefCell::new(Value::Null);
    common::poll_until(
        Instant::now() + Duration::from_secs(60),
        Duration::from_millis(100),
        || {
            let output = tool(binary, home, project, "runtime", r#"{"format":"json"}"#);
            if !output.status.success() {
                *last_observation.borrow_mut() = json!({
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                });
                return None;
            }
            let runtime = tool_payload(&output);
            let lifecycle: Option<SemanticModelLifecycleStatusV1> = serde_json::from_value(
                runtime
                    .get("semantic_model")
                    .expect("runtime must expose canonical project model lifecycle")
                    .clone(),
            )
            .expect("canonical project model lifecycle status");
            // These are activation's exact material prerequisites. Serving
            // state can already describe a vector generation without a profile
            // receipt, and cannot stand in for the selected model's lifecycle.
            let installed = lifecycle.is_some_and(|status| {
                status.selected_model.as_deref() == Some(DEFAULT_FASTEMBED_MODEL_ID)
                    && matches!(status.state,
                        Some(SemanticModelLifecycleStateV1::Installed {
                            model_id, artifact_digest: installed_digest, ..
                        } | SemanticModelLifecycleStateV1::Ready {
                            model_id, artifact_digest: installed_digest, ..
                        }) if model_id == DEFAULT_FASTEMBED_MODEL_ID
                            && installed_digest == artifact_digest
                    )
            });
            *last_observation.borrow_mut() = json!({
                "semantic_model": runtime["semantic_model"],
                "semantic_runtime": runtime["semantic_runtime"],
            });
            installed.then_some(())
        },
        || {
            format!(
                "project {} did not admit the verified semantic material: {}",
                project.display(),
                last_observation.borrow(),
            )
        },
    );
}

fn run_cli(binary: &Path, home: &Path, project: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(binary);
    common::apply_tracedecay_home_env(&mut command, home);
    command
        .current_dir(project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = TestChildProcess::new(command.spawn().expect("tracedecay CLI should spawn"));
    child
        .wait_with_output(CLI_COMMAND_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!("tracedecay CLI {args:?} did not finish within {CLI_COMMAND_TIMEOUT:?}: {error}")
        })
}

fn tool_payload(output: &Output) -> Value {
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "tool stdout was not JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let text = envelope["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool response had no text payload: {envelope}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool text was not JSON: {error}; text={text}"))
}

fn tool(binary: &Path, home: &Path, project: &Path, name: &str, arguments: &str) -> Output {
    let project_arg = project.to_string_lossy();
    run_cli(
        binary,
        home,
        project,
        &[
            "tool",
            "--project",
            project_arg.as_ref(),
            name,
            "--json",
            "--args",
            arguments,
        ],
    )
}

fn configuration_tool(
    binary: &Path,
    home: &Path,
    project: &Path,
    name: &str,
    arguments: &str,
) -> Value {
    let output = tool(binary, home, project, name, arguments);
    assert!(
        output.status.success(),
        "{name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).expect("typed configuration response")
}

#[test]
#[ignore = "requires the byte-pinned FastEmbed distribution fixture"]
fn shipped_cli_activates_a_published_profile_for_strict_semantic_search() {
    let fixture_root = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(std::path::PathBuf::from)
        .expect("distribution acceptance must provide TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE");
    assert!(
        fixture_root.is_dir(),
        "distribution FastEmbed fixture is not a directory: {}",
        fixture_root.display()
    );
    let binary = std::env::var_os("TRACEDECAY_TEST_BIN")
        .map(std::path::PathBuf::from)
        .expect("distribution acceptance must provide TRACEDECAY_TEST_BIN");
    assert!(
        binary.is_file(),
        "packaged tracedecay binary is not a file: {}",
        binary.display()
    );
    let home = TempDir::new().expect("isolated home");
    let project = TempDir::new().expect("isolated project");
    let home = canonical_existing_path(home.path());
    let project = canonical_existing_path(project.path());
    initialize_project(&project);
    let artifact_digest = install_profile_semantic_fixture(&home, &fixture_root);
    let _daemon = common::spawn_tracedecay_daemon_from(&home, &binary);
    let initialization = run_cli(&binary, &home, &project, &["init"]);
    assert!(
        initialization.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&initialization.stdout),
        String::from_utf8_lossy(&initialization.stderr)
    );
    select_project_semantic_model(&binary, &home, &project);
    wait_for_project_semantic_material(&binary, &home, &project, &artifact_digest);

    let unavailable = tool(
        &binary,
        &home,
        &project,
        "search",
        &json!({
            "query": PROBE_SYMBOL,
            "limit": 10,
            "format": "json",
            "semantic_mode": "strict_semantic",
        })
        .to_string(),
    );
    assert!(!unavailable.status.success());
    assert_eq!(
        tool_payload(&unavailable)["semantic"]["status"],
        "unavailable"
    );
    let initial_runtime = tool(&binary, &home, &project, "runtime", r#"{"format":"json"}"#);
    assert!(
        initial_runtime.status.success(),
        "tool runtime failed before activation\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&initial_runtime.stdout),
        String::from_utf8_lossy(&initial_runtime.stderr)
    );
    let initial_runtime = tool_payload(&initial_runtime);
    assert_ne!(
        initial_runtime["semantic_runtime"]["state"]["state"], "ready",
        "fresh runtime must not claim semantic serving readiness: {initial_runtime}"
    );

    let project_arg = project.to_string_lossy();
    let activation = run_cli(
        &binary,
        &home,
        &project,
        &[
            "semantic",
            "activate",
            "--profile",
            PROFILE_ID,
            "--project",
            project_arg.as_ref(),
            "--json",
        ],
    );
    assert!(
        activation.status.success(),
        "semantic activate failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&activation.stdout),
        String::from_utf8_lossy(&activation.stderr)
    );
    let receipt: Value = serde_json::from_slice(&activation.stdout)
        .expect("semantic activate --json must emit one receipt");
    assert!(
        receipt["profile_digest"]
            .as_str()
            .is_some_and(|v| v.starts_with("sha256:"))
    );
    assert!(
        receipt["report_digest"]
            .as_str()
            .is_some_and(|v| v.starts_with("sha256:"))
    );
    assert!(
        receipt["configuration_revision"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    assert_eq!(receipt["rollback_profile_id"], Value::Null);
    assert!(
        receipt["runtime_state"]["state"].is_string(),
        "activation receipt must carry a typed runtime state: {receipt}"
    );

    let ready_runtime = common::poll_until(
        Instant::now() + Duration::from_secs(60),
        Duration::from_millis(100),
        || {
            let runtime = tool(&binary, &home, &project, "runtime", r#"{"format":"json"}"#);
            if !runtime.status.success() {
                return None;
            }
            let payload = tool_payload(&runtime);
            (payload["semantic_runtime"]["state"]["state"] == "ready").then_some(payload)
        },
        || "activated semantic runtime did not publish typed ready state".to_owned(),
    );
    assert!(
        ready_runtime["semantic_runtime"]["state"]["receipt"]["activated_generation"]
            .as_str()
            .is_some_and(|generation| generation.starts_with("sha256:")),
        "ready runtime must receipt its active vector generation: {ready_runtime}"
    );

    let strict = tool(
        &binary,
        &home,
        &project,
        "search",
        &json!({
            "query": PROBE_SYMBOL,
            "limit": 10,
            "format": "json",
            "semantic_mode": "strict_semantic",
        })
        .to_string(),
    );
    assert!(strict.status.success());
    let strict = tool_payload(&strict);
    assert_eq!(strict["semantic"]["status"], "complete");
    let probe = strict["results"]
        .as_array()
        .and_then(|results| {
            results
                .iter()
                .find(|result| result["display"]["name"] == PROBE_SYMBOL)
        })
        .expect("strict search must return the probe");
    assert!(
        probe["candidate"]["contributions"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["retriever"] == "semantic"))
    );
}
