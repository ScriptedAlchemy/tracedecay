//! The daemon's inventory of retained memory and the one policy that frees it.
//!
//! Admission ([`super::ProcessResidentMemoryV1`]) bounds work while it runs;
//! this inventory bounds what outlives it. Every structure kept past the
//! request that built it registers an owner under its project and worktree.
//! The inventory never holds the memory itself: an owner reports what it holds
//! and when it was last used, and releases on request, so a registry entry can
//! never be the reference that keeps a retired generation alive.
//!
//! Scope end frees an owner: a worktree unused for [`RESIDENT_OWNER_IDLE_WINDOW_V1`]
//! gives back everything it holds and keeps serving from its disk-backed text
//! and graph artifacts. Pressure frees owners earlier, in
//! [`RESIDENT_OWNER_SHED_ORDER_V1`], and never the generation a worktree is
//! actively serving.
//!
//! Every release bumps a headroom epoch. Work refused for memory subscribes to
//! it, beside the pressure cell's epoch for ledger and measured headroom, and
//! retries then instead of waiting for an unrelated wake.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, WorktreeId};

use super::advance_headroom_epoch;

/// How long a worktree keeps its retained state after its last use.
///
/// Long enough that an agent working through a task never pays a re-decode
/// between tool calls; short enough that a project it moved away from gives
/// its memory back within one coffee break instead of at daemon exit.
pub const RESIDENT_OWNER_IDLE_WINDOW_V1: Duration = Duration::from_mins(10);

/// What one owner retains. Declaration order is the shed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidentOwnerKindV1 {
    /// Parse trees and extractions an increment kept for the files it
    /// re-extracted, so the next edit of one reparses only what changed.
    /// Released, that next edit parses the file from scratch.
    RetainedParses,
    /// Decoded generations other than the one a worktree serves, kept so
    /// pinned and branch reads do not re-decode. Always re-decodable.
    SupersededGeneration,
    /// The interactive catalog (name, file and import indices) built over a
    /// worktree's graph. Released, the next catalog read rebuilds it from the
    /// durable projection in the background.
    GraphCatalog,
    /// The decoded generation a worktree serves, with the derivations built
    /// from it (record index, test attribution). Released, the worktree keeps
    /// serving exact, lexical and graph reads from disk and re-decodes on the
    /// next read that needs the whole generation.
    DecodedGeneration,
    /// The native graph engine a worktree serves graph reads from. Released,
    /// its durable graph and verified head stay; the next graph read answers
    /// the typed warming state while the engine reopens in the background.
    GraphEngine,
    /// An open LSP session: the editor's unsaved documents and their parses.
    /// Nothing re-derives an unsaved buffer, so the inventory never releases
    /// it; the session's own lease ends it.
    Session,
}

/// Pressure releases owners in this order.
pub const RESIDENT_OWNER_SHED_ORDER_V1: [ResidentOwnerKindV1; 6] = [
    ResidentOwnerKindV1::RetainedParses,
    ResidentOwnerKindV1::SupersededGeneration,
    ResidentOwnerKindV1::GraphCatalog,
    ResidentOwnerKindV1::DecodedGeneration,
    ResidentOwnerKindV1::GraphEngine,
    ResidentOwnerKindV1::Session,
];

impl ResidentOwnerKindV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetainedParses => "retained_parses",
            Self::SupersededGeneration => "superseded_generation",
            Self::GraphCatalog => "graph_catalog",
            Self::DecodedGeneration => "decoded_generation",
            Self::GraphEngine => "graph_engine",
            Self::Session => "session",
        }
    }

    /// Whether the idle window alone releases this kind. An LSP session
    /// never: nothing re-derives an unsaved buffer. The catalog rebuilds
    /// from the durable projection the same way the engine reopens.
    #[must_use]
    pub const fn released_when_idle(self) -> bool {
        !matches!(self, Self::Session)
    }
}

/// What one owner's memory belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidentHoldingV1 {
    /// A decoded code generation, or state built over one.
    Generation(CodeGenerationId),
    /// An open LSP session, by its session id.
    Session(String),
    /// State the worktree keeps across its generations.
    Worktree,
}

impl ResidentHoldingV1 {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Generation(generation) => generation.as_str(),
            Self::Session(session) => session,
            Self::Worktree => "worktree",
        }
    }
}

