use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
use tracedecay_query::search_quality::{CandidateWorkloadV1, SearchEvalError, packaged};

const SOURCE_COMMIT: &str = "8312618fee8109b16be09e65f45118b4e550fa14";
const PACK_ID: &str = "184f6ca1eafd40e7889d15a20b7a5c861e80a47b";

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
        self.root
            .join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json")
    }

    pub(crate) fn workload(&self) -> &CandidateWorkloadV1 {
        &self.workload
    }
}

#[hotpath::measure(label = "search_eval.package.materialize")]
pub(crate) fn materialize() -> Result<PackagedEvaluatorAssets, SearchEvalError> {
    let workload = packaged::load_workload()?;
    let directory = tempfile::tempdir().map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator root: {error}"))
    })?;
    for (relative, bytes) in packaged::packaged_evaluator_files() {
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
    hotpath::measure_block!("search_eval.package.verify", {
        let materialized_workload = tracedecay_query::search_quality::load_candidate_workload(
            &directory
                .path()
                .join("tests/fixtures/search_quality/query-semantic-candidate-workload-v1.json"),
        )?;
        if materialized_workload != workload {
            return Err(SearchEvalError::Contract(
                "materialized evaluator workload differs from packaged bytes".to_owned(),
            ));
        }
        Ok::<(), SearchEvalError>(())
    })?;
    Ok(PackagedEvaluatorAssets {
        root: directory.path().to_path_buf(),
        _directory: directory,
        workload,
    })
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
