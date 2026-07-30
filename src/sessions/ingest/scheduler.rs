use std::path::{Path, PathBuf};

use tracedecay_store::ParseOffset;

use crate::application::host_admission::DEFAULT_MAX_RECORDS;
use crate::sessions::SessionProvider;
use crate::sessions::snapshot_observation::MAX_SNAPSHOT_CAPTURE_UNIT_BYTES;
use crate::sessions::source::{
    MAX_JSONL_RECORD_BYTES, TranscriptCursorKey, TranscriptSource,
};
use crate::store::TranscriptIngestStore;

use super::failure::{
    IngestPassBounds, IngestPassCoverage, RoundRobinAdmission, plan_round_robin_admission,
};

/// Durable fair-rotation cursor for profile-wide provider catch-up passes.
pub(super) const USER_INGEST_PROVIDER_FRONTIER_KEY: &str =
    "tracedecay-internal:user-ingest-provider-frontier:v1";

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

pub(super) async fn read_ingest_frontier<S: TranscriptIngestStore>(
    store: &S,
    key: &str,
) -> Option<u64> {
    match store.get_parse_offset(Path::new(key)).await {
        Ok(offset) => Some(offset.byte_offset),
        Err(_) => None,
    }
}

pub(super) async fn write_ingest_frontier<S: TranscriptIngestStore>(
    store: &S,
    key: &str,
    previous: u64,
    advance: usize,
) -> bool {
    if advance == 0 {
        return false;
    }
    let processed = u64::try_from(advance).unwrap_or(u64::MAX);
    store
        .advance_parse_offset_monotonic(
            Path::new(key),
            ParseOffset {
                byte_offset: previous.saturating_add(processed),
                mtime: 0,
                file_id: 1,
            },
        )
        .await
        .is_ok()
}
