//! tracedecay-storage: target type sketch for docs/plans/storage/README.md.
//!
//! Not compiled. Bodies are `todo!` and `// TODO` pseudocode; the README's cutover plan fills
//! them in step by step, and the README's "Implementation reconciliation" records every
//! deviation.
//!
//! One store set per REPOSITORY (shared by every clone and worktree), keyed by the existing
//! `ProjectId` and partitioned by storage class into SQLite files, beside a profile catalog and a
//! telemetry file. Every file runs on one bounded executor owned by the daemon. Every public
//! method is async and admits or refuses immediately.
//!
//! Public surface: Storage, StorageConfig, Project, Checkout, Configuration, WorkJournal,
//! JournalTx, MemoryStore, SessionStore, CodeGraph, StorageError, StorageUsage, MaintenanceSlice,
//! MaintenanceReport, ResetScope, plus the domain value types those methods take and return
//! (owned by tracedecay-contracts and tracedecay-domain, not by this crate). Nothing public names
//! rusqlite, SQL, WAL, threads, files, leases, probes or wire enums.

#![allow(dead_code, unused_variables)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// Domain types owned elsewhere. Their definitions are unchanged by this redesign.
use tracedecay_contracts::code::{GenerationDigest, SealedGenerationReader};
use tracedecay_contracts::configuration::{ConfigKey, ConfigRevision, ConfigValue};
use tracedecay_contracts::memory::{
    Fact, FactCommit, FactFeedback, FactHistory, FactId, FactLineage, FactQuery, MemorySearch,
    MemorySearchHit, MemoryStatus,
};
use tracedecay_contracts::sessions::{
    CaptureBatch, CaptureOutcome, DeadLetter, SessionPage, SessionQuery, SessionSearchHit,
    SourceCursor, SourceId, SummaryCandidate, SummaryRecord,
};
use tracedecay_contracts::work::{JournalAggregate, JournalFilter, OperationId};
use tracedecay_domain::{ProjectId, WorktreeId};

// =================================================================================================
// Public surface
// =================================================================================================

/// The daemon's single owner of every byte under the profile root.
///
/// Invariant: exactly one `Storage` per profile per host (daemon-authority lock, unchanged). No
/// other process opens a store file. Offline CLI commands only unlink files under the lifecycle
/// lease. This invariant is what makes the checkpoint reader fence total (see `executor`).
pub struct Storage {
    inner: Arc<StorageInner>,
}

struct StorageInner {
    profile: PathBuf,
    config: StorageConfig,
    executor: executor::Executor,
    catalog: executor::LaneHandle,
    telemetry: executor::LaneHandle,
    projects: std::sync::Mutex<std::collections::HashMap<ProjectId, Arc<ProjectInner>>>,
}

/// Operator-tunable budgets. Nothing here is a per-call option.
#[derive(Clone, Debug)]
pub struct StorageConfig {
    /// Storage executor threads for the whole daemon. Clamped to 2..=16. Default 6.
    pub executor_threads: usize,
    /// Most files with open connections at once; idle lanes beyond this close LRU. Default 64.
    pub open_lane_cap: usize,
    /// Profile budget, memory and durable journal rows excluded. Default 64 GiB.
    pub profile_budget_bytes: u64,
    /// Per-project evidence budget. Default 8 GiB.
    pub evidence_budget_bytes: u64,
    /// Per-project code budget (sealed generations plus code-index scopes). Default 16 GiB.
    pub code_budget_bytes: u64,
    /// Evidence age window, by capture time. Default 180 days.
    pub evidence_window: Duration,
    /// A project with no live checkout for this long keeps only its durable classes. Default 30 d.
    pub project_idle_collection: Duration,
}

impl Storage {
    /// Starts the executor. Re-registers any `projects/<id>/` the catalog does not name from its
    /// `project.json` (never deletes it), and deletes every `projects/*/scratch/`.
    pub async fn open(profile: PathBuf, config: StorageConfig) -> Result<Storage, StorageError> {
        // TODO: take daemon authority (unchanged crate tracedecay-daemon-identity).
        // TODO: pin profile-identity.json for the unlink probe (lifecycle::probe).
        // TODO: executor::Executor::start(config.executor_threads.clamp(2, 16), open_lane_cap).
        todo!("not implemented")
    }

    /// Temp-dir profile for tests. Same code path as `open`; no fakes, no traits to mock.
    pub async fn open_ephemeral() -> Result<(Storage, tempfile::TempDir), StorageError> {
        todo!("not implemented")
    }

    /// Resolves the checkout's project (section 2 of the README) and returns its store set.
    /// Opens no store file. Writes the checkout row on first sight and refreshes it at most
    /// hourly, from an in-memory cache otherwise.
    pub async fn project(&self, checkout: &Checkout) -> Result<Project, StorageError> {
        // TODO: identity::resolve(checkout, &catalog) as a catalog job (gix walk inside).
        todo!("not implemented")
    }

    /// Sessions and memory with no project (`projects/_profile`). `work()` and `code()` on it
    /// return `Refused(NoProject)`.
    pub fn profile(&self) -> Project {
        todo!("not implemented")
    }

    /// Profile configuration revisions (catalog.db, durable class).
    pub fn configuration(&self) -> Configuration {
        todo!("not implemented")
    }

    /// Sizes of every file and directory class, budgets, growth rate from persisted history,
    /// findings, dead-letter and loss-receipt counts. Reads telemetry.storage_usage; never scans
    /// a large table on the request path.
    pub async fn usage(&self) -> StorageUsage {
        todo!("not implemented")
    }

