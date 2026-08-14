//! query/semantic candidate-workload fixture integrity.
//!
//! This test validates the checked-in workload against the root-owned
//! `search_eval` module and the root-checked-in fixture corpus, so it stays
//! with the root crate while the query lane regressions live in
//! `crates/tracedecay-query/tests/search_quality_suite`.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay::search_eval::{
    compute_workload_digest, load_candidate_workload, validate_direct_workload,
};

#[test]
fn semantic_workload_and_incremental_fixture_are_byte_exact() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workload_path =
        repo_root.join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json");
    let workload = load_candidate_workload(&workload_path).expect("checked-in workload parses");
    let summary =
        validate_direct_workload(repo_root, None).expect("checked-in workload is authoritative");

    assert_eq!(
        compute_workload_digest(&workload).expect("workload digest"),
        "sha256:068eeb1726539df4575f0b0b516403c7123dbd157acfb22aa2a89c4fcfbb5610"
    );
    assert_eq!(
        summary.workload_digest,
        compute_workload_digest(&workload).expect("summary workload digest")
    );
    let after_path = repo_root.join(&workload.incremental_fixture.after_path);
    let after = fs::read_to_string(&after_path).expect("read after fixture");
    assert_eq!(
        hex::encode(Sha256::digest(after.as_bytes())),
        workload.incremental_fixture.after_sha256
    );
}
