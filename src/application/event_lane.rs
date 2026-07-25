//! Durable, bounded application activity lane shared by every adapter.
//!
//! The daemon already observes real work as it happens — host hooks arriving on
//! the MCP boundary, transcript messages landing in the session store, touched
//! paths entering the code-index queue, tool calls being dispatched. Producers
//! publish one bounded record at the exact point of observation; adapters
//! subscribe, replay, coalesce, and render the same retained values.
//!
//! Producers append before notifying live consumers. Reconnects and daemon
//! restarts replay the retained journal by producer sequence; broadcast is only
//! a wake-up optimization. Retention is bounded and every eviction advances a
//! persisted frontier with explicit drop accounting. Every producer names its
//! own scope: each record carries the project root where the work happened and
//! the registered project id when the producer already knows it. The consumer
//! resolves the rest from the project registry it polls anyway, so a pulse
//! never triggers a lookup on the hot path.
//!
//! Coalescing lives in the SSE adapter; producer sequence and the retained
//! control frontier live here and are shared by every consumer.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;
use tracedecay_application::{DirectorySyncPolicy, append_durable, atomic_write};

/// Live notifications are not authoritative. A lagged consumer reads the
/// durable frontier instead.
const BUS_CAPACITY: usize = 1024;
/// Product retention bound from Plan 11's SSE queue budget.
const RETAINED_ACTIVITY_CAPACITY: usize = 5_000;
/// 5,000 retained records remain below Plan 11's 10 MiB queue bound.
const MAX_ACTIVITY_RECORD_BYTES: usize = 2_048;
const JOURNAL_SCHEMA_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "dashboard-events-v1.jsonl";

/// One observed activity family. Each maps to a distinct SSE event name and a
/// distinct `kind.family` tag, so the frontend can style and route them
/// independently.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityFamilyV1 {
    /// A host lifecycle hook was admitted on the MCP hook boundary.
    Hook,
    /// Transcript messages were durably persisted into the session store.
    SessionIngest,
    /// Touched paths entered a mounted worktree's incremental code-index queue.
    CodeIndex,
    /// A `tools/call` was dispatched by the daemon's MCP server.
    ToolCall,
}

impl ActivityFamilyV1 {
    /// The SSE `event:` name carrying this family. The frontend subscribes to
    /// named events, so this list is part of the wire contract.
    pub(crate) const fn stream_name(self) -> &'static str {
        match self {
            Self::Hook => "hook_activity",
            Self::SessionIngest => "session_ingest",
            Self::CodeIndex => "code_index_activity",
            Self::ToolCall => "tool_call",
        }
    }

    /// Every family, in a stable order. Used by tests and by the wire-contract
    /// assertions that keep the frontend's subscription list honest.
    pub(crate) const ALL: [Self; 4] = [
        Self::Hook,
        Self::SessionIngest,
        Self::CodeIndex,
        Self::ToolCall,
    ];
}

