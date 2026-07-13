use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v2")
}

fn manifest() -> Value {
    serde_json::from_str(&fs::read_to_string(corpus_root().join("manifest.json")).unwrap()).unwrap()
}

fn files_under(root: &Path) -> impl Iterator<Item = walkdir::DirEntry> + '_ {
    WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
}

#[test]
fn manifest_is_complete_and_hashes_are_deterministic() {
    let manifest = manifest();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(
        manifest["content_policy"],
        "synthetic-or-minimally-redacted"
    );

    let required_coverage: BTreeSet<_> = [
        "subagent",
        "tool",
        "reasoning-summary",
        "goal",
        "git",
        "rewrite",
        "truncation",
        "malformed",
        "partial",
        "unicode",
        "missing-timestamp",
        "secret-placeholder",
    ]
    .into_iter()
    .collect();
    let mut observed_coverage = BTreeSet::new();
    let mut observed_providers = BTreeSet::new();
    let files = manifest["files"].as_array().expect("files array");
    assert!(!files.is_empty());

    for entry in files {
        let relative = entry["path"].as_str().expect("fixture path");
        assert!(relative.starts_with("providers/"));
        let provider = entry["provider_family"]
            .as_str()
            .expect("manifest provider_family");
        let path_provider = Path::new(relative)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("provider fixture file stem");
        assert_eq!(
            provider, path_provider,
            "provider/path mismatch: {relative}"
        );
        assert!(
            observed_providers.insert(provider),
            "duplicate provider manifest entry: {provider}"
        );

        let bytes = fs::read(corpus_root().join(relative)).unwrap();
        let actual = hex::encode(Sha256::digest(&bytes));
        assert_eq!(
            entry["sha256"].as_str().unwrap(),
            actual,
            "hash mismatch: {relative}"
        );
        for tag in entry["coverage"].as_array().expect("coverage array") {
            observed_coverage.insert(tag.as_str().unwrap());
        }
    }
    let provider_families = manifest["provider_families"]
        .as_array()
        .expect("provider_families array");
    let declared_providers: BTreeSet<_> = provider_families
        .iter()
        .map(|provider| provider.as_str().expect("provider family string"))
        .collect();
    assert_eq!(provider_families.len(), declared_providers.len());
    assert_eq!(observed_providers, declared_providers);
    assert!(
        required_coverage.is_subset(&observed_coverage),
        "missing coverage: {:?}",
        required_coverage.difference(&observed_coverage)
    );
}

#[test]
fn privacy_child_manifest_is_registered_and_current() {
    let manifest = manifest();
    let privacy = manifest["child_manifests"]
        .as_array()
        .expect("child_manifests array")
        .iter()
        .find(|entry| entry["path"] == "privacy/privacy-manifest.json")
        .expect("privacy child manifest is registered");
    let bytes = fs::read(corpus_root().join("privacy/privacy-manifest.json")).unwrap();
    let actual = hex::encode(Sha256::digest(&bytes));
    assert_eq!(privacy["sha256"].as_str().unwrap(), actual);
}

