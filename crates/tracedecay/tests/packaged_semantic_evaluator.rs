use std::process::Command;

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

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay-search-eval-direct"))
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

    let output = Command::new(env!("CARGO_BIN_EXE_tracedecay-search-eval-direct"))
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
