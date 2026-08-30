use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a search-eval package binary without pulling `tests/common`
/// (that module requires `test-helpers`, which this default-feature suite
/// does not enable).
fn search_eval_direct_bin() -> PathBuf {
    const NAME: &str = "tracedecay-search-eval-direct";
    const OVERRIDE: &str = "TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN";
    let binary = std::env::var_os(OVERRIDE)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let test_executable =
                std::env::current_exe().expect("test executable path should resolve");
            let profile_dir = test_executable
                .parent()
                .and_then(Path::parent)
                .expect("integration test should run from a Cargo profile directory");
            profile_dir.join(format!("{NAME}{}", std::env::consts::EXE_SUFFIX))
        });
    assert!(
        binary.is_file(),
        "search-eval binary `{NAME}` is missing at {}; build it with `kache cargo -- build -p tracedecay-search-eval --bin {NAME}` or set {OVERRIDE}",
        binary.display()
    );
    binary
}

#[test]
fn packaged_evaluator_binary_validates_without_a_source_checkout() {
    let project = tempfile::tempdir().expect("unrelated temporary project");
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\n",
    )
    .expect("unrelated project content");
    assert!(!project.path().join(".git").exists());
    assert!(
        !project
            .path()
            .join("tests/fixtures/search_quality")
            .exists()
    );

    let output = Command::new(search_eval_direct_bin())
        .current_dir(project.path())
        .arg("validate")
        .arg("--repo-root")
        .arg(project.path())
        .output()
        .expect("run packaged evaluator binary");
    assert!(
        output.status.success(),
        "packaged evaluator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("typed evaluator JSON");
    assert_eq!(response["command"], "validate");
    assert_eq!(response["status"], "pass");
    assert_eq!(response["query_count"], 28);
    assert_eq!(response["profile_count"], 3);
}

#[test]
fn qualify_native_rejects_an_unparseable_candidate_without_writing_output() {
    let project = tempfile::tempdir().expect("temporary project");
    let candidate = project.path().join("candidate.json");
    let output_path = project.path().join("qualification.json");
    std::fs::write(&candidate, b"not semantic candidate JSON").expect("invalid candidate");

    let output = Command::new(search_eval_direct_bin())
        .current_dir(project.path())
        .args(["qualify-native", "--project-root"])
        .arg(project.path())
        .arg("--candidate")
        .arg(&candidate)
        .arg("--output")
        .arg(&output_path)
        .output()
        .expect("run packaged evaluator binary");

    assert_eq!(output.status.code(), Some(2));
    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("typed evaluator JSON");
    assert_eq!(response["command"], "qualify_native");
    assert_eq!(response["status"], "fail");
    assert!(
        response["rationale"]
            .as_str()
            .is_some_and(|rationale| rationale.contains("parse"))
    );
    assert!(
        !output_path.exists(),
        "candidate parsing failure must not create a qualification artifact"
    );
}
