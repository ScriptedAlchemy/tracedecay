use tracedecay_domain::{
    GitCommitIdentityV1, GitCoverageV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexIdempotencyKey, GitIndexJournalPhaseV1, GitIndexPreviewDispositionV1,
    GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1,
    GitObjectFormatV1, GitOidV1, ManifestDigest, ProjectId, RepositoryId,
    RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesQueryV1,
    CodeRecoveryRepositoriesQueryV1, GitIndexPreviewInputReadV1, GitIndexTransactionBeginRequestV1,
    GitIndexTransactionBeginResultV1, GitIndexTransactionStoreError,
    GitIndexTransactionTerminalWriteV1,
};

use super::read::GitIndexReadExecutor;
use super::store::GlobalDbGitIndexTransactionStore;
use crate::{RegisteredGlobalDb, tests::harness::RegisteredGlobalDbHarness};

fn key(value: &str) -> GitIndexIdempotencyKey {
    GitIndexIdempotencyKey::new(value.to_owned()).expect("idempotency key")
}

async fn candidates_page(
    executor: &GitIndexReadExecutor<'_, '_>,
    query: CodeRecoveryCandidatesQueryV1,
) -> tracedecay_store::CodeRecoveryCandidatesPageV1 {
    match executor
        .execute_read(&CodeReadOperationV1::RecoveryCandidates(query))
        .await
        .expect("candidate page")
    {
        CodeReadResultV1::RecoveryCandidates(page) => page,
        other => panic!("unexpected candidate result: {other:?}"),
    }
}

