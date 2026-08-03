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
//! Two kinds of source feed this endpoint.
//!
//! **Polled digests** (cheap, within dashboard territory):
//! - `project_registry_changed` — polled from the project registry snapshot
//!   digest (real end-to-end);
//! - `storage_telemetry_invalidated` — polled coarsely from the summed store
//!   size (a real change signal that tells the client to refetch
//!   `/api/storage/telemetry`).
//!
//! **Durable activity records** (via [`crate::application::event_lane`]): the daemon
//! observes real agent work continuously — host hooks admitted on the MCP
//! boundary, transcript messages persisted, touched paths queued for indexing,
//! tool calls dispatched. Each producer durably publishes its own project
//! scope before waking live consumers, and this endpoint turns those records into
//! `hook_activity`, `session_ingest_activity`, `code_index_activity`, and
//! `tool_call_activity` events.
//!
//! Activity pulses are **coalesced, never forwarded one-for-one**. A machine
//! running many agents produces hook and index pulses far faster than any
//! visualization can render, and the client's queue is bounded. So pulses
//! accumulate into one bucket per `(family, project)` and flush on a fixed
//! [`ACTIVITY_FLUSH_INTERVAL`] tick — at most **two events per second per family
//! per project**, each carrying the coalesced `count`/`units` in its payload.
//! Slow or lagged consumers replay from the persisted producer frontier.
//! Retention eviction and rejected oversized records advance explicit drop and
//! coverage accounting; an expired resume emits `resume_gap`.
//!
//! All activity families share one canonical `dashboard_activity` stream and
//! producer sequence. SSE `Last-Event-ID` binds the durable run and sequence,
//! while the named SSE event and typed family preserve routing semantics.
//!
//! Declared-but-unfed families (documented seams; additive, tolerated
//! downstream):
//! - `code_index_generation_published` — needs the daemon
//!   `CodeIndexSchedulerRegistry` read port that `/api/code-index/freshness`
//!   also requires. Distinct from `code_index_activity`, which reports *queued
//!   work*, not a *published generation*.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Serialize;
use tokio_stream::wrappers::ReceiverStream;

use tracedecay_runtime_core::db::engine::QueryExecutor;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardScopeV1, DashboardWatermarkV1, now_micros, scope_from_state,
};
use crate::application::event_lane::{
    ActivityFamilyV1, ActivityFrontierV1, ActivityPulseV1, ActivityRecordV1,
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
/// Flush cadence for coalesced activity buckets. This is the rate limit: one
/// event per `(family, project)` per tick, i.e. at most 2/s per bucket.
const ACTIVITY_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Stream identity labels. Each stream carries its own monotone revision.
const STREAM_HEARTBEAT: &str = "heartbeat";
const STREAM_PROJECT_REGISTRY: &str = "project_registry";
const STREAM_STORAGE_TELEMETRY: &str = "storage_telemetry";
const STREAM_DASHBOARD_ACTIVITY: &str = "dashboard_activity";

/// The closed, additive event-family union.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "family")]
pub enum DashboardEventKindV1 {
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
    /// Host lifecycle hooks were admitted for this project in the last window.
    /// `count` is coalesced pulses, `hook_events` the underlying hook events.
    HookActivity {
        count: u64,
        hook_events: u64,
        detail: Option<String>,
    },
    /// Transcript messages were persisted into the session store for this
    /// project. `messages` is the real upserted-message count.
    SessionIngestActivity {
        count: u64,
        messages: u64,
        detail: Option<String>,
    },
    /// Touched paths entered this project's incremental code-index queue.
    CodeIndexActivity {
        count: u64,
        files: u64,
        detail: Option<String>,
    },
    /// Tool calls were dispatched by the daemon against this project.
    ToolCallActivity {
        count: u64,
        calls: u64,
        detail: Option<String>,
    },
    /// Work task mutations were committed for this project. Clients refetch
    /// canonical generation-bound Work projections.
    TaskActivity {
        count: u64,
        tasks: u64,
        detail: Option<String>,
    },
    /// Requested durable cursor predates the retained frontier. The client
    /// invalidates canonical reads once, then continues from `first_available`.
    ResumeGap {
        requested_after: u64,
        first_available: u64,
        dropped_events: u64,
    },
}

impl DashboardEventKindV1 {
    /// The SSE `event:` name that carries this kind. It is deliberately coarser
    /// than the envelope's `stream` field: activity streams are per project, but
    /// the client subscribes to a small closed set of *named* events, so the
    /// name stays at family granularity.
    fn stream(&self) -> &'static str {
        match self {
            Self::Heartbeat => STREAM_HEARTBEAT,
            Self::ProjectRegistryChanged { .. } => STREAM_PROJECT_REGISTRY,
            Self::StorageTelemetryInvalidated { .. } => STREAM_STORAGE_TELEMETRY,
            Self::CodeIndexGenerationPublished { .. } => "code_index",
            Self::HookActivity { .. } => ActivityFamilyV1::Hook.stream_name(),
            Self::SessionIngestActivity { .. } => ActivityFamilyV1::SessionIngest.stream_name(),
            Self::CodeIndexActivity { .. } => ActivityFamilyV1::CodeIndex.stream_name(),
            Self::ToolCallActivity { .. } => ActivityFamilyV1::ToolCall.stream_name(),
            Self::TaskActivity { .. } => ActivityFamilyV1::Task.stream_name(),
            Self::ResumeGap { .. } => "control",
        }
    }

    /// Build the activity kind for `family` from a coalesced bucket.
    fn activity(family: ActivityFamilyV1, count: u64, units: u64, detail: Option<String>) -> Self {
        match family {
            ActivityFamilyV1::Hook => Self::HookActivity {
                count,
                hook_events: units,
                detail,
            },
            ActivityFamilyV1::SessionIngest => Self::SessionIngestActivity {
                count,
                messages: units,
                detail,
            },
            ActivityFamilyV1::CodeIndex => Self::CodeIndexActivity {
                count,
                files: units,
                detail,
            },
            ActivityFamilyV1::ToolCall => Self::ToolCallActivity {
                count,
                calls: units,
                detail,
            },
            ActivityFamilyV1::Task => Self::TaskActivity {
                count,
                tasks: units,
                detail,
            },
        }
    }
}

