use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
use tracedecay_query::search_quality::candidate_output::WORKLOAD_RELATIVE;
use tracedecay_query::search_quality::{
    CandidateWorkloadV1, SearchEvalError, load_candidate_workload, packaged,
};

const SOURCE_COMMIT: &str = "8312618fee8109b16be09e65f45118b4e550fa14";
const PACK_ID: &str = "184f6ca1eafd40e7889d15a20b7a5c861e80a47b";
const CONTEXT_PACK_ID: &str = "f3c7116c6c911657b2b0d933cbf89ae00dfdee64";

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
        self.root.join(WORKLOAD_RELATIVE)
    }

    pub(crate) fn workload(&self) -> &CandidateWorkloadV1 {
        &self.workload
    }
}

#[tracing::instrument(name = "search_eval.package.materialize", level = "trace", skip_all)]
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
    {
        let _span = tracing::trace_span!("search_eval.package.verify").entered();
        {
            let materialized_workload =
                load_candidate_workload(&directory.path().join(WORKLOAD_RELATIVE))?;
            if materialized_workload != workload {
                return Err(SearchEvalError::Contract(
                    "materialized evaluator workload differs from packaged bytes".to_owned(),
                ));
            }
            Ok::<(), SearchEvalError>(())
        }
    }?;
    Ok(PackagedEvaluatorAssets {
        root: directory.path().to_path_buf(),
        _directory: directory,
        workload,
    })
}

/// Writes the checked-in evaluator object pack into an existing `.git`
/// directory.
///
/// The pack is a genuine sparse subset of the product repository: it carries
/// the exact commit, tree spine, and blob objects the checked-in workload pins
/// for its historical queries, under their real object identities. Writing it
/// into a repository therefore only backfills objects. It can never replace or
/// contradict an object the repository already holds.
///
/// The evaluator needs that backfill because the pinned historical commits live
/// on a long-running integration branch. A rebase there re-parents the commit,
/// which mints a new commit identity and leaves the pinned one unreachable from
/// every ref. Local clones keep resolving it, because cloning a path on the same
/// filesystem hard-links the whole object store including unreachable objects.
/// A wire-protocol clone, which is what CI checks out, transfers only
/// ref-reachable objects and drops it. Materializing the checked-in pack keeps
/// the pinned history resolvable in both.
fn write_checked_in_object_pack(git: &Path) -> Result<(), SearchEvalError> {
    let pack_root = git.join("objects/pack");
    fs::create_dir_all(&pack_root).map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator Git authority: {error}"))
    })?;
    let decode = |encoded: &str, kind: &str| {
        hex::decode(encoded.split_whitespace().collect::<String>()).map_err(|error| {
            SearchEvalError::Contract(format!("decode packaged evaluator Git {kind}: {error}"))
        })
    };
    for (id, pack, index) in [
        (
            PACK_ID,
            include_str!("../assets/git/evaluator.pack.hex"),
            include_str!("../assets/git/evaluator.idx.hex"),
        ),
        (
            CONTEXT_PACK_ID,
            include_str!("../assets/git/evaluator-context.pack.hex"),
            include_str!("../assets/git/evaluator-context.idx.hex"),
        ),
    ] {
        let pack = decode(pack, "object pack")?;
        let index = decode(index, "object index")?;
        fs::write(pack_root.join(format!("pack-{id}.pack")), pack).map_err(|error| {
            SearchEvalError::Contract(format!("write packaged evaluator Git object pack: {error}"))
        })?;
        fs::write(pack_root.join(format!("pack-{id}.idx")), index).map_err(|error| {
            SearchEvalError::Contract(format!(
                "write packaged evaluator Git object index: {error}"
            ))
        })?;
    }
    Ok(())
}