    /// One time-sliced maintenance pass. The daemon calls it every 10 min and again at once while
    /// `report.has_more`. Owns idle checkpoints, the unlink probe, usage sampling, class levers,
    /// worktree and project collection, and the response-handle sweep.
    pub async fn maintain(&self, slice: MaintenanceSlice) -> MaintenanceReport {
        // TODO: lifecycle::probe for every pinned lane; ProfileLost aborts the slice.
        // TODO: sample usage per lane and directory; append telemetry.storage_usage.
        // TODO: retention::plan(..) -> Vec<Lever>; run each as a background job until the slice
        //       budget is spent; has_more if any remain.
        todo!("not implemented")
    }

    /// `tracedecay wipe`: closes the project's lanes, then removes what the scope names.
    pub async fn reset(&self, project: ProjectId, scope: ResetScope) -> Result<(), StorageError> {
        todo!("not implemented")
    }

    /// `tracedecay projects forget`: closes lanes, unlinks projects/<id>/ including memory.db,
    /// deletes the catalog rows and aliases. The only operation that deletes durable data.
    /// Idempotent: forgetting an unknown id is Ok.
    pub async fn forget(&self, project: ProjectId) -> Result<(), StorageError> {
        todo!("not implemented")
    }

    /// Drains admitted writes (settlement outlives requests), TRUNCATEs every open WAL, joins
    /// the executor threads.
    pub async fn shutdown(self) {
        todo!("not implemented")
    }
}

/// A project's store set. Cheap to clone. Handles open their file lazily on first job.
#[derive(Clone)]
pub struct Project {
    inner: Arc<ProjectInner>,
}

struct ProjectInner {
    id: ProjectId,
    root: PathBuf, // P/projects/<project_id>
    journal: executor::LaneHandle,
    memory: executor::LaneHandle,
    evidence: executor::LaneHandle,
    derived: executor::LaneHandle,
}

impl Project {
    pub fn id(&self) -> ProjectId {
        self.inner.id.clone()
    }
    pub fn memory(&self) -> MemoryStore {
        todo!("not implemented")
    }
    pub fn sessions(&self) -> SessionStore {
        todo!("not implemented")
    }
    pub fn work(&self) -> WorkJournal {
        todo!("not implemented")
    }
    /// Code graph of one worktree. Refused if the worktree does not resolve to this project.
    pub async fn code(&self, worktree: &Checkout) -> Result<CodeGraph, StorageError> {
        todo!("not implemented")
    }
}

// -------------------------------------------------------------------------------------------------
// Identity
// -------------------------------------------------------------------------------------------------

/// A directory the caller works in, validated at the boundary (exists, local FS, not $HOME or /).
#[derive(Clone, Debug)]
pub struct Checkout {
    root: PathBuf,
    kind: CheckoutKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CheckoutKind {
    FullClone,
    LinkedWorktree,
    /// An initialized directory below a worktree root (a monorepo subproject).
    Subproject {
        relative: PathBuf,
    },
    NonGit,
}

impl Checkout {
    /// Walks up to the nearest initialized directory or the worktree root, as today's
    /// `discover_project_root` does. Validation lives here; downstream trusts a `Checkout`.
    pub fn discover(cwd: &Path) -> Result<Checkout, StorageError> {
        todo!("not implemented")
    }
}

mod identity {
    use super::*;

    /// A fact about a checkout that can name its repository. Every anchor of a project is an
    /// alias row in catalog.db; anchors only ever accumulate, so a project never re-keys.
    ///
    /// Host evidence (2026-10-16): of 14 full clones of one repository, 7 have a local-path
    /// origin, 5 https, 2 ssh. Only the root commit converges all 14.
    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub(crate) enum IdentityAnchor {
        /// A parentless commit reachable from HEAD (full parent walk; every root is recorded).
        RootCommit([u8; 20]),
        /// Root commit plus the subproject's path below the worktree root.
        Subproject { root: [u8; 20], relative: PathBuf },
        /// Shallow clone (root unknown): normalized remote URL (scheme, user, case, `.git`).
        Remote(String),
        /// Canonical worktree or directory root. Recorded for every checkout.
        Path(PathBuf),
    }

    /// Profile-scoped operator decisions, recorded in catalog.db (never in the shared marker).
    pub(crate) enum IdentityOverride {
        /// `tracedecay project link <id>`: this checkout belongs to `id`.
        Link(ProjectId),
        /// `tracedecay project split`: do not adopt through these anchors.
        Split(Vec<IdentityAnchor>),
    }

