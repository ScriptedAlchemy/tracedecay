//! Bounded application activity over the registered Plan-26 observation store.
//!
//! The broadcast channel is only a wake-up optimization. Durable replay reads
//! canonical `activity.observed.v1` envelopes from the registered project
//! session authority, whose normal retention policy owns expiry and cleanup.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{OnceLock, atomic};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracedecay_application::{
    ApplicationContractError, ObservabilityApplicationV1, ObservabilityHorizonV1,
    ObservabilityQueryV1, now_micros,
};
use tracedecay_domain::{
    ActivityObservedV1, CoverageStateV1, McpDispatchObservedV1, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    canonical_sha256,
};

use crate::observability::RegisteredObservabilityPortV1;
use tracedecay_global_db::RegisteredGlobalDb;

const BUS_CAPACITY: usize = 1024;
const RETAINED_ACTIVITY_CAPACITY: usize = 5_000;
const ACTIVITY_EVENT_KIND: &str = "activity.observed.v1";
const ACTIVITY_SCHEMA_VERSION: u32 = 1;
const MCP_DISPATCH_EVENT_KIND: &str = "mcp.dispatch.observed.v1";
const MCP_DISPATCH_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActivityFamilyV1 {
    Hook,
    SessionIngest,
    CodeIndex,
    ToolCall,
    Task,
}

impl ActivityFamilyV1 {
    pub const fn stream_name(self) -> &'static str {
        match self {
            Self::Hook => "hook_activity",
            Self::SessionIngest => "session_ingest",
            Self::CodeIndex => "code_index_activity",
            Self::ToolCall => "tool_call",
            Self::Task => "task_activity",
        }
    }

    const fn observation_label(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::SessionIngest => "session_ingest",
            Self::CodeIndex => "code_index",
            Self::ToolCall => "tool_call",
            Self::Task => "task",
        }
    }

    fn from_observation_label(value: &str) -> Option<Self> {
        match value {
            "hook" => Some(Self::Hook),
            "session_ingest" => Some(Self::SessionIngest),
            "code_index" => Some(Self::CodeIndex),
            "tool_call" => Some(Self::ToolCall),
            "task" => Some(Self::Task),
            _ => None,
        }
    }

    /// Canonical family set for exhaustive projections and generated adapters.
    pub const ALL: [Self; 5] = [
        Self::Hook,
        Self::SessionIngest,
        Self::CodeIndex,
        Self::ToolCall,
        Self::Task,
    ];
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityPulseV1 {
    pub family: ActivityFamilyV1,
    /// Live-only routing hint. It is deliberately absent from the retained
    /// observation payload; replay resolves through the typed project id.
    pub project_root: PathBuf,
    pub project_id: Option<String>,
    pub units: u64,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityRecordV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub producer_sequence: u64,
    pub observation_time_micros: i64,
    pub retained_from_sequence: u64,
    /// Proven lower bound. Zero does not claim complete retention; consumers
    /// must also inspect `resume_gap`.
    pub dropped_events: u64,
    pub pulse: ActivityPulseV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityFrontierV1 {
    pub run_id: String,
    pub next_sequence: u64,
    pub retained_from_sequence: u64,
    pub dropped_events: u64,
    pub watermark: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityReplayV1 {
    pub records: Vec<ActivityRecordV1>,
    pub frontier: ActivityFrontierV1,
    pub resume_gap: bool,
}

fn live_bus() -> &'static broadcast::Sender<ActivityRecordV1> {
    static BUS: OnceLock<broadcast::Sender<ActivityRecordV1>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(BUS_CAPACITY).0)
}

fn boot_id() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| format!("activity-{}-{}", std::process::id(), now_micros().0))
}

fn next_local_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, atomic::Ordering::Relaxed)
}

fn mcp_dispatch_boot_id() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| format!("mcp-dispatch-{}-{}", std::process::id(), now_micros().0))
}

fn next_mcp_dispatch_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, atomic::Ordering::Relaxed)
}

fn authoritative_project_id(db: &RegisteredGlobalDb, supplied: Option<&str>) -> Option<String> {
    let bound = db.binding().shard_id.scope.project_id()?.as_str();
    match supplied {
        Some(supplied) if supplied != bound => None,
        _ => Some(bound.to_owned()),
    }
}

