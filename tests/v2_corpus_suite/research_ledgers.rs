#![allow(clippy::expect_used, clippy::unwrap_used)]

use jsonschema::Validator;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracedecay_domain::research::{
    EVIDENCE_CONSUMER_BINDING_SCHEMA_V1, EvidenceConsumerBindingV1, EvidenceLedgerReviewV1,
    EvidenceReviewStateV1, ManifestDigest,
};

mod yaml;

use yaml::parse_yaml;

const REVIEW_DATE: &str = "2026-07-13";
const PR14A_REVIEWED_LEDGER_DIGEST: &str =
    "sha256:98f27b9bfa70f5a64434e3bbf8ef98a0a7f5a290f1536704da388d3686be559e";
const PR36R_HOST_LEDGER_DIGEST: &str =
    "sha256:48a73634b047e2c86d2a79069761768736eb316b6c243f8b2e079e7489eaa607";
const PR14A_CONSUMER_ID: &str = "pr14a.native-semantic-benchmark";
const PR36R_CONSUMER_ID: &str = "pr36r.host-release-manifest";
const PR14A_SELECTED_ENTRY_IDS: &[&str] = &[
    "native-model-gte-large-q",
    "native-model-jina-code",
    "native-runtime-fastembed",
    "native-runtime-ort",
    "native-runtime-ort-sys",
];

struct LedgerSpec {
    name: &'static str,
    ledger: &'static str,
    schema: &'static str,
    digest: &'static str,
}

const LEDGERS: &[LedgerSpec] = &[
    LedgerSpec {
        name: "Hermes port",
        ledger: "docs/research/hermes-kanban-port-ledger.yaml",
        schema: "tests/fixtures/v2/hermes-port-ledger-schema.json",
        digest: "sha256:1f77a884648767c27d6c63120b1dc524e285ddd8fc122dd7da8286a62acb0b1d",
    },
    LedgerSpec {
        name: "host bundle evidence",
        ledger: "docs/research/host-bundle-evidence-ledger.yaml",
        schema: "tests/fixtures/v2/host-bundle-evidence-schema.json",
        digest: PR36R_HOST_LEDGER_DIGEST,
    },
    LedgerSpec {
        name: "native semantic evidence",
        ledger: "docs/research/native-semantic-model-evidence-ledger.yaml",
        schema: "tests/fixtures/v2/native-semantic-model-evidence-schema.json",
        digest: PR14A_REVIEWED_LEDGER_DIGEST,
    },
];

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    let path = repo_path(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn read_json(relative: &str) -> Value {
    serde_json::from_str(&read(relative))
        .unwrap_or_else(|error| panic!("failed to parse {relative}: {error}"))
}

fn ledger(spec: &LedgerSpec) -> Value {
    parse_yaml(&read(spec.ledger))
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", spec.ledger))
}

fn sha256(relative: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(read(relative).as_bytes()))
    )
}

fn validator(spec: &LedgerSpec) -> Validator {
    jsonschema::options()
        .should_validate_formats(true)
        .build(&read_json(spec.schema))
        .unwrap_or_else(|error| panic!("{} schema does not compile: {error}", spec.name))
}

fn schema_errors(validator: &Validator, value: &Value) -> Vec<String> {
    validator
        .iter_errors(value)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect()
}

fn entries<'a>(value: &'a Value, field: &str) -> Vec<&'a Map<String, Value>> {
    object_array(&value[field], field)
}

fn object_array<'a>(value: &'a Value, field: &str) -> Vec<&'a Map<String, Value>> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|entry| {
            entry
                .as_object()
                .unwrap_or_else(|| panic!("{field} item must be an object"))
        })
        .collect()
}

fn string<'a>(object: &'a Map<String, Value>, field: &str) -> &'a str {
    object[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} must be a string"))
}

fn strings<'a>(object: &'a Map<String, Value>, field: &str) -> Vec<&'a str> {
    object[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{field} item must be a string"))
        })
        .collect()
}

fn assert_unique<'a>(values: impl IntoIterator<Item = &'a str>, label: &str) {
    let mut seen = BTreeSet::new();
    for value in values {
        assert!(seen.insert(value), "duplicate {label}: {value}");
    }
}