    /// Resolution, strongest first:
    /// 1. an override for this checkout in this profile;
    /// 2. the marker's project id, only if this profile holds that project
    ///    (today's `is_initialized_project_root` rule);
    /// 3. any anchor that is an alias of an existing project (adopt it);
    /// 4. a new project minted as today from the canonical root.
    /// Every anchor computed is then recorded as an alias of the result.
    pub(crate) fn resolve(checkout: &Checkout) -> Result<(ProjectId, WorktreeId), StorageError> {
        todo!("not implemented")
    }
}

// -------------------------------------------------------------------------------------------------
// Storage classes: the one table every policy derives from
// -------------------------------------------------------------------------------------------------

/// One SQLite file per class per project (Catalog and Telemetry are per profile).
/// Adding a class means adding a row here; every policy below is a `match` on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StorageClass {
    Catalog,
    Telemetry,
    Journal,
    Memory,
    Evidence,
    Derived,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Synchronous {
    Full,
    Normal,
}

/// What a confirmed corrupt file becomes. No variant keeps a copy of the old file.
#[derive(Clone, Copy, Debug)]
pub(crate) enum OnCorruptFile {
    /// Unlink and rebuild from the producer (derived from evidence).
    Rebuild,
    /// Row-by-row copy through a fresh connection into a new file, atomic rename, finding.
    Salvage,
    /// `Salvage`, then re-ingest the gap from host files and write a loss receipt.
    SalvageAndReingest,
    /// Unlink; the rows are counters and events (telemetry).
    Drop,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum OnSchemaMismatch {
    /// Forward-only, additive steps at open; a non-additive step copies into `<class>.next`.
    Migrate,
    /// Unlink and rebuild.
    Rebuild,
}

pub(crate) struct ClassPolicy {
    pub file_name: &'static str,
    pub synchronous: Synchronous,
    /// WAL size that raises the fence for a TRUNCATE.
    pub wal_soft_bytes: u64,
    /// Foreground classes keep one reserved executor thread.
    pub foreground: bool,
    /// Measured from the moment a pool thread starts the read job.
    pub read_deadline: Duration,
    pub on_corrupt_file: OnCorruptFile,
    pub on_schema_mismatch: OnSchemaMismatch,
    /// May a budget lever delete rows of this class? Memory: never.
    pub evictable: bool,
}

impl StorageClass {
    pub(crate) const fn policy(self) -> ClassPolicy {
        const MIB: u64 = 1 << 20;
        const fn small(
            file_name: &'static str,
            synchronous: Synchronous,
            on_corrupt_file: OnCorruptFile,
            evictable: bool,
        ) -> ClassPolicy {
            ClassPolicy {
                file_name,
                synchronous,
                wal_soft_bytes: 8 * MIB,
                foreground: true,
                read_deadline: Duration::from_secs(2),
                on_corrupt_file,
                on_schema_mismatch: OnSchemaMismatch::Migrate,
                evictable,
            }
        }
        match self {
            StorageClass::Catalog => small(
                "catalog.db",
                Synchronous::Full,
                OnCorruptFile::Salvage,
                false,
            ),
            // Journal rows that are durable (definitions, approvals, scope sets) are never
            // evicted; the lever prunes only terminal aggregates and expired grants.
            StorageClass::Journal => small(
                "journal.db",
                Synchronous::Full,
                OnCorruptFile::Salvage,
                true,
            ),
            StorageClass::Memory => small(
                "memory.db",
                Synchronous::Full,
                OnCorruptFile::Salvage,
                false,
            ),
            StorageClass::Telemetry => ClassPolicy {
                foreground: false,
                ..small(
                    "telemetry.db",
                    Synchronous::Normal,
                    OnCorruptFile::Drop,
                    true,
                )
            },
            StorageClass::Evidence => ClassPolicy {
                file_name: "evidence.db",
                synchronous: Synchronous::Normal,
                wal_soft_bytes: 32 * MIB,
                foreground: false,
                read_deadline: Duration::from_secs(5),
                on_corrupt_file: OnCorruptFile::SalvageAndReingest,
                on_schema_mismatch: OnSchemaMismatch::Migrate,
                evictable: true,
            },
            StorageClass::Derived => ClassPolicy {
                file_name: "derived.db",
                synchronous: Synchronous::Normal,
                wal_soft_bytes: 32 * MIB,
                foreground: false,
                read_deadline: Duration::from_secs(5),
                on_corrupt_file: OnCorruptFile::Rebuild,
                on_schema_mismatch: OnSchemaMismatch::Rebuild,
                evictable: true,
            },
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Handles
// -------------------------------------------------------------------------------------------------

pub struct Configuration {
    catalog: executor::LaneHandle,
}

impl Configuration {
    pub async fn get(&self, key: &ConfigKey) -> Result<Option<ConfigValue>, StorageError> {
        todo!("not implemented")
    }
    /// Compare-and-set on the current revision; returns the new revision.
    pub async fn set(
        &self,
        key: ConfigKey,
        value: ConfigValue,
        expected: Option<ConfigRevision>,
    ) -> Result<ConfigRevision, StorageError> {
        todo!("not implemented")
    }
    pub async fn history(&self, key: &ConfigKey) -> Result<Vec<ConfigRevision>, StorageError> {
        todo!("not implemented")
    }
}

/// Work, Workflow, handoff grants, scope sets, git-index transactions, native integration, graph
/// heads. Replaces 24 sync ports (77 methods), three std-thread actors and interactive exact SQL.
pub struct WorkJournal {
    lane: executor::LaneHandle,
}

/// The view a journal job gets inside its one write transaction. Loads and stores any journal
/// aggregate, so invariants that span rows (capacity admission, placement uniqueness, run
/// control across sibling attempts, fence epochs) hold in one transaction.
pub struct JournalTx<'t> {
    tx: &'t mut executor::WriteTx<'t>,
}

impl JournalTx<'_> {
    pub fn load<A: JournalAggregate>(&self, key: &A::Key) -> Result<Option<A>, StorageError> {
        todo!("not implemented")
    }
    pub fn list<A: JournalAggregate>(
        &self,
        filter: &A::Filter,
        limit: u32,
    ) -> Result<Vec<A>, StorageError> {
        todo!("not implemented")
    }
    /// Stores the new state and appends its events, under a version check.
    pub fn store<A: JournalAggregate>(
        &mut self,
        aggregate: &A,
        expected_version: Option<u64>,
    ) -> Result<(), StorageError> {
        todo!("not implemented")
    }
}

impl WorkJournal {
    pub async fn get<A: JournalAggregate>(&self, key: A::Key) -> Result<Option<A>, StorageError> {
        todo!("not implemented")
    }

