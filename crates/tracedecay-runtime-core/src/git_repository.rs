//! Single read-only Git repository authority.
//!
//! Repository topology, refs, HEAD, object format, operation state, status,
//! and bounded history are read through `gix`.

use std::collections::{BTreeSet, HashMap};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use gix::bstr::ByteSlice as _;
use tracedecay_domain::git::{
    GitChangeKindV1, GitDegradationV1, GitFileModeV1, GitHeadStateV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, GitStatusEntryV1, GitTrackedStatusV1,
};

mod history;
mod native_integration;
pub use history::{
    GitHistoryBudget, GitHistoryOptions, GitHistoryTermination, GitRepositoryHistory,
};
pub use native_integration::{
    GitNativeApplyOutcome, GitNativeCandidateTreeV1, GitNativeCandidateTreeVisitError,
    GitNativeIntegrationMode, GitNativePreflight, GitNativePreflightCaptureError,
    GitNativePreflightDisposition, GitNativeUnsupportedReason,
};

/// A typed failure from the in-process Git repository authority.
#[derive(Debug, thiserror::Error)]
pub enum GitRepositoryError {
    #[error("not a Git repository: {path}")]
    NotARepository { path: String },
    #[error("Git repository at {path} is unreadable: {detail}")]
    UnreadableRepository { path: String, detail: String },
    #[error("Git HEAD is unreadable: {detail}")]
    UnreadableHead { detail: String },
    /// A discovery for this path is already in progress and has not published.
    ///
    /// Callers must not wait on the in-flight walk: the walk is blocking
    /// filesystem IO, and waiting for it on this thread is what pinned every
    /// other project open behind one hung `open()`.
    #[error("repository discovery blocked on {path}")]
    DiscoveryBlocked { path: String },
    #[error("Git repository {operation} failed: {detail}")]
    Operation {
        operation: &'static str,
        detail: String,
    },
    #[error(transparent)]
    Domain(#[from] tracedecay_domain::research::DomainError),
}

/// One resolved reference and its direct object target, if it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitReference {
    pub name: String,
    pub target: Option<GitOidV1>,
    pub symbolic_target: Option<String>,
}

/// Repository status without application-specific repository identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryStatus {
    pub head: GitHeadStateV1,
    pub operation: GitOperationStateV1,
    pub entries: Vec<GitStatusEntryV1>,
    pub degradations: BTreeSet<GitDegradationV1>,
}

/// One thread-safe `gix` repository authority.
#[derive(Debug)]
pub struct GitRepositoryAuthority {
    repository: gix::ThreadSafeRepository,
    worktree_root: Option<PathBuf>,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

/// The repository paths one discovery resolves, before any ref or object is
/// read: the upward walk for `.git`, the repository open, and the canonical
/// form of each directory it names.
///
/// Separated from [`GitRepositoryAuthority`] because topology is the only part
/// of a discovery that is stable for a checkout. HEAD, refs, and status are
/// live reads and are answered from a freshly discovered authority every time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryTopologyV1 {
    pub worktree_root: Option<PathBuf>,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

/// One retained topology resolution.
///
/// The mutex covers only the memo and the single-flight flag. The blocking
/// repository walk runs outside it, so a hung `open()` of one checkout cannot
/// queue every other project-open thread on this lock.
#[derive(Default)]
struct CheckoutTopologySlot {
    state: Mutex<CheckoutTopologyState>,
}

#[derive(Default)]
enum CheckoutTopologyState {
    #[default]
    Vacant,
    Resolving,
    Ready(Arc<GitRepositoryTopologyV1>),
}

/// Retained checkout-root topologies.
///
/// Bounded by [`MAX_RETAINED_CHECKOUT_TOPOLOGIES`]; a full map is cleared
/// rather than evicted by age, because every entry is a pure memo that costs
/// one discovery to rebuild.
static CHECKOUT_TOPOLOGY: LazyLock<Mutex<HashMap<PathBuf, Arc<CheckoutTopologySlot>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A daemon serves few project roots; this bound exists so a long-lived
/// process that probes many paths cannot grow the memo without limit.
const MAX_RETAINED_CHECKOUT_TOPOLOGIES: usize = 64;

/// Last branch read from each Git directory's HEAD, keyed by that file's
/// identity. Bounded and cleared like [`CHECKOUT_TOPOLOGY`].
static HEAD_BRANCHES: LazyLock<Mutex<HeadBranchMemo>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

type HeadBranchMemo = HashMap<PathBuf, (HeadFileStamp, Option<String>)>;

/// Identity of a HEAD file. Git replaces HEAD by renaming a lock file over
/// it, so every checkout yields a new inode and change time.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadFileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
    #[cfg(unix)]
    changed: (i64, i64),
}

