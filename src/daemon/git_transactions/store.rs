//! Durable, daemon-owned persistence for Git index transactions.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionId,
    RepositoryId,
};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1,
    GitIndexTransactionRecordV1, GitIndexTransactionStore, GitIndexTransactionStoreError,
    GitIndexTransactionStoreResult, GitIndexTransactionTerminalWriteV1,
};

pub(crate) const GIT_INDEX_TRANSACTION_STORE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QuarantineRecordV1 {
    repository_id: RepositoryId,
    transaction_id: GitIndexTransactionId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistentStateV1 {
    schema_version: u16,
    previews: Vec<GitIndexPreviewV1>,
    records: Vec<GitIndexTransactionRecordV1>,
    quarantines: Vec<QuarantineRecordV1>,
}

impl Default for PersistentStateV1 {
    fn default() -> Self {
        Self {
            schema_version: GIT_INDEX_TRANSACTION_STORE_SCHEMA_VERSION,
            previews: Vec::new(),
            records: Vec::new(),
            quarantines: Vec::new(),
        }
    }
}

/// One process-local handle to the daemon's fsync-backed transaction journal.
///
/// The daemon is the sole writer. Every mutation is validated under one mutex
/// and replaces the complete versioned state file with an fsync + rename, so a
/// crash exposes either the old state or the complete new state.
pub(crate) struct PersistentGitIndexTransactionStore {
    path: PathBuf,
    state: Mutex<PersistentStateV1>,
}

impl PersistentGitIndexTransactionStore {
    pub(crate) fn open(path: impl Into<PathBuf>) -> GitIndexTransactionStoreResult<Self> {
        let path = path.into();
        let state = read_or_initialize(&path)?;
        let store = Self {
            path,
            state: Mutex::new(state),
        };
        store.persist_current()?;
        Ok(store)
    }

    fn persist_current(&self) -> GitIndexTransactionStoreResult<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        persist_state(&self.path, &state)
    }

    fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut PersistentStateV1) -> GitIndexTransactionStoreResult<T>,
    ) -> GitIndexTransactionStoreResult<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        let result = mutation(&mut state)?;
        persist_state(&self.path, &state)?;
        Ok(result)
    }
}

