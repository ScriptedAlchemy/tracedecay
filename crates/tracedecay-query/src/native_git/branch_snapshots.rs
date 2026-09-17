//! Native `gix` reads behind the local branch-ref snapshot values in
//! `tracedecay_contracts::branch_snapshots`.

use std::path::Path;

use gix::bstr::ByteSlice as _;
use tracedecay_contracts::branch_snapshots::{
    BranchSnapshot, LocalBranchReadControlV1, LocalBranchRevisionV1, LocalBranchSnapshotErrorV1,
    LocalBranchSnapshotsV1,
};
use tracedecay_domain::GitOidV1;

const MAX_LOCAL_BRANCH_SCAN: usize = 4_096;

/// Resolves one exact local `refs/heads/*` branch tip to commit and tree.
pub fn local_branch_revision_controlled(
    project_root: &Path,
    branch: &str,
    control: &LocalBranchReadControlV1,
) -> Result<LocalBranchRevisionV1, LocalBranchSnapshotErrorV1> {
    control.termination().map_or(Ok(()), Err)?;
    let repo =
        gix::open(project_root).map_err(|_| LocalBranchSnapshotErrorV1::RepositoryUnavailable)?;
    local_branch_revision_in_repository(&repo, branch, control)
}

fn local_branch_revision_in_repository(
    repo: &gix::Repository,
    branch: &str,
    control: &LocalBranchReadControlV1,
) -> Result<LocalBranchRevisionV1, LocalBranchSnapshotErrorV1> {
    let refname = format!("refs/heads/{branch}");
    gix::refs::FullName::try_from(refname.as_str()).map_err(|_| {
        LocalBranchSnapshotErrorV1::InvalidReference {
            branch: branch.to_owned(),
        }
    })?;
    let mut reference = repo
        .try_find_reference(&refname)
        .map_err(|_| LocalBranchSnapshotErrorV1::ReferenceUnavailable {
            branch: branch.to_owned(),
        })?
        .ok_or_else(|| LocalBranchSnapshotErrorV1::NotFound {
            branch: branch.to_owned(),
        })?;
    control.termination().map_or(Ok(()), Err)?;
    let commit =
        reference
            .peel_to_commit()
            .map_err(|_| LocalBranchSnapshotErrorV1::InvalidReference {
                branch: branch.to_owned(),
            })?;
    let commit_id = commit.id().to_string();
    let tree_id = commit
        .tree_id()
        .map_err(|_| LocalBranchSnapshotErrorV1::ReferenceUnavailable {
            branch: branch.to_owned(),
        })?
        .to_string();
    control.termination().map_or(Ok(()), Err)?;
    Ok(LocalBranchRevisionV1 {
        commit: GitOidV1::new(commit_id).map_err(|_| {
            LocalBranchSnapshotErrorV1::InvalidReference {
                branch: branch.to_owned(),
            }
        })?,
        tree: GitOidV1::new(tree_id).map_err(|_| LocalBranchSnapshotErrorV1::InvalidReference {
            branch: branch.to_owned(),
        })?,
    })
}

