use serde_json::json;

use tracedecay_domain::*;

const SHA1_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA1_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA1_C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const DIGEST_X: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const DIGEST_Y: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn oid(value: &str) -> GitOidV1 {
    GitOidV1::new(value).unwrap()
}

fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

fn repository() -> RepositoryId {
    RepositoryId::new("repository.fixture").unwrap()
}

fn hunk(old: (u32, u32), new: (u32, u32)) -> GitHunkV1 {
    GitHunkV1 {
        old_start: old.0,
        old_lines: old.1,
        new_start: new.0,
        new_lines: new.1,
        section: None,
        patch_digest: digest(DIGEST_X),
    }
}

fn file_diff(path: &str, change: GitChangeKindV1) -> GitFileDiffV1 {
    GitFileDiffV1 {
        path: path.to_owned(),
        original_path: None,
        change,
        old_mode: None,
        new_mode: None,
        old_blob: None,
        new_blob: None,
        binary: false,
        submodule: false,
        insertions: Some(1),
        deletions: Some(0),
        hunks: vec![hunk((1, 1), (1, 2))],
    }
}

fn identity(name: &str) -> GitCommitIdentityV1 {
    GitCommitIdentityV1 {
        name: name.to_owned(),
        email: format!("{name}@example.com"),
        at: UtcMicros(1_700_000_000_000_000),
    }
}

fn commit(value: &str) -> GitCommitMetadataV1 {
    GitCommitMetadataV1 {
        commit: oid(value),
        tree: oid(SHA1_C),
        parents: vec![],
        author: identity("author"),
        committer: identity("committer"),
        subject: "subject".to_owned(),
        message_digest: digest(DIGEST_X),
    }
}

fn hunk_ref() -> HunkRefV1 {
    HunkRefV1 {
        repository: repository(),
        worktree: WorktreeId::new("worktree.fixture").unwrap(),
        direction: HunkDirectionV1::WorkingTreeToIndex,
        path: "src/main.rs".to_owned(),
        original_path: None,
        expected_base_blob: GitBlobExpectationV1::Present(oid(SHA1_A)),
        expected_index_entry: GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::Present(oid(SHA1_A)),
            mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
            unmerged_stage: None,
        },
        expected_worktree_blob: Some(GitBlobExpectationV1::Present(oid(SHA1_B))),
        expected_worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
        hunk_header: "@@ -1,3 +1,4 @@".to_owned(),
        context_digest: digest(DIGEST_X),
        patch_digest: digest(DIGEST_Y),
        selected_line_bitmap: full_hunk_selection_bitmap(4),
        attributes_digest: None,
        preview_id: "preview.fixture".to_owned(),
        schema_version: HUNK_REF_SCHEMA_VERSION_V1.to_owned(),
        snapshot_digest: digest(DIGEST_X),
    }
}

#[test]
fn git_oid_accepts_sha1_and_sha256_and_derives_format() {
    let sha1 = oid(SHA1_A);
    assert_eq!(sha1.format(), GitObjectFormatV1::Sha1);
    assert_eq!(GitObjectFormatV1::Sha1.oid_hex_len(), 40);

    let sha256 = oid(&"d".repeat(64));
    assert_eq!(sha256.format(), GitObjectFormatV1::Sha256);
    assert_eq!(GitObjectFormatV1::Sha256.oid_hex_len(), 64);
}

#[test]
fn git_oid_rejects_noncanonical_values() {
    for bad in [
        "",
        "abc",
        &"a".repeat(39),
        &"a".repeat(41),
        &"A".repeat(40),
        &"g".repeat(40),
        &"a".repeat(63),
    ] {
        assert!(GitOidV1::new(bad).is_err(), "accepted oid {bad:?}");
    }
    assert!(serde_json::from_value::<GitOidV1>(json!("not-an-oid")).is_err());
}

#[test]
fn file_mode_validation_and_kind_helpers() {
    let regular = GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap();
    assert!(!regular.is_submodule());
    assert!(!regular.is_symlink());
    assert!(
        GitFileModeV1::new(GitFileModeV1::GITLINK)
            .unwrap()
            .is_submodule()
    );
    assert!(
        GitFileModeV1::new(GitFileModeV1::SYMLINK)
            .unwrap()
            .is_symlink()
    );

    for bad in ["", "10064", "1006444", "10084a", "888888"] {
        assert!(GitFileModeV1::new(bad).is_err(), "accepted mode {bad:?}");
    }
}

