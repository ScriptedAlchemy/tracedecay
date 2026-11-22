use super::*;
use tracedecay_application::pr_tracking::{
    acquire_manual_branch_lifecycle, default_pr_command_control,
};

struct Fixture {
    _temporary: tempfile::TempDir,
    repo: PathBuf,
    origin: PathBuf,
    data: PathBuf,
    head: String,
}

fn git(root: &Path, args: &[&str]) -> String {
    let result = successful_git_with_control(root, args, default_pr_command_control())
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let repo = root.join("repository");
        let origin = root.join("origin.git");
        let data = root.join("project-store");
        for directory in [&repo, &origin, &data] {
            std::fs::create_dir_all(directory).unwrap();
        }
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "TraceDecay Test"]);
        git(
            &repo,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::write(repo.join("source.txt"), "published source\n").unwrap();
        git(&repo, &["add", "source.txt"]);
        git(&repo, &["commit", "-qm", "published source"]);
        git(&origin, &["init", "--bare", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-qu", "origin", "main"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        Self {
            _temporary: temporary,
            repo,
            origin,
            data,
            head,
        }
    }

    fn add(&self, pr: u64) -> (String, LegacyManagedPr) {
        let label = format!("tracedecay/autotrack/pr/{pr}");
        let tracking_ref = format!("refs/tracedecay/pr/{pr}");
        let worktree = self.data.join("pr-worktrees").join(format!("pr-{pr}"));
        git(&self.repo, &["update-ref", &tracking_ref, &self.head]);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &label,
                &worktree.to_string_lossy(),
                &self.head,
            ],
        );
        let managed = LegacyManagedPr {
            pr,
            head_branch: "main".to_owned(),
            head_sha: self.head.clone(),
            worktree,
            tracking_ref,
            retirement_reason: None,
        };
        let mut state = load_state(&self.data).unwrap();
        state.managed.insert(label.clone(), managed.clone());
        persist_retirement(&self.data, &state).unwrap();
        (label, managed)
    }

    fn fingerprint(&self, label: &str, managed: &LegacyManagedPr) -> (Vec<u8>, String, String) {
        (
            std::fs::read(managed.worktree.join("source.txt")).unwrap(),
            git(&self.repo, &["rev-parse", &format!("refs/heads/{label}")]),
            git(&self.repo, &["rev-parse", &managed.tracking_ref]),
        )
    }
}

#[tokio::test]
async fn published_legacy_worktrees_retire_without_touching_shared_stores() {
    let fixture = Fixture::new();
    let entries = [fixture.add(1), fixture.add(2)];
    for file in ["facts.db", "sessions.db", "configuration.db"] {
        std::fs::write(fixture.data.join(file), b"shared history").unwrap();
    }
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired.len(), entries.len());
    assert_eq!(report.preserved, Vec::new());
    assert!(load_state(&fixture.data).unwrap().managed.is_empty());
    for (label, managed) in entries {
        assert!(!managed.worktree.exists());
        for reference in [format!("refs/heads/{label}"), managed.tracking_ref] {
            assert_eq!(
                exact_commit(&fixture.repo, &reference, default_pr_command_control()).unwrap(),
                None
            );
        }
    }
    for file in ["facts.db", "sessions.db", "configuration.db"] {
        assert_eq!(
            std::fs::read(fixture.data.join(file)).unwrap(),
            b"shared history"
        );
    }
    let retried = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(retried.retired.is_empty() && retried.preserved.is_empty());
    schedulers.shutdown().await;
}

#[tokio::test]
async fn dirty_foreign_and_leased_artifacts_are_preserved_and_reported() {
    let fixture = Fixture::new();
    let dirty = fixture.add(1);
    let foreign = fixture.add(2);
    let leased = fixture.add(3);
    let git_locked = fixture.add(4);
    std::fs::write(dirty.1.worktree.join("source.txt"), "local edits\n").unwrap();
    let mut state = load_state(&fixture.data).unwrap();
    state.managed.get_mut(&foreign.0).unwrap().worktree = fixture.repo.clone();
    persist_retirement(&fixture.data, &state).unwrap();
    let held = acquire_manual_branch_lifecycle(&fixture.data, &leased.0)
        .await
        .unwrap();
    git(
        &fixture.repo,
        &["worktree", "lock", &git_locked.1.worktree.to_string_lossy()],
    );
    let entries = [&dirty, &foreign, &leased, &git_locked];
    let before = entries.map(|(label, managed)| fixture.fingerprint(label, managed));
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired, Vec::<String>::new());
    assert_eq!(
        report.preserved,
        [
            (dirty.0.clone(), PreservedReason::Dirty),
            (foreign.0.clone(), PreservedReason::Foreign),
            (leased.0.clone(), PreservedReason::Leased),
            (git_locked.0.clone(), PreservedReason::Leased),
        ]
    );
    for ((label, managed), before) in entries.into_iter().zip(before) {
        assert_eq!(fixture.fingerprint(label, managed), before);
    }
    let finding = legacy_pr_artifacts_finding(&fixture.data).unwrap();
    for reason in ["dirty", "foreign", "leased"] {
        assert!(finding.contains(reason), "{finding}");
    }
    for (label, managed) in [&dirty, &leased, &git_locked] {
        assert!(finding.contains(label), "{finding}");
        assert!(
            finding.contains(&managed.worktree.display().to_string()),
            "{finding}"
        );
        assert!(finding.contains(&managed.tracking_ref), "{finding}");
    }
    drop(held);
    schedulers.shutdown().await;
}