/// One observation. Cheap to clone: two `Arc`-free owned strings at most.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ActivityPulseV1 {
    pub(crate) family: ActivityFamilyV1,
    /// Project root the work happened in. Not required to be canonical; the
    /// consumer canonicalizes once per emitted bucket, not once per pulse.
    pub(crate) project_root: PathBuf,
    /// Registered project id, when the producer already holds it. `None` means
    /// "resolve me from the registry", not "no project".
    pub(crate) project_id: Option<String>,
    /// How many underlying units this pulse represents (hook events, messages,
    /// queued files, tool calls). Always at least 1.
    pub(crate) units: u64,
    /// A short producer-supplied label (hook kind, provider, tool name). Bounded
    /// by construction — every current producer passes a static or already-short
    /// identifier, never user content.
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ActivityRecordV1 {
    pub(crate) schema_version: u32,
    pub(crate) run_id: String,
    pub(crate) producer_sequence: u64,
    pub(crate) observation_time_micros: i64,
    pub(crate) retained_from_sequence: u64,
    pub(crate) dropped_events: u64,
    pub(crate) pulse: ActivityPulseV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityFrontierV1 {
    pub(crate) run_id: String,
    pub(crate) next_sequence: u64,
    pub(crate) retained_from_sequence: u64,
    pub(crate) dropped_events: u64,
    pub(crate) watermark: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityReplayV1 {
    pub(crate) records: Vec<ActivityRecordV1>,
    pub(crate) frontier: ActivityFrontierV1,
    pub(crate) resume_gap: bool,
}

#[derive(Debug, Error)]
pub(crate) enum ActivityLaneError {
    #[error("dashboard event lane capacity must be non-zero")]
    InvalidCapacity,
    #[error("dashboard event lane I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("dashboard event lane encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("dashboard event producer sequence is exhausted")]
    SequenceExhausted,
    #[error("dashboard event record exceeds the bounded lane payload")]
    RecordTooLarge,
}

struct DurableActivityStateV1 {
    run_id: String,
    next_sequence: u64,
    retained_from_sequence: u64,
    dropped_events: u64,
    physical_records: usize,
    records: VecDeque<ActivityRecordV1>,
}

pub(crate) struct DurableActivityLaneV1 {
    path: PathBuf,
    capacity: usize,
    state: Mutex<DurableActivityStateV1>,
    live: broadcast::Sender<ActivityRecordV1>,
}

impl DurableActivityLaneV1 {
    pub(crate) fn open(path: PathBuf, capacity: usize) -> Result<Self, ActivityLaneError> {
        if capacity == 0 {
            return Err(ActivityLaneError::InvalidCapacity);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let (mut records, physical_records, repair_tail) = load_records(&path)?;
        let last = records.back().cloned();
        while records.len() > capacity {
            records.pop_front();
        }
        let retained_from_sequence = records.front().map_or_else(
            || {
                last.as_ref()
                    .map_or(1, |record| record.producer_sequence + 1)
            },
            |record| record.producer_sequence,
        );
        let dropped_events = last
            .as_ref()
            .map_or(0, |record| record.dropped_events)
            .max(retained_from_sequence.saturating_sub(1));
        let next_sequence = last
            .as_ref()
            .map_or(1, |record| record.producer_sequence.saturating_add(1));
        let run_id = last.map_or_else(new_run_id, |record| record.run_id);
        let (live, _) = broadcast::channel(BUS_CAPACITY);
        let lane = Self {
            path,
            capacity,
            state: Mutex::new(DurableActivityStateV1 {
                run_id,
                next_sequence,
                retained_from_sequence,
                dropped_events,
                physical_records,
                records,
            }),
            live,
        };
        if repair_tail
            || lane
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .physical_records
                > capacity
        {
            lane.compact()?;
        }
        Ok(lane)
    }

    pub(crate) fn publish(&self, pulse: ActivityPulseV1) -> Result<u64, ActivityLaneError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = state.next_sequence;
        let next_sequence = sequence
            .checked_add(1)
            .ok_or(ActivityLaneError::SequenceExhausted)?;
        let evicts = state.records.len() == self.capacity;
        let dropped_events = state.dropped_events.saturating_add(u64::from(evicts));
        let retained_from_sequence = state
            .records
            .iter()
            .nth(usize::from(evicts))
            .map_or(sequence, |record| record.producer_sequence);
        let record = ActivityRecordV1 {
            schema_version: JOURNAL_SCHEMA_VERSION,
            run_id: state.run_id.clone(),
            producer_sequence: sequence,
            observation_time_micros: now_micros(),
            retained_from_sequence,
            dropped_events,
            pulse,
        };
        let encoded = match serde_json::to_vec(&record) {
            Ok(encoded) => encoded,
            Err(error) => {
                state.dropped_events = state.dropped_events.saturating_add(1);
                return Err(error.into());
            }
        };
        if encoded.len() > MAX_ACTIVITY_RECORD_BYTES {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return Err(ActivityLaneError::RecordTooLarge);
        }
        if let Err(error) = append_record(&self.path, &encoded) {
            state.dropped_events = state.dropped_events.saturating_add(1);
            let _ = rewrite_records(&self.path, &state.records);
            state.physical_records = state.records.len();
            return Err(error);
        }
        if evicts {
            state.records.pop_front();
        }
        state.next_sequence = next_sequence;
        state.dropped_events = dropped_events;
        state.records.push_back(record.clone());
        state.retained_from_sequence = state
            .records
            .front()
            .map_or(state.next_sequence, |record| record.producer_sequence);
        state.physical_records = state.physical_records.saturating_add(1);
        let compaction_slack = (self.capacity / 10).clamp(1, 64);
        let compact = state.physical_records >= self.capacity.saturating_add(compaction_slack);
        drop(state);
        if compact {
            self.compact()?;
        }
        let _ = self.live.send(record);
        Ok(sequence)
    }

    pub(crate) fn replay_after(&self, after: Option<u64>) -> ActivityReplayV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = after.unwrap_or(0);
        let resume_gap =
            after.is_some() && requested.saturating_add(1) < state.retained_from_sequence;
        let records = state
            .records
            .iter()
            .filter(|record| record.producer_sequence > requested)
            .cloned()
            .collect();
        ActivityReplayV1 {
            records,
            frontier: frontier(&state),
            resume_gap,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ActivityRecordV1> {
        self.live.subscribe()
    }

    fn compact(&self) -> Result<(), ActivityLaneError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rewrite_records(&self.path, &state.records)?;
        state.physical_records = state.records.len();
        Ok(())
    }
}

fn frontier(state: &DurableActivityStateV1) -> ActivityFrontierV1 {
    ActivityFrontierV1 {
        run_id: state.run_id.clone(),
        next_sequence: state.next_sequence,
        retained_from_sequence: state.retained_from_sequence,
        dropped_events: state.dropped_events,
        watermark: state.next_sequence.saturating_sub(1).to_string(),
    }
}

fn new_run_id() -> String {
    format!("dashboard-events-{}-{}", std::process::id(), now_micros())
}

fn load_records(
    path: &Path,
) -> Result<(VecDeque<ActivityRecordV1>, usize, bool), ActivityLaneError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((VecDeque::new(), 0, false));
        }
        Err(error) => return Err(error.into()),
    };
    let mut records = VecDeque::new();
    let mut physical = 0;
    let mut previous = 0;
    let mut repair_tail = false;
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                repair_tail = true;
                break;
            }
            Err(error) => return Err(error.into()),
        };
        let Ok(record) = serde_json::from_str::<ActivityRecordV1>(&line) else {
            repair_tail = true;
            break;
        };
        if record.schema_version != JOURNAL_SCHEMA_VERSION || record.producer_sequence <= previous {
            repair_tail = true;
            break;
        }
        previous = record.producer_sequence;
        physical += 1;
        records.push_back(record);
    }
    Ok((records, physical, repair_tail))
}

