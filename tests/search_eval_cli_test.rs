#![allow(clippy::option_env_unwrap)] // compile-time env probe in test
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tracedecay_domain::{EvidenceIndexV1, RunManifestV1};

fn evaluator_bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_tracedecay-search-eval")
        .expect("Cargo must build the canonical tracedecay-search-eval binary")
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search_quality")
}

fn run(args: &[&str]) -> Output {
    Command::new(evaluator_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}):\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn validate_reports_frozen_fixture_and_run_digests_without_outputs() {
    let output = run(&["validate", "--fixtures", fixture_root().to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "validate");
    assert_eq!(payload["status"], "valid");
    assert_eq!(payload["authority"], "contract_only");
    assert_eq!(
        payload["fixture_manifest_digest"],
        "sha256:5da56cc98447ff962e62ad0cb2757e0c4936f514dfa82265c5402bcc2b56a4f4"
    );
    assert_eq!(
        payload["run_manifest_digest"],
        "sha256:3db745fc343cbfc00e9bbb173979474bdd3d324e5f5bf87c65f2357e803f3fe1"
    );
    assert_eq!(
        payload["holdout_seal_digest"],
        "sha256:09c282aa525296def61e1ac5d7d8b98201296fd3c67acafc9fb531c51ea0b9de"
    );
}

#[test]
fn compare_requires_accepted_but_writes_an_immutable_blocked_report() {
    let output_root = tempfile::tempdir().unwrap();
    let output = run(&[
        "compare",
        "--fixtures",
        fixture_root().to_str().unwrap(),
        "--output-root",
        output_root.path().to_str().unwrap(),
        "--require-outcome",
        "accepted",
    ]);
    assert_eq!(output.status.code(), Some(3));
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "compare");
    assert_eq!(payload["outcome"], "blocked");
    assert_eq!(payload["required_outcome"], "accepted");
    assert_eq!(payload["requirement_satisfied"], false);
    assert_eq!(
        payload["blocked_on"][0]["locator"],
        "direct-filesystem://search-quality/holdout/judgments-v1"
    );
    assert_eq!(
        payload["blocked_on"][0]["digest"],
        "sha256:09c282aa525296def61e1ac5d7d8b98201296fd3c67acafc9fb531c51ea0b9de"
    );

    let report_path = PathBuf::from(payload["report_path"].as_str().unwrap());
    let evidence_index_path = PathBuf::from(payload["evidence_index_path"].as_str().unwrap());
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let evidence_index: Value =
        serde_json::from_slice(&fs::read(&evidence_index_path).unwrap()).unwrap();
    assert_eq!(report["outcome"], "blocked");
    assert_eq!(report["no_access_before_lock"], true);
    assert_eq!(report["run_revision"], 1);
    assert_eq!(evidence_index["authority"], "contract_only");
    assert!(
        evidence_index["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["acceptance_authority"] == false)
    );
    serde_json::from_value::<EvidenceIndexV1>(evidence_index)
        .unwrap()
        .verify_digest()
        .unwrap();
    assert!(
        !report_path
            .parent()
            .unwrap()
            .join("promotion-v1.json")
            .exists()
    );

    let original_report = fs::read(&report_path).unwrap();
    let repeated = run(&[
        "compare",
        "--fixtures",
        fixture_root().to_str().unwrap(),
        "--output-root",
        output_root.path().to_str().unwrap(),
        "--require-outcome",
        "accepted",
    ]);
    assert_eq!(repeated.status.code(), Some(2));
    assert_eq!(stdout_json(&repeated)["outcome"], "invalid_run");
    assert_eq!(fs::read(report_path).unwrap(), original_report);

    let mut revision_two: RunManifestV1 =
        serde_json::from_slice(&fs::read(fixture_root().join("run-contract-v1.json")).unwrap())
            .unwrap();
    revision_two.revision = 2;
    revision_two.candidate_revision = "contract-only-append-only-revision-2".to_string();
    revision_two.digest = revision_two.compute_digest().unwrap();
    let revision_two_path = output_root.path().join("run-v2.json");
    fs::write(
        &revision_two_path,
        serde_json::to_vec_pretty(&revision_two).unwrap(),
    )
    .unwrap();
    let next = run(&[
        "compare",
        "--fixtures",
        fixture_root().to_str().unwrap(),
        "--run-manifest",
        revision_two_path.to_str().unwrap(),
        "--output-root",
        output_root.path().to_str().unwrap(),
        "--require-outcome",
        "accepted",
    ]);
    assert_eq!(next.status.code(), Some(3));
    let next_payload = stdout_json(&next);
    assert_eq!(next_payload["outcome"], "blocked");
    let next_report: Value =
        serde_json::from_slice(&fs::read(next_payload["report_path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(next_report["run_revision"], 2);
    assert_eq!(
        next_report["supersedes_report_digest"],
        report["report_digest"]
    );
}

#[test]
fn invalid_run_manifest_fails_before_opening_direct_holdout_labels() {
    let temp = tempfile::tempdir().unwrap();
    let tampered_run = temp.path().join("run-tampered.json");
    let mut run_manifest: Value =
        serde_json::from_slice(&fs::read(fixture_root().join("run-contract-v1.json")).unwrap())
            .unwrap();
    run_manifest["candidate_revision"] = Value::String("changed-after-freeze".to_string());
    fs::write(
        &tampered_run,
        serde_json::to_vec_pretty(&run_manifest).unwrap(),
    )
    .unwrap();
    let output_root = temp.path().join("outputs");

    let output = run(&[
        "compare",
        "--fixtures",
        fixture_root().to_str().unwrap(),
        "--run-manifest",
        tampered_run.to_str().unwrap(),
        "--output-root",
        output_root.to_str().unwrap(),
        "--require-outcome",
        "accepted",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let payload = stdout_json(&output);
    assert_eq!(payload["outcome"], "invalid_run");
    assert!(
        payload["rationale"]
            .as_str()
            .unwrap()
            .contains("run manifest")
    );
}

#[test]
fn cli_help_exposes_no_packet_seal_or_owner_arguments() {
    let root = run(&["--help"]);
    assert!(
        root.status.success(),
        "{}",
        String::from_utf8_lossy(&root.stderr)
    );
    let help = String::from_utf8_lossy(&root.stdout);
    for banned in [
        "  packet ",
        "\n  packet",
        "  seal ",
        "\n  seal",
        "owner-decision",
        "holdout-accessed-by",
        "holdout-profile-root",
        "--owner ",
        "blinded-packet",
    ] {
        assert!(
            !help.to_ascii_lowercase().contains(&banned.to_ascii_lowercase()),
            "CLI help unexpectedly exposes {banned}:\n{help}"
        );
    }
    assert!(
        help.contains("generate-candidates"),
        "expected generate-candidates in help:\n{help}"
    );

    let compare = run(&["compare", "--help"]);
    assert!(
        compare.status.success(),
        "{}",
        String::from_utf8_lossy(&compare.stderr)
    );
    let compare_help = String::from_utf8_lossy(&compare.stdout);
    for banned in [
        "owner-decision",
        "holdout-accessed-by",
        "holdout-profile-root",
        "--owner ",
        "blinded-packet",
        "  seal ",
        "  packet ",
    ] {
        assert!(
            !compare_help
                .to_ascii_lowercase()
                .contains(&banned.to_ascii_lowercase()),
            "compare help unexpectedly exposes {banned}:\n{compare_help}"
        );
    }
    assert!(
        compare_help.contains("holdout-labels"),
        "expected holdout-labels in compare help:\n{compare_help}"
    );
}