/// Memory an owner references together with owners in other worktrees: the
/// decoded pages of one sealed content, held once however many linked
/// worktrees serve it.
#[derive(Clone, Debug)]
pub struct ResidentSharedContentV1 {
    pub digest: ManifestDigest,
    pub bytes: u64,
    /// The shared allocation itself. Owners are one row only when they
    /// reference the same allocation, never merely equal digests; the weak
    /// reference keeps its address from being reused while a report runs.
    pub allocation: Weak<dyn Any + Send + Sync>,
}

impl ResidentSharedContentV1 {
    fn same_allocation(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.allocation, &other.allocation)
    }
}

impl PartialEq for ResidentSharedContentV1 {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest && self.bytes == other.bytes && self.same_allocation(other)
    }
}

impl Eq for ResidentSharedContentV1 {}

/// Bytes an owner holds, as the owner knows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentOwnerBytesV1 {
    /// Summed from the capacities of the owner's own allocations.
    Measured(u64),
    /// The owner holds memory it cannot size; reported, never guessed.
    Unmeasured,
}

impl ResidentOwnerBytesV1 {
    #[must_use]
    pub const fn measured(self) -> Option<u64> {
        match self {
            Self::Measured(bytes) => Some(bytes),
            Self::Unmeasured => None,
        }
    }
}

/// One owner's current holding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentOwnerSampleV1 {
    pub holding: ResidentHoldingV1,
    /// What this owner holds by itself, excluding `shared`.
    pub bytes: ResidentOwnerBytesV1,
    pub last_used: Instant,
    /// The held generation is the one the worktree currently serves.
    pub serving: bool,
    /// Content this owner references with owners of other worktrees.
    pub shared: Option<ResidentSharedContentV1>,
}

/// Outcome of asking an owner to release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentOwnerReleaseV1 {
    Released {
        bytes: ResidentOwnerBytesV1,
    },
    /// Work that needs the held state is running; ask again later.
    Busy,
    /// Nothing was held.
    Empty,
}

/// Retained state the inventory can observe and free.
pub trait ResidentOwnerV1: Send + Sync {
    /// What is held now, or `None` when nothing is.
    fn sample(&self) -> Option<ResidentOwnerSampleV1>;
    /// Drop what is held. Must not block on long-running work.
    fn release(&self) -> ResidentOwnerReleaseV1;
}

/// The project and worktree an owner belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResidentOwnerScopeV1 {
    pub project_id: ProjectId,
    pub worktree_id: WorktreeId,
}

/// Why an owner was released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentOwnerReleaseCauseV1 {
    Idle,
    Pressure,
}

/// One release the inventory performed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentOwnerReleasedV1 {
    pub scope: ResidentOwnerScopeV1,
    pub kind: ResidentOwnerKindV1,
    pub holding: ResidentHoldingV1,
    pub bytes: ResidentOwnerBytesV1,
    pub cause: ResidentOwnerReleaseCauseV1,
}

/// One worktree holding a report row's memory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResidentOwnerHolderV1 {
    pub worktree_id: WorktreeId,
    pub holding: ResidentHoldingV1,
}

/// One row of the public report: the memory one owner holds, or one shared
/// content with every worktree referencing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentOwnerReportRowV1 {
    pub project_id: ProjectId,
    pub kind: ResidentOwnerKindV1,
    /// Ordered by worktree; more than one only when they share `content_digest`.
    pub holders: Vec<ResidentOwnerHolderV1>,
    pub content_digest: Option<ManifestDigest>,
    /// The shared content once plus what each holder holds by itself.
    pub bytes: ResidentOwnerBytesV1,
    /// Since the most recent holder's last use.
    pub idle_for: Duration,
    /// Pressure will not release some holder: it holds the generation its
    /// worktree serves and the worktree is inside its idle window.
    pub protected: bool,
}

/// Everything the inventory holds, as one report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentOwnersReportV1 {
    pub idle_window: Duration,
    pub owners: Vec<ResidentOwnerReportRowV1>,
    pub measured_bytes: u64,
    pub unmeasured_owners: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident owner registration sequence exhausted")]
pub struct ResidentOwnerRegistrationFailureV1;

struct OwnerEntryV1 {
    scope: ResidentOwnerScopeV1,
    kind: ResidentOwnerKindV1,
    owner: Weak<dyn ResidentOwnerV1>,
}