fn append_record(path: &Path, record: &[u8]) -> Result<(), ActivityLaneError> {
    let mut frame = Vec::with_capacity(record.len().saturating_add(1));
    frame.extend_from_slice(record);
    frame.push(b'\n');
    append_durable(path, &frame, DirectorySyncPolicy::TolerateUnsupported)?;
    Ok(())
}

fn rewrite_records(
    path: &Path,
    records: &VecDeque<ActivityRecordV1>,
) -> Result<(), ActivityLaneError> {
    let mut encoded = Vec::new();
    for record in records {
        serde_json::to_writer(&mut encoded, record)?;
        encoded.push(b'\n');
    }
    atomic_write(
        path,
        "dashboard-event-compaction",
        &encoded,
        DirectorySyncPolicy::TolerateUnsupported,
    )?;
    Ok(())
}

fn lanes() -> &'static Mutex<HashMap<PathBuf, Arc<DurableActivityLaneV1>>> {
    static LANES: OnceLock<Mutex<HashMap<PathBuf, Arc<DurableActivityLaneV1>>>> = OnceLock::new();
    LANES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn global_lane() -> Option<Arc<DurableActivityLaneV1>> {
    let path = crate::config::user_data_dir()?.join(JOURNAL_FILE);
    let mut lanes = lanes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lane) = lanes.get(&path) {
        return Some(Arc::clone(lane));
    }
    let lane =
        Arc::new(DurableActivityLaneV1::open(path.clone(), RETAINED_ACTIVITY_CAPACITY).ok()?);
    lanes.insert(path, Arc::clone(&lane));
    Some(lane)
}