fn materialize_git_authority(root: &Path) -> Result<(), SearchEvalError> {
    let git = root.join(".git");
    let refs_root = git.join("refs");
    fs::create_dir_all(&refs_root).map_err(|error| {
        SearchEvalError::Contract(format!("create packaged evaluator Git refs: {error}"))
    })?;
    write_checked_in_object_pack(&git)?;
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
        ProjectId::new("project.packaged-evaluator-assets").ok()?,
        RepositoryId::new("repository.packaged-evaluator-assets").ok()?,
        WorktreeId::new("worktree.packaged-evaluator-assets").ok()?,
        None,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_query::search_quality::{
        DirectEvaluationStatusV1, QUERY_BASELINE_PROFILE, packaged,
    };

    #[test]
    fn source_and_packaged_evaluations_preserve_corpus_and_fallback_receipts() {
        let assets = super::materialize().expect("materialized evaluator fixture");
        assert!(crate::root_admitted_corpus_scope(assets.root()).is_none());
        assert!(
            tracedecay_runtime_core::storage::write_repository_identity_marker(
                assets.root(),
                "project.search-eval-source-fixture",
            )
            .expect("enroll the temporary evaluator repository")
        );
        let scope = crate::root_admitted_corpus_scope(assets.root())
            .expect("source evaluation admits the fixture's authoritative identity");
        assert_eq!(
            scope.project_id.as_str(),
            "project.search-eval-source-fixture"
        );

        let profiles = vec![QUERY_BASELINE_PROFILE.to_owned()];
        let source = crate::compare_direct(
            assets.root(),
            Some(&assets.workload_path()),
            Some(&profiles),
            crate::root_admitted_corpus_scope,
        )
        .expect("source-backed evaluator execution");
        // The packaged entry point must carry its own workload, corpus, and
        // Git authority, independently of the enrolled source fixture above.
        let packaged =
            crate::compare_default_direct(Some(&profiles)).expect("packaged evaluator execution");
        assert_eq!(packaged.command, "compare");
        assert_eq!(
            packaged
                .profiles
                .iter()
                .map(|profile| (profile.profile_id.as_str(), profile.partition.as_str()))
                .collect::<Vec<_>>(),
            [
                ("query-fallback", "train"),
                ("query-fallback", "validation")
            ]
        );

        assert_eq!(source.workload_digest, packaged::WORKLOAD_SHA256);
        assert_eq!(source.workload_digest, packaged.workload_digest);
        assert_eq!(source.corpus_digest, packaged.corpus_digest);
        assert_eq!(source.execution_contract, packaged.execution_contract);
        assert_eq!(
            source.profile_material_digests,
            packaged.profile_material_digests
        );

        let workload = assets.workload();
        let expected_receipts = workload
            .expected_query_fallback_digests
            .iter()
            .collect::<BTreeMap<_, _>>();
        for report in [&source, &packaged] {
            assert_eq!(report.command, "compare");
            assert_eq!(report.status, DirectEvaluationStatusV1::Fail);
            assert_eq!(
                report.fixture_source_repository_commit,
                workload.source_repository_commit
            );
            assert_eq!(
                report.fixture_source_repository_tree,
                workload.source_repository_tree
            );
            assert!(
                report
                    .profiles
                    .iter()
                    .any(|profile| profile.failed_queries > 0)
            );
            assert_eq!(report.profiles.len(), expected_receipts.len());
            for profile in &report.profiles {
                assert_eq!(profile.profile_id, QUERY_BASELINE_PROFILE);
                assert!(profile.fallback_stable);
                assert!(profile.fallback_matches_expected);
            }

            let observed_receipts = report
                .raw_outputs
                .iter()
                .map(|output| {
                    assert_eq!(output.profile_id, QUERY_BASELINE_PROFILE);
                    assert_eq!(output.workload_digest, report.workload_digest);
                    assert_eq!(output.corpus_digest, report.corpus_digest);
                    assert!(output.query_fallback_matches_expected);
                    assert_eq!(output.fallback_digest, output.query_fallback_digest);
                    assert_eq!(
                        output.query_fallback_digest,
                        output.expected_query_fallback_digest
                    );
                    (&output.partition, &output.query_fallback_digest)
                })
                .collect::<BTreeMap<_, _>>();
            assert_eq!(observed_receipts.len(), report.raw_outputs.len());
            assert_eq!(observed_receipts, expected_receipts);
        }
    }
}
