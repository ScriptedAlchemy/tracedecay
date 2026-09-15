//! Process-wide admission for structurally measured resident allocations.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use sysinfo::{MemoryRefreshKind, RefreshKind, System};
use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

use crate::profiled_lock::{ProfiledMutex, ProfiledMutexGuard};

/// Conservative fallback when the host cannot report physical memory.
///
/// Production normally derives the authority from the machine. This fallback
/// is not a project-size ceiling: the authority governs concurrently live
/// resident allocations, while project data must remain paged or durable.
pub const DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1: NonZeroU64 =
    NonZeroU64::MIN.saturating_add(6 * 1024 * 1024 * 1024 - 1);

/// Environment override for the process resident-memory admission limit, in
/// bytes. Unset, unparseable, or zero values fall back to the RAM-derived
/// authority. The code-index worker pool derives its reservation from this
/// same limit, so raising it can both admit and widen indexing, up to any
/// finite cgroup-v2 memory ceiling.
pub const PROCESS_RESIDENT_MEMORY_LIMIT_ENV_V1: &str = "TRACEDECAY_RESIDENT_MEMORY_LIMIT_BYTES";

const PROC_SELF_CGROUP_V1: &str = "/proc/self/cgroup";
const CGROUP_V2_ROOT_V1: &str = "/sys/fs/cgroup";

/// Derive the concurrent resident-allocation authority for a known host size.
#[must_use]
pub fn process_resident_memory_limit_for_system_v1(total_memory_bytes: u64) -> NonZeroU64 {
    NonZeroU64::new(total_memory_bytes.saturating_sub(total_memory_bytes / 4))
        .unwrap_or(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1)
}

/// Read the operator override for the resident-allocation authority.
///
/// Unset, unparseable, and zero values yield `None` so the caller keeps the
/// RAM-derived authority.
#[must_use]
fn process_resident_memory_limit_override_v1() -> Option<NonZeroU64> {
    std::env::var(PROCESS_RESIDENT_MEMORY_LIMIT_ENV_V1)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(NonZeroU64::new)
}

fn cgroup_v2_process_directory_v1(
    proc_self_cgroup: &Path,
    cgroup_root: &Path,
) -> Option<std::path::PathBuf> {
    let membership = std::fs::read_to_string(proc_self_cgroup).ok()?;
    let relative = membership.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        if hierarchy != "0" || !controllers.is_empty() {
            return None;
        }
        Path::new(path)
            .strip_prefix("/")
            .ok()
            .map(Path::to_path_buf)
    })?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    Some(cgroup_root.join(relative))
}

fn finite_cgroup_memory_value_v1(path: &Path) -> Option<u64> {
    let value = std::fs::read_to_string(path).ok()?;
    let value = value.trim();
    if value == "max" {
        return None;
    }
    value.parse::<u64>().ok().map(|value| value.max(1))
}

fn cgroup_v2_memory_limit_v1(proc_self_cgroup: &Path, cgroup_root: &Path) -> Option<u64> {
    let mut directory = cgroup_v2_process_directory_v1(proc_self_cgroup, cgroup_root)?;
    let mut effective_limit = None;
    loop {
        for filename in ["memory.max", "memory.high"] {
            if let Some(limit) = finite_cgroup_memory_value_v1(&directory.join(filename)) {
                effective_limit =
                    Some(effective_limit.map_or(limit, |current: u64| current.min(limit)));
            }
        }
        if directory == cgroup_root {
            break;
        }
        let parent = directory.parent()?;
        if !parent.starts_with(cgroup_root) {
            return None;
        }
        directory = parent.to_path_buf();
    }
    effective_limit
}

fn effective_memory_bytes_v1(total_memory_bytes: u64, cgroup_limit: Option<u64>) -> u64 {
    match cgroup_limit {
        Some(cgroup_limit) if total_memory_bytes == 0 => cgroup_limit,
        Some(cgroup_limit) => total_memory_bytes.min(cgroup_limit),
        None => total_memory_bytes,
    }
}

fn process_resident_memory_limit_v1(
    total_memory_bytes: u64,
    cgroup_limit: Option<u64>,
    override_limit: Option<NonZeroU64>,
) -> NonZeroU64 {
    let automatic_limit = process_resident_memory_limit_for_system_v1(effective_memory_bytes_v1(
        total_memory_bytes,
        cgroup_limit,
    ));
    override_limit.map_or(automatic_limit, |override_limit| {
        cgroup_limit.map_or(override_limit, |cgroup_limit| {
            NonZeroU64::new(override_limit.get().min(cgroup_limit))
                .unwrap_or(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1)
        })
    })
}

