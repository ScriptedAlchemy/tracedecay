use std::fs;
use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use super::process::run_command_with_stdin;
use super::{FixedGitIndexRunner, NativeGitIndexError};
use tempfile::tempdir;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::GitCommandBounds;

const PIPE_ECHO_HELPER_ENV: &str = "TRACEDECAY_GIT_INDEX_PIPE_ECHO_HELPER";
const PIPE_EARLY_EXIT_HELPER_ENV: &str = "TRACEDECAY_GIT_INDEX_PIPE_EARLY_EXIT_HELPER";
const PIPE_FAIL_HELPER_ENV: &str = "TRACEDECAY_GIT_INDEX_PIPE_FAIL_HELPER";

#[test]
fn pipe_echo_helper() {
    if std::env::var_os(PIPE_EARLY_EXIT_HELPER_ENV).is_some() {
        return;
    }
    if std::env::var_os(PIPE_ECHO_HELPER_ENV).is_none() {
        return;
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).expect("read helper stdin");
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .expect("write helper stdout");
        output.flush().expect("flush helper stdout");
    }
    assert!(
        std::env::var_os(PIPE_FAIL_HELPER_ENV).is_none(),
        "requested child failure"
    );
}

#[test]
fn command_with_large_bidirectional_pipes_drains_output_while_writing_input() {
    let input = vec![0xa5; 4 * 1024 * 1024];
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args([
            "--exact",
            "git_index_transactions::tests::pipe_echo_helper",
            "--nocapture",
        ])
        .env(PIPE_ECHO_HELPER_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let output = run_command_with_stdin(command, "pipe-echo", &input)
        .expect("large bidirectional subprocess completes");
    assert_eq!(
        output
            .stdout
            .iter()
            .copied()
            .filter(|byte| *byte == 0xa5)
            .count(),
        input.len(),
        "the concurrent drain must retain every emitted byte"
    );
}

#[test]
fn command_child_failure_remains_typed_after_pipe_drain() {
    let input = vec![0xa5; 4 * 1024 * 1024];
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args([
            "--exact",
            "git_index_transactions::tests::pipe_echo_helper",
            "--nocapture",
        ])
        .env(PIPE_ECHO_HELPER_ENV, "1")
        .env(PIPE_FAIL_HELPER_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    assert!(matches!(
        run_command_with_stdin(command, "pipe-child-failure", &input),
        Err(NativeGitIndexError::GitFailed {
            operation: "pipe-child-failure",
            ..
        })
    ));
}

#[test]
fn command_that_closes_stdin_early_returns_a_typed_io_failure() {
    let input = vec![0xa5; 4 * 1024 * 1024];
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args([
            "--exact",
            "git_index_transactions::tests::pipe_echo_helper",
            "--nocapture",
        ])
        .env(PIPE_EARLY_EXIT_HELPER_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    assert!(matches!(
        run_command_with_stdin(command, "pipe-early-exit", &input),
        Err(NativeGitIndexError::Io(_))
    ));
}

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
    fs::write(directory.path().join("tracked.txt"), b"tracked\n").expect("tracked file");
    fs::write(
        directory.path().join(".gitattributes"),
        b"tracked.txt diff=tracedecay merge=tracedecay filter=tracedecay\n",
    )
    .expect("bind every driver kind to a path");
    let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");

    let set = |key: &str| {
        assert!(
            Command::new("git")
                .current_dir(directory.path())
                .args(["config", "--local", key, "external-driver"])
                .status()
                .expect("git config starts")
                .success()
        );
    };
    let unset = |key: &str| {
        assert!(
            Command::new("git")
                .current_dir(directory.path())
                .args(["config", "--local", "--unset-all", key])
                .status()
                .expect("git config unset starts")
                .success()
        );
    };

    for key in [
        "diff.external",
        "merge.tracedecay.driver",
        "diff.tracedecay.command",
        "diff.tracedecay.textconv",
        "filter.tracedecay.clean",
        "filter.tracedecay.smudge",
        "filter.tracedecay.process",
    ] {
        set(key);
        assert!(
            runner
                .has_external_drivers()
                .expect("driver classification"),
            "{key} is bound to a path by gitattributes and must refuse a preview"
        );
        unset(key);
        assert!(!runner.has_external_drivers().expect("driver removed"));
    }

    // A driver definition no attribute binds cannot rewrite this repository's
    // content. `git lfs install --system` puts exactly such a definition in
    // `/etc/gitconfig` on every GitHub-hosted runner and most developer
    // machines; classifying it as applied refused every preview there.
    for key in [
        "filter.lfs.clean",
        "filter.lfs.smudge",
        "filter.lfs.process",
        "merge.unbound.driver",
        "diff.unbound.command",
        "diff.unbound.textconv",
    ] {
        set(key);
        assert!(
            !runner
                .has_external_drivers()
                .expect("unbound driver classification"),
            "{key} binds no path in this repository and must not refuse a preview"
        );
        unset(key);
    }

    // `diff.external` names no driver to bind: it replaces the diff machinery
    // for every diff, so it refuses with no attribute at all.
    fs::remove_file(directory.path().join(".gitattributes")).expect("drop attribute bindings");
    set("merge.tracedecay.driver");
    assert!(!runner.has_external_drivers().expect("unbound named driver"));
    set("diff.external");
    assert!(
        runner
            .has_external_drivers()
            .expect("unconditional external diff driver")
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
    let set_filemode = |value: &str| {
        assert!(
            Command::new("git")
                .current_dir(directory.path())
                .args(["config", "--local", "core.filemode", value])
                .status()
                .expect("git config starts")
                .success()
        );
    };
    let read_filemode = || {
        let output = Command::new("git")
            .current_dir(directory.path())
            .args(["config", "--local", "--bool", "--get", "core.filemode"])
            .output()
            .expect("git config read starts");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("core.filemode is UTF-8")
            .trim()
            .to_string()
    };
    set_filemode("true");
    assert_eq!(read_filemode(), "true");

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
    set_filemode("false");
    assert_eq!(read_filemode(), "false");
    assert_ne!(
        capabilities_before,
        runner
            .filesystem_capabilities_digest()
            .expect("changed filesystem capabilities")
    );
}

#[test]
fn sparse_digest_tracks_sparse_checkout_authority() {
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
    let initial = runner.sparse_digest().expect("initial sparse digest");

    fs::write(
        directory.path().join(".git/info/sparse-checkout"),
        b"/src/\n",
    )
    .expect("sparse checkout patterns");
    let with_patterns = runner.sparse_digest().expect("pattern sparse digest");
    assert_ne!(initial, with_patterns);

    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["config", "--local", "core.sparseCheckout", "true"])
            .status()
            .expect("git config starts")
            .success()
    );
    assert_ne!(
        with_patterns,
        runner.sparse_digest().expect("enabled sparse digest")
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

fn committed_file(name: &str, contents: &[u8]) -> (tempfile::TempDir, FixedGitIndexRunner) {
    let directory = tempdir().expect("temporary repository");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["init", "--quiet"])
            .status()
            .expect("git init starts")
            .success()
    );
    fs::write(directory.path().join(name), contents).expect("tracked file");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", name])
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
    (directory, runner)
}

#[test]
fn tracked_worktree_digest_honors_cancellation_before_filesystem_walk() {
    let (directory, _runner) = committed_file("tracked.txt", b"before\n");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let runner = FixedGitIndexRunner::new(directory.path())
        .expect("runner")
        .with_command_bounds(GitCommandBounds {
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: Some(cancel),
            ..GitCommandBounds::default()
        });
    assert!(
        matches!(
            runner.tracked_worktree_digest(),
            Err(NativeGitIndexError::Cancelled)
        ),
        "a cancelled digest must fail closed instead of minting identity"
    );
    runner
        .acquire_index_lock()
        .expect("cancellation must not leave the real index lock held");
}

fn assert_git_diff_head_hides(repository: &std::path::Path, path: &str) {
    let output = Command::new("git")
        .current_dir(repository)
        .args([
            "diff",
            "-z",
            "--name-only",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "HEAD",
        ])
        .output()
        .expect("git diff HEAD starts");
    assert!(output.status.success(), "git diff HEAD");
    assert!(
        !output
            .stdout
            .split(|byte| *byte == 0)
            .any(|entry| entry == path.as_bytes()),
        "git diff HEAD must omit {path} when a worktree check flag hides it"
    );
}

#[test]
fn tracked_worktree_digest_changes_when_assume_unchanged_bytes_change() {
    let (directory, runner) = committed_file("source.rs", b"committed\n");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["update-index", "--assume-unchanged", "--", "source.rs"])
            .status()
            .expect("assume-unchanged starts")
            .success()
    );
    let before = runner
        .tracked_worktree_digest()
        .expect("digest before hidden edit");
    fs::write(
        directory.path().join("source.rs"),
        b"hidden worktree drift\n",
    )
    .expect("hidden edit");
    assert_git_diff_head_hides(directory.path(), "source.rs");
    let after = runner
        .tracked_worktree_digest()
        .expect("digest after hidden edit");
    assert_ne!(
        before, after,
        "assume-unchanged is not byte evidence; the digest must bind worktree bytes"
    );
}