impl HeadFileStamp {
    fn read(head: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(head).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: (metadata.dev(), metadata.ino()),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

/// Repository topology for `path`, resolved once per checkout root.
///
/// Live defect this exists for: one daemon connection asked the same
/// repository for its common directory, worktree root, and linked-worktree
/// shape a dozen times, and each question ran a complete `gix` discovery,
/// an upward walk to the filesystem root plus a repository open. On a slow
/// volume that is seconds per question, paid again by every concurrent
/// client, and it ran inline on the tokio workers that also poll the daemon's
/// accept loop.
///
/// Only a path that **is** its own worktree root is retained. The checkout's
/// `.git` marker and linked-worktree `commondir` are revalidated before reuse,
/// so replacing or retargeting that root cannot inherit its old identity.
/// Every other path, a subdirectory, a bare repository, an unresolvable
/// directory, is discovered live, so a repository created below it is
/// observed immediately.
pub fn repository_topology(
    path: &Path,
) -> Result<Arc<GitRepositoryTopologyV1>, GitRepositoryError> {
    let slot = checkout_topology_slot(path);
    if let Some(topology) = live_ready_topology(&slot) {
        return Ok(topology);
    }
    if !begin_checkout_resolution(&slot) {
        return Err(GitRepositoryError::DiscoveryBlocked {
            path: path.display().to_string(),
        });
    }
    let guard = CheckoutResolutionGuard { slot: &slot };
    #[cfg(any(test, feature = "test-helpers"))]
    observe_topology_resolution(path);
    let topology = Arc::new(
        hotpath::measure_block!(
            "runtime_core.git.topology.resolve",
            GitRepositoryAuthority::discover_uncached(path)
        )?
        .into_topology(),
    );
    guard.publish(path, &topology);
    Ok(topology)
}

/// A live retained topology, or `None` when the memo is empty, stale, or
/// another thread owns the walk. Liveness reads the filesystem and must not
/// run while the slot mutex is held.
fn live_ready_topology(slot: &CheckoutTopologySlot) -> Option<Arc<GitRepositoryTopologyV1>> {
    let ready = {
        let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            CheckoutTopologyState::Ready(topology) => Some(Arc::clone(topology)),
            CheckoutTopologyState::Vacant | CheckoutTopologyState::Resolving => None,
        }
    }?;
    checkout_topology_is_live(&ready).then_some(ready)
}

/// Claim the single in-flight walk for `slot`.
///
/// `false` means another thread already owns it. The caller reports
/// [`GitRepositoryError::DiscoveryBlocked`] instead of waiting: waiting on
/// this mutex is the queue that stalled every other project open.
fn begin_checkout_resolution(slot: &CheckoutTopologySlot) -> bool {
    let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
    match &*state {
        CheckoutTopologyState::Resolving => false,
        CheckoutTopologyState::Ready(_) | CheckoutTopologyState::Vacant => {
            *state = CheckoutTopologyState::Resolving;
            true
        }
    }
}

/// Clears a claimed resolution that did not publish, including on panic.
struct CheckoutResolutionGuard<'a> {
    slot: &'a CheckoutTopologySlot,
}

impl CheckoutResolutionGuard<'_> {
    fn publish(self, path: &Path, topology: &Arc<GitRepositoryTopologyV1>) {
        let retain = topology
            .worktree_root
            .as_deref()
            .is_some_and(|root| path.canonicalize().is_ok_and(|canonical| canonical == root));
        if !retain && let Some(root) = topology.worktree_root.as_deref() {
            // Publish the root before this slot, and without holding this
            // slot: the root's own resolution never locks a second slot, but
            // holding this one across that lock would invert the order.
            publish_checkout_root_topology(root, topology);
        }
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *state = if retain {
            CheckoutTopologyState::Ready(Arc::clone(topology))
        } else {
            CheckoutTopologyState::Vacant
        };
        std::mem::forget(self);
    }
}

impl Drop for CheckoutResolutionGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, CheckoutTopologyState::Resolving) {
            *state = CheckoutTopologyState::Vacant;
        }
    }
}

/// Retain a topology under the worktree root it resolved, not the path it was
/// discovered from.
///
/// Safe to call while holding another path's slot: the root's own resolution
/// takes the retain arm above and never reaches for a second slot, so no
/// thread holds these two locks in the opposite order.
fn publish_checkout_root_topology(root: &Path, topology: &Arc<GitRepositoryTopologyV1>) {
    let slot = checkout_topology_slot(root);
    let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
    *state = CheckoutTopologyState::Ready(Arc::clone(topology));
}

/// A live retained topology for `path`, without resolving one.
///
/// Never creates a slot: a peek that inserted would let unresolvable paths
/// evict the memo this exists to preserve.
fn retained_checkout_topology(path: &Path) -> Option<Arc<GitRepositoryTopologyV1>> {
    let slot = Arc::clone(
        CHECKOUT_TOPOLOGY
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)?,
    );
    live_ready_topology(&slot)
}

fn checkout_topology_slot(path: &Path) -> Arc<CheckoutTopologySlot> {
    let mut slots = CHECKOUT_TOPOLOGY
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(slot) = slots.get(path) {
        return Arc::clone(slot);
    }
    if slots.len() >= MAX_RETAINED_CHECKOUT_TOPOLOGIES {
        slots.clear();
    }
    let slot = Arc::new(CheckoutTopologySlot::default());
    slots.insert(path.to_path_buf(), Arc::clone(&slot));
    slot
}

/// Whether a retained checkout-root topology still describes the filesystem.
///
/// `<root>/.git` is the entry the upward walk stopped at. Its live target and
/// the per-worktree `commondir` target must still equal the retained identity;
/// existence alone would let a path replacement inherit stale authority.
fn checkout_topology_is_live(topology: &GitRepositoryTopologyV1) -> bool {
    let Some(root) = topology.worktree_root.as_ref() else {
        return false;
    };
    let dot_git = root.join(".git");
    let live_git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let Ok(path) = gix::discover::path::from_gitdir_file(&dot_git) else {
            return false;
        };
        path
    };
    let Ok(live_git_dir) = live_git_dir.canonicalize() else {
        return false;
    };
    if live_git_dir != topology.git_dir {
        return false;
    }
    let common_dir_file = live_git_dir.join("commondir");
    let live_common_dir =
        match gix::discover::path::from_plain_file_relative_to_file(&common_dir_file) {
            Some(Ok(path)) => match path.canonicalize() {
                Ok(path) => path,
                Err(_) => return false,
            },
            Some(Err(_)) => return false,
            None => live_git_dir,
        };
    live_common_dir == topology.common_dir
}

