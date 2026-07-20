use tracedecay::code_index::generations::GenerationPlanner;
use tracedecay::code_index::git_join::{
    GenerationGitFileJoinStateV1, GenerationGitFileOnlyReasonV1, GenerationGitJoinCoverageV1,
    GenerationGitJoinErrorV1, GenerationGitJoinV1, GenerationGitWatermarkV1,
    GitFileContentIdentityV1,
};
use tracedecay::code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, GitChangeKindV1, GitCoverageV1, GitDiffScopeV1,
    GitDiffV1, GitFileDiffV1, GitFileModeV1, GitHunkV1, GitOidV1, ManifestDigest, RepositoryId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1, UtcMicros,
    ValidatedCodeSnapshotV1,
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

fn watermark(snapshot: &ValidatedCodeSnapshotV1) -> GenerationGitWatermarkV1 {
    GenerationGitWatermarkV1 {
        repository: snapshot.snapshot.repository.clone(),
        worktree: snapshot.snapshot.worktree.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        snapshot_content_identity: snapshot.snapshot.content_identity.clone(),
        git_snapshot_digest: manifest_digest('9'),
        captured_at: UtcMicros(11),
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
            &watermark(&snapshot),
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
        &watermark(&snapshot),
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
            &watermark(&snapshot),
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
    let mut stale = watermark(&snapshot);
    stale.snapshot_content_identity = content('e');
    assert_eq!(
        GenerationGitJoinV1::join(&manifest, &snapshot, &diff, &stale, &contents),
        Err(GenerationGitJoinErrorV1::StaleContentWatermark)
    );
}
