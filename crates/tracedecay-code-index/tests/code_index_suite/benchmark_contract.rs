use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const WORKLOAD_PATH: &str = "../../benchmarks/pr9-code-index/workload-v1.json";
const EXPECTED_PATH: &str = "../../benchmarks/pr9-code-index/expected-v1.json";

fn load_json(path: &str) -> Value {
    let absolute = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    let bytes = fs::read(&absolute)
        .unwrap_or_else(|error| panic!("read pinned PR9 artifact {}: {error}", absolute.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse pinned PR9 artifact {}: {error}", absolute.display()))
}

fn u64_at(value: &Value, pointer: &str) -> u64 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing unsigned integer at {pointer}"))
}

#[test]
fn workload_pins_exact_current_and_ten_x_measurement_matrix() {
    let workload = load_json(WORKLOAD_PATH);
    assert_eq!(u64_at(&workload, "/schema_version"), 1);
    assert_eq!(workload["workload_id"].as_str(), Some("pr9-code-index-v1"));
    assert_eq!(
        workload["harness_revision"].as_str(),
        Some("code-index-chunks.v2")
    );
    assert_eq!(u64_at(&workload, "/repetitions/warmups"), 5);
    assert_eq!(u64_at(&workload, "/repetitions/measured"), 30);

    let cases = workload["cases"]
        .as_array()
        .expect("workload cases")
        .iter()
        .map(|case| case["name"].as_str().expect("case name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        cases,
        BTreeSet::from([
            "clean",
            "chunker_replay",
            "deletion",
            "incompatible_rebuild",
            "no_op",
            "warm_one_file",
        ])
    );

    let scales = workload["scales"].as_array().expect("workload scales");
    assert_eq!(scales.len(), 2);
    let current = scales
        .iter()
        .find(|scale| scale["name"] == "current")
        .expect("current scale");
    let ten_x = scales
        .iter()
        .find(|scale| scale["name"] == "10x")
        .expect("10x scale");
    assert_eq!(current["factor"].as_u64(), Some(1));
    assert_eq!(ten_x["factor"].as_u64(), Some(10));

    for field in ["files", "bytes", "chunks"] {
        assert_eq!(
            ten_x[field].as_u64(),
            current[field].as_u64().map(|value| value * 10),
            "{field} must be an exact 10x workload"
        );
    }
    for field in ["content_digest", "descriptor_digest"] {
        let digest = workload["corpus"][field].as_str().expect("pinned digest");
        assert!(
            digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64),
            "{field} must be a canonical sha256 digest"
        );
    }
    assert_eq!(
        workload["runtime"]["cargo_command"].as_str(),
        Some("cargo bench --bench code_index_chunks -- --run")
    );
}

#[test]
fn expected_counts_cover_every_case_at_both_scales() {
    let workload = load_json(WORKLOAD_PATH);
    let expected = load_json(EXPECTED_PATH);
    assert_eq!(u64_at(&expected, "/schema_version"), 1);
    assert_eq!(expected["workload_id"], workload["workload_id"]);

    let expected_scales = expected["scales"].as_array().expect("expected scales");
    assert_eq!(expected_scales.len(), 2);
    for scale in expected_scales {
        let cases = scale["cases"].as_array().expect("expected cases");
        assert_eq!(cases.len(), 6);
        for case in cases {
            for field in [
                "files_parsed",
                "chunks_added_or_changed",
                "chunks_deleted",
                "chunks_reused",
                "projection_calls",
                "input_bytes",
                "output_bytes",
            ] {
                assert!(
                    case[field].as_u64().is_some(),
                    "{}:{} must pin {field}",
                    scale["name"],
                    case["name"]
                );
            }
        }
    }
}
