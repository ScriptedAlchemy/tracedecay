use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

const NORMALIZATION_PROVIDERS: [&str; 7] = [
    "claude",
    "codex",
    "cursor",
    "cursor_composer",
    "hermes",
    "kiro",
    "vibe",
];
const CLINE_FAMILY_PROVIDERS: [&str; 3] = ["cline", "roo-code", "kilo"];

fn fixture_root(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(relative)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn sha256(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}

fn input_paths(root: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for provider in fs::read_dir(root).unwrap() {
        let provider = provider.unwrap();
        if !provider.file_type().unwrap().is_dir() {
            continue;
        }
        let provider_name = provider.file_name();
        for entry in fs::read_dir(provider.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type().unwrap().is_file() && name.ends_with(".input.json") {
                paths.insert(format!("{}/{}", provider_name.to_string_lossy(), name));
            }
        }
    }
    paths
}

#[test]
fn provider_fixture_manifest_covers_every_accepted_native_input() {
    let root = fixture_root("provider_normalization");
    let manifest = read_json(&root.join("manifest.json"));
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(
        manifest["supported_providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|provider| provider.as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        NORMALIZATION_PROVIDERS.into_iter().collect()
    );

    let mut manifested_paths = BTreeSet::new();
    let mut manifested_providers = BTreeSet::new();
    let mut authoritative_providers = BTreeSet::new();
    for fixture in manifest["fixtures"].as_array().unwrap() {
        let provider = fixture["provider"].as_str().unwrap();
        let path = fixture["path"].as_str().unwrap();
        let origin = fixture["origin"].as_str().unwrap();
        assert!(
            matches!(
                origin,
                "redacted_native_capture" | "synthetic_value_contract"
            ),
            "{path}: unknown fixture origin {origin}"
        );
        assert_eq!(
            fixture["provider_version"].as_str(),
            Some("unversioned"),
            "{path}: provider version must be explicit without inventing a version"
        );
        assert!(
            !fixture["origin_evidence"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{path}: missing origin evidence"
        );
        assert_eq!(
            fixture["sha256"].as_str().unwrap(),
            sha256(&root.join(path)),
            "{path}: payload bytes changed without provenance update"
        );
        assert!(
            manifested_paths.insert(path.to_owned()),
            "{path}: duplicate"
        );
        manifested_providers.insert(provider);
        if origin == "redacted_native_capture" {
            authoritative_providers.insert(provider);
        }
    }

    assert_eq!(manifested_paths, input_paths(&root));
    assert_eq!(
        manifested_providers,
        NORMALIZATION_PROVIDERS.into_iter().collect()
    );
    assert_eq!(
        authoritative_providers,
        ["claude", "codex", "hermes"].into_iter().collect()
    );
}

#[test]
fn cline_family_manifest_covers_every_snapshot_input() {
    let root = fixture_root("transcript_golden/cline_like");
    let manifest = read_json(&root.join("manifest.json"));
    assert_eq!(
        manifest["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|provider| provider["provider"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        CLINE_FAMILY_PROVIDERS.into_iter().collect()
    );
    let provenance = &manifest["fixture_provenance"];
    assert_eq!(provenance["origin"], "synthetic_value_contract");
    assert_eq!(provenance["provider_version"], "unversioned");
    assert!(
        !provenance["origin_evidence"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );

    let manifested = provenance["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| {
            let path = input["path"].as_str().unwrap();
            assert_eq!(
                input["sha256"].as_str().unwrap(),
                sha256(&root.join(path)),
                "{path}: payload bytes changed without provenance update"
            );
            path.to_owned()
        })
        .collect::<BTreeSet<_>>();
    let accepted = fs::read_dir(root.join("input"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            format!("input/{}", entry.file_name().to_string_lossy())
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(manifested, accepted);
}
