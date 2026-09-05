use tracedecay_domain::canonical_text::sha256_hex;

use super::candidate_output::{
    CandidateWorkloadV1, compute_corpus_digest_from_embedded_bytes, validate_workload_for_tuning,
};
use super::evaluate::SearchEvalError;

const WORKLOAD_PATH: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";
const WORKLOAD_SHA256: &str = "1859c7091287d53bf4c80506373d9d537079ebc252ee3cd118443df42a7894bc";

const FILES: &[(&str, &[u8])] = &[
    (
        WORKLOAD_PATH,
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs"
        ),
    ),
    (
        "tests/fixtures/context_eval_project/src/auth/login.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/context_eval_project/src/auth/login.rs"
        ),
    ),
    (
        "tests/fixtures/context_eval_project/src/storage/config_store.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/context_eval_project/src/storage/config_store.rs"
        ),
    ),
    (
        "tests/fixtures/sample.dockerfile",
        include_bytes!("../../assets/runtime-root/tests/fixtures/sample.dockerfile"),
    ),
    (
        "evals/agent_adoption/fixture/Cargo.lock",
        include_bytes!("../../assets/runtime-root/evals/agent_adoption/fixture/Cargo.lock"),
    ),
    (
        "tests/fixtures/search_quality/corpus/cargo-slot/src/main.rust.fixture",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/corpus/cargo-slot/src/main.rust.fixture"
        ),
    ),
    (
        "tests/fixtures/search_quality/incremental/time-after.rs",
        include_bytes!(
            "../../assets/runtime-root/tests/fixtures/search_quality/incremental/time-after.rs"
        ),
    ),
];

pub fn packaged_evaluator_files() -> &'static [(&'static str, &'static [u8])] {
    FILES
}

pub fn load_workload() -> Result<CandidateWorkloadV1, SearchEvalError> {
    let observed_workload_digest = sha256_hex(FILES[0].1);
    if observed_workload_digest != WORKLOAD_SHA256 {
        return Err(SearchEvalError::Contract(format!(
            "packaged evaluator workload digest mismatch: expected {WORKLOAD_SHA256}, observed {observed_workload_digest}"
        )));
    }
    let workload = serde_json::from_slice::<CandidateWorkloadV1>(FILES[0].1).map_err(|error| {
        SearchEvalError::Contract(format!("parse packaged evaluator workload: {error}"))
    })?;
    validate_workload_for_tuning(&workload)?;
    Ok(workload)
}

/// Derive the corpus binding from the bytes embedded in this package. This is
/// deliberately separate from evaluator materialization: qualification loading
/// must not create a temporary evaluator root merely to establish corpus identity.
pub fn current_corpus_digest(workload: &CandidateWorkloadV1) -> Result<String, SearchEvalError> {
    Ok(compute_corpus_digest_from_embedded_bytes(workload, FILES)?)
}
