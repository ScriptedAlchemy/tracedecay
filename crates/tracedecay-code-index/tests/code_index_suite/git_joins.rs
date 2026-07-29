use tracedecay_code_index::generations::GenerationPlanner;
use tracedecay_code_index::git_join::{
    GenerationGitBlameJoinCoverageV1, GenerationGitBlameJoinV1, GenerationGitEvidenceScopeV1,
    GenerationGitFileJoinStateV1, GenerationGitFileOnlyReasonV1,
    GenerationGitHistoryJoinCoverageV1, GenerationGitHistoryJoinV1, GenerationGitJoinCoverageV1,
    GenerationGitJoinErrorV1, GenerationGitJoinV1, GenerationGitReadWatermarkV1,
    GenerationGitWatermarkV1, GitFileContentIdentityV1, GitSymbolLineBindingV1,
};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, GitBlameAvailabilityV1, GitBlameLineV1, GitBlameV1,
    GitChangeKindV1, GitCommitIdentityV1, GitCommitMetadataV1, GitCoverageV1, GitDegradationV1,
    GitDiffScopeV1, GitDiffV1, GitFileDiffV1, GitFileModeV1, GitHistoryV1, GitHunkV1, GitOidV1,
    ManifestDigest, RepositoryId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SnapshotFileDispositionV1, UtcMicros, ValidatedCodeSnapshotV1,
};

use super::support::{id, registry};

fn content(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn manifest_digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("valid fixture oid")
}

fn file(
    occurrence: &str,
    path: &str,
    digest: char,
    disposition: SnapshotFileDispositionV1,
) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: id(occurrence),
        logical_path: path.to_owned(),
        language: (disposition == SnapshotFileDispositionV1::Present).then(|| id("rust")),
        content_digest: content(digest),
        disposition,
    }
}

fn generation(
    mut files: Vec<SanitizedCodeFileV1>,
) -> (ValidatedCodeSnapshotV1, CodeGenerationManifestV1) {
    files.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id("repository.fixture"),
        worktree: Some(id("worktree.fixture")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.fixture")),
        sanitizer_revision: id("sanitizer.v1"),
        sanitization_receipts: vec![id("receipt.fixture")],
        content_identity: content('f'),
        captured_at: UtcMicros(10),
        files,
    };
    let intake = SanitizedCodeIntake::new(registry(), id("sanitizer.v1"), UtcMicros(20));
    let validated = intake
        .validate(snapshot)
        .expect("validated fixture snapshot");
    let manifest = GenerationPlanner::new(
        id("repository.fixture"),
        registry(),
        id("chunker.v1"),
        id("privacy.fixture"),
        7,
    )
    .plan_generation(&validated, None, UtcMicros(30))
    .expect("sealed fixture generation");
    (validated, manifest)
}

fn watermark(snapshot: &ValidatedCodeSnapshotV1, diff: &GitDiffV1) -> GenerationGitWatermarkV1 {
    let mut watermark = GenerationGitWatermarkV1 {
        repository: snapshot.snapshot.repository.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        snapshot_content_identity: snapshot.snapshot.content_identity.clone(),
        scope: GenerationGitEvidenceScopeV1 {
            worktree: snapshot.snapshot.worktree.clone(),
            index_tree: Some(oid('e')),
            tree: Some(oid('f')),
            reference: snapshot.snapshot.reference.clone(),
            options_digest: manifest_digest('8'),
        },
        diff_scope: diff.scope.clone(),
        git_snapshot_digest: manifest_digest('9'),
        captured_at: UtcMicros(11),
    };
    watermark.git_snapshot_digest = watermark
        .recompute_evidence_digest(diff)
        .expect("canonical Git evidence digest");
    watermark
}

fn read_watermark(snapshot: &ValidatedCodeSnapshotV1) -> GenerationGitReadWatermarkV1 {
    GenerationGitReadWatermarkV1 {
        repository: snapshot.snapshot.repository.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        snapshot_content_identity: snapshot.snapshot.content_identity.clone(),
        scope: GenerationGitEvidenceScopeV1 {
            worktree: snapshot.snapshot.worktree.clone(),
            index_tree: Some(oid('e')),
            tree: Some(oid('f')),
            reference: snapshot.snapshot.reference.clone(),
            options_digest: manifest_digest('8'),
        },
        evidence_digest: manifest_digest('9'),
        captured_at: UtcMicros(11),
    }
}