/// Counts live `gix` discoveries and injects discovery latency, per root.
///
/// Repository discovery is the blocking filesystem cost the daemon's route
/// resolution is bounded against, so tests need both to observe how many
/// discoveries a journey really runs and to make one slow on demand.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Default)]
struct RepositoryDiscoveryObservation {
    discoveries: u64,
    topology_resolutions: u64,
    delay: Option<std::time::Duration>,
    /// When set, live discovery pays [`Self::delay`] then returns
    /// [`GitRepositoryError::UnreadableRepository`] without opening the
    /// repository, so callers can exercise the Git CLI fallback after a
    /// slow unreadable authority phase.
    force_unreadable: bool,
    /// When set, the walk waits on a test channel instead of sleeping, so a
    /// hung discovery is controllable without occupying a timeout.
    block: Option<std::sync::Arc<RepositoryDiscoveryBlockGate>>,
}

#[cfg(any(test, feature = "test-helpers"))]
static REPOSITORY_DISCOVERY_OBSERVATIONS: LazyLock<
    Mutex<HashMap<PathBuf, RepositoryDiscoveryObservation>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(any(test, feature = "test-helpers"))]
fn repository_discovery_observations()
-> std::sync::MutexGuard<'static, HashMap<PathBuf, RepositoryDiscoveryObservation>> {
    REPOSITORY_DISCOVERY_OBSERVATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(any(test, feature = "test-helpers"))]
fn observed_discovery_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

#[cfg(any(test, feature = "test-helpers"))]
fn observe_repository_discovery(path: &Path) {
    let (delay, block) = {
        let mut observations = repository_discovery_observations();
        if observations.is_empty() {
            return;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut delay = None;
        let mut block = None;
        for (root, observation) in observations.iter_mut() {
            if !canonical.starts_with(root) {
                continue;
            }
            observation.discoveries = observation.discoveries.saturating_add(1);
            delay = delay.or(observation.delay);
            if block.is_none() {
                block.clone_from(&observation.block);
            }
        }
        (delay, block)
    };
    // Waiting under the observation lock would serialize every other root's
    // discovery behind this one and hide the concurrency the tests assert.
    if let Some(block) = block {
        block.enter_and_wait();
    }
    if let Some(delay) = delay {
        std::thread::sleep(delay);
    }
}

/// One test-owned block of the live discovery walk under a root.
///
/// The walk signals [`Self::entered`] and then waits on `release`. Other
/// projects are not in this wait, and other callers of the blocked root get
/// [`GitRepositoryError::DiscoveryBlocked`] instead of queueing behind it.
#[cfg(any(test, feature = "test-helpers"))]
struct RepositoryDiscoveryBlockGate {
    entered_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    entered: tokio::sync::watch::Sender<bool>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl RepositoryDiscoveryBlockGate {
    fn enter_and_wait(&self) {
        let _ = self.entered.send(true);
        if let Some(entered) = self
            .entered_tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = entered.send(());
        }
        if let Some(release) = self
            .release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = release.recv();
        }
    }
}