    /// Keyset-paged; each page is its own read job (no snapshot spans pages).
    pub async fn list<A: JournalAggregate>(
        &self,
        filter: A::Filter,
        after: Option<A::Key>,
        limit: u32,
    ) -> Result<Vec<A>, StorageError> {
        todo!("not implemented")
    }

    /// One idempotent journal transaction.
    ///
    /// The ledger is consulted FIRST: if `operation` already committed (within the 7 d
    /// idempotency window), the recorded outcome returns and `job` never runs. Otherwise `job`
    /// runs inside one write transaction and its outcome is recorded with the operation id.
    /// `job` is re-runnable (`Fn`): the executor may run it again after a reconnect. It cannot
    /// `.await`, so it cannot hold the writer across a round trip; work that must await I/O
    /// decides outside and stores under `expected_version`, which refuses a stale write with
    /// `Refused(VersionConflict)`.
    pub async fn transact<R, F>(&self, operation: OperationId, job: F) -> Result<R, StorageError>
    where
        R: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
        F: Fn(&mut JournalTx<'_>) -> Result<R, StorageError> + Send + Sync + 'static,
    {
        // TODO: lane.write(Priority::Foreground, move |tx| {
        //   if let Some(prior) = tx.ledger_outcome(&operation)? { return Ok(prior) }
        //   let outcome = job(&mut JournalTx { tx })?;
        //   tx.record_ledger(&operation, &outcome)?;
        //   Ok(outcome)
        // }).await
        todo!("not implemented")
    }
}

/// Facts and the retrieval anchors they cite, holographic banks, preferences, decisions, memory
/// graph. Durable class: FULL, never size-evicted, forward migrations only.
pub struct MemoryStore {
    lane: executor::LaneHandle,
}

impl MemoryStore {
    pub async fn commit(&self, commit: FactCommit) -> Result<Fact, StorageError> {
        todo!("not implemented")
    }
    pub async fn current(&self, query: FactQuery) -> Result<Vec<Fact>, StorageError> {
        todo!("not implemented")
    }
    pub async fn as_of(&self, query: FactQuery, at: SystemTime) -> Result<Vec<Fact>, StorageError> {
        todo!("not implemented")
    }
    pub async fn lineage(&self, id: FactId) -> Result<FactLineage, StorageError> {
        todo!("not implemented")
    }
    pub async fn get(&self, id: FactId) -> Result<Option<Fact>, StorageError> {
        todo!("not implemented")
    }
    pub async fn list(
        &self,
        query: FactQuery,
        after: Option<FactId>,
        limit: u32,
    ) -> Result<Vec<Fact>, StorageError> {
        todo!("not implemented")
    }
    /// Covers today's search, probe and reason variants through `MemorySearch` modes.
    pub async fn search(&self, search: MemorySearch) -> Result<Vec<MemorySearchHit>, StorageError> {
        todo!("not implemented")
    }
    /// Memory-graph neighbourhood (SQLite adjacency tables; depth-bounded recursive CTE).
    pub async fn related(&self, id: FactId, depth: u8) -> Result<Vec<Fact>, StorageError> {
        todo!("not implemented")
    }
    pub async fn contradictions(
        &self,
        query: FactQuery,
    ) -> Result<Vec<(Fact, Fact)>, StorageError> {
        todo!("not implemented")
    }
    pub async fn history(&self, id: FactId) -> Result<FactHistory, StorageError> {
        todo!("not implemented")
    }
    pub async fn feedback(&self, feedback: FactFeedback) -> Result<(), StorageError> {
        todo!("not implemented")
    }
    pub async fn status(&self) -> Result<MemoryStatus, StorageError> {
        todo!("not implemented")
    }
}

/// Transcript evidence and its derived read model behind one handle. Callers never pick a file.
pub struct SessionStore {
    evidence: executor::LaneHandle,
    derived: executor::LaneHandle,
}

impl SessionStore {
    /// Appends observations and advances the source cursor in one evidence transaction.
    /// Idempotent on (source, stable_record_id). Bodies are deflated blobs keyed by content hash,
    /// committed with their row (no payload file, no orphan).
    pub async fn capture(&self, batch: CaptureBatch) -> Result<CaptureOutcome, StorageError> {
        todo!("not implemented")
    }

    /// Projects evidence beyond each stream's cursor into derived.db, per stream
    /// (provider, session), within a fixed time slice; see `sessions::projection`.
    pub async fn project_pending(&self) -> Result<ProjectionPass, StorageError> {
        todo!("not implemented")
    }

    pub async fn summary_candidates(
        &self,
        limit: u32,
    ) -> Result<Vec<SummaryCandidate>, StorageError> {
        todo!("not implemented")
    }
    /// Summaries are evidence (possibly LLM-made, not rebuildable). The summary DAG references
    /// observation ids and sequence ranges, never derived rows. Recording one makes the rows it
    /// covers evictable first.
    pub async fn record_summary(&self, summary: SummaryRecord) -> Result<(), StorageError> {
        todo!("not implemented")
    }