fn commit_identity() -> GitCommitIdentityV1 {
    GitCommitIdentityV1 {
        name: "Trace Decay".to_owned(),
        email: "trace@example.test".to_owned(),
        at: UtcMicros(5),
    }
}

fn hunk(byte: char) -> GitHunkV1 {
    GitHunkV1 {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
        section: None,
        patch_digest: manifest_digest(byte),
    }
}

fn text_diff(path: &str, original_path: Option<&str>, change: GitChangeKindV1) -> GitFileDiffV1 {
    GitFileDiffV1 {
        path: path.to_owned(),
        original_path: original_path.map(str::to_owned),
        change,
        old_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
        new_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
        old_blob: Some(oid('a')),
        new_blob: Some(oid('b')),
        binary: false,
        submodule: false,
        insertions: Some(1),
        deletions: Some(1),
        hunks: vec![hunk('1')],
    }
}

#[test]
fn working_staged_and_range_diffs_join_only_at_exact_watermarks() {
    let (snapshot, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let contents = vec![GitFileContentIdentityV1 {
        path: "src/live.rs".to_owned(),
        content_digest: content('a'),
    }];

    for scope in [
        GitDiffScopeV1::WorkingTree,
        GitDiffScopeV1::Staged,
        GitDiffScopeV1::CommitRange {
            base: oid('c'),
            head: oid('d'),
        },
    ] {
        let diff = GitDiffV1 {
            repository: id::<RepositoryId>("repository.fixture"),
            scope: scope.clone(),
            files: vec![text_diff("src/live.rs", None, GitChangeKindV1::Modified)],
            coverage: GitCoverageV1::complete(),
        };
        let joined = GenerationGitJoinV1::join(
            &manifest,
            &snapshot,
            &diff,
            &watermark(&snapshot, &diff),
            &contents,
        )
        .expect("exact Git/code generation join");

        assert_eq!(joined.generation_id, manifest.generation_id);
        assert_eq!(joined.code_snapshot_digest, manifest.snapshot_digest);
        assert_eq!(joined.scope, scope);
        assert_eq!(joined.coverage, GenerationGitJoinCoverageV1::Complete);
        assert_eq!(joined.files.len(), 1);
        assert_eq!(joined.files[0].file_occurrence_id.as_str(), "file.live");
        assert_eq!(
            joined.files[0].join_state,
            GenerationGitFileJoinStateV1::Exact
        );
    }
}

#[test]
fn git_snapshot_digest_binds_both_hunk_sides() {
    let (snapshot, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let diff = GitDiffV1 {
        repository: id("repository.fixture"),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![text_diff("src/live.rs", None, GitChangeKindV1::Modified)],
        coverage: GitCoverageV1::complete(),
    };
    let contents = vec![GitFileContentIdentityV1 {
        path: "src/live.rs".to_owned(),
        content_digest: content('a'),
    }];
    let evidence = watermark(&snapshot, &diff);

    let mut old_side_drift = diff.clone();
    old_side_drift.files[0].hunks[0].old_start = 2;
    let mut new_side_drift = diff;
    new_side_drift.files[0].hunks[0].new_start = 2;

    for tampered in [old_side_drift, new_side_drift] {
        assert_eq!(
            GenerationGitJoinV1::join(&manifest, &snapshot, &tampered, &evidence, &contents),
            Err(GenerationGitJoinErrorV1::StaleGitEvidence)
        );
    }
}

#[test]
fn git_snapshot_digest_binds_worktree_index_tree_ref_and_options() {
    let (snapshot, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let diff = GitDiffV1 {
        repository: id("repository.fixture"),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![text_diff("src/live.rs", None, GitChangeKindV1::Modified)],
        coverage: GitCoverageV1::complete(),
    };
    let contents = vec![GitFileContentIdentityV1 {
        path: "src/live.rs".to_owned(),
        content_digest: content('a'),
    }];
    let evidence = watermark(&snapshot, &diff);

    let mut worktree_drift = evidence.clone();
    worktree_drift.scope.worktree = Some(id("worktree.other"));
    let mut index_drift = evidence.clone();
    index_drift.scope.index_tree = Some(oid('a'));
    let mut tree_drift = evidence.clone();
    tree_drift.scope.tree = Some(oid('b'));
    let mut ref_drift = evidence.clone();
    ref_drift.scope.reference = Some(id("ref.other"));
    let mut options_drift = evidence;
    options_drift.scope.options_digest = manifest_digest('7');

    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &worktree_drift, &contents),
        Err(GenerationGitJoinErrorV1::WorktreeMismatch)
    );
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &index_drift, &contents),
        Err(GenerationGitJoinErrorV1::StaleGitEvidence)
    );
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &tree_drift, &contents),
        Err(GenerationGitJoinErrorV1::StaleGitEvidence)
    );
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &ref_drift, &contents),
        Err(GenerationGitJoinErrorV1::ReferenceMismatch)
    );
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &options_drift, &contents),
        Err(GenerationGitJoinErrorV1::StaleGitEvidence)
    );
}

