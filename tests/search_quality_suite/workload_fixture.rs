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
        "sha256:9c793070efc601a13e145c233dbc6fb859764ce9d24d64c636552c9e020db1ce"
    );
    assert_eq!(
        summary.workload_digest,
        compute_workload_digest(&workload).expect("summary workload digest")
    );
    assert_eq!(workload.execution_contract.exact_file_count, 13);
    assert_eq!(workload.execution_contract.exact_corpus_bytes, 147_447);
    assert_eq!(
        workload.execution_contract.exact_eligible_chunks_current,
        1_960
    );
    assert_eq!(
        workload.execution_contract.exact_eligible_chunks_10x,
        workload
            .execution_contract
            .exact_eligible_chunks_current
            .checked_mul(10)
            .expect("exact 10x chunk count")
    );

    let source = workload
        .corpus
        .iter()
        .find(|document| document.document_id == workload.incremental_fixture.document_id)
        .expect("incremental source belongs to the corpus");
    let before = fs::read_to_string(repo_root.join(&source.path)).expect("read before fixture");
    let after_path = repo_root.join(&workload.incremental_fixture.after_path);
    let after = fs::read_to_string(&after_path).expect("read after fixture");
    assert_eq!(
        hex::encode(Sha256::digest(after.as_bytes())),
        workload.incremental_fixture.after_sha256
    );
    assert_eq!(
        before.matches("pub fn validate(&self)").count(),
        1,
        "incremental edit must have one authentic source target"
    );
    assert_eq!(
        after,
        before.replacen("pub fn validate(&self)", "pub fn validate_bounds(&self)", 1),
        "incremental fixture must contain only the declared one-symbol edit"
    );
}
