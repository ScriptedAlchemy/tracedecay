use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_semantic::SemanticModelLifecycleOwnerV1;
use tracedecay_semantic_contracts::{DEFAULT_FASTEMBED_MODEL_ID, SemanticModelLifecycleStateV1};

use crate::common::{self, canonical_existing_path, tracedecay_command_with_home};

const PROFILE_ID: &str = "hybrid-conservative";
const PROBE_SYMBOL: &str = "semantic_cli_activation_probe";

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

fn seed_distribution_fixture(
    lifecycle_root: &Path,
    fixture_root: &Path,
    owner: &SemanticModelLifecycleOwnerV1,
) {
    let model = owner
        .catalog()
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .expect("production catalog contains the default model");
    let repository = format!("models--{}", model.model_code.replace('/', "--"));
    let repository_root = lifecycle_root.join("hf-hub-cache").join(repository);
    let snapshot = repository_root
        .join("snapshots")
        .join(&model.source.revision);
    for member in model.members.values() {
        let destination = snapshot.join(&member.upstream_path);
        std::fs::create_dir_all(destination.parent().expect("model member parent"))
            .expect("cached model member directory");
        std::fs::copy(fixture_root.join(&member.path), &destination)
            .expect("copy byte-pinned model member");
    }
    let reference = repository_root.join("refs").join(&model.source.revision);
    std::fs::create_dir_all(reference.parent().expect("revision reference parent"))
        .expect("revision reference directory");
    std::fs::write(reference, &model.source.revision).expect("revision reference");
}

fn install_semantic_fixture(home: &Path, fixture_root: &Path) {
    let profile = home.join(".tracedecay");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile)
        .expect("isolated profile root");
    let lifecycle_root = tracedecay_semantic::default_lifecycle_root_in(&profile);
    let owner = SemanticModelLifecycleOwnerV1::open_default(&lifecycle_root)
        .expect("isolated semantic lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, fixture_root, &owner);
    owner
        .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select production semantic model");
    owner
        .acquire_blocking_for_tests()
        .expect("install verified distribution fixture");
    assert!(
        matches!(
            owner.status().state,
            Some(
                SemanticModelLifecycleStateV1::Installed { .. }
                    | SemanticModelLifecycleStateV1::Ready { .. }
            )
        ),
        "byte-pinned semantic model must be installed before the daemon starts"
    );
}

fn wait_for_semantic_model_ready(home: &Path) {
    let lifecycle_root = tracedecay_semantic::default_lifecycle_root_in(&home.join(".tracedecay"));
    common::poll_until(
        Instant::now() + Duration::from_secs(60),
        Duration::from_millis(100),
        || {
            let owner = SemanticModelLifecycleOwnerV1::open_default(&lifecycle_root).ok()?;
            matches!(
                owner.status().state,
                Some(SemanticModelLifecycleStateV1::Ready { .. })
            )
            .then_some(())
        },
        || "production daemon did not load the verified semantic model".to_owned(),
    );
}

fn run_cli(home: &Path, project: &Path, args: &[&str]) -> Output {
    tracedecay_command_with_home(home)
        .current_dir(project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("tracedecay CLI should run")
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

fn strict_search(home: &Path, project: &Path) -> Output {
    let project_arg = project.to_string_lossy();
    let arguments = json!({
        "query": PROBE_SYMBOL,
        "limit": 10,
        "format": "json",
        "semantic_mode": "strict_semantic",
    })
    .to_string();
    run_cli(
        home,
        project,
        &[
            "tool",
            "--project",
            project_arg.as_ref(),
            "search",
            "--json",
            "--args",
            &arguments,
        ],
    )
}

fn assert_semantic_contribution(payload: &Value) {
    assert_eq!(
        payload["semantic"]["status"], "complete",
        "strict search must report completed semantic execution: {payload}"
    );
    let results = payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("strict search returned no result list: {payload}"));
    let probe = results
        .iter()
        .find(|result| result["display"]["name"] == PROBE_SYMBOL)
        .unwrap_or_else(|| panic!("strict search did not return {PROBE_SYMBOL}: {payload}"));
    let contributions = probe["candidate"]["contributions"]
        .as_array()
        .unwrap_or_else(|| panic!("probe had no contribution list: {probe}"));
    assert!(
        contributions
            .iter()
            .any(|contribution| contribution["retriever"] == "semantic"),
        "strict result must include a semantic candidate contribution: {probe}"
    );
}

#[test]
#[ignore = "requires TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE from distribution acceptance"]
fn shipped_cli_activates_a_published_profile_for_strict_semantic_search() {
    let fixture_root = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_dir())
        .expect("byte-pinned FastEmbed distribution fixture");
    let home = TempDir::new().expect("isolated home");
    let project = TempDir::new().expect("isolated project");
    let home = canonical_existing_path(home.path());
    let project = canonical_existing_path(project.path());

    initialize_project(&project);
    install_semantic_fixture(&home, &fixture_root);
    common::initialize_tracedecay_cli_project(&home, &project);
    wait_for_semantic_model_ready(&home);

    let unavailable = strict_search(&home, &project);
    assert!(
        !unavailable.status.success(),
        "fresh strict search must refuse unavailable semantic serving"
    );
    let unavailable_payload = tool_payload(&unavailable);
    assert_eq!(unavailable_payload["semantic"]["status"], "unavailable");

    let project_arg = project.to_string_lossy();
    let activation = run_cli(
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
        .expect("semantic activate --json must emit one JSON receipt");
    assert!(
        receipt["profile_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "activation must receipt the published profile: {receipt}"
    );
    assert!(
        receipt["report_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "activation must receipt the published evaluation report: {receipt}"
    );
    assert!(
        receipt["configuration_revision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty()),
        "activation must receipt the configuration compare-and-swap: {receipt}"
    );
    assert_eq!(
        receipt["rollback_profile_id"],
        Value::Null,
        "a fresh activation has no prior profile to retain"
    );

    let strict = common::poll_until(
        Instant::now() + Duration::from_secs(60),
        Duration::from_millis(100),
        || {
            let output = strict_search(&home, &project);
            if !output.status.success() {
                return None;
            }
            let payload = tool_payload(&output);
            (payload["semantic"]["status"] == "complete").then_some(payload)
        },
        || "activated semantic runtime did not become ready for strict search".to_owned(),
    );
    assert_semantic_contribution(&strict);
}