fn activity_envelope(
    project_id: &str,
    family: ActivityFamilyV1,
    units: u64,
    detail: Option<String>,
) -> Option<ObservabilityEnvelopeV1> {
    let observed_at = now_micros().0;
    let producer_sequence = next_local_sequence();
    let event_id = canonical_sha256(&(
        "tracedecay.activity.observed.v1",
        boot_id(),
        producer_sequence,
        project_id,
        family.observation_label(),
        units,
        detail.as_deref(),
    ))
    .ok()?;
    let event_id = format!("activity:{}", event_id.as_str());
    Some(ObservabilityEnvelopeV1 {
        event_id: event_id.clone(),
        event_kind: ACTIVITY_EVENT_KIND.to_owned(),
        schema_revision: ACTIVITY_SCHEMA_VERSION,
        idempotency_key: event_id.clone(),
        trace_id: event_id,
        scope_ref: project_id.to_owned(),
        capability: "activity".to_owned(),
        operation: family.observation_label().to_owned(),
        event_time_micros: observed_at,
        observation_time_micros: observed_at,
        valid_from_micros: Some(observed_at),
        valid_until_micros: None,
        quantity: Some(units as f64),
        unit: Some("events".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "activity-observer.v1".to_owned(),
        configuration_revision: "registered-project-session.v1".to_owned(),
        policy_revision: "local-activity-retention.v1".to_owned(),
        watermark: format!("{boot_id}:{producer_sequence}", boot_id = boot_id()),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: boot_id().to_owned(),
        producer_sequence,
        payload: ObservabilityPayloadV1::Activity(ActivityObservedV1 {
            family: family.observation_label().to_owned(),
            units,
            detail,
        }),
    })
}

fn mcp_dispatch_envelope(
    project_id: &str,
    observation: McpDispatchObservedV1,
) -> Result<ObservabilityEnvelopeV1, ApplicationContractError> {
    let observed_at = now_micros().0;
    let producer_sequence = next_mcp_dispatch_sequence();
    let event_id = canonical_sha256(&(
        "tracedecay.mcp.dispatch.observed.v1",
        mcp_dispatch_boot_id(),
        producer_sequence,
        project_id,
        &observation,
    ))
    .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
    let event_id = format!("mcp-dispatch:{}", event_id.as_str());
    Ok(ObservabilityEnvelopeV1 {
        event_id: event_id.clone(),
        event_kind: MCP_DISPATCH_EVENT_KIND.to_owned(),
        schema_revision: MCP_DISPATCH_SCHEMA_VERSION,
        idempotency_key: event_id.clone(),
        trace_id: event_id,
        scope_ref: project_id.to_owned(),
        capability: "mcp".to_owned(),
        operation: "dispatch".to_owned(),
        event_time_micros: observed_at,
        observation_time_micros: observed_at,
        valid_from_micros: Some(observed_at),
        valid_until_micros: None,
        quantity: Some(1.0),
        unit: Some("dispatches".to_owned()),
        terminal_result: Some(observation.terminal_result()),
        producer_revision: "mcp-dispatch-observer.v1".to_owned(),
        configuration_revision: "registered-project-session.v1".to_owned(),
        policy_revision: "mcp-dispatch-deadline.v1".to_owned(),
        watermark: format!(
            "{boot_id}:{producer_sequence}",
            boot_id = mcp_dispatch_boot_id()
        ),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: mcp_dispatch_boot_id().to_owned(),
        producer_sequence,
        payload: ObservabilityPayloadV1::McpDispatch(observation),
    })
}

/// Records one fixed-shape MCP dispatch receipt through the project-bound
/// observability authority. The caller receives a typed storage failure and
/// must not change the already-determined MCP terminal response because
/// telemetry persistence failed.
pub async fn record_mcp_dispatch(
    db: &RegisteredGlobalDb,
    observation: McpDispatchObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id =
        authoritative_project_id(db, None).ok_or(ApplicationContractError::Inconsistent {
            field: "mcp_dispatch_observability.project_scope",
        })?;
    let envelope = mcp_dispatch_envelope(&project_id, observation)?;
    let port = RegisteredObservabilityPortV1::new(db);
    ObservabilityApplicationV1::new(port, port)
        .record(envelope)
        .await
}

pub fn enabled(db: Option<&RegisteredGlobalDb>) -> bool {
    db.and_then(|db| authoritative_project_id(db, None))
        .is_some()
}

pub fn subscribe() -> Option<broadcast::Receiver<ActivityRecordV1>> {
    Some(live_bus().subscribe())
}

pub async fn publish(
    db: &RegisteredGlobalDb,
    family: ActivityFamilyV1,
    project_root: &Path,
    project_id: Option<&str>,
    units: u64,
    detail: Option<&str>,
) {
    let Some(project_id) = authoritative_project_id(db, project_id) else {
        return;
    };
    let units = units.max(1);
    let detail = ActivityObservedV1::bounded_detail(family.observation_label(), detail);
    let Some(envelope) = activity_envelope(&project_id, family, units, detail.clone()) else {
        return;
    };
    let observed_at = envelope.observation_time_micros;
    let port = RegisteredObservabilityPortV1::new(db);
    let Ok(storage_cursor) = ObservabilityApplicationV1::new(port, port)
        .record(envelope)
        .await
    else {
        return;
    };
    let Some(row_id) = storage_cursor
        .strip_prefix("analytics:")
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    let record = ActivityRecordV1 {
        schema_version: ACTIVITY_SCHEMA_VERSION,
        run_id: "registered-observability-v1".to_owned(),
        producer_sequence: row_id,
        observation_time_micros: observed_at,
        retained_from_sequence: row_id,
        dropped_events: 0,
        pulse: ActivityPulseV1 {
            family,
            project_root: project_root.to_path_buf(),
            project_id: Some(project_id),
            units,
            detail,
        },
    };
    let _ = live_bus().send(record);
}

pub async fn replay_after(
    db: &RegisteredGlobalDb,
    project_id: &str,
    after: Option<u64>,
) -> Option<ActivityReplayV1> {
    authoritative_project_id(db, Some(project_id))?;
    let port = RegisteredObservabilityPortV1::new(db);
    let page = ObservabilityApplicationV1::new(port, port)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: project_id.to_owned(),
            event_kinds: vec![ACTIVITY_EVENT_KIND.to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: (RETAINED_ACTIVITY_CAPACITY + 1) as u32,
        })
        .await
        .ok()?;
    let capped =
        page.events.len() > RETAINED_ACTIVITY_CAPACITY || page.coverage == CoverageStateV1::Capped;
    let mut records = page
        .events
        .into_iter()
        .zip(page.event_cursors)
        .filter_map(|(event, cursor)| {
            let sequence = cursor
                .strip_prefix("analytics:")
                .and_then(|value| value.parse::<u64>().ok())?;
            let ObservabilityPayloadV1::Activity(payload) = event.payload else {
                return None;
            };
            let family = ActivityFamilyV1::from_observation_label(&payload.family)?;
            Some(ActivityRecordV1 {
                schema_version: ACTIVITY_SCHEMA_VERSION,
                run_id: "registered-observability-v1".to_owned(),
                producer_sequence: sequence,
                observation_time_micros: event.observation_time_micros,
                retained_from_sequence: 0,
                dropped_events: u64::from(capped),
                pulse: ActivityPulseV1 {
                    family,
                    project_root: PathBuf::new(),
                    project_id: Some(project_id.to_owned()),
                    units: payload.units.max(1),
                    detail: payload.detail,
                },
            })
        })
        .collect::<Vec<_>>();
    let requested_retained = after.is_some_and(|requested| {
        records
            .iter()
            .any(|record| record.producer_sequence == requested)
    });
    records.sort_by_key(|record| record.producer_sequence);
    if records.len() > RETAINED_ACTIVITY_CAPACITY {
        records.remove(0);
    }
    let retained_from_sequence = records
        .first()
        .map_or(after.unwrap_or(0).saturating_add(1), |record| {
            record.producer_sequence
        });
    for record in &mut records {
        record.retained_from_sequence = retained_from_sequence;
    }
    let requested = after.unwrap_or(0);
    let resume_gap = after.is_some() && capped && !requested_retained;
    records.retain(|record| record.producer_sequence > requested);
    let latest = records
        .last()
        .map_or(requested, |record| record.producer_sequence);
    Some(ActivityReplayV1 {
        records,
        frontier: ActivityFrontierV1 {
            run_id: "registered-observability-v1".to_owned(),
            next_sequence: latest.saturating_add(1),
            retained_from_sequence,
            dropped_events: u64::from(capped),
            watermark: format!("analytics:{latest}"),
        },
        resume_gap,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::ObservabilityQueryPort;

    #[test]
    fn family_stream_names_are_distinct_and_stable() {
        let mut names = ActivityFamilyV1::ALL
            .iter()
            .map(|family| family.stream_name())
            .collect::<Vec<_>>();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique);
        assert_eq!(ActivityFamilyV1::Hook.stream_name(), "hook_activity");
        assert_eq!(ActivityFamilyV1::ToolCall.stream_name(), "tool_call");
    }

    /// The canonical envelope validator keeps its own list of admitted activity
    /// families, and replay resolves a retained observation back through the
    /// label. A family this lane can publish but either side rejects is
    /// swallowed by the error-tolerant publish path and renders as no activity.
    ///
    /// The match below is what binds the enum to [`ActivityFamilyV1::ALL`]:
    /// adding a family without listing it there leaves this match
    /// non-exhaustive, so the omission is a compile error rather than a family
    /// this test silently never visits.
    #[test]
    fn every_published_family_is_admitted_by_the_canonical_envelope() {
        for family in ActivityFamilyV1::ALL {
            let label = match family {
                ActivityFamilyV1::Hook => "hook",
                ActivityFamilyV1::SessionIngest => "session_ingest",
                ActivityFamilyV1::CodeIndex => "code_index",
                ActivityFamilyV1::ToolCall => "tool_call",
                ActivityFamilyV1::Task => "task",
            };
            assert_eq!(family.observation_label(), label);
            assert_eq!(
                ActivityFamilyV1::from_observation_label(label),
                Some(family),
                "replay must resolve {label:?} back to the family that published it"
            );

            let envelope = activity_envelope("project.activity.family", family, 1, None)
                .unwrap_or_else(|| panic!("{family:?} envelope"));
            assert_eq!(
                envelope.validate(),
                Ok(()),
                "{family:?} publishes label {label:?}, which the canonical envelope must admit"
            );
        }
        assert_eq!(
            ActivityFamilyV1::ALL.len(),
            5,
            "extend the exhaustive match above when this changes"
        );
    }

    #[test]
    fn source_owner_activity_strips_unbounded_detail_before_persistence() {
        let detail = ActivityObservedV1::bounded_detail(
            ActivityFamilyV1::ToolCall.observation_label(),
            Some("external-tool-name"),
        );
        assert_eq!(detail, None);
        let envelope = activity_envelope("project.activity", ActivityFamilyV1::ToolCall, 1, detail)
            .unwrap_or_else(|| panic!("activity envelope"));
        assert_eq!(envelope.validate(), Ok(()));
        let ObservabilityPayloadV1::Activity(payload) = envelope.payload else {
            panic!("activity payload");
        };
        assert_eq!(payload.detail, None);
    }

    #[tokio::test]
    async fn registered_activity_replays_without_retaining_project_paths() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id = tracedecay_domain::ProjectId::new("project.activity").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let db = runtime
            .project_database()
            .expect("project observation database");

        publish(
            db,
            ActivityFamilyV1::Hook,
            project.path(),
            Some(project_id.as_str()),
            2,
            Some("after_edit"),
        )
        .await;
        let replay = replay_after(db, project_id.as_str(), None)
            .await
            .expect("activity replay");
        assert_eq!(replay.records.len(), 1);
        assert_eq!(replay.records[0].pulse.units, 2);
        assert_eq!(replay.records[0].pulse.project_root, PathBuf::new());
        assert_eq!(
            replay.records[0].pulse.project_id.as_deref(),
            Some(project_id.as_str())
        );
        assert!(!replay.resume_gap);

        let after = replay.records[0].producer_sequence;
        let caught_up = replay_after(db, project_id.as_str(), Some(after))
            .await
            .expect("caught-up replay");
        assert!(caught_up.records.is_empty());
        assert!(!caught_up.resume_gap);
        assert!(
            !tracedecay_runtime_core::storage::default_profile_root()
                .expect("profile root")
                .join("dashboard-events-v1.jsonl")
                .exists()
        );
    }

    #[tokio::test]
    async fn mcp_dispatch_receipt_uses_the_bound_project_observability_authority() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id =
            tracedecay_domain::ProjectId::new("project.mcp.dispatch").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let db = runtime
            .project_database()
            .expect("project observation database");
        let observation = McpDispatchObservedV1 {
            route_admission_micros: 4,
            handler_micros: 16,
            result_materialization_micros: 3,
            total_micros: 23,
            deadline: tracedecay_domain::McpDispatchDeadlineV1::Enforced,
            cancellation: tracedecay_domain::McpDispatchCancellationV1::NotRequested,
            terminal: tracedecay_domain::McpDispatchTerminalV1::Completed,
        };

        let cursor = record_mcp_dispatch(db, observation.clone())
            .await
            .expect("record MCP dispatch");
        assert!(cursor.starts_with("analytics:"));

        let port = RegisteredObservabilityPortV1::new(db);
        let page = port
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: project_id.to_string(),
                event_kinds: vec![MCP_DISPATCH_EVENT_KIND.to_owned()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 10,
            })
            .await
            .expect("query MCP dispatch");
        assert_eq!(page.events.len(), 1);
        let event = &page.events[0];
        assert_eq!(event.scope_ref, project_id.as_str());
        assert_eq!(event.capability, "mcp");
        assert_eq!(event.operation, "dispatch");
        assert_eq!(
            event.terminal_result,
            Some(ObservabilityTerminalResultV1::Succeeded)
        );
        assert_eq!(
            event.payload,
            ObservabilityPayloadV1::McpDispatch(observation)
        );
    }
}
