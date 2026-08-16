use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

use crate::candidate_output::compute_corpus_digest_from_embedded_bytes;
use crate::{
    CandidateWorkloadV1, SearchEvalError, load_candidate_workload, validate_workload_for_tuning,
};

#[cfg(test)]
thread_local! {
    static MATERIALIZATION_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn materialization_count() -> u64 {
    MATERIALIZATION_COUNT.with(std::cell::Cell::get)
}

const WORKLOAD_PATH: &str =
    "tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json";
const SOURCE_COMMIT: &str = "8312618fee8109b16be09e65f45118b4e550fa14";
const PACK_ID: &str = "184f6ca1eafd40e7889d15a20b7a5c861e80a47b";
const WORKLOAD_SHA256: &str = "8805b9aa556d86d0ad82b3d55107e4cb0c267f288455cb769883de217c73834a";

const FILES: &[(&str, &[u8])] = &[
    (
        WORKLOAD_PATH,
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/time.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/watermark.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/canonical.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/error.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/research/coverage.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/repository.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/integration.rs"
        ),
    ),
    (
        "tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/crates/tracedecay-domain/src/session.rs"
        ),
    ),
    (
        "tests/fixtures/context_eval_project/src/auth/login.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/context_eval_project/src/auth/login.rs"
        ),
    ),
    (
        "tests/fixtures/context_eval_project/src/storage/config_store.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/context_eval_project/src/storage/config_store.rs"
        ),
    ),
    (
        "tests/fixtures/sample.dockerfile",
        include_bytes!("../assets/runtime-root/tests/fixtures/sample.dockerfile"),
    ),
    (
        "evals/agent_adoption/fixture/Cargo.lock",
        include_bytes!("../assets/runtime-root/evals/agent_adoption/fixture/Cargo.lock"),
    ),
    (
        "tests/fixtures/search_quality/corpus/cargo-slot/src/main.rust.fixture",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/corpus/cargo-slot/src/main.rust.fixture"
        ),
    ),
    (
        "tests/fixtures/search_quality/incremental/time-after.rs",
        include_bytes!(
            "../assets/runtime-root/tests/fixtures/search_quality/incremental/time-after.rs"
        ),
    ),
];

pub(crate) struct PackagedEvaluatorAssets {
    _directory: TempDir,
    root: PathBuf,
    workload: CandidateWorkloadV1,
}

impl PackagedEvaluatorAssets {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn workload_path(&self) -> PathBuf {
        self.root.join(WORKLOAD_PATH)
    }

    pub(crate) fn workload(&self) -> &CandidateWorkloadV1 {
        &self.workload
    }
}

pub(crate) fn materialize() -> Result<PackagedEvaluatorAssets, SearchEvalError> {
    let workload = load_workload()?;
    #[cfg(test)]
    MATERIALIZATION_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    let directory = tempfile::tempdir().map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator root: {error}"))
    })?;
    for (relative, bytes) in FILES {
        let path = directory.path().join(relative);
        let parent = path.parent().ok_or_else(|| {
            SearchEvalError::Contract(format!(
                "packaged evaluator asset has no parent: {}",
                path.display()
            ))
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            SearchEvalError::Contract(format!(
                "create packaged evaluator directory {}: {error}",
                parent.display()
            ))
        })?;
        fs::write(&path, bytes).map_err(|error| {
            SearchEvalError::Contract(format!(
                "write packaged evaluator asset {}: {error}",
                path.display()
            ))
        })?;
    }
    materialize_git_authority(directory.path())?;
    let materialized_workload = load_candidate_workload(&directory.path().join(WORKLOAD_PATH))?;
    if materialized_workload != workload {
        return Err(SearchEvalError::Contract(
            "materialized evaluator workload differs from packaged bytes".to_owned(),
        ));
    }
    Ok(PackagedEvaluatorAssets {
        root: directory.path().to_path_buf(),
        _directory: directory,
        workload,
    })
}