#[test]
fn tracked_worktree_digest_changes_when_skip_worktree_bytes_change() {
    let (directory, runner) = committed_file("source.rs", b"committed\n");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["update-index", "--skip-worktree", "--", "source.rs"])
            .status()
            .expect("skip-worktree starts")
            .success()
    );
    let before = runner
        .tracked_worktree_digest()
        .expect("digest before hidden edit");
    fs::write(
        directory.path().join("source.rs"),
        b"hidden skip-worktree drift\n",
    )
    .expect("hidden edit");
    assert_git_diff_head_hides(directory.path(), "source.rs");
    let after = runner
        .tracked_worktree_digest()
        .expect("digest after hidden edit");
    assert_ne!(
        before, after,
        "skip-worktree is not byte evidence; the digest must bind worktree bytes"
    );
}

#[cfg(unix)]
#[test]
fn tracked_worktree_digest_observes_executable_mode_when_filemode_is_false() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, runner) = committed_file("source.rs", b"committed\n");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["config", "--local", "core.filemode", "false"])
            .status()
            .expect("core.filemode=false starts")
            .success()
    );
    let before = runner
        .tracked_worktree_digest()
        .expect("digest before hidden mode change");
    let path = directory.path().join("source.rs");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod +x");
    assert_git_diff_head_hides(directory.path(), "source.rs");
    let after = runner
        .tracked_worktree_digest()
        .expect("digest after hidden mode change");
    assert_ne!(
        before, after,
        "core.filemode=false is not mode evidence; the digest must bind worktree executable identity"
    );
    let mut restored = fs::metadata(&path).expect("metadata").permissions();
    restored.set_mode(0o644);
    fs::set_permissions(&path, restored).expect("chmod 0644");
    assert_eq!(
        before,
        runner
            .tracked_worktree_digest()
            .expect("digest after restoring mode"),
        "restoring the worktree mode must recover the prior identity"
    );
}