#[test]
fn head_state_roundtrips_and_exposes_commit() {
    let attached = GitHeadStateV1::Attached {
        branch: "main".to_owned(),
        commit: oid(SHA1_A),
    };
    let detached = GitHeadStateV1::Detached {
        commit: oid(SHA1_A),
    };
    let unborn = GitHeadStateV1::Unborn {
        branch: "main".to_owned(),
    };

    assert_eq!(attached.commit(), Some(&oid(SHA1_A)));
    assert_eq!(attached.branch(), Some("main"));
    assert_eq!(detached.branch(), None);
    assert_eq!(unborn.commit(), None);

    for state in [attached, detached, unborn] {
        state.validate().unwrap();
        let wire = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<GitHeadStateV1>(&wire).unwrap(),
            state
        );
    }
}

#[test]
fn coverage_dedupes_sorts_and_reports_completeness() {
    let mut coverage = GitCoverageV1::complete();
    assert!(coverage.is_complete());

    coverage = GitCoverageV1::degraded(vec![
        GitDegradationV1::SubmoduleState,
        GitDegradationV1::DetachedHead,
        GitDegradationV1::DetachedHead,
    ]);
    assert!(!coverage.is_complete());
    assert!(coverage.records(GitDegradationV1::DetachedHead));
    assert_eq!(coverage.degradations.len(), 2);
    coverage.validate().unwrap();

    coverage.record(GitDegradationV1::SparseCheckout);
    coverage.record(GitDegradationV1::SparseCheckout);
    assert_eq!(coverage.degradations.len(), 3);
    coverage.validate().unwrap();

    let mut unsorted = coverage.clone();
    unsorted.degradations.reverse();
    if unsorted.degradations != coverage.degradations {
        assert!(unsorted.validate().is_err());
    }
}

#[test]
fn status_counts_and_cleanliness() {
    let status = GitStatusV1 {
        repository: repository(),
        head: GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: oid(SHA1_A),
        },
        operation: GitOperationStateV1::None,
        entries: vec![
            GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                path: "staged.txt".to_owned(),
                original_path: None,
                index: GitChangeKindV1::Added,
                worktree: GitChangeKindV1::Unmodified,
                head_mode: None,
                index_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                submodule: false,
            }),
            GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                path: "dirty.txt".to_owned(),
                original_path: None,
                index: GitChangeKindV1::Unmodified,
                worktree: GitChangeKindV1::Modified,
                head_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                index_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                submodule: false,
            }),
            GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                path: "conflict.txt".to_owned(),
                original_path: None,
                index: GitChangeKindV1::Unmerged,
                worktree: GitChangeKindV1::Unmerged,
                head_mode: None,
                index_mode: None,
                worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                submodule: false,
            }),
            GitStatusEntryV1::Untracked {
                path: "new.txt".to_owned(),
            },
            GitStatusEntryV1::Ignored {
                path: "app.log".to_owned(),
            },
        ],
        coverage: GitCoverageV1::complete(),
    };

    assert_eq!(status.staged_count(), 1);
    assert_eq!(status.unstaged_count(), 1);
    assert_eq!(status.conflicted_count(), 1);
    assert_eq!(status.untracked_count(), 1);
    assert_eq!(status.ignored_count(), 1);
    assert!(!status.is_clean());
    status.validate().unwrap();

    let clean = GitStatusV1 {
        entries: vec![],
        ..status
    };
    assert!(clean.is_clean());
}

#[test]
fn status_rejects_duplicate_paths() {
    let entry = GitStatusEntryV1::Untracked {
        path: "same.txt".to_owned(),
    };
    let status = GitStatusV1 {
        repository: repository(),
        head: GitHeadStateV1::Unborn {
            branch: "main".to_owned(),
        },
        operation: GitOperationStateV1::None,
        entries: vec![entry.clone(), entry],
        coverage: GitCoverageV1::complete(),
    };
    assert_eq!(
        status.validate(),
        Err(DomainError::DuplicateId {
            field: "status entry path"
        })
    );
}

#[test]
fn hunk_range_invariants_match_git_addressing() {
    // Pure insertion at the top of the file: old side addressed at 0,0.
    hunk((0, 0), (1, 3)).validate().unwrap();
    // Normal replacement hunk.
    hunk((1, 3), (1, 4)).validate().unwrap();
    // A non-empty side cannot start at line 0.
    assert!(hunk((0, 2), (1, 2)).validate().is_err());
    assert!(hunk((1, 2), (0, 2)).validate().is_err());
    assert_eq!(hunk((0, 0), (1, 3)).normalized_header(), "@@ -0,0 +1,3 @@");
}

