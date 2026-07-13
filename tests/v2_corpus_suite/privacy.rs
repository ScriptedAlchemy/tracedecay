#![allow(clippy::expect_used, clippy::unwrap_used)]

use jsonschema::Validator;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const PRIVACY: &str = "tests/fixtures/v2/privacy";
const HOST_FIXTURES: &[&str] = &[
    "host-canonical-sources.json",
    "host-component-archives.json",
    "host-hook-stdin.json",
    "host-marketplace-artifacts.json",
    "host-owned-config-backups.json",
    "host-probe-diagnostics.json",
    "host-rendered-trees.json",
];

fn repo_path(relative: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_bytes(relative: impl AsRef<Path>) -> Vec<u8> {
    let path = repo_path(relative);
    fs::read(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn read_json(relative: impl AsRef<Path>) -> Value {
    let path = relative.as_ref();
    serde_json::from_slice(&read_bytes(path))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn privacy_json(file: &str) -> Value {
    read_json(format!("{PRIVACY}/{file}"))
}

fn objects<'a>(value: &'a Value, field: &str) -> Vec<&'a serde_json::Map<String, Value>> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|item| {
            item.as_object()
                .unwrap_or_else(|| panic!("{field} item must be an object"))
        })
        .collect()
}

fn strings(value: &Value, field: &str) -> BTreeSet<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{field} item must be a string"))
                .into()
        })
        .collect()
}

fn field<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> &'a str {
    object[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} must be a string"))
}

fn assert_unique(objects: &[&serde_json::Map<String, Value>], field_name: &str) {
    let mut seen = BTreeSet::new();
    for object in objects {
        let value = field(object, field_name);
        assert!(seen.insert(value), "duplicate {field_name}: {value}");
    }
}

fn looks_secret(value: &str) -> bool {
    let aws = value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|part| {
            part.len() == 20
                && part.starts_with("AKIA")
                && part
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        });
    aws || value.contains("ghp_000000000000000000000000000000000000")
        || value.contains("sk-test-000000000000000000000000000000000000000000000000")
        || value.contains(concat!(
            "xoxb-",
            "000000000000-000000000000-000000000000000000000000"
        ))
        || value.contains("-----BEGIN PRIVATE KEY-----")
        || (value.contains("://fixture_user:fixture_password@") && value.contains(".invalid"))
}

fn assert_content_free(value: &Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                assert!(
                    !matches!(
                        key.as_str(),
                        "value"
                            | "candidate"
                            | "snippet"
                            | "content"
                            | "candidate_sha256"
                            | "candidate_digest"
                            | "fingerprint"
                    ),
                    "content-bearing field is forbidden: {key}"
                );
                assert_content_free(value);
            }
        }
        Value::Array(values) => values.iter().for_each(assert_content_free),
        Value::String(value) => assert!(
            !looks_secret(value),
            "metadata contains a synthetic secret canary"
        ),
        _ => {}
    }
}

fn manifest() -> Value {
    privacy_json("privacy-manifest.json")
}

#[test]
fn privacy_manifest_is_complete_and_hashes_are_deterministic() {
    let schema = privacy_json("privacy-manifest.schema.json");
    let manifest = manifest();
    let validator = Validator::new(&schema).expect("privacy manifest schema compiles");
    let errors = validator
        .iter_errors(&manifest)
        .map(|e| e.to_string())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "privacy manifest schema failures:\n{}",
        errors.join("\n")
    );

    let fixtures = objects(&manifest, "fixtures");
    assert_unique(&fixtures, "fixture_id");
    for fixture in fixtures {
        let relative = field(fixture, "relative_path");
        let actual = hex::encode(Sha256::digest(read_bytes(relative)));
        assert_eq!(
            field(fixture, "sha256"),
            actual,
            "stale digest for {relative}"
        );
    }

    for collection in ["surfaces", "occurrences"] {
        let entries = objects(&manifest, collection);
        assert_unique(
            &entries,
            if collection == "surfaces" {
                "surface_id"
            } else {
                "occurrence_id"
            },
        );
    }
}

#[test]
fn privacy_manifest_is_registered_by_the_v2_corpus() {
    let root = read_json("tests/fixtures/v2/manifest.json");
    let children = objects(&root, "child_manifests");
    let privacy = children
        .iter()
        .find(|entry| field(entry, "path") == "privacy/privacy-manifest.json")
        .expect("privacy child manifest is registered");
    let actual = hex::encode(Sha256::digest(read_bytes(
        "tests/fixtures/v2/privacy/privacy-manifest.json",
    )));
    assert_eq!(field(privacy, "sha256"), actual);
}

