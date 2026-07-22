use std::fs;

use tempfile::TempDir;

use super::*;

#[test]
fn command_accepts_only_fixed_s11_gate_ids_and_exact_identities() {
    let fixture_sha = "a".repeat(64);
    let commit_sha = "b".repeat(40);
    let product_binary_sha = "c".repeat(64);
    let evidence_binary_sha = "d".repeat(64);
    let request = EvidenceCommand::parse([
        "--gate",
        MAINTENANCE_GATE_ID,
        "--fixture",
        "/tmp/fixture",
        "--output",
        "/tmp/evidence.json",
        "--fixture-sha256",
        fixture_sha.as_str(),
        "--product-commit-sha",
        commit_sha.as_str(),
        "--product-binary-sha256",
        product_binary_sha.as_str(),
        "--evidence-binary-sha256",
        evidence_binary_sha.as_str(),
    ])
    .unwrap();

    assert_eq!(request.gate, EvidenceGate::MaintenanceDoctor);
    assert_eq!(request.fixture_sha256, "a".repeat(64));
    assert_eq!(request.product_commit_sha, "b".repeat(40));
    assert_eq!(request.product_binary_sha256, "c".repeat(64));
    assert_eq!(request.evidence_binary_sha256, "d".repeat(64));

    let error = EvidenceCommand::parse([
        "--gate",
        "arbitrary-command",
        "--fixture",
        "/tmp/fixture",
        "--output",
        "/tmp/evidence.json",
        "--fixture-sha256",
        fixture_sha.as_str(),
        "--product-commit-sha",
        commit_sha.as_str(),
        "--product-binary-sha256",
        product_binary_sha.as_str(),
        "--evidence-binary-sha256",
        evidence_binary_sha.as_str(),
    ])
    .unwrap_err();
    assert!(error.to_string().contains("fixed S11 gate"));

    let error = EvidenceCommand::parse([
        "--gate",
        MAINTENANCE_GATE_ID,
        "--fixture",
        "/tmp/fixture",
        "--output",
        "/tmp/evidence.json",
        "--fixture-sha256",
        fixture_sha.as_str(),
        "--product-commit-sha",
        commit_sha.as_str(),
        "--product-binary-sha256",
        product_binary_sha.as_str(),
        "--evidence-binary-sha256",
        product_binary_sha.as_str(),
    ])
    .unwrap_err();
    assert!(error.to_string().contains("distinct artifacts"));
}

#[test]
fn fixture_fingerprint_matches_runner_canonical_tree_identity() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("a.txt"), b"alpha").unwrap();
    fs::write(temp.path().join("nested").join("b.txt"), b"beta").unwrap();

    let entries = vec![
        FingerprintEntry {
            path: "a.txt".to_owned(),
            sha256: sha256_hex(b"alpha"),
        },
        FingerprintEntry {
            path: "nested/b.txt".to_owned(),
            sha256: sha256_hex(b"beta"),
        },
    ];
    let expected = sha256_hex(serde_json::to_string(&entries).unwrap().as_bytes());

    assert_eq!(fingerprint_tree(temp.path()).unwrap(), expected);
}

#[test]
fn evidence_refuses_a_fixture_that_does_not_match_the_runner_identity() {
    let temp = TempDir::new().unwrap();
    let fixture = temp.path().join("fixture");
    fs::create_dir(&fixture).unwrap();
    fs::write(
        fixture.join(FIXTURE_MANIFEST),
        r#"{"schema_version":1,"project_root":"project","profile_root":"profile"}"#,
    )
    .unwrap();
    fs::create_dir(fixture.join("project")).unwrap();
    fs::create_dir(fixture.join("profile")).unwrap();
    let output_parent = temp.path().join("outside");
    fs::create_dir(&output_parent).unwrap();
    let output = output_parent.join("evidence.json");

    let error = execute(EvidenceCommand {
        gate: EvidenceGate::MaintenanceDoctor,
        fixture,
        output,
        fixture_sha256: "a".repeat(64),
        product_commit_sha: "b".repeat(40),
        product_binary_sha256: "c".repeat(64),
        evidence_binary_sha256: "d".repeat(64),
        crash_count: 0,
        restore_rehearsals: 0,
    })
    .unwrap_err();

    assert!(error.to_string().contains("fixture identity mismatch"));
}