#[test]
fn file_diff_invariants_for_binary_submodule_and_renames() {
    let mut binary = file_diff("blob.bin", GitChangeKindV1::Modified);
    binary.binary = true;
    binary.insertions = None;
    binary.deletions = None;
    binary.hunks = vec![];
    binary.validate().unwrap();

    let mut invalid = binary.clone();
    invalid.hunks = vec![hunk((1, 1), (1, 1))];
    assert!(invalid.validate().is_err());

    let mut renamed = file_diff("new.rs", GitChangeKindV1::Renamed);
    renamed.original_path = Some("old.rs".to_owned());
    renamed.validate().unwrap();

    renamed.original_path = None;
    assert!(renamed.validate().is_err());

    let mut misplaced = file_diff("plain.rs", GitChangeKindV1::Modified);
    misplaced.original_path = Some("old.rs".to_owned());
    assert!(misplaced.validate().is_err());
}

#[test]
fn diff_rejects_duplicate_file_paths() {
    let diff = GitDiffV1 {
        repository: repository(),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![
            file_diff("same.rs", GitChangeKindV1::Modified),
            file_diff("same.rs", GitChangeKindV1::Modified),
        ],
        coverage: GitCoverageV1::complete(),
    };
    assert_eq!(
        diff.validate(),
        Err(DomainError::DuplicateId {
            field: "diff file path"
        })
    );
}

#[test]
fn history_rejects_duplicate_commits() {
    let history = GitHistoryV1 {
        repository: repository(),
        commits: vec![commit(SHA1_A), commit(SHA1_A)],
        truncated: false,
        coverage: GitCoverageV1::complete(),
    };
    assert_eq!(
        history.validate(),
        Err(DomainError::DuplicateId {
            field: "history commit"
        })
    );
}

#[test]
fn blame_availability_invariants() {
    let unavailable = GitBlameV1 {
        repository: repository(),
        path: "missing.rs".to_owned(),
        lines: vec![],
        availability: GitBlameAvailabilityV1::PathNotTracked,
        coverage: GitCoverageV1::complete(),
    };
    unavailable.validate().unwrap();
    assert!(!unavailable.is_available());

    let mut incoherent = unavailable.clone();
    incoherent.lines = vec![GitBlameLineV1 {
        final_line: 1,
        origin_line: 1,
        commit: oid(SHA1_A),
        author: identity("author"),
        boundary: false,
        previous: None,
    }];
    assert!(incoherent.validate().is_err());

    let available = GitBlameV1 {
        repository: repository(),
        path: "tracked.rs".to_owned(),
        lines: vec![
            GitBlameLineV1 {
                final_line: 1,
                origin_line: 1,
                commit: oid(SHA1_A),
                author: identity("author"),
                boundary: false,
                previous: None,
            },
            GitBlameLineV1 {
                final_line: 2,
                origin_line: 2,
                commit: oid(SHA1_B),
                author: identity("author"),
                boundary: true,
                previous: Some(GitBlamePreviousV1 {
                    commit: oid(SHA1_C),
                    path: "old.rs".to_owned(),
                }),
            },
        ],
        availability: GitBlameAvailabilityV1::Available,
        coverage: GitCoverageV1::complete(),
    };
    available.validate().unwrap();
    assert!(available.is_available());

    let mut disordered = available.clone();
    disordered.lines.swap(0, 1);
    assert!(disordered.validate().is_err());
}

#[test]
fn hunk_ref_selection_bitmap_counts_and_queries_lines() {
    let bitmap = full_hunk_selection_bitmap(70);
    assert_eq!(bitmap.len(), 2);
    assert_eq!(bitmap[1], 0b111111);

    let reference = HunkRefV1 {
        selected_line_bitmap: bitmap,
        ..hunk_ref()
    };
    assert_eq!(reference.selected_line_count(), 70);
    assert!(reference.selects_line(1));
    assert!(reference.selects_line(70));
    assert!(!reference.selects_line(71));
    assert!(!reference.selects_line(0));
    reference.validate().unwrap();

    assert!(full_hunk_selection_bitmap(64)[0] == u64::MAX);
    let mut empty = hunk_ref();
    empty.selected_line_bitmap = vec![];
    assert!(empty.validate().is_err());
    let mut zero = hunk_ref();
    zero.selected_line_bitmap = vec![0];
    assert!(zero.validate().is_err());
}

