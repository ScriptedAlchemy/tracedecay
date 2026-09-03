use std::fs;
use std::process::Command;

use super::{FixedGitIndexRunner, NativeGitIndexError};
use tempfile::tempdir;

#[test]
fn existing_native_index_lock_blocks_mutation_before_git_runs() {
    let directory = tempdir().expect("temporary repository");
    let initialized = Command::new("git")
        .current_dir(directory.path())
        .args(["init", "--quiet"])
        .status()
        .expect("git init starts");
    assert!(initialized.success());

    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    fs::write(runner.index_lock_path(), b"external Git transaction").expect("index lock");

    assert!(matches!(
        runner.ensure_index_unlocked(),
        Err(NativeGitIndexError::IndexLocked)
    ));
}

#[test]
fn unreadable_optional_git_metadata_is_not_treated_as_absent() {
    let directory = tempdir().expect("temporary repository");
    let initialized = Command::new("git")
        .current_dir(directory.path())
        .args(["init", "--quiet"])
        .status()
        .expect("git init starts");
    assert!(initialized.success());
    fs::create_dir(directory.path().join(".gitmodules")).expect("metadata directory");

    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    assert!(matches!(
        runner.submodule_digest(),
        Err(NativeGitIndexError::Io(_))
    ));
}

#[test]
fn commit_boundary_errors_remain_distinct_from_safe_native_failures() {
    let safe = NativeGitIndexError::StaleRepositoryState;
    let unknown = safe.into_commit_boundary_unknown("index publish");
    assert!(unknown.is_commit_boundary_unknown());
    assert!(!NativeGitIndexError::PatchDoesNotMatchHunk.is_commit_boundary_unknown());
}

#[test]
fn repository_attributes_digest_tracks_effective_attributes() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(directory.path().join("tracked.txt"), b"tracked\n").expect("tracked file");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "tracked.txt"])
            .status()
            .expect("git add starts")
            .success()
    );

    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    let before = runner.attributes_digest().expect("attributes before");
    fs::write(
        directory.path().join(".gitattributes"),
        b"tracked.txt merge=binary\n",
    )
    .expect("attributes");
    let after = runner.attributes_digest().expect("attributes after");

    assert_ne!(before, after);
}

#[test]
fn configured_merge_diff_and_filter_drivers_are_preview_only() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");

    for key in [
        "diff.external",
        "merge.tracedecay.driver",
        "diff.tracedecay.command",
        "diff.tracedecay.textconv",
        "filter.tracedecay.clean",
        "filter.tracedecay.smudge",
        "filter.tracedecay.process",
    ] {
        assert!(
            Command::new("git")
                .current_dir(directory.path())
                .args(["config", "--local", key, "external-driver"])
                .status()
                .expect("git config starts")
                .success()
        );
        assert!(
            runner
                .has_external_drivers()
                .expect("driver classification")
        );
        assert!(
            Command::new("git")
                .current_dir(directory.path())
                .args(["config", "--local", "--unset-all", key])
                .status()
                .expect("git config unset starts")
                .success()
        );
        assert!(!runner.has_external_drivers().expect("driver removed"));
    }
}

#[test]
fn configuration_and_filesystem_capability_digests_are_distinct() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    let configuration_before = runner.configuration_digest().expect("configuration");
    let capabilities_before = runner
        .filesystem_capabilities_digest()
        .expect("filesystem capabilities");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["config", "--local", "tracedecay.fixture", "changed"])
            .status()
            .expect("git config starts")
            .success()
    );
    assert_ne!(
        configuration_before,
        runner
            .configuration_digest()
            .expect("changed configuration")
    );
    assert_eq!(
        capabilities_before,
        runner
            .filesystem_capabilities_digest()
            .expect("unchanged filesystem capabilities")
    );
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["config", "--local", "core.filemode", "false"])
            .status()
            .expect("git config starts")
            .success()
    );
    assert_ne!(
        capabilities_before,
        runner
            .filesystem_capabilities_digest()
            .expect("changed filesystem capabilities")
    );
}