#[test]
fn rename_deletion_and_binary_evidence_preserve_native_git_typing() {
    let (snapshot, manifest) = generation(vec![
        file(
            "file.binary",
            "assets/blob.bin",
            'b',
            SnapshotFileDispositionV1::Binary,
        ),
        file(
            "file.deleted",
            "src/deleted.rs",
            'c',
            SnapshotFileDispositionV1::Deleted,
        ),
        file(
            "file.renamed",
            "src/renamed.rs",
            'd',
            SnapshotFileDispositionV1::Renamed,
        ),
    ]);
    let mut deleted = text_diff("src/deleted.rs", None, GitChangeKindV1::Deleted);
    deleted.new_blob = None;
    deleted.new_mode = None;
    deleted.insertions = Some(0);
    let binary = GitFileDiffV1 {
        path: "assets/blob.bin".to_owned(),
        original_path: None,
        change: GitChangeKindV1::Modified,
        old_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
        new_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
        old_blob: Some(oid('a')),
        new_blob: Some(oid('b')),
        binary: true,
        submodule: false,
        insertions: None,
        deletions: None,
        hunks: Vec::new(),
    };
    let diff = GitDiffV1 {
        repository: id("repository.fixture"),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![
            binary,
            deleted,
            text_diff(
                "src/renamed.rs",
                Some("src/original.rs"),
                GitChangeKindV1::Renamed,
            ),
        ],
        coverage: GitCoverageV1::complete(),
    };
    let contents = vec![
        GitFileContentIdentityV1 {
            path: "assets/blob.bin".to_owned(),
            content_digest: content('b'),
        },
        GitFileContentIdentityV1 {
            path: "src/deleted.rs".to_owned(),
            content_digest: content('c'),
        },
        GitFileContentIdentityV1 {
            path: "src/renamed.rs".to_owned(),
            content_digest: content('d'),
        },
    ];

    let joined = GenerationGitJoinV1::join(
        &manifest,
        &snapshot,
        &diff,
        &watermark(&snapshot, &diff),
        &contents,
    )
    .expect("typed non-text cases remain joinable");

    assert!(matches!(
        joined.coverage,
        GenerationGitJoinCoverageV1::Partial { .. }
    ));
    assert_eq!(
        joined.files[0].join_state,
        GenerationGitFileJoinStateV1::FileOnly {
            reason: GenerationGitFileOnlyReasonV1::Binary,
        }
    );
    assert_eq!(joined.files[1].change, GitChangeKindV1::Deleted);
    assert_eq!(
        joined.files[2].original_path.as_deref(),
        Some("src/original.rs")
    );
}

#[test]
fn stale_generation_or_content_watermarks_never_join() {
    let (snapshot, mut manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let diff = GitDiffV1 {
        repository: id("repository.fixture"),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![text_diff("src/live.rs", None, GitChangeKindV1::Modified)],
        coverage: GitCoverageV1::complete(),
    };
    let contents = vec![GitFileContentIdentityV1 {
        path: "src/live.rs".to_owned(),
        content_digest: content('a'),
    }];

    manifest.snapshot_digest = manifest_digest('8');
    assert_eq!(
        GenerationGitJoinV1::join(
            &manifest,
            &snapshot,
            &diff,
            &watermark(&snapshot, &diff),
            &contents,
        ),
        Err(GenerationGitJoinErrorV1::StaleGenerationWatermark)
    );

    let (_, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let mut stale = watermark(&snapshot, &diff);
    stale.snapshot_content_identity = content('e');
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &stale, &contents),
        Err(GenerationGitJoinErrorV1::StaleContentWatermark)
    );
}

