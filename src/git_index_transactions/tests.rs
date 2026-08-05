use std::fs;
use std::process::Command;

use tempfile::tempdir;
use tracedecay_domain::GitOidV1;

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
fn configured_openpgp_program_keeps_signing_preview_only() {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args([
                "config",
                "--local",
                "gpg.openpgp.program",
                "external-provider",
            ])
            .status()
            .expect("git config starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    assert!(
        !runner
            .signing_key_available("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("signing classification")
    );
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

#[cfg(unix)]
#[test]
fn linked_worktree_uses_common_directory_hooks() {
    use std::os::unix::fs::PermissionsExt;

    let repository = tempdir().expect("repository");
    let linked_parent = tempdir().expect("linked worktree parent");
    let linked = linked_parent.path().join("linked");
    assert!(
        Command::new("git")
            .current_dir(repository.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(repository.path().join("tracked.txt"), b"tracked\n").expect("tracked file");
    assert!(
        Command::new("git")
            .current_dir(repository.path())
            .args(["add", "--", "tracked.txt"])
            .status()
            .expect("git add starts")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(repository.path())
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
    assert!(
        Command::new("git")
            .current_dir(repository.path())
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "linked",
                linked.to_str().expect("utf-8 linked path"),
            ])
            .status()
            .expect("git worktree add starts")
            .success()
    );
    let hook = repository.path().join(".git/hooks/pre-merge-commit");
    fs::write(&hook, b"#!/bin/sh\nexit 0\n").expect("hook");
    let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).expect("hook permissions");

    let runner = FixedGitIndexRunner::new(&linked).expect("linked runner");
    assert!(
        runner
            .has_applicable_commit_hooks()
            .expect("linked hook classification")
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
fn unrelated_ref_drift_rejects_atomic_destination_update() {
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
    let value = |arguments: &[&str]| {
        let output = Command::new("git")
            .current_dir(directory.path())
            .args(arguments)
            .output()
            .expect("git command starts");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("utf-8 git output")
            .trim()
            .to_owned()
    };
    let branch = value(&["symbolic-ref", "-q", "HEAD"]);
    let old = GitOidV1::new(value(&["rev-parse", "HEAD"])).expect("old oid");
    let tree = value(&["rev-parse", "HEAD^{tree}"]);
    let created = Command::new("git")
        .current_dir(directory.path())
        .args([
            "-c",
            "user.name=TraceDecay",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit-tree",
            &tree,
            "-p",
            old.as_str(),
            "-m",
            "candidate",
        ])
        .output()
        .expect("commit-tree starts");
    assert!(created.status.success());
    let new_value = GitOidV1::new(
        String::from_utf8(created.stdout)
            .expect("utf-8 commit")
            .trim(),
    )
    .expect("new oid");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["branch", "other", old.as_str()])
            .status()
            .expect("branch starts")
            .success()
    );
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    let expected_refs = runner.ref_snapshot().expect("ref snapshot");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["update-ref", "refs/heads/other", new_value.as_str()])
            .status()
            .expect("unrelated update starts")
            .success()
    );

    assert!(
        runner
            .update_ref_with_namespace_cas(&branch, &new_value, &old, &expected_refs)
            .is_err()
    );
    assert_eq!(value(&["rev-parse", &branch]), old.as_str());
}