#[test]
fn hunk_ref_digest_is_domain_separated_stable_and_self_verifying() {
    let reference = hunk_ref();
    let digest = reference.compute_digest().unwrap();
    assert_eq!(digest, reference.compute_digest().unwrap());
    reference.verify_digest(&digest).unwrap();
    assert_eq!(
        reference.verify_digest(&ManifestDigest::new(DIGEST_Y).unwrap()),
        Err(DomainError::DigestMismatch)
    );

    // Domain separation: the same payload hashed under a different domain
    // separator cannot collide with the HunkRef digest.
    let foreign = canonical_sha256(&serde_json::json!({
        "domain": "tracedecay.other.v1",
        "hunk_ref": serde_json::to_value(&reference).unwrap(),
    }))
    .unwrap();
    assert_ne!(digest, foreign);
}

#[test]
fn hunk_ref_digest_detects_independent_field_drift() {
    let reference = hunk_ref();
    let digest = reference.compute_digest().unwrap();

    let mutations: Vec<HunkRefV1> = vec![
        HunkRefV1 {
            path: "src/other.rs".to_owned(),
            ..reference.clone()
        },
        HunkRefV1 {
            direction: HunkDirectionV1::IndexToHead,
            ..reference.clone()
        },
        HunkRefV1 {
            expected_base_blob: GitBlobExpectationV1::AbsentFile,
            ..reference.clone()
        },
        HunkRefV1 {
            hunk_header: "@@ -1,3 +1,5 @@".to_owned(),
            ..reference.clone()
        },
        HunkRefV1 {
            selected_line_bitmap: full_hunk_selection_bitmap(5),
            ..reference.clone()
        },
        HunkRefV1 {
            preview_id: "preview.other".to_owned(),
            ..reference.clone()
        },
        HunkRefV1 {
            snapshot_digest: ManifestDigest::new(DIGEST_Y).unwrap(),
            ..reference.clone()
        },
    ];

    for mutated in mutations {
        assert!(
            mutated.verify_digest(&digest).is_err(),
            "field drift must invalidate the HunkRef digest"
        );
    }
}

#[test]
fn git_values_roundtrip_through_serde() {
    let status = GitStatusV1 {
        repository: repository(),
        head: GitHeadStateV1::Detached {
            commit: oid(SHA1_A),
        },
        operation: GitOperationStateV1::Merge,
        entries: vec![GitStatusEntryV1::Ignored {
            path: "app.log".to_owned(),
        }],
        coverage: GitCoverageV1::degraded(vec![GitDegradationV1::IgnoredCollision]),
    };
    let diff = GitDiffV1 {
        repository: repository(),
        scope: GitDiffScopeV1::CommitRange {
            base: oid(SHA1_A),
            head: oid(SHA1_B),
        },
        files: vec![file_diff("src/a.rs", GitChangeKindV1::Modified)],
        coverage: GitCoverageV1::complete(),
    };
    let history = GitHistoryV1 {
        repository: repository(),
        commits: vec![commit(SHA1_A)],
        truncated: true,
        coverage: GitCoverageV1::degraded(vec![GitDegradationV1::TruncatedOutput]),
    };
    let reference = hunk_ref();

    for value in [
        serde_json::to_string(&status).unwrap(),
        serde_json::to_string(&diff).unwrap(),
        serde_json::to_string(&history).unwrap(),
        serde_json::to_string(&reference).unwrap(),
    ] {
        assert!(serde_json::from_str::<serde_json::Value>(&value).is_ok());
    }

    let status_wire = serde_json::to_string(&status).unwrap();
    assert_eq!(
        serde_json::from_str::<GitStatusV1>(&status_wire).unwrap(),
        status
    );
    let diff_wire = serde_json::to_string(&diff).unwrap();
    assert_eq!(serde_json::from_str::<GitDiffV1>(&diff_wire).unwrap(), diff);
    let history_wire = serde_json::to_string(&history).unwrap();
    assert_eq!(
        serde_json::from_str::<GitHistoryV1>(&history_wire).unwrap(),
        history
    );
    let ref_wire = serde_json::to_string(&reference).unwrap();
    assert_eq!(
        serde_json::from_str::<HunkRefV1>(&ref_wire).unwrap(),
        reference
    );
}