pub(crate) fn load_workload() -> Result<CandidateWorkloadV1, SearchEvalError> {
    let observed_workload_digest = hex::encode(Sha256::digest(FILES[0].1));
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
/// deliberately separate from `materialize`: qualification loading must not
/// create a temporary evaluator root merely to establish corpus identity.
pub(crate) fn current_corpus_digest(
    workload: &CandidateWorkloadV1,
) -> Result<String, SearchEvalError> {
    compute_corpus_digest_from_embedded_bytes(workload, FILES).map_err(SearchEvalError::from)
}

fn materialize_git_authority(root: &Path) -> Result<(), SearchEvalError> {
    let git = root.join(".git");
    let pack_root = git.join("objects/pack");
    let refs_root = git.join("refs");
    fs::create_dir_all(&pack_root).map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator Git authority: {error}"))
    })?;
    fs::create_dir_all(&refs_root).map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator Git refs: {error}"))
    })?;
    let decode = |encoded: &str, kind: &str| {
        hex::decode(encoded.split_whitespace().collect::<String>()).map_err(|error| {
            SearchEvalError::Contract(format!("decode packaged evaluator Git {kind}: {error}"))
        })
    };
    let pack = decode(
        include_str!("../assets/git/evaluator.pack.hex"),
        "object pack",
    )?;
    let index = decode(
        include_str!("../assets/git/evaluator.idx.hex"),
        "object index",
    )?;
    fs::write(pack_root.join(format!("pack-{PACK_ID}.pack")), pack).map_err(|error| {
        SearchEvalError::Contract(format!("write packaged evaluator Git object pack: {error}"))
    })?;
    fs::write(pack_root.join(format!("pack-{PACK_ID}.idx")), index).map_err(|error| {
        SearchEvalError::Contract(format!(
            "write packaged evaluator Git object index: {error}"
        ))
    })?;
    fs::write(git.join("HEAD"), format!("{SOURCE_COMMIT}\n")).map_err(|error| {
        SearchEvalError::Contract(format!("write packaged evaluator Git HEAD: {error}"))
    })?;
    fs::write(
        git.join("config"),
        b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
    )
    .map_err(|error| {
        SearchEvalError::Contract(format!("write packaged evaluator Git config: {error}"))
    })?;
    Ok(())
}

pub(crate) fn admitted_scope(_root: &Path) -> Option<ResolvedScope> {
    ResolvedScope::new(
        ProjectId::new("project.semantic-evaluator-assets").ok()?,
        RepositoryId::new("repository.semantic-evaluator-assets").ok()?,
        WorktreeId::new("worktree.semantic-evaluator-assets").ok()?,
        None,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_metadata_load_does_not_materialize_runtime_assets() {
        let before = super::MATERIALIZATION_COUNT.with(std::cell::Cell::get);

        crate::load_default_evaluated_profile_material("query-fallback")
            .expect("packaged profile metadata");

        let after = super::MATERIALIZATION_COUNT.with(std::cell::Cell::get);
        assert_eq!(after, before);
    }

    #[test]
    fn packaged_workload_is_independent_of_mounted_project() {
        let unrelated = tempfile::tempdir().expect("unrelated project");
        std::fs::write(
            unrelated.path().join("Cargo.toml"),
            "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\n",
        )
        .expect("unrelated project content");

        let summary = crate::validate_default_activation_workload(unrelated.path())
            .expect("packaged evaluator workload");
        assert_eq!(summary.query_count, 28);
        assert_eq!(summary.profile_count, 3);
    }

    #[test]
    fn packaged_evaluator_runs_against_an_unrelated_project() {
        let unrelated = tempfile::tempdir().expect("unrelated project");
        std::fs::write(
            unrelated.path().join("Cargo.toml"),
            "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\n",
        )
        .expect("unrelated project content");
        let profiles = vec!["query-fallback".to_owned()];

        let report = crate::compare_default_direct(unrelated.path(), Some(&profiles))
            .expect("packaged evaluator execution");
        assert_eq!(report.command, "compare");
        assert!(!report.profiles.is_empty());
    }
}