#[tokio::test]
async fn tracked_edits_hidden_by_index_flags_preserve_every_recorded_artifact() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let fixture = Fixture::new();
        let (label, managed) = fixture.add(1);
        git(
            &managed.worktree,
            &["update-index", flag, "--", "source.txt"],
        );
        std::fs::write(
            managed.worktree.join("source.txt"),
            b"uncommitted hidden work must survive\n",
        )
        .unwrap();
        assert!(
            git(
                &managed.worktree,
                &[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored",
                ],
            )
            .is_empty(),
            "{flag} must hide the fixture edit"
        );
        let before = fixture.fingerprint(&label, &managed);
        let flags_before = git(&managed.worktree, &["ls-files", "-v", "--", "source.txt"]);
        let schedulers = CodeIndexSchedulerRegistryV1::new(1);

        let report = retire_project(
            &fixture.repo,
            &fixture.data,
            &schedulers,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(report.retired.is_empty(), "{flag}");
        assert_eq!(
            report.preserved,
            [(label.clone(), PreservedReason::Unavailable)],
            "{flag}"
        );
        assert!(managed.worktree.is_dir(), "{flag}");
        assert_eq!(fixture.fingerprint(&label, &managed), before, "{flag}");
        assert_eq!(
            git(&managed.worktree, &["ls-files", "-v", "--", "source.txt"]),
            flags_before,
            "{flag} must remain set"
        );
        let state = load_state(&fixture.data).unwrap();
        assert_eq!(state.managed.len(), 1, "{flag}");
        let preserved = state.managed.get(&label).unwrap();
        assert_eq!(preserved.worktree, managed.worktree, "{flag}");
        assert_eq!(preserved.tracking_ref, managed.tracking_ref, "{flag}");
        assert_eq!(preserved.head_sha, managed.head_sha, "{flag}");
        assert_eq!(
            preserved.retirement_reason,
            Some(PreservedReason::Unavailable),
            "{flag}"
        );
        schedulers.shutdown().await;
    }
}

#[tokio::test]
async fn clean_worktrees_with_unpublished_commits_are_preserved_byte_for_byte() {
    let fixture = Fixture::new();
    let advanced = fixture.add(1);
    let moved_tracking_ref = fixture.add(2);
    for (_, managed) in [&advanced, &moved_tracking_ref] {
        std::fs::write(
            managed.worktree.join("source.txt"),
            format!("unpublished {}\n", managed.pr),
        )
        .unwrap();
        git(
            &managed.worktree,
            &["commit", "-qam", "local unpublished commit"],
        );
    }
    let unpublished_tracking = git(&moved_tracking_ref.1.worktree, &["rev-parse", "HEAD"]);
    git(
        &fixture.repo,
        &[
            "update-ref",
            &moved_tracking_ref.1.tracking_ref,
            &unpublished_tracking,
        ],
    );
    git(
        &moved_tracking_ref.1.worktree,
        &["reset", "--hard", &fixture.head],
    );
    let entries = [&advanced, &moved_tracking_ref];
    let before = entries.map(|(label, managed)| fixture.fingerprint(label, managed));
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired, Vec::<String>::new());
    assert_eq!(
        report.preserved,
        [
            (advanced.0.clone(), PreservedReason::Unpublished),
            (moved_tracking_ref.0.clone(), PreservedReason::Unpublished),
        ]
    );
    for ((label, managed), before) in entries.into_iter().zip(before) {
        assert_eq!(fixture.fingerprint(label, managed), before);
        git(&fixture.repo, &["cat-file", "-e", &before.1]);
        git(&fixture.repo, &["cat-file", "-e", &before.2]);
    }
    assert_eq!(load_state(&fixture.data).unwrap().managed.len(), 2);
    assert!(
        legacy_pr_artifacts_finding(&fixture.data)
            .unwrap()
            .contains("unpublished commits")
    );
    schedulers.shutdown().await;
}

