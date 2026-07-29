use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_store::StoreShardScopeV1;

use crate::application::host_admission::DEFAULT_MAX_RECORDS;
use crate::application::observation::ObservationCancellation;
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::SessionProvider;
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::snapshot_observation::MAX_SNAPSHOT_CAPTURE_UNIT_BYTES;
use crate::sessions::source::{
    MAX_JSONL_RECORD_BYTES, TranscriptCursorKey, TranscriptDiscoveryBounds, TranscriptSource,
    path_byte_len, try_ingest_source_with_store,
};
use crate::store::GlobalDbTranscriptStore;

use super::failure::{
    IngestPassBounds, IngestPassCoverage, IngestPassOutcome, RoundRobinAdmission,
    TranscriptCatchUpFailure, allocate_pass_byte_budgets, classify_transcript_ingest_failure,
    plan_round_robin_admission, scheduling_write_required,
};

/// Durable fair-rotation cursor for project file-transcript multi-source passes.
pub(super) const TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY: &str =
    "tracedecay-internal:transcript-ingest-source-frontier:v1";

/// Durable fair-rotation cursor for profile-wide provider catch-up passes.
pub(super) const USER_INGEST_PROVIDER_FRONTIER_KEY: &str =
    "tracedecay-internal:user-ingest-provider-frontier:v1";

const MAX_TRANSIENT_INGEST_FRONTIERS: usize = 256;

#[derive(Clone, PartialEq, Eq)]
struct TransientIngestAuthority {
    brain_id: BrainId,
    profile_id: UserProfileId,
    project_id: ProjectId,
    providers: Vec<&'static str>,
}

impl TransientIngestAuthority {
    fn new(
        db: &RegisteredGlobalDb,
        project_id: &ProjectId,
        sources: &[Box<dyn TranscriptSource>],
    ) -> Self {
        Self {
            brain_id: db.binding().shard_id.brain_id.clone(),
            profile_id: db.binding().shard_id.profile_id.clone(),
            project_id: project_id.clone(),
            providers: sources.iter().map(|source| source.provider()).collect(),
        }
    }
}

static TRANSIENT_INGEST_FRONTIERS: OnceLock<Mutex<VecDeque<(TransientIngestAuthority, u64)>>> =
    OnceLock::new();

fn transient_ingest_frontier(authority: &TransientIngestAuthority) -> u64 {
    let frontiers = TRANSIENT_INGEST_FRONTIERS.get_or_init(|| Mutex::new(VecDeque::new()));
    let frontiers = frontiers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    frontiers
        .iter()
        .find_map(|(candidate, frontier)| (candidate == authority).then_some(*frontier))
        .unwrap_or(0)
}

fn set_transient_ingest_frontier(authority: &TransientIngestAuthority, frontier: u64) {
    let frontiers = TRANSIENT_INGEST_FRONTIERS.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut frontiers = frontiers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = frontiers
        .iter()
        .position(|(candidate, _)| candidate == authority)
    {
        frontiers.remove(index);
    }
    if frontier == 0 {
        return;
    }
    if frontiers.len() >= MAX_TRANSIENT_INGEST_FRONTIERS {
        frontiers.pop_front();
    }
    frontiers.push_back((authority.clone(), frontier));
}

