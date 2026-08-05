use std::fs;
use std::process::Command;

use tempfile::tempdir;

use super::{FixedGitIndexRunner, NativeGitIndexError};

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

#[cfg(unix)]
#[test]
fn merge_and_reference_transaction_hooks_are_applicable() {
    use std::os::unix::fs::PermissionsExt;

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
    for hook in ["pre-merge-commit", "reference-transaction"] {
        let path = directory.path().join(".git").join("hooks").join(hook);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("hook");
        let mut permissions = fs::metadata(&path).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("hook permissions");
        assert!(
            runner
                .has_applicable_commit_hooks()
                .expect("hook classification")
        );
        fs::remove_file(path).expect("remove hook");
        assert!(!runner.has_applicable_commit_hooks().expect("hook removed"));
    }
}
