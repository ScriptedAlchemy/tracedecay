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
        "sha256:19a6706c6d360854597c6928ba4da2c35b6c86697628de92cd7775d032c9768d"
    );
    assert_eq!(
        payload["run_manifest_digest"],
        "sha256:98eecb3d79e9bd9b8ac125ecb0a9ea27f9c55372994a289649738168b4937e04"
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
        "authorized-store://search-quality/holdout/judgments-v1"
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
fn invalid_run_manifest_fails_before_opening_sealed_holdout_labels() {
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
fn owner_decision_command_rejects_non_terminal_outcomes_and_missing_digests() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner-decision.json");
    let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let draft = serde_json::json!({
        "schema_version": 1,
        "decision_kind": "owner_decision_v1",
        "authority": "owner_delegated_by_user_2026-07-22",
        "source_repository_commit": "01b0a0afe34c3342d6b5b076383f86ed8a8d0c66",
        "source_repository_tree": "3d8de57a843244229c3b19995c9d0b9e00081769",
        "corpus_digest": digest,
        "partition_digest": digest,
        "label_digest": digest,
        "profile_digest": digest,
        "toolchain_digest": digest,
        "hardware_digest": digest,
        "report_digest": digest,
        "evidence_index_digest": digest,
        "outcome": "blocked",
        "decided_by": "owner-search-quality-lead",
        "rationale": "should be rejected by validator",
        "gate_receipt_digests": [],
        "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    });
    fs::write(&path, serde_json::to_vec_pretty(&draft).unwrap()).unwrap();
    let output = run(&["owner-decision", "--input", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let payload = stdout_json(&output);
    assert_eq!(payload["command"], "owner_decision");
    assert_eq!(payload["status"], "invalid");
}