/// Production bounds for transcript multi-source passes (discovery/queue/work).
pub(crate) fn default_ingest_pass_bounds() -> IngestPassBounds {
    let jsonl_bytes = u64::try_from(MAX_JSONL_RECORD_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let bytes_per_unit = jsonl_bytes.max(MAX_SNAPSHOT_CAPTURE_UNIT_BYTES);
    IngestPassBounds {
        discovered_units: DEFAULT_MAX_RECORDS,
        units_per_pass: 64,
        units_per_source: 32,
        queue_depth: 256,
        bytes_per_unit,
        bytes_per_pass: bytes_per_unit.saturating_mul(8),
        retries: 0,
    }
}

pub(super) fn merge_project_provider_backpressure(
    coverage: IngestPassCoverage,
    source_units: u64,
    provider_units: u64,
    provider_deferred: u64,
) -> IngestPassCoverage {
    if provider_deferred == 0 {
        return coverage;
    }
    let (admitted_units, rejected_units) = match coverage {
        IngestPassCoverage::Complete => (
            source_units.saturating_add(provider_units),
            provider_deferred,
        ),
        IngestPassCoverage::Partial { deferred_units } => (
            source_units.saturating_add(provider_units),
            deferred_units.saturating_add(provider_deferred),
        ),
        IngestPassCoverage::Backpressured {
            admitted_units,
            rejected_units,
        } => (
            admitted_units.saturating_add(provider_units),
            rejected_units.saturating_add(provider_deferred),
        ),
    };
    IngestPassCoverage::Backpressured {
        admitted_units,
        rejected_units,
    }
}

/// Stable provider order for profile-wide fair catch-up rotation.
pub(super) const USER_CATCH_UP_PROVIDERS: &[SessionProvider] = &[
    SessionProvider::Codex,
    SessionProvider::Cursor,
    SessionProvider::Hermes,
    SessionProvider::Claude,
    SessionProvider::Kiro,
    SessionProvider::Cline,
    SessionProvider::RooCode,
    SessionProvider::Kilo,
    SessionProvider::Vibe,
];

pub(super) fn plan_user_provider_admission(
    selected_count: usize,
    frontier: u64,
    bounds: IngestPassBounds,
) -> RoundRobinAdmission {
    let admitted_limit = bounds
        .discovered_units
        .min(bounds.units_per_pass)
        .min(bounds.queue_depth);
    plan_round_robin_admission(selected_count, frontier, admitted_limit)
}

pub(super) fn finish_user_provider_coverage(
    coverage: IngestPassCoverage,
    selected: usize,
    attempted: usize,
    providers_deferred: usize,
) -> IngestPassCoverage {
    if providers_deferred == 0 && attempted >= selected {
        return coverage;
    }
    let rejected = selected
        .saturating_sub(attempted)
        .saturating_add(providers_deferred);
    if attempted == 0
        || providers_deferred > 0
        || matches!(coverage, IngestPassCoverage::Backpressured { .. })
    {
        IngestPassCoverage::Backpressured {
            admitted_units: u64::try_from(attempted).unwrap_or(u64::MAX),
            rejected_units: u64::try_from(rejected).unwrap_or(u64::MAX),
        }
    } else {
        IngestPassCoverage::Partial {
            deferred_units: u64::try_from(rejected).unwrap_or(u64::MAX),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DiscoveredIngestUnit {
    pub source_id: String,
    pub path: PathBuf,
    pub source_index: usize,
}

/// Restrict an existing [`TranscriptSource`] to a single admitted path so fair
/// rotation can interleave work units without a fallback writer or private sink.
pub(super) struct SinglePathSource<'a> {
    inner: &'a dyn TranscriptSource,
    path: PathBuf,
}

impl<'a> SinglePathSource<'a> {
    pub(super) fn new(inner: &'a dyn TranscriptSource, path: PathBuf) -> Self {
        Self { inner, path }
    }
}

impl TranscriptSource for SinglePathSource<'_> {
    fn provider(&self) -> &'static str {
        self.inner.provider()
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }

    fn cursor_key(&self, transcript_path: &Path) -> TranscriptCursorKey {
        self.inner.cursor_key(transcript_path)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: crate::sessions::shared::StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<crate::sessions::source::ParsedTranscript> {
        self.inner
            .parse_new(path, prev, project_root, max_new_bytes)
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: crate::sessions::shared::StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> crate::sessions::source::TranscriptIngestResult<
        Option<crate::sessions::source::ParsedTranscript>,
    > {
        self.inner
            .try_parse_new(path, prev, project_root, max_new_bytes)
    }
}

/// Discover path work units in one stable fair canonical order.
///
/// Each source is asked for a deterministic
/// [`TranscriptSource::discover_transcript_paths_page`] under
/// [`TranscriptDiscoveryBounds`] derived from `bounds`. Retained units are
/// capped again by file count, path bytes, and cumulative discovery bytes
/// before `DiscoveredIngestUnit` materialization. Truncation becomes deferred
/// work so callers emit typed [`IngestPassCoverage::Backpressured`] without
/// embedding oversized paths in failure payloads.
#[derive(Debug)]
struct IngestDiscoveryPage {
    units: Vec<DiscoveredIngestUnit>,
    deferred: usize,
    frontier_base: u64,
}

#[cfg(test)]
pub(super) fn discover_ingest_units(
    sources: &[Box<dyn TranscriptSource>],
    project_root: &Path,
    bounds: IngestPassBounds,
    frontier_offset: u64,
) -> (Vec<DiscoveredIngestUnit>, usize) {
    let page = discover_ingest_page(sources, project_root, bounds, frontier_offset);
    (page.units, page.deferred)
}

fn discover_ingest_page(
    sources: &[Box<dyn TranscriptSource>],
    project_root: &Path,
    bounds: IngestPassBounds,
    frontier_offset: u64,
) -> IngestDiscoveryPage {
    let mut page = discover_ingest_page_at(sources, project_root, bounds, frontier_offset);
    if frontier_offset > 0 && page.units.is_empty() && page.deferred > 0 {
        page = discover_ingest_page_at(sources, project_root, bounds, 0);
        page.frontier_base = 0;
    }
    page
}

fn discover_ingest_page_at(
    sources: &[Box<dyn TranscriptSource>],
    project_root: &Path,
    bounds: IngestPassBounds,
    frontier_offset: u64,
) -> IngestDiscoveryPage {
    let discovery_bounds =
        TranscriptDiscoveryBounds::from_discovered_units(bounds.discovered_units);
    let mut per_source = Vec::with_capacity(sources.len());
    let mut discovery_truncated = false;
    let mut source_omitted = 0usize;
    let source_count = sources.len().max(1);
    let frontier = usize::try_from(frontier_offset).unwrap_or(usize::MAX);
    let base_offset = frontier / source_count;
    let remainder = frontier % source_count;
    for (source_index, source) in sources.iter().enumerate() {
        let source_offset = base_offset.saturating_add(usize::from(source_index < remainder));
        let (report, omitted) =
            source.discover_transcript_paths_page(project_root, discovery_bounds, source_offset);
        if report.is_truncated() {
            discovery_truncated = true;
        }
        let skipped_oversized =
            usize::try_from(report.skipped_oversized_entries).unwrap_or(usize::MAX);
        source_omitted = source_omitted.saturating_add(omitted.max(skipped_oversized));
        let mut seen = HashSet::with_capacity(report.paths.len());
        let paths: Vec<PathBuf> = report
            .paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        per_source.push((source_index, source.provider().to_string(), paths));
    }
    let total_paths = per_source
        .iter()
        .map(|(_, _, paths)| paths.len())
        .fold(0usize, usize::saturating_add);
    if total_paths == 0 {
        let deferred = if discovery_truncated || source_omitted > 0 {
            source_omitted.max(1)
        } else {
            0
        };
        return IngestDiscoveryPage {
            units: Vec::new(),
            deferred,
            frontier_base: frontier_offset,
        };
    }

    let max_depth = per_source
        .iter()
        .map(|(_, _, paths)| paths.len())
        .max()
        .unwrap_or(0);
    let provider_order: Vec<usize> = (remainder..per_source.len()).chain(0..remainder).collect();
    let mut units = Vec::with_capacity(bounds.discovered_units.min(total_paths));
    let mut unit_discovery_bytes = 0u64;
    'discovery: for path_index in 0..max_depth {
        for &provider_index in &provider_order {
            let (source_index, source_id, paths) = &per_source[provider_index];
            let Some(path) = paths.get(path_index) else {
                continue;
            };
            if units.len() >= bounds.discovered_units {
                discovery_truncated = true;
                break 'discovery;
            }
            let path_bytes = path_byte_len(path);
            if path_bytes > discovery_bounds.max_path_bytes {
                source_omitted = source_omitted.saturating_add(1);
                continue;
            }
            let path_charge = u64::try_from(path_bytes).unwrap_or(u64::MAX);
            let source_charge = u64::try_from(source_id.len()).unwrap_or(u64::MAX);
            let entry_charge = path_charge.saturating_add(source_charge);
            if unit_discovery_bytes.saturating_add(entry_charge)
                > discovery_bounds.max_discovery_bytes
            {
                discovery_truncated = true;
                break 'discovery;
            }
            unit_discovery_bytes = unit_discovery_bytes.saturating_add(entry_charge);
            units.push(DiscoveredIngestUnit {
                source_id: source_id.clone(),
                path: path.clone(),
                source_index: *source_index,
            });
        }
    }
    let mut deferred = source_omitted.saturating_add(total_paths.saturating_sub(units.len()));
    if discovery_truncated {
        deferred = deferred.max(1);
    }
    IngestDiscoveryPage {
        units,
        deferred,
        frontier_base: frontier_offset,
    }
}

/// Admit one contiguous slice of the stable canonical fair order.
///
/// The durable frontier is an offset in exactly this order. No second scheduler
/// may reorder the selected slice before frontier advancement.
pub(super) fn admit_fair_ingest_units(
    units: &[DiscoveredIngestUnit],
    frontier_offset: u64,
    bounds: IngestPassBounds,
) -> (Vec<usize>, IngestPassCoverage) {
    if units.is_empty() {
        return (Vec::new(), IngestPassCoverage::Complete);
    }
    let pass_limit = bounds.units_per_pass.min(bounds.queue_depth);
    let plan = plan_round_robin_admission(units.len(), frontier_offset, pass_limit);
    let mut admitted = Vec::with_capacity(plan.admitted_indices.len());
    let mut per_source = vec![
        0usize;
        units
            .iter()
            .map(|unit| unit.source_index)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    ];
    let mut source_bound_hit = false;
    for index in plan.admitted_indices {
        let source_index = units[index].source_index;
        if per_source[source_index] >= bounds.units_per_source {
            source_bound_hit = true;
            break;
        }
        per_source[source_index] = per_source[source_index].saturating_add(1);
        admitted.push(index);
    }
    let rejected = units.len().saturating_sub(admitted.len());
    let coverage = if rejected == 0 {
        IngestPassCoverage::Complete
    } else if source_bound_hit || bounds.queue_depth < bounds.units_per_pass {
        IngestPassCoverage::Backpressured {
            admitted_units: u64::try_from(admitted.len()).unwrap_or(u64::MAX),
            rejected_units: u64::try_from(rejected).unwrap_or(u64::MAX),
        }
    } else {
        IngestPassCoverage::Partial {
            deferred_units: u64::try_from(rejected).unwrap_or(u64::MAX),
        }
    };
    (admitted, coverage)
}

pub(super) async fn read_ingest_frontier(db: &RegisteredGlobalDb, key: &str) -> Option<u64> {
    match db.get_parse_offset_result(key).await {
        Ok(Some(offset)) => Some(offset.byte_offset),
        Ok(None) => Some(0),
        Err(_) => None,
    }
}

pub(super) async fn write_ingest_frontier(
    db: &RegisteredGlobalDb,
    key: &str,
    previous: u64,
    advance: usize,
) -> bool {
    if advance == 0 {
        return false;
    }
    let processed = u64::try_from(advance).unwrap_or(u64::MAX);
    db.advance_parse_offset_result(
        key,
        crate::global_db::ParseOffset {
            byte_offset: previous.saturating_add(processed),
            mtime: 0,
            file_id: 1,
        },
    )
    .await
    .is_ok()
}

/// Bounded fair multi-source ingest with typed coverage / scheduling outcomes.
pub(crate) async fn ingest_sources_bounded(
    db: &RegisteredGlobalDb,
    project_root: &Path,
    project_id: &ProjectId,
    sources: &[Box<dyn TranscriptSource>],
    bounds: IngestPassBounds,
    cancellation: &ObservationCancellation,
) -> IngestPassOutcome {
    if db.binding().shard_id.scope
        != (StoreShardScopeV1::ProjectSessions {
            project_id: project_id.clone(),
        })
    {
        return IngestPassOutcome::failed(TranscriptCatchUpFailure::new(
            "all",
            "project_sessions_authority",
            "project_sessions_authority_mismatch",
            false,
        ));
    }
    let store = GlobalDbTranscriptStore::new(db);
    let Some(durable_frontier) =
        read_ingest_frontier(db, TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY).await
    else {
        return IngestPassOutcome::failed(TranscriptCatchUpFailure::pass_frontier_unavailable());
    };
    let transient_authority = TransientIngestAuthority::new(db, project_id, sources);
    let frontier = durable_frontier.saturating_add(transient_ingest_frontier(&transient_authority));
    let discovery = discover_ingest_page(sources, project_root, bounds, frontier);
    let units = discovery.units;
    let deferred_discovery_units = discovery.deferred;
    // Discovery already rotated the stable canonical order to `frontier`.
    let (admitted, mut coverage) = admit_fair_ingest_units(&units, 0, bounds);
    if deferred_discovery_units > 0 {
        let deferred = u64::try_from(
            deferred_discovery_units.saturating_add(units.len().saturating_sub(admitted.len())),
        )
        .unwrap_or(u64::MAX);
        coverage = IngestPassCoverage::Backpressured {
            admitted_units: u64::try_from(admitted.len()).unwrap_or(u64::MAX),
            rejected_units: deferred,
        };
    }
    let hard_backpressure = matches!(coverage, IngestPassCoverage::Backpressured { .. });

    let mut stats = TranscriptIngestStats::default();
    let mut failures = Vec::new();
    let mut units_completed = 0u64;
    let mut units_failed = 0u64;
    let mut attempted = 0usize;
    let mut cancelled = false;
    // Cursor/stat progress and terminal failure dispositions rotate the pass;
    // a successful no-op does not create scheduling state.
    let mut scheduling_progress = false;
    let budget_slots = admitted
        .len()
        .saturating_mul(bounds.retries.saturating_add(1));
    let initial_budgets = allocate_pass_byte_budgets(budget_slots, bounds);
    let mut remaining_bytes = initial_budgets
        .iter()
        .copied()
        .fold(0_u64, u64::saturating_add);

    for &index in &admitted {
        if cancellation.is_cancelled() {
            cancelled = true;
            failures.push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
        if remaining_bytes == 0 || bounds.bytes_per_unit == 0 {
            break;
        }
        let Some(unit) = units.get(index) else {
            continue;
        };
        let Some(source) = sources.get(unit.source_index).map(Box::as_ref) else {
            continue;
        };
        attempted = attempted.saturating_add(1);
        let outcome = ingest_admitted_unit(
            &store,
            db,
            source,
            &unit.path,
            project_root,
            bounds,
            &mut remaining_bytes,
        )
        .await;
        scheduling_progress |= outcome.progressed;
        stats = stats.merge(outcome.stats);
        if let Some(failure) = outcome.failure {
            // Failed source cursors/frontiers are not advanced by the store
            // path; fair rotation still consumed this slot so later sources are
            // not starved.
            failures.push(failure);
            units_failed = units_failed.saturating_add(1);
        } else if outcome.completed {
            units_completed = units_completed.saturating_add(1);
        }
        if cancellation.is_cancelled() {
            cancelled = true;
            failures.push(TranscriptCatchUpFailure::pass_cancelled());
            break;
        }
    }

    if cancelled {
        let deferred = u64::try_from(
            units
                .len()
                .saturating_sub(attempted)
                .saturating_add(deferred_discovery_units),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        coverage = match coverage {
            IngestPassCoverage::Backpressured { rejected_units, .. } => {
                IngestPassCoverage::Backpressured {
                    admitted_units: u64::try_from(attempted).unwrap_or(u64::MAX),
                    rejected_units: rejected_units.max(deferred),
                }
            }
            IngestPassCoverage::Complete | IngestPassCoverage::Partial { .. } => {
                IngestPassCoverage::Partial {
                    deferred_units: deferred,
                }
            }
        };
    } else if attempted < units.len() {
        let known_deferred = units.len().saturating_sub(attempted);
        let deferred = u64::try_from(known_deferred.saturating_add(deferred_discovery_units))
            .unwrap_or(u64::MAX);
        coverage = if attempted == 0 || hard_backpressure {
            IngestPassCoverage::Backpressured {
                admitted_units: u64::try_from(attempted).unwrap_or(u64::MAX),
                rejected_units: deferred,
            }
        } else {
            IngestPassCoverage::Partial {
                deferred_units: deferred,
            }
        };
    }

    if matches!(coverage, IngestPassCoverage::Backpressured { .. }) {
        failures.push(TranscriptCatchUpFailure::pass_backpressured());
    }

    let write = scheduling_progress && scheduling_write_required(coverage, attempted, cancelled);
    let scheduling_state_written = if write {
        write_ingest_frontier(
            db,
            TRANSCRIPT_INGEST_SOURCE_FRONTIER_KEY,
            discovery.frontier_base,
            attempted,
        )
        .await
    } else {
        false
    };
    if write && !scheduling_state_written {
        failures.push(TranscriptCatchUpFailure::pass_frontier_unavailable());
    }
    if !cancelled {
        if scheduling_state_written {
            set_transient_ingest_frontier(&transient_authority, 0);
        } else if attempted > 0 {
            let transient_frontier = discovery
                .frontier_base
                .saturating_add(u64::try_from(attempted).unwrap_or(u64::MAX))
                .saturating_sub(durable_frontier);
            set_transient_ingest_frontier(&transient_authority, transient_frontier);
        }
    }

    IngestPassOutcome {
        stats,
        failures,
        coverage,
        scheduling_state_written,
        units_admitted: u64::try_from(attempted).unwrap_or(u64::MAX),
        units_completed,
        units_failed,
        byte_bounds_enforced: true,
    }
}

/// Disposition of one admitted work unit after its bounded retry loop.
struct UnitIngestOutcome {
    stats: TranscriptIngestStats,
    failure: Option<TranscriptCatchUpFailure>,
    progressed: bool,
    completed: bool,
}

/// Ingest one admitted path unit under its byte grant, charging `remaining_bytes`
/// and reporting whether the durable cursor or stats advanced.
async fn ingest_admitted_unit(
    store: &GlobalDbTranscriptStore<'_>,
    db: &RegisteredGlobalDb,
    source: &dyn TranscriptSource,
    path: &Path,
    project_root: &Path,
    bounds: IngestPassBounds,
    remaining_bytes: &mut u64,
) -> UnitIngestOutcome {
    let single = SinglePathSource::new(source, path.to_path_buf());
    let cursor_key = source.cursor_key(path).durable_text();
    let cursor_before = db
        .get_parse_offset_result(&cursor_key)
        .await
        .map(|offset| offset.map(|offset| (offset.byte_offset, offset.mtime, offset.file_id)));
    let mut attempts = 0usize;
    loop {
        let grant = (*remaining_bytes).min(bounds.bytes_per_unit);
        if grant == 0 {
            return UnitIngestOutcome {
                stats: TranscriptIngestStats::default(),
                failure: None,
                progressed: false,
                completed: false,
            };
        }
        *remaining_bytes = remaining_bytes.saturating_sub(grant);
        match try_ingest_source_with_store(store, &single, project_root, Some(grant)).await {
            Ok(source_stats) => {
                let cursor_after = db.get_parse_offset_result(&cursor_key).await.map(|offset| {
                    offset.map(|offset| (offset.byte_offset, offset.mtime, offset.file_id))
                });
                let cursor_progress = match (&cursor_before, &cursor_after) {
                    (Ok(before), Ok(after)) => before != after,
                    _ => true,
                };
                let progressed = cursor_progress
                    || source_stats.sessions_upserted > 0
                    || source_stats.messages_upserted > 0;
                return UnitIngestOutcome {
                    stats: source_stats,
                    failure: None,
                    progressed,
                    completed: true,
                };
            }
            Err(error) => {
                attempts = attempts.saturating_add(1);
                let failure =
                    classify_transcript_ingest_failure(source.provider(), "transcript", &error);
                if failure.retryable && attempts <= bounds.retries && *remaining_bytes > 0 {
                    continue;
                }
                tracing::warn!(
                    provider = source.provider(),
                    reason_code = failure.reason_code,
                    retryable = failure.retryable,
                    "project transcript catch-up failed"
                );
                return UnitIngestOutcome {
                    stats: TranscriptIngestStats::default(),
                    failure: Some(failure),
                    progressed: true,
                    completed: false,
                };
            }
        }
    }
}
