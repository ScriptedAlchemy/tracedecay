#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::{SessionProvider, claude_observation, git_correlation, source};

use super::failure::{classify_claude_observation_failure, classify_transcript_ingest_failure};
use super::project::{parse_git_log_commits, push_file_source};
use super::startup::{StartupUserIngestGuard, TranscriptIngestOutcome};
use super::user::{provider_selected, registered_project_roots_from};

#[tokio::test]
async fn registered_project_roots_include_modern_registry_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = temp.path().join("repo");
    let worktree = temp.path().join("repo-worktree");
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let canonical = std::fs::canonicalize(canonical).unwrap();
    let worktree = std::fs::canonicalize(worktree).unwrap();
    let db = GlobalDb::open_at(&temp.path().join("global.db"))
        .await
        .unwrap();
    db.upsert_code_project("project-1", &canonical, None, None, None)
        .await
        .unwrap();
    db.upsert_project_alias(&worktree, "project-1")
        .await
        .unwrap();

    let roots = registered_project_roots_from(&db).await.unwrap();

    assert!(roots.contains(&canonical));
    assert!(roots.contains(&worktree));
}

// macOS filesystems reject invalid UTF-8 path components with EILSEQ.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn registered_project_roots_preserve_non_unicode_current_root() {
    use std::os::unix::ffi::OsStringExt;

    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let db = GlobalDb::open_at(&temp.path().join("global.db"))
        .await
        .unwrap();
    db.upsert_code_project("project-native", &root, None, None, None)
        .await
        .unwrap();

    let roots = registered_project_roots_from(&db).await.unwrap();

    assert!(roots.contains(&root));
}

#[test]
fn provider_scoped_user_catch_up_excludes_unrelated_providers() {
    assert!(provider_selected(
        Some(SessionProvider::Hermes),
        SessionProvider::Hermes
    ));
    for unrelated in [
        SessionProvider::Codex,
        SessionProvider::Cursor,
        SessionProvider::Claude,
        SessionProvider::Vibe,
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
        SessionProvider::Kiro,
    ] {
        assert!(!provider_selected(Some(SessionProvider::Hermes), unrelated));
    }
    assert!(provider_selected(None, SessionProvider::Codex));
    assert!(provider_selected(None, SessionProvider::Hermes));
}

#[test]
fn project_claude_ingest_never_uses_legacy_transcript_source() {
    let mut sources = Vec::new();
    push_file_source(&mut sources, SessionProvider::Claude);
    assert!(sources.is_empty());
}

#[test]
fn transcript_failure_classification_is_bounded_and_drives_outcome_success() {
    let error =
        source::TranscriptIngestError::Store(tracedecay_store::TranscriptStoreError::Storage {
            operation: "private operation",
            source: Box::new(std::io::Error::other("private source detail")),
        });
    let failure = classify_transcript_ingest_failure("codex", "transcript", &error);

    assert_eq!(failure.provider, "codex");
    assert_eq!(failure.source, "transcript");
    assert_eq!(failure.reason_code, "transcript_storage_failed");
    assert!(failure.retryable);
    let outcome = TranscriptIngestOutcome::new(TranscriptIngestStats::default(), vec![failure]);
    assert!(!outcome.is_success());
    let rendered = serde_json::to_string(&outcome.failures).unwrap();
    assert!(!rendered.contains("private operation"));
    assert!(!rendered.contains("private source detail"));
}

#[test]
fn transcript_contract_failures_are_not_retryable() {
    let error = source::TranscriptIngestError::CursorKeyMismatch {
        expected: "private expected key".to_string(),
        actual: "private actual key".to_string(),
    };
    let failure = classify_transcript_ingest_failure("cursor", "hook", &error);

    assert_eq!(failure.reason_code, "transcript_cursor_key_mismatch");
    assert!(!failure.retryable);
}

#[test]
fn cursor_advance_receipt_collisions_are_permanent() {
    let error = claude_observation::ClaudeObservationIngestError::Store(
        tracedecay_store::ObservationStoreError::CursorAdvanceCollision,
    );

    let failure = classify_claude_observation_failure(&error);

    assert_eq!(failure.reason_code, "observation_cursor_advance_collision");
    assert!(!failure.retryable);
}

#[test]
fn transcript_privacy_and_non_durable_failures_are_bounded_and_permanent() {
    let privacy = source::TranscriptIngestError::Privacy(
        crate::privacy::PrivacySanitizerError::InvalidPolicy,
    );
    let privacy = classify_transcript_ingest_failure("claude", "hook", &privacy);
    assert_eq!(privacy.reason_code, "transcript_privacy_rejected");
    assert!(!privacy.retryable);

    let non_durable = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 7,
        end_offset: 99,
        reason: "private detail",
    };
    let non_durable = classify_transcript_ingest_failure("claude", "hook", &non_durable);
    assert_eq!(non_durable.reason_code, "transcript_record_non_durable");
    assert!(!non_durable.retryable);
    assert!(
        !serde_json::to_string(&non_durable)
            .unwrap()
            .contains("private detail")
    );
}

#[test]
fn transcript_source_contract_failures_are_bounded_and_permanent() {
    let errors = [
        source::TranscriptIngestError::Domain(tracedecay_domain::SessionId::new("").unwrap_err()),
        source::TranscriptIngestError::ObservationContract(
            tracedecay_domain::ClaudeFileGenerationV1::new(0).unwrap_err(),
        ),
        source::TranscriptIngestError::InvalidFrameState {
            provider: "private provider detail",
        },
        source::TranscriptIngestError::InvalidSourceIdentity {
            provider: "private provider detail",
            path: PathBuf::from("/private/source/path"),
        },
    ];

    for error in errors {
        let failure = classify_transcript_ingest_failure("claude", "hook", &error);
        assert_eq!(failure.reason_code, "transcript_source_contract_invalid");
        assert!(!failure.retryable);
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("private provider detail")
        );
    }
}

#[test]
fn parse_git_log_commits_reads_sha_and_time_skipping_malformed() {
    let stdout = concat!(
        "ABCDEF1234567890 1700000000\n",
        "\n",
        "missing-time\n",
        "cafebabe not-a-number\n",
        "deadbeefdeadbeef 1700000200\n",
    );
    let commits = parse_git_log_commits(stdout);
    assert_eq!(
        commits,
        vec![
            git_correlation::ScannedCommit {
                sha: "abcdef1234567890".to_string(),
                committed_at: 1_700_000_000,
            },
            git_correlation::ScannedCommit {
                sha: "deadbeefdeadbeef".to_string(),
                committed_at: 1_700_000_200,
            },
        ]
    );
}

#[test]
fn parse_git_log_commits_empty_is_empty() {
    assert!(parse_git_log_commits("").is_empty());
}

#[test]
fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
    let profile = tempfile::tempdir().unwrap().path().to_path_buf();
    let first = StartupUserIngestGuard::claim(profile.clone()).expect("first claim");
    assert!(StartupUserIngestGuard::claim(profile.clone()).is_none());

    drop(first);
    let mut retry = StartupUserIngestGuard::claim(profile.clone())
        .expect("an incomplete claim must release immediately");
    retry.completed = true;
    drop(retry);

    assert!(
        StartupUserIngestGuard::claim(profile).is_none(),
        "a completed sweep should suppress the startup herd during cooldown"
    );
}