async fn repositories_page(
    executor: &GitIndexReadExecutor<'_, '_>,
    query: CodeRecoveryRepositoriesQueryV1,
) -> tracedecay_store::CodeRecoveryRepositoriesPageV1 {
    match executor
        .execute_read(&CodeReadOperationV1::RecoveryRepositories(query))
        .await
        .expect("repository page")
    {
        CodeReadResultV1::RecoveryRepositories(page) => page,
        other => panic!("unexpected repository result: {other:?}"),
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture object id")
}

fn preview() -> GitIndexPreviewV1 {
    preview_for(
        "repository.git-transaction.fixture",
        "preview.git-transaction.fixture",
        UtcMicros(100),
    )
}

fn preview_input() -> GitIndexPreviewInputV1 {
    let preview = preview();
    GitIndexPreviewInputV1::new_commit(
        preview.preview_id,
        preview.repository_snapshot,
        GitIndexCommitIntentV1::new(
            "restart-stable private commit intent\n".to_owned(),
            GitCommitIdentityV1 {
                name: "Preview Author".to_owned(),
                email: "preview-author@example.com".to_owned(),
                at: UtcMicros(1_000_000),
            },
            GitCommitIdentityV1 {
                name: "Preview Committer".to_owned(),
                email: "preview-committer@example.com".to_owned(),
                at: UtcMicros(1_000_000),
            },
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .expect("private commit intent"),
        UtcMicros(10),
        UtcMicros(30_000_010),
    )
    .expect("preview input")
}

fn preview_for(repository_id: &str, preview_id: &str, expires_at: UtcMicros) -> GitIndexPreviewV1 {
    let snapshot = RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.git-transaction.fixture"),
        id::<RepositoryId>(repository_id),
        Some(id::<WorktreeId>("worktree.git-transaction.fixture")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        tracedecay_domain::GitOperationStateV1::None,
        Some(digest('0')),
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("repository snapshot")
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('5'),
    )
    .expect("native repository snapshot");
    let intent = GitIndexCommitIntentV1::new(
        "sensitive commit body must remain ephemeral\n".to_owned(),
        GitCommitIdentityV1 {
            name: "Sensitive Author".to_owned(),
            email: "sensitive-author@example.com".to_owned(),
            at: UtcMicros(1_000_000),
        },
        GitCommitIdentityV1 {
            name: "Sensitive Committer".to_owned(),
            email: "sensitive-committer@example.com".to_owned(),
            at: UtcMicros(1_000_000),
        },
        GitIndexSigningPolicyV1::SignatureRequired {
            key_reference: "sensitive-signing-key".to_owned(),
        },
    )
    .expect("commit intent");
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    GitIndexPreviewV1::new_with_commit_intent(
        GitIndexPreviewId::new(preview_id).expect("preview id"),
        GitIndexTransactionOperationV1::CommitIndex,
        snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        snapshot.index.tree_id.clone(),
        Some(&intent),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        expires_at,
    )
    .expect("preview")
}

fn begin_request(preview: &GitIndexPreviewV1, key: &str) -> GitIndexTransactionBeginRequestV1 {
    GitIndexTransactionBeginRequestV1 {
        idempotency_key: GitIndexIdempotencyKey::new(key.to_owned()).expect("idempotency key"),
        input_digest: digest('6'),
        preview: preview.clone(),
        journal: GitIndexTransactionJournalV1::prepared(
            GitIndexTransactionId::new(format!("transaction.{key}")).expect("transaction id"),
            preview,
            UtcMicros(11),
        )
        .expect("prepared journal"),
    }
}

fn terminal_write(
    request: &GitIndexTransactionBeginRequestV1,
    outcome: GitIndexReceiptOutcomeV1,
    committed_at: UtcMicros,
) -> GitIndexTransactionTerminalWriteV1 {
    let mut journal = request.journal.clone();
    journal
        .advance(
            match outcome {
                GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
                GitIndexReceiptOutcomeV1::AbortedNoChange => {
                    GitIndexJournalPhaseV1::AbortedNoChange
                }
                GitIndexReceiptOutcomeV1::NeedsInspection => {
                    GitIndexJournalPhaseV1::NeedsInspection
                }
            },
            committed_at,
        )
        .expect("terminal journal");
    let receipt = GitIndexTransactionReceiptV1::new(
        GitIndexReceiptId::new(format!(
            "receipt.{}",
            request.journal.transaction_id.as_str()
        ))
        .expect("receipt id"),
        request.journal.transaction_id.clone(),
        &request.preview,
        request.preview.repository_snapshot_digest.clone(),
        request.preview.repository_snapshot.index.tree_id.clone(),
        request.preview.repository_snapshot.head.commit().cloned(),
        None,
        outcome,
        committed_at,
    )
    .expect("terminal receipt");
    GitIndexTransactionTerminalWriteV1 {
        idempotency_key: request.idempotency_key.clone(),
        expected_phase_epoch: journal.phase_epoch,
        journal,
        receipt,
    }
}

async fn open_database() -> RegisteredGlobalDbHarness {
    RegisteredGlobalDbHarness::open("git-index-transaction-store").await
}

fn test_store(database: &RegisteredGlobalDb) -> GlobalDbGitIndexTransactionStore<'_> {
    GlobalDbGitIndexTransactionStore::new(database)
}

#[tokio::test]
async fn preview_commitments_are_immutable_and_conflicts_are_rejected() {
    let database = open_database().await;
    let store = test_store(database.registered.as_ref());
    let original = preview();
    store
        .save_preview(original.clone())
        .await
        .expect("save immutable preview");
    store
        .save_preview(original.clone())
        .await
        .expect("identical preview save is idempotent");
    assert_eq!(
        store
            .read_preview(&original.preview_id)
            .await
            .expect("read preview"),
        Some(original.clone())
    );

    let conflicting = preview_for(
        original.repository_snapshot.repository_id.as_str(),
        original.preview_id.as_str(),
        UtcMicros(101),
    );
    assert_eq!(
        store.save_preview(conflicting).await,
        Err(GitIndexTransactionStoreError::PreviewConflict)
    );
    assert_eq!(
        store
            .read_preview(&original.preview_id)
            .await
            .expect("read immutable preview"),
        Some(original)
    );
}

#[tokio::test]
async fn journal_compare_and_swap_rejects_stale_phase_epochs() {
    let database = open_database().await;
    let store = test_store(database.registered.as_ref());
    let request = begin_request(&preview(), "idempotency.journal-cas.fixture");
    store
        .begin_or_replay(request.clone())
        .await
        .expect("start transaction");
    let mut replacement = request.journal.clone();
    replacement
        .advance(GitIndexJournalPhaseV1::NativeApplyStarted, UtcMicros(12))
        .expect("advance journal");
    assert_eq!(
        store
            .compare_and_swap_journal(
                &request.idempotency_key,
                request.journal.phase_epoch,
                replacement.clone(),
            )
            .await
            .expect("first compare-and-swap"),
        replacement
    );
    assert_eq!(
        store
            .compare_and_swap_journal(
                &request.idempotency_key,
                request.journal.phase_epoch,
                replacement,
            )
            .await,
        Err(GitIndexTransactionStoreError::JournalConflict)
    );
}

#[tokio::test]
async fn canonical_schema_persists_only_commit_intent_digest() {
    let database = open_database().await;
    let preview = preview();
    let store = test_store(database.registered.as_ref());
    store
        .save_preview(preview.clone())
        .await
        .expect("save immutable preview");

    let snapshot = database
        .registered
        .read_snapshot()
        .await
        .expect("schema snapshot");
    let mut rows = snapshot
        .query(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'git_index_%'
             ORDER BY name",
            (),
        )
        .await
        .expect("list transaction tables");
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.expect("read transaction table") {
        tables.push(row.get::<String>(0).expect("table name"));
    }
    assert_eq!(
        tables,
        vec![
            "git_index_preview_commitments",
            "git_index_preview_inputs",
            "git_index_repository_quarantines",
            "git_index_transaction_inputs",
            "git_index_transaction_journals",
            "git_index_transaction_receipts",
        ]
    );
    let mut rows = snapshot
        .query(
            "SELECT preview_json, commit_intent_digest
             FROM git_index_preview_commitments WHERE preview_id = ?1",
            params![preview.preview_id.as_str()],
        )
        .await
        .expect("read preview commitment");
    let row = rows
        .next()
        .await
        .expect("read preview row")
        .expect("stored preview row");
    let encoded = row.get::<String>(0).expect("preview json");
    assert_eq!(
        row.get::<Option<String>>(1).expect("intent digest"),
        preview
            .commit_intent_digest
            .as_ref()
            .map(ToString::to_string)
    );
    for secret in [
        "sensitive commit body",
        "Sensitive Author",
        "sensitive-author@example.com",
        "Sensitive Committer",
        "sensitive-committer@example.com",
        "sensitive-signing-key",
    ] {
        assert!(
            !encoded.contains(secret),
            "persistent preview commitment leaked {secret:?}"
        );
    }
}

#[tokio::test]
async fn preview_input_survives_restart_then_gc_retains_typed_expiry_without_private_payload() {
    let database = open_database().await;
    let input = preview_input();
    let preview_id = input.preview_id.clone();
    {
        let store = test_store(database.registered.as_ref());
        store
            .save_preview_input(input.clone())
            .await
            .expect("save private preview input");
        assert_eq!(
            store
                .next_live_preview_input_expiry()
                .await
                .expect("read next expiry"),
            Some(UtcMicros(30_000_010))
        );
        assert_eq!(
            store
                .read_preview_input(&preview_id, UtcMicros(30_000_009))
                .await
                .expect("read current input"),
            GitIndexPreviewInputReadV1::Available(Box::new(input.clone()))
        );
    }
    let reopened = database.restart().await;
    let store = test_store(reopened.registered.as_ref());
    assert_eq!(
        store
            .read_preview_input(&preview_id, UtcMicros(30_000_009))
            .await
            .expect("read input after restart"),
        GitIndexPreviewInputReadV1::Available(Box::new(input))
    );
    let gc = store
        .purge_expired_preview_inputs_and_next(UtcMicros(30_000_010), 1)
        .await
        .expect("purge one expired payload");
    assert_eq!(gc.purged, 1);
    assert_eq!(gc.next_expiry, None);
    assert!(matches!(
        store
            .read_preview_input(&preview_id, UtcMicros(30_000_010))
            .await
            .expect("read expired tombstone"),
        GitIndexPreviewInputReadV1::Expired {
            expired_at: UtcMicros(30_000_010),
            purged_at: Some(UtcMicros(30_000_010)),
        }
    ));

    let snapshot = reopened
        .registered
        .read_snapshot()
        .await
        .expect("schema snapshot");
    let mut rows = snapshot
        .query(
            "SELECT input_json, purged_at
             FROM git_index_preview_inputs WHERE preview_id = ?1",
            params![preview_id.as_str()],
        )
        .await
        .expect("read purged input row");
    let row = rows
        .next()
        .await
        .expect("read purged input")
        .expect("purged input tombstone");
    assert_eq!(row.get::<Option<String>>(0).expect("private payload"), None);
    assert_eq!(
        row.get::<Option<i64>>(1).expect("purged at"),
        Some(30_000_010)
    );
}

#[tokio::test]
async fn restart_replays_terminal_receipt_and_rejects_conflicting_input() {
    let database = open_database().await;
    let preview = preview();
    let request = begin_request(&preview, "idempotency.restart.fixture");
    let receipt = {
        let store = test_store(database.registered.as_ref());
        assert!(matches!(
            store.begin_or_replay(request.clone()).await,
            Ok(GitIndexTransactionBeginResultV1::Started(_))
        ));
        store
            .write_terminal(terminal_write(
                &request,
                GitIndexReceiptOutcomeV1::AbortedNoChange,
                UtcMicros(12),
            ))
            .await
            .expect("atomic terminal receipt")
    };
    let reopened = database.restart().await;
    let store = test_store(reopened.registered.as_ref());
    assert!(matches!(
        store.begin_or_replay(request.clone()).await,
        Ok(GitIndexTransactionBeginResultV1::Replay(stored)) if *stored == receipt
    ));
    let mut conflicting = request;
    conflicting.input_digest = digest('7');
    assert_eq!(
        store.begin_or_replay(conflicting).await,
        Err(GitIndexTransactionStoreError::IdempotencyConflict)
    );
    assert!(
        !reopened
            .registered
            .db_path()
            .with_file_name("git-index-transactions.json")
            .exists(),
        "the canonical store must not create a JSON side-file"
    );
}

#[tokio::test]
async fn terminal_failure_receipts_persist_and_only_inspection_requires_recovery() {
    let database = open_database().await;
    let preview = preview();
    let aborted = begin_request(&preview, "idempotency.failure.aborted");
    let inspection = begin_request(&preview, "idempotency.failure.inspection");
    let (aborted_receipt, inspection_receipt) = {
        let store = test_store(database.registered.as_ref());
        store
            .begin_or_replay(aborted.clone())
            .await
            .expect("start abort transaction");
        let aborted_receipt = store
            .write_terminal(terminal_write(
                &aborted,
                GitIndexReceiptOutcomeV1::AbortedNoChange,
                UtcMicros(12),
            ))
            .await
            .expect("persist abort receipt");

        store
            .begin_or_replay(inspection.clone())
            .await
            .expect("start inspection transaction");
        let inspection_receipt = store
            .write_terminal(terminal_write(
                &inspection,
                GitIndexReceiptOutcomeV1::NeedsInspection,
                UtcMicros(13),
            ))
            .await
            .expect("persist inspection receipt");
        (aborted_receipt, inspection_receipt)
    };
    let reopened = database.restart().await;
    let store = test_store(reopened.registered.as_ref());
    assert!(matches!(
        store.begin_or_replay(aborted).await,
        Ok(GitIndexTransactionBeginResultV1::Replay(stored))
            if *stored == aborted_receipt
    ));
    assert!(matches!(
        store.begin_or_replay(inspection.clone()).await,
        Ok(GitIndexTransactionBeginResultV1::Replay(stored))
            if *stored == inspection_receipt
    ));
    let candidates = store
        .recovery_candidates(&preview.repository_snapshot.repository_id)
        .await
        .expect("recovery candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].journal.transaction_id,
        inspection.journal.transaction_id
    );
}

#[tokio::test]
async fn quarantine_is_durable_and_new_keys_remain_blocked_until_proven_clear() {
    let database = open_database().await;
    let preview = preview();
    let request = begin_request(&preview, "idempotency.quarantine.fixture");
    {
        let store = test_store(database.registered.as_ref());
        store
            .begin_or_replay(request.clone())
            .await
            .expect("start transaction");
        store
            .quarantine_repository(
                &preview.repository_snapshot.repository_id,
                &request.journal.transaction_id,
            )
            .await
            .expect("fence repository");
        store
            .write_terminal(terminal_write(
                &request,
                GitIndexReceiptOutcomeV1::NeedsInspection,
                UtcMicros(12),
            ))
            .await
            .expect("write inspection receipt");
    }
    let reopened = database.restart().await;
    let store = test_store(reopened.registered.as_ref());
    let mut blocked = begin_request(&preview, "idempotency.quarantine.blocked");
    blocked.journal.transaction_id =
        GitIndexTransactionId::new("transaction.idempotency.quarantine.blocked")
            .expect("blocked transaction id");
    assert_eq!(
        store.begin_or_replay(blocked.clone()).await,
        Err(GitIndexTransactionStoreError::RepositoryQuarantined)
    );
    assert_eq!(
        store
            .recovery_repositories()
            .await
            .expect("recovery repositories"),
        vec![preview.repository_snapshot.repository_id.clone()]
    );
    assert!(matches!(
        store.begin_or_replay(request.clone()).await,
        Ok(GitIndexTransactionBeginResultV1::Replay(receipt))
            if receipt.outcome == GitIndexReceiptOutcomeV1::NeedsInspection
    ));

    let insufficient_proof = GitIndexTransactionReceiptV1::new_with_final_snapshot(
        GitIndexReceiptId::new("receipt.quarantine.insufficient-proof").expect("proof id"),
        request.journal.transaction_id.clone(),
        &preview,
        None,
        preview.repository_snapshot.index.tree_id.clone(),
        preview.repository_snapshot.head.commit().cloned(),
        None,
        GitIndexReceiptOutcomeV1::AbortedNoChange,
        UtcMicros(13),
    )
    .expect("insufficient recovery proof");
    assert_eq!(
        store
            .clear_repository_quarantine(
                &preview.repository_snapshot.repository_id,
                &request.journal.transaction_id,
                insufficient_proof,
            )
            .await,
        Err(GitIndexTransactionStoreError::ReceiptConflict)
    );

    let proof = GitIndexTransactionReceiptV1::new(
        GitIndexReceiptId::new("receipt.quarantine.recovery-proof").expect("proof id"),
        request.journal.transaction_id.clone(),
        &preview,
        preview.repository_snapshot_digest.clone(),
        preview.repository_snapshot.index.tree_id.clone(),
        preview.repository_snapshot.head.commit().cloned(),
        None,
        GitIndexReceiptOutcomeV1::AbortedNoChange,
        UtcMicros(13),
    )
    .expect("recovery proof");
    store
        .clear_repository_quarantine(
            &preview.repository_snapshot.repository_id,
            &request.journal.transaction_id,
            proof,
        )
        .await
        .expect("proven quarantine clear");
    assert_eq!(
        store
            .quarantine_repository(
                &preview.repository_snapshot.repository_id,
                &request.journal.transaction_id,
            )
            .await,
        Err(GitIndexTransactionStoreError::ReceiptConflict),
        "a proven clear must retain its resolution evidence rather than reactivate"
    );
    let reopened = reopened.restart().await;
    let store = test_store(reopened.registered.as_ref());
    assert!(
        store
            .recovery_repositories()
            .await
            .expect("recovery repositories after clear")
            .is_empty()
    );
    assert!(matches!(
        store.begin_or_replay(blocked).await,
        Ok(GitIndexTransactionBeginResultV1::Started(_))
    ));
}

#[tokio::test]
async fn proven_terminal_receipt_atomically_resolves_an_admission_quarantine() {
    let database = open_database().await;
    let preview = preview();
    let request = begin_request(&preview, "idempotency.quarantine.terminal-proof");
    let store = test_store(database.registered.as_ref());
    store
        .begin_or_replay(request.clone())
        .await
        .expect("start transaction");
    store
        .quarantine_repository(
            &preview.repository_snapshot.repository_id,
            &request.journal.transaction_id,
        )
        .await
        .expect("fence admitted transaction");

    store
        .write_terminal(terminal_write(
            &request,
            GitIndexReceiptOutcomeV1::AbortedNoChange,
            UtcMicros(12),
        ))
        .await
        .expect("publish proof and resolve fence atomically");

    assert!(
        store
            .recovery_repositories()
            .await
            .expect("recovery repositories")
            .is_empty()
    );
    let snapshot = database
        .registered
        .read_snapshot()
        .await
        .expect("quarantine snapshot");
    let mut rows = snapshot
        .query(
            "SELECT active, resolution_receipt_json IS NOT NULL
             FROM git_index_repository_quarantines
             WHERE repository_id = ?1 AND transaction_id = ?2",
            params![
                preview.repository_snapshot.repository_id.as_str(),
                request.journal.transaction_id.as_str(),
            ],
        )
        .await
        .expect("read retained quarantine");
    let row = rows
        .next()
        .await
        .expect("read quarantine row")
        .expect("retained quarantine row");
    assert_eq!(row.get::<i64>(0).expect("active"), 0);
    assert_eq!(row.get::<i64>(1).expect("resolution receipt"), 1);
}

#[tokio::test]
async fn recovery_indexes_include_only_repositories_with_recoverable_records() {
    let database = open_database().await;
    let first_preview = preview();
    let second_preview = preview_for(
        "repository.git-transaction.second",
        "preview.git-transaction.second",
        UtcMicros(100),
    );
    let first = begin_request(&first_preview, "idempotency.recovery.first");
    let second = begin_request(&second_preview, "idempotency.recovery.second");
    let store = test_store(database.registered.as_ref());
    store
        .begin_or_replay(first.clone())
        .await
        .expect("start first recovery candidate");
    store
        .begin_or_replay(second.clone())
        .await
        .expect("start second recovery candidate");

    assert_eq!(
        store
            .recovery_repositories()
            .await
            .expect("recovery repositories"),
        vec![
            first_preview.repository_snapshot.repository_id.clone(),
            second_preview.repository_snapshot.repository_id.clone(),
        ]
    );
    assert_eq!(
        store
            .recovery_candidates(&first_preview.repository_snapshot.repository_id)
            .await
            .expect("first recovery candidates")
            .into_iter()
            .map(|record| record.journal.transaction_id)
            .collect::<Vec<_>>(),
        vec![first.journal.transaction_id.clone()]
    );

    store
        .write_terminal(terminal_write(
            &first,
            GitIndexReceiptOutcomeV1::AbortedNoChange,
            UtcMicros(12),
        ))
        .await
        .expect("terminalize first candidate");
    assert_eq!(
        store
            .recovery_repositories()
            .await
            .expect("remaining recovery repositories"),
        vec![second_preview.repository_snapshot.repository_id]
    );
}

#[tokio::test]
async fn failed_inspection_terminal_insert_rolls_back_journal_and_quarantine() {
    let database = open_database().await;
    let preview = preview();
    let request = begin_request(&preview, "idempotency.atomic.fixture");
    let store = test_store(database.registered.as_ref());
    store
        .begin_or_replay(request.clone())
        .await
        .expect("start transaction");
    database
        .registered
        .writer_connection()
        .expect("writer connection for fault trigger")
        .execute_batch(
            "CREATE TRIGGER fail_git_index_terminal_receipt
             BEFORE INSERT ON git_index_transaction_receipts
             BEGIN
                SELECT RAISE(ABORT, 'injected terminal receipt failure');
             END;",
        )
        .await
        .expect("install fault trigger");
    assert_eq!(
        store
            .write_terminal(terminal_write(
                &request,
                GitIndexReceiptOutcomeV1::NeedsInspection,
                UtcMicros(12),
            ))
            .await,
        Err(GitIndexTransactionStoreError::Unavailable)
    );
    assert!(matches!(
        store.begin_or_replay(request).await,
        Ok(GitIndexTransactionBeginResultV1::RecoveryRequired(record))
            if record.journal.phase == GitIndexJournalPhaseV1::Prepared
                && record.terminal_receipt.is_none()
    ));
    let new_request = begin_request(&preview, "idempotency.atomic.after-failure");
    assert!(matches!(
        store.begin_or_replay(new_request).await,
        Ok(GitIndexTransactionBeginResultV1::Started(_))
    ));
}

#[tokio::test]
async fn read_executor_round_trips_preview_and_transaction_record() {
    let database = open_database().await;
    let preview = preview();
    let request = begin_request(&preview, "idempotency.read-executor.round-trip");
    let store = test_store(database.registered.as_ref());
    store
        .save_preview(preview.clone())
        .await
        .expect("save preview");
    let record = match store
        .begin_or_replay(request.clone())
        .await
        .expect("start transaction")
    {
        GitIndexTransactionBeginResultV1::Started(record) => *record,
        other => panic!("unexpected begin outcome: {other:?}"),
    };

    let executor = GitIndexReadExecutor::new(&store);

    assert_eq!(
        executor
            .execute_read(&CodeReadOperationV1::Preview(preview.preview_id.clone()))
            .await
            .expect("preview read"),
        CodeReadResultV1::Preview(Box::new(Some(preview.clone())))
    );
    assert_eq!(
        executor
            .execute_read(&CodeReadOperationV1::TransactionRecord(
                request.idempotency_key.clone(),
            ))
            .await
            .expect("record read"),
        CodeReadResultV1::TransactionRecord(Box::new(Some(record)))
    );

    // Absent keys project to `None` rather than an error.
    assert_eq!(
        executor
            .execute_read(&CodeReadOperationV1::Preview(
                GitIndexPreviewId::new("preview.read-executor.absent").expect("absent preview id"),
            ))
            .await
            .expect("absent preview read"),
        CodeReadResultV1::Preview(Box::new(None))
    );
    assert_eq!(
        executor
            .execute_read(&CodeReadOperationV1::TransactionRecord(key(
                "idempotency.read-executor.absent"
            )))
            .await
            .expect("absent record read"),
        CodeReadResultV1::TransactionRecord(Box::new(None))
    );
}

#[tokio::test]
async fn read_executor_keyset_walks_recovery_candidates() {
    let database = open_database().await;
    let preview = preview();
    let repository_id = preview.repository_snapshot.repository_id.clone();
    let store = test_store(database.registered.as_ref());
    // Three non-terminal (Prepared) transactions on one repository are all
    // recovery candidates; keys are ordered so the keyset walk is deterministic.
    let keys = [
        key("idempotency.candidate.a"),
        key("idempotency.candidate.b"),
        key("idempotency.candidate.c"),
    ];
    for candidate in &keys {
        store
            .begin_or_replay(begin_request(&preview, candidate.as_str()))
            .await
            .expect("start recovery candidate");
    }

    let executor = GitIndexReadExecutor::new(&store);
    let candidates_query =
        |after: Option<GitIndexIdempotencyKey>, limit: u32| CodeRecoveryCandidatesQueryV1 {
            repository_id: repository_id.clone(),
            after,
            limit,
        };

    let first = candidates_page(&executor, candidates_query(None, 2)).await;
    assert_eq!(
        first
            .records
            .iter()
            .map(|record| record.idempotency_key.clone())
            .collect::<Vec<_>>(),
        vec![keys[0].clone(), keys[1].clone()]
    );
    assert_eq!(first.next, Some(keys[1].clone()));

    let second = candidates_page(&executor, candidates_query(first.next.clone(), 2)).await;
    assert_eq!(
        second
            .records
            .iter()
            .map(|record| record.idempotency_key.clone())
            .collect::<Vec<_>>(),
        vec![keys[2].clone()]
    );
    assert_eq!(second.next, None);

    // A zero limit yields an empty page regardless of remaining candidates.
    let empty = candidates_page(&executor, candidates_query(None, 0)).await;
    assert!(empty.records.is_empty());
    assert_eq!(empty.next, None);
}

#[tokio::test]
async fn read_executor_keyset_walks_recovery_repositories() {
    let database = open_database().await;
    let first_preview = preview();
    let second_preview = preview_for(
        "repository.git-transaction.second",
        "preview.git-transaction.second",
        UtcMicros(100),
    );
    let first_repository = first_preview.repository_snapshot.repository_id.clone();
    let second_repository = second_preview.repository_snapshot.repository_id.clone();
    let store = test_store(database.registered.as_ref());
    store
        .begin_or_replay(begin_request(&first_preview, "idempotency.repo-walk.first"))
        .await
        .expect("start first repository candidate");
    store
        .begin_or_replay(begin_request(
            &second_preview,
            "idempotency.repo-walk.second",
        ))
        .await
        .expect("start second repository candidate");

    let executor = GitIndexReadExecutor::new(&store);

    let first = repositories_page(
        &executor,
        CodeRecoveryRepositoriesQueryV1 {
            after: None,
            limit: 1,
        },
    )
    .await;
    assert_eq!(first.repositories, vec![first_repository.clone()]);
    assert_eq!(first.next, Some(first_repository.clone()));

    let second = repositories_page(
        &executor,
        CodeRecoveryRepositoriesQueryV1 {
            after: first.next.clone(),
            limit: 1,
        },
    )
    .await;
    assert_eq!(second.repositories, vec![second_repository]);
    assert_eq!(second.next, None);
}
