use super::*;
use tracedecay_runtime_core::worktree::{GitRepoIdentity, GitRepoIdentityOutcome};

fn mixed_identity(path: &Path) -> GitRepoIdentityOutcome {
    if path == Path::new("/unavailable") {
        GitRepoIdentityOutcome::Unknown
    } else {
        GitRepoIdentityOutcome::Resolved(GitRepoIdentity {
            worktree_root: path.to_path_buf(),
            common_dir: PathBuf::from("/shared/.git"),
        })
    }
}

#[test]
fn definitive_metadata_path_match_overrides_unknown_auxiliary_path() {
    let metadata = serde_json::json!({
        "projectPath": "/match",
        "workspaceDirectory": "/unavailable"
    });
    let tmp = tempfile::TempDir::new().unwrap();
    let mut source = ClineLikeSource::cline_with_home(tmp.path());
    source.project_matchers = ProjectRootMatcherCache::with_identity_resolver(mixed_identity);

    assert_eq!(
        source.snapshot_location_from_metadata(&metadata, Path::new("/project")),
        Some(PathBuf::from("/match"))
    );
}