    /// FTS over derived (contentless FTS5, rowid = evidence sequence); bodies hydrate from
    /// evidence in a second read job. Returns `partial: true` while derived is rebuilding.
    pub async fn search(&self, query: SessionQuery) -> Result<Vec<SessionSearchHit>, StorageError> {
        todo!("not implemented")
    }
    pub async fn load_session(
        &self,
        session: &str,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SessionPage, StorageError> {
        todo!("not implemented")
    }
    pub async fn cursor(&self, source: &SourceId) -> Result<Option<SourceCursor>, StorageError> {
        todo!("not implemented")
    }
    pub async fn dead_letters(&self, limit: u32) -> Result<Vec<DeadLetter>, StorageError> {
        todo!("not implemented")
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProjectionPass {
    pub projected: u32,
    pub dead_lettered: u32,
    pub streams_waiting_retry: u32,
    pub has_more: bool,
}

/// Code graph of one worktree: sealed immutable generations plus one head in the journal.
pub struct CodeGraph {
    project: Project,
    worktree: WorktreeId,
    dir: PathBuf, // projects/<id>/worktrees/<worktree_id>/
}

/// A private staging directory under projects/<id>/scratch/ for one seal. Dropping it without
/// `publish` deletes it; `Storage::open` deletes every leftover.
pub struct SealScratch {
    dir: PathBuf,
}

impl SealScratch {
    /// Where the code-index runtime writes the generation file and the sealed container.
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl CodeGraph {
    pub async fn head(&self) -> Result<Option<GenerationDigest>, StorageError> {
        todo!("not implemented")
    }
    pub async fn begin_seal(&self) -> Result<SealScratch, StorageError> {
        todo!("not implemented")
    }
    /// Proves the sealed container against its digest, renames it into the worktree dir, CASes
    /// the GraphHead aggregate in the journal, and retires generations beyond active + 2.
    pub async fn publish(
        &self,
        scratch: SealScratch,
        digest: GenerationDigest,
    ) -> Result<GenerationDigest, StorageError> {
        todo!("not implemented")
    }
    /// Read-only open of the head generation.
    pub async fn open_head(&self) -> Result<SealedGenerationReader, StorageError> {
        todo!("not implemented")
    }
}

// -------------------------------------------------------------------------------------------------
// Errors, usage, maintenance values
// -------------------------------------------------------------------------------------------------

/// The only error callers see. No engine text, codes, or WAL numbers.
#[derive(Clone, Debug)]
pub enum StorageError {
    /// Lane queue full; refused without waiting.
    Busy {
        retry_after: Duration,
    },
    Unavailable {
        class: ClassName,
        reason: UnavailableReason,
    },
    Refused(Refusal),
    NotFound,
}

#[derive(Clone, Copy, Debug)]
pub enum ClassName {
    Catalog,
    Journal,
    Memory,
    Sessions,
    Code,
}

#[derive(Clone, Debug)]
pub enum UnavailableReason {
    /// A class corruption policy is healing the file (rebuild, salvage, quick_check).
    Recovering,
    /// Derived read model rebuilding; reads may return partial results instead.
    Rebuilding {
        progress_permille: u16,
    },
    /// Read passed its class deadline and was interrupted twice.
    TimedOut,
    /// Profile root replaced or deleted under the daemon.
    ProfileLost,
    ShuttingDown,
}

#[derive(Clone, Debug)]
pub enum Refusal {
    VersionConflict,
    NoProject,
    WorktreeNotInProject,
    InvalidInput(String),
}

#[derive(Clone, Debug)]
pub struct StorageUsage {
    pub profile_bytes: u64,
    pub profile_budget_bytes: u64,
    pub projects: Vec<ProjectUsage>,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug)]
pub struct ProjectUsage {
    pub id: ProjectId,
    pub last_active: SystemTime,
    pub live_checkouts: u32,
    pub classes: Vec<ClassUsage>,
    pub worktrees: Vec<(WorktreeId, u64)>,
    pub dead_letters: u64,
    pub loss_receipts: u64,
}

#[derive(Clone, Debug)]
pub struct ClassUsage {
    pub class: ClassName,
    pub file_bytes: u64,
    pub wal_bytes: u64,
    pub free_bytes: u64,
    pub budget_bytes: Option<u64>,
    /// From persisted telemetry.storage_usage history; survives restarts.
    pub growth_bytes_per_day: i64,
}

#[derive(Clone, Debug)]
pub enum Finding {
    OverBudget {
        project: Option<ProjectId>,
        class: ClassName,
        bytes: u64,
        budget: u64,
    },
    MemoryLarge {
        project: ProjectId,
        bytes: u64,
    },
    DeadLetters {
        project: ProjectId,
        count: u64,
    },
    EvidenceLost {
        project: ProjectId,
        receipts: u64,
    },
    Salvaged {
        project: Option<ProjectId>,
        class: ClassName,
        rows_lost: u64,
    },
    ForeignReader {
        file: PathBuf,
        pids: Vec<u32>,
    },
    /// A projects/<id>/ directory with neither a catalog row nor a project.json.
    UnidentifiedDirectory {
        path: PathBuf,
        holds_memory: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct MaintenanceSlice {
    pub time_budget: Duration,
}
impl Default for MaintenanceSlice {
    fn default() -> Self {
        MaintenanceSlice {
            time_budget: Duration::from_millis(200),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MaintenanceReport {
    pub reclaimed_bytes: u64,
    pub checkpoints: u32,
    pub levers_run: u32,
    pub collected_worktrees: u32,
    pub collected_projects: u32,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ResetScope {
    /// Evidence, derived, worktrees/, scratch/, and non-durable journal rows. Identity,
    /// memory.db and durable journal rows (definitions, approvals, scope sets) stay.
    KeepDurable,
    /// Only derived.db and worktrees/ (rebuildables).
    RebuildablesOnly,
}

// =================================================================================================
// Private: executor
// =================================================================================================

mod executor {
    //! One fixed pool of OS threads (`tracedecay-store-N`) serves every SQLite file.
    //! A Lane is per-file state; threads are not. Thread count never depends on file count.
    //!
    //! Invariants:
    //! - At most one thread holds a lane's write connection at a time (lane write token).
    //! - Every reader of a file is an executor read job with a deadline and an InterruptHandle
    //!   the executor owns. No read snapshot outlives its job; no job spans an `.await`.
    //! - Therefore the executor can fence readers and TRUNCATE the WAL at any time.
    //! - Jobs are re-runnable and have no effect outside their transaction, so a job can run
    //!   again after a fence interrupt or a reconnect.
    //! - Admitted writes run to commit or rollback even if the caller's future is dropped.

    use super::*;

    pub(crate) struct Executor {
        threads: Vec<std::thread::JoinHandle<()>>,
        ready: Arc<ReadyQueue>,
        lanes: std::sync::Mutex<lru::LruCache<FileKey, Arc<Lane>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct FileKey {
        pub path: PathBuf,
        pub class: StorageClass,
    }

    /// Cheap handle the public stores hold. Opening is lazy: the first job opens the file on a
    /// pool thread (WAL recovery included) while the caller awaits.
    #[derive(Clone)]
    pub(crate) struct LaneHandle {
        key: FileKey,
        executor: Arc<ReadyQueue>,
    }

    /// Global run queue of lanes with pending work, ordered by class priority, with one thread
    /// reserved for foreground classes.
    pub(crate) struct ReadyQueue {/* crossbeam deque + condvar; foreground reservation counter */}

    pub(crate) struct Lane {
        key: FileKey,
        state: std::sync::Mutex<LaneState>,
    }

    pub(crate) struct LaneState {
        /// None until first job; closed again after `IDLE_CLOSE`.
        writer: Option<rusqlite::Connection>,
        idle_readers: Vec<rusqlite::Connection>, // at most READERS_PER_FILE
        interrupts: Vec<rusqlite::InterruptHandle>,
        writes: FairQueue<WriteJob>, // existing deficit round-robin queue, moved from rusqlite-runtime
        reads: std::collections::VecDeque<ReadJob>,
        active_reads: usize,
        write_token_held: bool,
        fence: Fence,
        health: LaneHealth,
        pinned: Option<PinnedFile>, // fd plus (dev, ino) for the unlink probe
        last_job: std::time::Instant,
        queued_bytes: usize,
    }

    pub(crate) struct FairQueue<J> {
        _j: std::marker::PhantomData<J>,
    }

    pub(crate) struct PinnedFile {
        pub fd: std::fs::File,
        pub dev: u64,
        pub ino: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Fence {
        Open,
        /// New reads queue; in-flight reads finish or are interrupted at their deadline. Lasts
        /// at most one read deadline plus the TRUNCATE.
        Draining {
            since: std::time::Instant,
        },
    }

    #[derive(Clone, Debug)]
    pub(crate) enum LaneHealth {
        Serving,
        /// Rerun failed on a fresh connection; `quick_check` runs as a background job.
        Confirming,
        /// Class policy is healing the file (`fault::recovery`).
        Recovering(OnCorruptFile),
        /// File unlinked or replaced under us; closed, reopen on next job.
        Detached,
    }

    pub(crate) const READERS_PER_FILE: usize = 2;
    pub(crate) const IDLE_CLOSE: Duration = Duration::from_secs(60);
    pub(crate) const QUEUE_MAX_OPS: usize = 2048;
    pub(crate) const QUEUE_MAX_BYTES: usize = 16 << 20;
    pub(crate) const WRITE_STATEMENT_DEADLINE: Duration = Duration::from_secs(30);
    pub(crate) const FOREIGN_READER_WAL_CEILING: u64 = 512 << 20;

    /// Re-runnable jobs. A write job takes its own savepoint inside the batch's transaction.
    pub(crate) type WriteJob = Arc<dyn Fn(&mut WriteTx<'_>) -> JobResult + Send + Sync + 'static>;
    pub(crate) type ReadJob = Arc<dyn Fn(&ReadTx<'_>) -> JobResult + Send + Sync + 'static>;
    pub(crate) type JobResult = Result<Box<dyn std::any::Any + Send>, JobError>;

    pub(crate) enum JobError {
        /// Ordinary failure: roll back this job's savepoint, reply with the error.
        Domain(StorageError),
        /// SQLite said corrupt, not-a-database or I/O: roll back the batch, reconnect, rerun
        /// each job alone (fault ladder).
        Suspect(rusqlite::Error),
        /// The fence interrupted the read; rerun after the fence drops.
        Interrupted,
    }

    /// Write transaction view handed to jobs inside this crate only. One savepoint per job;
    /// several jobs batch into one BEGIN IMMEDIATE (dwell 2 ms foreground / 10 ms background).
    pub(crate) struct WriteTx<'c> {
        tx: rusqlite::Savepoint<'c>,
    }
    pub(crate) struct ReadTx<'c> {
        tx: rusqlite::Transaction<'c>,
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Priority {
        Health,
        Foreground,
        Background,
    }

    impl LaneHandle {
        /// try_send semantics: a full lane returns Busy immediately; never waits for room.
        pub(crate) async fn write<R, F>(
            &self,
            priority: Priority,
            job: F,
        ) -> Result<R, StorageError>
        where
            R: Send + 'static,
            F: Fn(&mut WriteTx<'_>) -> Result<R, StorageError> + Send + Sync + 'static,
        {
            // TODO: wrap job with a oneshot sender; enqueue; mark lane ready; await receiver.
            // Dropping the returned future drops only the receiver (settlement outlives request).
            todo!("not implemented")
        }

        /// Runs on a pooled reader connection under the class deadline; at the deadline the
        /// executor interrupts and reruns once after any fence; a second miss is TimedOut.
        pub(crate) async fn read<R, F>(&self, priority: Priority, job: F) -> Result<R, StorageError>
        where
            R: Send + 'static,
            F: Fn(&ReadTx<'_>) -> Result<R, StorageError> + Send + Sync + 'static,
        {
            todo!("not implemented")
        }
    }

    /// What a pool thread does after finishing any job on a lane. Pure: table-tested.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CheckpointAction {
        None,
        /// Start fencing readers; the TRUNCATE runs when active_reads hits 0.
        RaiseFence,
        /// No readers active (or fence drained): run wal_checkpoint(TRUNCATE) now.
        Truncate,
        /// Idle long enough: TRUNCATE then close every connection of the lane.
        TruncateAndClose,
        /// TRUNCATE busy with zero executor readers: another process holds a read mark. Drop
        /// the fence, record Finding::ForeignReader, retry with backoff (1 s to 60 s).
        ForeignReaderBackoff,
    }

    pub(crate) fn checkpoint_decision(
        class: StorageClass,
        wal_bytes: u64,
        active_reads: usize,
        fence: Fence,
        idle_for: Duration,
        last_truncate_busy_without_readers: bool,
    ) -> CheckpointAction {
        // TODO:
        // if last_truncate_busy_without_readers { return ForeignReaderBackoff }
        // if idle_for >= IDLE_CLOSE && active_reads == 0 { return TruncateAndClose }
        // if wal_bytes < class.policy().wal_soft_bytes { return None }
        // match (fence, active_reads) {
        //   (_, 0) => Truncate,
        //   (Fence::Open, _) => RaiseFence,
        //   (Fence::Draining { .. }, _) => None,   // readers finish or are interrupted
        // }
        // Writes return Busy only past FOREIGN_READER_WAL_CEILING. No retry loop, no drain state.
        todo!("not implemented")
    }

    /// Refusal from the single sync door.
    pub(crate) struct WrongThread;

    impl Executor {
        /// The only synchronous entry point, for offline CLI threads that own no runtime.
        /// Refuses (in every build profile) on a pool thread or a native or Tokio worker.
        pub(crate) fn block_on<R>(
            &self,
            future: impl std::future::Future<Output = R>,
        ) -> Result<R, WrongThread> {
            todo!("not implemented")
        }

        pub(crate) fn start(threads: usize, open_lane_cap: usize) -> Executor {
            // TODO: spawn `threads` named tracedecay-store-{i}; each loops:
            //   lane = ready.pop(reserve_foreground);
            //   run one write batch or one read job (or a checkpoint/close action);
            //   apply checkpoint_decision; re-mark lane ready if work remains.
            // Opening a lane: writer connection with PRAGMAs from the class policy
            //   (journal_mode=WAL, synchronous per class, wal_autocheckpoint=0, NO_CKPT_ON_CLOSE,
            //    auto_vacuum=INCREMENTAL at creation, busy_timeout=0, trusted_schema=0),
            //   verify schema digest -> OnSchemaMismatch, pin the fd for the unlink probe.
            // Opening derived.db also rewinds any stream cursor ahead of evidence (power loss).
            todo!("not implemented")
        }
    }
}

// =================================================================================================
// Private: fault classification and recovery
// =================================================================================================

mod fault {
    //! Classification uses rusqlite::ErrorCode only. No message strings, no export.
    //! Schema code creates no TEMP tables: #4094 was a corrupt in-heap TEMP cache on the writer
    //! connection after interrupted statements. Caches are Rust structures or ordinary
    //! derived.db tables, so dropping a connection loses nothing durable.

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Verdict {
        /// Constraint, contract, decode: roll back one savepoint; typed refusal.
        Statement,
        /// First corrupt/not-a-database/I/O error: drop every connection of the file, rerun
        /// each job of the batch alone on a fresh connection.
        SuspectConnection,
        /// The rerun passed: the fault lived in connection or heap state. Counted only.
        ProcessLocal,
        /// The rerun failed and quick_check passed: the input is poison. Dead-letter or refuse.
        PoisonInput,
        /// The rerun failed and quick_check failed: apply the class's OnCorruptFile.
        FileCorrupt,
    }

    /// Pure; table-tested.
    pub(crate) fn verdict(
        err: &rusqlite::Error,
        after_rerun: bool,
        quick_check_passed: Option<bool>,
    ) -> Verdict {
        // TODO: match err.sqlite_error_code():
        //   DatabaseCorrupt | NotADatabase | SystemIoFailure => match (after_rerun, quick_check_passed) {
        //     (false, _) => SuspectConnection,
        //     (true, Some(true)) => PoisonInput,
        //     (true, Some(false)) => FileCorrupt,
        //     (true, None) => SuspectConnection, // quick_check still to run (background lane)
        //   }
        //   _ => Statement
        todo!("not implemented")
    }
}

// =================================================================================================
// Private: projection with dead letters (evidence -> derived)
// =================================================================================================

mod sessions {
    pub(crate) mod projection {
        //! Queue = evidence rows with seq > derived.projected_through(stream). The cursor and
        //! dead letters live in derived.db, so a derived rebuild resets them with it. Ordering is
        //! per stream (provider, session), never per file. The checkpoint never advances over a
        //! row without either projecting it or recording a dead letter (the designed path).

        use std::time::Duration;

        pub(crate) const MAX_ATTEMPTS: u32 = 3;

        #[derive(Clone, Debug)]
        pub(crate) enum ProjectionFailure {
            /// The lane is busy or unavailable: says nothing about the row.
            LaneUnavailable,
            /// The row failed while other streams of the same file projected.
            Row { reason_code: &'static str },
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(crate) enum Disposition {
            /// Leave the cursor; this stream waits `delay`; other streams continue.
            Retry { delay: Duration },
            /// Write dead_letters(observation, stream, reason, attempts, projector_version);
            /// advance projected_through past the row with gap_reason = dead_letter.
            DeadLetter { reason_code: &'static str },
        }

        /// Pure; table-tested. A lane-wide outage never dead-letters.
        pub(crate) fn dispose(failure: &ProjectionFailure, attempts: u32) -> Disposition {
            // TODO:
            // LaneUnavailable => Retry { delay: backoff(attempts) }  (5 s doubling, cap 300 s)
            // Row { reason_code } if attempts + 1 >= MAX_ATTEMPTS => DeadLetter { reason_code }
            // Row { .. } => Retry { delay: backoff(attempts) }
            todo!("not implemented")
        }

        /// Re-tries each dead letter alone (no stream rewind) on a projector version change and
        /// once per daemon incarnation; a success deletes the dead letter.
        pub(crate) fn redrive_dead_letters(current_projector_version: &str) {
            todo!("not implemented")
        }
    }
}

// =================================================================================================
// Private: retention, class levers and budgets
// =================================================================================================

mod retention {
    use super::*;

    pub(crate) struct Budgets {
        pub profile_bytes: u64,
        pub evidence_per_project: u64,
        pub code_per_project: u64,
        pub evidence_window: Duration,
        pub journal_terminal_age: Duration, // 90 d
        pub idempotency_age: Duration,      // 7 d
        pub telemetry_events_age: Duration, // 30 d
        pub code_history: u8,               // active + 2
        pub project_idle_collection: Duration,
    }

    /// Each lever touches exactly one class. A class's pressure pulls only its own lever; the
    /// profile budget pulls levers in this enum's order (rebuildable classes first) and never
    /// evicts evidence to pay for code. No lever deletes memory or durable journal rows.
    #[derive(Debug)]
    pub(crate) enum Lever {
        /// The worktree root is gone: remove projects/<id>/worktrees/<wt>/ now.
        DeadWorktree {
            project: ProjectId,
            worktree: WorktreeId,
        },
        /// Non-head generations first, then heads of the idlest worktrees (rebuilt on demand).
        Code {
            project: ProjectId,
            target_bytes: u64,
        },
        /// Trim derived rows behind the evidence window.
        DerivedTrim {
            project: ProjectId,
            before_captured_at: SystemTime,
        },
        /// By capture time, summary-covered rows first; cascades to derived rows of the same
        /// sequences in the next derived job (consumer-side delete, no cross-file transaction).
        Evidence {
            project: ProjectId,
            before_captured_at: SystemTime,
            uncovered_allowed: bool,
        },
        JournalTerminal {
            project: ProjectId,
            older_than: SystemTime,
        },
        Telemetry {
            older_than: SystemTime,
        },
        ResponseHandles {
            older_than: SystemTime,
            over_bytes: u64,
        },
        /// No live checkout for project_idle_collection: keep memory.db, durable journal rows
        /// and project.json; remove the rest.
        IdleProject {
            project: ProjectId,
        },
    }

    pub(crate) struct Liveness {
        pub project: ProjectId,
        pub last_active: SystemTime,
        /// stat() of every recorded checkout root this tick; NOT the shared .git.
        pub live_worktrees: Vec<WorktreeId>,
        pub dead_worktrees: Vec<WorktreeId>,
    }

    /// Pure planner: usage + liveness + budgets -> ordered levers.
    pub(crate) fn plan(
        usage: &StorageUsage,
        liveness: &[Liveness],
        budgets: &Budgets,
        now: SystemTime,
    ) -> Vec<Lever> {
        todo!("not implemented")
    }

    /// A `projects/<id>/` the catalog does not name: re-register it from project.json. With no
    /// project.json either, remove only its rebuildable classes and raise
    /// Finding::UnidentifiedDirectory; memory.db is never removed here.
    pub(crate) fn adopt_unregistered(dir: &Path) {
        todo!("not implemented")
    }
}

// =================================================================================================
// Private: lifecycle probes
// =================================================================================================

mod lifecycle {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum ProbeResult {
        Intact,
        /// fstat(pinned).st_nlink == 0 or the path now names another inode: close the lane
        /// (frees the space), mark Detached, reopen on next job.
        Detached,
        /// profile-identity.json unlinked or replaced: close every lane and exit the daemon
        /// with ProfileLost; the supervisor restarts against the new profile.
        ProfileLost,
    }

    pub(crate) fn probe(pinned: &executor::PinnedFile, path: &Path) -> ProbeResult {
        todo!("not implemented")
    }
}

// =================================================================================================
// Usage sketch as compile-shaped call sites (mirrors README "Usage")
// =================================================================================================

#[cfg(any())]
mod call_sites {
    use super::*;

    // daemon-service: Work cancellation (was off_native_thread + WorkAttemptService over 24 ports).
    async fn cancel_attempt(
        project: &Project,
        operation: OperationId,
        attempt: WorkAttemptKey,
        cmd: CancelCmd,
    ) -> Result<CancelOutcome, StorageError> {
        project
            .work()
            .transact(operation, move |tx| {
                tracedecay_contracts::work::request_cancellation(tx, &attempt, &cmd)
            })
            .await
    }

    // session-runtime refresh worker pass.
    async fn refresh(project: &Project, batch: CaptureBatch) -> Result<(), StorageError> {
        let sessions = project.sessions();
        sessions.capture(batch).await?;
        while sessions.project_pending().await?.has_more {}
        Ok(())
    }

    // daemon maintenance coordinator.
    async fn tick(storage: &Storage) {
        while storage.maintain(MaintenanceSlice::default()).await.has_more {}
    }
}