#[test]
fn privacy_positive_invalid_corpus_covers_required_classes() {
    let corpus = privacy_json("positive-invalid.json");
    let cases = objects(&corpus, "cases");
    assert_unique(&cases, "id");
    let actual: BTreeSet<String> = cases
        .iter()
        .map(|case| field(case, "detector_class").to_owned())
        .collect();
    let expected: BTreeSet<String> = [
        "aws_access_key_id",
        "database_url_credentials",
        "github_personal_access_token",
        "openai_api_key",
        "private_key_pem",
        "slack_bot_token",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(actual, expected);
    for case in cases {
        assert_eq!(case["expected_detection"], true);
        assert!(looks_secret(field(case, "value")));
        assert!(!field(case, "credential_validity").is_empty());
    }
}

#[test]
fn privacy_negative_corpus_has_no_builtin_findings() {
    let corpus = privacy_json("negative-realistic.json");
    let cases = objects(&corpus, "cases");
    assert_unique(&cases, "id");
    assert!(cases.len() >= 6);
    for case in cases {
        assert_eq!(case["expected_detection"], false);
        assert!(
            !looks_secret(field(case, "value")),
            "unexpected finding in {}",
            field(case, "id")
        );
    }
}

#[test]
fn privacy_serialized_fields_are_scanned_independently() {
    let corpus = privacy_json("serialized-field-boundary.json");
    let cases = objects(&corpus, "cases");
    assert_unique(&cases, "id");
    for case in cases {
        let document: Value =
            serde_json::from_str(field(case, "serialized")).expect("serialized case is JSON");
        let sensitive = field(case, "sensitive_json_pointer");
        assert_eq!(sensitive, field(case, "expected_redacted_json_pointer"));
        let leaf = document
            .pointer(sensitive)
            .unwrap_or_else(|| panic!("missing JSON pointer {sensitive}"));
        assert!(looks_secret(
            leaf.as_str().expect("sensitive field is a string")
        ));

        let mut scrubbed = document;
        *scrubbed
            .pointer_mut(sensitive)
            .expect("sensitive pointer is mutable") = Value::String("[REDACTED]".into());
        assert!(
            !looks_secret(&scrubbed.to_string()),
            "candidate crossed a serialized-field boundary"
        );
    }
}

#[test]
fn privacy_sink_inventory_covers_v1_v2_and_forbidden_sinks() {
    let inventory = privacy_json("v1-v2-sink-inventory.json");
    let sinks = objects(&inventory, "sinks");
    assert_unique(&sinks, "id");
    assert!(sinks.len() >= 25);
    for sink in sinks {
        assert!(!field(sink, "v2_boundary").is_empty());
        assert!(!strings(&Value::Object((*sink).clone()), "v1_sources").is_empty());
        assert!(
            field(sink, "required_state").contains("sanitized")
                || field(sink, "required_state").contains("content-free")
                || field(sink, "required_state").contains("protected")
                || field(sink, "required_state").contains("classified")
                || field(sink, "required_state").contains("rescanned")
        );
    }

    let canary_corpus = privacy_json("forbidden-sink-canaries.json");
    let canaries = objects(&canary_corpus, "cases");
    assert_unique(&canaries, "id");
    let positive_corpus = privacy_json("positive-invalid.json");
    let positive: BTreeMap<_, _> = objects(&positive_corpus, "cases")
        .into_iter()
        .map(|case| (field(case, "detector_class"), field(case, "value")))
        .collect();
    for canary in canaries {
        assert_eq!(
            positive.get(field(canary, "detector_class")).copied(),
            Some(field(canary, "value"))
        );
        assert!(!strings(&Value::Object((*canary).clone()), "forbidden_sinks").is_empty());
    }
}

#[test]
fn privacy_historical_regressions_are_anchored() {
    let history = privacy_json("historical-regressions.json");
    assert_eq!(
        history["content_policy"],
        "metadata-only; evidence descriptions contain no candidate content or candidate digests"
    );
    let regressions = objects(&history, "regressions");
    assert_unique(&regressions, "id");
    assert!(regressions.len() >= 15);
    for regression in regressions {
        assert_ne!(
            regression.contains_key("evidence_anchor"),
            regression.contains_key("source_anchor")
        );
        assert!(!field(regression, "failure_class").is_empty());
        assert!(!field(regression, "required_assertion").is_empty());
    }
    assert_content_free(&history);
}

#[test]
fn privacy_host_surfaces_have_independent_receipts() {
    let mut surfaces = BTreeSet::new();
    let mut receipts = BTreeSet::new();
    for file in HOST_FIXTURES {
        let host = privacy_json(file);
        assert_eq!(host["schema_version"], 1);
        assert_eq!(host["metadata_only"], true);
        assert!(surfaces.insert(host["surface_id"].as_str().unwrap().to_owned()));
        let receipt = host["receipt_ref"].as_str().unwrap().to_owned();
        assert!(receipts.insert(receipt.clone()));
        assert_eq!(
            receipt,
            format!(
                "target/v2-privacy/receipts/hosts/{}.json",
                host["surface_id"].as_str().unwrap()
            )
        );
        assert_eq!(
            strings(&host, "coverage"),
            ["classification", "discovery", "receipt-binding"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_content_free(&host);
    }
    assert_eq!(surfaces.len(), 7);
}

#[test]
fn privacy_legacy_fixture_replacements_preserve_detector_coverage() {
    let manifest = manifest();
    let occurrences = objects(&manifest, "occurrences");
    assert_eq!(occurrences.len(), 9);
    assert_unique(&occurrences, "occurrence_id");
    let expected: BTreeSet<_> = (1..=9).map(|n| format!("PR2B-LEGACY-{n:03}")).collect();
    let actual: BTreeSet<String> = occurrences
        .iter()
        .map(|item| field(item, "occurrence_id").to_owned())
        .collect();
    assert_eq!(actual, expected);
    for occurrence in occurrences {
        assert_eq!(occurrence["source_class"], "legacy-fixture-reference");
        assert_eq!(occurrence["coverage_state"], "covered");
        assert!(!field(occurrence, "symbol_anchor").is_empty());
        assert!(
            occurrence["detector_rule_ref"]["rule_id"] == "generic-api-key"
                || occurrence["detector_rule_ref"]["rule_id"] == "private-key"
        );
    }
}

#[test]
fn privacy_scanner_receipts_pin_gitleaks_8_30_1_and_detect_secrets_1_5_0() {
    let manifest = manifest();
    let receipts = objects(&manifest, "receipt_contracts");
    assert_unique(&receipts, "receipt_id");
    let expected = [
        (
            "gitleaks",
            "8.30.1",
            "target/v2-privacy/receipts/gitleaks-8.30.1.json",
        ),
        (
            "detect-secrets",
            "1.5.0",
            "target/v2-privacy/receipts/detect-secrets-1.5.0.json",
        ),
    ];
    for (detector, version, path) in expected {
        let receipt = receipts
            .iter()
            .find(|item| field(item, "detector_id") == detector)
            .unwrap_or_else(|| panic!("missing {detector} receipt"));
        assert_eq!(field(receipt, "detector_version"), version);
        assert_eq!(field(receipt, "relative_path"), path);
        let required = strings(&Value::Object((**receipt).clone()), "required_fields");
        for name in [
            "tool_version",
            "config_digest",
            "reviewed_base_commit",
            "candidate_commit",
            "scanned_surface_ids",
            "coverage_state",
            "finding_count",
            "artifact_digest",
        ] {
            assert!(required.contains(name), "{detector} receipt omits {name}");
        }
        let forbidden = strings(&Value::Object((**receipt).clone()), "forbidden_fields");
        for name in [
            "candidate",
            "candidate_digest",
            "candidate_fingerprint",
            "content",
            "snippet",
        ] {
            assert!(
                forbidden.contains(name),
                "{detector} receipt permits {name}"
            );
        }
    }
}

#[test]
fn privacy_repository_and_generated_derivatives_are_zero_finding() {
    let manifest = manifest();
    assert_eq!(
        manifest["content_policy"],
        "metadata-only-no-candidate-content"
    );
    assert_content_free(&manifest);

    let surfaces = objects(&manifest, "surfaces");
    let classes: BTreeSet<_> = surfaces
        .iter()
        .map(|item| field(item, "source_class"))
        .collect();
    for required in [
        "repository-candidate",
        "generated-derivative",
        "host-surface-metadata",
    ] {
        assert!(
            classes.contains(required),
            "missing scanned surface class {required}"
        );
    }
    for surface in surfaces {
        assert_ne!(surface["coverage_state"], "unknown");
        assert!(!strings(&Value::Object((*surface).clone()), "receipt_refs").is_empty());
        assert!(!surface["detector_rule_refs"].as_array().unwrap().is_empty());
    }
}
