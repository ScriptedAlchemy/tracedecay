use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

const NATIVE_NORMALIZATION_PROVIDERS: [&str; 3] = ["claude", "codex", "hermes"];
const UNAVAILABLE_NORMALIZATION_PROVIDERS: [&str; 4] =
    ["cursor", "cursor_composer", "kiro", "vibe"];
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
fn provider_fixture_manifest_separates_native_acceptance_from_behavioral_inputs() {
    let root = fixture_root("provider_normalization");
    let manifest = read_json(&root.join("manifest.json"));
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(
        manifest["native_acceptance"]["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|provider| provider.as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        NATIVE_NORMALIZATION_PROVIDERS.into_iter().collect()
    );

    let mut manifested_paths = BTreeSet::new();
    let mut manifested_providers = BTreeSet::new();
    for fixture in manifest["native_acceptance"]["fixtures"]
        .as_array()
        .unwrap()
    {
        let provider = fixture["provider"].as_str().unwrap();
        let path = fixture["path"].as_str().unwrap();
        assert_eq!(fixture["origin"], "redacted_native_capture");
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
    }

    for category in ["behavioral_inputs", "negative_inputs"] {
        for fixture in manifest[category].as_array().unwrap() {
            let path = fixture["path"].as_str().unwrap();
            assert_eq!(
                fixture["sha256"].as_str().unwrap(),
                sha256(&root.join(path)),
                "{path}: payload bytes changed without provenance update"
            );
            assert!(
                manifested_paths.insert(path.to_owned()),
                "{path}: classified more than once"
            );
        }
    }
    assert_eq!(manifested_paths, input_paths(&root));
    assert_eq!(
        manifested_providers,
        NATIVE_NORMALIZATION_PROVIDERS.into_iter().collect()
    );

    let unavailable = manifest["unavailable_normalization"].as_array().unwrap();
    assert_eq!(
        unavailable
            .iter()
            .map(|entry| entry["provider"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        UNAVAILABLE_NORMALIZATION_PROVIDERS.into_iter().collect()
    );
    for entry in unavailable {
        let provider = entry["provider"].as_str().unwrap();
        assert_eq!(entry["state"], "unavailable", "{provider}");
        assert_eq!(
            entry["reason"], "checked_in_native_transcript_missing",
            "{provider}"
        );
        assert!(
            !entry["native_surface"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{provider}: missing exact native surface"
        );
        assert!(
            matches!(
                entry["provider_identity"]["release"]["state"].as_str(),
                Some("known" | "unavailable")
            ),
            "{provider}: provider release provenance must be explicit"
        );
        if entry["provider_identity"]["release"]["state"] == "known" {
            assert!(
                !entry["provider_identity"]["release"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "{provider}: known release has no value"
            );
            let evidence = entry["provider_identity"]["release"]["evidence"]
                .as_str()
                .unwrap();
            assert!(
                root.join(evidence).is_file(),
                "{provider}: release evidence does not exist: {evidence}"
            );
        }
        assert_eq!(
            entry["schema_provenance"]["state"], "unavailable",
            "{provider}: schema provenance must not be inferred from generated samples"
        );
        assert!(
            !entry["schema_provenance"]["reason"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{provider}: missing schema blocker"
        );
    }
}

#[test]
fn cline_family_manifest_reports_unavailable_native_acceptance() {
    let root = fixture_root("transcript_golden/cline_like");
    let manifest = read_json(&root.join("manifest.json"));
    assert_eq!(manifest["schema_version"], 2);
    assert_eq!(
        manifest["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|provider| provider["provider"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        CLINE_FAMILY_PROVIDERS.into_iter().collect()
    );
    for provider in manifest["providers"].as_array().unwrap() {
        let provider_id = provider["provider"].as_str().unwrap();
        assert_eq!(
            provider["normalization_acceptance"]["state"], "unavailable",
            "{provider_id}"
        );
        assert_eq!(
            provider["normalization_acceptance"]["reason"], "checked_in_native_transcript_missing",
            "{provider_id}"
        );
        assert_eq!(
            provider["schema_provenance"]["state"], "unavailable",
            "{provider_id}"
        );
        assert!(
            !provider["provider_identity"]["extension_id"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{provider_id}: missing exact extension identity"
        );
    }

    let behavioral = &manifest["behavioral_fixture"];
    assert_eq!(behavioral["purpose"], "adapter_behavior_only");
    assert_eq!(behavioral["confers_native_acceptance"], false);
    let manifested = behavioral["inputs"]
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