/// Handle for a discovery walk parked on a channel.
///
/// Dropping `release` unblocks the walk. Await [`Self::entered`] to learn the
/// walk has reached the blocking section.
#[cfg(any(test, feature = "test-helpers"))]
pub struct RepositoryDiscoveryBlock {
    pub entered: tokio::sync::oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl RepositoryDiscoveryBlock {
    /// Wait until the parked walk has entered its blocking section.
    pub async fn wait_entered(&mut self) {
        let _ = (&mut self.entered).await;
    }

    /// Let the parked walk finish.
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

/// Park every live discovery walk under `root` until [`RepositoryDiscoveryBlock::release`].
///
/// The walk signals `entered` from the blocking section and does not hold the
/// topology slot while it waits. Implies [`observe_repository_discovery_for_test`].
#[cfg(any(test, feature = "test-helpers"))]
pub fn block_repository_discovery_for_test(root: &Path) -> RepositoryDiscoveryBlock {
    forget_retained_checkout_topology_for_test(root);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (watch_tx, _watch_rx) = tokio::sync::watch::channel(false);
    repository_discovery_observations().insert(
        observed_discovery_root(root),
        RepositoryDiscoveryObservation {
            block: Some(std::sync::Arc::new(RepositoryDiscoveryBlockGate {
                entered_tx: Mutex::new(Some(entered_tx)),
                entered: watch_tx,
                release: Mutex::new(Some(release_rx)),
            })),
            ..RepositoryDiscoveryObservation::default()
        },
    );
    RepositoryDiscoveryBlock {
        entered: entered_rx,
        release: release_tx,
    }
}

/// `true` once a test block for `directory` has entered its wait.
///
/// `false` immediately when no block is armed, so production deadlines keep
/// their own timer.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn wait_until_repository_discovery_blocks(directory: &Path) -> bool {
    let Some(block) = armed_discovery_block(directory) else {
        return false;
    };
    let mut entered = block.entered.subscribe();
    if *entered.borrow() {
        return true;
    }
    while entered.changed().await.is_ok() {
        if *entered.borrow() {
            return true;
        }
    }
    *entered.borrow()
}

/// Production builds arm no discovery blocks, so the probe budget always
/// falls through to its own deadline.
#[cfg(not(any(test, feature = "test-helpers")))]
pub async fn wait_until_repository_discovery_blocks(_directory: &Path) -> bool {
    false
}

#[cfg(any(test, feature = "test-helpers"))]
fn armed_discovery_block(directory: &Path) -> Option<std::sync::Arc<RepositoryDiscoveryBlockGate>> {
    let observations = repository_discovery_observations();
    if observations.is_empty() {
        return None;
    }
    let canonical = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    observations.iter().find_map(|(root, observation)| {
        canonical
            .starts_with(root)
            .then(|| observation.block.clone())
            .flatten()
    })
}

/// Begin counting live discoveries under `root`, and forget any topology
/// already retained for it, so a fixture's counts start from a cold authority.
#[cfg(any(test, feature = "test-helpers"))]
pub fn observe_repository_discovery_for_test(root: &Path) {
    forget_retained_checkout_topology_for_test(root);
    repository_discovery_observations().insert(
        observed_discovery_root(root),
        RepositoryDiscoveryObservation::default(),
    );
}

/// Make every live discovery walk under `root` take `delay`, modelling a
/// repository on a slow volume. A repository opened from a retained topology
/// pays no walk and so is not delayed, which is exactly the convergence the
/// deferral tests assert. Implies [`observe_repository_discovery_for_test`].
#[cfg(any(test, feature = "test-helpers"))]
pub fn delay_repository_discovery_for_test(root: &Path, delay: std::time::Duration) {
    forget_retained_checkout_topology_for_test(root);
    repository_discovery_observations().insert(
        observed_discovery_root(root),
        RepositoryDiscoveryObservation {
            delay: Some(delay),
            ..RepositoryDiscoveryObservation::default()
        },
    );
}

/// Pay `delay` on every live discovery under `root`, then fail as an
/// unreadable authority so the Git CLI fallback is the only resolution path.
///
/// Models a slow in-process phase that returns unreadable while both phases
/// still share one discovery deadline.
#[cfg(any(test, feature = "test-helpers"))]
pub fn unreadable_repository_discovery_for_test(root: &Path, delay: std::time::Duration) {
    forget_retained_checkout_topology_for_test(root);
    repository_discovery_observations().insert(
        observed_discovery_root(root),
        RepositoryDiscoveryObservation {
            delay: Some(delay),
            force_unreadable: true,
            ..RepositoryDiscoveryObservation::default()
        },
    );
}

#[cfg(any(test, feature = "test-helpers"))]
fn forced_unreadable_repository_discovery(path: &Path) -> bool {
    let observations = repository_discovery_observations();
    if observations.is_empty() {
        return false;
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    observations
        .iter()
        .any(|(root, observation)| observation.force_unreadable && canonical.starts_with(root))
}

/// Live `gix` discoveries observed under `root` since observation began.
#[cfg(any(test, feature = "test-helpers"))]
#[must_use]
pub fn repository_discovery_count_for_test(root: &Path) -> u64 {
    repository_discovery_observations()
        .get(&observed_discovery_root(root))
        .map_or(0, |observation| observation.discoveries)
}

/// Topology resolutions under `root`, the discoveries the retained authority
/// could not answer, since observation began.
#[cfg(any(test, feature = "test-helpers"))]
#[must_use]
pub fn repository_topology_resolution_count_for_test(root: &Path) -> u64 {
    repository_discovery_observations()
        .get(&observed_discovery_root(root))
        .map_or(0, |observation| observation.topology_resolutions)
}

#[cfg(any(test, feature = "test-helpers"))]
fn observe_topology_resolution(path: &Path) {
    let mut observations = repository_discovery_observations();
    if observations.is_empty() {
        return;
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for (root, observation) in observations.iter_mut() {
        if canonical.starts_with(root) {
            observation.topology_resolutions = observation.topology_resolutions.saturating_add(1);
        }
    }
}

/// Stop observing `root` and drop its retained topology.
#[cfg(any(test, feature = "test-helpers"))]
pub fn reset_repository_discovery_for_test(root: &Path) {
    repository_discovery_observations().remove(&observed_discovery_root(root));
    forget_retained_checkout_topology_for_test(root);
}

#[cfg(any(test, feature = "test-helpers"))]
fn forget_retained_checkout_topology_for_test(root: &Path) {
    let canonical = observed_discovery_root(root);
    CHECKOUT_TOPOLOGY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|path, _| !path.starts_with(&canonical) && !canonical.starts_with(path));
}

impl GitRepositoryAuthority {
    /// Open the repository `path` belongs to.
    ///
    /// A retained topology answers the *where* half of a discovery, the
    /// upward walk for `.git` and the canonical form of each directory it
    /// names, so this opens the repository directly at its own Git directory
    /// instead of walking the volume again. Live reads (HEAD, refs, status)
    /// still come from a freshly opened repository.
    ///
    /// Live defect this exists for: the topology memo only short-circuited
    /// `repository_topology`. Every HEAD read, one per route resolution, from
    /// `current_branch`, still ran a complete repository discovery, so on a slow
    /// volume a deferred route never converged: the memo was warm and the next
    /// request paid the whole walk again anyway.
    pub fn discover(path: &Path) -> Result<Self, GitRepositoryError> {
        if let Some(topology) = retained_checkout_topology(path)
            && let Some(authority) = Self::open_retained(&topology)
        {
            return Ok(authority);
        }
        // Cold opens share the per-path walk. A second caller that finds the
        // walk already in progress is discovery-blocked instead of starting
        // another `open()` and queueing on the slot.
        let topology = repository_topology(path)?;
        Self::open_retained(&topology).ok_or_else(|| GitRepositoryError::UnreadableRepository {
            path: path.display().to_string(),
            detail: "retained Git directory could not be opened".to_owned(),
        })
    }

    /// Open a repository whose topology is already known, or `None` when the
    /// open fails and the full walk has to decide.
    fn open_retained(topology: &GitRepositoryTopologyV1) -> Option<Self> {
        let repository = hotpath::measure_block!(
            "runtime_core.git.repository_open_retained",
            crate::git_open::open(&topology.git_dir)
        )
        .ok()?;
        Some(Self {
            repository: repository.into_sync(),
            worktree_root: topology.worktree_root.clone(),
            git_dir: topology.git_dir.clone(),
            common_dir: topology.common_dir.clone(),
        })
    }

    #[hotpath::measure(label = "runtime_core.git.repository_discover")]
    fn discover_uncached(path: &Path) -> Result<Self, GitRepositoryError> {
        #[cfg(any(test, feature = "test-helpers"))]
        observe_repository_discovery(path);
        #[cfg(any(test, feature = "test-helpers"))]
        if forced_unreadable_repository_discovery(path) {
            return Err(GitRepositoryError::UnreadableRepository {
                path: path.display().to_string(),
                detail: "test-forced unreadable repository discovery".to_owned(),
            });
        }
        let repository = hotpath::measure_block!(
            "runtime_core.git.repository_discover.walk",
            crate::git_open::discover(path)
        )
        .map_err(|error| match error {
            gix::discover::Error::Discover(gix::discover::upwards::Error::NoGitRepository {
                ..
            }) => GitRepositoryError::NotARepository {
                path: path.display().to_string(),
            },
            error => GitRepositoryError::UnreadableRepository {
                path: path.display().to_string(),
                detail: error.to_string(),
            },
        })?;
        let (worktree_root, git_dir, common_dir) = hotpath::measure_block!(
            "runtime_core.git.repository_discover.canonicalize",
            (
                repository
                    .workdir()
                    .map(|path| canonical(path, "worktree root"))
                    .transpose(),
                canonical(repository.git_dir(), "Git directory"),
                canonical(repository.common_dir(), "Git common directory"),
            )
        );
        let worktree_root = worktree_root.map_err(|error| repository_error(path, error))?;
        let git_dir = git_dir.map_err(|error| repository_error(path, error))?;
        let common_dir = common_dir.map_err(|error| repository_error(path, error))?;
        Ok(Self {
            repository: repository.into_sync(),
            worktree_root,
            git_dir,
            common_dir,
        })
    }

    /// Exact per-worktree checkout root, absent for bare repositories.
    pub fn worktree_root(&self) -> Option<&Path> {
        self.worktree_root.as_deref()
    }

    /// The stable paths this discovery resolved, without the open repository.
    fn into_topology(self) -> GitRepositoryTopologyV1 {
        GitRepositoryTopologyV1 {
            worktree_root: self.worktree_root,
            git_dir: self.git_dir,
            common_dir: self.common_dir,
        }
    }

    /// Exact per-worktree Git directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Shared repository common directory.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// Repository object format from parsed Git configuration.
    pub fn object_format(&self) -> Result<GitObjectFormatV1, GitRepositoryError> {
        match self.repository.to_thread_local().object_hash() {
            gix::hash::Kind::Sha1 => Ok(GitObjectFormatV1::Sha1),
            gix::hash::Kind::Sha256 => Ok(GitObjectFormatV1::Sha256),
            format => Err(GitRepositoryError::Operation {
                operation: "object format",
                detail: format!("unsupported object format {format}"),
            }),
        }
    }

    /// The branch HEAD names for the checkout `path` belongs to; `None` when
    /// HEAD is detached or the repository is unreadable.
    ///
    /// Every tool call asks this, and opening the repository per call was the
    /// dominant per-request Git cost. For a retained checkout the answer is
    /// reused until the HEAD file's identity changes. Reftable repositories
    /// keep HEAD in the table stack, so their answer is never reused.
    pub fn current_branch(path: &Path) -> Option<String> {
        let topology = repository_topology(path).ok()?;
        let stamp = HeadFileStamp::read(&topology.git_dir.join("HEAD"));
        if let Some(stamp) = &stamp
            && let Some((cached, branch)) = HEAD_BRANCHES
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&topology.git_dir)
            && cached == stamp
        {
            return branch.clone();
        }
        let authority = match Self::open_retained(&topology) {
            Some(authority) => authority,
            None => Self::discover(path).ok()?,
        };
        let branch = authority.head().ok()?.branch().map(str::to_owned);
        // The stamp was taken before the read, so a HEAD replaced in between
        // is re-read on the next call instead of being pinned.
        if let Some(stamp) = stamp
            && !topology.common_dir.join("reftable").exists()
        {
            let mut branches = HEAD_BRANCHES.lock().unwrap_or_else(PoisonError::into_inner);
            if branches.len() >= MAX_RETAINED_CHECKOUT_TOPOLOGIES {
                branches.clear();
            }
            branches.insert(topology.git_dir.clone(), (stamp, branch.clone()));
        }
        branch
    }

    /// Exact HEAD state for this repository or linked worktree.
    #[hotpath::measure(label = "runtime_core.git.head")]
    pub fn head(&self) -> Result<GitHeadStateV1, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        head_from_gix(&repository)
    }