#[derive(Default)]
struct OwnersStateV1 {
    owners: BTreeMap<u64, OwnerEntryV1>,
    next_sequence: u64,
}

struct LiveOwnerV1 {
    scope: ResidentOwnerScopeV1,
    kind: ResidentOwnerKindV1,
    owner: Arc<dyn ResidentOwnerV1>,
    sample: ResidentOwnerSampleV1,
}

/// The process inventory of retained owners.
pub struct ResidentOwnersV1 {
    idle_window: Duration,
    state: Mutex<OwnersStateV1>,
    headroom: watch::Sender<u64>,
}

impl fmt::Debug for ResidentOwnersV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentOwnersV1")
            .field("idle_window", &self.idle_window)
            .finish_non_exhaustive()
    }
}

impl ResidentOwnersV1 {
    #[must_use]
    pub fn new(idle_window: Duration) -> Self {
        Self {
            idle_window,
            state: Mutex::new(OwnersStateV1::default()),
            headroom: watch::Sender::new(0),
        }
    }

    /// Changes each time an owner's memory is given back.
    #[must_use]
    pub fn subscribe_headroom(&self) -> watch::Receiver<u64> {
        self.headroom.subscribe()
    }

    /// Record that memory was given back outside this inventory.
    pub fn note_headroom(&self) {
        advance_headroom_epoch(&self.headroom);
    }

    fn note_released(
        &self,
        released: Vec<ResidentOwnerReleasedV1>,
    ) -> Vec<ResidentOwnerReleasedV1> {
        if !released.is_empty() {
            self.note_headroom();
        }
        released
    }

    #[must_use]
    pub const fn idle_window(&self) -> Duration {
        self.idle_window
    }

    /// Register an owner. The registration unregisters on drop, so an owner
    /// that is itself dropped leaves no row behind.
    pub fn register(
        self: &Arc<Self>,
        scope: ResidentOwnerScopeV1,
        kind: ResidentOwnerKindV1,
        owner: Weak<dyn ResidentOwnerV1>,
    ) -> Result<ResidentOwnerRegistrationV1, ResidentOwnerRegistrationFailureV1> {
        let mut state = self.lock_state();
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .ok_or(ResidentOwnerRegistrationFailureV1)?;
        state
            .owners
            .insert(sequence, OwnerEntryV1 { scope, kind, owner });
        Ok(ResidentOwnerRegistrationV1 {
            owners: Arc::downgrade(self),
            sequence,
        })
    }

    /// Release every owner of a kind [released when
    /// idle](ResidentOwnerKindV1::released_when_idle) whose last use is older
    /// than the idle window.
    pub fn release_idle(&self, now: Instant) -> Vec<ResidentOwnerReleasedV1> {
        let released = self
            .live_owners()
            .into_iter()
            .filter(|live| live.kind.released_when_idle() && self.idle(&live.sample, now))
            .filter_map(|live| Self::release_one(live, ResidentOwnerReleaseCauseV1::Idle))
            .collect();
        self.note_released(released)
    }

