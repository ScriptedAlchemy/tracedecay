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
    let mut observed = BTreeSet::new();
    let files = manifest["files"].as_array().expect("files array");
    assert!(!files.is_empty());

    for entry in files {
        let relative = entry["path"].as_str().expect("fixture path");
        assert!(relative.starts_with("providers/"));
        let bytes = fs::read(corpus_root().join(relative)).unwrap();
        let actual = hex::encode(Sha256::digest(&bytes));
        assert_eq!(
            entry["sha256"].as_str().unwrap(),
            actual,
            "hash mismatch: {relative}"
        );
        for tag in entry["coverage"].as_array().expect("coverage array") {
            observed.insert(tag.as_str().unwrap());
        }
    }
    assert!(
        required_coverage.is_subset(&observed),
        "missing coverage: {:?}",
        required_coverage.difference(&observed)
    );
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

    for relative in actual {
        let original = fs::read_to_string(root.join(&relative)).unwrap();
        let value: Value = serde_json::from_str(&original).unwrap();
        assert_eq!(value["provenance"], "synthetic");
        assert!(
            value.get("records").and_then(Value::as_array).is_some(),
            "records missing: {relative}"
        );
        let normalized = serde_json::to_string_pretty(&value).unwrap() + "\n";
        let reparsed: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(
            value, reparsed,
            "normalization changed semantics: {relative}"
        );
        assert_eq!(
            original, normalized,
            "fixture is not canonically normalized: {relative}"
        );
    }
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
    for entry in files_under(&root) {
        let text = fs::read_to_string(entry.path()).unwrap();
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
    for key in [
        "label",
        "architecture",
        "logical_cpus",
        "memory_gib",
        "os",
        "rust_version",
    ] {
        assert!(
            reference.get(key).is_some(),
            "reference_machine.{key} missing"
        );
    }

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
    assert_eq!(
        first_bytes.iter().filter(|byte| **byte == b'\n').count(),
        manifest["benchmark"]["generated_records"].as_u64().unwrap() as usize
    );
}
