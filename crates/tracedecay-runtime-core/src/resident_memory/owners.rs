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

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

/// How long a worktree keeps its retained state after its last use.
///
/// Long enough that an agent working through a task never pays a re-decode
/// between tool calls; short enough that a project it moved away from gives
/// its memory back within one coffee break instead of at daemon exit.
pub const RESIDENT_OWNER_IDLE_WINDOW_V1: Duration = Duration::from_mins(10);

/// What one owner retains. Declaration order is the shed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidentOwnerKindV1 {
    /// Decoded generations other than the one a worktree serves, kept so
    /// pinned and branch reads do not re-decode. Always re-decodable.
    SupersededGeneration,
    /// The decoded generation a worktree serves, with the derivations built
    /// from it (record index, test attribution). Released, the worktree keeps
    /// serving exact, lexical and graph reads from disk and re-decodes on the
    /// next read that needs the whole generation.
    DecodedGeneration,
    /// The native graph engine a worktree serves graph reads from. Released,
    /// its durable graph and verified head stay; the next graph read answers
    /// the typed warming state while the engine reopens in the background.
    GraphEngine,
}

/// Pressure releases owners in this order.
pub const RESIDENT_OWNER_SHED_ORDER_V1: [ResidentOwnerKindV1; 3] = [
    ResidentOwnerKindV1::SupersededGeneration,
    ResidentOwnerKindV1::DecodedGeneration,
    ResidentOwnerKindV1::GraphEngine,
];

impl ResidentOwnerKindV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SupersededGeneration => "superseded_generation",
            Self::DecodedGeneration => "decoded_generation",
            Self::GraphEngine => "graph_engine",
        }
    }
}

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
    pub generation_id: CodeGenerationId,
    pub bytes: ResidentOwnerBytesV1,
    pub last_used: Instant,
    /// The held generation is the one the worktree currently serves.
    pub serving: bool,
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
    pub generation_id: CodeGenerationId,
    pub bytes: ResidentOwnerBytesV1,
    pub cause: ResidentOwnerReleaseCauseV1,
}

/// One owner row of the public report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentOwnerReportRowV1 {
    pub scope: ResidentOwnerScopeV1,
    pub kind: ResidentOwnerKindV1,
    pub generation_id: CodeGenerationId,
    pub bytes: ResidentOwnerBytesV1,
    pub idle_for: Duration,
    /// Pressure will not release this owner: it holds the generation its
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
        }
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

    /// Release every owner whose last use is older than the idle window.
    pub fn release_idle(&self, now: Instant) -> Vec<ResidentOwnerReleasedV1> {
        self.live_owners()
            .into_iter()
            .filter(|live| self.idle(&live.sample, now))
            .filter_map(|live| Self::release_one(live, ResidentOwnerReleaseCauseV1::Idle))
            .collect()
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
        released
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
        candidates
            .into_iter()
            .filter(|live| live.kind == tier)
            .filter_map(|live| Self::release_one(live, ResidentOwnerReleaseCauseV1::Pressure))
            .collect()
    }

    #[must_use]
    pub fn report(&self, now: Instant) -> ResidentOwnersReportV1 {
        let mut owners = self
            .live_owners()
            .into_iter()
            .map(|live| ResidentOwnerReportRowV1 {
                protected: self.protected(&live.sample, now),
                idle_for: now.saturating_duration_since(live.sample.last_used),
                scope: live.scope,
                kind: live.kind,
                generation_id: live.sample.generation_id,
                bytes: live.sample.bytes,
            })
            .collect::<Vec<_>>();
        owners.sort_by(|left, right| {
            (&left.scope, left.kind, &left.generation_id).cmp(&(
                &right.scope,
                right.kind,
                &right.generation_id,
            ))
        });
        let measured_bytes = owners
            .iter()
            .filter_map(|row| row.bytes.measured())
            .fold(0_u64, u64::saturating_add);
        let unmeasured_owners = owners
            .iter()
            .filter(|row| row.bytes.measured().is_none())
            .count();
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
                generation_id: live.sample.generation_id,
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
        held: AtomicBool,
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
                held: AtomicBool::new(true),
            })
        }
    }

    impl ResidentOwnerV1 for FixtureOwner {
        fn sample(&self) -> Option<ResidentOwnerSampleV1> {
            self.held
                .load(Ordering::Acquire)
                .then(|| ResidentOwnerSampleV1 {
                    generation_id: CodeGenerationId::new(self.generation).unwrap(),
                    bytes: ResidentOwnerBytesV1::Measured(self.bytes),
                    last_used: self.last_used,
                    serving: self.serving,
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
            .map(|release| release.generation_id.as_str())
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
            owners
                .report(now)
                .owners
                .iter()
                .map(|row| (row.generation_id.as_str(), row.protected))
                .collect::<Vec<_>>(),
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
            owners
                .report(now)
                .owners
                .iter()
                .map(|row| row.generation_id.as_str())
                .collect::<Vec<_>>(),
            ["generation.serving"],
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
