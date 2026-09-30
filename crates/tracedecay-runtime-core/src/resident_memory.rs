//! Process-wide admission for structurally measured resident allocations.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use sysinfo::{MemoryRefreshKind, RefreshKind, System};
use tracedecay_domain::process_heap::installed_process_allocator_release_v1;
use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

use crate::profiled_lock::{ProfiledMutex, ProfiledMutexGuard};

mod owners;

pub use owners::{
    RESIDENT_OWNER_IDLE_WINDOW_V1, RESIDENT_OWNER_SHED_ORDER_V1, ResidentHoldingV1,
    ResidentOwnerBytesV1, ResidentOwnerHolderV1, ResidentOwnerKindV1,
    ResidentOwnerRegistrationFailureV1, ResidentOwnerRegistrationV1, ResidentOwnerReleaseCauseV1,
    ResidentOwnerReleaseV1, ResidentOwnerReleasedV1, ResidentOwnerReportRowV1,
    ResidentOwnerSampleV1, ResidentOwnerScopeV1, ResidentOwnerV1, ResidentOwnersReportV1,
    ResidentOwnersV1, ResidentSharedContentV1, process_resident_owners_v1,
};

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
/// same limit, so raising it can both admit and widen indexing, up to the
/// hard cgroup ceiling (`memory.max`, or `memory.high` when max is unlimited).
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

/// The two cgroup-v2 memory controls on this process, walked to the mount root.
///
/// `memory.max` is the kernel kill line. `memory.high` is the reclaim line
/// underneath it. They stay separate so each is used for what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CgroupMemoryCeilingV1 {
    max_bytes: Option<u64>,
    high_bytes: Option<u64>,
}

/// Hard service ceiling: `memory.max` when it is finite, otherwise `memory.high`.
///
/// A lone `memory.high` is the ceiling because the operator left no band above
/// the reclaim line. When both are finite, high is pressure, not a tighter max.
fn cgroup_service_ceiling_bytes(ceiling: CgroupMemoryCeilingV1) -> Option<u64> {
    ceiling.max_bytes.or(ceiling.high_bytes)
}

fn tighten(bound: Option<u64>, limit: u64) -> u64 {
    bound.map_or(limit, |current| current.min(limit))
}