/// Size the shared resident-allocation authority for this process.
///
/// The automatic authority uses the lower of physical RAM and this process's
/// finite cgroup-v2 `memory.max` / `memory.high`, then retains one quarter
/// outside modeled concurrent allocations. [`PROCESS_RESIDENT_MEMORY_LIMIT_ENV_V1`]
/// can lower or raise the automatic authority, but a finite cgroup ceiling
/// remains an upper bound. The resulting authority throttles simultaneous
/// scratch ownership; it never limits repository bytes on disk.
#[must_use]
pub fn detected_process_resident_memory_limit_v1() -> NonZeroU64 {
    let system = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    let total_memory_bytes = system.total_memory();
    let proc_self_cgroup = Path::new(PROC_SELF_CGROUP_V1);
    let cgroup_root = Path::new(CGROUP_V2_ROOT_V1);
    let cgroup_limit = cgroup_v2_memory_limit_v1(proc_self_cgroup, cgroup_root);
    let effective_memory_bytes = effective_memory_bytes_v1(total_memory_bytes, cgroup_limit);
    let limit = process_resident_memory_limit_v1(
        total_memory_bytes,
        cgroup_limit,
        process_resident_memory_limit_override_v1(),
    );
    hotpath::gauge!("resident_memory.system_total_bytes").set(total_memory_bytes as f64);
    hotpath::gauge!("resident_memory.effective_total_bytes").set(effective_memory_bytes as f64);
    if let Some(cgroup_limit) = cgroup_limit {
        hotpath::gauge!("resident_memory.cgroup_limit_bytes").set(cgroup_limit as f64);
    }
    hotpath::gauge!("resident_memory.admission_limit_bytes").set(limit.get() as f64);
    limit
}

/// Fraction of the configured limit, in permille, at or above which *measured*
/// process RSS is treated as over budget.
///
/// Reservations model what `TraceDecay` knows it is about to allocate. They do
/// not see the embedding runtime, grafeo stores, decoded generations, or
/// publish transients, so a process can sit far inside its reservation ceiling
/// while real RSS runs several times past the configured limit. This watermark
/// is where the admission decision stops trusting the model and starts
/// trusting the measurement.
pub const RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1: u64 = 900;

/// Fraction of the configured limit, in permille, at or below which measured
/// RSS clears the over-budget latch.
///
/// Strictly below the high watermark so a process hovering at the boundary
/// does not alternate admit/refuse on consecutive samples. Between the two
/// watermarks the previous verdict stands.
pub const RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1: u64 = 750;

/// Largest request still admitted while measured RSS is over budget.
///
/// Over-budget is a refusal of *growth*, not a process-wide stop: small
/// bookkeeping reservations still complete so the daemon can keep serving,
/// retiring, and releasing. Anything larger than this floor is exactly the
/// class of admission that turned a 16GiB configured limit into a 42GiB
/// resident process, so it waits for pressure to fall.
pub const RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1: u64 = 8 * 1024 * 1024;