    /// Whether any current main or linked worktree has the reference checked out.
    ///
    /// Reading the complete repository worktree inventory lets mutation
    /// callers fail closed when a linked checkout was not part of their
    /// authorized routing roots or appeared after preflight.
    pub fn reference_is_checked_out(&self, reference: &str) -> Result<bool, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let main = repository
            .main_repo()
            .map_err(|error| operation("open main worktree", error))?;
        if main.workdir().is_some() && head_matches_reference(head_from_gix(&main)?, reference) {
            return Ok(true);
        }
        for proxy in repository
            .worktrees()
            .map_err(|error| operation("list worktrees", error))?
        {
            let linked = proxy
                .into_repo()
                .map_err(|error| operation("open linked worktree", error))?;
            if head_matches_reference(head_from_gix(&linked)?, reference) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// All ordinary repository refs in stable name order.
    #[hotpath::measure(label = "runtime_core.git.references")]
    pub fn references(&self) -> Result<Vec<GitReference>, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let platform = repository
            .references()
            .map_err(|error| operation("references", error))?;
        let iter = platform
            .all()
            .map_err(|error| operation("references", error))?;
        let mut references = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|error| operation("references", error))?;
            let target = reference.target();
            references.push(GitReference {
                name: reference.name().as_bstr().to_string(),
                target: target
                    .try_id()
                    .map(|target| GitOidV1::new(target.to_string()))
                    .transpose()?,
                symbolic_target: target.try_name().map(|name| name.as_bstr().to_string()),
            });
        }
        references.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(references)
    }

    /// Parsed in-progress operation state.
    ///
    /// `gix::state::InProgress` has no sequencer variant. An interrupted
    /// `cherry-pick`/`revert` sequence whose `CHERRY_PICK_HEAD`/`REVERT_HEAD`
    /// is already gone still leaves `.git/sequencer` behind, and `gix` reports
    /// no in-progress state at all for it. Reading the directory marker keeps
    /// [`GitOperationStateV1::Sequencer`] reachable through the status path
    /// instead of collapsing an in-progress sequence to `None`.
    pub fn operation_state(&self) -> GitOperationStateV1 {
        use gix::state::InProgress;

        match self.repository.to_thread_local().state() {
            None if self.git_dir.join("sequencer").is_dir() => GitOperationStateV1::Sequencer,
            None => GitOperationStateV1::None,
            Some(InProgress::Merge) => GitOperationStateV1::Merge,
            Some(
                InProgress::ApplyMailbox
                | InProgress::ApplyMailboxRebase
                | InProgress::Rebase
                | InProgress::RebaseInteractive,
            ) => GitOperationStateV1::Rebase,
            Some(InProgress::CherryPick | InProgress::CherryPickSequence) => {
                GitOperationStateV1::CherryPick
            }
            Some(InProgress::Revert | InProgress::RevertSequence) => GitOperationStateV1::Revert,
            Some(InProgress::Bisect) => GitOperationStateV1::Bisect,
        }
    }

    /// Live staged, unstaged, untracked, ignored, conflict, and submodule
    /// status directly from the current index and working tree.
    #[hotpath::measure(label = "runtime_core.git.status")]
    pub fn status(&self) -> Result<GitRepositoryStatus, GitRepositoryError> {
        use gix::diff::index::ChangeRef;
        use gix::dir::entry::Status as DirectoryStatus;
        use gix::status::Item;
        use gix::status::index_worktree::Item as IndexWorktreeItem;
        use gix::status::plumbing::index_as_worktree::{Change as WorktreeChange, EntryStatus};

        let repository = self.repository.to_thread_local();
        let mut platform = repository
            .status(gix::progress::Discard)
            .map_err(|error| operation("status", error))?
            .untracked_files(gix::status::UntrackedFiles::Files)
            .index_worktree_rewrites(None);
        platform.dirwalk_options_mut(|options| {
            options.set_emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
        });
        let status = platform
            .into_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|error| operation("status", error))?;

        let mut tracked = HashMap::<String, TrackedStatusBuilder>::new();
        let mut loose = HashMap::<String, GitStatusEntryV1>::new();
        for item in status {
            match item.map_err(|error| operation("status", error))? {
                Item::TreeIndex(change) => match change {
                    ChangeRef::Addition {
                        location,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(GitChangeKindV1::Added, None, Some(mode(entry_mode)?), None);
                    }
                    ChangeRef::Deletion {
                        location,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                GitChangeKindV1::Deleted,
                                Some(mode(entry_mode)?),
                                None,
                                None,
                            );
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                GitChangeKindV1::Modified,
                                Some(mode(previous_entry_mode)?),
                                Some(mode(entry_mode)?),
                                None,
                            );
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        source_entry_mode,
                        location,
                        entry_mode,
                        copy,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        let source = path_text(source_location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                if copy {
                                    GitChangeKindV1::Copied
                                } else {
                                    GitChangeKindV1::Renamed
                                },
                                Some(mode(source_entry_mode)?),
                                Some(mode(entry_mode)?),
                                Some(source),
                            );
                    }
                },
                Item::IndexWorktree(worktree) => match worktree {
                    IndexWorktreeItem::Modification {
                        entry,
                        rela_path,
                        status,
                        ..
                    } => {
                        let path = path_text(rela_path.as_ref(), "status")?;
                        match status {
                            EntryStatus::NeedsUpdate(_) => {}
                            // Porcelain reports `git add --intent-to-add` as a
                            // tracked entry with a worktree-side addition
                            // (` A`), never as `??` untracked.
                            EntryStatus::IntentToAdd => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                builder.worktree = GitChangeKindV1::Added;
                                builder.index_mode = Some(mode(entry.mode)?);
                                builder.worktree_mode =
                                    worktree_mode(self.worktree_root.as_deref(), &builder.path)?;
                            }
                            EntryStatus::Conflict { entries, .. } => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                builder.index = GitChangeKindV1::Unmerged;
                                builder.worktree = GitChangeKindV1::Unmerged;
                                builder.index_mode = entries
                                    .iter()
                                    .flatten()
                                    .next()
                                    .map(|entry| mode(entry.mode))
                                    .transpose()?;
                                builder.worktree_mode =
                                    worktree_mode(self.worktree_root.as_deref(), &builder.path)?;
                            }
                            EntryStatus::Change(change) => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                if builder.index_mode.is_none() {
                                    builder.index_mode = Some(mode(entry.mode)?);
                                }
                                if builder.head_mode.is_none() {
                                    builder.head_mode = Some(mode(entry.mode)?);
                                }
                                builder.submodule |= entry.mode.is_submodule();
                                match change {
                                    WorktreeChange::Removed => {
                                        builder.worktree = GitChangeKindV1::Deleted;
                                        builder.worktree_mode = None;
                                    }
                                    WorktreeChange::Type { worktree_mode } => {
                                        builder.worktree = GitChangeKindV1::TypeChanged;
                                        builder.worktree_mode = Some(mode(worktree_mode)?);
                                    }
                                    WorktreeChange::Modification { .. } => {
                                        builder.worktree = GitChangeKindV1::Modified;
                                        builder.worktree_mode = worktree_mode(
                                            self.worktree_root.as_deref(),
                                            &builder.path,
                                        )?;
                                    }
                                    WorktreeChange::SubmoduleModification(_) => {
                                        builder.worktree = GitChangeKindV1::Modified;
                                        builder.worktree_mode = Some(mode(entry.mode)?);
                                        builder.submodule = true;
                                    }
                                }
                            }
                        }
                    }
                    IndexWorktreeItem::DirectoryContents { entry, .. } => {
                        let path = path_text(entry.rela_path.as_ref(), "status")?;
                        match entry.status {
                            DirectoryStatus::Ignored(_) => {
                                loose.insert(path.clone(), GitStatusEntryV1::Ignored { path });
                            }
                            DirectoryStatus::Untracked => {
                                loose.insert(path.clone(), GitStatusEntryV1::Untracked { path });
                            }
                            DirectoryStatus::Pruned | DirectoryStatus::Tracked => {}
                        }
                    }
                    IndexWorktreeItem::Rewrite { .. } => {}
                },
            }
        }

        for path in tracked.keys() {
            loose.remove(path);
        }
        let mut entries = tracked
            .into_values()
            .map(TrackedStatusBuilder::finish)
            .map(GitStatusEntryV1::Tracked)
            .collect::<Vec<_>>();
        entries.extend(loose.into_values());
        entries.sort_by(|left, right| left.path().cmp(right.path()));

        let head = self.head()?;
        let op_state = self.operation_state();
        let mut degradations = self.degradations(&repository, &head, op_state);
        if entries
            .iter()
            .any(|entry| matches!(entry, GitStatusEntryV1::Tracked(value) if value.is_conflicted()))
        {
            degradations.insert(GitDegradationV1::ConflictedState);
        }
        if entries
            .iter()
            .any(|entry| matches!(entry, GitStatusEntryV1::Tracked(value) if value.submodule))
        {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }
        if has_ignored_collision(&entries) {
            degradations.insert(GitDegradationV1::IgnoredCollision);
        }
        Ok(GitRepositoryStatus {
            head,
            operation: op_state,
            entries,
            degradations,
        })
    }

    fn degradations(
        &self,
        repository: &gix::Repository,
        head: &GitHeadStateV1,
        operation: GitOperationStateV1,
    ) -> BTreeSet<GitDegradationV1> {
        let mut degradations = BTreeSet::new();
        match head {
            GitHeadStateV1::Detached { .. } => {
                degradations.insert(GitDegradationV1::DetachedHead);
            }
            GitHeadStateV1::Unborn { .. } => {
                degradations.insert(GitDegradationV1::UnbornBranch);
            }
            GitHeadStateV1::Attached { .. } => {}
        }
        if operation != GitOperationStateV1::None {
            degradations.insert(GitDegradationV1::InProgressOperation);
        }
        if repository
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
        {
            degradations.insert(GitDegradationV1::SparseCheckout);
        }
        if std::fs::read_dir(&self.git_dir).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            })
        }) {
            degradations.insert(GitDegradationV1::SplitIndex);
        }
        if self
            .worktree_root
            .as_ref()
            .is_some_and(|root| root.join(".gitmodules").is_file())
        {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }
        degradations
    }
}