#[test]
fn every_provider_fixture_is_manifested_and_synthetic() {
    let manifest = manifest();
    let root = corpus_root();
    let expected_providers: BTreeSet<_> = [
        "antigravity",
        "claude",
        "cline",
        "codex",
        "copilot",
        "cursor",
        "gemini",
        "hermes",
        "kilo",
        "kimi",
        "kiro",
        "opencode",
        "roo-code",
        "vibe",
        "zed",
    ]
    .into_iter()
    .collect();
    let declared_providers: BTreeSet<_> = manifest["provider_families"]
        .as_array()
        .unwrap()
        .iter()
        .map(|provider| provider.as_str().unwrap())
        .collect();
    assert_eq!(declared_providers, expected_providers);
    let declared: BTreeSet<_> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_owned())
        .collect();
    let providers_root = root.join("providers");
    let actual: BTreeSet<_> = files_under(&providers_root)
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(declared, actual);

    let canonical_record_families: BTreeSet<_> = [
        "assistant_event",
        "message",
        "tool_result",
        "malformed_input",
        "partial_input",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let authoritative_record_families: BTreeSet<_> = canonical_record_families
        .iter()
        .cloned()
        .chain(std::iter::once("credential_positive_control".to_owned()))
        .collect();
    let mut observed_corpus_record_families = BTreeSet::new();

    for entry in manifest["files"].as_array().unwrap() {
        let relative = entry["path"].as_str().unwrap();
        let original = fs::read_to_string(root.join(relative)).unwrap();
        let value: Value = serde_json::from_str(&original).unwrap();
        let provider = entry["provider_family"].as_str().unwrap();
        assert_eq!(value["provenance"], "synthetic");
        assert_eq!(value["schema_version"], 1, "schema mismatch: {relative}");
        assert_eq!(
            value["provider_family"], provider,
            "fixture provider mismatch: {relative}"
        );
        let records = value["records"].as_array().expect("records array");
        assert_eq!(
            records.len(),
            entry["records"].as_u64().expect("manifest record count") as usize,
            "record count mismatch: {relative}"
        );
        let source_record_families = entry["source_record_families"]
            .as_array()
            .expect("source_record_families array");
        let declared_record_families: BTreeSet<_> = source_record_families
            .iter()
            .map(|family| family.as_str().expect("record family string"))
            .collect();
        assert_eq!(
            source_record_families.len(),
            declared_record_families.len(),
            "duplicate declared record family: {relative}"
        );
        let mut observed_record_families = BTreeSet::new();
        let mut record_ids = BTreeSet::new();
        let mut concrete_coverage = BTreeSet::new();
        for record in records {
            let record_id = record["id"].as_str().expect("record id string");
            assert!(!record_id.is_empty(), "empty record id: {relative}");
            assert!(
                record_ids.insert(record_id),
                "duplicate record id {record_id}: {relative}"
            );
            let record_type = record["record_type"].as_str().expect("record_type string");
            observed_record_families.insert(record_type);
            observed_corpus_record_families.insert(record_type.to_owned());
            if record_type == "message" {
                assert!(
                    record["content"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("message");
            }
        }
        assert_eq!(
            observed_record_families, declared_record_families,
            "record families mismatch: {relative}"
        );

        match provider {
            "claude" => {
                let event = records
                    .iter()
                    .find(|record| record["record_type"] == "assistant_event")
                    .expect("Claude assistant event");
                let subagent = event["subagent"]
                    .as_object()
                    .expect("Claude subagent object");
                assert!(
                    subagent["id"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                assert!(
                    subagent["parent_id"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("subagent");
                let tool = event["tool"].as_object().expect("Claude tool object");
                assert!(tool["name"].as_str().is_some_and(|value| !value.is_empty()));
                assert!(tool["arguments"].is_object());
                assert!(
                    tool["result"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("tool");
                assert!(
                    event["reasoning_summary"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("reasoning-summary");
            }
            "codex" => {
                let event = records
                    .iter()
                    .find(|record| record["record_type"] == "assistant_event")
                    .expect("Codex assistant event");
                assert!(
                    event["goal"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("goal");
                let git = event["git"].as_object().expect("Codex git object");
                assert!(
                    git["branch"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                assert_eq!(git["commit"].as_str().map(str::len), Some(40));
                concrete_coverage.insert("git");
            }
            "cursor" => {
                let event = records
                    .iter()
                    .find(|record| record["record_type"] == "assistant_event")
                    .expect("Cursor assistant event");
                assert!(
                    event["rewrite"]["supersedes"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("rewrite");
                assert_eq!(event["truncated"], true);
                assert!(
                    event["content"]
                        .as_str()
                        .is_some_and(|value| value.contains("TRUNCATED"))
                );
                concrete_coverage.insert("truncation");
            }
            "gemini" => {
                let malformed = records
                    .iter()
                    .find(|record| record["record_type"] == "malformed_input")
                    .expect("Gemini malformed input");
                assert_eq!(malformed["parse_status"], "rejected");
                assert!(
                    malformed["raw_fragment"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("malformed");
                let partial = records
                    .iter()
                    .find(|record| record["record_type"] == "partial_input")
                    .expect("Gemini partial input");
                assert_eq!(partial["partial"], true);
                assert!(
                    partial["content"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                );
                concrete_coverage.insert("partial");
            }
            "hermes" => {
                let unicode = records
                    .iter()
                    .find(|record| record["id"] == "hermes-unicode-001")
                    .expect("Hermes Unicode record");
                assert!(
                    unicode["content"]
                        .as_str()
                        .is_some_and(|value| !value.is_ascii())
                );
                concrete_coverage.insert("unicode");
                assert!(unicode["timestamp"].is_null());
                concrete_coverage.insert("missing-timestamp");
                let placeholder = records
                    .iter()
                    .find(|record| record["id"] == "hermes-secret-placeholder-001")
                    .expect("Hermes secret placeholder");
                assert_eq!(placeholder["record_type"], "tool_result");
                assert!(
                    placeholder["content"]
                        .as_str()
                        .is_some_and(|value| value.contains("<REDACTED_SYNTHETIC_SECRET>"))
                );
                concrete_coverage.insert("secret-placeholder");
                let canary = records
                    .iter()
                    .find(|record| record["id"] == "hermes-credential-positive-control-001")
                    .expect("Hermes credential positive control");
                assert_eq!(canary["record_type"], "credential_positive_control");
                assert_eq!(
                    canary["label"],
                    "synthetic-reserved-invalid-positive-control"
                );
                assert_eq!(
                    canary["content"],
                    "sk-SYNTHETIC_RESERVED_INVALID_000000000000"
                );
                concrete_coverage.insert("synthetic-credential-positive-control");
            }
            _ => {}
        }
        for coverage in entry["coverage"].as_array().expect("coverage array") {
            let coverage = coverage.as_str().expect("coverage string");
            assert!(
                concrete_coverage.contains(coverage),
                "coverage {coverage} lacks concrete evidence: {relative}"
            );
        }

        let normalization = &manifest["normalization_contract"];
        assert_eq!(normalization["json_key_order"], "lexicographic");
        assert_eq!(normalization["line_ending"], "LF");
        assert_eq!(normalization["terminal_newline"], true);
        assert_eq!(normalization["unicode"], "preserved");
        assert!(!original.contains('\r'), "non-LF line ending: {relative}");
        assert!(
            original.ends_with('\n'),
            "terminal newline missing: {relative}"
        );
        let normalized = serde_json::to_string_pretty(&value).unwrap() + "\n";
        assert_eq!(
            original, normalized,
            "fixture is not canonically normalized: {relative}"
        );
    }
    assert!(canonical_record_families.is_subset(&observed_corpus_record_families));
    assert_eq!(
        observed_corpus_record_families,
        authoritative_record_families
    );
}

#[test]
fn fixtures_pass_secret_scan() {
    let forbidden = [
        regex::Regex::new(r"A(?:KI|SI)A[0-9A-Z]{16}").unwrap(),
        regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{20,}").unwrap(),
        regex::Regex::new(r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}").unwrap(),
        regex::Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").unwrap(),
        regex::Regex::new(r"AIza[0-9A-Za-z_-]{30,}").unwrap(),
        regex::Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap(),
        regex::Regex::new(r"eyJ[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}")
            .unwrap(),
    ];
    let root = corpus_root();
    let canary_path = root.join("providers/hermes.json");
    let canary_value = "sk-SYNTHETIC_RESERVED_INVALID_000000000000";
    let canary_matches = forbidden
        .iter()
        .filter(|pattern| pattern.is_match(canary_value))
        .count();
    assert_eq!(
        canary_matches, 1,
        "secret detector must match the canary once"
    );

    for entry in files_under(&root) {
        let mut text = fs::read_to_string(entry.path()).unwrap();
        if entry.path() == canary_path {
            let mut fixture: Value = serde_json::from_str(&text).unwrap();
            let records = fixture["records"].as_array_mut().expect("records array");
            let matching_records: Vec<_> = records
                .iter_mut()
                .filter(|record| record["id"] == "hermes-credential-positive-control-001")
                .collect();
            assert_eq!(
                matching_records.len(),
                1,
                "expected exactly one secret canary"
            );
            let canary = &mut *matching_records.into_iter().next().unwrap();
            assert_eq!(canary["record_type"], "credential_positive_control");
            assert_eq!(
                canary["label"],
                "synthetic-reserved-invalid-positive-control"
            );
            assert_eq!(canary["content"], canary_value);
            canary["content"] = Value::String("<REMOVED_SYNTHETIC_CANARY>".to_owned());
            text = serde_json::to_string(&fixture).unwrap();
        }
        for pattern in &forbidden {
            assert!(
                !pattern.is_match(&text),
                "secret-shaped pattern {pattern:?} in {}",
                entry.path().display()
            );
        }
    }
}

#[test]
fn reference_machine_and_generator_are_reproducible() {
    let manifest = manifest();
    let reference = &manifest["reference_machine"];
    for key in ["label", "architecture", "os", "rust_version"] {
        let value = reference[key]
            .as_str()
            .unwrap_or_else(|| panic!("reference_machine.{key} must be a string"));
        assert!(
            !value.trim().is_empty(),
            "reference_machine.{key} must be nonempty"
        );
    }
    assert!(
        reference["logical_cpus"]
            .as_u64()
            .is_some_and(|value| value > 0),
        "reference_machine.logical_cpus must be a nonzero integer"
    );
    assert!(
        reference["memory_gib"]
            .as_f64()
            .is_some_and(|value| value.is_finite() && value > 0.0),
        "reference_machine.memory_gib must be a positive finite number"
    );

    let benchmark = &manifest["benchmark"];
    let scale_factor = benchmark["scale_factor"].as_u64().unwrap();
    assert_eq!(scale_factor, 10);
    let base_records: u64 = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["records"].as_u64().unwrap())
        .sum();
    assert_eq!(
        benchmark["generated_records"].as_u64().unwrap(),
        base_records * scale_factor
    );

    let generator =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v2_corpus_suite/generate_10x.py");
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for output in [first.path(), second.path()] {
        let status = Command::new("python3")
            .arg(&generator)
            .arg("--output")
            .arg(output)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let first_bytes = fs::read(first.path().join("synthetic-10x.jsonl")).unwrap();
    let second_bytes = fs::read(second.path().join("synthetic-10x.jsonl")).unwrap();
    assert_eq!(first_bytes, second_bytes);
    let generated_sha256 = hex::encode(Sha256::digest(&first_bytes));
    assert_eq!(
        generated_sha256,
        manifest["benchmark"]["generated_sha256"].as_str().unwrap()
    );
    let receipt: Value =
        serde_json::from_str(&fs::read_to_string(first.path().join("receipt.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["sha256"], generated_sha256);
    assert_eq!(receipt["scale_factor"], scale_factor);
    assert_eq!(receipt["records"], base_records * scale_factor);
    assert_eq!(
        first_bytes.iter().filter(|byte| **byte == b'\n').count(),
        manifest["benchmark"]["generated_records"].as_u64().unwrap() as usize
    );
}