/// Resolve one watermark in bytes from a permille fraction of the limit.
#[must_use]
pub fn resident_memory_watermark_bytes_v1(limit_bytes: NonZeroU64, permille: u64) -> u64 {
    let scaled = u128::from(limit_bytes.get()) * u128::from(permille) / 1_000;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// Sample this process's resident set size directly from the kernel.
///
/// The one `/proc/self/status` `VmRSS` parser in the workspace: the daemon's
/// dedicated resident-memory sampler publishes its samples into
/// [`process_resident_memory_pressure_v1`], and load-scoped watchdogs (the
/// semantic session pool's cold-load resident bound) sample it directly.
/// Returns `None` where the kernel surface is unavailable (non-Linux hosts),
/// which callers must treat as unobserved, never as zero.
#[must_use]
pub fn sampled_process_resident_bytes_v1() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kib = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?;
        kib.checked_mul(1_024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What the last measured RSS sample says about this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentMemoryPressureStateV1 {
    /// No sample has been published yet, so admission has nothing measured to
    /// consult and falls back to the reservation ceiling alone. An abstention,
    /// never a claim that the process is small.
    Unobserved,
    /// Measured RSS is below the pressure watermarks, or between them with the
    /// latch clear.
    Nominal {
        observed_bytes: u64,
        limit_bytes: u64,
        high_watermark_bytes: u64,
    },
    /// Measured RSS reached the high watermark and has not yet fallen back to
    /// the low watermark.
    OverBudget {
        observed_bytes: u64,
        limit_bytes: u64,
        high_watermark_bytes: u64,
        low_watermark_bytes: u64,
    },
}

impl ResidentMemoryPressureStateV1 {
    #[must_use]
    #[hotpath::skip]
    pub const fn is_over_budget(self) -> bool {
        matches!(self, Self::OverBudget { .. })
    }

    /// The last measured RSS, or `None` when nothing has been sampled.
    #[must_use]
    #[hotpath::skip]
    pub const fn observed_bytes(self) -> Option<u64> {
        match self {
            Self::Unobserved => None,
            Self::Nominal { observed_bytes, .. } | Self::OverBudget { observed_bytes, .. } => {
                Some(observed_bytes)
            }
        }
    }
}

/// The measurement handed to pressure reclaimers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResidentMemoryPressureReleaseRequestV1 {
    pub observed_bytes: u64,
    pub limit_bytes: u64,
    pub high_watermark_bytes: u64,
    /// Measured bytes above the high watermark.
    pub excess_bytes: u64,
}

/// Releases retained state that is reclaimable without losing durable truth,
/// returning the bytes it dropped. Reclaimers must never revoke work that is
/// already admitted and running.
pub type ResidentMemoryPressureReclaimerV1 =
    dyn Fn(ResidentMemoryPressureReleaseRequestV1) -> u64 + Send + Sync + 'static;

#[derive(Default)]
struct ResidentMemoryPressureReclaimerStateV1 {
    reclaimers: BTreeMap<(u32, u64), Arc<ResidentMemoryPressureReclaimerV1>>,
    next_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident-memory pressure reclaimer registration sequence exhausted")]
pub struct ResidentMemoryPressureRegistrationFailureV1;

/// The measured side of the memory accounting loop.
///
/// One dedicated reader samples real RSS (`/proc/self/status` `VmRSS` on
/// Linux), publishes the `daemon.process.resident_bytes` gauge, and feeds this
/// cell. Admission reads the same canonical observation; there is no second
/// parser or publisher.
pub struct ResidentMemoryPressureV1 {
    limit_bytes: NonZeroU64,
    high_watermark_bytes: u64,
    low_watermark_bytes: u64,
    observed_bytes: AtomicU64,
    observed: AtomicBool,
    over_budget: AtomicBool,
    state: ProfiledMutex<ResidentMemoryPressureReclaimerStateV1>,
}

impl fmt::Debug for ResidentMemoryPressureV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentMemoryPressureV1")
            .field("limit_bytes", &self.limit_bytes)
            .field("high_watermark_bytes", &self.high_watermark_bytes)
            .field("low_watermark_bytes", &self.low_watermark_bytes)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl ResidentMemoryPressureV1 {
    #[must_use]
    pub fn new(limit_bytes: NonZeroU64) -> Self {
        let high_watermark_bytes = resident_memory_watermark_bytes_v1(
            limit_bytes,
            RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1,
        );
        let low_watermark_bytes = resident_memory_watermark_bytes_v1(
            limit_bytes,
            RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1,
        )
        .min(high_watermark_bytes);
        Self {
            limit_bytes,
            high_watermark_bytes,
            low_watermark_bytes,
            observed_bytes: AtomicU64::new(0),
            observed: AtomicBool::new(false),
            over_budget: AtomicBool::new(false),
            state: hotpath::mutex!(
                Mutex::new(ResidentMemoryPressureReclaimerStateV1::default()),
                label = "runtime_core.resident.pressure"
            ),
        }
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn limit_bytes(&self) -> u64 {
        self.limit_bytes.get()
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn high_watermark_bytes(&self) -> u64 {
        self.high_watermark_bytes
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn low_watermark_bytes(&self) -> u64 {
        self.low_watermark_bytes
    }

    /// Publish one measured RSS sample and return the resulting state.
    ///
    /// The latch rises at the high watermark and clears only at the low
    /// watermark; between them the previous verdict stands, so admission does
    /// not flap while RSS hovers. Reaching the high watermark also runs the
    /// registered pressure reclaimers as the emergency response, because
    /// refusing new admissions alone cannot shrink state that is already
    /// retained.
    pub fn publish_observed_resident_bytes(
        &self,
        observed_bytes: u64,
    ) -> ResidentMemoryPressureStateV1 {
        self.publish_observation(observed_bytes);
        if observed_bytes >= self.high_watermark_bytes {
            self.run_pressure_reclaimers(observed_bytes);
        }
        self.publish_over_budget_gauge();
        self.state()
    }

    /// Publish RSS measured by a reclaimer after it released memory.
    ///
    /// This updates the canonical admission observation without running the
    /// reclaimer registry again. Reclaimers that can measure their process
    /// effect use this path from inside the original pressure pass.
    fn publish_post_reclaim_observed_resident_bytes(
        &self,
        observed_bytes: u64,
    ) -> ResidentMemoryPressureStateV1 {
        self.publish_observation(observed_bytes);
        self.publish_over_budget_gauge();
        self.state()
    }

    fn publish_observation(&self, observed_bytes: u64) {
        self.observed_bytes.store(observed_bytes, Ordering::Release);
        self.observed.store(true, Ordering::Release);
        hotpath::gauge!("daemon.memory.observed_resident_bytes").set(observed_bytes as f64);
        if observed_bytes >= self.high_watermark_bytes {
            self.over_budget.store(true, Ordering::Release);
        } else if observed_bytes <= self.low_watermark_bytes {
            self.over_budget.store(false, Ordering::Release);
        }
    }

    fn publish_over_budget_gauge(&self) {
        hotpath::gauge!("daemon.memory.over_budget").set(f64::from(u8::from(
            self.over_budget.load(Ordering::Acquire),
        )));
    }

    #[must_use]
    pub fn state(&self) -> ResidentMemoryPressureStateV1 {
        if !self.observed.load(Ordering::Acquire) {
            return ResidentMemoryPressureStateV1::Unobserved;
        }
        let observed_bytes = self.observed_bytes.load(Ordering::Acquire);
        if self.over_budget.load(Ordering::Acquire) {
            return ResidentMemoryPressureStateV1::OverBudget {
                observed_bytes,
                limit_bytes: self.limit_bytes.get(),
                high_watermark_bytes: self.high_watermark_bytes,
                low_watermark_bytes: self.low_watermark_bytes,
            };
        }
        ResidentMemoryPressureStateV1::Nominal {
            observed_bytes,
            limit_bytes: self.limit_bytes.get(),
            high_watermark_bytes: self.high_watermark_bytes,
        }
    }

    /// Register a reclaimer run when measured RSS reaches the high watermark.
    /// Lower priorities run first; the registration unregisters on drop.
    pub fn register_pressure_reclaimer(
        self: &Arc<Self>,
        priority: u32,
        callback: Arc<ResidentMemoryPressureReclaimerV1>,
    ) -> Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1>
    {
        let mut state = self.lock_state();
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .ok_or(ResidentMemoryPressureRegistrationFailureV1)?;
        state.reclaimers.insert((priority, sequence), callback);
        Ok(ResidentMemoryPressureRegistrationV1 {
            pressure: Arc::downgrade(self),
            priority,
            sequence,
        })
    }

    fn run_pressure_reclaimers(&self, observed_bytes: u64) -> u64 {
        let reclaimers: Vec<Arc<ResidentMemoryPressureReclaimerV1>> = {
            let state = self.lock_state();
            state.reclaimers.values().map(Arc::clone).collect()
        };
        if reclaimers.is_empty() {
            return 0;
        }
        let request = ResidentMemoryPressureReleaseRequestV1 {
            observed_bytes,
            limit_bytes: self.limit_bytes.get(),
            high_watermark_bytes: self.high_watermark_bytes,
            excess_bytes: observed_bytes.saturating_sub(self.high_watermark_bytes),
        };
        let mut released_bytes = 0_u64;
        for reclaimer in reclaimers {
            released_bytes = released_bytes.saturating_add(reclaimer(request));
        }
        hotpath::gauge!("daemon.memory.pressure_released_bytes").set(released_bytes as f64);
        released_bytes
    }

    fn lock_state(&self) -> ProfiledMutexGuard<'_, ResidentMemoryPressureReclaimerStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub struct ResidentMemoryPressureRegistrationV1 {
    pressure: Weak<ResidentMemoryPressureV1>,
    priority: u32,
    sequence: u64,
}

impl fmt::Debug for ResidentMemoryPressureRegistrationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentMemoryPressureRegistrationV1")
            .field("priority", &self.priority)
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl Drop for ResidentMemoryPressureRegistrationV1 {
    fn drop(&mut self) {
        let Some(pressure) = self.pressure.upgrade() else {
            return;
        };
        pressure
            .lock_state()
            .reclaimers
            .remove(&(self.priority, self.sequence));
    }
}

static PROCESS_RESIDENT_MEMORY_PRESSURE_V1: OnceLock<Arc<ResidentMemoryPressureV1>> =
    OnceLock::new();

/// The one measured-RSS cell for this process.
///
/// RSS is a process fact, not a per-authority one: every authority in the
/// process shares the same kernel accounting, so the cell is a process
/// singleton rather than an `Arc` threaded through the store runtime. Tests
/// build isolated cells and pass them to
/// [`ProcessResidentMemoryV1::with_pressure`] instead of touching this.
#[must_use]
pub fn process_resident_memory_pressure_v1() -> &'static Arc<ResidentMemoryPressureV1> {
    PROCESS_RESIDENT_MEMORY_PRESSURE_V1.get_or_init(|| {
        Arc::new(ResidentMemoryPressureV1::new(
            detected_process_resident_memory_limit_v1(),
        ))
    })
}

/// The allocator trim runs after every state reclaimer, so the pages those
/// reclaimers just freed are returned in the same pass.
pub const PROCESS_ALLOCATOR_TRIM_PRESSURE_PRIORITY_V1: u32 = u32::MAX;

static PROCESS_ALLOCATOR_TRIM_REGISTRATION_V1: OnceLock<
    Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1>,
> = OnceLock::new();

/// Bytes of RSS returned by one allocator trim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessAllocatorTrimV1 {
    /// Whether the allocator reported releasing anything.
    pub trimmed: bool,
    /// Measured RSS before the trim, when the kernel surface reports it.
    pub before_bytes: Option<u64>,
    /// Measured RSS after the trim, when the kernel surface reports it.
    pub after_bytes: Option<u64>,
}

impl ProcessAllocatorTrimV1 {
    /// RSS the trim returned to the kernel; zero when it was not measurable.
    #[must_use]
    pub fn released_bytes(self) -> u64 {
        match (self.before_bytes, self.after_bytes) {
            (Some(before), Some(after)) => before.saturating_sub(after),
            _ => 0,
        }
    }
}

/// Return freed-but-retained allocator pages to the kernel.
///
/// glibc keeps freed chunks inside its per-thread arenas and only unmaps a
/// heap from its top, so a fan-out that allocates and frees on dozens of
/// worker threads leaves most of that memory resident forever: indexing a
/// 68 MB source tree on four cores measured 8.5 GB of arena system memory
/// with 3.5 GB live and 4.9 GB free, and one `malloc_trim(0)` returned 3.9 GB
/// of RSS at once. Measured RSS is what admission trusts, so those pages
/// refuse real work. Other allocators return zero here; the reclaimer is a
/// no-op for them.
#[must_use]
pub fn release_process_allocator_memory_v1() -> ProcessAllocatorTrimV1 {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let before_bytes = sampled_process_resident_bytes_v1();
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc
        // maintenance call that takes no pointers and invalidates no live
        // allocation; it only advises the kernel about pages the allocator
        // no longer uses.
        let trimmed = unsafe { libc::malloc_trim(0) } == 1;
        let after_bytes = sampled_process_resident_bytes_v1();
        let trim = ProcessAllocatorTrimV1 {
            trimmed,
            before_bytes,
            after_bytes,
        };
        hotpath::gauge!("daemon.memory.allocator_trim_released_bytes").set(trim.released_bytes());
        trim
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        ProcessAllocatorTrimV1 {
            trimmed: false,
            before_bytes: None,
            after_bytes: None,
        }
    }
}

/// Register the allocator trim as the last pressure reclaimer of `pressure`.
pub fn register_process_allocator_pressure_reclaimer_v1(
    pressure: &Arc<ResidentMemoryPressureV1>,
) -> Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1> {
    let pressure_weak = Arc::downgrade(pressure);
    pressure.register_pressure_reclaimer(
        PROCESS_ALLOCATOR_TRIM_PRESSURE_PRIORITY_V1,
        Arc::new(move |request| {
            let trim = release_process_allocator_memory_v1();
            if let (Some(after_bytes), Some(pressure)) = (trim.after_bytes, pressure_weak.upgrade())
            {
                pressure.publish_post_reclaim_observed_resident_bytes(after_bytes);
            }
            tracing::info!(
                event = "process_allocator_trimmed",
                trimmed = trim.trimmed,
                released_bytes = trim.released_bytes(),
                observed_bytes = request.observed_bytes,
                high_watermark_bytes = request.high_watermark_bytes,
                "returned freed allocator pages under resident-memory pressure"
            );
            trim.released_bytes()
        }),
    )
}

/// Install the allocator trim reclaimer on the process pressure cell, once.
///
/// Returns whether this call installed it; later calls are no-ops that
/// return `false`. Registration failure remains typed, and the registration
/// lives for the process.
pub fn install_process_allocator_pressure_reclaimer_v1()
-> Result<bool, ResidentMemoryPressureRegistrationFailureV1> {
    install_process_allocator_pressure_reclaimer_on_v1(
        &PROCESS_ALLOCATOR_TRIM_REGISTRATION_V1,
        process_resident_memory_pressure_v1(),
    )
}

fn install_process_allocator_pressure_reclaimer_on_v1(
    registration: &OnceLock<
        Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1>,
    >,
    pressure: &Arc<ResidentMemoryPressureV1>,
) -> Result<bool, ResidentMemoryPressureRegistrationFailureV1> {
    let mut installed = false;
    let result = registration.get_or_init(|| {
        installed = true;
        register_process_allocator_pressure_reclaimer_v1(pressure)
    });
    result
        .as_ref()
        .map(|_| installed)
        .map_err(|failure| *failure)
}

/// Stable component label inside one exact generation identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResidentMemoryComponentIdV1(&'static str);

impl ResidentMemoryComponentIdV1 {
    pub fn new(value: &'static str) -> Result<Self, ResidentMemoryComponentIdErrorV1> {
        if value.is_empty()
            || value.len() > 128
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(ResidentMemoryComponentIdErrorV1);
        }
        Ok(Self(value))
    }

    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident-memory component id must be canonical and at most 128 bytes")]
pub struct ResidentMemoryComponentIdErrorV1;

/// Exact owner of retained process memory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResidentMemoryKeyV1 {
    pub project_id: ProjectId,
    pub worktree_id: WorktreeId,
    pub generation_id: CodeGenerationId,
    pub component: ResidentMemoryComponentIdV1,
}

/// Typed refusal after one bounded reclaim pass.
///
/// Two distinct refusals, never collapsed into one: the modeled reservations
/// are full, or the *measured* process is over budget while the model still
/// claims room. Both name the exact bytes that produced them so no caller has
/// to guess, and neither is ever a silent stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResidentMemoryAdmissionFailureV1 {
    /// Admitted reservations plus this request exceed the configured limit.
    #[error(
        "resident-memory admission denied: used={used_bytes} requested={requested_bytes} limit={limit_bytes}"
    )]
    ReservationCeiling {
        used_bytes: u64,
        requested_bytes: u64,
        limit_bytes: u64,
    },
    /// Measured process RSS is at or above the high watermark. Reservations
    /// say there is room; the kernel says otherwise, and the measurement wins.
    #[error(
        "resident-memory admission over budget: observed_rss={observed_bytes} configured_limit={limit_bytes} high_watermark={high_watermark_bytes} requested={requested_bytes} floor={floor_bytes}"
    )]
    ObservedOverBudget {
        observed_bytes: u64,
        limit_bytes: u64,
        high_watermark_bytes: u64,
        requested_bytes: u64,
        floor_bytes: u64,
    },
}