/// One coalescing bucket: all pulses of one family for one project observed
/// within a single flush window.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ActivityBucketKeyV1 {
    family: ActivityFamilyV1,
    project_root: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ActivityBucketV1 {
    /// Pulses folded into this bucket.
    count: u64,
    /// Underlying units summed across those pulses.
    units: u64,
    /// Registered project id, taken from the first pulse that carried one. A
    /// producer that knows its own id is always more authoritative than the
    /// registry lookup the flush falls back to.
    project_id: Option<String>,
    /// Most recent producer label in the window.
    detail: Option<String>,
    /// Exact persisted control record at the end of this coalesced bucket.
    last_record: Option<ActivityRecordV1>,
}

impl ActivityBucketV1 {
    fn absorb(&mut self, pulse: ActivityPulseV1) {
        self.count = self.count.saturating_add(1);
        self.units = self.units.saturating_add(pulse.units);
        if self.project_id.is_none() {
            self.project_id = pulse.project_id;
        }
        if pulse.detail.is_some() {
            self.detail = pulse.detail;
        }
    }

    fn absorb_record(&mut self, record: ActivityRecordV1) {
        self.absorb(record.pulse.clone());
        if self
            .last_record
            .as_ref()
            .is_none_or(|current| current.producer_sequence < record.producer_sequence)
        {
            self.last_record = Some(record);
        }
    }
}

/// One typed SSE event with its full monotone-revision envelope.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DashboardEventV1 {
    pub stream: String,
    pub run_id: String,
    pub event_revision: u64,
    /// Global producer sequence used by SSE `Last-Event-ID`.
    pub producer_sequence: Option<u64>,
    /// First producer sequence still replayable from the durable lane.
    pub retained_from_sequence: Option<u64>,
    /// Total events evicted or rejected before this event was published.
    pub dropped_events: u64,
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
    activity_dropped_events: u64,
    last_registry_digest: Option<String>,
    last_store_total_bytes: Option<u64>,
    /// Canonical project root → registered project id, refreshed for free from
    /// the registry poll this task already runs. Lets a producer that does not
    /// hold its project id (transcript ingest) still land on the right neuron.
    registry_roots: HashMap<PathBuf, String>,
}

impl EventStreamState {
    fn new(run_id: String) -> Self {
        Self {
            run_id,
            heartbeat_revision: 0,
            registry_revision: 0,
            storage_revision: 0,
            activity_dropped_events: 0,
            last_registry_digest: None,
            last_store_total_bytes: None,
            registry_roots: HashMap::new(),
        }
    }