#[test]
fn history_and_blame_preserve_native_git_evidence_and_occurrence_ids() {
    let (snapshot, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let history = GitHistoryV1 {
        repository: id("repository.fixture"),
        commits: vec![GitCommitMetadataV1 {
            commit: oid('c'),
            tree: oid('d'),
            parents: vec![oid('b')],
            author: commit_identity(),
            committer: commit_identity(),
            subject: "join history".to_owned(),
            message_digest: manifest_digest('4'),
        }],
        truncated: false,
        coverage: GitCoverageV1::complete(),
    };
    let mut history_watermark = read_watermark(&snapshot);
    history_watermark.evidence_digest = history_watermark
        .recompute_history_digest(&history)
        .expect("canonical history evidence digest");
    let joined_history =
        GenerationGitHistoryJoinV1::join(&manifest, &snapshot, &history, &history_watermark)
            .expect("history joins to the exact generation");

    assert_eq!(joined_history.history, history);
    assert_eq!(
        joined_history.coverage,
        GenerationGitHistoryJoinCoverageV1::Complete
    );

    let blame = GitBlameV1 {
        repository: id("repository.fixture"),
        path: "src/live.rs".to_owned(),
        lines: vec![GitBlameLineV1 {
            final_line: 2,
            origin_line: 7,
            commit: oid('c'),
            author: commit_identity(),
            boundary: false,
            previous: None,
        }],
        availability: GitBlameAvailabilityV1::Available,
        coverage: GitCoverageV1::complete(),
    };
    let mut blame_watermark = read_watermark(&snapshot);
    blame_watermark.evidence_digest = blame_watermark
        .recompute_blame_digest(&blame)
        .expect("canonical blame evidence digest");
    let joined_blame = GenerationGitBlameJoinV1::join(
        &manifest,
        &snapshot,
        &blame,
        &blame_watermark,
        &GitFileContentIdentityV1 {
            path: "src/live.rs".to_owned(),
            content_digest: content('a'),
        },
        &[GitSymbolLineBindingV1 {
            generation_id: manifest.generation_id.clone(),
            file_occurrence_id: id("file.live"),
            symbol_occurrence_id: id("symbol.live"),
            content_digest: content('a'),
            start_line: 1,
            end_line: 3,
        }],
    )
    .expect("blame joins to exact file and symbol occurrences");

    assert_eq!(
        joined_blame.coverage,
        GenerationGitBlameJoinCoverageV1::Complete
    );
    assert_eq!(
        joined_blame.lines[0].symbol_occurrence_ids,
        vec![id("symbol.live")]
    );
}

#[test]
fn history_blame_degradation_and_tampering_remain_explicit() {
    let (snapshot, manifest) = generation(vec![file(
        "file.live",
        "src/live.rs",
        'a',
        SnapshotFileDispositionV1::Present,
    )]);
    let history = GitHistoryV1 {
        repository: id("repository.fixture"),
        commits: Vec::new(),
        truncated: true,
        coverage: GitCoverageV1::degraded(vec![GitDegradationV1::ShallowBoundary]),
    };
    let mut history_watermark = read_watermark(&snapshot);
    history_watermark.evidence_digest = history_watermark
        .recompute_history_digest(&history)
        .expect("canonical history evidence digest");
    let joined =
        GenerationGitHistoryJoinV1::join(&manifest, &snapshot, &history, &history_watermark)
            .expect("degraded history remains typed evidence");
    assert!(matches!(
        joined.coverage,
        GenerationGitHistoryJoinCoverageV1::Partial {
            truncated: true,
            ..
        }
    ));

    let blame = GitBlameV1 {
        repository: id("repository.fixture"),
        path: "src/live.rs".to_owned(),
        lines: vec![GitBlameLineV1 {
            final_line: 1,
            origin_line: 1,
            commit: oid('c'),
            author: commit_identity(),
            boundary: false,
            previous: None,
        }],
        availability: GitBlameAvailabilityV1::Available,
        coverage: GitCoverageV1::complete(),
    };
    let mut blame_watermark = read_watermark(&snapshot);
    blame_watermark.evidence_digest = blame_watermark
        .recompute_blame_digest(&blame)
        .expect("canonical blame evidence digest");
    let mut tampered = blame;
    tampered.lines[0].origin_line = 2;

    assert_eq!(
        GenerationGitBlameJoinV1::join(
            &manifest,
            &snapshot,
            &tampered,
            &blame_watermark,
            &GitFileContentIdentityV1 {
                path: "src/live.rs".to_owned(),
                content_digest: content('a'),
            },
            &[],
        ),
        Err(GenerationGitJoinErrorV1::StaleGitEvidence)
    );
}