fn is_hex_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn semantic_ledger() -> Value {
    ledger(&LEDGERS[2])
}

fn semantic_rows(value: &Value) -> Vec<&Map<String, Value>> {
    entries(value, "runtime")
        .into_iter()
        .chain(entries(value, "models"))
        .collect()
}

fn dependency_rows_are_reviewed(
    rows: &[&Map<String, Value>],
    reused_entry_ids: &BTreeSet<&str>,
    claimed_digest: &str,
    reviewed_digest: &str,
    actual_digest: &str,
) -> bool {
    let Ok(evidence_ledger_digest) = ManifestDigest::new(claimed_digest) else {
        return false;
    };
    let Ok(ledger_digest) = ManifestDigest::new(reviewed_digest) else {
        return false;
    };
    let Ok(actual_ledger_digest) = ManifestDigest::new(actual_digest) else {
        return false;
    };
    let binding = EvidenceConsumerBindingV1 {
        schema_version: EVIDENCE_CONSUMER_BINDING_SCHEMA_V1.into(),
        consumer_id: "test.dependent-reuse".into(),
        evidence_ledger_digest,
        selected_entry_ids: reused_entry_ids
            .iter()
            .map(|entry_id| (*entry_id).to_owned())
            .collect(),
    };
    let review = EvidenceLedgerReviewV1 {
        ledger_digest: ledger_digest.clone(),
        entries: rows
            .iter()
            .map(|row| {
                let state = match string(row, "review_state") {
                    "reviewed" => EvidenceReviewStateV1::Reviewed,
                    "blocked_provenance" => EvidenceReviewStateV1::BlockedProvenance,
                    _ => EvidenceReviewStateV1::ReviewRequired,
                };
                (string(row, "entry_id").to_owned(), state)
            })
            .collect(),
    };
    binding
        .validate_against("test.dependent-reuse", &actual_ledger_digest, &review)
        .is_ok()
}

fn evidence_binding(relative: &str) -> EvidenceConsumerBindingV1 {
    let binding: EvidenceConsumerBindingV1 = serde_json::from_str(&read(relative))
        .unwrap_or_else(|error| panic!("failed to parse {relative}: {error}"));
    binding
        .validate()
        .unwrap_or_else(|error| panic!("invalid evidence binding {relative}: {error}"));
    binding
}

fn semantic_review(value: &Value, digest: &str) -> EvidenceLedgerReviewV1 {
    let fresh = value["accessed_on"]
        .as_str()
        .is_some_and(|accessed_on| accessed_on >= REVIEW_DATE);
    EvidenceLedgerReviewV1 {
        ledger_digest: ManifestDigest::new(digest).unwrap(),
        entries: semantic_rows(value)
            .into_iter()
            .map(|row| {
                let state = if fresh && string(row, "review_state") == "reviewed" {
                    EvidenceReviewStateV1::Reviewed
                } else if string(row, "review_state") == "blocked_provenance" {
                    EvidenceReviewStateV1::BlockedProvenance
                } else {
                    EvidenceReviewStateV1::ReviewRequired
                };
                (string(row, "entry_id").to_owned(), state)
            })
            .collect(),
    }
}