    /// Build a heartbeat event with a monotone heartbeat-stream revision.
    fn heartbeat(&mut self, scope: &DashboardScopeV1) -> DashboardEventV1 {
        self.heartbeat_revision = self.heartbeat_revision.saturating_add(1);
        DashboardEventV1 {
            stream: STREAM_HEARTBEAT.to_string(),
            run_id: self.run_id.clone(),
            event_revision: self.heartbeat_revision,
            producer_sequence: None,
            retained_from_sequence: None,
            dropped_events: 0,
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
            producer_sequence: None,
            retained_from_sequence: None,
            dropped_events: 0,
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
            producer_sequence: None,
            retained_from_sequence: None,
            dropped_events: 0,
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

    /// Resolve the registered project id for an observed project root. Prefers
    /// the id the producer already supplied; falls back to the registry map,
    /// canonicalizing once (only here, at flush time — never on the producer's
    /// hot path).
    fn resolve_project_id(&self, root: &Path, supplied: Option<String>) -> Option<String> {
        if supplied.is_some() {
            return supplied;
        }
        if let Some(id) = self.registry_roots.get(root) {
            return Some(id.clone());
        }
        let canonical = root.canonicalize().ok()?;
        self.registry_roots.get(&canonical).cloned()
    }

    /// Turn one coalesced bucket into an envelope-disciplined event. `base` is
    /// the serving dashboard's scope: the observed project's id replaces
    /// `project_id`, while the storage identity stays the store this daemon
    /// actually observed the work in — which is exactly where the observation
    /// was recorded.
    fn activity_event(
        &mut self,
        key: &ActivityBucketKeyV1,
        bucket: ActivityBucketV1,
        base: &DashboardScopeV1,
    ) -> Option<DashboardEventV1> {
        let record = bucket.last_record.as_ref()?;
        let project_id = self.resolve_project_id(&key.project_root, bucket.project_id);
        let dropped_events = record.dropped_events;
        let newly_dropped = dropped_events.saturating_sub(self.activity_dropped_events);
        self.activity_dropped_events = self.activity_dropped_events.max(dropped_events);
        let coverage = if newly_dropped == 0 {
            DashboardCoverageV1::complete(bucket.count, "activity_events")
        } else {
            DashboardCoverageV1::partial(
                bucket.count.saturating_add(newly_dropped),
                bucket.count,
                "activity_events",
                vec!["retention_eviction".to_string()],
            )
        };
        Some(DashboardEventV1 {
            stream: STREAM_DASHBOARD_ACTIVITY.to_string(),
            run_id: record.run_id.clone(),
            event_revision: record.producer_sequence,
            producer_sequence: Some(record.producer_sequence),
            retained_from_sequence: Some(record.retained_from_sequence),
            dropped_events,
            entity_revision: Some(record.producer_sequence),
            scope: DashboardScopeV1 {
                project_id,
                storage_mode: base.storage_mode.clone(),
                store_root: base.store_root.clone(),
            },
            observation_time_micros: record.observation_time_micros,
            source_watermark: Some(DashboardWatermarkV1 {
                source: "dashboard_activity_lane".to_string(),
                watermark: record.producer_sequence.to_string(),
            }),
            coverage,
            kind: DashboardEventKindV1::activity(
                key.family,
                bucket.count,
                bucket.units,
                bucket.detail,
            ),
        })
    }

    /// Drain every open bucket into events, emptying `pending`.
    fn flush_activity(
        &mut self,
        pending: &mut std::collections::BTreeMap<ActivityBucketKeyV1, ActivityBucketV1>,
        base: &DashboardScopeV1,
    ) -> Vec<DashboardEventV1> {
        let mut buckets: Vec<_> = std::mem::take(pending).into_iter().collect();
        buckets.sort_by_key(|(_, bucket)| {
            bucket
                .last_record
                .as_ref()
                .map_or(0, |record| record.producer_sequence)
        });
        buckets
            .into_iter()
            .filter_map(|(key, bucket)| self.activity_event(&key, bucket, base))
            .collect()
    }

    /// Poll all real sources against `state`, appending any change events.
    async fn poll_sources(
        &mut self,
        state: &DashboardState,
        scope: &DashboardScopeV1,
    ) -> Vec<DashboardEventV1> {
        let mut events = Vec::new();
        if let Some(snapshot) = registry_snapshot(state).await {
            self.registry_roots = snapshot.roots;
            if let Some(event) = self.detect_registry_change(snapshot.digest, snapshot.count, scope)
            {
                events.push(event);
            }
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
pub async fn events(State(state): State<DashboardState>, headers: HeaderMap) -> impl IntoResponse {
    let scope = scope_from_state(&state);
    let run_id = format!("run-{}-{}", std::process::id(), now_micros());
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(CHANNEL_CAPACITY);

    // Subscribe before reading history. Any event appended between those two
    // steps is either in the replay or the live receiver and is deduplicated by
    // producer sequence.
    let mut activity = crate::application::event_lane::subscribe();
    let requested = parse_last_event_id(&headers);
    let activity_db = state.lcm_db.clone();
    let activity_project_id = state.project_id.clone();
    let initial_replay = match (activity_db.as_deref(), activity_project_id.as_deref()) {
        (Some(db), Some(project_id)) => {
            crate::application::event_lane::replay_after(
                db,
                project_id,
                requested.as_ref().map(|resume| resume.sequence),
            )
            .await
        }
        _ => None,
    };

    tokio::spawn(async move {
        let mut stream_state = EventStreamState::new(run_id);
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut flush = tokio::time::interval(ACTIVITY_FLUSH_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick: u64 = 0;
        let mut pending: std::collections::BTreeMap<ActivityBucketKeyV1, ActivityBucketV1> =
            std::collections::BTreeMap::new();
        let mut producer_cursor = requested.as_ref().map_or(0, |resume| resume.sequence);
        let mut control = Vec::new();
        if let Some(mut replay) = initial_replay {
            let run_mismatch = requested
                .as_ref()
                .is_some_and(|resume| resume.run_id != replay.frontier.run_id);
            let invalid_frontier = requested
                .as_ref()
                .is_some_and(|resume| resume.sequence >= replay.frontier.next_sequence);
            if (run_mismatch || invalid_frontier)
                && let (Some(db), Some(project_id)) =
                    (activity_db.as_deref(), activity_project_id.as_deref())
                && let Some(from_start) =
                    crate::application::event_lane::replay_after(db, project_id, None).await
            {
                replay = from_start;
            }
            if replay.resume_gap || run_mismatch || invalid_frontier {
                control.push(resume_gap_event(
                    requested.as_ref().map_or(0, |resume| resume.sequence),
                    &replay.frontier,
                    &scope,
                ));
                producer_cursor = replay.frontier.retained_from_sequence.saturating_sub(1);
            }
            for record in replay.records {
                producer_cursor = producer_cursor.max(record.producer_sequence);
                accumulate_record(&mut pending, record);
            }
        }

        // Prime the source baselines immediately so the first real change emits.
        let _ = stream_state.poll_sources(&state, &scope).await;
        for event in control
            .into_iter()
            .chain(stream_state.flush_activity(&mut pending, &scope))
        {
            let Ok(frame) = encode_event(&event) else {
                return;
            };
            if tx.send(Ok(frame)).await.is_err() {
                return;
            }
        }

        loop {
            let batch: Vec<DashboardEventV1> = tokio::select! {
                _ = interval.tick() => {
                    tick = tick.saturating_add(1);
                    let mut batch = Vec::new();
                    if tick.is_multiple_of(HEARTBEAT_EVERY_TICKS) {
                        batch.push(stream_state.heartbeat(&scope));
                    }
                    batch.extend(stream_state.poll_sources(&state, &scope).await);
                    batch
                }
                _ = flush.tick() => {
                    if pending.is_empty() {
                        continue;
                    }
                    stream_state.flush_activity(&mut pending, &scope)
                }
                // `broadcast::Receiver::recv` is cancel-safe, so losing this
                // branch to a poll/flush tick never drops a pulse.
                received = receive_activity(&mut activity) => {
                    match received {
                        Some(Ok(record)) => {
                            if record.producer_sequence > producer_cursor {
                                producer_cursor = record.producer_sequence;
                                accumulate_record(&mut pending, record);
                            }
                        }
                        Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                            if let (Some(db), Some(project_id)) =
                                (activity_db.as_deref(), activity_project_id.as_deref())
                                && let Some(replay) = crate::application::event_lane::replay_after(
                                    db,
                                    project_id,
                                    Some(producer_cursor),
                                ).await
                            {
                                if replay.resume_gap {
                                    let event = resume_gap_event(producer_cursor, &replay.frontier, &scope);
                                    let Ok(frame) = encode_event(&event) else {
                                        return;
                                    };
                                    if tx.send(Ok(frame)).await.is_err() {
                                        return;
                                    }
                                    producer_cursor = replay.frontier.retained_from_sequence.saturating_sub(1);
                                }
                                for record in replay.records {
                                    if record.producer_sequence > producer_cursor {
                                        producer_cursor = record.producer_sequence;
                                        accumulate_record(&mut pending, record);
                                    }
                                }
                            }
                        }
                        Some(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                            activity = None;
                        }
                        None => {}
                    }
                    continue;
                }
            };

            for event in batch {
                let Ok(frame) = encode_event(&event) else {
                    return;
                };
                if tx.send(Ok(frame)).await.is_err() {
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

#[derive(Clone)]
struct EventResumeV1 {
    run_id: String,
    sequence: u64,
}

fn parse_last_event_id(headers: &HeaderMap) -> Option<EventResumeV1> {
    let value = headers.get("last-event-id")?.to_str().ok()?;
    if let Ok(sequence) = value.parse() {
        return Some(EventResumeV1 {
            run_id: String::new(),
            sequence,
        });
    }
    let (run_id, sequence) = value.rsplit_once(':')?;
    Some(EventResumeV1 {
        run_id: run_id.to_string(),
        sequence: sequence.parse().ok()?,
    })
}

async fn receive_activity(
    receiver: &mut Option<tokio::sync::broadcast::Receiver<ActivityRecordV1>>,
) -> Option<Result<ActivityRecordV1, tokio::sync::broadcast::error::RecvError>> {
    match receiver {
        Some(receiver) => Some(receiver.recv().await),
        None => std::future::pending().await,
    }
}

fn resume_gap_event(
    requested_after: u64,
    frontier: &ActivityFrontierV1,
    scope: &DashboardScopeV1,
) -> DashboardEventV1 {
    let missing = frontier
        .retained_from_sequence
        .saturating_sub(requested_after.saturating_add(1));
    DashboardEventV1 {
        stream: "control".to_string(),
        run_id: frontier.run_id.clone(),
        event_revision: frontier.retained_from_sequence,
        producer_sequence: None,
        retained_from_sequence: Some(frontier.retained_from_sequence),
        dropped_events: frontier.dropped_events,
        entity_revision: None,
        scope: scope.clone(),
        observation_time_micros: now_micros(),
        source_watermark: Some(DashboardWatermarkV1 {
            source: "dashboard_activity_lane".to_string(),
            watermark: frontier.watermark.clone(),
        }),
        coverage: if missing == 0 {
            DashboardCoverageV1::unknown()
        } else {
            DashboardCoverageV1::partial(
                missing,
                0,
                "activity_events",
                vec!["resume_gap".to_string()],
            )
        },
        kind: DashboardEventKindV1::ResumeGap {
            requested_after,
            first_available: frontier.retained_from_sequence,
            dropped_events: frontier.dropped_events,
        },
    }
}

fn accumulate_record(
    pending: &mut std::collections::BTreeMap<ActivityBucketKeyV1, ActivityBucketV1>,
    record: ActivityRecordV1,
) {
    let key = ActivityBucketKeyV1 {
        family: record.pulse.family,
        project_root: record.pulse.project_root.clone(),
    };
    if let Some(bucket) = pending.get_mut(&key) {
        bucket.absorb_record(record);
        return;
    }
    pending.entry(key).or_default().absorb_record(record);
}

/// Serialize one typed event into an SSE frame, named by its stream so the
/// client can route by `event:` without parsing the payload first.
fn encode_event(event: &DashboardEventV1) -> Result<Event, serde_json::Error> {
    let data = serde_json::to_string(event)?;
    let frame = Event::default().event(event.kind.stream()).data(data);
    let resume_sequence = match &event.kind {
        DashboardEventKindV1::ResumeGap {
            first_available, ..
        } => Some(first_available.saturating_sub(1)),
        _ => event.producer_sequence,
    };
    Ok(match resume_sequence {
        Some(sequence) => frame.id(format!("{}:{sequence}", event.run_id)),
        None => frame,
    })
}

/// One observation of the project registry: its change digest, its size, and
/// the canonical-root → project-id map the activity flush resolves against.
struct RegistrySnapshot {
    digest: String,
    count: u64,
    roots: HashMap<PathBuf, String>,
}

/// Compute a stable digest of the project-registry snapshot plus its count.
async fn registry_snapshot(state: &DashboardState) -> Option<RegistrySnapshot> {
    let db = state.savings_db.as_ref()?;
    let projects = db.list_code_projects(250).await.ok()?;
    let count = projects.len() as u64;
    let mut hasher = DefaultHasher::new();
    count.hash(&mut hasher);
    let mut roots = HashMap::with_capacity(projects.len());
    for project in &projects {
        // Hash a stable identity for each project row. `Debug` is deterministic
        // for the record and avoids depending on a specific public accessor.
        format!("{project:?}").hash(&mut hasher);
        roots.insert(
            PathBuf::from(&project.canonical_root),
            project.project_id.clone(),
        );
    }
    Some(RegistrySnapshot {
        digest: format!("{:016x}", hasher.finish()),
        count,
        roots,
    })
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
pub(crate) async fn dashboard_state_fixture(
    project_id: &str,
) -> (tempfile::TempDir, DashboardState) {
    use std::sync::Arc;

    use tokio::sync::RwLock;
    use tracedecay_domain::{FactOwnerV1, ProjectId};
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_usecases::configuration::ProductionUserSettingsDaemonClient;

    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n").expect("fixture source");
    let database_path = project.path().join("dashboard.db");
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(&database_path, "dashboard API state fixture")
        .expect("fixture database authority");
    let (database, _) = Database::publish_test_runtime(
        &database_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("fixture database");
    let database = Arc::new(database);
    let project_identity = ProjectId::new(project_id).expect("fixture project id");
    let project_root = project.path().to_path_buf();
    let store_root = project_root.join("store");
    let dashboard_root = store_root.join("dashboard");
    std::fs::create_dir_all(&dashboard_root).expect("fixture dashboard root");

    let state = DashboardState {
        project_id: Some(project_identity.as_str().to_owned()),
        resolved_scope: crate::scope::resolve_dashboard_scope(
            &project_root,
            Some(project_identity.as_str()),
        ),
        project_graph: None,
        project_graph_resolver: None,
        memory_owner: FactOwnerV1::Project {
            project_id: project_identity,
        },
        graph_conn: database.engine_conn(),
        _database_guards: vec![Arc::clone(&database)],
        graph_telemetry_handle: database.storage_telemetry_handle().ok(),
        graph_db_path: database_path.display().to_string(),
        mem_db: Arc::clone(&database),
        mem_db_path: database_path.display().to_string(),
        lcm_db: None,
        lcm_db_path: String::new(),
        lcm_scope: "unavailable".to_owned(),
        savings_db: None,
        savings_db_path: String::new(),
        project_root,
        code_index_freshness_reader: None,
        feedback_status_reader: None,
        storage_mode: "profile_sharded".to_owned(),
        store_root,
        config_path: project.path().join("config.json"),
        dashboard_root,
        retention_config: crate::config::RetentionConfig::default(),
        user_settings: Arc::new(ProductionUserSettingsDaemonClient),
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(crate::token_count::TokenCountCache::new()),
        code_diagnostics_authority: None,
        automation_scheduler_reconciler: None,
        automation_writer: crate::standalone_dashboard_automation_writer(),
        doctor_report_reader: None,
        doctor_remediation_dispatcher: None,
        application_invocation_executor: None,
    };
    (project, state)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    async fn registered_database_for_test(
        path: &Path,
    ) -> std::sync::Arc<tracedecay_global_db::RegisteredGlobalDb> {
        use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(path, "dashboard registry fixture")
            .expect("registry authority");
        let (database, _) =
            Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("registry database");
        // `Database::conn()` is the retained reader; schema DDL has to run on
        // the serialized writer lane or the exact SQL channel reports
        // `WriterUnavailable`.
        {
            let writer = database
                .writer_connection("initialize dashboard registry fixture schema")
                .await
                .expect("registry writer lane");
            tracedecay_global_db::ensure_registered_schema(writer.engine_connection())
                .await
                .expect("registered schema");
        }
        let runtime = database.retained_runtime().clone();
        let binding = runtime.binding().clone();
        let locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority("attach dashboard registry fixture")
            .expect("registered runtime authority");
        std::sync::Arc::new(
            tracedecay_global_db::RegisteredGlobalDb::migrate_and_attach(
                runtime, binding, locator, authority,
            )
            .await
            .expect("registered dashboard fixture"),
        )
    }

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
    fn activity_coverage_counts_only_new_drops_for_each_bucket() {
        let mut state = EventStreamState::new("run-test".to_string());
        let scope = scope();
        let key = ActivityBucketKeyV1 {
            family: ActivityFamilyV1::Hook,
            project_root: PathBuf::from("/repo/alpha"),
        };
        let pulse = ActivityPulseV1 {
            family: ActivityFamilyV1::Hook,
            project_root: key.project_root.clone(),
            project_id: Some("proj-alpha".into()),
            units: 1,
            detail: None,
        };
        let bucket = |sequence, pulse| ActivityBucketV1 {
            count: 1,
            units: 1,
            project_id: Some("proj-alpha".into()),
            detail: None,
            last_record: Some(ActivityRecordV1 {
                schema_version: 1,
                run_id: "lane-1".into(),
                producer_sequence: sequence,
                observation_time_micros: sequence as i64,
                retained_from_sequence: 3,
                dropped_events: 2,
                pulse,
            }),
        };

        let first = state
            .activity_event(&key, bucket(3, pulse.clone()), &scope)
            .expect("first activity");
        assert_eq!(first.coverage.eligible, Some(3));
        assert_eq!(first.coverage.examined, Some(1));

        let second = state
            .activity_event(&key, bucket(4, pulse), &scope)
            .expect("second activity");
        assert!(second.coverage.is_complete());
        assert_eq!(second.coverage.eligible, Some(1));
        assert_eq!(second.coverage.examined, Some(1));
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

    fn pulse(family: ActivityFamilyV1, root: &str, units: u64) -> ActivityPulseV1 {
        ActivityPulseV1 {
            family,
            project_root: PathBuf::from(root),
            project_id: None,
            units,
            detail: None,
        }
    }

    fn record(sequence: u64, _stream_revision: u64, pulse: ActivityPulseV1) -> ActivityRecordV1 {
        ActivityRecordV1 {
            schema_version: 1,
            run_id: "run-durable".into(),
            producer_sequence: sequence,
            observation_time_micros: 42,
            retained_from_sequence: 1,
            dropped_events: 0,
            pulse,
        }
    }

    #[test]
    fn a_burst_coalesces_into_one_bucket_per_family_and_project() {
        let mut pending = std::collections::BTreeMap::new();
        for sequence in 1..=50 {
            accumulate_record(
                &mut pending,
                record(sequence, 0, pulse(ActivityFamilyV1::Hook, "/repo/a", 2)),
            );
        }
        accumulate_record(
            &mut pending,
            record(51, 0, pulse(ActivityFamilyV1::Hook, "/repo/b", 1)),
        );
        accumulate_record(
            &mut pending,
            record(52, 0, pulse(ActivityFamilyV1::ToolCall, "/repo/a", 1)),
        );

        assert_eq!(pending.len(), 3, "one bucket per (family, project)");
        let hot = pending
            .get(&ActivityBucketKeyV1 {
                family: ActivityFamilyV1::Hook,
                project_root: PathBuf::from("/repo/a"),
            })
            .expect("hot bucket");
        assert_eq!(hot.count, 50, "every pulse is counted");
        assert_eq!(hot.units, 100, "underlying units sum");
    }

    #[test]
    fn canonical_activity_stream_is_monotone_and_preserves_project_scope() {
        let mut state = EventStreamState::new("run-test".to_string());
        let base = scope();
        let mut pending = std::collections::BTreeMap::new();

        accumulate_record(
            &mut pending,
            record(1, 1, {
                let mut p = pulse(ActivityFamilyV1::Hook, "/repo/a", 3);
                p.project_id = Some("proj-a".into());
                p.detail = Some("file_edit".into());
                p
            }),
        );
        accumulate_record(
            &mut pending,
            record(2, 1, {
                let mut p = pulse(ActivityFamilyV1::Hook, "/repo/b", 1);
                p.project_id = Some("proj-b".into());
                p
            }),
        );

        let first = state.flush_activity(&mut pending, &base);
        assert!(pending.is_empty(), "flushing drains every bucket");
        assert_eq!(first.len(), 2);
        assert_eq!(
            first
                .iter()
                .map(|event| event.event_revision)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        for event in &first {
            assert_eq!(event.kind.stream(), "hook_activity");
        }
        let a = first
            .iter()
            .find(|event| event.scope.project_id.as_deref() == Some("proj-a"))
            .expect("project a stream");
        assert_eq!(a.stream, STREAM_DASHBOARD_ACTIVITY);
        assert_eq!(a.scope.project_id.as_deref(), Some("proj-a"));
        assert_eq!(
            a.kind,
            DashboardEventKindV1::HookActivity {
                count: 1,
                hook_events: 3,
                detail: Some("file_edit".into()),
            }
        );

        // A second window continues the shared durable producer frontier.
        accumulate_record(
            &mut pending,
            record(3, 2, {
                let mut p = pulse(ActivityFamilyV1::Hook, "/repo/a", 1);
                p.project_id = Some("proj-a".into());
                p
            }),
        );
        let second = state.flush_activity(&mut pending, &base);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].stream, STREAM_DASHBOARD_ACTIVITY);
        assert_eq!(second[0].event_revision, 3);
    }

    #[test]
    fn a_pulse_without_a_project_id_resolves_through_the_registry_map() {
        let mut state = EventStreamState::new("run-test".to_string());
        state
            .registry_roots
            .insert(PathBuf::from("/repo/ingested"), "proj-ingested".to_string());
        let mut pending = std::collections::BTreeMap::new();
        accumulate_record(
            &mut pending,
            record(1, 1, {
                let mut p = pulse(ActivityFamilyV1::SessionIngest, "/repo/ingested", 7);
                p.detail = Some("claude".into());
                p
            }),
        );

        let events = state.flush_activity(&mut pending, &scope());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].stream, STREAM_DASHBOARD_ACTIVITY);
        assert_eq!(
            events[0].scope.project_id.as_deref(),
            Some("proj-ingested"),
            "the event carries the OBSERVED project, not the serving dashboard's"
        );
        assert_eq!(
            events[0].kind,
            DashboardEventKindV1::SessionIngestActivity {
                count: 1,
                messages: 7,
                detail: Some("claude".into()),
            }
        );
    }

    #[test]
    fn activity_families_serialize_with_their_own_family_tags() {
        for family in ActivityFamilyV1::ALL {
            let kind = DashboardEventKindV1::activity(family, 1, 1, None);
            let value = serde_json::to_value(&kind).unwrap();
            let tag = value["family"].as_str().expect("family tag").to_string();
            assert!(
                tag.ends_with("_activity"),
                "activity families are tagged as activity: {tag}"
            );
            // The SSE event name must be the one the frontend subscribes to.
            assert_eq!(kind.stream(), family.stream_name());
        }
        assert_eq!(
            serde_json::to_value(DashboardEventKindV1::activity(
                ActivityFamilyV1::ToolCall,
                4,
                4,
                Some("tracedecay_context".into()),
            ))
            .unwrap()["family"],
            "tool_call_activity"
        );
    }

    #[test]
    fn resume_header_and_gap_event_preserve_canonical_control_values() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "lane-a:41".parse().expect("valid header"));
        let resume = parse_last_event_id(&headers).expect("resume");
        assert_eq!(resume.run_id, "lane-a");
        assert_eq!(resume.sequence, 41);

        let event = resume_gap_event(
            41,
            &ActivityFrontierV1 {
                run_id: "lane-b".into(),
                next_sequence: 48,
                retained_from_sequence: 44,
                dropped_events: 43,
                watermark: "47".into(),
            },
            &scope(),
        );
        let wire = serde_json::to_value(event).expect("canonical event value");
        assert_eq!(wire["kind"]["family"], "resume_gap");
        assert_eq!(wire["kind"]["requested_after"], 41);
        assert_eq!(wire["kind"]["first_available"], 44);
        assert_eq!(wire["source_watermark"]["watermark"], "47");
        assert_eq!(wire["coverage"]["completeness"], "partial");
    }

    #[test]
    fn coalesced_events_end_at_the_highest_persisted_producer_sequence() {
        let mut state = EventStreamState::new("connection-run".into());
        let mut pending = std::collections::BTreeMap::new();
        accumulate_record(
            &mut pending,
            record(9, 1, pulse(ActivityFamilyV1::ToolCall, "/repo/z", 1)),
        );
        accumulate_record(
            &mut pending,
            record(10, 1, pulse(ActivityFamilyV1::Hook, "/repo/a", 1)),
        );
        let events = state.flush_activity(&mut pending, &scope());
        assert_eq!(
            events
                .iter()
                .filter_map(|event| event.producer_sequence)
                .collect::<Vec<_>>(),
            vec![9, 10],
            "the final SSE id must always be the durable frontier"
        );
    }

    #[tokio::test]
    async fn http_sse_frame_uses_the_canonical_event_value_and_resume_identity() {
        let mut state = EventStreamState::new("connection-run".into());
        let mut pending = std::collections::BTreeMap::new();
        accumulate_record(
            &mut pending,
            record(7, 0, pulse(ActivityFamilyV1::Hook, "/repo/a", 2)),
        );
        let event = state
            .flush_activity(&mut pending, &scope())
            .pop()
            .expect("event");
        let frame = encode_event(&event).expect("SSE encoding");
        let response =
            Sse::new(tokio_stream::iter(vec![Ok::<_, Infallible>(frame)])).into_response();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("SSE body");
        let text = std::str::from_utf8(&body).expect("UTF-8 SSE");
        assert_eq!(
            text.lines()
                .find_map(|line| line.strip_prefix("id:"))
                .map(str::trim),
            Some("run-durable:7")
        );
        assert_eq!(
            text.lines()
                .find_map(|line| line.strip_prefix("event:"))
                .map(str::trim),
            Some("hook_activity")
        );
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .expect("SSE data");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(data).expect("event JSON"),
            serde_json::to_value(event).expect("canonical event value")
        );
    }

    #[tokio::test]
    async fn poll_sources_reads_real_state_and_primes_baseline() {
        let _pin = crate::test_support::PinnedUserDataDir::new();
        let (project, mut dash) = dashboard_state_fixture("project.dashboard-events").await;
        let registry = registered_database_for_test(&project.path().join("registry.db")).await;
        dash.savings_db_path = registry.db_path().display().to_string();
        dash.savings_db = Some(registry);
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