#[tokio::test]
async fn cancelled_retirement_keeps_every_recorded_artifact() {
    let fixture = Fixture::new();
    let entries = [fixture.add(1), fixture.add(2)];
    let before = entries
        .each_ref()
        .map(|(label, managed)| fixture.fingerprint(label, managed));
    let ledger_before = std::fs::read(fixture.data.join(STATE_FILENAME)).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(&fixture.repo, &fixture.data, &schedulers, &cancellation)
        .await
        .unwrap();
    assert!(report.retired.is_empty() && report.preserved.is_empty());
    for ((label, managed), before) in entries.iter().zip(before) {
        assert_eq!(fixture.fingerprint(label, managed), before);
    }
    assert_eq!(
        std::fs::read(fixture.data.join(STATE_FILENAME)).unwrap(),
        ledger_before
    );
    assert_eq!(load_state(&fixture.data).unwrap().managed.len(), 2);
    schedulers.shutdown().await;
}

#[tokio::test]
async fn remote_pr_heads_without_recorded_ownership_create_no_checkouts() {
    let fixture = Fixture::new();
    for pr in 1..=12 {
        git(
            &fixture.origin,
            &["update-ref", &format!("refs/pull/{pr}/head"), &fixture.head],
        );
    }
    let before = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(report.retired.is_empty() && report.preserved.is_empty());
    assert_eq!(
        git(&fixture.repo, &["worktree", "list", "--porcelain"]),
        before
    );
    assert!(!fixture.data.join(STATE_FILENAME).exists());
    assert!(!fixture.data.join("pr-worktrees").exists());
    println!("legacy_pr_retirement_after remote_pr_heads=12 new_worktrees=0");
    schedulers.shutdown().await;
}

#[tokio::test]
async fn malformed_ownership_is_preserved_before_any_mutation() {
    let fixture = Fixture::new();
    let (label, managed) = fixture.add(1);
    let before = fixture.fingerprint(&label, &managed);
    let bytes = b"{invalid ownership";
    std::fs::write(fixture.data.join(STATE_FILENAME), bytes).unwrap();
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        retire_project(
            &fixture.repo,
            &fixture.data,
            &schedulers,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.data.join(STATE_FILENAME)).unwrap(),
        bytes
    );
    assert_eq!(fixture.fingerprint(&label, &managed), before);
    assert!(
        legacy_pr_artifacts_finding(&fixture.data)
            .unwrap()
            .contains("cannot be read")
    );
    schedulers.shutdown().await;
}

#[tokio::test]
async fn stale_refs_and_missing_registered_worktrees_keep_their_ownership_records() {
    let fixture = Fixture::new();
    let retargeted = fixture.add(1);
    let missing = fixture.add(2);
    std::fs::write(fixture.repo.join("source.txt"), "new published source\n").unwrap();
    git(&fixture.repo, &["commit", "-qam", "new published commit"]);
    git(&fixture.repo, &["push", "-q", "origin", "main"]);
    let successor = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(
        &fixture.repo,
        &["update-ref", &retargeted.1.tracking_ref, &successor],
    );
    let before = fixture.fingerprint(&retargeted.0, &retargeted.1);
    // Deliberately lose only this fixture checkout. Git still records its
    // identity and can retain detached commits or a user worktree lock.
    std::fs::remove_dir_all(&missing.1.worktree).unwrap();
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired, Vec::<String>::new());
    assert_eq!(
        report.preserved,
        [
            (retargeted.0.clone(), PreservedReason::Foreign),
            (missing.0.clone(), PreservedReason::Unavailable),
        ]
    );
    assert_eq!(fixture.fingerprint(&retargeted.0, &retargeted.1), before);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &missing.1.tracking_ref]),
        fixture.head
    );
    assert_eq!(load_state(&fixture.data).unwrap().managed.len(), 2);
    schedulers.shutdown().await;
}

