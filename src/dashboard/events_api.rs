//! `GET /api/events` — the dashboard's typed Server-Sent Events stream.
//!
//! The PR14 frontend replaces polling with one revision-monotone SSE path (plan
//! 11 §"Finalized implementation architecture" → SSE module). Every event
//! carries stream/run identity, a monotone event revision, an entity revision,
//! exact scope, observation time, an optional source watermark, and coverage.
//! The client reducer deduplicates by `(stream, event_revision)`, rejects stale
//! generations, and refetches the canonical read model on a revision gap — so
//! this endpoint deliberately emits **coarse invalidation** events, never full
//! read-model payloads. A periodic heartbeat (both a typed `heartbeat` event and
//! transport-level keep-alive comment frames) proves liveness.
//!
//! The event family union is closed and additive: a new family is added as a new
//! variant, and a client generated against an older schema renders an unknown
//! family as `unsupported_schema` rather than crashing.
//!
//! Seeded real sources (cheap, within dashboard territory):
//! - `project_registry_changed` — polled from the project registry snapshot
//!   digest (real end-to-end);
//! - `storage_telemetry_invalidated` — polled coarsely from the summed store
//!   size (a real change signal that tells the client to refetch
//!   `/api/storage/telemetry`).
//!
//! Declared-but-unfed families (documented seams; additive, tolerated
//! downstream):
//! - `code_index_generation_published` — needs the daemon
//!   `CodeIndexSchedulerRegistry` read port that `/api/code-index/freshness`
//!   also requires.

use std::collections::hash_map::DefaultHasher;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Serialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::db::engine::QueryExecutor;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardScopeV1, DashboardWatermarkV1, now_micros, scope_from_state,
};

/// Poll cadence for the source pollers and heartbeat. Kept modest so a settled
/// dashboard coalesces to well under the plan's render budget.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Transport-level keep-alive comment cadence.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Emit a typed heartbeat event every Nth poll tick.
const HEARTBEAT_EVERY_TICKS: u64 = 5;
/// Bound the channel so a slow client cannot grow the queue without limit.
const CHANNEL_CAPACITY: usize = 256;

/// Stream identity labels. Each stream carries its own monotone revision.
const STREAM_HEARTBEAT: &str = "heartbeat";
const STREAM_PROJECT_REGISTRY: &str = "project_registry";
const STREAM_STORAGE_TELEMETRY: &str = "storage_telemetry";

/// The closed, additive event-family union.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "family")]
pub(crate) enum DashboardEventKindV1 {
    /// Liveness heartbeat; carries no invalidation.
    Heartbeat,
    /// The project registry snapshot changed; client refetches project lists.
    ProjectRegistryChanged { project_count: u64, digest: String },
    /// A coarse storage-telemetry change; client refetches `/api/storage/telemetry`.
    StorageTelemetryInvalidated { total_bytes: u64 },
    /// A new code-index generation was published. Declared but unfed until the
    /// scheduler-registry read port is wired.
    #[allow(dead_code)]
    CodeIndexGenerationPublished { generation_id: String },
}

impl DashboardEventKindV1 {
    fn stream(&self) -> &'static str {
        match self {
            Self::Heartbeat => STREAM_HEARTBEAT,
            Self::ProjectRegistryChanged { .. } => STREAM_PROJECT_REGISTRY,
            Self::StorageTelemetryInvalidated { .. } => STREAM_STORAGE_TELEMETRY,
            Self::CodeIndexGenerationPublished { .. } => "code_index",
        }
    }
}

/// One typed SSE event with its full monotone-revision envelope.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct DashboardEventV1 {
    pub stream: String,
    pub run_id: String,
    pub event_revision: u64,
    pub entity_revision: Option<u64>,
    pub scope: DashboardScopeV1,
    pub observation_time_micros: i64,
    pub source_watermark: Option<DashboardWatermarkV1>,
    pub coverage: DashboardCoverageV1,
    pub kind: DashboardEventKindV1,
}