impl GitIndexTransactionStore for PersistentGitIndexTransactionStore {
    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        preview.validate()?;
        self.mutate(|state| {
            match state
                .previews
                .iter()
                .find(|stored| stored.preview_id == preview.preview_id)
            {
                Some(stored) if stored != &preview => {
                    return Err(GitIndexTransactionStoreError::PreviewConflict);
                }
                Some(_) => return Ok(()),
                None => {}
            }
            state.previews.push(preview);
            state
                .previews
                .sort_by(|left, right| left.preview_id.cmp(&right.preview_id));
            Ok(())
        })
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        let state = self
            .state
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        Ok(state
            .previews
            .iter()
            .find(|preview| &preview.preview_id == preview_id)
            .cloned())
    }

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        request.validate()?;
        self.mutate(|state| {
            let repository_id = &request.preview.repository_snapshot.repository_id;
            if state
                .quarantines
                .iter()
                .any(|entry| &entry.repository_id == repository_id)
            {
                return Err(GitIndexTransactionStoreError::RepositoryQuarantined);
            }
            if let Some(existing) = state
                .records
                .iter()
                .find(|record| record.idempotency_key == request.idempotency_key)
            {
                if existing.input_digest != request.input_digest
                    || existing.preview != request.preview
                    || existing.journal.transaction_id != request.journal.transaction_id
                {
                    return Err(GitIndexTransactionStoreError::IdempotencyConflict);
                }
                return Ok(match &existing.terminal_receipt {
                    Some(receipt) => {
                        GitIndexTransactionBeginResultV1::Replay(Box::new(receipt.clone()))
                    }
                    None => GitIndexTransactionBeginResultV1::Started(Box::new(existing.clone())),
                });
            }
            if state.previews.iter().any(|preview| {
                preview.preview_id == request.preview.preview_id && preview != &request.preview
            }) {
                return Err(GitIndexTransactionStoreError::PreviewConflict);
            }
            if !state
                .previews
                .iter()
                .any(|preview| preview.preview_id == request.preview.preview_id)
            {
                state.previews.push(request.preview.clone());
                state
                    .previews
                    .sort_by(|left, right| left.preview_id.cmp(&right.preview_id));
            }
            let record = GitIndexTransactionRecordV1 {
                idempotency_key: request.idempotency_key,
                input_digest: request.input_digest,
                preview: request.preview,
                journal: request.journal,
                terminal_receipt: None,
            };
            record.validate()?;
            state.records.push(record.clone());
            state
                .records
                .sort_by(|left, right| left.idempotency_key.cmp(&right.idempotency_key));
            Ok(GitIndexTransactionBeginResultV1::Started(Box::new(record)))
        })
    }

    fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: tracedecay_domain::GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<tracedecay_domain::GitIndexTransactionJournalV1> {
        replacement.validate()?;
        if replacement.phase.is_terminal() {
            return Err(GitIndexTransactionStoreError::JournalConflict);
        }
        self.mutate(|state| {
            let record = state
                .records
                .iter_mut()
                .find(|record| &record.idempotency_key == idempotency_key)
                .ok_or(GitIndexTransactionStoreError::JournalConflict)?;
            let current = &record.journal;
            if current.phase_epoch != expected_phase_epoch
                || replacement.phase_epoch != expected_phase_epoch.saturating_add(1)
                || !current.phase.permits_successor(replacement.phase)
                || current.transaction_id != replacement.transaction_id
                || current.preview_id != replacement.preview_id
                || current.preview_digest != replacement.preview_digest
                || current.repository_id != replacement.repository_id
                || current.worktree_id != replacement.worktree_id
                || current.operation != replacement.operation
                || current.expected_snapshot_digest != replacement.expected_snapshot_digest
                || current.started_at != replacement.started_at
            {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }
            record.journal = replacement.clone();
            Ok(replacement)
        })
    }

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<tracedecay_domain::GitIndexTransactionReceiptV1> {
        write.validate()?;
        self.mutate(|state| {
            let record = state
                .records
                .iter_mut()
                .find(|record| record.idempotency_key == write.idempotency_key)
                .ok_or(GitIndexTransactionStoreError::ReceiptConflict)?;
            if let Some(existing) = &record.terminal_receipt {
                return if existing == &write.receipt {
                    Ok(existing.clone())
                } else {
                    Err(GitIndexTransactionStoreError::ReceiptConflict)
                };
            }
            let current = &record.journal;
            if write.expected_phase_epoch != current.phase_epoch.saturating_add(1)
                || !current.phase.permits_successor(write.journal.phase)
                || current.transaction_id != write.journal.transaction_id
                || current.preview_id != write.journal.preview_id
                || current.preview_digest != write.journal.preview_digest
                || current.repository_id != write.journal.repository_id
                || current.worktree_id != write.journal.worktree_id
                || current.operation != write.journal.operation
                || current.expected_snapshot_digest != write.journal.expected_snapshot_digest
                || current.started_at != write.journal.started_at
            {
                return Err(GitIndexTransactionStoreError::JournalConflict);
            }
            record.journal = write.journal;
            record.terminal_receipt = Some(write.receipt.clone());
            record.validate()?;
            Ok(write.receipt)
        })
    }

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        let state = self
            .state
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        Ok(state
            .records
            .iter()
            .filter(|record| {
                &record.journal.repository_id == repository_id && record.journal.requires_recovery()
            })
            .cloned()
            .collect())
    }

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        let state = self
            .state
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        let repositories = state
            .records
            .iter()
            .filter(|record| record.journal.requires_recovery())
            .map(|record| record.journal.repository_id.clone())
            .collect::<BTreeSet<_>>();
        Ok(repositories.into_iter().collect())
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        repository_id.validate()?;
        transaction_id.validate()?;
        self.mutate(|state| {
            if !state.quarantines.iter().any(|entry| {
                &entry.repository_id == repository_id && &entry.transaction_id == transaction_id
            }) {
                state.quarantines.push(QuarantineRecordV1 {
                    repository_id: repository_id.clone(),
                    transaction_id: transaction_id.clone(),
                });
                state.quarantines.sort_by(|left, right| {
                    (&left.repository_id, &left.transaction_id)
                        .cmp(&(&right.repository_id, &right.transaction_id))
                });
            }
            Ok(())
        })
    }
}

