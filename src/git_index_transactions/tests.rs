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

#[cfg(unix)]
#[test]
fn concurrently_created_ref_aborts_before_destination_commit() {
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::Duration;

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
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
    let expected_refs = runner.ref_snapshot().expect("ref snapshot");

    let hook = directory.path().join(".git/hooks/reference-transaction");
    let prepared_signal = directory.path().join(".git/ref-prepared.signal");
    let race_release = directory.path().join(".git/ref-race.release");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = prepared ]; then\n\
               while read old_value new_value ref_name; do\n\
                 if [ \"$ref_name\" = \"{branch}\" ]; then\n\
                   : > \"{}\"\n\
                   attempt=0\n\
                   while [ ! -e \"{}\" ] && [ \"$attempt\" -lt 1000 ]; do\n\
                     attempt=$((attempt + 1))\n\
                     sleep 0.01\n\
                   done\n\
                 fi\n\
               done\n\
             fi\n\
             exit 0\n",
            prepared_signal.display(),
            race_release.display(),
        ),
    )
    .expect("reference transaction hook");
    let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).expect("hook permissions");

    let race_root = directory.path().to_owned();
    let race_old = old.clone();
    let race_signal = prepared_signal.clone();
    let race_release_path = race_release.clone();
    let concurrent = thread::spawn(move || {
        for _ in 0..2_000 {
            if race_signal.exists() {
                let created = Command::new("git")
                    .current_dir(&race_root)
                    .args(["branch", "concurrent", race_old.as_str()])
                    .status()
                    .expect("concurrent branch starts");
                assert!(created.success());
                fs::write(race_release_path, b"release\n").expect("release prepared transaction");
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("destination ref transaction never reached prepared state");
    });

    let result = runner.update_ref_with_namespace_cas(&branch, &new_value, &old, &expected_refs);
    concurrent.join().expect("concurrent ref writer");

    assert!(matches!(
        result,
        Err(NativeGitIndexError::StaleRepositoryState)
    ));
    assert_eq!(value(&["rev-parse", &branch]), old.as_str());
    assert_eq!(value(&["rev-parse", "refs/heads/concurrent"]), old.as_str());
}