/// Per-connection event-stream state: monotone per-stream revisions plus the
/// last-seen source snapshots used for change detection.
struct EventStreamState {
    run_id: String,
    heartbeat_revision: u64,
    registry_revision: u64,
    storage_revision: u64,
    last_registry_digest: Option<String>,
    last_store_total_bytes: Option<u64>,
}

impl EventStreamState {
    fn new(run_id: String) -> Self {
        Self {
            run_id,
            heartbeat_revision: 0,
            registry_revision: 0,
            storage_revision: 0,
            last_registry_digest: None,
            last_store_total_bytes: None,
        }
    }

    /// Build a heartbeat event with a monotone heartbeat-stream revision.
    fn heartbeat(&mut self, scope: &DashboardScopeV1) -> DashboardEventV1 {
        self.heartbeat_revision = self.heartbeat_revision.saturating_add(1);
        DashboardEventV1 {
            stream: STREAM_HEARTBEAT.to_string(),
            run_id: self.run_id.clone(),
            event_revision: self.heartbeat_revision,
            entity_revision: None,
            scope: scope.clone(),
            observation_time_micros: now_micros(),
            source_watermark: None,
            coverage: DashboardCoverageV1::unknown(),
            kind: DashboardEventKindV1::Heartbeat,
        }
    }

    /// Detect a project-registry change from a freshly computed digest. The first
    /// observation sets the baseline without emitting (the client already loads
    /// the current registry on connect); a subsequent different digest emits a
    /// monotone `project_registry_changed` event.
    fn detect_registry_change(
        &mut self,
        digest: String,
        project_count: u64,
        scope: &DashboardScopeV1,
    ) -> Option<DashboardEventV1> {
        let changed = self
            .last_registry_digest
            .as_ref()
            .is_some_and(|previous| previous != &digest);
        let first = self.last_registry_digest.is_none();
        self.last_registry_digest = Some(digest.clone());
        if !changed || first {
            return None;
        }
        self.registry_revision = self.registry_revision.saturating_add(1);
        Some(DashboardEventV1 {
            stream: STREAM_PROJECT_REGISTRY.to_string(),
            run_id: self.run_id.clone(),
            event_revision: self.registry_revision,
            entity_revision: Some(self.registry_revision),
            scope: scope.clone(),
            observation_time_micros: now_micros(),
            source_watermark: Some(DashboardWatermarkV1 {
                source: STREAM_PROJECT_REGISTRY.to_string(),
                watermark: digest,
            }),
            coverage: DashboardCoverageV1::complete(project_count, "projects"),
            kind: DashboardEventKindV1::ProjectRegistryChanged {
                project_count,
                digest: self.last_registry_digest.clone().unwrap_or_default(),
            },
        })
    }

    /// Detect a coarse storage-telemetry change from the summed store size.
    fn detect_storage_change(
        &mut self,
        total_bytes: u64,
        scope: &DashboardScopeV1,
    ) -> Option<DashboardEventV1> {
        let changed = self
            .last_store_total_bytes
            .is_some_and(|previous| previous != total_bytes);
        let first = self.last_store_total_bytes.is_none();
        self.last_store_total_bytes = Some(total_bytes);
        if !changed || first {
            return None;
        }
        self.storage_revision = self.storage_revision.saturating_add(1);
        Some(DashboardEventV1 {
            stream: STREAM_STORAGE_TELEMETRY.to_string(),
            run_id: self.run_id.clone(),
            event_revision: self.storage_revision,
            entity_revision: Some(self.storage_revision),
            scope: scope.clone(),
            observation_time_micros: now_micros(),
            source_watermark: Some(DashboardWatermarkV1 {
                source: STREAM_STORAGE_TELEMETRY.to_string(),
                watermark: total_bytes.to_string(),
            }),
            coverage: DashboardCoverageV1::unknown(),
            kind: DashboardEventKindV1::StorageTelemetryInvalidated { total_bytes },
        })
    }

