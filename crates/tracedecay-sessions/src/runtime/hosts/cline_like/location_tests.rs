use super::*;
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
};

fn mixed_identity(path: &Path) -> GitRepositoryIdentityOutcome {
    if path == Path::new("/unavailable") {
        GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
    } else {
        GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
            worktree_root: path.to_path_buf(),
            git_dir: path.join(".git"),
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

/// The storage roots are the durable transcript cursor keys, so each one must
/// be spelled the way the host spells a path it built itself.
///
/// This executes everywhere but can only fail on Windows: `Path::join` renders
/// `join("User/globalStorage")` as `...\User/globalStorage` there, while on a
/// host whose separator is `/` the two spellings are the same string. The
/// assertion is the Windows guard the `/`-joined literals lost.
#[test]
fn storage_roots_are_joined_per_component() {
    let home = Path::new("home");
    let storage = |extension| {
        crate::host_ports::vscode_data_dir(home)
            .join("User")
            .join("globalStorage")
            .join(extension)
            .join("tasks")
    };

    assert_eq!(
        ClineLikeSource::cline_with_home(home).storage_roots,
        vec![storage("saoudrizwan.claude-dev")]
    );
    assert_eq!(
        ClineLikeSource::roo_code_with_home(home).storage_roots,
        vec![storage("rooveterinaryinc.roo-cline")]
    );
    assert_eq!(
        ClineLikeSource::kilo_with_home(home).storage_roots,
        vec![
            storage("kilocode.kilo-code"),
            home.join(".kilocode")
                .join("cli")
                .join("global")
                .join("tasks"),
        ]
    );
}