#[tokio::test]
async fn branches_attached_to_other_worktrees_preserve_recorded_ownership() {
    for placement in ["primary", "linked", "duplicate", "alias"] {
        let fixture = Fixture::new();
        let (label, managed) = fixture.add(1);
        let user_checkout = fixture.repo.parent().unwrap().join("user-checkout");
        let keep_owned_checkout = matches!(placement, "duplicate" | "alias");
        if !keep_owned_checkout {
            git(
                &fixture.repo,
                &["worktree", "remove", &managed.worktree.to_string_lossy()],
            );
        }
        let user_root = if placement == "primary" {
            git(&fixture.repo, &["checkout", "-q", &label]);
            // Invoke retirement from a linked repository as well: its own
            // HEAD cannot substitute for inspecting the primary HEAD.
            git(
                &fixture.repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "observer",
                    &user_checkout.to_string_lossy(),
                    "main",
                ],
            );
            &fixture.repo
        } else if placement == "alias" {
            git(
                &fixture.repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "--detach",
                    &user_checkout.to_string_lossy(),
                    &fixture.head,
                ],
            );
            git(
                &fixture.repo,
                &[
                    "symbolic-ref",
                    "refs/heads/user-alias",
                    &format!("refs/heads/{label}"),
                ],
            );
            git(
                &user_checkout,
                &["symbolic-ref", "HEAD", "refs/heads/user-alias"],
            );
            &user_checkout
        } else {
            git(
                &fixture.repo,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "--force",
                    &user_checkout.to_string_lossy(),
                    &label,
                ],
            );
            &user_checkout
        };
        std::fs::write(user_root.join("source.txt"), "user work remains\n").unwrap();
        let schedulers = CodeIndexSchedulerRegistryV1::new(1);
        let report = retire_project(
            &user_checkout,
            &fixture.data,
            &schedulers,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(report.retired.is_empty(), "{placement}");
        assert_eq!(
            report.preserved,
            [(label.clone(), PreservedReason::Foreign)],
            "{placement}"
        );
        assert_eq!(
            git(user_root, &["symbolic-ref", "HEAD"]),
            format!("refs/heads/{label}"),
            "{placement}"
        );
        assert_eq!(git(user_root, &["rev-parse", "HEAD"]), fixture.head);
        assert_eq!(
            git(&fixture.repo, &["rev-parse", &managed.tracking_ref]),
            fixture.head
        );
        assert_eq!(
            std::fs::read(user_root.join("source.txt")).unwrap(),
            b"user work remains\n"
        );
        assert_eq!(managed.worktree.exists(), keep_owned_checkout);
        assert!(
            load_state(&fixture.data)
                .unwrap()
                .managed
                .contains_key(&label)
        );
        schedulers.shutdown().await;
    }
}

#[tokio::test]
async fn symbolic_and_non_commit_refs_preserve_every_owned_artifact() {
    let fixture = Fixture::new();
    let symbolic_tracking = fixture.add(1);
    let symbolic_branch = fixture.add(2);
    let blob_tracking = fixture.add(3);
    let tag_tracking = fixture.add(4);
    git(
        &fixture.repo,
        &["update-ref", "refs/heads/user-owned", &fixture.head],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            &symbolic_tracking.1.tracking_ref,
            "refs/heads/main",
        ],
    );
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            &format!("refs/heads/{}", symbolic_branch.0),
            "refs/heads/user-owned",
        ],
    );
    let blob = git(&fixture.repo, &["hash-object", "-w", "source.txt"]);
    git(
        &fixture.repo,
        &["update-ref", &blob_tracking.1.tracking_ref, &blob],
    );
    git(
        &fixture.repo,
        &["tag", "-am", "foreign annotation", "foreign-tag"],
    );
    let tag = git(&fixture.repo, &["rev-parse", "refs/tags/foreign-tag"]);
    git(
        &fixture.repo,
        &["update-ref", &tag_tracking.1.tracking_ref, &tag],
    );
    let entries = [
        &symbolic_tracking,
        &symbolic_branch,
        &blob_tracking,
        &tag_tracking,
    ];
    let before = entries.map(|(label, managed)| fixture.fingerprint(label, managed));
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired, Vec::<String>::new());
    assert_eq!(
        report.preserved,
        entries.map(|(label, _)| (label.clone(), PreservedReason::Foreign))
    );
    for ((label, managed), before) in entries.into_iter().zip(before) {
        assert_eq!(fixture.fingerprint(label, managed), before);
    }
    assert_eq!(
        git(
            &fixture.repo,
            &["symbolic-ref", &symbolic_tracking.1.tracking_ref]
        ),
        "refs/heads/main"
    );
    assert_eq!(
        git(
            &fixture.repo,
            &["symbolic-ref", &format!("refs/heads/{}", symbolic_branch.0)]
        ),
        "refs/heads/user-owned"
    );
    for reference in ["refs/heads/main", "refs/heads/user-owned"] {
        assert_eq!(git(&fixture.repo, &["rev-parse", reference]), fixture.head);
    }
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "refs/tags/foreign-tag"]),
        tag
    );
    assert_eq!(
        load_state(&fixture.data).unwrap().managed.len(),
        entries.len()
    );
    schedulers.shutdown().await;
}