/// Durable publication is armed whenever the user profile root is available.
pub(crate) fn enabled() -> bool {
    global_lane().is_some()
}

pub(crate) fn subscribe() -> Option<broadcast::Receiver<ActivityRecordV1>> {
    Some(global_lane()?.subscribe())
}

pub(crate) fn replay_after(after: Option<u64>) -> Option<ActivityReplayV1> {
    Some(global_lane()?.replay_after(after))
}

pub(crate) fn publish(
    family: ActivityFamilyV1,
    project_root: &Path,
    project_id: Option<&str>,
    units: u64,
    detail: Option<&str>,
) {
    let Some(lane) = global_lane() else {
        return;
    };
    let _ = lane.publish(ActivityPulseV1 {
        family,
        project_root: project_root.to_path_buf(),
        project_id: project_id.map(ToOwned::to_owned),
        units: units.max(1),
        detail: detail.map(ToOwned::to_owned),
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    #[test]
    fn durable_lane_replays_after_restart_and_preserves_sequence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dashboard-events-v1.jsonl");
        {
            let lane = DurableActivityLaneV1::open(path.clone(), 8).unwrap();
            assert_eq!(
                lane.publish(ActivityPulseV1 {
                    family: ActivityFamilyV1::Hook,
                    project_root: PathBuf::from("/repo/alpha"),
                    project_id: Some("proj-alpha".into()),
                    units: 2,
                    detail: Some("file_edit".into()),
                })
                .unwrap(),
                1
            );
            assert_eq!(
                lane.publish(ActivityPulseV1 {
                    family: ActivityFamilyV1::ToolCall,
                    project_root: PathBuf::from("/repo/alpha"),
                    project_id: Some("proj-alpha".into()),
                    units: 1,
                    detail: Some("context".into()),
                })
                .unwrap(),
                2
            );
        }

        let reopened = DurableActivityLaneV1::open(path, 8).unwrap();
        let replay = reopened.replay_after(Some(1));
        assert_eq!(replay.records.len(), 1);
        assert_eq!(replay.records[0].producer_sequence, 2);
        assert_eq!(replay.frontier.next_sequence, 3);
        assert_eq!(replay.frontier.retained_from_sequence, 1);
        assert_eq!(replay.frontier.dropped_events, 0);
        assert!(!replay.resume_gap);
    }

    #[test]
    fn durable_lane_reports_expired_resume_and_persisted_drop_coverage() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dashboard-events-v1.jsonl");
        {
            let lane = DurableActivityLaneV1::open(path.clone(), 2).unwrap();
            for index in 0..4 {
                lane.publish(ActivityPulseV1 {
                    family: ActivityFamilyV1::CodeIndex,
                    project_root: PathBuf::from("/repo/alpha"),
                    project_id: Some("proj-alpha".into()),
                    units: 1,
                    detail: Some(format!("file-{index}")),
                })
                .unwrap();
            }
            let replay = lane.replay_after(Some(1));
            assert!(replay.resume_gap);
            assert_eq!(replay.frontier.retained_from_sequence, 3);
            assert_eq!(replay.frontier.dropped_events, 2);
            assert_eq!(
                replay
                    .records
                    .iter()
                    .map(|record| record.producer_sequence)
                    .collect::<Vec<_>>(),
                vec![3, 4]
            );
        }

        let reopened = DurableActivityLaneV1::open(path, 2).unwrap();
        let replay = reopened.replay_after(None);
        assert_eq!(replay.frontier.retained_from_sequence, 3);
        assert_eq!(replay.frontier.dropped_events, 2);
        assert_eq!(replay.frontier.watermark, "4");
    }

    #[test]
    fn oversized_event_is_rejected_and_the_next_record_persists_the_drop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dashboard-events-v1.jsonl");
        let lane = DurableActivityLaneV1::open(path.clone(), 8).unwrap();
        let error = lane
            .publish(ActivityPulseV1 {
                family: ActivityFamilyV1::ToolCall,
                project_root: PathBuf::from("/repo/alpha"),
                project_id: Some("proj-alpha".into()),
                units: 1,
                detail: Some("x".repeat(4_096)),
            })
            .unwrap_err();
        assert!(matches!(error, ActivityLaneError::RecordTooLarge));
        assert_eq!(
            lane.publish(ActivityPulseV1 {
                family: ActivityFamilyV1::ToolCall,
                project_root: PathBuf::from("/repo/alpha"),
                project_id: Some("proj-alpha".into()),
                units: 1,
                detail: Some("context".into()),
            })
            .unwrap(),
            1
        );
        drop(lane);

        let replay = DurableActivityLaneV1::open(path, 8)
            .unwrap()
            .replay_after(None);
        assert_eq!(replay.frontier.dropped_events, 1);
        assert_eq!(replay.records[0].dropped_events, 1);
    }

    #[test]
    fn interrupted_tail_is_repaired_before_later_publication() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dashboard-events-v1.jsonl");
        let lane = DurableActivityLaneV1::open(path.clone(), 8).unwrap();
        lane.publish(ActivityPulseV1 {
            family: ActivityFamilyV1::Hook,
            project_root: PathBuf::from("/repo/alpha"),
            project_id: Some("proj-alpha".into()),
            units: 1,
            detail: None,
        })
        .unwrap();
        drop(lane);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{")
            .unwrap();

        let repaired = DurableActivityLaneV1::open(path.clone(), 8).unwrap();
        repaired
            .publish(ActivityPulseV1 {
                family: ActivityFamilyV1::ToolCall,
                project_root: PathBuf::from("/repo/alpha"),
                project_id: Some("proj-alpha".into()),
                units: 1,
                detail: None,
            })
            .unwrap();
        drop(repaired);

        let replay = DurableActivityLaneV1::open(path, 8)
            .unwrap()
            .replay_after(None);
        assert_eq!(
            replay
                .records
                .iter()
                .map(|record| record.producer_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn family_stream_names_are_distinct_and_stable() {
        let mut names = ActivityFamilyV1::ALL
            .iter()
            .map(|family| family.stream_name())
            .collect::<Vec<_>>();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "stream names must be distinct");
        assert_eq!(ActivityFamilyV1::Hook.stream_name(), "hook_activity");
        assert_eq!(
            ActivityFamilyV1::SessionIngest.stream_name(),
            "session_ingest"
        );
        assert_eq!(
            ActivityFamilyV1::CodeIndex.stream_name(),
            "code_index_activity"
        );
        assert_eq!(ActivityFamilyV1::ToolCall.stream_name(), "tool_call");
    }

    #[tokio::test]
    async fn durable_lane_notifies_live_consumers_and_replays_late_consumers() {
        let temp = tempfile::tempdir().unwrap();
        let lane =
            DurableActivityLaneV1::open(temp.path().join("dashboard-events-v1.jsonl"), 8).unwrap();
        let mut receiver = lane.subscribe();

        lane.publish(ActivityPulseV1 {
            family: ActivityFamilyV1::Hook,
            project_root: PathBuf::from("/repo/alpha"),
            project_id: Some("proj-alpha".into()),
            units: 3,
            detail: Some("file_edit".into()),
        })
        .unwrap();
        let record = receiver.recv().await.expect("record");
        let pulse = record.pulse;
        assert_eq!(pulse.family, ActivityFamilyV1::Hook);
        assert_eq!(pulse.project_root, PathBuf::from("/repo/alpha"));
        assert_eq!(pulse.project_id.as_deref(), Some("proj-alpha"));
        assert_eq!(pulse.units, 3);
        assert_eq!(pulse.detail.as_deref(), Some("file_edit"));

        drop(receiver);
        lane.publish(ActivityPulseV1 {
            family: ActivityFamilyV1::ToolCall,
            project_root: PathBuf::from("/repo/beta"),
            project_id: None,
            units: 1,
            detail: None,
        })
        .unwrap();
        let replay = lane.replay_after(Some(1));
        assert_eq!(replay.records.len(), 1);
        assert_eq!(replay.records[0].producer_sequence, 2);
        assert_eq!(replay.records[0].pulse.units, 1);
    }
}