    /// Release unprotected owners in shed order, least recently used first
    /// within one kind, until at least `excess_bytes` measured bytes are
    /// freed. An unmeasured release counts as progress but frees no bytes, so
    /// shedding continues past it.
    pub fn shed(&self, excess_bytes: u64, now: Instant) -> Vec<ResidentOwnerReleasedV1> {
        let mut candidates = self
            .live_owners()
            .into_iter()
            .filter(|live| !self.protected(&live.sample, now))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|live| (live.kind, live.sample.last_used));
        let mut released = Vec::new();
        let mut freed = 0_u64;
        for live in candidates {
            if freed >= excess_bytes && !released.is_empty() {
                break;
            }
            if let Some(release) = Self::release_one(live, ResidentOwnerReleaseCauseV1::Pressure) {
                freed = freed.saturating_add(release.bytes.measured().unwrap_or(0));
                released.push(release);
            }
        }
        self.note_released(released)
    }

    /// Release the first shed tier that holds an unprotected owner. Used when
    /// the kernel reports memory stalls but RSS gives no byte target.
    pub fn shed_one_tier(&self, now: Instant) -> Vec<ResidentOwnerReleasedV1> {
        let candidates = self
            .live_owners()
            .into_iter()
            .filter(|live| !self.protected(&live.sample, now))
            .collect::<Vec<_>>();
        let Some(tier) = candidates.iter().map(|live| live.kind).min() else {
            return Vec::new();
        };
        let released = candidates
            .into_iter()
            .filter(|live| live.kind == tier)
            .filter_map(|live| Self::release_one(live, ResidentOwnerReleaseCauseV1::Pressure))
            .collect();
        self.note_released(released)
    }

    /// Every live owner as one row, except that owners of one project and
    /// kind referencing the same shared content form a single row listing
    /// each worktree, and the content counts once in the row and the total.
    #[must_use]
    pub fn report(&self, now: Instant) -> ResidentOwnersReportV1 {
        let mut owners: Vec<ResidentOwnerReportRowV1> = Vec::new();
        let mut row_contents: Vec<Option<ResidentSharedContentV1>> = Vec::new();
        let mut contents: Vec<ResidentSharedContentV1> = Vec::new();
        let mut own_measured = 0_u64;
        let mut unmeasured_owners = 0_usize;
        for live in self.live_owners() {
            let protected = self.protected(&live.sample, now);
            let idle_for = now.saturating_duration_since(live.sample.last_used);
            match live.sample.bytes {
                ResidentOwnerBytesV1::Measured(bytes) => {
                    own_measured = own_measured.saturating_add(bytes);
                }
                ResidentOwnerBytesV1::Unmeasured => unmeasured_owners += 1,
            }
            let holder = ResidentOwnerHolderV1 {
                worktree_id: live.scope.worktree_id,
                holding: live.sample.holding,
            };
            let shared = live.sample.shared;
            if let Some(content) = &shared
                && !contents.iter().any(|seen| seen.same_allocation(content))
            {
                contents.push(content.clone());
            }
            let existing = shared.as_ref().and_then(|content| {
                owners
                    .iter()
                    .zip(&row_contents)
                    .position(|(row, row_content)| {
                        row.project_id == live.scope.project_id
                            && row.kind == live.kind
                            && row_content
                                .as_ref()
                                .is_some_and(|row_content| row_content.same_allocation(content))
                    })
            });
            if let Some(row) = existing.and_then(|index| owners.get_mut(index)) {
                row.holders.push(holder);
                row.bytes = add_owner_bytes(row.bytes, live.sample.bytes);
                row.idle_for = row.idle_for.min(idle_for);
                row.protected |= protected;
                continue;
            }
            let shared_bytes = shared.as_ref().map_or(0, |content| content.bytes);
            owners.push(ResidentOwnerReportRowV1 {
                project_id: live.scope.project_id,
                kind: live.kind,
                holders: vec![holder],
                content_digest: shared.as_ref().map(|content| content.digest.clone()),
                bytes: add_owner_bytes(
                    ResidentOwnerBytesV1::Measured(shared_bytes),
                    live.sample.bytes,
                ),
                idle_for,
                protected,
            });
            row_contents.push(shared);
        }
        for row in &mut owners {
            row.holders.sort();
        }
        owners.sort_by(|left, right| {
            (&left.project_id, left.kind, &left.holders).cmp(&(
                &right.project_id,
                right.kind,
                &right.holders,
            ))
        });
        let measured_bytes = contents.iter().fold(own_measured, |total, content| {
            total.saturating_add(content.bytes)
        });
        ResidentOwnersReportV1 {
            idle_window: self.idle_window,
            owners,
            measured_bytes,
            unmeasured_owners,
        }
    }

    fn idle(&self, sample: &ResidentOwnerSampleV1, now: Instant) -> bool {
        now.saturating_duration_since(sample.last_used) >= self.idle_window
    }

    fn protected(&self, sample: &ResidentOwnerSampleV1, now: Instant) -> bool {
        sample.serving && !self.idle(sample, now)
    }

    fn release_one(
        live: LiveOwnerV1,
        cause: ResidentOwnerReleaseCauseV1,
    ) -> Option<ResidentOwnerReleasedV1> {
        match live.owner.release() {
            ResidentOwnerReleaseV1::Released { bytes } => Some(ResidentOwnerReleasedV1 {
                scope: live.scope,
                kind: live.kind,
                holding: live.sample.holding,
                bytes,
                cause,
            }),
            ResidentOwnerReleaseV1::Busy | ResidentOwnerReleaseV1::Empty => None,
        }
    }

    /// Owners that are alive and hold something, sampled without the
    /// inventory lock so an owner's own locks never nest inside it.
    fn live_owners(&self) -> Vec<LiveOwnerV1> {
        let entries = {
            let state = self.lock_state();
            state
                .owners
                .values()
                .filter_map(|entry| {
                    entry
                        .owner
                        .upgrade()
                        .map(|owner| (entry.scope.clone(), entry.kind, owner))
                })
                .collect::<Vec<_>>()
        };
        entries
            .into_iter()
            .filter_map(|(scope, kind, owner)| {
                owner.sample().map(|sample| LiveOwnerV1 {
                    scope,
                    kind,
                    owner,
                    sample,
                })
            })
            .collect()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, OwnersStateV1> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn add_owner_bytes(
    left: ResidentOwnerBytesV1,
    right: ResidentOwnerBytesV1,
) -> ResidentOwnerBytesV1 {
    match (left, right) {
        (ResidentOwnerBytesV1::Measured(left), ResidentOwnerBytesV1::Measured(right)) => {
            ResidentOwnerBytesV1::Measured(left.saturating_add(right))
        }
        _ => ResidentOwnerBytesV1::Unmeasured,
    }
}

/// Keeps one owner in the inventory; dropping it removes the row.
pub struct ResidentOwnerRegistrationV1 {
    owners: Weak<ResidentOwnersV1>,
    sequence: u64,
}

impl fmt::Debug for ResidentOwnerRegistrationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentOwnerRegistrationV1")
            .field("sequence", &self.sequence)
            .finish()
    }
}