fn cgroup_v2_memory_ceiling_v1(
    proc_self_cgroup: &Path,
    cgroup_root: &Path,
) -> Option<CgroupMemoryCeilingV1> {
    let mut directory = cgroup_v2_process_directory_v1(proc_self_cgroup, cgroup_root)?;
    let mut max_bytes = None;
    let mut high_bytes = None;
    loop {
        if let Some(limit) = finite_cgroup_memory_value_v1(&directory.join("memory.max")) {
            max_bytes = Some(tighten(max_bytes, limit));
        }
        if let Some(limit) = finite_cgroup_memory_value_v1(&directory.join("memory.high")) {
            high_bytes = Some(tighten(high_bytes, limit));
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
    Some(CgroupMemoryCeilingV1 {
        max_bytes,
        high_bytes,
    })
}

fn effective_memory_bytes_v1(total_memory_bytes: u64, cgroup_limit: Option<u64>) -> u64 {
    match cgroup_limit {
        Some(cgroup_limit) if total_memory_bytes == 0 => cgroup_limit,
        Some(cgroup_limit) => total_memory_bytes.min(cgroup_limit),
        None => total_memory_bytes,
    }
}

struct ResidentMemoryAuthorityV1 {
    limit_bytes: NonZeroU64,
    /// `memory.high` when it sits strictly below the hard admission ceiling.
    reclaim_watermark_bytes: Option<u64>,
}

fn finite_nonzero_bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or(DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1)
}

/// Admission ceiling for one host and one cgroup reading.
///
/// Host reserve (one quarter of physical RAM) and the cgroup service ceiling
/// are alternative protections, not stacked discounts. The reserve applies
/// when the process can otherwise spend the machine. A finite cgroup already
/// reserved the rest of the machine, so the hard ceiling is
/// `min(host allowance, memory.max)` — or `memory.high` only when max is
/// unlimited. `memory.high` below that ceiling is the reclaim watermark, not
/// a second cut. An explicit override replaces the host reserve and is still
/// capped by the hard ceiling.
fn resident_memory_authority_v1(
    total_memory_bytes: u64,
    cgroup: Option<CgroupMemoryCeilingV1>,
    override_limit: Option<NonZeroU64>,
) -> ResidentMemoryAuthorityV1 {
    let cgroup = cgroup.unwrap_or(CgroupMemoryCeilingV1 {
        max_bytes: None,
        high_bytes: None,
    });
    let service_ceiling = cgroup_service_ceiling_bytes(cgroup);
    let host_allowance = (total_memory_bytes != 0)
        .then(|| process_resident_memory_limit_for_system_v1(total_memory_bytes));
    let automatic_limit = match (host_allowance, service_ceiling) {
        (Some(host), Some(ceiling)) => finite_nonzero_bytes(host.get().min(ceiling)),
        (Some(host), None) => host,
        (None, Some(ceiling)) => finite_nonzero_bytes(ceiling),
        (None, None) => DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
    };
    let limit_bytes = match override_limit {
        Some(override_limit) => match service_ceiling {
            Some(ceiling) => finite_nonzero_bytes(override_limit.get().min(ceiling)),
            None => override_limit,
        },
        None => automatic_limit,
    };
    let reclaim_watermark_bytes = cgroup.high_bytes.filter(|high| *high < limit_bytes.get());
    ResidentMemoryAuthorityV1 {
        limit_bytes,
        reclaim_watermark_bytes,
    }
}

/// Size the shared resident-allocation authority for this process.
///
/// The automatic authority is the lower of the host reserve and this
/// process's hard cgroup ceiling (`memory.max`, or `memory.high` when max is
/// unlimited). A finite `memory.high` below that ceiling is the pressure
/// watermark, not a further discount of the ceiling.
/// [`PROCESS_RESIDENT_MEMORY_LIMIT_ENV_V1`] can lower or raise the automatic
/// authority, but the hard cgroup ceiling remains an upper bound. The
/// resulting authority throttles simultaneous scratch ownership; it never
/// limits repository bytes on disk.
#[must_use]
pub fn detected_process_resident_memory_limit_v1() -> NonZeroU64 {
    read_resident_memory_authority_v1().limit_bytes
}

/// Physical RAM of this host, or `None` when the platform does not report it.
#[must_use]
pub fn physical_memory_bytes_v1() -> Option<u64> {
    let system = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    Some(system.total_memory()).filter(|bytes| *bytes != 0)
}

fn read_resident_memory_authority_v1() -> ResidentMemoryAuthorityV1 {
    let total_memory_bytes = physical_memory_bytes_v1().unwrap_or(0);
    let proc_self_cgroup = Path::new(PROC_SELF_CGROUP_V1);
    let cgroup_root = Path::new(CGROUP_V2_ROOT_V1);
    let cgroup = cgroup_v2_memory_ceiling_v1(proc_self_cgroup, cgroup_root);
    let service_ceiling = cgroup.and_then(cgroup_service_ceiling_bytes);
    let effective_memory_bytes = effective_memory_bytes_v1(total_memory_bytes, service_ceiling);
    let authority = resident_memory_authority_v1(
        total_memory_bytes,
        cgroup,
        process_resident_memory_limit_override_v1(),
    );
    hotpath::gauge!("resident_memory.system_total_bytes").set(total_memory_bytes as f64);
    hotpath::gauge!("resident_memory.effective_total_bytes").set(effective_memory_bytes as f64);
    if let Some(high_bytes) = cgroup.and_then(|ceiling| ceiling.high_bytes) {
        hotpath::gauge!("resident_memory.cgroup_high_bytes").set(high_bytes as f64);
    }
    if let Some(service_ceiling) = service_ceiling {
        hotpath::gauge!("resident_memory.cgroup_limit_bytes").set(service_ceiling as f64);
    }
    hotpath::gauge!("resident_memory.admission_limit_bytes")
        .set(authority.limit_bytes.get() as f64);
    authority
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

/// Shortest gap between two kernel reads taken by
/// [`ResidentMemoryPressureV1::sample_for_checkpoint`].
///
/// Checkpoints are polled from per-row loops. One sample opens and formats
/// `/proc/self/status` and every cgroup memory file up the hierarchy, which
/// on a large, busy cgroup costs far more than the row it guards: a sealed
/// graph build of a 200k-symbol repository spent most of its wall time in
/// those reads and never finished inside its budget (#2505). Within this gap
/// the standing observation answers, so a build can outgrow a sample by at
/// most what it allocates in 10 ms.
pub const RESIDENT_MEMORY_CHECKPOINT_SAMPLE_INTERVAL_V1: Duration = Duration::from_millis(10);

/// Resolve one watermark in bytes from a permille fraction of the limit.
#[must_use]
pub fn resident_memory_watermark_bytes_v1(limit_bytes: NonZeroU64, permille: u64) -> u64 {
    let scaled = u128::from(limit_bytes.get()) * u128::from(permille) / 1_000;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// One kernel reading of this process's resident set, split by whether the
/// kernel can take the pages back without swapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessResidentSampleV1 {
    /// Every resident page (`VmRSS`), clean file-backed mappings included.
    pub resident_bytes: u64,
    /// Anonymous and shared-memory pages (`RssAnon + RssShmem`).
    pub unreclaimable_bytes: u64,
    /// Anonymous pages the kernel moved to swap (`VmSwap`).
    pub swapped_bytes: u64,
    /// Bytes the kernel charges toward a finite `memory.max`: `memory.current`
    /// minus `inactive_file`, plus `memory.swap.current`, on the cgroup with
    /// the tightest finite ceiling.
    ///
    /// `None` when no cgroup has a finite `memory.max`. An unlimited cgroup's
    /// file cache is not a kill line, and inactive file pages are what the
    /// kernel reclaims before it kills. Active file pages of a mapped store
    /// stay in this figure because they are not dropped before the kill.
    pub cgroup_committed_bytes: Option<u64>,
}

impl ProcessResidentSampleV1 {
    /// Bytes admission compares with the watermark.
    ///
    /// The daemon's anonymous state is the floor, swapped pages included:
    /// they are live heap the next touch faults back in, so a sample taken
    /// while the kernel swaps under a cgroup ceiling must not read as room.
    /// A finite cgroup ceiling also counts its committed working set, so a
    /// build cannot be admitted while `memory.current` is already at the kill
    /// line and only the anonymous subset sits under the watermark.
    #[must_use]
    pub fn admission_bytes(self) -> u64 {
        self.unreclaimable_bytes
            .saturating_add(self.swapped_bytes)
            .max(self.cgroup_committed_bytes.unwrap_or(0))
    }
}

#[cfg(target_os = "linux")]
fn status_kib_field_bytes(status: &str, field: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1_024)
}

#[cfg(target_os = "linux")]
fn process_resident_sample_from_status_v1(status: &str) -> Option<ProcessResidentSampleV1> {
    let anon = status_kib_field_bytes(status, "RssAnon")?;
    let shmem = status_kib_field_bytes(status, "RssShmem")?;
    Some(ProcessResidentSampleV1 {
        resident_bytes: status_kib_field_bytes(status, "VmRSS")?,
        unreclaimable_bytes: anon.checked_add(shmem)?,
        swapped_bytes: status_kib_field_bytes(status, "VmSwap")?,
        cgroup_committed_bytes: None,
    })
}

#[cfg(target_os = "linux")]
fn memory_stat_field_bytes(stat: &str, field: &str) -> Option<u64> {
    stat.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        if parts.next()? != field {
            return None;
        }
        parts.next()?.parse::<u64>().ok()
    })
}