fn head_matches_reference(head: GitHeadStateV1, reference: &str) -> bool {
    let GitHeadStateV1::Attached { branch, .. } = head else {
        return false;
    };
    branch == reference
        || reference
            .strip_prefix("refs/heads/")
            .is_some_and(|short| branch == short)
}

#[derive(Debug)]
struct TrackedStatusBuilder {
    path: String,
    original_path: Option<String>,
    index: GitChangeKindV1,
    worktree: GitChangeKindV1,
    head_mode: Option<GitFileModeV1>,
    index_mode: Option<GitFileModeV1>,
    worktree_mode: Option<GitFileModeV1>,
    submodule: bool,
}

impl TrackedStatusBuilder {
    fn new(path: String) -> Self {
        Self {
            path,
            original_path: None,
            index: GitChangeKindV1::Unmodified,
            worktree: GitChangeKindV1::Unmodified,
            head_mode: None,
            index_mode: None,
            worktree_mode: None,
            submodule: false,
        }
    }

    fn set_index(
        &mut self,
        change: GitChangeKindV1,
        head_mode: Option<GitFileModeV1>,
        index_mode: Option<GitFileModeV1>,
        original_path: Option<String>,
    ) {
        self.index = change;
        self.head_mode = head_mode;
        self.worktree_mode.clone_from(&index_mode);
        self.index_mode = index_mode;
        self.original_path = original_path;
        self.submodule = self
            .index_mode
            .as_ref()
            .or(self.head_mode.as_ref())
            .is_some_and(GitFileModeV1::is_submodule);
    }