#[cfg(unix)]
#[test]
fn tracked_worktree_digest_preserves_mode_symlink_and_absent_identity() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let (directory, runner) = committed_file("regular.txt", b"regular\n");
    fs::write(directory.path().join("exec.sh"), b"#!/bin/sh\n").expect("executable");
    let mut permissions = fs::metadata(directory.path().join("exec.sh"))
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(directory.path().join("exec.sh"), permissions).expect("chmod +x");
    symlink("regular.txt", directory.path().join("link.txt")).expect("symlink");
    assert!(
        Command::new("git")
            .current_dir(directory.path())
            .args(["add", "--", "exec.sh", "link.txt"])
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
                "mode and symlink",
            ])
            .status()
            .expect("git commit starts")
            .success()
    );
    let with_present = runner
        .tracked_worktree_digest()
        .expect("present mode and symlink digest");
    fs::remove_file(directory.path().join("regular.txt")).expect("absent regular file");
    let with_absent = runner
        .tracked_worktree_digest()
        .expect("absent file digest");
    assert_ne!(
        with_present, with_absent,
        "an absent worktree file must change identity"
    );
    fs::write(directory.path().join("regular.txt"), b"regular\n").expect("restore regular");
    assert_eq!(
        with_present,
        runner.tracked_worktree_digest().expect("restored digest"),
        "restoring the absent file must recover the prior identity"
    );
}

#[cfg(unix)]
#[test]
fn tracked_worktree_digest_still_reads_dirty_worktree_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, runner) = committed_file("secret.txt", b"before\n");
    let path = directory.path().join("secret.txt");
    fs::write(&path, b"after\n").expect("dirty worktree");
    let original = fs::metadata(&path).expect("metadata").permissions();
    let mut locked = original.clone();
    locked.set_mode(0o000);
    fs::set_permissions(&path, locked).expect("deny dirty reads");
    let digest = runner.tracked_worktree_digest();
    fs::set_permissions(&path, original).expect("restore dirty reads");
    assert!(
        matches!(digest, Err(NativeGitIndexError::Io(_))),
        "a dirty path must still bind worktree bytes and fail closed when unreadable"
    );
}