/// Lists one stable lexical page from a bounded complete local-ref snapshot.
#[hotpath::measure(label = "query.native_git.branch_snapshots.list")]
pub fn local_branch_snapshots_controlled(
    project_root: &Path,
    control: &LocalBranchReadControlV1,
) -> Result<LocalBranchSnapshotsV1, LocalBranchSnapshotErrorV1> {
    if control.max_refs == 0 {
        return Err(LocalBranchSnapshotErrorV1::InvalidLimit);
    }
    if let Some(after) = control.after.as_deref() {
        let refname = format!("refs/heads/{after}");
        gix::refs::FullName::try_from(refname.as_str()).map_err(|_| {
            LocalBranchSnapshotErrorV1::InvalidReference {
                branch: after.to_owned(),
            }
        })?;
    }
    control.termination().map_or(Ok(()), Err)?;
    let repo =
        gix::open(project_root).map_err(|_| LocalBranchSnapshotErrorV1::RepositoryUnavailable)?;
    let references = repo
        .references()
        .map_err(|_| LocalBranchSnapshotErrorV1::EnumerationUnavailable)?;
    let branches = references
        .local_branches()
        .map_err(|_| LocalBranchSnapshotErrorV1::EnumerationUnavailable)?;
    let mut names = Vec::new();
    for reference in branches {
        control.termination().map_or(Ok(()), Err)?;
        if names.len() == MAX_LOCAL_BRANCH_SCAN {
            return Err(LocalBranchSnapshotErrorV1::CapacityExceeded {
                examined: names.len() + 1,
            });
        }
        let reference =
            reference.map_err(|_| LocalBranchSnapshotErrorV1::EnumerationUnavailable)?;
        names.push(reference.name().shorten().to_str_lossy().into_owned());
    }
    names.sort_unstable();
    names.dedup();
    let examined = names.len();
    let start = control.after.as_deref().map_or(0, |after| {
        names.partition_point(|name| name.as_str() <= after)
    });
    let mut selected = names
        .into_iter()
        .skip(start)
        .take(control.max_refs + 1)
        .collect::<Vec<_>>();
    let truncated = selected.len() > control.max_refs;
    if truncated {
        selected.truncate(control.max_refs);
    }
    let next_after = truncated.then(|| selected.last().cloned()).flatten();
    let mut snapshots = Vec::with_capacity(selected.len());
    for name in selected {
        let revision = local_branch_revision_in_repository(&repo, &name, control)?;
        snapshots.push(BranchSnapshot {
            name,
            commit: revision.commit.as_str().to_owned(),
            tree: revision.tree.as_str().to_owned(),
        });
    }
    Ok(LocalBranchSnapshotsV1 {
        snapshots,
        examined,
        truncated,
        next_after,
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tracedecay_contracts::{CancellationSignal, Deadline, clock};

    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_owned()
    }

    fn control(max_refs: usize) -> LocalBranchReadControlV1 {
        LocalBranchReadControlV1 {
            max_refs,
            after: None,
            deadline: None,
            cancellation: None,
        }
    }

    #[test]
    fn selected_local_ref_resolves_independently_of_active_head() {
        let root = tempfile::tempdir().expect("tempdir");
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("fixture.txt"), "base\n").expect("write base");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        git(root.path(), &["branch", "selected"]);
        let selected = git(root.path(), &["rev-parse", "refs/heads/selected"]);
        std::fs::write(root.path().join("fixture.txt"), "active\n").expect("write active");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "active",
            ],
        );

        let resolved = local_branch_revision_controlled(root.path(), "selected", &control(1))
            .expect("selected ref");
        assert_eq!(resolved.commit.as_str(), selected);
        assert_ne!(
            resolved.commit.as_str(),
            git(root.path(), &["rev-parse", "HEAD"])
        );
        assert!(local_branch_revision_controlled(root.path(), "missing", &control(1)).is_err());
    }

    #[test]
    fn many_refs_are_bounded_and_termination_is_typed() {
        let root = tempfile::tempdir().expect("tempdir");
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("fixture.txt"), "base\n").expect("write base");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        let head = git(root.path(), &["rev-parse", "HEAD"]);
        for index in 0..96 {
            std::fs::write(
                root.path()
                    .join(".git/refs/heads")
                    .join(format!("many-{index:03}")),
                format!("{head}\n"),
            )
            .expect("write loose ref");
        }
        let page = local_branch_snapshots_controlled(
            root.path(),
            &LocalBranchReadControlV1 {
                max_refs: 32,
                after: None,
                deadline: None,
                cancellation: None,
            },
        )
        .expect("bounded branch page");
        assert_eq!(page.snapshots.len(), 32);
        assert_eq!(page.examined, 97);
        assert!(page.truncated);
        assert!(page.next_after.is_some());

        let cancellation = CancellationSignal::active("cancel.branch-refs").expect("cancellation");
        cancellation.cancel(clock::now_micros());
        assert_eq!(
            local_branch_snapshots_controlled(
                root.path(),
                &LocalBranchReadControlV1 {
                    max_refs: 32,
                    after: None,
                    deadline: None,
                    cancellation: Some(cancellation),
                },
            ),
            Err(LocalBranchSnapshotErrorV1::Cancelled)
        );
        let expired = Deadline::new(clock::now_micros()).expect("deadline");
        assert_eq!(
            local_branch_snapshots_controlled(
                root.path(),
                &LocalBranchReadControlV1 {
                    max_refs: 32,
                    after: None,
                    deadline: Some(expired),
                    cancellation: None,
                },
            ),
            Err(LocalBranchSnapshotErrorV1::TimedOut)
        );
        assert!(matches!(
            local_branch_revision_controlled(root.path(), "missing", &control(1)),
            Err(LocalBranchSnapshotErrorV1::NotFound { .. })
        ));
        assert!(matches!(
            local_branch_revision_controlled(root.path(), "bad..name", &control(1)),
            Err(LocalBranchSnapshotErrorV1::InvalidReference { .. })
        ));
    }

    #[test]
    fn packed_and_loose_refs_share_one_stable_lexical_page() {
        let root = tempfile::tempdir().expect("tempdir");
        git(root.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(root.path().join("fixture.txt"), "base\n").expect("write base");
        git(root.path(), &["add", "fixture.txt"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        git(root.path(), &["branch", "alpha"]);
        git(root.path(), &["branch", "charlie"]);
        git(root.path(), &["pack-refs", "--all", "--prune"]);
        git(root.path(), &["branch", "beta"]);
        git(root.path(), &["branch", "zulu"]);

        let page = local_branch_snapshots_controlled(
            root.path(),
            &LocalBranchReadControlV1 {
                max_refs: 2,
                after: None,
                deadline: None,
                cancellation: None,
            },
        )
        .expect("mixed ref page");

        assert_eq!(
            page.snapshots
                .iter()
                .map(|snapshot| snapshot.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(
            page.examined, 5,
            "a stable prefix must be selected from the complete bounded ref set"
        );
        assert!(page.truncated);
        assert_eq!(page.next_after.as_deref(), Some("beta"));

        let next = local_branch_snapshots_controlled(
            root.path(),
            &LocalBranchReadControlV1 {
                max_refs: 2,
                after: page.next_after,
                deadline: None,
                cancellation: None,
            },
        )
        .expect("next mixed ref page");
        assert_eq!(
            next.snapshots
                .iter()
                .map(|snapshot| snapshot.name.as_str())
                .collect::<Vec<_>>(),
            ["charlie", "main"]
        );
        assert!(next.truncated);
        assert_eq!(next.next_after.as_deref(), Some("main"));
        let final_page = local_branch_snapshots_controlled(
            root.path(),
            &LocalBranchReadControlV1 {
                max_refs: 2,
                after: next.next_after,
                deadline: None,
                cancellation: None,
            },
        )
        .expect("final mixed ref page");
        assert_eq!(
            final_page
                .snapshots
                .iter()
                .map(|snapshot| snapshot.name.as_str())
                .collect::<Vec<_>>(),
            ["zulu"]
        );
        assert!(!final_page.truncated);
        assert!(final_page.next_after.is_none());
    }
}
