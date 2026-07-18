use std::{env, fs, path::{Path, PathBuf}, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u64 = 2;
const WORKLOAD_ID: &str = "pr8-session-temporal-v1";
const BLOCKED_REASON: &str = "authentic_provider_capture_and_public_production_path_unavailable";
const WORKLOAD_PATH: &str = "benchmarks/pr8-temporal/workload-v1.json";
const EVIDENCE_INDEX_PATH: &str = "benchmarks/pr8-temporal/evidence-index.json";
const RESULT_PATH: &str = "benchmarks/pr8-temporal/result-provisional.json";
const RUNNER_PATH: &str = "scripts/run-pr8-temporal-benchmark.sh";

type BenchResult<T> = Result<T, String>;

fn main() {
    let arguments = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect::<Vec<_>>();
    let result = match arguments.as_slice() {
        [] => validate_contract(),
        [argument] if argument == "--validate-only" => validate_contract(),
        [argument] if argument == "--run" => Err(format!(
            "BLOCKED: {BLOCKED_REASON}; no benchmark samples may be collected"
        )),
        _ => Err(
            "usage: cargo test --bench session_temporal | cargo bench --bench session_temporal -- --run"
                .to_owned(),
        ),
    };
    if let Err(error) = result {
        eprintln!("PR8 temporal benchmark: {error}");
        std::process::exit(1);
    }
}

fn validate_contract() -> BenchResult<()> {
    let root = repository_root();
    let workload_path = root.join(WORKLOAD_PATH);
    let workload = read_json(&workload_path)?;
    require_json_value(&workload["schema_version"], json!(SCHEMA_VERSION), "workload schema")?;
    require_json_value(&workload["workload_id"], json!(WORKLOAD_ID), "workload id")?;
    require_json_value(&workload["status"], json!("blocked"), "workload status")?;
    require_json_value(
        &workload["blocked_reason"],
        json!(BLOCKED_REASON),
        "workload blocked reason",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["independently_sourced"],
        json!(false),
        "fixture source status",
    )?;
    require_json_value(
        &workload["fixture_evidence"]["sanitization_receipt"],
        Value::Null,
        "fixture sanitization receipt",
    )?;
    require_json_value(
        &workload["measurement_contract"],
        Value::Null,
        "blocked measurement contract",
    )?;
    validate_file_inventory(&root, &workload["file_inventory"])?;
    validate_bench_profile(&root)?;

    let index = read_json(&root.join(EVIDENCE_INDEX_PATH))?;
    require_json_value(&index["schema_version"], json!(SCHEMA_VERSION), "index schema")?;
    require_json_value(&index["current_acceptance"], Value::Null, "current acceptance")?;
    require_json_value(
        &index["blocked"],
        json!("result-provisional.json"),
        "blocked result",
    )?;

    let result = read_json(&root.join(RESULT_PATH))?;
    require_json_value(&result["schema_version"], json!(SCHEMA_VERSION), "result schema")?;
    require_json_value(&result["workload_id"], json!(WORKLOAD_ID), "result workload id")?;
    require_json_value(&result["capture_status"], json!("blocked"), "capture status")?;
    require_json_value(
        &result["acceptance_eligible"],
        json!(false),
        "acceptance eligibility",
    )?;
    require_json_value(&result["blocked_reason"], json!(BLOCKED_REASON), "result reason")?;
    require_json_value(&result["measurement"], Value::Null, "blocked measurement")?;
    require_json_value(
        &result["workload_manifest_sha256"],
        json!(sha256_file(&workload_path)?),
        "workload manifest hash",
    )?;

    let runner = fs::read_to_string(root.join(RUNNER_PATH))
        .map_err(|error| format!("read runner: {error}"))?;
    for token in ["--dry-run", "--run", "BLOCKED", "PR8 temporal measurements require Linux"] {
        if !runner.contains(token) {
            return Err(format!("runner is missing required token {token:?}"));
        }
    }
    Ok(())
}

fn validate_file_inventory(root: &Path, inventory: &Value) -> BenchResult<()> {
    let entries = inventory
        .as_array()
        .ok_or_else(|| "file_inventory must be an array".to_owned())?;
    if entries.is_empty() {
        return Err("file_inventory must not be empty".to_owned());
    }
    let mut paths = std::collections::BTreeSet::new();
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| "inventory path must be a string".to_owned())?;
        let expected = entry["sha256"]
            .as_str()
            .ok_or_else(|| format!("inventory hash missing for {path}"))?;
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(format!("inventory path must remain repository-relative: {path}"));
        }
        if !paths.insert(path) {
            return Err(format!("duplicate inventory path: {path}"));
        }
        let actual = sha256_file(&root.join(relative))?;
        if actual != expected {
            return Err(format!(
                "inventory hash mismatch for {path}: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

fn validate_bench_profile(root: &Path) -> BenchResult<()> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|error| format!("read Cargo.toml: {error}"))?;
    let profile = manifest
        .split_once("[profile.bench]")
        .map(|(_, profile)| profile.split("\n[").next().unwrap_or(profile))
        .ok_or_else(|| "Cargo.toml is missing [profile.bench]".to_owned())?;
    for line in [
        "opt-level = 3",
        "debug = false",
        "debug-assertions = false",
        "overflow-checks = false",
        "incremental = false",
    ] {
        if !profile.lines().any(|candidate| candidate.trim() == line) {
            return Err(format!("bench profile is missing {line:?}"));
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> BenchResult<Value> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&contents).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn require_json_value(actual: &Value, expected: Value, label: &str) -> BenchResult<()> {
    if actual != &expected {
        return Err(format!("{label} mismatch: expected {expected}, got {actual}"));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> BenchResult<String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read {} for SHA-256: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[allow(dead_code)]
fn current_commit(root: &Path) -> BenchResult<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| format!("read current commit: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