    fn finish(self) -> GitTrackedStatusV1 {
        GitTrackedStatusV1 {
            path: self.path,
            original_path: self.original_path,
            index: self.index,
            worktree: self.worktree,
            head_mode: self.head_mode,
            index_mode: self.index_mode,
            worktree_mode: self.worktree_mode,
            submodule: self.submodule,
        }
    }
}

fn head_from_gix(repository: &gix::Repository) -> Result<GitHeadStateV1, GitRepositoryError> {
    let head = repository
        .head()
        .map_err(|error| GitRepositoryError::UnreadableHead {
            detail: error.to_string(),
        })?;
    let branch = head
        .referent_name()
        .and_then(|name| name.as_bstr().to_str().ok())
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    match (head.id(), branch) {
        (Some(commit), Some(branch)) => Ok(GitHeadStateV1::Attached {
            branch,
            commit: GitOidV1::new(commit.to_string())?,
        }),
        (Some(commit), None) => Ok(GitHeadStateV1::Detached {
            commit: GitOidV1::new(commit.to_string())?,
        }),
        (None, Some(branch)) => Ok(GitHeadStateV1::Unborn { branch }),
        (None, None) => Err(GitRepositoryError::UnreadableHead {
            detail: "HEAD has neither a commit nor a branch".to_owned(),
        }),
    }
}