    /// Poll all real sources against `state`, appending any change events.
    async fn poll_sources(
        &mut self,
        state: &DashboardState,
        scope: &DashboardScopeV1,
    ) -> Vec<DashboardEventV1> {
        let mut events = Vec::new();
        if let Some((digest, count)) = registry_snapshot(state).await
            && let Some(event) = self.detect_registry_change(digest, count, scope)
        {
            events.push(event);
        }
        if let Some(total) = summed_store_bytes(state).await
            && let Some(event) = self.detect_storage_change(total, scope)
        {
            events.push(event);
        }
        events
    }
}

/// `GET /api/events`
pub(crate) async fn events(State(state): State<DashboardState>) -> impl IntoResponse {
    let scope = scope_from_state(&state);
    let run_id = format!("run-{}-{}", std::process::id(), now_micros());
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(CHANNEL_CAPACITY);

    tokio::spawn(async move {
        let mut stream_state = EventStreamState::new(run_id);
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick: u64 = 0;

        // Prime the source baselines immediately so the first real change emits.
        let _ = stream_state.poll_sources(&state, &scope).await;

        loop {
            interval.tick().await;
            tick = tick.saturating_add(1);

            let mut batch: Vec<DashboardEventV1> = Vec::new();
            if tick.is_multiple_of(HEARTBEAT_EVERY_TICKS) {
                batch.push(stream_state.heartbeat(&scope));
            }
            batch.extend(stream_state.poll_sources(&state, &scope).await);

            for event in batch {
                if tx.send(encode_event(&event)).await.is_err() {
                    return; // client disconnected
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(KEEP_ALIVE_INTERVAL)
            .text("keep-alive"),
    )
}

/// Serialize one typed event into an SSE frame, named by its stream so the
/// client can route by `event:` without parsing the payload first.
fn encode_event(event: &DashboardEventV1) -> Result<Event, Infallible> {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    Ok(Event::default()
        .event(event.kind.stream())
        .id(event.event_revision.to_string())
        .data(data))
}

/// Compute a stable digest of the project-registry snapshot plus its count.
async fn registry_snapshot(state: &DashboardState) -> Option<(String, u64)> {
    let db = state.savings_db.as_ref()?;
    let projects = db.list_code_projects(250).await.ok()?;
    let count = projects.len() as u64;
    let mut hasher = DefaultHasher::new();
    count.hash(&mut hasher);
    for project in &projects {
        // Hash a stable identity for each project row. `Debug` is deterministic
        // for the record and avoids depending on a specific public accessor.
        format!("{project:?}").hash(&mut hasher);
    }
    Some((format!("{:016x}", hasher.finish()), count))
}

/// Sum the observed size of the always-held stores (graph + memory) as a coarse
/// storage-change signal. Returns `None` only when neither pragma read succeeds.
async fn summed_store_bytes(state: &DashboardState) -> Option<u64> {
    let mut total: u64 = 0;
    let mut any = false;
    if let Some(bytes) = store_total_bytes(&state.graph_conn).await {
        total = total.saturating_add(bytes);
        any = true;
    }
    if let Some(bytes) = store_total_bytes(&state.mem_db.engine_conn()).await {
        total = total.saturating_add(bytes);
        any = true;
    }
    any.then_some(total)
}

async fn store_total_bytes(conn: &(impl QueryExecutor + ?Sized)) -> Option<u64> {
    let page_size = pragma_u64(conn, "page_size").await?;
    let page_count = pragma_u64(conn, "page_count").await?;
    Some(page_size.saturating_mul(page_count))
}

async fn pragma_u64(conn: &(impl QueryExecutor + ?Sized), pragma: &str) -> Option<u64> {
    let sql = format!("PRAGMA {pragma}");
    let mut rows = conn.query(&sql, ()).await.ok()?;
    let row = rows.next().await.ok()??;
    let value = row.get::<i64>(0).ok()?;
    Some(value.max(0) as u64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tracedecay::TraceDecay;

    fn scope() -> DashboardScopeV1 {
        DashboardScopeV1 {
            project_id: Some("proj".into()),
            storage_mode: "profile_sharded".into(),
            store_root: "/store".into(),
        }
    }

    #[test]
    fn heartbeat_revisions_are_monotone_per_stream() {
        let mut state = EventStreamState::new("run-test".to_string());
        let scope = scope();
        let first = state.heartbeat(&scope);
        let second = state.heartbeat(&scope);
        let third = state.heartbeat(&scope);
        assert_eq!(first.event_revision, 1);
        assert_eq!(second.event_revision, 2);
        assert_eq!(third.event_revision, 3);
        assert_eq!(first.stream, STREAM_HEARTBEAT);
        assert_eq!(first.kind, DashboardEventKindV1::Heartbeat);
        assert_eq!(first.run_id, "run-test");
    }

    #[test]
    fn registry_change_emits_only_after_baseline_and_is_monotone() {
        let mut state = EventStreamState::new("run-test".to_string());
        let scope = scope();

        // First observation is the baseline: no event.
        assert!(
            state
                .detect_registry_change("digest-a".into(), 3, &scope)
                .is_none()
        );
        // Same digest: still no event.
        assert!(
            state
                .detect_registry_change("digest-a".into(), 3, &scope)
                .is_none()
        );

        // Seeded change: emits a monotone event carrying the new digest.
        let event = state
            .detect_registry_change("digest-b".into(), 4, &scope)
            .expect("registry change event");
        assert_eq!(event.stream, STREAM_PROJECT_REGISTRY);
        assert_eq!(event.event_revision, 1);
        assert_eq!(
            event.kind,
            DashboardEventKindV1::ProjectRegistryChanged {
                project_count: 4,
                digest: "digest-b".into(),
            }
        );
        assert_eq!(
            event.source_watermark.as_ref().unwrap().watermark,
            "digest-b"
        );

        // A second change increments the registry-stream revision.
        let next = state
            .detect_registry_change("digest-c".into(), 4, &scope)
            .expect("second registry change");
        assert_eq!(next.event_revision, 2);
    }

    #[test]
    fn storage_change_emits_only_after_baseline() {
        let mut state = EventStreamState::new("run-test".to_string());
        let scope = scope();
        assert!(state.detect_storage_change(1000, &scope).is_none());
        assert!(state.detect_storage_change(1000, &scope).is_none());
        let event = state
            .detect_storage_change(2048, &scope)
            .expect("storage change event");
        assert_eq!(event.stream, STREAM_STORAGE_TELEMETRY);
        assert_eq!(
            event.kind,
            DashboardEventKindV1::StorageTelemetryInvalidated { total_bytes: 2048 }
        );
    }

    #[test]
    fn event_kinds_serialize_additively_with_family_tag() {
        let value = serde_json::to_value(DashboardEventKindV1::CodeIndexGenerationPublished {
            generation_id: "gen-1".into(),
        })
        .unwrap();
        assert_eq!(value["family"], "code_index_generation_published");
        let heartbeat = serde_json::to_value(DashboardEventKindV1::Heartbeat).unwrap();
        assert_eq!(heartbeat["family"], "heartbeat");
    }

    #[tokio::test]
    async fn poll_sources_reads_real_state_and_primes_baseline() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path())
            .await
            .expect("project init");
        let dash = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        let scope = scope_from_state(&dash);
        let mut state = EventStreamState::new("run-test".to_string());

        // First poll primes the baselines and emits nothing.
        let primed = state.poll_sources(&dash, &scope).await;
        assert!(primed.is_empty(), "baseline poll must not emit events");
        // The storage baseline is a real summed size read.
        assert!(state.last_store_total_bytes.unwrap_or(0) > 0);
        assert!(state.last_registry_digest.is_some());
    }
}
