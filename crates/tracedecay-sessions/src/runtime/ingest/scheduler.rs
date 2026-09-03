use std::path::Path;

use tracedecay_store::ParseOffset;

use crate::admission::DEFAULT_MAX_RECORDS;
use crate::runtime::SessionProvider;
use crate::runtime::codex::CodexDiscoveryFrontier;
use crate::runtime::snapshot_observation::MAX_SNAPSHOT_CAPTURE_UNIT_BYTES;
use crate::runtime::source::{MAX_JSONL_RECORD_BYTES, TranscriptIngestResult};
use crate::runtime::store_port::TranscriptIngestStore;

use super::failure::{
    IngestPassBounds, IngestPassCoverage, RoundRobinAdmission, plan_round_robin_admission,
};

/// Durable fair-rotation cursor for profile-wide provider catch-up passes.
pub const USER_INGEST_PROVIDER_FRONTIER_KEY: &str =
    "tracedecay-internal:user-ingest-provider-frontier:v1";

/// Durable fair-rotation cursor for project-scoped provider catch-up passes.
/// Lives in the project's transcript store, so a daemon restart resumes the
/// sweep at the provider after the last persisted pass instead of restarting
/// the rotation at the first provider.
pub const PROJECT_INGEST_PROVIDER_FRONTIER_KEY: &str =
    "tracedecay-internal:project-ingest-provider-frontier:v1";

/// Durable versioned Codex discovery frontier for the profile corpus.
pub const USER_INGEST_CODEX_HISTORY_FRONTIER_KEY: &str =
    "tracedecay-internal:user-ingest-codex-history-frontier:v2";
pub const USER_INGEST_CODEX_HISTORY_EPOCH_KEY: &str =
    "tracedecay-internal:user-ingest-codex-history-epoch:v2";

/// Production bounds for transcript multi-source passes (discovery/queue/work).
pub(super) fn default_ingest_pass_bounds() -> IngestPassBounds {
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
    SessionProvider::Kimi,
    SessionProvider::OpenCode,
    SessionProvider::Cline,
    SessionProvider::RooCode,
    SessionProvider::Kilo,
    SessionProvider::Vibe,
];