/// Working set a finite `memory.max` will kill for: `memory.current` minus
/// `inactive_file`, plus the cgroup's swapped anonymous pages
/// (`memory.swap.current`), on the cgroup directory with the tightest finite
/// ceiling.
///
/// `None` when every `memory.max` is absent or `max`. Counting `memory.current`
/// on an unlimited cgroup treats the machine's page cache as a kill line and
/// refuses work the kernel can reclaim.
#[cfg(target_os = "linux")]
fn cgroup_committed_bytes_v1(proc_self_cgroup: &Path, cgroup_root: &Path) -> Option<u64> {
    let mut directory = cgroup_v2_process_directory_v1(proc_self_cgroup, cgroup_root)?;
    let mut chosen: Option<(std::path::PathBuf, u64)> = None;
    loop {
        if let Some(limit) = finite_cgroup_memory_value_v1(&directory.join("memory.max")) {
            let tighter = chosen.as_ref().is_none_or(|(_, current)| limit < *current);
            if tighter {
                chosen = Some((directory.clone(), limit));
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
    let (directory, _) = chosen?;
    let current = std::fs::read_to_string(directory.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let inactive_file = std::fs::read_to_string(directory.join("memory.stat"))
        .ok()
        .and_then(|stat| memory_stat_field_bytes(&stat, "inactive_file"))
        .unwrap_or(0);
    let swapped = std::fs::read_to_string(directory.join("memory.swap.current"))
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Some(
        current
            .saturating_sub(inactive_file)
            .saturating_add(swapped),
    )
}

/// Sample this process's resident set directly from the kernel.
///
/// The one `/proc/self/status` parser in the workspace: the daemon's
/// dedicated resident-memory sampler and every admission re-measure read it
/// through [`ResidentMemoryPressureV1::sample_and_publish`]. Returns `None`
/// where the kernel surface is unavailable (non-Linux hosts), which callers
/// must treat as unobserved, never as zero.
#[must_use]
pub fn sampled_process_resident_v1() -> Option<ProcessResidentSampleV1> {
    #[cfg(target_os = "linux")]
    {
        let mut sample = process_resident_sample_from_status_v1(
            &std::fs::read_to_string("/proc/self/status").ok()?,
        )?;
        sample.cgroup_committed_bytes =
            cgroup_committed_bytes_v1(Path::new(PROC_SELF_CGROUP_V1), Path::new(CGROUP_V2_ROOT_V1));
        Some(sample)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Unreclaimable bytes, for growth measurement. Admission publishes
/// [`ProcessResidentSampleV1::admission_bytes`] instead.
#[must_use]
pub fn sampled_process_resident_bytes_v1() -> Option<u64> {
    sampled_process_resident_v1().map(|sample| sample.unreclaimable_bytes)
}

/// Where a pressure cell reads the process's resident set.
pub type ProcessResidentSamplerV1 = dyn Fn() -> Option<ProcessResidentSampleV1> + Send + Sync;

/// Share of the last ten seconds, in percent, that some task in this process's
/// cgroup stalled on memory, at or above which the daemon sheds retained state
/// even while RSS is under its watermark. Under `MemoryHigh` the kernel
/// reclaims by stalling the cgroup; a sustained tenth of wall time lost to
/// that means retained caches are costing serving latency.
pub const RESIDENT_MEMORY_PSI_SOME_AVG10_SHED_PERCENT_V1: f64 = 10.0;

/// PSI memory `some avg10` for this process's cgroup (`memory.pressure`), or
/// the host's (`/proc/pressure/memory`) when the cgroup does not expose it.
/// `None` where the kernel has no PSI; callers treat that as unobserved.
#[must_use]
pub fn sampled_memory_pressure_some_avg10_v1() -> Option<f64> {
    let cgroup = cgroup_v2_process_directory_v1(
        Path::new(PROC_SELF_CGROUP_V1),
        Path::new(CGROUP_V2_ROOT_V1),
    )
    .map(|directory| directory.join("memory.pressure"));
    cgroup
        .into_iter()
        .chain(std::iter::once(std::path::PathBuf::from(
            "/proc/pressure/memory",
        )))
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| psi_some_avg10_v1(&text))
}

fn psi_some_avg10_v1(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
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
/// One dedicated reader samples the process (`/proc/self/status` on Linux),
/// publishes the `daemon.process.resident_bytes` gauge, and feeds this cell
/// the unreclaimable bytes. Admission re-measures through the same sampler;
/// there is no second parser or publisher.
pub struct ResidentMemoryPressureV1 {
    limit_bytes: NonZeroU64,
    high_watermark_bytes: u64,
    low_watermark_bytes: u64,
    observed_bytes: AtomicU64,
    observed: AtomicBool,
    over_budget: AtomicBool,
    state: ProfiledMutex<ResidentMemoryPressureReclaimerStateV1>,
    sampler: Arc<ProcessResidentSamplerV1>,
    checkpoint_epoch: Instant,
    /// Microseconds after `checkpoint_epoch` before which a checkpoint keeps
    /// the standing observation; `u64::MAX` while one checkpoint is reading.
    next_checkpoint_sample_micros: AtomicU64,
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
        Self::with_reclaim_line(limit_bytes, None, Arc::new(sampled_process_resident_v1))
    }

    /// A cell that reads the process through `sampler` instead of the kernel.
    #[must_use]
    pub fn with_sampler(limit_bytes: NonZeroU64, sampler: Arc<ProcessResidentSamplerV1>) -> Self {
        Self::with_reclaim_line(limit_bytes, None, sampler)
    }

    /// `reclaim_watermark_bytes` is a cgroup `memory.high` that sits strictly
    /// below `limit_bytes`. It replaces the percentage high watermark so the
    /// operator's band down to `memory.max` is not discounted again. Absent,
    /// zero, or not strictly below the ceiling, the percentage watermarks stand.
    fn with_reclaim_line(
        limit_bytes: NonZeroU64,
        reclaim_watermark_bytes: Option<u64>,
        sampler: Arc<ProcessResidentSamplerV1>,
    ) -> Self {
        let percentage_high = resident_memory_watermark_bytes_v1(
            limit_bytes,
            RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1,
        );
        let percentage_low = resident_memory_watermark_bytes_v1(
            limit_bytes,
            RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1,
        )
        .min(percentage_high);
        let (high_watermark_bytes, low_watermark_bytes) = match reclaim_watermark_bytes {
            Some(reclaim) if reclaim > 0 && reclaim < limit_bytes.get() => {
                let low = u64::try_from(
                    u128::from(reclaim)
                        * u128::from(RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1)
                        / u128::from(RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1),
                )
                .unwrap_or(u64::MAX)
                .min(reclaim);
                (reclaim, low)
            }
            _ => (percentage_high, percentage_low),
        };
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
            sampler,
            checkpoint_epoch: Instant::now(),
            next_checkpoint_sample_micros: AtomicU64::new(0),
        }
    }

    /// Read the process and publish its admission bytes. `None` when the
    /// process cannot be read, which leaves the last observation standing.
    pub fn sample_and_publish(
        &self,
    ) -> Option<(ProcessResidentSampleV1, ResidentMemoryPressureStateV1)> {
        let sample = (self.sampler)()?;
        Some((
            sample,
            self.publish_observed_resident_bytes(sample.admission_bytes()),
        ))
    }

    /// Publish a fresh admission sample without running pressure reclaimers.
    ///
    /// Capture and graph checkpoints run on the indexing pool. A reclaimer
    /// sheds retained owners and trims the allocator; doing that on a pool
    /// thread once RSS crosses the watermark overflows that thread's stack.
    /// The latch is what stops the allocating pass. [`Self::sample_and_publish`]
    /// remains the path that reclaims, from admission and the maintenance sampler.
    ///
    /// At most one checkpoint per [`RESIDENT_MEMORY_CHECKPOINT_SAMPLE_INTERVAL_V1`]
    /// reads the process; the others, and any checkpoint racing that read,
    /// answer the standing observation.
    pub fn sample_for_checkpoint(&self) -> Option<ResidentMemoryPressureStateV1> {
        let now = self.checkpoint_micros();
        let next = self.next_checkpoint_sample_micros.load(Ordering::Acquire);
        if now < next
            || self
                .next_checkpoint_sample_micros
                .compare_exchange(next, u64::MAX, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Some(self.state());
        }
        let sample = (self.sampler)();
        hotpath::gauge!("daemon.memory.checkpoint_samples_total").inc(1_u64);
        let interval = u64::try_from(RESIDENT_MEMORY_CHECKPOINT_SAMPLE_INTERVAL_V1.as_micros())
            .unwrap_or(u64::MAX);
        self.next_checkpoint_sample_micros.store(
            self.checkpoint_micros().saturating_add(interval),
            Ordering::Release,
        );
        self.publish_observation(sample?.admission_bytes());
        self.publish_over_budget_gauge();
        Some(self.state())
    }

    fn checkpoint_micros(&self) -> u64 {
        u64::try_from(self.checkpoint_epoch.elapsed().as_micros()).unwrap_or(u64::MAX - 1)
    }

    /// [`Self::sample_and_publish`] reduced to the admission bytes: the
    /// post-reclaim observation, or zero when the process cannot be read.
    pub fn measure_admission_bytes(&self) -> u64 {
        self.sample_and_publish().map_or(0, |(sample, state)| {
            state
                .observed_bytes()
                .unwrap_or_else(|| sample.admission_bytes())
        })
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
        let authority = read_resident_memory_authority_v1();
        Arc::new(ResidentMemoryPressureV1::with_reclaim_line(
            authority.limit_bytes,
            authority.reclaim_watermark_bytes,
            Arc::new(sampled_process_resident_v1),
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
/// Allocators keep freed pages for reuse: glibc inside its per-thread arenas
/// (indexing a 68 MB source tree on four cores measured 8.5 GB of arena
/// system memory with 3.5 GB live, and one `malloc_trim(0)` returned 3.9 GB),
/// mimalloc in pages it purges only after a delay or on collection. Measured
/// RSS is what admission trusts, so those pages refuse real work until the
/// allocator is asked for them.
///
/// A mimalloc global allocator serves only Rust allocations. `SQLite`,
/// tree-sitter, and libgit2 call `malloc` directly, so glibc's arenas are
/// trimmed after the installed release as well.
#[must_use]
pub fn release_process_allocator_memory_v1() -> ProcessAllocatorTrimV1 {
    measured_trim(|| {
        let released = installed_process_allocator_release_v1()
            .map(|release| (release.release)())
            .is_some();
        glibc_trim() || released
    })
}

/// Return freed glibc arena pages to the kernel without the installed release.
///
/// C-library churn (`SQLite` statements and caches on the store writers,
/// tree-sitter parses on the index workers) accumulates between the events
/// that run the full release, so the daemon runs this on its resident-memory
/// sampling cadence. It never waits on a busy worker pool.
#[must_use]
pub fn release_c_library_heap_v1() -> ProcessAllocatorTrimV1 {
    measured_trim(glibc_trim)
}

fn measured_trim(trim: impl FnOnce() -> bool) -> ProcessAllocatorTrimV1 {
    let before_bytes = sampled_process_resident_bytes_v1();
    let trimmed = trim();
    let after_bytes = sampled_process_resident_bytes_v1();
    let trim = ProcessAllocatorTrimV1 {
        trimmed,
        before_bytes,
        after_bytes,
    };
    hotpath::gauge!("daemon.memory.allocator_trim_released_bytes").set(trim.released_bytes());
    trim
}

fn glibc_trim() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc
        // maintenance call that takes no pointers and invalidates no live
        // allocation; it only advises the kernel about pages the allocator
        // no longer uses.
        unsafe { libc::malloc_trim(0) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
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

/// Retained owners are the largest reclaimable state, so the inventory sheds
/// first and later reclaimers (the allocator trim last) return what it freed.
pub const RESIDENT_OWNERS_PRESSURE_PRIORITY_V1: u32 = 0;

/// Shed retained owners in [`RESIDENT_OWNER_SHED_ORDER_V1`] whenever `pressure`
/// reaches its high watermark, until the measured excess is freed.
pub fn register_resident_owners_pressure_reclaimer_v1(
    pressure: &Arc<ResidentMemoryPressureV1>,
    owners: &Arc<ResidentOwnersV1>,
) -> Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1> {
    let owners = Arc::downgrade(owners);
    pressure.register_pressure_reclaimer(
        RESIDENT_OWNERS_PRESSURE_PRIORITY_V1,
        Arc::new(move |request| {
            let Some(owners) = owners.upgrade() else {
                return 0;
            };
            owners
                .shed(request.excess_bytes, std::time::Instant::now())
                .iter()
                .map(|released| {
                    log_resident_owner_release_v1(released);
                    released.bytes.measured().unwrap_or(0)
                })
                .fold(0, u64::saturating_add)
        }),
    )
}

/// The one operator log line for a retained-owner release.
pub fn log_resident_owner_release_v1(released: &ResidentOwnerReleasedV1) {
    tracing::info!(
        event = "resident_owner_released",
        project_id = released.scope.project_id.as_str(),
        worktree_id = released.scope.worktree_id.as_str(),
        kind = released.kind.as_str(),
        holding = released.holding.as_str(),
        bytes = released.bytes.measured(),
        cause = match released.cause {
            ResidentOwnerReleaseCauseV1::Idle => "idle",
            ResidentOwnerReleaseCauseV1::Pressure => "pressure",
        },
        "released retained memory"
    );
}

static PROCESS_RESIDENT_OWNERS_PRESSURE_REGISTRATION_V1: OnceLock<
    Result<ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureRegistrationFailureV1>,
> = OnceLock::new();

/// Bind the process inventory to the process pressure cell, once.
pub fn install_process_resident_owners_pressure_reclaimer_v1()
-> Result<(), ResidentMemoryPressureRegistrationFailureV1> {
    PROCESS_RESIDENT_OWNERS_PRESSURE_REGISTRATION_V1
        .get_or_init(|| {
            register_resident_owners_pressure_reclaimer_v1(
                process_resident_memory_pressure_v1(),
                process_resident_owners_v1(),
            )
        })
        .as_ref()
        .map(|_| ())
        .map_err(|failure| *failure)
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

    /// Refuse growth that exceeds measured RSS headroom or its pressure latch.
    ///
    /// Only *new* admissions are refused. Nothing already reserved is revoked,
    /// shrunk, or released by this path: the reservation guards outlive
    /// pressure exactly as before. Nominal RSS must leave room under the hard
    /// limit for the allocation; an existing pressure latch still clears when a
    /// later sample falls to the low watermark.
    fn observed_over_budget_refusal(
        &self,
        requested_bytes: NonZeroU64,
    ) -> Option<ResidentMemoryAdmissionFailureV1> {
        let pressure = self.pressure.state();
        let observed_bytes = pressure.observed_bytes()?;
        let high_watermark_bytes = self.pressure.high_watermark_bytes();
        if requested_bytes.get() <= RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1
            || (!pressure.is_over_budget()
                && requested_bytes.get()
                    <= self.pressure.limit_bytes().saturating_sub(observed_bytes))
        {
            return None;
        }
        hotpath::gauge!("daemon.memory.admission_refused").inc(1.0);
        hotpath::gauge!("runtime_core.resident.refusals").inc(1.0);
        Some(ResidentMemoryAdmissionFailureV1::ObservedOverBudget {
            observed_bytes,
            limit_bytes: self.pressure.limit_bytes(),
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

    /// Move one reservation's contribution onto `to_component` and keep only
    /// `measured_bytes`, under the same lock.
    ///
    /// [`Self::reserve`] re-checks measured RSS. Dropping a charge and
    /// reserving again is a gap: an overlapping consumer can sit on the
    /// watermark and the new admission is refused even though these bytes
    /// were already held. The ledger move does not ask for a new admission.
    fn transfer_component(
        &self,
        from: &ResidentMemoryKeyV1,
        to_component: ResidentMemoryComponentIdV1,
        reserved_bytes: u64,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        if measured_bytes > reserved_bytes {
            return Err(ResidentMemoryAdjustmentFailureV1 {
                reserved_bytes,
                measured_bytes,
            });
        }
        let mut state = self.lock_state();
        let remove_source = {
            let Some(charge) = state.charges.get_mut(from) else {
                return Err(ResidentMemoryAdjustmentFailureV1 {
                    reserved_bytes,
                    measured_bytes,
                });
            };
            if *charge < reserved_bytes {
                return Err(ResidentMemoryAdjustmentFailureV1 {
                    reserved_bytes: *charge,
                    measured_bytes,
                });
            }
            *charge -= reserved_bytes;
            *charge == 0
        };
        if remove_source {
            state.charges.remove(from);
        }
        let released_bytes = reserved_bytes - measured_bytes;
        state.used_bytes -= released_bytes;
        hotpath::gauge!("runtime_core.resident.used_bytes").set(state.used_bytes as f64);
        if measured_bytes > 0 {
            let mut to = from.clone();
            to.component = to_component;
            *state.charges.entry(to).or_default() += measured_bytes;
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

    /// Charge overlapping work to this reservation's exact owner. The new
    /// guard releases only its additional bytes, leaving this charge intact.
    pub fn reserve_additional(
        &self,
        requested_bytes: NonZeroU64,
    ) -> Result<Self, ResidentMemoryAdmissionFailureV1> {
        self.authority.pressure().measure_admission_bytes();
        self.authority.reserve(self.key.clone(), requested_bytes)
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

    /// Keep this charge and name it `component`, shrinking to `measured_bytes`
    /// when the held amount is larger.
    ///
    /// The bytes stay in the ledger for the whole move. Callers that instead
    /// drop this reservation and [`ProcessResidentMemoryV1::reserve`] the
    /// destination open a gap where measured RSS can refuse a charge that
    /// was already admitted.
    pub fn transfer_component(
        &mut self,
        component: ResidentMemoryComponentIdV1,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        self.authority.transfer_component(
            &self.key,
            component,
            self.reserved_bytes,
            measured_bytes,
        )?;
        self.key.component = component;
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