fn read_or_initialize(path: &Path) -> GitIndexTransactionStoreResult<PersistentStateV1> {
    let Some(parent) = path.parent() else {
        return Err(GitIndexTransactionStoreError::Unavailable);
    };
    fs::create_dir_all(parent).map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    if !path.exists() {
        return Ok(PersistentStateV1::default());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    let state: PersistentStateV1 = serde_json::from_slice(&bytes)
        .map_err(|error| GitIndexTransactionStoreError::InvalidData(error.to_string()))?;
    if state.schema_version != GIT_INDEX_TRANSACTION_STORE_SCHEMA_VERSION {
        return Err(GitIndexTransactionStoreError::InvalidData(format!(
            "unsupported git index transaction store schema version {}",
            state.schema_version
        )));
    }
    for preview in &state.previews {
        preview.validate()?;
    }
    for record in &state.records {
        record.validate()?;
    }
    Ok(state)
}

fn persist_state(path: &Path, state: &PersistentStateV1) -> GitIndexTransactionStoreResult<()> {
    let parent = path
        .parent()
        .ok_or(GitIndexTransactionStoreError::Unavailable)?;
    fs::create_dir_all(parent).map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| GitIndexTransactionStoreError::InvalidData(error.to_string()))?;
    let temporary = temporary_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    drop(file);
    fs::rename(&temporary, path).map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        GitCoverageV1, GitHeadStateV1, GitIndexPreviewDispositionV1, GitIndexTransactionJournalV1,
        GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1, ManifestDigest, ProjectId,
        RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
        RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn oid(byte: char) -> GitOidV1 {
        GitOidV1::new(byte.to_string().repeat(40)).expect("valid object id")
    }

    fn preview() -> GitIndexPreviewV1 {
        let snapshot = RepositoryStateSnapshotV1::new(
            ProjectId::new("project.fixture").expect("project id"),
            RepositoryId::new("repository.fixture").expect("repository id"),
            Some(WorktreeId::new("worktree.fixture").expect("worktree id")),
            1,
            GitObjectFormatV1::Sha1,
            GitHeadStateV1::Attached {
                branch: "main".to_owned(),
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
            None,
            None,
            None,
            None,
            UtcMicros(1),
            GitCoverageV1::complete(),
        )
        .expect("repository snapshot");
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
        GitIndexPreviewV1::new(
            GitIndexPreviewId::new("preview.fixture").expect("preview id"),
            GitIndexTransactionOperationV1::CommitIndex,
            snapshot,
            snapshot_digest,
            Vec::new(),
            Some(oid('c')),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(1),
            UtcMicros(10),
        )
        .expect("preview")
    }

    fn begin_request(preview: &GitIndexPreviewV1) -> GitIndexTransactionBeginRequestV1 {
        GitIndexTransactionBeginRequestV1 {
            idempotency_key: GitIndexIdempotencyKey::new("idempotency.fixture")
                .expect("idempotency key"),
            input_digest: digest('e'),
            preview: preview.clone(),
            journal: GitIndexTransactionJournalV1::prepared(
                GitIndexTransactionId::new("transaction.fixture").expect("transaction id"),
                preview,
                UtcMicros(1),
            )
            .expect("prepared journal"),
        }
    }

    #[test]
    fn store_schema_is_registered_durably_and_reopens() {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("git-index-transactions.json");

        let store = PersistentGitIndexTransactionStore::open(&path).expect("create store");
        drop(store);
        let reopened = PersistentGitIndexTransactionStore::open(&path).expect("reopen store");
        drop(reopened);

        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("read state")).expect("valid JSON");
        assert_eq!(
            state["schema_version"],
            GIT_INDEX_TRANSACTION_STORE_SCHEMA_VERSION
        );
    }

    #[test]
    fn store_refuses_unknown_future_schema() {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("git-index-transactions.json");
        std::fs::write(
            &path,
            br#"{"schema_version":2,"previews":[],"records":[],"quarantines":[]}"#,
        )
        .expect("write future schema");

        assert!(matches!(
            PersistentGitIndexTransactionStore::open(path),
            Err(GitIndexTransactionStoreError::InvalidData(_))
        ));
    }

    #[test]
    fn terminal_receipt_and_phase_commit_atomically_and_replay_after_reopen() {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("git-index-transactions.json");
        let preview = preview();
        let request = begin_request(&preview);
        let store = PersistentGitIndexTransactionStore::open(&path).expect("create store");
        assert!(matches!(
            store.begin_or_replay(request.clone()),
            Ok(GitIndexTransactionBeginResultV1::Started(_))
        ));

        let receipt = tracedecay_domain::GitIndexTransactionReceiptV1::new(
            tracedecay_domain::GitIndexReceiptId::new("receipt.fixture").expect("receipt id"),
            request.journal.transaction_id.clone(),
            &preview,
            preview.repository_snapshot_digest.clone(),
            preview.repository_snapshot.index.tree_id.clone(),
            preview.repository_snapshot.head.commit().cloned(),
            None,
            tracedecay_domain::GitIndexReceiptOutcomeV1::AbortedNoChange,
            UtcMicros(2),
        )
        .expect("terminal receipt");
        let mut terminal_journal = request.journal.clone();
        terminal_journal
            .advance(
                tracedecay_domain::GitIndexJournalPhaseV1::AbortedNoChange,
                UtcMicros(2),
            )
            .expect("terminal transition");
        store
            .write_terminal(GitIndexTransactionTerminalWriteV1 {
                idempotency_key: request.idempotency_key.clone(),
                expected_phase_epoch: terminal_journal.phase_epoch,
                journal: terminal_journal,
                receipt: receipt.clone(),
            })
            .expect("atomic terminal write");
        drop(store);

        let reopened = PersistentGitIndexTransactionStore::open(path).expect("reopen store");
        assert!(matches!(
            reopened.begin_or_replay(request),
            Ok(GitIndexTransactionBeginResultV1::Replay(stored)) if *stored == receipt
        ));
        assert!(
            reopened
                .recovery_repositories()
                .expect("recovery repositories")
                .is_empty()
        );
    }

    #[test]
    fn journal_cas_cannot_persist_a_terminal_phase_without_its_receipt() {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("git-index-transactions.json");
        let preview = preview();
        let request = begin_request(&preview);
        let store = PersistentGitIndexTransactionStore::open(path).expect("create store");
        store
            .begin_or_replay(request.clone())
            .expect("begin transaction");
        let mut terminal = request.journal;
        terminal
            .advance(
                tracedecay_domain::GitIndexJournalPhaseV1::AbortedNoChange,
                UtcMicros(2),
            )
            .expect("terminal transition");

        assert_eq!(
            store.compare_and_swap_journal(&request.idempotency_key, 1, terminal),
            Err(GitIndexTransactionStoreError::JournalConflict)
        );
    }
}