pub(super) fn plan_provider_rotation_admission(
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

#[hotpath::measure(label = "sessions.ingest.frontier_read", future = true)]
pub(super) async fn read_ingest_frontier<S: TranscriptIngestStore>(
    store: &S,
    key: &str,
) -> Option<u64> {
    match store.get_parse_offset(Path::new(key)).await {
        Ok(offset) => Some(offset.byte_offset),
        Err(_) => None,
    }
}

#[hotpath::measure(label = "sessions.ingest.frontier_write", future = true)]
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

#[hotpath::measure(label = "sessions.ingest.codex_frontier_read", future = true)]
pub(super) async fn read_codex_discovery_frontier<S: TranscriptIngestStore>(
    store: &S,
) -> TranscriptIngestResult<CodexDiscoveryFrontier> {
    let stored = store
        .get_parse_offset(Path::new(USER_INGEST_CODEX_HISTORY_FRONTIER_KEY))
        .await?;
    let epoch = store
        .get_parse_offset(Path::new(USER_INGEST_CODEX_HISTORY_EPOCH_KEY))
        .await?;
    CodexDiscoveryFrontier::from_parse_offsets(stored, epoch)
}

#[hotpath::measure(label = "sessions.ingest.codex_frontier_write", future = true)]
pub(super) async fn write_codex_discovery_frontier<S: TranscriptIngestStore>(
    store: &S,
    expected: CodexDiscoveryFrontier,
    frontier: CodexDiscoveryFrontier,
) -> TranscriptIngestResult<()> {
    let (frontier_offset, epoch_offset) = frontier.into_parse_offsets();
    let (expected_frontier, expected_epoch) = expected.into_parse_offsets();
    store
        .replace_parse_offset_pair(
            (
                Path::new(USER_INGEST_CODEX_HISTORY_FRONTIER_KEY),
                expected_frontier,
                frontier_offset,
            ),
            (
                Path::new(USER_INGEST_CODEX_HISTORY_EPOCH_KEY),
                expected_epoch,
                epoch_offset,
            ),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tempfile::TempDir;
    use tracedecay_store::{
        ParseOffset, TranscriptStore, TranscriptStoreError, TranscriptStoreResult,
        TranscriptWriteBatch, TranscriptWriteKind,
    };

    use crate::runtime::git_correlation::{CommitSessionRecord, SpanObservation};
    use crate::runtime::source::TranscriptDiscoveryBounds;
    use crate::runtime::store_port::TranscriptIngestStore;
    use crate::runtime::{SessionProvider, SessionRecord, codex};

    use super::{
        USER_CATCH_UP_PROVIDERS, read_codex_discovery_frontier, write_codex_discovery_frontier,
    };

    #[derive(Default)]
    struct RestartableStore {
        offsets: Mutex<BTreeMap<PathBuf, ParseOffset>>,
        fail_reads: AtomicBool,
        writes: AtomicUsize,
    }

    impl TranscriptStore for RestartableStore {
        fn get_parse_offset(
            &self,
            cursor_path: &Path,
        ) -> impl std::future::Future<Output = TranscriptStoreResult<ParseOffset>> + Send {
            let result = if self.fail_reads.load(Ordering::Relaxed) {
                Err(TranscriptStoreError::Storage {
                    operation: "get_parse_offset",
                    source: Box::new(std::io::Error::other("injected frontier read failure")),
                })
            } else {
                Ok(*self
                    .offsets
                    .lock()
                    .expect("offset lock")
                    .get(cursor_path)
                    .unwrap_or(&ParseOffset::default()))
            };
            std::future::ready(result)
        }

        fn persist_transcript_batch(
            &self,
            batch: TranscriptWriteBatch,
        ) -> impl std::future::Future<Output = TranscriptStoreResult<()>> + Send {
            let (cursor_path, kind) = batch.into_parts();
            let (expected, next) = match kind {
                TranscriptWriteKind::AdvanceOffset {
                    expected_offset,
                    next_offset,
                }
                | TranscriptWriteKind::Upsert {
                    expected_offset,
                    next_offset,
                    ..
                } => (expected_offset, next_offset),
            };
            let mut offsets = self.offsets.lock().expect("offset lock");
            let actual = *offsets.get(&cursor_path).unwrap_or(&ParseOffset::default());
            let result = if actual == expected {
                offsets.insert(cursor_path, next);
                self.writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            } else {
                Err(TranscriptStoreError::Conflict {
                    cursor_path,
                    expected,
                    actual,
                })
            };
            std::future::ready(result)
        }
    }

    impl TranscriptIngestStore for RestartableStore {
        fn replace_parse_offset_pair(
            &self,
            first: (&Path, ParseOffset, ParseOffset),
            second: (&Path, ParseOffset, ParseOffset),
        ) -> impl std::future::Future<Output = TranscriptStoreResult<()>> + Send {
            let mut offsets = self.offsets.lock().expect("offset lock");
            let first_actual = *offsets.get(first.0).unwrap_or(&ParseOffset::default());
            let second_actual = *offsets.get(second.0).unwrap_or(&ParseOffset::default());
            let result = if first_actual != first.1 {
                Err(TranscriptStoreError::Conflict {
                    cursor_path: first.0.to_path_buf(),
                    expected: first.1,
                    actual: first_actual,
                })
            } else if second_actual != second.1 {
                Err(TranscriptStoreError::Conflict {
                    cursor_path: second.0.to_path_buf(),
                    expected: second.1,
                    actual: second_actual,
                })
            } else {
                offsets.insert(first.0.to_path_buf(), first.2);
                offsets.insert(second.0.to_path_buf(), second.2);
                self.writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            };
            std::future::ready(result)
        }

        fn get_session(
            &self,
            _provider: &str,
            _session_id: &str,
        ) -> impl std::future::Future<Output = TranscriptStoreResult<Option<SessionRecord>>> + Send
        {
            std::future::ready(Ok(None))
        }

        fn persist_transcript_batch_with_git_evidence(
            &self,
            batch: TranscriptWriteBatch,
            _commit_records: &[CommitSessionRecord],
            _span_observations: &[SpanObservation],
        ) -> impl std::future::Future<Output = TranscriptStoreResult<()>> + Send {
            self.persist_transcript_batch(batch)
        }
    }

    fn write_dated_rollout(home: &Path, name: &str) -> PathBuf {
        let dir = home.join(".codex/sessions/2026/08/17");
        std::fs::create_dir_all(&dir).expect("session directory");
        let path = dir.join(format!("rollout-{name}.jsonl"));
        std::fs::write(&path, b"{}\n").expect("rollout");
        path
    }

    #[test]
    fn user_catch_up_schedules_every_final_host() {
        for provider in [
            SessionProvider::Claude,
            SessionProvider::Codex,
            SessionProvider::Cursor,
            SessionProvider::Kimi,
            SessionProvider::OpenCode,
        ] {
            assert!(USER_CATCH_UP_PROVIDERS.contains(&provider));
        }
    }

    #[tokio::test]
    async fn profile_codex_frontier_converges_beyond_budget_and_survives_restart() {
        let temp = TempDir::new().expect("tempdir");
        let mut expected = BTreeSet::new();
        for index in 0..41 {
            expected.insert(write_dated_rollout(temp.path(), &format!("{index:02}")));
        }
        let source = codex::CodexSource::with_home(temp.path());
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let store = RestartableStore::default();
        let mut covered = BTreeSet::new();
        let mut discovery_state = codex::CodexDiscoveryState::default();

        for _ in 0..128 {
            let frontier = read_codex_discovery_frontier(&store)
                .await
                .expect("read frontier");
            let pass = source
                .discover_transcript_paths_with_state(bounds, frontier, &mut discovery_state)
                .expect("discover");
            covered.extend(pass.report.paths.iter().cloned());
            write_codex_discovery_frontier(&store, frontier, pass.next_frontier)
                .await
                .expect("persist frontier");
            discovery_state.acknowledge();
            if pass.next_frontier.is_complete() {
                break;
            }
        }
        assert_eq!(covered, expected);

        let reloaded = read_codex_discovery_frontier(&store)
            .await
            .expect("reload frontier");
        assert!(reloaded.is_complete());
        let idle = codex::CodexSource::with_home(temp.path())
            .discover_transcript_paths_with_state(bounds, reloaded, &mut discovery_state)
            .expect("restart discovery");
        assert!(idle.report.paths.is_empty());
        assert!(idle.next_frontier.is_complete());
        // Production consumers acknowledge every delivered pass (idle ones
        // included); an unacknowledged pass replays verbatim on the next
        // discovery, which would mask the addition below.
        discovery_state.acknowledge();

        let added = write_dated_rollout(temp.path(), "after-restart");
        // Change detection restarts discovery through a validation sweep that
        // measures the corpus before it emits, so the addition is delivered
        // across the following bounded passes, not in the detection pass
        // itself.
        let awakened_source = codex::CodexSource::with_home(temp.path());
        let detection = awakened_source
            .discover_transcript_paths_with_state(bounds, reloaded, &mut discovery_state)
            .expect("addition detection");
        assert!(!detection.next_frontier.is_complete());
        write_codex_discovery_frontier(&store, reloaded, detection.next_frontier)
            .await
            .expect("persist detection frontier");
        discovery_state.acknowledge();
        let mut awakened_paths: BTreeSet<_> = detection.report.paths.iter().cloned().collect();
        for _ in 0..128 {
            let frontier = read_codex_discovery_frontier(&store)
                .await
                .expect("read awakened frontier");
            let pass = awakened_source
                .discover_transcript_paths_with_state(bounds, frontier, &mut discovery_state)
                .expect("addition discovery");
            awakened_paths.extend(pass.report.paths.iter().cloned());
            write_codex_discovery_frontier(&store, frontier, pass.next_frontier)
                .await
                .expect("persist awakened frontier");
            discovery_state.acknowledge();
            if pass.next_frontier.is_complete() {
                break;
            }
        }
        assert!(awakened_paths.contains(&added));
        assert!(
            read_codex_discovery_frontier(&store)
                .await
                .expect("final frontier")
                .is_complete()
        );
    }

    #[tokio::test]
    async fn profile_codex_frontier_read_error_performs_no_persistence() {
        let store = RestartableStore::default();
        store.fail_reads.store(true, Ordering::Relaxed);

        let result = read_codex_discovery_frontier(&store).await;

        assert!(result.is_err());
        assert_eq!(store.writes.load(Ordering::Relaxed), 0);
    }
}