fn host_review(value: &Value, digest: &str) -> EvidenceLedgerReviewV1 {
    EvidenceLedgerReviewV1 {
        ledger_digest: ManifestDigest::new(digest).unwrap(),
        entries: entries(value, "entries")
            .into_iter()
            .map(|row| {
                let review = row["review"].as_object().expect("review object");
                let current = string(row, "evidence_state") != "assumed"
                    && string(review, "expires_on") >= REVIEW_DATE;
                (
                    string(row, "entry_id").to_owned(),
                    if current {
                        EvidenceReviewStateV1::Reviewed
                    } else {
                        EvidenceReviewStateV1::ReviewRequired
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn committed_research_ledgers_match_strict_schemas() {
    for spec in LEDGERS {
        let errors = schema_errors(&validator(spec), &ledger(spec));
        assert!(
            errors.is_empty(),
            "{} violates {}:\n{}",
            spec.ledger,
            spec.schema,
            errors.join("\n")
        );
    }
}

#[test]
fn committed_research_ledger_schemas_reject_unknown_fields() {
    for spec in LEDGERS {
        let mut value = ledger(spec);
        value
            .as_object_mut()
            .expect("ledger root is an object")
            .insert("unknown_pr2a_field".into(), Value::Bool(true));
        assert!(
            !schema_errors(&validator(spec), &value).is_empty(),
            "{} schema accepted an unknown root field",
            spec.name
        );

        let mut value = ledger(spec);
        let rows = if value.get("entries").is_some() {
            value["entries"].as_array_mut().expect("entries array")
        } else {
            value["runtime"].as_array_mut().expect("runtime array")
        };
        rows[0]
            .as_object_mut()
            .expect("ledger row object")
            .insert("unknown_pr2a_row_field".into(), Value::Bool(true));
        assert!(
            !schema_errors(&validator(spec), &value).is_empty(),
            "{} schema accepted an unknown row field",
            spec.name
        );
    }
}

#[test]
fn committed_research_ledger_digests_are_frozen() {
    for spec in LEDGERS {
        assert_eq!(sha256(spec.ledger), spec.digest, "{} drifted", spec.name);
    }
}

#[test]
fn committed_research_ledgers_have_complete_unique_identities() {
    let hermes = ledger(&LEDGERS[0]);
    let hosts = ledger(&LEDGERS[1]);
    let semantic = semantic_ledger();

    assert_unique(
        entries(&hermes, "entries")
            .into_iter()
            .map(|row| string(row, "entry_id")),
        "Hermes entry_id",
    );
    assert_unique(
        entries(&hosts, "entries")
            .into_iter()
            .map(|row| string(row, "entry_id")),
        "host evidence entry_id",
    );
    assert_unique(
        semantic_rows(&semantic)
            .into_iter()
            .map(|row| string(row, "entry_id")),
        "semantic evidence entry_id",
    );

    let host_identities = entries(&hosts, "entries")
        .into_iter()
        .map(|row| {
            format!(
                "{}|{}|{}|{}",
                string(row, "host_profile"),
                string(row, "surface"),
                string(row, "host_version_range"),
                string(row, "capability_code")
            )
        })
        .collect::<Vec<_>>();
    assert_unique(
        host_identities.iter().map(String::as_str),
        "host capability identity",
    );
}

#[test]
fn native_semantic_provenance_is_complete() {
    let value = semantic_ledger();
    let runtime = entries(&value, "runtime");
    let models = entries(&value, "models");

    assert_eq!(
        runtime
            .iter()
            .map(|row| string(row, "crate"))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["fastembed", "ort", "ort-sys"])
    );
    assert_eq!(
        models
            .iter()
            .map(|row| string(row, "fastembed_enum"))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "BGERerankerV2M3",
            "GTELargeENV15Q",
            "JinaEmbeddingsV2BaseCode",
        ])
    );

    for model in models {
        let artifacts = object_array(&model["artifacts"], "artifacts");
        let kinds = artifacts
            .iter()
            .map(|artifact| string(artifact, "kind"))
            .collect::<BTreeSet<_>>();
        assert!(
            kinds.contains("model"),
            "{} lacks model bytes",
            string(model, "entry_id")
        );
        assert!(
            kinds.contains("tokenizer"),
            "{} lacks tokenizer bytes",
            string(model, "entry_id")
        );
        assert!(
            kinds.contains("config"),
            "{} lacks config bytes",
            string(model, "entry_id")
        );
        for artifact in artifacts {
            let source = (string(artifact, "repository"), string(artifact, "revision"));
            assert!(
                source
                    == (
                        string(model, "resolved_repository"),
                        string(model, "revision")
                    )
                    || source
                        == (
                            string(model, "upstream_repository"),
                            string(model, "upstream_revision")
                        )
            );
            assert!(string(artifact, "immutable_url").contains(string(artifact, "revision")));
        }
    }
}

#[test]
fn model_enum_is_not_artifact_identity() {
    let value = semantic_ledger();
    for model in entries(&value, "models") {
        assert_ne!(
            string(model, "fastembed_enum"),
            string(model, "resolved_repository")
        );
        for artifact in model["artifacts"].as_array().expect("artifacts array") {
            let artifact = artifact.as_object().expect("artifact object");
            assert!(!string(artifact, "path").is_empty());
            assert!(string(artifact, "digest").contains(':'));
        }
    }
}

#[test]
fn registry_mapping_matches_pinned_files() {
    let value = semantic_ledger();
    let runtime_version = entries(&value, "runtime")
        .into_iter()
        .find(|row| string(row, "crate") == "fastembed")
        .map(|row| string(row, "version"))
        .expect("fastembed runtime row");
    let mapping_prefix = format!("https://docs.rs/crate/fastembed/{runtime_version}/source/");

    for model in entries(&value, "models") {
        assert!(string(model, "fastembed_mapping").starts_with(&mapping_prefix));
        assert!(is_hex_revision(string(model, "revision")));
        assert!(
            !model["artifacts"]
                .as_array()
                .expect("artifacts array")
                .is_empty()
        );
    }
}

#[test]
fn all_model_and_runtime_bytes_have_license_disposition() {
    let value = semantic_ledger();
    for runtime in entries(&value, "runtime") {
        assert!(!string(runtime, "license").trim().is_empty());
        assert_eq!(string(runtime, "review_state"), "reviewed");
    }
    for model in entries(&value, "models") {
        let blocked = string(model, "review_state") == "blocked_provenance";
        let mut unresolved = false;
        for artifact in model["artifacts"].as_array().expect("artifacts array") {
            let artifact = artifact.as_object().expect("artifact object");
            unresolved |= string(artifact, "license_disposition") == "unresolved";
        }
        assert_eq!(unresolved, blocked);
        assert_eq!(
            model["promotion_blockers"]
                .as_array()
                .expect("promotion_blockers array")
                .is_empty(),
            !blocked
        );
    }
    assert_eq!(value["promotion_eligible"].as_bool(), Some(false));
}

#[test]
fn mutable_model_ref_is_rejected() {
    let value = semantic_ledger();
    for model in entries(&value, "models") {
        let revision = string(model, "revision");
        assert!(is_hex_revision(revision));
        assert!(is_hex_revision(string(model, "upstream_revision")));
        for anchor in strings(model, "retrieval_anchors") {
            assert!(anchor.contains("/tree/"));
            assert!(!anchor.ends_with("/main") && !anchor.ends_with("/master"));
        }
    }
}

#[test]
fn digest_or_revision_drift_blocks_promotion() {
    let value = semantic_ledger();
    assert_ne!(sha256(LEDGERS[2].ledger), "sha256:stale");
    for model in entries(&value, "models") {
        assert!(is_hex_revision(string(model, "revision")));
        assert!(is_hex_revision(string(model, "upstream_revision")));
    }
    assert_eq!(sha256(LEDGERS[2].ledger), PR14A_REVIEWED_LEDGER_DIGEST);
}

#[test]
fn pr14a_requires_reviewed_semantic_evidence() {
    let mut value = semantic_ledger();
    assert_eq!(sha256(LEDGERS[2].ledger), PR14A_REVIEWED_LEDGER_DIGEST);
    let binding = evidence_binding("tests/fixtures/v2/pr14a-native-semantic-evidence-binding.json");
    assert_eq!(binding.consumer_id, PR14A_CONSUMER_ID);
    assert_eq!(
        binding
            .selected_entry_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        PR14A_SELECTED_ENTRY_IDS
    );
    let actual_digest = ManifestDigest::new(sha256(LEDGERS[2].ledger)).unwrap();
    let review = semantic_review(&value, actual_digest.as_str());
    binding
        .validate_against(PR14A_CONSUMER_ID, &actual_digest, &review)
        .unwrap();

    let mut missing = review.clone();
    missing.entries.remove(PR14A_SELECTED_ENTRY_IDS[0]);
    assert!(
        binding
            .validate_against(PR14A_CONSUMER_ID, &actual_digest, &missing)
            .is_err()
    );

    let mut stale = binding.clone();
    stale.evidence_ledger_digest =
        ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
    assert!(
        stale
            .validate_against(PR14A_CONSUMER_ID, &actual_digest, &review)
            .is_err()
    );

    value["models"]
        .as_array_mut()
        .expect("models array")
        .iter_mut()
        .find(|row| row["entry_id"] == "native-model-jina-code")
        .expect("Jina row")["review_state"] = Value::String("review_required".into());
    assert!(
        binding
            .validate_against(
                PR14A_CONSUMER_ID,
                &actual_digest,
                &semantic_review(&value, PR14A_REVIEWED_LEDGER_DIGEST),
            )
            .is_err()
    );

    value["models"]
        .as_array_mut()
        .expect("models array")
        .iter_mut()
        .find(|row| row["entry_id"] == "native-model-jina-code")
        .expect("Jina row")["review_state"] = Value::String("reviewed".into());
    value["accessed_on"] = Value::String("2026-07-12".into());
    assert!(
        binding
            .validate_against(
                PR14A_CONSUMER_ID,
                &actual_digest,
                &semantic_review(&value, PR14A_REVIEWED_LEDGER_DIGEST),
            )
            .is_err()
    );

    let bge = entries(&value, "models")
        .into_iter()
        .find(|row| string(row, "entry_id") == "native-model-bge-reranker-v2-m3")
        .expect("BGE evidence row");
    assert_eq!(string(bge, "review_state"), "blocked_provenance");
    assert_eq!(value["promotion_eligible"].as_bool(), Some(false));
}

#[test]
fn release_manifest_evidence_ledger_digest_matches() {
    assert_eq!(sha256(LEDGERS[1].ledger), PR36R_HOST_LEDGER_DIGEST);
    let binding = evidence_binding("tests/fixtures/v2/pr36r-host-release-evidence-binding.json");
    assert_eq!(binding.consumer_id, PR36R_CONSUMER_ID);
    let mut value = ledger(&LEDGERS[1]);
    let mut expected_entry_ids = entries(&value, "entries")
        .into_iter()
        .map(|row| string(row, "entry_id").to_owned())
        .collect::<Vec<_>>();
    expected_entry_ids.sort();
    assert_eq!(binding.selected_entry_ids, expected_entry_ids);

    let actual_digest = ManifestDigest::new(sha256(LEDGERS[1].ledger)).unwrap();
    let review = host_review(&value, actual_digest.as_str());
    binding
        .validate_against(PR36R_CONSUMER_ID, &actual_digest, &review)
        .unwrap();

    let mut missing = review.clone();
    missing.entries.remove(&binding.selected_entry_ids[0]);
    assert!(
        binding
            .validate_against(PR36R_CONSUMER_ID, &actual_digest, &missing)
            .is_err()
    );

    let mut stale = binding.clone();
    stale.evidence_ledger_digest =
        ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
    assert!(
        stale
            .validate_against(PR36R_CONSUMER_ID, &actual_digest, &review)
            .is_err()
    );

    value["entries"].as_array_mut().expect("entries array")[0]["evidence_state"] =
        Value::String("assumed".into());
    assert!(
        binding
            .validate_against(
                PR36R_CONSUMER_ID,
                &actual_digest,
                &host_review(&value, PR36R_HOST_LEDGER_DIGEST),
            )
            .is_err()
    );

    value["entries"].as_array_mut().expect("entries array")[0]["evidence_state"] =
        Value::String("documented".into());
    value["entries"].as_array_mut().expect("entries array")[0]["review"]["expires_on"] =
        Value::String("2026-07-12".into());
    assert!(
        binding
            .validate_against(
                PR36R_CONSUMER_ID,
                &actual_digest,
                &host_review(&value, PR36R_HOST_LEDGER_DIGEST),
            )
            .is_err()
    );
}

#[test]
fn hermes_selected_spans_have_source_tests_and_review() {
    let value = ledger(&LEDGERS[0]);
    assert_eq!(
        value["source"]["commit"].as_str(),
        Some("732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69")
    );
    for row in entries(&value, "entries") {
        let id = string(row, "entry_id");
        for span in object_array(&row["source_spans"], "source_spans") {
            let start = span["start_line"].as_u64().expect("source start_line");
            let end = span["end_line"].as_u64().expect("source end_line");
            assert!(start <= end, "{id} has an invalid source span");
            assert!(is_hex_revision(
                string(span, "digest").trim_start_matches("git-sha1:")
            ));
        }
        for span in object_array(&row["source_tests"], "source_tests") {
            let start = span["start_line"].as_u64().expect("test start_line");
            let end = span["end_line"].as_u64().expect("test end_line");
            assert!(start <= end, "{id} has an invalid source-test span");
            assert!(is_hex_revision(
                string(span, "digest").trim_start_matches("git-sha1:")
            ));
        }
        assert!(
            !strings(row, "v2_regressions").is_empty(),
            "{id} lacks a V2 regression"
        );
        if string(row, "review_state") == "review_required" {
            assert!(
                row.get("review_blocker").and_then(Value::as_str).is_some(),
                "{id} must explain why dependent reuse remains blocked"
            );
        }
    }

    let rows = entries(&value, "entries");
    let actual_digest = sha256(LEDGERS[0].ledger);
    assert!(!dependency_rows_are_reviewed(
        &rows,
        &BTreeSet::new(),
        LEDGERS[0].digest,
        LEDGERS[0].digest,
        &actual_digest,
    ));
    assert!(!dependency_rows_are_reviewed(
        &rows,
        &BTreeSet::from(["hermes-port-kanban-db"]),
        LEDGERS[0].digest,
        LEDGERS[0].digest,
        &actual_digest,
    ));
    assert!(!dependency_rows_are_reviewed(
        &rows,
        &BTreeSet::from(["hermes-port-kanban-swarm"]),
        "sha256:stale-hermes-evidence",
        LEDGERS[0].digest,
        &actual_digest,
    ));
    assert!(!dependency_rows_are_reviewed(
        &rows,
        &BTreeSet::from(["hermes-port-kanban-swarm"]),
        LEDGERS[0].digest,
        LEDGERS[0].digest,
        &format!("sha256:{}", "0".repeat(64)),
    ));
    assert!(dependency_rows_are_reviewed(
        &rows,
        &BTreeSet::from(["hermes-port-kanban-swarm"]),
        LEDGERS[0].digest,
        LEDGERS[0].digest,
        &actual_digest,
    ));
}

#[test]
fn host_capability_matrix_is_complete_and_immutable() {
    let value = ledger(&LEDGERS[1]);
    let expected = BTreeMap::from([
        (
            "claude-code",
            BTreeSet::from([
                "agent",
                "command",
                "component",
                "hook",
                "mcp",
                "release",
                "rule",
                "skill",
            ]),
        ),
        (
            "codex",
            BTreeSet::from([
                "agent",
                "command",
                "component",
                "hook",
                "mcp",
                "release",
                "rule",
                "skill",
            ]),
        ),
        (
            "cursor",
            BTreeSet::from([
                "agent",
                "command",
                "component",
                "hook",
                "mcp",
                "release",
                "rule",
                "skill",
            ]),
        ),
    ]);
    let mut actual: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();

    for row in entries(&value, "entries") {
        let host = string(row, "host_profile");
        actual
            .entry(host)
            .or_default()
            .insert(string(row, "surface"));
        assert_ne!(string(row, "evidence_state"), "assumed");
        let review = row["review"].as_object().expect("review object");
        assert!(
            string(review, "expires_on") >= REVIEW_DATE,
            "expired row: {}",
            string(row, "entry_id")
        );

        let source = row["source"].as_object().expect("source object");
        if string(source, "kind") == "official_repository_schema" {
            let commit = string(source, "repository_commit");
            assert!(is_hex_revision(commit));
            assert!(string(source, "canonical_url").contains(commit));
            assert!(!string(source, "repository_path").is_empty());
            assert!(
                source
                    .get("content_digest")
                    .and_then(Value::as_str)
                    .is_some()
            );
        }
    }
    assert_eq!(actual, expected);
}