impl ResidentMemoryAdmissionFailureV1 {
    #[must_use]
    #[hotpath::skip]
    pub const fn requested_bytes(&self) -> u64 {
        match self {
            Self::ReservationCeiling {
                requested_bytes, ..
            }
            | Self::ObservedOverBudget {
                requested_bytes, ..
            } => *requested_bytes,
        }
    }

    #[must_use]
    #[hotpath::skip]
    pub const fn limit_bytes(&self) -> u64 {
        match self {
            Self::ReservationCeiling { limit_bytes, .. }
            | Self::ObservedOverBudget { limit_bytes, .. } => *limit_bytes,
        }
    }

    /// Whether this refusal came from measured RSS rather than the modeled
    /// ceiling. Over-budget refusals clear as pressure falls, so callers retry
    /// them instead of treating the input as permanently unservable.
    #[must_use]
    #[hotpath::skip]
    pub const fn is_observed_over_budget(&self) -> bool {
        matches!(self, Self::ObservedOverBudget { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "resident-memory reservation cannot grow after allocation: reserved={reserved_bytes} measured={measured_bytes}"
)]
pub struct ResidentMemoryAdjustmentFailureV1 {
    pub reserved_bytes: u64,
    pub measured_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident-memory reclaimer registration sequence exhausted")]
pub struct ResidentMemoryReclaimerRegistrationFailureV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemoryChargeV1 {
    pub key: ResidentMemoryKeyV1,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemorySnapshotV1 {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub charges: Vec<ResidentMemoryChargeV1>,
    pub process_shared_charges: Vec<ProcessSharedMemoryChargeV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSharedMemoryChargeV1 {
    pub component: ResidentMemoryComponentIdV1,
    pub bytes: u64,
}

impl ResidentMemorySnapshotV1 {
    pub fn charge_for(&self, key: &ResidentMemoryKeyV1) -> u64 {
        self.charges
            .iter()
            .find(|charge| charge.key == *key)
            .map_or(0, |charge| charge.bytes)
    }

    pub fn process_shared_charge_for(&self, component: ResidentMemoryComponentIdV1) -> u64 {
        self.process_shared_charges
            .iter()
            .find(|charge| charge.component == component)
            .map_or(0, |charge| charge.bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemoryReclaimRequestV1 {
    pub key: ResidentMemoryKeyV1,
    pub used_bytes: u64,
    pub requested_bytes: u64,
    pub limit_bytes: u64,
    pub shortfall_bytes: u64,
}

pub type ResidentMemoryReclaimerV1 = dyn Fn(ResidentMemoryReclaimRequestV1) + Send + Sync + 'static;

struct ReclaimerEntryV1 {
    callback: Arc<ResidentMemoryReclaimerV1>,
}

#[derive(Default)]
struct ResidentMemoryStateV1 {
    used_bytes: u64,
    charges: BTreeMap<ResidentMemoryKeyV1, u64>,
    process_shared_charges: BTreeMap<ResidentMemoryComponentIdV1, u64>,
    reclaimers: BTreeMap<(u32, u64), Arc<ResidentMemoryReclaimerV1>>,
    next_reclaimer_sequence: u64,
}

/// The single process ceiling. Callers share one pointer-identical `Arc`.
pub struct ProcessResidentMemoryV1 {
    limit_bytes: NonZeroU64,
    state: ProfiledMutex<ResidentMemoryStateV1>,
    /// Measured RSS this admission consults before trusting its own model.
    pressure: Arc<ResidentMemoryPressureV1>,
}

impl fmt::Debug for ProcessResidentMemoryV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.snapshot();
        formatter
            .debug_struct("ProcessResidentMemoryV1")
            .field("used_bytes", &snapshot.used_bytes)
            .field("limit_bytes", &snapshot.limit_bytes)
            .finish_non_exhaustive()
    }
}

impl ProcessResidentMemoryV1 {
    /// Production constructor: binds the one process-wide measured-RSS cell.
    pub fn new(limit_bytes: NonZeroU64) -> Self {
        Self::with_pressure(
            limit_bytes,
            Arc::clone(process_resident_memory_pressure_v1()),
        )
    }

    /// Bind an explicit measured-RSS cell instead of the process singleton.
    ///
    /// Production always uses [`Self::new`], whose cell is fed by the daemon's
    /// `/proc/self/status` sampler. Tests use this to inject a fake RSS series
    /// without a `/proc` read and without leaking pressure between cases.
    pub fn with_pressure(limit_bytes: NonZeroU64, pressure: Arc<ResidentMemoryPressureV1>) -> Self {
        Self {
            limit_bytes,
            state: hotpath::mutex!(
                Mutex::new(ResidentMemoryStateV1::default()),
                label = "runtime_core.resident.state"
            ),
            pressure,
        }
    }

    /// The measured-RSS cell this admission consults.
    #[must_use]
    pub fn pressure(&self) -> &Arc<ResidentMemoryPressureV1> {
        &self.pressure
    }

    #[hotpath::measure(label = "runtime_core.resident.reserve")]
    pub fn reserve(
        self: &Arc<Self>,
        key: ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> Result<ResidentMemoryReservationV1, ResidentMemoryAdmissionFailureV1> {
        if let Some(failure) = self.observed_over_budget_refusal(requested_bytes) {
            return Err(failure);
        }
        if let Some(reservation) = self.try_reserve(&key, requested_bytes) {
            return Ok(reservation);
        }

        if requested_bytes.get() <= self.limit_bytes.get() {
            let reclaimers = self.reclaimers();
            for reclaimer in reclaimers {
                (reclaimer.callback)(self.reclaim_request(key.clone(), requested_bytes));
                if let Some(reservation) = self.try_reserve(&key, requested_bytes) {
                    return Ok(reservation);
                }
            }
        }

        Err(self.admission_failure(requested_bytes))
    }

    /// Reserves one process-shared component without fabricating a project,
    /// worktree, or code-generation owner. These reservations use the same
    /// process ceiling and RAII release authority as project generations.
    #[hotpath::measure(label = "runtime_core.resident.reserve_shared")]
    pub fn reserve_process_shared(
        self: &Arc<Self>,
        component: ResidentMemoryComponentIdV1,
        requested_bytes: NonZeroU64,
    ) -> Result<ProcessSharedMemoryReservationV1, ResidentMemoryAdmissionFailureV1> {
        if let Some(failure) = self.observed_over_budget_refusal(requested_bytes) {
            return Err(failure);
        }
        let mut state = self.lock_state();
        let Some(next_used) = state.used_bytes.checked_add(requested_bytes.get()) else {
            hotpath::gauge!("runtime_core.resident.refusals").inc(1.0);
            return Err(self.admission_failure_from_used(state.used_bytes, requested_bytes));
        };
        if next_used > self.limit_bytes.get() {
            hotpath::gauge!("runtime_core.resident.refusals").inc(1.0);
            return Err(self.admission_failure_from_used(state.used_bytes, requested_bytes));
        }
        state.used_bytes = next_used;
        *state.process_shared_charges.entry(component).or_default() += requested_bytes.get();
        hotpath::gauge!("runtime_core.resident.reservations").inc(1.0);
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        Ok(ProcessSharedMemoryReservationV1 {
            authority: Arc::clone(self),
            component,
            reserved_bytes: requested_bytes.get(),
        })
    }

    pub fn register_reclaimer(
        self: &Arc<Self>,
        priority: u32,
        callback: Arc<ResidentMemoryReclaimerV1>,
    ) -> Result<ResidentMemoryReclaimerRegistrationV1, ResidentMemoryReclaimerRegistrationFailureV1>
    {
        let mut state = self.lock_state();
        let sequence = state.next_reclaimer_sequence;
        state.next_reclaimer_sequence = sequence
            .checked_add(1)
            .ok_or(ResidentMemoryReclaimerRegistrationFailureV1)?;
        state.reclaimers.insert((priority, sequence), callback);
        Ok(ResidentMemoryReclaimerRegistrationV1 {
            authority: Arc::downgrade(self),
            priority,
            sequence,
        })
    }

    pub fn snapshot(&self) -> ResidentMemorySnapshotV1 {
        let state = self.lock_state();
        ResidentMemorySnapshotV1 {
            used_bytes: state.used_bytes,
            limit_bytes: self.limit_bytes.get(),
            charges: state
                .charges
                .iter()
                .map(|(key, bytes)| ResidentMemoryChargeV1 {
                    key: key.clone(),
                    bytes: *bytes,
                })
                .collect(),
            process_shared_charges: state
                .process_shared_charges
                .iter()
                .map(|(component, bytes)| ProcessSharedMemoryChargeV1 {
                    component: *component,
                    bytes: *bytes,
                })
                .collect(),
        }
    }

    fn lock_state(&self) -> ProfiledMutexGuard<'_, ResidentMemoryStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_reserve(
        self: &Arc<Self>,
        key: &ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> Option<ResidentMemoryReservationV1> {
        let mut state = self.lock_state();
        let next_used = state.used_bytes.checked_add(requested_bytes.get())?;
        if next_used > self.limit_bytes.get() {
            return None;
        }
        state.used_bytes = next_used;
        *state.charges.entry(key.clone()).or_default() += requested_bytes.get();
        hotpath::gauge!("runtime_core.resident.reservations").inc(1.0);
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        Some(ResidentMemoryReservationV1 {
            authority: Arc::clone(self),
            key: key.clone(),
            reserved_bytes: requested_bytes.get(),
        })
    }

    fn reclaimers(&self) -> Vec<ReclaimerEntryV1> {
        let state = self.lock_state();
        state
            .reclaimers
            .values()
            .map(|callback| ReclaimerEntryV1 {
                callback: Arc::clone(callback),
            })
            .collect()
    }

    fn reclaim_request(
        &self,
        key: ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> ResidentMemoryReclaimRequestV1 {
        let state = self.lock_state();
        let available_bytes = self.limit_bytes.get() - state.used_bytes;
        let shortfall_bytes = requested_bytes.get().saturating_sub(available_bytes);
        ResidentMemoryReclaimRequestV1 {
            key,
            used_bytes: state.used_bytes,
            requested_bytes: requested_bytes.get(),
            limit_bytes: self.limit_bytes.get(),
            shortfall_bytes,
        }
    }

    /// Refuse growth while measured RSS is over budget.
    ///
    /// Only *new* admissions are refused. Nothing already reserved is revoked,
    /// shrunk, or released by this path: the reservation guards outlive
    /// pressure exactly as before, and the refusal clears on its own once a
    /// later sample falls to the low watermark.
    fn observed_over_budget_refusal(
        &self,
        requested_bytes: NonZeroU64,
    ) -> Option<ResidentMemoryAdmissionFailureV1> {
        let ResidentMemoryPressureStateV1::OverBudget {
            observed_bytes,
            limit_bytes,
            high_watermark_bytes,
            ..
        } = self.pressure.state()
        else {
            return None;
        };
        if requested_bytes.get() <= RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1 {
            return None;
        }
        hotpath::gauge!("daemon.memory.admission_refused").inc(1.0);
        hotpath::gauge!("runtime_core.resident.refusals").inc(1.0);
        Some(ResidentMemoryAdmissionFailureV1::ObservedOverBudget {
            observed_bytes,
            limit_bytes,
            high_watermark_bytes,
            requested_bytes: requested_bytes.get(),
            floor_bytes: RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1,
        })
    }

    fn admission_failure(&self, requested_bytes: NonZeroU64) -> ResidentMemoryAdmissionFailureV1 {
        hotpath::gauge!("runtime_core.resident.refusals").inc(1.0);
        self.admission_failure_from_used(self.lock_state().used_bytes, requested_bytes)
    }

    fn admission_failure_from_used(
        &self,
        used_bytes: u64,
        requested_bytes: NonZeroU64,
    ) -> ResidentMemoryAdmissionFailureV1 {
        ResidentMemoryAdmissionFailureV1::ReservationCeiling {
            used_bytes,
            requested_bytes: requested_bytes.get(),
            limit_bytes: self.limit_bytes.get(),
        }
    }

    fn shrink(
        &self,
        key: &ResidentMemoryKeyV1,
        reserved_bytes: u64,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        if measured_bytes > reserved_bytes {
            return Err(ResidentMemoryAdjustmentFailureV1 {
                reserved_bytes,
                measured_bytes,
            });
        }
        let released_bytes = reserved_bytes - measured_bytes;
        if released_bytes == 0 {
            return Ok(());
        }
        let mut state = self.lock_state();
        state.used_bytes -= released_bytes;
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        if let Some(charge) = state.charges.get_mut(key) {
            *charge -= released_bytes;
            if *charge == 0 {
                state.charges.remove(key);
            }
        }
        Ok(())
    }

    fn release(&self, key: &ResidentMemoryKeyV1, reserved_bytes: u64) {
        if reserved_bytes == 0 {
            return;
        }
        let mut state = self.lock_state();
        state.used_bytes -= reserved_bytes;
        hotpath::gauge!("runtime_core.resident.reservations").dec(1.0);
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        if let Some(charge) = state.charges.get_mut(key) {
            *charge -= reserved_bytes;
            if *charge == 0 {
                state.charges.remove(key);
            }
        }
    }

    fn shrink_process_shared(
        &self,
        component: ResidentMemoryComponentIdV1,
        reserved_bytes: u64,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        if measured_bytes > reserved_bytes {
            return Err(ResidentMemoryAdjustmentFailureV1 {
                reserved_bytes,
                measured_bytes,
            });
        }
        let released_bytes = reserved_bytes - measured_bytes;
        if released_bytes == 0 {
            return Ok(());
        }
        let mut state = self.lock_state();
        state.used_bytes -= released_bytes;
        if let Some(charge) = state.process_shared_charges.get_mut(&component) {
            *charge -= released_bytes;
            if *charge == 0 {
                state.process_shared_charges.remove(&component);
            }
        }
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        Ok(())
    }

    fn release_process_shared(&self, component: ResidentMemoryComponentIdV1, reserved_bytes: u64) {
        if reserved_bytes == 0 {
            return;
        }
        let mut state = self.lock_state();
        state.used_bytes -= reserved_bytes;
        if let Some(charge) = state.process_shared_charges.get_mut(&component) {
            *charge -= reserved_bytes;
            if *charge == 0 {
                state.process_shared_charges.remove(&component);
            }
        }
        hotpath::gauge!("runtime_core.resident.reservations").dec(1.0);
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
    }
}

pub struct ResidentMemoryReservationV1 {
    authority: Arc<ProcessResidentMemoryV1>,
    key: ResidentMemoryKeyV1,
    reserved_bytes: u64,
}

impl fmt::Debug for ResidentMemoryReservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentMemoryReservationV1")
            .field("key", &self.key)
            .field("reserved_bytes", &self.reserved_bytes)
            .finish_non_exhaustive()
    }
}

impl ResidentMemoryReservationV1 {
    pub fn key(&self) -> &ResidentMemoryKeyV1 {
        &self.key
    }

    #[hotpath::skip]
    pub const fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }

    pub fn shrink_to(
        &mut self,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        self.authority
            .shrink(&self.key, self.reserved_bytes, measured_bytes)?;
        self.reserved_bytes = measured_bytes;
        Ok(())
    }
}

impl Drop for ResidentMemoryReservationV1 {
    fn drop(&mut self) {
        self.authority.release(&self.key, self.reserved_bytes);
    }
}

pub struct ProcessSharedMemoryReservationV1 {
    authority: Arc<ProcessResidentMemoryV1>,
    component: ResidentMemoryComponentIdV1,
    reserved_bytes: u64,
}

impl ProcessSharedMemoryReservationV1 {
    #[hotpath::skip]
    pub const fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }

    pub fn shrink_to(
        &mut self,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        self.authority.shrink_process_shared(
            self.component,
            self.reserved_bytes,
            measured_bytes,
        )?;
        self.reserved_bytes = measured_bytes;
        Ok(())
    }
}

impl fmt::Debug for ProcessSharedMemoryReservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessSharedMemoryReservationV1")
            .field("component", &self.component)
            .field("reserved_bytes", &self.reserved_bytes)
            .finish_non_exhaustive()
    }
}

impl Drop for ProcessSharedMemoryReservationV1 {
    fn drop(&mut self) {
        self.authority
            .release_process_shared(self.component, self.reserved_bytes);
    }
}

pub struct ResidentMemoryReclaimerRegistrationV1 {
    authority: Weak<ProcessResidentMemoryV1>,
    priority: u32,
    sequence: u64,
}

impl Drop for ResidentMemoryReclaimerRegistrationV1 {
    fn drop(&mut self) {
        let Some(authority) = self.authority.upgrade() else {
            return;
        };
        authority
            .lock_state()
            .reclaimers
            .remove(&(self.priority, self.sequence));
    }
}

#[cfg(test)]
mod tests;