impl Drop for ResidentOwnerRegistrationV1 {
    fn drop(&mut self) {
        if let Some(owners) = self.owners.upgrade() {
            owners.lock_state().owners.remove(&self.sequence);
        }
    }
}

static PROCESS_RESIDENT_OWNERS_V1: OnceLock<Arc<ResidentOwnersV1>> = OnceLock::new();

/// The process inventory. Retained state is a process fact like RSS, so the
/// inventory is a singleton the same way the pressure cell is; tests build
/// their own [`ResidentOwnersV1`] and inject it.
#[must_use]
pub fn process_resident_owners_v1() -> &'static Arc<ResidentOwnersV1> {
    PROCESS_RESIDENT_OWNERS_V1
        .get_or_init(|| Arc::new(ResidentOwnersV1::new(RESIDENT_OWNER_IDLE_WINDOW_V1)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct FixtureOwner {
        generation: &'static str,
        bytes: u64,
        last_used: Instant,
        serving: bool,
        shared: Option<ResidentSharedContentV1>,
        held: AtomicBool,
    }

    /// One shared allocation several fixture owners reference.
    fn content(digit: &str, bytes: u64) -> (Arc<dyn Any + Send + Sync>, ResidentSharedContentV1) {
        let allocation: Arc<dyn Any + Send + Sync> = Arc::new(());
        let shared = ResidentSharedContentV1 {
            digest: ManifestDigest::new(format!("sha256:{}", digit.repeat(64))).unwrap(),
            bytes,
            allocation: Arc::downgrade(&allocation),
        };
        (allocation, shared)
    }

    impl FixtureOwner {
        fn new(
            generation: &'static str,
            bytes: u64,
            last_used: Instant,
            serving: bool,
        ) -> Arc<Self> {
            Arc::new(Self {
                generation,
                bytes,
                last_used,
                serving,
                shared: None,
                held: AtomicBool::new(true),
            })
        }

        fn sharing(
            generation: &'static str,
            bytes: u64,
            content: &ResidentSharedContentV1,
            last_used: Instant,
        ) -> Arc<Self> {
            Arc::new(Self {
                generation,
                bytes,
                last_used,
                serving: true,
                shared: Some(content.clone()),
                held: AtomicBool::new(true),
            })
        }
    }

    impl ResidentOwnerV1 for FixtureOwner {
        fn sample(&self) -> Option<ResidentOwnerSampleV1> {
            self.held
                .load(Ordering::Acquire)
                .then(|| ResidentOwnerSampleV1 {
                    holding: ResidentHoldingV1::Generation(
                        CodeGenerationId::new(self.generation).unwrap(),
                    ),
                    bytes: ResidentOwnerBytesV1::Measured(self.bytes),
                    last_used: self.last_used,
                    serving: self.serving,
                    shared: self.shared.clone(),
                })
        }

        fn release(&self) -> ResidentOwnerReleaseV1 {
            if self.held.swap(false, Ordering::AcqRel) {
                ResidentOwnerReleaseV1::Released {
                    bytes: ResidentOwnerBytesV1::Measured(self.bytes),
                }
            } else {
                ResidentOwnerReleaseV1::Empty
            }
        }
    }

    fn scope(worktree: &str) -> ResidentOwnerScopeV1 {
        ResidentOwnerScopeV1 {
            project_id: ProjectId::new("project.fixture").unwrap(),
            worktree_id: WorktreeId::new(worktree).unwrap(),
        }
    }

    fn register(
        owners: &Arc<ResidentOwnersV1>,
        worktree: &str,
        kind: ResidentOwnerKindV1,
        owner: &Arc<FixtureOwner>,
    ) -> ResidentOwnerRegistrationV1 {
        let owner: Arc<dyn ResidentOwnerV1> = Arc::clone(owner) as Arc<dyn ResidentOwnerV1>;
        owners
            .register(scope(worktree), kind, Arc::downgrade(&owner))
            .unwrap()
    }

    fn released_generations(released: &[ResidentOwnerReleasedV1]) -> Vec<&str> {
        released
            .iter()
            .map(|release| release.holding.as_str())
            .collect()
    }

    fn row_generations(report: &ResidentOwnersReportV1) -> Vec<(&str, bool)> {
        report
            .owners
            .iter()
            .flat_map(|row| {
                row.holders
                    .iter()
                    .map(|holder| (holder.holding.as_str(), row.protected))
            })
            .collect()
    }

    #[test]
    fn pressure_sheds_in_the_documented_order_and_never_the_active_serving_generation() {
        let start = Instant::now();
        let now = start + Duration::from_mins(10);
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let active_serving = FixtureOwner::new("generation.active", 4_000, now, true);
        let idle_serving = FixtureOwner::new("generation.idle", 3_000, start, true);
        let superseded_old = FixtureOwner::new("generation.old", 1_000, start, false);
        let superseded_recent = FixtureOwner::new("generation.recent", 2_000, now, false);
        let _registrations = [
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::DecodedGeneration,
                &active_serving,
            ),
            register(
                &owners,
                "worktree.b",
                ResidentOwnerKindV1::DecodedGeneration,
                &idle_serving,
            ),
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::SupersededGeneration,
                &superseded_recent,
            ),
            register(
                &owners,
                "worktree.b",
                ResidentOwnerKindV1::SupersededGeneration,
                &superseded_old,
            ),
        ];

        let released = owners.shed(u64::MAX, now);

        assert_eq!(
            released_generations(&released),
            ["generation.old", "generation.recent", "generation.idle"]
        );
        assert_eq!(
            row_generations(&owners.report(now)),
            [("generation.active", true)]
        );
    }

    #[test]
    fn crossing_memory_high_sheds_through_the_pressure_cell_in_order() {
        use super::super::{
            ResidentMemoryPressureV1, register_resident_owners_pressure_reclaimer_v1,
        };
        let pressure = Arc::new(ResidentMemoryPressureV1::with_reclaim_line(
            std::num::NonZeroU64::new(10_000).unwrap(),
            Some(6_000),
            Arc::new(|| None),
        ));
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let _reclaimer =
            register_resident_owners_pressure_reclaimer_v1(&pressure, &owners).unwrap();
        let now = Instant::now();
        let serving = FixtureOwner::new("generation.serving", 4_000, now, true);
        let superseded = FixtureOwner::new("generation.superseded", 1_500, now, false);
        let _registrations = [
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::DecodedGeneration,
                &serving,
            ),
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::SupersededGeneration,
                &superseded,
            ),
        ];

        pressure.publish_observed_resident_bytes(5_900);
        assert_eq!(
            owners.report(now).measured_bytes,
            5_500,
            "below memory.high nothing sheds"
        );

        pressure.publish_observed_resident_bytes(7_000);
        assert_eq!(
            row_generations(&owners.report(now)),
            [("generation.serving", true)],
            "memory.high sheds the superseded decode first and keeps the serving one"
        );

        pressure.publish_observed_resident_bytes(9_500);
        assert_eq!(
            owners.report(now).measured_bytes,
            4_000,
            "even far past memory.high the actively serving generation is never shed"
        );
    }

    #[test]
    fn shedding_stops_once_the_excess_is_freed() {
        let now = Instant::now();
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let first = FixtureOwner::new("generation.first", 1_500, now, false);
        let second = FixtureOwner::new(
            "generation.second",
            1_500,
            now + Duration::from_secs(1),
            false,
        );
        let _registrations = [
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::SupersededGeneration,
                &first,
            ),
            register(
                &owners,
                "worktree.a",
                ResidentOwnerKindV1::SupersededGeneration,
                &second,
            ),
        ];

        let released = owners.shed(1_000, now + Duration::from_secs(2));

        assert_eq!(released_generations(&released), ["generation.first"]);
        assert_eq!(owners.report(now).measured_bytes, 1_500);
    }

    #[test]
    fn idle_release_frees_only_owners_past_the_window() {
        let start = Instant::now();
        let now = start + Duration::from_secs(301);
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let idle = FixtureOwner::new("generation.idle", 3_000, start, true);
        let recent = FixtureOwner::new(
            "generation.recent",
            4_000,
            start + Duration::from_secs(2),
            true,
        );
        let _registrations = [
            register(
                &owners,
                "worktree.idle",
                ResidentOwnerKindV1::DecodedGeneration,
                &idle,
            ),
            register(
                &owners,
                "worktree.recent",
                ResidentOwnerKindV1::DecodedGeneration,
                &recent,
            ),
        ];

        assert_eq!(owners.report(now).measured_bytes, 7_000);
        let released = owners.release_idle(now);

        assert_eq!(released_generations(&released), ["generation.idle"]);
        assert_eq!(released[0].cause, ResidentOwnerReleaseCauseV1::Idle);
        assert_eq!(owners.report(now).measured_bytes, 4_000);
    }

    #[test]
    fn the_idle_window_releases_idle_graph_catalogs_and_keeps_recent_ones() {
        const CATALOG_MIB: u64 = 12 * 1024 * 1024;
        const ENGINE_MIB: u64 = 2 * 1024 * 1024;
        let start = Instant::now();
        let now = start + Duration::from_secs(301);
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let idle_catalog = FixtureOwner::new("generation.idle-catalog", CATALOG_MIB, start, true);
        let idle_engine = FixtureOwner::new("generation.idle-engine", ENGINE_MIB, start, true);
        let recent_catalog = FixtureOwner::new(
            "generation.recent-catalog",
            CATALOG_MIB,
            start + Duration::from_secs(2),
            true,
        );
        let recent_engine = FixtureOwner::new(
            "generation.recent-engine",
            ENGINE_MIB,
            start + Duration::from_secs(2),
            true,
        );
        let _registrations = [
            register(
                &owners,
                "worktree.idle",
                ResidentOwnerKindV1::GraphCatalog,
                &idle_catalog,
            ),
            register(
                &owners,
                "worktree.idle",
                ResidentOwnerKindV1::GraphEngine,
                &idle_engine,
            ),
            register(
                &owners,
                "worktree.recent",
                ResidentOwnerKindV1::GraphCatalog,
                &recent_catalog,
            ),
            register(
                &owners,
                "worktree.recent",
                ResidentOwnerKindV1::GraphEngine,
                &recent_engine,
            ),
        ];

        let before = owners.report(now).measured_bytes;
        let released = owners.release_idle(now);
        let after = owners.report(now).measured_bytes;
        let mut kinds = released
            .iter()
            .map(|release| release.kind)
            .collect::<Vec<_>>();
        kinds.sort();
        eprintln!(
            "STEADY_RSS_PROOF before={before} after={after} \
             released_catalog={CATALOG_MIB} released_engine={ENGINE_MIB} worktrees=2"
        );
        assert_eq!(before, 2 * (CATALOG_MIB + ENGINE_MIB));
        assert_eq!(
            kinds,
            [
                ResidentOwnerKindV1::GraphCatalog,
                ResidentOwnerKindV1::GraphEngine
            ]
        );
        assert_eq!(
            released_generations(&released),
            ["generation.idle-catalog", "generation.idle-engine"]
        );
        assert_eq!(after, CATALOG_MIB + ENGINE_MIB);
        assert_eq!(
            released
                .iter()
                .map(|release| release.bytes.measured().unwrap_or(0))
                .sum::<u64>(),
            CATALOG_MIB + ENGINE_MIB
        );
    }

    #[test]
    fn the_idle_window_keeps_an_lsp_session() {
        let start = Instant::now();
        let now = start + Duration::from_secs(301);
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let session = FixtureOwner::new("generation.session", 8_000, start, true);
        let _registration = register(
            &owners,
            "worktree.a",
            ResidentOwnerKindV1::Session,
            &session,
        );

        assert_eq!(owners.release_idle(now).len(), 0);
        assert_eq!(owners.report(now).measured_bytes, 8_000);
    }

    #[test]
    fn only_a_release_moves_the_headroom_epoch() {
        let start = Instant::now();
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let mut headroom = owners.subscribe_headroom();
        let owner = FixtureOwner::new("generation.idle", 3_000, start, false);
        let _registration = register(
            &owners,
            "worktree.a",
            ResidentOwnerKindV1::DecodedGeneration,
            &owner,
        );

        assert_eq!(owners.release_idle(start).len(), 0);
        assert!(
            !headroom.has_changed().unwrap(),
            "a sweep that frees nothing"
        );

        assert_eq!(
            owners.release_idle(start + Duration::from_secs(301)).len(),
            1
        );
        assert!(headroom.has_changed().unwrap());
        assert_eq!(*headroom.borrow_and_update(), 1);

        owners.note_headroom();
        assert_eq!(*headroom.borrow_and_update(), 2);
    }

    #[test]
    fn worktrees_sharing_one_content_report_one_row_that_counts_it_once() {
        let start = Instant::now();
        let now = start + Duration::from_mins(1);
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let (_shared, shared) = content("a", 9_000);
        // Equal digest, separate allocation: two copies, never one row.
        let (_copy, copy) = content("a", 9_000);
        let (_other, other_content) = content("b", 4_000);
        let first = FixtureOwner::sharing("generation.first", 100, &shared, start);
        let second = FixtureOwner::sharing(
            "generation.second",
            150,
            &shared,
            start + Duration::from_secs(30),
        );
        let copied = FixtureOwner::sharing("generation.copied", 70, &copy, start);
        let other = FixtureOwner::sharing("generation.other", 50, &other_content, start);
        let _registrations = [
            register(
                &owners,
                "worktree.second",
                ResidentOwnerKindV1::DecodedGeneration,
                &second,
            ),
            register(
                &owners,
                "worktree.first",
                ResidentOwnerKindV1::DecodedGeneration,
                &first,
            ),
            register(
                &owners,
                "worktree.other",
                ResidentOwnerKindV1::DecodedGeneration,
                &other,
            ),
            register(
                &owners,
                "worktree.copied",
                ResidentOwnerKindV1::DecodedGeneration,
                &copied,
            ),
        ];

        let report = owners.report(now);

        assert_eq!(
            report
                .owners
                .iter()
                .map(|row| (
                    row.holders
                        .iter()
                        .map(|holder| (holder.worktree_id.as_str(), holder.holding.as_str()))
                        .collect::<Vec<_>>(),
                    row.bytes.measured(),
                    row.idle_for,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    vec![("worktree.copied", "generation.copied")],
                    Some(9_070),
                    Duration::from_mins(1),
                ),
                (
                    vec![
                        ("worktree.first", "generation.first"),
                        ("worktree.second", "generation.second"),
                    ],
                    Some(9_250),
                    Duration::from_secs(30),
                ),
                (
                    vec![("worktree.other", "generation.other")],
                    Some(4_050),
                    Duration::from_mins(1),
                ),
            ]
        );
        assert_eq!(report.measured_bytes, 22_370);
    }

    #[test]
    fn a_dropped_registration_or_owner_leaves_no_row() {
        let now = Instant::now();
        let owners = Arc::new(ResidentOwnersV1::new(Duration::from_mins(5)));
        let kept = FixtureOwner::new("generation.kept", 10, now, true);
        let dropped = FixtureOwner::new("generation.dropped", 20, now, true);
        let _kept = register(
            &owners,
            "worktree.a",
            ResidentOwnerKindV1::DecodedGeneration,
            &kept,
        );
        let dropped_registration = register(
            &owners,
            "worktree.b",
            ResidentOwnerKindV1::DecodedGeneration,
            &dropped,
        );
        assert_eq!(owners.report(now).measured_bytes, 30);

        drop(dropped_registration);

        assert_eq!(owners.report(now).measured_bytes, 10);
    }
}