fn repository_error(path: &Path, error: impl std::fmt::Display) -> GitRepositoryError {
    GitRepositoryError::UnreadableRepository {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

fn path_text(
    path: &gix::bstr::BStr,
    operation_name: &'static str,
) -> Result<String, GitRepositoryError> {
    path.to_str()
        .map(str::to_owned)
        .map_err(|error| GitRepositoryError::Operation {
            operation: operation_name,
            detail: error.to_string(),
        })
}

fn mode(mode: gix::index::entry::Mode) -> Result<GitFileModeV1, GitRepositoryError> {
    GitFileModeV1::new(format!("{:06o}", mode.bits())).map_err(Into::into)
}

fn worktree_mode(
    root: Option<&Path>,
    path: &str,
) -> Result<Option<GitFileModeV1>, GitRepositoryError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metadata = match std::fs::symlink_metadata(root.join(path)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation("status worktree mode", error)),
    };
    let value = if metadata.file_type().is_symlink() {
        GitFileModeV1::SYMLINK
    } else if metadata.is_dir() {
        GitFileModeV1::GITLINK
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o111 != 0 {
                GitFileModeV1::EXECUTABLE
            } else {
                GitFileModeV1::REGULAR
            }
        }
        #[cfg(not(unix))]
        {
            GitFileModeV1::REGULAR
        }
    };
    GitFileModeV1::new(value).map(Some).map_err(Into::into)
}

fn has_ignored_collision(entries: &[GitStatusEntryV1]) -> bool {
    let ignored = entries
        .iter()
        .filter_map(|entry| match entry {
            GitStatusEntryV1::Ignored { path } => Some(path.trim_end_matches('/')),
            _ => None,
        })
        .collect::<Vec<_>>();
    entries.iter().any(|entry| {
        let path = match entry {
            GitStatusEntryV1::Ignored { .. } => return false,
            _ => entry.path(),
        };
        ignored.iter().any(|ignored_path| {
            parent_dir(ignored_path) == parent_dir(path)
                || path.starts_with(&format!("{ignored_path}/"))
                || (!parent_dir(path).is_empty()
                    && ignored_path.starts_with(&format!("{}/", parent_dir(path))))
        })
    })
}

fn parent_dir(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn canonical(path: &Path, operation_name: &'static str) -> Result<PathBuf, GitRepositoryError> {
    path.canonicalize()
        .map_err(|error| operation(operation_name, error))
}

fn operation(operation: &'static str, error: impl std::fmt::Display) -> GitRepositoryError {
    GitRepositoryError::Operation {
        operation,
        detail: error.to_string(),
    }
}
