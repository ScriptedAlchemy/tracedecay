use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};
use crate::storage::{BRANCH_META_FILENAME, PrivateStoreIo};

use super::branch_db_family_paths;

const JOURNAL_FILENAME: &str = ".branch-delete-transaction.json";
const JOURNAL_VERSION: u32 = 1;
const QUARANTINE_MARKER: &str = ".branch-delete-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Prepared,
    CommittedOrphans,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FamilySnapshot {
    db: bool,
    wal: bool,
    shm: bool,
}

impl FamilySnapshot {
    fn values(&self) -> [bool; 3] {
        [self.db, self.wal, self.shm]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeletionEntry {
    db_file: String,
    present: FamilySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeletionJournal {
    version: u32,
    transaction_id: String,
    state: JournalState,
    metadata_before: Option<String>,
    metadata_after: Option<String>,
    entries: Vec<DeletionEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransactionPhase {
    AfterJournalBeforeDeletingPublication,
    AfterMove(usize),
    BeforeRefRevalidation,
    BeforeMetadataPublication,
    AfterCommitBeforeCleanup,
    AfterPhysicalRollbackBeforeDeletingRollback,
    AfterDeletingRollbackBeforeJournalClear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryDisposition {
    PreCommitRollback,
    CommittedCleanup,
}

pub(super) struct PendingRecovery {
    journal: DeletionJournal,
    disposition: RecoveryDisposition,
}

impl PendingRecovery {
    pub(super) fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    pub(super) fn database_paths(&self, tracedecay_dir: &Path) -> Vec<PathBuf> {
        self.journal
            .entries
            .iter()
            .map(|entry| tracedecay_dir.join(&entry.db_file))
            .collect()
    }

    pub(super) fn recover<V, T>(
        self,
        tracedecay_dir: &Path,
        validate_quarantined_stores: V,
        transition_tombstones: T,
    ) -> Result<()>
    where
        V: FnOnce(&[PathBuf]) -> Result<()>,
        T: FnOnce(RecoveryDisposition) -> Result<()>,
    {
        let validation_paths = validation_database_paths(tracedecay_dir, &self.journal)?;
        validate_quarantined_stores(&validation_paths)?;
        match self.disposition {
            RecoveryDisposition::PreCommitRollback => {
                rollback_files(tracedecay_dir, &self.journal)?;
                transition_tombstones(self.disposition)?;
            }
            RecoveryDisposition::CommittedCleanup => {
                sync_committed_recovery_state(tracedecay_dir, &self.journal)?;
                transition_tombstones(self.disposition)?;
                cleanup_files(tracedecay_dir, &self.journal)?;
            }
        }
        clear_journal(tracedecay_dir)
    }
}

pub(super) fn prepare_pending_recovery(tracedecay_dir: &Path) -> Result<Option<PendingRecovery>> {
    let Some(journal) = load_journal(tracedecay_dir)? else {
        return Ok(None);
    };
    validate_journal(tracedecay_dir, &journal)?;
    let current = read_current_metadata(tracedecay_dir)?;
    let disposition = match journal.state {
        JournalState::Prepared if current == journal.metadata_before => {
            RecoveryDisposition::PreCommitRollback
        }
        JournalState::Prepared
            if journal.metadata_before != journal.metadata_after
                && current == journal.metadata_after =>
        {
            RecoveryDisposition::CommittedCleanup
        }
        JournalState::CommittedOrphans
            if journal.metadata_before == journal.metadata_after
                && current == journal.metadata_after =>
        {
            RecoveryDisposition::CommittedCleanup
        }
        _ => {
            return Err(config_error(format!(
                "cannot recover branch deletion transaction '{}': branch metadata matches neither the exact pre-commit nor post-commit state",
                journal.transaction_id
            )));
        }
    };
    Ok(Some(PendingRecovery {
        journal,
        disposition,
    }))
}

pub(super) fn ensure_no_pending_recovery(tracedecay_dir: &Path) -> Result<()> {
    if load_journal(tracedecay_dir)?.is_some() {
        return Err(config_error(format!(
            "branch deletion recovery is required at '{}' before branch metadata may change",
            tracedecay_dir.display()
        )));
    }
    Ok(())
}

pub(super) struct CommitRequest<'a> {
    pub(super) tracedecay_dir: &'a Path,
    pub(super) supplied_transaction_id: Option<&'a str>,
    pub(super) database_paths: &'a [PathBuf],
    pub(super) metadata_before: Option<String>,
    pub(super) metadata_after: Option<String>,
}

pub(super) fn commit_with_hook<P, V, R, H>(
    request: CommitRequest<'_>,
    publish_deleting: P,
    validate_precommit: V,
    rollback_deleting: R,
    mut hook: H,
) -> Result<()>
where
    P: FnOnce() -> Result<()>,
    V: FnOnce(&[PathBuf]) -> Result<()>,
    R: FnOnce() -> Result<()>,
    H: FnMut(TransactionPhase) -> Result<()>,
{
    let CommitRequest {
        tracedecay_dir,
        supplied_transaction_id,
        database_paths,
        metadata_before,
        metadata_after,
    } = request;
    if database_paths.is_empty() {
        validate_precommit(&[])?;
        let current = read_current_metadata(tracedecay_dir)?;
        if current != metadata_before {
            return Err(config_error(
                "branch metadata changed after deletion selection; transaction refused",
            ));
        }
        if metadata_before != metadata_after {
            hook(TransactionPhase::BeforeMetadataPublication)?;
            let after = metadata_after.as_deref().ok_or_else(|| {
                config_error("tracked branch deletion cannot remove branch metadata entirely")
            })?;
            publish_metadata(tracedecay_dir, after)?;
        }
        return Ok(());
    }
    if journal_path(tracedecay_dir).exists() {
        return Err(config_error(format!(
            "branch deletion transaction journal '{}' already exists after recovery",
            journal_path(tracedecay_dir).display()
        )));
    }

    let transaction_id = supplied_transaction_id.map_or_else(transaction_id, str::to_string);
    let mut entries = database_paths
        .iter()
        .map(|path| snapshot_entry(tracedecay_dir, path))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by(|left, right| left.db_file.cmp(&right.db_file));
    if entries
        .windows(2)
        .any(|pair| pair[0].db_file == pair[1].db_file)
    {
        return Err(config_error(
            "branch deletion transaction contains duplicate database paths",
        ));
    }
    let mut journal = DeletionJournal {
        version: JOURNAL_VERSION,
        transaction_id,
        state: JournalState::Prepared,
        metadata_before,
        metadata_after,
        entries,
    };
    validate_journal(tracedecay_dir, &journal)?;
    persist_journal(tracedecay_dir, &journal)?;

    let operation = (|| {
        hook(TransactionPhase::AfterJournalBeforeDeletingPublication)?;
        publish_deleting()?;

        let mut moved = 0_usize;
        for entry in &journal.entries {
            for (source, quarantine, expected_present) in
                family_states(tracedecay_dir, &journal, entry)?
            {
                if expected_present {
                    require_regular_file(&source, "branch store family member")?;
                    require_missing(&quarantine, "branch deletion quarantine")?;
                    move_file_durably(&source, &quarantine, "branch store quarantine")?;
                    moved += 1;
                    hook(TransactionPhase::AfterMove(moved))?;
                } else {
                    require_missing(&source, "branch store family member")?;
                    require_missing(&quarantine, "branch deletion quarantine")?;
                }
            }
        }

        let validation_paths = validation_database_paths(tracedecay_dir, &journal)?;
        hook(TransactionPhase::BeforeRefRevalidation)?;
        validate_precommit(&validation_paths)?;
        require_original_family_missing(tracedecay_dir, &journal)?;
        let current = read_current_metadata(tracedecay_dir)?;
        if current != journal.metadata_before {
            return Err(config_error(
                "branch metadata changed after deletion selection; transaction refused",
            ));
        }

        if journal.metadata_before == journal.metadata_after {
            let mut committed = journal.clone();
            committed.state = JournalState::CommittedOrphans;
            persist_journal(tracedecay_dir, &committed)?;
            journal = committed;
        } else {
            hook(TransactionPhase::BeforeMetadataPublication)?;
            let after = journal.metadata_after.as_deref().ok_or_else(|| {
                config_error("tracked branch deletion cannot remove branch metadata entirely")
            })?;
            publish_metadata(tracedecay_dir, after)?;
        }
        hook(TransactionPhase::AfterCommitBeforeCleanup)?;
        cleanup_committed(tracedecay_dir, &journal)
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(primary) => {
            let committed = if journal.state == JournalState::CommittedOrphans {
                true
            } else if journal.metadata_before != journal.metadata_after {
                match read_current_metadata(tracedecay_dir) {
                    Ok(current) => current == journal.metadata_after,
                    Err(_) => return Err(primary),
                }
            } else {
                false
            };
            if committed {
                return Err(primary);
            }
            if let Err(rollback_error) = rollback_files(tracedecay_dir, &journal) {
                return Err(config_error(format!(
                    "{primary}; physical rollback also failed and recovery evidence was retained: {rollback_error}"
                )));
            }
            if let Err(failpoint) =
                hook(TransactionPhase::AfterPhysicalRollbackBeforeDeletingRollback)
            {
                return Err(config_error(format!(
                    "{primary}; rollback stopped after physical restoration and recovery evidence was retained: {failpoint}"
                )));
            }
            if let Err(rollback_error) = rollback_deleting() {
                return Err(config_error(format!(
                    "{primary}; deletion-fence rollback also failed and recovery evidence was retained: {rollback_error}"
                )));
            }
            if let Err(failpoint) = hook(TransactionPhase::AfterDeletingRollbackBeforeJournalClear)
            {
                return Err(config_error(format!(
                    "{primary}; rollback stopped after deletion-fence restoration and recovery evidence was retained: {failpoint}"
                )));
            }
            match clear_journal(tracedecay_dir) {
                Ok(()) => Err(primary),
                Err(clear_error) => Err(config_error(format!(
                    "{primary}; rollback succeeded but journal cleanup failed and recovery evidence was retained: {clear_error}"
                ))),
            }
        }
    }
}

fn snapshot_entry(tracedecay_dir: &Path, db_path: &Path) -> Result<DeletionEntry> {
    let db_file = validate_database_path(tracedecay_dir, db_path)?;
    let family = branch_db_family_paths(db_path);
    let mut present = [false; 3];
    for (index, path) in family.iter().enumerate() {
        present[index] = inspect_regular_file(path, "branch store family member")?;
    }
    Ok(DeletionEntry {
        db_file,
        present: FamilySnapshot {
            db: present[0],
            wal: present[1],
            shm: present[2],
        },
    })
}

fn validate_journal(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    if journal.version != JOURNAL_VERSION {
        return Err(config_error(format!(
            "unsupported branch deletion journal version {}",
            journal.version
        )));
    }
    if journal.transaction_id.is_empty()
        || journal.transaction_id.len() > 512
        || journal.transaction_id.chars().any(char::is_control)
        || journal.transaction_id.contains('/')
        || journal.transaction_id.contains('\\')
    {
        return Err(config_error("invalid branch deletion transaction id"));
    }
    if journal.entries.is_empty() {
        return Err(config_error(
            "branch deletion journal contains no database entries",
        ));
    }
    if journal.state == JournalState::CommittedOrphans
        && journal.metadata_before != journal.metadata_after
    {
        return Err(config_error(
            "only metadata-neutral orphan deletion may use committed_orphans journal state",
        ));
    }
    validate_store_root(tracedecay_dir)?;
    let mut previous = None;
    for entry in &journal.entries {
        let db_path = tracedecay_dir.join(&entry.db_file);
        let normalized = validate_database_path(tracedecay_dir, &db_path)?;
        if normalized != entry.db_file {
            return Err(config_error(format!(
                "branch deletion journal path '{}' is not normalized",
                entry.db_file
            )));
        }
        if previous
            .as_deref()
            .is_some_and(|value| value >= entry.db_file.as_str())
        {
            return Err(config_error(
                "branch deletion journal database entries are duplicated or unsorted",
            ));
        }
        previous = Some(entry.db_file.clone());
        for (source, quarantine, _) in family_states(tracedecay_dir, journal, entry)? {
            if source.parent() != quarantine.parent() {
                return Err(config_error(
                    "branch deletion quarantine is not a sibling path",
                ));
            }
        }
    }
    Ok(())
}

fn validate_store_root(tracedecay_dir: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(tracedecay_dir).map_err(|error| {
        config_error(format!(
            "cannot inspect branch store root '{}': {error}",
            tracedecay_dir.display()
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(config_error(format!(
            "branch store root '{}' is not an unambiguous directory",
            tracedecay_dir.display()
        )));
    }
    let branches = tracedecay_dir.join("branches");
    let metadata = std::fs::symlink_metadata(&branches).map_err(|error| {
        config_error(format!(
            "cannot inspect branch database directory '{}': {error}",
            branches.display()
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(config_error(format!(
            "branch database directory '{}' is not an unambiguous directory",
            branches.display()
        )));
    }
    Ok(())
}

fn validate_database_path(tracedecay_dir: &Path, db_path: &Path) -> Result<String> {
    validate_store_root(tracedecay_dir)?;
    let relative = db_path.strip_prefix(tracedecay_dir).map_err(|_| {
        config_error(format!(
            "branch database path '{}' escapes store root '{}'",
            db_path.display(),
            tracedecay_dir.display()
        ))
    })?;
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !relative.starts_with("branches")
        || relative.components().count() < 2
        || !relative
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("db"))
    {
        return Err(config_error(format!(
            "branch database path '{}' is not a normalized branches/*.db store path",
            db_path.display()
        )));
    }
    let parent = db_path.parent().ok_or_else(|| {
        config_error(format!(
            "branch database path '{}' has no parent",
            db_path.display()
        ))
    })?;
    let canonical_root = tracedecay_dir.canonicalize().map_err(|error| {
        config_error(format!(
            "cannot resolve branch store root '{}': {error}",
            tracedecay_dir.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        config_error(format!(
            "cannot resolve branch database parent '{}': {error}",
            parent.display()
        ))
    })?;
    if !canonical_parent.starts_with(canonical_root.join("branches")) {
        return Err(config_error(format!(
            "branch database parent '{}' escapes the branch store directory",
            parent.display()
        )));
    }
    relative
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| config_error("branch database path is not valid UTF-8"))
}

fn family_states(
    tracedecay_dir: &Path,
    journal: &DeletionJournal,
    entry: &DeletionEntry,
) -> Result<Vec<(PathBuf, PathBuf, bool)>> {
    let db_path = tracedecay_dir.join(&entry.db_file);
    let family = branch_db_family_paths(&db_path);
    let quarantine = quarantine_family_paths(&db_path, &journal.transaction_id)?;
    let present = entry.present.values();
    Ok(family
        .into_iter()
        .zip(quarantine)
        .zip(present)
        .map(|((source, quarantine), expected)| (source, quarantine, expected))
        .collect())
}

fn validation_database_paths(
    tracedecay_dir: &Path,
    journal: &DeletionJournal,
) -> Result<Vec<PathBuf>> {
    let mut paths = journal
        .entries
        .iter()
        .map(|entry| tracedecay_dir.join(&entry.db_file))
        .collect::<Vec<_>>();
    paths.extend(quarantine_database_paths(tracedecay_dir, journal)?);
    Ok(paths)
}

fn require_original_family_missing(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    for entry in &journal.entries {
        for (source, _, _) in family_states(tracedecay_dir, journal, entry)? {
            require_missing(&source, "original branch store family member")?;
        }
    }
    Ok(())
}

fn quarantine_database_paths(
    tracedecay_dir: &Path,
    journal: &DeletionJournal,
) -> Result<Vec<PathBuf>> {
    journal
        .entries
        .iter()
        .map(|entry| {
            family_states(tracedecay_dir, journal, entry)?
                .into_iter()
                .next()
                .map(|(_, quarantine, _)| quarantine)
                .ok_or_else(|| config_error("branch deletion family is empty"))
        })
        .collect()
}

fn quarantine_family_paths(database: &Path, transaction_id: &str) -> Result<[PathBuf; 3]> {
    let parent = database.parent().ok_or_else(|| {
        config_error(format!(
            "branch store path '{}' has no parent",
            database.display()
        ))
    })?;
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            config_error(format!(
                "branch store path '{}' has a non-UTF-8 filename",
                database.display()
            ))
        })?;
    let transaction_component = transaction_file_component(transaction_id);
    let database = parent.join(format!(
        ".{name}{QUARANTINE_MARKER}{transaction_component}.quarantine"
    ));
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = database.as_os_str().to_os_string();
    shm.push("-shm");
    Ok([database, PathBuf::from(wal), PathBuf::from(shm)])
}

fn move_file_durably(source: &Path, destination: &Path, record_name: &str) -> Result<()> {
    crate::db::DatabaseAuthority::replace_file_atomically(source, destination, record_name)
        .map_err(|error| {
            config_error(format!(
                "failed to move '{}' to '{}' for {record_name}: {error}",
                source.display(),
                destination.display()
            ))
        })?;
    let parent = destination.parent().ok_or_else(|| {
        config_error(format!(
            "branch store path '{}' has no parent",
            destination.display()
        ))
    })?;
    sync_directory(parent)
}

fn rollback_files(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    let states = journal
        .entries
        .iter()
        .map(|entry| family_states(tracedecay_dir, journal, entry))
        .collect::<Result<Vec<_>>>()?;
    for family in &states {
        for (source, quarantine, expected_present) in family {
            let source_exists = inspect_regular_file(source, "branch store family member")?;
            let quarantine_exists = inspect_regular_file(quarantine, "branch deletion quarantine")?;
            match (*expected_present, source_exists, quarantine_exists) {
                (true, true, false) | (true, false, true) | (false, false, false) => {}
                _ => {
                    return Err(config_error(format!(
                        "cannot roll back branch deletion transaction '{}': ambiguous source/quarantine state for '{}'",
                        journal.transaction_id,
                        source.display()
                    )));
                }
            }
        }
    }
    for family in states.into_iter().rev() {
        for (source, quarantine, expected_present) in family.into_iter().rev() {
            if expected_present && quarantine.exists() {
                move_file_durably(&quarantine, &source, "branch store quarantine rollback")?;
            }
        }
    }
    Ok(())
}

fn cleanup_committed(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    cleanup_files(tracedecay_dir, journal)?;
    clear_journal(tracedecay_dir)
}

fn sync_committed_recovery_state(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    for entry in &journal.entries {
        let database = tracedecay_dir.join(&entry.db_file);
        let parent = database.parent().ok_or_else(|| {
            config_error(format!(
                "branch database path '{}' has no parent",
                database.display()
            ))
        })?;
        sync_directory(parent)?;
    }
    if journal.metadata_before != journal.metadata_after {
        sync_file(&tracedecay_dir.join(BRANCH_META_FILENAME))?;
        sync_directory(tracedecay_dir)?;
    }
    Ok(())
}

fn cleanup_files(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    let states = journal
        .entries
        .iter()
        .map(|entry| family_states(tracedecay_dir, journal, entry))
        .collect::<Result<Vec<_>>>()?;
    for family in &states {
        for (source, quarantine, expected_present) in family {
            let source_exists = inspect_regular_file(source, "branch store family member")?;
            let quarantine_exists = inspect_regular_file(quarantine, "branch deletion quarantine")?;
            match (*expected_present, source_exists, quarantine_exists) {
                (true, false, true | false) | (false, false, false) => {}
                _ => {
                    return Err(config_error(format!(
                        "cannot clean committed branch deletion transaction '{}': ambiguous source/quarantine state for '{}'",
                        journal.transaction_id,
                        source.display()
                    )));
                }
            }
        }
    }
    for family in states {
        for (source, quarantine, expected_present) in family {
            if expected_present && quarantine.exists() {
                std::fs::remove_file(&quarantine).map_err(|error| {
                    config_error(format!(
                        "failed to delete quarantined branch store file '{}': {error}",
                        quarantine.display()
                    ))
                })?;
                sync_directory(source.parent().ok_or_else(|| {
                    config_error(format!(
                        "branch store path '{}' has no parent",
                        source.display()
                    ))
                })?)?;
            }
        }
    }
    Ok(())
}

fn inspect_regular_file(path: &Path, description: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            require_single_link(path, &metadata)?;
            Ok(true)
        }
        Ok(_) => Err(config_error(format!(
            "{description} '{}' is not an unambiguous regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(config_error(format!(
            "cannot inspect {description} '{}': {error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
fn require_single_link(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.nlink() == 1 {
        Ok(())
    } else {
        Err(config_error(format!(
            "branch store family member '{}' has {} hard links; deletion identity is ambiguous",
            path.display(),
            metadata.nlink()
        )))
    }
}

#[cfg(windows)]
fn require_single_link(path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    let links = crate::db::windows_hard_link_count(path)?;
    if links == 1 {
        Ok(())
    } else {
        Err(config_error(format!(
            "branch store family member '{}' has {links} hard links; deletion identity is ambiguous",
            path.display()
        )))
    }
}

#[cfg(not(any(unix, windows)))]
fn require_single_link(path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    Err(config_error(format!(
        "cannot prove branch store family member '{}' has a single hard link on this platform",
        path.display()
    )))
}

fn require_regular_file(path: &Path, description: &str) -> Result<()> {
    if inspect_regular_file(path, description)? {
        Ok(())
    } else {
        Err(config_error(format!(
            "expected {description} '{}' is missing",
            path.display()
        )))
    }
}

fn require_missing(path: &Path, description: &str) -> Result<()> {
    if inspect_regular_file(path, description)? {
        Err(config_error(format!(
            "unexpected {description} '{}' already exists",
            path.display()
        )))
    } else {
        Ok(())
    }
}

fn persist_journal(tracedecay_dir: &Path, journal: &DeletionJournal) -> Result<()> {
    validate_journal(tracedecay_dir, journal)?;
    let path = journal_path(tracedecay_dir);
    let transaction_component = transaction_file_component(&journal.transaction_id);
    let temp = tracedecay_dir.join(format!("{JOURNAL_FILENAME}.tmp-{transaction_component}"));
    let bytes = serde_json::to_vec_pretty(journal)?;
    if let Err(error) = PrivateStoreIo::write_file_atomically(&path, &temp, &bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    sync_file(&path)?;
    sync_directory(tracedecay_dir)
}

fn load_journal(tracedecay_dir: &Path) -> Result<Option<DeletionJournal>> {
    let path = journal_path(tracedecay_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(config_error(format!(
                "branch deletion journal '{}' is not an unambiguous regular file",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "cannot inspect branch deletion journal '{}': {error}",
                path.display()
            )));
        }
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        config_error(format!(
            "cannot read branch deletion journal '{}': {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        config_error(format!(
            "cannot parse branch deletion journal '{}': {error}",
            path.display()
        ))
    })
}

fn publish_metadata(tracedecay_dir: &Path, serialized: &str) -> Result<()> {
    let path = tracedecay_dir.join(BRANCH_META_FILENAME);
    crate::branch_meta::save_branch_meta_serialized(tracedecay_dir, serialized).map_err(
        |error| {
            config_error(format!(
                "cannot publish branch metadata '{}': {error}",
                path.display()
            ))
        },
    )?;
    sync_file(&path)?;
    sync_directory(tracedecay_dir)
}

fn read_current_metadata(tracedecay_dir: &Path) -> Result<Option<String>> {
    let path = tracedecay_dir.join(BRANCH_META_FILENAME);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(config_error(format!(
                "branch metadata '{}' is not an unambiguous regular file",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "cannot inspect branch metadata '{}': {error}",
                path.display()
            )));
        }
    }
    std::fs::read_to_string(&path).map(Some).map_err(|error| {
        config_error(format!(
            "cannot read branch metadata '{}': {error}",
            path.display()
        ))
    })
}

fn clear_journal(tracedecay_dir: &Path) -> Result<()> {
    let path = journal_path(tracedecay_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => sync_directory(tracedecay_dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(config_error(format!(
            "failed to clear branch deletion journal '{}': {error}",
            path.display()
        ))),
    }
}

#[cfg(not(windows))]
fn sync_file(path: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| config_error(format!("failed to sync '{}': {error}", path.display())))
}

#[cfg(windows)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "Windows publication already flushes through MoveFileExW; callers share one contract"
)]
fn sync_file(_path: &Path) -> Result<()> {
    // PrivateStoreIo publishes these records with MoveFileExW's
    // MOVEFILE_WRITE_THROUGH. Reopening the replaced path for a second flush
    // is not portable on Windows and can fail with ERROR_ACCESS_DENIED.
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    crate::application::host_admission::sync_directory(
        path,
        crate::application::host_admission::DirectorySyncPolicy::Strict,
    )
    .map_err(|error| {
        config_error(format!(
            "failed to sync directory '{}': {error}",
            path.display()
        ))
    })
}

fn journal_path(tracedecay_dir: &Path) -> PathBuf {
    tracedecay_dir.join(JOURNAL_FILENAME)
}

fn transaction_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn transaction_file_component(transaction_id: &str) -> String {
    transaction_id
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                (byte as char).to_string()
            } else {
                format!("_{byte:02x}")
            }
        })
        .collect()
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