#[test]
fn repository_control_redirection_never_retargets_a_retained_runner() {
    let retained = tempdir().expect("retained repository");
    let foreign = tempdir().expect("foreign repository");
    for repository in [retained.path(), foreign.path()] {
        assert!(
            Command::new("git")
                .current_dir(repository)
                .args(["init", "--quiet"])
                .status()
                .expect("git init starts")
                .success()
        );
    }
    let runner = FixedGitIndexRunner::new(retained.path()).expect("runner");
    let retained_git_dir = retained.path().join(".git");
    let displaced_git_dir = retained.path().join(".git.retained");
    fs::rename(&retained_git_dir, &displaced_git_dir).expect("displace retained control directory");
    fs::write(
        &retained_git_dir,
        format!("gitdir: {}\n", foreign.path().join(".git").display()),
    )
    .expect("foreign repository redirection");

    assert!(
        runner.refs_digest().is_err(),
        "the runner must fail closed instead of following the replacement .git authority"
    );
}

#[test]
fn tracked_worktree_digest_is_independent_of_index_publication() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(directory.path().join("tracked.txt"), b"before\n").expect("tracked file");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "tracked.txt"])
            .status()
            .expect("git add starts")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args([
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ])
            .status()
            .expect("git commit starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    fs::write(directory.path().join("tracked.txt"), b"after\n").expect("changed file");
    let before_stage = runner
        .tracked_worktree_digest()
        .expect("worktree digest before stage");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "tracked.txt"])
            .status()
            .expect("git add starts")
            .success()
    );
    let after_stage = runner
        .tracked_worktree_digest()
        .expect("worktree digest after stage");
    assert_eq!(before_stage, after_stage);

    fs::write(directory.path().join("tracked.txt"), b"concurrent drift\n").expect("drift file");
    assert_ne!(
        after_stage,
        runner
            .tracked_worktree_digest()
            .expect("worktree digest after drift")
    );
}

#[test]
fn worktree_digest_binds_added_and_renamed_paths_across_index_publication() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(directory.path().join("old.txt"), b"old\n").expect("tracked file");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "old.txt"])
            .status()
            .expect("git add starts")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args([
                "-c",
                "user.name=TraceDecay",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ])
            .status()
            .expect("git commit starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");

    fs::write(directory.path().join("added.txt"), b"added\n").expect("added file");
    let added_before_stage = runner
        .tracked_worktree_digest()
        .expect("added path before stage");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "added.txt"])
            .status()
            .expect("git add starts")
            .success()
    );
    assert_eq!(
        added_before_stage,
        runner
            .tracked_worktree_digest()
            .expect("added path after stage"),
        "publishing an added path to the index must retain the same byte manifest"
    );
    fs::write(directory.path().join("added.txt"), b"drifted\n").expect("added path drift");
    assert_ne!(
        added_before_stage,
        runner
            .tracked_worktree_digest()
            .expect("added path after drift")
    );

    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["mv", "old.txt", "renamed.txt"])
            .status()
            .expect("git mv starts")
            .success()
    );
    let renamed = runner
        .tracked_worktree_digest()
        .expect("renamed path manifest");
    fs::write(directory.path().join("old.txt"), b"collision\n").expect("old-name collision");
    assert_ne!(
        renamed,
        runner
            .tracked_worktree_digest()
            .expect("renamed path collision manifest"),
        "the retained HEAD name must remain bound during a rename"
    );
}

#[test]
fn untracked_and_ignored_name_digests_bind_namespace_collisions() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(directory.path().join(".gitignore"), b"ignored-*\n").expect("ignore rules");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", ".gitignore"])
            .status()
            .expect("git add starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    assert_eq!(
        runner.untracked_name_digest().expect("untracked names"),
        None
    );
    assert_eq!(runner.ignored_name_digest().expect("ignored names"), None);

    fs::write(directory.path().join("visible-a"), b"one\n").expect("untracked path");
    let untracked_a = runner
        .untracked_name_digest()
        .expect("first untracked names");
    fs::rename(
        directory.path().join("visible-a"),
        directory.path().join("visible-b"),
    )
    .expect("rename untracked path");
    assert_ne!(
        untracked_a,
        runner
            .untracked_name_digest()
            .expect("renamed untracked names")
    );

    fs::write(directory.path().join("ignored-a"), b"one\n").expect("first ignored path");
    let ignored_a = runner.ignored_name_digest().expect("first ignored names");
    fs::write(directory.path().join("ignored-b"), b"two\n").expect("second ignored path");
    assert_ne!(
        ignored_a,
        runner.ignored_name_digest().expect("second ignored names")
    );
}