#[test]
fn a_symbolic_ref_substituted_after_verification_cannot_delete_its_target() {
    let fixture = Fixture::new();
    let (label, managed) = fixture.add(1);
    let (_, _, tracking) = verify_owned_published_artifacts(
        &fixture.repo,
        &fixture.data,
        &label,
        &managed,
        default_pr_command_control(),
    )
    .unwrap();
    git(
        &fixture.repo,
        &["symbolic-ref", &managed.tracking_ref, "refs/heads/main"],
    );
    // Git versions differ in whether the expected-OID comparison accepts a
    // symbolic alias. Either result must leave its target and checkout intact.
    let _outcome = delete_exact_commit_ref(
        &fixture.repo,
        &managed.tracking_ref,
        tracking.as_deref().unwrap(),
        default_pr_command_control(),
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "refs/heads/main"]),
        fixture.head
    );
    assert_eq!(
        git(
            &fixture.repo,
            &["rev-parse", &format!("refs/heads/{label}")]
        ),
        fixture.head
    );
    assert_eq!(
        std::fs::read(managed.worktree.join("source.txt")).unwrap(),
        b"published source\n"
    );
}

#[tokio::test]
async fn a_remote_alias_to_the_retiring_branch_is_not_a_retained_commit_anchor() {
    let fixture = Fixture::new();
    let (label, mut managed) = fixture.add(1);
    std::fs::write(
        managed.worktree.join("source.txt"),
        "only this PR retains this commit\n",
    )
    .unwrap();
    git(&managed.worktree, &["commit", "-qam", "PR-only commit"]);
    managed.head_sha = git(&managed.worktree, &["rev-parse", "HEAD"]);
    git(
        &fixture.repo,
        &["update-ref", &managed.tracking_ref, &managed.head_sha],
    );
    let mut state = load_state(&fixture.data).unwrap();
    state.managed.insert(label.clone(), managed.clone());
    persist_retirement(&fixture.data, &state).unwrap();
    let remote_alias = "refs/remotes/origin/managed-alias";
    let branch_ref = format!("refs/heads/{label}");
    git(&fixture.repo, &["symbolic-ref", remote_alias, &branch_ref]);
    let before = fixture.fingerprint(&label, &managed);
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let report = retire_project(
        &fixture.repo,
        &fixture.data,
        &schedulers,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.retired, Vec::<String>::new());
    assert_eq!(
        report.preserved,
        [(label.clone(), PreservedReason::Unpublished)]
    );
    assert_eq!(fixture.fingerprint(&label, &managed), before);
    assert_eq!(
        git(&fixture.repo, &["symbolic-ref", remote_alias]),
        branch_ref
    );
    assert_eq!(load_state(&fixture.data).unwrap().managed.len(), 1);
    schedulers.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_ledger_or_store_is_never_destructive_authority() {
    use std::os::unix::fs::symlink;

    for replace_store in [false, true] {
        let fixture = Fixture::new();
        let (label, managed) = fixture.add(1);
        let before = fixture.fingerprint(&label, &managed);
        let ledger = fixture.data.join(STATE_FILENAME);
        let bytes = std::fs::read(&ledger).unwrap();
        let foreign = fixture.repo.parent().unwrap().join("foreign-data");
        let alias = if replace_store {
            &fixture.data
        } else {
            &ledger
        };
        std::fs::rename(alias, &foreign).unwrap();
        symlink(&foreign, alias).unwrap();
        let schedulers = CodeIndexSchedulerRegistryV1::new(1);
        assert!(
            retire_project(
                &fixture.repo,
                &fixture.data,
                &schedulers,
                &CancellationToken::new(),
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.fingerprint(&label, &managed), before);
        assert_eq!(std::fs::read(&ledger).unwrap(), bytes);
        assert!(
            std::fs::symlink_metadata(alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            legacy_pr_artifacts_finding(&fixture.data)
                .unwrap()
                .contains("cannot be read")
        );
        schedulers.shutdown().await;
    }
}

#[tokio::test]
async fn a_nonregular_ledger_preserves_worktrees_and_refs() {
    let fixture = Fixture::new();
    let (label, managed) = fixture.add(1);
    let before = fixture.fingerprint(&label, &managed);
    let ledger = fixture.data.join(STATE_FILENAME);
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        retire_project(
            &fixture.repo,
            &fixture.data,
            &schedulers,
            &CancellationToken::new(),
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.fingerprint(&label, &managed), before);
    assert!(ledger.is_dir());
    schedulers.shutdown().await;
}
